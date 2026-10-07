//! Keeps the installed maps in step with the game: finds terrains in the game and Workshop
//! folders, imports the one the game is on (or re-imports it after a mod update), and upgrades
//! maps built by older versions. All the work happens on one background thread.

use crossbeam_channel::{Receiver, Sender};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::import::{self, Catalog, WorldSource, catalog};
use crate::maps;

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub done: usize,
    pub total: usize,
}

#[derive(Default)]
pub struct LibraryState {
    pub catalog: Option<Arc<Catalog>>,
    pub scanning: bool,
    pub job: Option<Job>,
    /// A map that is ready and should be shown (taken by the UI).
    pub ready: Option<String>,
    pub message: Option<String>,
    /// Bumped whenever map packs on disk change.
    pub generation: u64,
}

enum Request {
    Scan,
    Ensure { world: String, mods: Vec<String> },
    Import { id: String },
}

pub struct Library {
    requests: Sender<Request>,
    pub state: Arc<Mutex<LibraryState>>,
}

impl Library {
    pub fn new(game_dir: Option<PathBuf>, ctx: egui::Context) -> Self {
        let (requests, rx) = crossbeam_channel::unbounded();
        let state = Arc::new(Mutex::new(LibraryState::default()));
        let worker = Worker {
            roots: catalog::roots(game_dir.as_deref()),
            state: state.clone(),
            ctx,
        };
        std::thread::Builder::new()
            .name("library".into())
            .spawn(move || worker.run(rx))
            .expect("spawning the library worker");
        let _ = requests.send(Request::Scan);
        Self { requests, state }
    }

    /// Makes sure `world` is imported and current, then marks it ready to show.
    pub fn ensure(&self, world: &str, mods: &[String]) {
        let _ = self.requests.send(Request::Ensure {
            world: world.to_lowercase(),
            mods: mods.to_vec(),
        });
    }

    pub fn rescan(&self) {
        let _ = self.requests.send(Request::Scan);
    }

    pub fn import(&self, id: &str) {
        let _ = self.requests.send(Request::Import { id: id.to_string() });
    }
}

struct Worker {
    roots: Vec<(PathBuf, bool)>,
    state: Arc<Mutex<LibraryState>>,
    ctx: egui::Context,
}

impl Worker {
    fn run(self, rx: Receiver<Request>) {
        let mut upgraded = false;
        for request in rx {
            match request {
                Request::Scan => {
                    self.scan();
                    if !upgraded {
                        upgraded = true;
                        self.upgrade_old_imports();
                    }
                }
                Request::Ensure { world, mods } => self.ensure(&world, &mods),
                Request::Import { id } => {
                    if let Some(source) = self.catalog().best(&id, &[]) {
                        self.import(&source, false);
                    }
                }
            }
        }
    }

    fn update(&self, f: impl FnOnce(&mut LibraryState)) {
        f(&mut self.state.lock().unwrap());
        self.ctx.request_repaint();
    }

    fn scan(&self) -> Arc<Catalog> {
        self.update(|s| s.scanning = true);
        let start = std::time::Instant::now();
        let catalog = Arc::new(catalog::scan(&self.roots));
        log::info!(
            "found {} terrains in {:.1?}",
            catalog.unique().len(),
            start.elapsed()
        );
        self.update(|s| {
            s.scanning = false;
            s.catalog = Some(catalog.clone());
        });
        catalog
    }

    fn catalog(&self) -> Arc<Catalog> {
        let existing = self.state.lock().unwrap().catalog.clone();
        existing.unwrap_or_else(|| self.scan())
    }

    fn ensure(&self, world: &str, mods: &[String]) {
        let mut catalog = self.catalog();
        if catalog.best(world, mods).is_none() {
            // Maybe a newly downloaded mod.
            catalog = self.scan();
        }
        let Some(source) = catalog.best(world, mods) else {
            log::warn!("no map files found for {world}");
            let installed = maps::load(&maps::maps_dir().join(world)).is_ok();
            self.update(|s| {
                if installed {
                    s.ready = Some(world.to_string());
                } else {
                    s.message = Some(format!(
                        "No map files for \"{world}\" were found in the game or Workshop folders."
                    ));
                }
            });
            return;
        };
        match maps::load(&maps::maps_dir().join(&source.id)) {
            Ok(mut pack) if pack.meta.source == source.fingerprint() => {
                self.refresh_pois(&source, &mut pack);
                self.update(|s| s.ready = Some(source.id.clone()));
            }
            _ => self.import(&source, true),
        }
    }

    /// Rebuilds the points of interest if what they come from (or how) has changed.
    fn refresh_pois(&self, source: &WorldSource, pack: &mut maps::MapPack) {
        if pack.meta.pois_source == source.pois_fingerprint() {
            return;
        }
        log::info!("updating the points of interest of {}", pack.meta.name);
        match import::refresh_pois(source, pack) {
            Ok(()) => self.update(|s| s.generation += 1),
            Err(e) => log::warn!("{}: {e:#}", pack.meta.name),
        }
    }

    fn import(&self, source: &WorldSource, show: bool) {
        let job = |done, total| Job {
            id: source.id.clone(),
            name: source.name.clone(),
            done,
            total,
        };
        self.update(|s| {
            s.job = Some(job(0, 1));
            s.message = None;
        });
        let result = import::import_world(source, &|p| {
            self.state.lock().unwrap().job = Some(job(p.done, p.total));
            self.ctx.request_repaint();
        });
        self.update(|s| {
            s.job = None;
            s.generation += 1;
            match result {
                Ok(_) => {
                    if show {
                        s.ready = Some(source.id.clone());
                    }
                }
                Err(e) => {
                    log::error!("importing {}: {e:#}", source.name);
                    s.message = Some(format!("Importing {} failed: {e:#}", source.name));
                }
            }
        });
    }

    /// Rebuilds maps imported by an older version (for example, before points of interest).
    fn upgrade_old_imports(&self) {
        let catalog = self.catalog();
        for pack in maps::load_all() {
            if pack.meta.format == 0 {
                continue;
            }
            let mods: Vec<String> = pack.meta.mod_id.iter().cloned().collect();
            let Some(source) = catalog.best(&pack.meta.id, &mods) else {
                continue;
            };
            if pack.meta.format < catalog::IMPORT_VERSION {
                log::info!("updating {} to the current import format", pack.meta.name);
                self.import(&source, false);
            } else {
                let mut pack = pack;
                self.refresh_pois(&source, &mut pack);
            }
        }
    }
}
