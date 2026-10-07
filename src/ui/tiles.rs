//! Streams map tiles from disk into GPU textures on background threads.

use crossbeam_channel::{Receiver, Sender};
use egui::{ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Roughly 900 KB of GPU memory per 480 px tile.
const MAX_TEXTURES: usize = 400;
const WORKERS: usize = 4;

enum Slot {
    Pending,
    Missing,
    Loaded(TextureHandle),
}

struct Entry {
    slot: Slot,
    last_used: u64,
}

enum Loaded {
    Image(ColorImage),
    Missing,
    /// No longer on screen by the time a worker got to it.
    Skipped,
}

/// Tiles requested this frame and last frame; workers skip anything else.
#[derive(Default)]
struct Wanted {
    current: HashSet<PathBuf>,
    previous: HashSet<PathBuf>,
}

pub struct TileCache {
    entries: HashMap<PathBuf, Entry>,
    frame: u64,
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, Loaded)>,
    wanted: Arc<Mutex<Wanted>>,
}

impl TileCache {
    pub fn new(ctx: &egui::Context) -> Self {
        let (requests, request_rx) = crossbeam_channel::unbounded::<PathBuf>();
        let (result_tx, results) = crossbeam_channel::unbounded();
        let wanted = Arc::new(Mutex::new(Wanted::default()));
        for i in 0..WORKERS {
            let (rx, tx, wanted, ctx) = (
                request_rx.clone(),
                result_tx.clone(),
                wanted.clone(),
                ctx.clone(),
            );
            std::thread::Builder::new()
                .name(format!("tiles-{i}"))
                .spawn(move || {
                    for path in rx {
                        let still_wanted = {
                            let w = wanted.lock().unwrap();
                            w.current.contains(&path) || w.previous.contains(&path)
                        };
                        let loaded = if still_wanted {
                            load(&path)
                        } else {
                            Loaded::Skipped
                        };
                        if tx.send((path, loaded)).is_err() {
                            break;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawning tile loader");
        }
        Self {
            entries: HashMap::new(),
            frame: 0,
            requests,
            results,
            wanted,
        }
    }

    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        {
            let mut w = self.wanted.lock().unwrap();
            let w = &mut *w;
            std::mem::swap(&mut w.previous, &mut w.current);
            w.current.clear();
        }
        for (path, loaded) in self.results.try_iter() {
            match loaded {
                Loaded::Image(image) => {
                    let name = path.to_string_lossy();
                    let texture = ctx.load_texture(name, image, TextureOptions::LINEAR);
                    if let Some(entry) = self.entries.get_mut(&path) {
                        entry.slot = Slot::Loaded(texture);
                    }
                }
                Loaded::Missing => {
                    if let Some(entry) = self.entries.get_mut(&path) {
                        entry.slot = Slot::Missing;
                    }
                }
                Loaded::Skipped => {
                    self.entries.remove(&path);
                }
            }
        }
    }

    /// Returns the tile's texture if it's ready, queueing a load otherwise.
    /// `Some(None)` means the tile doesn't exist (no data there).
    pub fn get(&mut self, path: &Path) -> Option<Option<egui::TextureId>> {
        let frame = self.frame;
        self.wanted.lock().unwrap().current.insert(path.to_owned());
        let entry = self.entries.entry(path.to_owned()).or_insert_with(|| {
            let _ = self.requests.send(path.to_owned());
            Entry {
                slot: Slot::Pending,
                last_used: frame,
            }
        });
        entry.last_used = frame;
        match &entry.slot {
            Slot::Pending => None,
            Slot::Missing => Some(None),
            Slot::Loaded(texture) => Some(Some(texture.id())),
        }
    }

    /// Returns the tile only if it's already loaded, without requesting it.
    pub fn peek(&mut self, path: &Path) -> Option<egui::TextureId> {
        let entry = self.entries.get_mut(path)?;
        match &entry.slot {
            Slot::Loaded(texture) => {
                entry.last_used = self.frame;
                Some(texture.id())
            }
            _ => None,
        }
    }

    pub fn end_frame(&mut self) {
        let loaded = self
            .entries
            .values()
            .filter(|e| matches!(e.slot, Slot::Loaded(_)))
            .count();
        if loaded <= MAX_TEXTURES {
            return;
        }
        let mut old: Vec<(u64, PathBuf)> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e.slot, Slot::Loaded(_)) && e.last_used + 1 < self.frame)
            .map(|(p, e)| (e.last_used, p.clone()))
            .collect();
        old.sort();
        for (_, path) in old.into_iter().take(loaded - MAX_TEXTURES) {
            self.entries.remove(&path);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Drops every tile sharper than `max_level`, keeping the cheap overview levels warm.
    pub fn trim(&mut self, max_level: u32) {
        self.entries.retain(|path, _| {
            let level = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str()?.parse().ok());
            level.is_some_and(|l: u32| l <= max_level)
        });
    }
}

fn load(path: &PathBuf) -> Loaded {
    let image = match image::open(path) {
        Ok(image) => image.into_rgba8(),
        Err(image::ImageError::IoError(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Loaded::Missing;
        }
        Err(e) => {
            log::warn!("{}: {e}", path.display());
            return Loaded::Missing;
        }
    };
    let size = [image.width() as usize, image.height() as usize];
    Loaded::Image(ColorImage::from_rgba_unmultiplied(size, image.as_raw()))
}
