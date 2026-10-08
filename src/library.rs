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
    /// The game left its map: stop retrying it.
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
    /// retries the game's map if it couldn't be found before.
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
        // The game's map, while it isn't ready and up to date: a rescan tries it again.
        let mut unsettled = None;
        for request in rx {
            // A bug tripped by some mod's files mustn't stop the library for the whole session.
            let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.handle(request, &mut upgraded, &mut unsettled)
            }));
            if handled.is_err() {
                unsettled = None;
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
        unsettled: &mut Option<(String, Vec<String>)>,
    ) {
        match request {
            Request::Scan => {
                self.scan();
                if !*upgraded {
                    *upgraded = true;
                    self.upgrade_old_imports();
                }
                // Maybe newly downloaded, or readable again.
                if let Some((world, mods)) = unsettled.take()
                    && !self.ensure(&world, &mods, true)
                {
                    *unsettled = Some((world, mods));
                }
            }
            Request::Forget => *unsettled = None,
            Request::Ensure { world, mods } => {
                *unsettled = (!self.ensure(&world, &mods, false)).then_some((world, mods));
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

    /// Returns whether that settled it: the map is ready and up to date, or there's nothing
    /// more to try (no files, but an imported picture).
    /// `retry` after a rescan: the catalog is fresh, and the user may be looking at another map,
    /// so an old import is only shown again if it was rebuilt.
    fn ensure(&self, world: &str, mods: &[String], retry: bool) -> bool {
        let mut catalog = self.catalog();
        // A newly downloaded mod: maybe this map, or another copy of it that the server uses.
        if !retry
            && (catalog.best(world, mods).is_none()
                || mods.iter().any(|m| !catalog.mods.contains(m)))
        {
            catalog = self.scan();
        }
        let Some(source) = catalog.best(world, mods).map(|s| self.fresh(s, mods)) else {
            log::warn!("no map files found for {world}");
            let installed = maps::load(&maps::maps_dir().join(world)).ok();
            // Only a picture the user imported is final; an old import is checked again when
            // the files turn up (the game folder picked, say).
            let settled = installed.as_ref().is_some_and(|p| p.meta.format == 0);
            let installed = installed.is_some();
            self.update(|s| {
                if installed {
                    if !retry {
                        s.ready = Some(world.to_string());
                    }
                } else {
                    s.message = Some(format!(
                        "No map files for \"{world}\" were found in the game or Workshop folders."
                    ));
                }
            });
            return settled;
        };
        match maps::load(&maps::maps_dir().join(&source.id)) {
            Ok(mut pack) if source.is_current(&pack) => {
                let refreshed = self.refresh_pois(&source, &mut pack);
                self.update(|s| s.ready = Some(source.id.clone()));
                refreshed
            }
            installed => {
                let imported = self.import(&source, true);
                // An installed picture still works if the terrain's own files don't.
                if !imported && installed.is_ok() {
                    self.update(|s| s.ready = Some(source.id.clone()));
                }
                imported
            }
        }
    }

    /// Rebuilds the points of interest if what they come from (or how) has changed; returns
    /// whether they're up to date.
    fn refresh_pois(&self, source: &WorldSource, pack: &mut maps::MapPack) -> bool {
        if pack.meta.pois_source == source.pois_fingerprint() {
            return true;
        }
        log::info!("updating the points of interest of {}", pack.meta.name);
        match import::refresh_pois(source, pack) {
            Ok(()) => {
                self.update(|s| s.generation += 1);
                true
            }
            Err(e) => {
                log::warn!("{}: {e:#}", pack.meta.name);
                false
            }
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
