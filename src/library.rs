//! Keeps the installed maps in step with the game: finds terrains in the game and Workshop
//! folders, imports the one the game is on (or re-imports it after a mod update), and upgrades
//! maps built by older versions. All the work happens on one background thread.

use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use crate::import::{self, Catalog, WorldSource, catalog};
use crate::{maps, paths};

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
    /// The game left its map: rescans stop checking it.
    Forget,
    Ensure {
        world: String,
        mods: Vec<String>,
    },
    Import {
        id: String,
    },
}

pub struct Library {
    requests: Sender<Request>,
    pub state: Arc<Mutex<LibraryState>>,
}

impl Library {
    pub fn new(ctx: egui::Context) -> Self {
        let (requests, rx) = crossbeam_channel::unbounded();
        let state = Arc::new(Mutex::new(LibraryState::default()));
        let worker = Worker {
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

    /// Looks for the game again (after the user picked its folder) and rescans, which also
    /// checks the game's map again (it may not have been found before).
    pub fn relocate(&self, config: &crate::config::Config) {
        paths::refresh(config);
        self.rescan();
    }

    /// Makes sure `world` is imported and current, then marks it ready to show.
    pub fn ensure(&self, world: &str, mods: &[String]) {
        let _ = self.requests.send(Request::Ensure {
            world: world.to_lowercase(),
            mods: mods.to_vec(),
        });
    }

    /// The game is no longer on a map (main menu, or closed).
    pub fn forget(&self) {
        let _ = self.requests.send(Request::Forget);
    }

    pub fn rescan(&self) {
        let _ = self.requests.send(Request::Scan);
    }

    pub fn import(&self, id: &str) {
        let _ = self.requests.send(Request::Import { id: id.to_string() });
    }
}

struct Worker {
    state: Arc<Mutex<LibraryState>>,
    ctx: egui::Context,
}

impl Worker {
    fn run(self, rx: Receiver<Request>) {
        let mut upgraded = false;
        // The game's map (and the server's mods): a rescan checks it again.
        let mut wanted = None;
        for request in rx {
            // A bug tripped by some mod's files mustn't stop the library for the whole session.
            let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.handle(request, &mut upgraded, &mut wanted)
            }));
            if handled.is_err() {
                wanted = None;
                self.update(|s| {
                    s.job = None;
                    s.scanning = false;
                    s.message = Some("Reading the map files failed; see the log.".into());
                });
            }
        }
    }

    fn handle(
        &self,
        request: Request,
        upgraded: &mut bool,
        wanted: &mut Option<(String, Vec<String>)>,
    ) {
        match request {
            Request::Scan => {
                self.scan();
                if !*upgraded {
                    *upgraded = true;
                    self.upgrade_old_imports();
                }
                // Its files may have changed, or turned up, or become readable again.
                if let Some((world, mods)) = wanted {
                    self.ensure(world, mods, true);
                }
            }
            Request::Forget => *wanted = None,
            Request::Ensure { world, mods } => {
                self.ensure(&world, &mods, false);
                *wanted = Some((world, mods));
            }
            Request::Import { id } => {
                if let Some(source) = self.catalog().best(&id, &[]) {
                    self.import(&self.fresh(source, &[]), false);
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
        // Steam folders too: the Workshop folder appears with the first mod download.
        let paths = paths::rediscover();
        let catalog = Arc::new(catalog::scan(&catalog::roots(&paths)));
        log::info!(
            "found {} terrains in {:.1?}",
            catalog.unique().len(),
            start.elapsed()
        );
        self.update(|s| {
            s.scanning = false;
            s.catalog = Some(catalog.clone());
            // Also picks up maps another program installed (`dayz-map import-image`).
            s.generation += 1;
        });
        catalog
    }

    /// `source`, rescanned if a mod update changed its files since the scan (the archive
    /// offsets the scan recorded would be wrong).
    fn fresh(&self, source: WorldSource, mods: &[String]) -> WorldSource {
        if !source.changed_since_scan() {
            return source;
        }
        log::info!("{} changed since the last scan; rescanning", source.name);
        self.scan().best(&source.id, mods).unwrap_or(source)
    }

    fn catalog(&self) -> Arc<Catalog> {
        let existing = self.state.lock().unwrap().catalog.clone();
        existing.unwrap_or_else(|| self.scan())
    }

    /// `again` after a rescan: the catalog is fresh, and the user may be looking at another map,
    /// so the game's map is only shown again if it was rebuilt.
    fn ensure(&self, world: &str, mods: &[String], again: bool) {
        let mut catalog = self.catalog();
        // A newly downloaded mod: maybe this map, or another copy of it that the server uses.
        if !again
            && (catalog.best(world, mods).is_none()
                || mods.iter().any(|m| !catalog.mods.contains(m)))
        {
            catalog = self.scan();
        }
        let Some(source) = catalog.best(world, mods).map(|s| self.fresh(s, mods)) else {
            log::warn!("no map files found for {world}");
            let installed = maps::load(&maps::maps_dir().join(world)).is_ok();
            self.update(|s| {
                if installed {
                    if !again {
                        s.ready = Some(world.to_string());
                    }
                } else {
                    s.message = Some(format!(
                        "No map files for \"{world}\" were found in the game or Workshop folders."
                    ));
                }
            });
            return;
        };
        match maps::load(&maps::maps_dir().join(&source.id)) {
            Ok(mut pack) if source.is_current(&pack) => {
                self.refresh_pois(&source, &mut pack);
                if !again {
                    self.update(|s| s.ready = Some(source.id.clone()));
                }
            }
            installed => {
                // An installed picture still works if the terrain's own files don't.
                if !self.import(&source, true) && installed.is_ok() && !again {
                    self.update(|s| s.ready = Some(source.id.clone()));
                }
            }
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

    /// Imports a world; returns whether it worked.
    fn import(&self, source: &WorldSource, show: bool) -> bool {
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
        let ok = result.is_ok();
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
        ok
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
            let source = self.fresh(source, &mods);
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
