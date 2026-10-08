//! Builds map packs from the game's own files.
//!
//! Every DayZ terrain ships its satellite texture as a grid of overlapping PAA tiles
//! (`layers\S_XXX_YYY_lco.paa`). Importing crops the overlap, writes a tile pyramid that the
//! overlay can stream at any zoom level, and extracts points of interest.

pub mod catalog;
pub mod layout;
mod lzo;
mod lzss;
pub mod paa;
pub mod pbo;
pub mod poi;
pub mod rap;
pub mod water;
pub mod wrp;

use anyhow::{Context, Result, bail};
use image::{RgbaImage, imageops};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::maps::{self, LayerMeta, MapMeta, MapPack};
pub use catalog::{Catalog, TileSource, WorldSource};
use layout::TileLayout;

pub const SATELLITE: &str = "satellite";
/// The layer `import-image` makes.
const PICTURE: &str = "picture";

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// Keeps two imports of one map (the overlay's and `dayz-map import`, say) from working in the
/// same folder at once. A lock the system holds on `import.lock`, so it goes away however the
/// process ends.
struct ImportLock(#[allow(dead_code)] std::fs::File);

impl ImportLock {
    /// Takes the map's lock, waiting for another import of it to finish. Also says whether it
    /// waited, in which case the map may now be up to date.
    fn take(map_dir: &Path) -> Result<(Self, bool)> {
        std::fs::create_dir_all(map_dir)?;
        let path = map_dir.join("import.lock");
        // (Never deleted: a process waiting on the old file would lock it while another
        // creates and locks a new one.)
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok((Self(file), false)),
            Err(std::fs::TryLockError::WouldBlock) => {
                log::info!("waiting for another import of {}", map_dir.display());
                file.lock()
                    .with_context(|| format!("locking {}", path.display()))?;
                Ok((Self(file), true))
            }
            Err(std::fs::TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("locking {}", path.display()))
            }
        }
    }
}

/// Imports a world into its map pack, replacing an older import.
pub fn import_world(source: &WorldSource, progress: &(dyn Fn(Progress) + Sync)) -> Result<MapPack> {
    let tiles = &source.satellite;
    let layout = TileLayout::from_rvmats(&tiles.pbo)
        .with_context(|| format!("reading the tile layout of {}", source.name))?;
    let world_size = source
        .world_size
        .unwrap_or_else(|| layout.coverage(tiles.grid))
        .round();
    maps::check_world_size(world_size).with_context(|| source.name.clone())?;
    log::info!(
        "importing {} from {}: {world_size:.0} m, {} tiles per side",
        source.name,
        source.source_label(),
        tiles.grid
    );

    let dir = maps::maps_dir().join(&source.id);
    let (_lock, waited) = ImportLock::take(&dir)?;
    if waited
        && let Ok(pack) = maps::load(&dir)
        && source.is_current(&pack)
    {
        return Ok(pack);
    }
    // Points of interest first: a game file that can't be read stops the import before the
    // slow part. Everything is written beside the old map, which stays in use until the new
    // map.toml is saved.
    let generation = maps::generation();
    let pois = pois_json(source)?;
    let (layer, mut tiles_written) = build_layer(tiles, &layout, &dir, &generation, progress)?;
    let (pois_file, mut pois_written) = write_pois(&dir, &generation, &pois)?;
    sync_tiles(&dir.join(layer.dir.as_deref().unwrap_or(SATELLITE)))?;

    let old = maps::load(&dir).ok().map(|p| p.meta);
    let meta = MapMeta {
        id: source.id.clone(),
        name: source.name.clone(),
        world_size,
        format: catalog::IMPORT_VERSION,
        source: source.fingerprint(),
        mod_id: source.mod_id.clone(),
        pois_source: source.pois_fingerprint(),
        // A picture the user imported stays in front: they chose it over the satellite.
        layers: maps::recorded_layers(&dir)
            .into_iter()
            .filter(|l| l.id == PICTURE)
            .chain([layer])
            .collect(),
        pois: Some(pois_file),
    };
    maps::save_meta(&dir, &meta)?;
    tiles_written.keep();
    pois_written.keep();
    maps::remove_unused(&dir, old.as_ref(), &meta);
    Ok(MapPack { meta, dir })
}

/// Rebuilds only the points of interest of an imported map; returns whether it did (another
/// import may have done it meanwhile).
pub fn refresh_pois(source: &WorldSource, pack: &mut MapPack) -> Result<bool> {
    let (_lock, _) = ImportLock::take(&pack.dir)?;
    // Another import may have rewritten it since it was loaded, maybe from another copy of the
    // terrain (an experimental build): leave that one's points of interest alone.
    let current = maps::load(&pack.dir)?;
    if !source.is_current(&current) {
        log::info!("{} was reimported meanwhile", current.meta.name);
        *pack = current;
        return Ok(false);
    }
    if current.meta.pois_source == source.pois_fingerprint() {
        // Done by another import meanwhile; saving again would only make the overlay reload.
        *pack = current;
        return Ok(false);
    }
    let old = current.meta.clone();
    let mut meta = current.meta;
    let (file, mut written) = write_pois(&pack.dir, &maps::generation(), &pois_json(source)?)?;
    meta.pois = Some(file);
    meta.pois_source = source.pois_fingerprint();
    maps::save_meta(&pack.dir, &meta)?;
    written.keep();
    maps::remove_unused(&pack.dir, Some(&old), &meta);
    pack.meta = meta;
    Ok(true)
}

/// Writes a new points-of-interest file (unused until map.toml names it), deleted again unless
/// kept.
fn write_pois(dir: &Path, generation: &str, json: &[u8]) -> Result<(String, RemoveOnDrop)> {
    let name = format!("pois-{generation}.json");
    let path = dir.join(&name);
    let guard = RemoveOnDrop(Some(path.clone()));
    maps::write_synced(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok((name, guard))
}

/// The world's points of interest, as `pois.json` holds them.
fn pois_json(source: &WorldSource) -> Result<Vec<u8>> {
    let (mut markers, zones) = source
        .economy
        .as_ref()
        .map(|e| e.read_all().map(|files| poi::from_economy(&files)))
        .transpose()
        .context("reading the economy files")?
        .unwrap_or_default();
    let water = water_markers(source)?;
    if water.iter().any(|m| m.kind == poi::Kind::Water) {
        // The terrain lists every well; the economy only the ones that spawn loot.
        markers.retain(|m| m.kind != poi::Kind::Water);
    }
    markers.extend(water);
    // JSON can't hold NaN or infinity (a bad number in a mod's files), and one would make the
    // whole file unreadable.
    markers.retain(|m| m.x.is_finite() && m.z.is_finite());
    // Mods sometimes park objects far off the terrain (one Deer Isle build has water at
    // x = -180000); a marker there would only stretch the map's bounds.
    if let Some(size) = source.world_size {
        let on_map = |c: f32| (0.0..=size as f32).contains(&c);
        markers.retain(|m| on_map(m.x) && on_map(m.z));
    }
    let pois = poi::Pois {
        places: source
            .places
            .iter()
            .filter(|p| p.x.is_finite() && p.z.is_finite())
            .cloned()
            .collect(),
        markers,
        zones: zones
            .into_iter()
            .filter(|z| z.x.is_finite() && z.z.is_finite() && z.radius.is_finite())
            .collect(),
    };
    log::info!(
        "{}: {} places, {} markers, {} zones",
        source.name,
        pois.places.len(),
        pois.markers.len(),
        pois.zones.len()
    );
    Ok(serde_json::to_vec(&pois)?)
}

/// The terrain's wells and fresh water. A world file that can't be read right now is an error
/// (the caller keeps the markers it has); one that's corrupt just gives none.
fn water_markers(source: &WorldSource) -> Result<Vec<poi::Marker>> {
    let Some(terrain) = &source.terrain else {
        return Ok(Vec::new());
    };
    let data = match terrain.read() {
        Ok(data) => data,
        Err(e) if pbo::is_io(&e) => return Err(e.context("reading the world file")),
        Err(e) => {
            log::warn!("{}: reading the world file: {e:#}", source.name);
            return Ok(Vec::new());
        }
    };
    let Some(objects) = wrp::objects(&data, |m| water::fresh_water(m).is_some()) else {
        log::warn!("{}: couldn't read the world file's objects", source.name);
        return Ok(Vec::new());
    };
    drop(data);
    let wells = water::well_classes(&source.scripts)?;
    let markers = water::markers(&objects, &wells);
    log::info!(
        "{}: {} wells, {} fresh water",
        source.name,
        markers
            .iter()
            .filter(|m| m.kind == poi::Kind::Water)
            .count(),
        markers
            .iter()
            .filter(|m| m.kind == poi::Kind::FreshWater)
            .count()
    );
    Ok(markers)
}

/// Builds the satellite layer in a new folder, deleted again unless kept.
fn build_layer(
    source: &TileSource,
    layout: &TileLayout,
    map_dir: &Path,
    generation: &str,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<(LayerMeta, RemoveOnDrop)> {
    // Empty tiles are stored tiny, so take the size from the largest one.
    let sample = source
        .tiles
        .values()
        .max_by_key(|&&i| source.pbo.entries[i].size)
        .context("no tiles")?;
    let src_px = paa::decode(&source.pbo.read(&source.pbo.entries[*sample])?)?.width();
    let overlap_px = (layout.overlap_frac * f64::from(src_px)).round() as u32;
    let tile_px = src_px - 2 * overlap_px;
    // Every tile is scaled to the largest one's size, so a mod pairing one huge tile with a big
    // grid of tiny ones would take hours and gigabytes. Real maps are about 16,000 px across,
    // in tiles of 320 to 480 px, at most 64 to a side.
    if tile_px > 1024 || source.grid > 256 || u64::from(tile_px) * u64::from(source.grid) > 40_960 {
        bail!(
            "the satellite image would be {} tiles of {tile_px} px across, too large to be a \
             real map",
            source.grid
        );
    }
    let max_level = source.grid.next_power_of_two().trailing_zeros();
    let folder = format!("{SATELLITE}-{generation}");
    let meta = LayerMeta {
        id: SATELLITE.into(),
        dir: Some(folder.clone()),
        name: "Satellite".into(),
        tile_px,
        grid: source.grid,
        max_level,
        ext: "jpg".into(),
        tile_m: layout.step_m,
        origin: [layout.left, layout.top],
    };

    let work_dir = map_dir.join(&folder);
    // Removed if anything fails (or panics) before the map uses it; a map with a bad tile would
    // otherwise leave hundreds of MB behind on every attempt.
    let work = RemoveOnDrop(Some(work_dir.clone()));
    for level in 0..=max_level {
        std::fs::create_dir_all(work_dir.join(level.to_string()))?;
    }

    let builder = Builder {
        source,
        meta: &meta,
        overlap_px,
        dir: &work_dir,
        done: AtomicUsize::new(0),
        total: (0..=max_level)
            .map(|l| (meta.grid_at(l) as usize).pow(2))
            .sum(),
        progress,
    };

    // Build the full-resolution subtrees in parallel, then the few top levels from their roots.
    let split = max_level.min(2);
    let tasks: Vec<(u32, u32)> = grid_coords(meta.grid_at(split)).collect();
    let next = AtomicUsize::new(0);
    // Set when a thread fails, so the others stop instead of building the rest of the map.
    let failed = AtomicBool::new(false);
    let results = Mutex::new(HashMap::new());
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    while !failed.load(Ordering::Relaxed)
                        && let Some(&(x, y)) = tasks.get(next.fetch_add(1, Ordering::Relaxed))
                    {
                        let image = builder.build(split, x, y, None).inspect_err(|_| {
                            failed.store(true, Ordering::Relaxed);
                        })?;
                        results.lock().unwrap().insert((x, y), image);
                    }
                    Ok(())
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("import thread panicked")?;
        }
        Ok(())
    })?;
    let mut below = results.into_inner().unwrap();
    for level in (0..split).rev() {
        let mut current = HashMap::new();
        for (x, y) in grid_coords(meta.grid_at(level)) {
            current.insert((x, y), builder.build(level, x, y, Some(&mut below))?);
        }
        below = current;
    }

    Ok((meta, work))
}

/// A folder or file to delete when dropped, unless kept.
struct RemoveOnDrop(Option<std::path::PathBuf>);

impl RemoveOnDrop {
    fn keep(&mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
        }
    }
}

fn grid_coords(grid: u32) -> impl Iterator<Item = (u32, u32)> {
    (0..grid).flat_map(move |y| (0..grid).map(move |x| (x, y)))
}

struct Builder<'a> {
    source: &'a TileSource,
    meta: &'a LayerMeta,
    overlap_px: u32,
    dir: &'a Path,
    done: AtomicUsize,
    total: usize,
    progress: &'a (dyn Fn(Progress) + Sync),
}

type Tile = Option<RgbaImage>;

impl Builder<'_> {
    /// Writes tile `(level, x, y)` and returns its pixels, or `None` if the area has no data.
    /// Children come from `below` when given, otherwise they are built recursively.
    fn build(
        &self,
        level: u32,
        x: u32,
        y: u32,
        mut below: Option<&mut HashMap<(u32, u32), Tile>>,
    ) -> Result<Tile> {
        let tile = if level == self.meta.max_level {
            self.source_tile(x, y)?
        } else {
            let px = self.meta.tile_px;
            let mut canvas: Option<RgbaImage> = None;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (cx, cy) = (2 * x + dx, 2 * y + dy);
                let child = match below.as_deref_mut() {
                    Some(map) => map.remove(&(cx, cy)).flatten(),
                    None if cx < self.meta.grid_at(level + 1)
                        && cy < self.meta.grid_at(level + 1) =>
                    {
                        self.build(level + 1, cx, cy, None)?
                    }
                    None => None,
                };
                if let Some(child) = child {
                    let canvas = canvas.get_or_insert_with(|| RgbaImage::new(2 * px, 2 * px));
                    imageops::replace(canvas, &child, i64::from(dx * px), i64::from(dy * px));
                }
            }
            canvas.map(|c| downsample(&c))
        };
        if let Some(image) = &tile {
            self.save(image, level, x, y)?;
        }
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        (self.progress)(Progress {
            done,
            total: self.total,
        });
        Ok(tile)
    }

    fn source_tile(&self, x: u32, y: u32) -> Result<Tile> {
        let Some(&index) = self.source.tiles.get(&(x, y)) else {
            return Ok(None);
        };
        let entry = &self.source.pbo.entries[index];
        let mut image = paa::decode(&self.source.pbo.read(entry)?)
            .with_context(|| format!("decoding {}", entry.name))?;
        let px = self.meta.tile_px;
        let full = px + 2 * self.overlap_px;
        if image.height() != image.width() || !full.is_multiple_of(image.width()) {
            bail!(
                "{} has an unexpected size {}x{}",
                entry.name,
                image.width(),
                image.height()
            );
        }
        // Areas with nothing on them (open sea) are stored as tiny solid tiles.
        if image.width() != full {
            image = imageops::resize(&image, full, full, imageops::FilterType::Nearest);
        }
        Ok(Some(
            imageops::crop_imm(&image, self.overlap_px, self.overlap_px, px, px).to_image(),
        ))
    }

    fn save(&self, image: &RgbaImage, level: u32, x: u32, y: u32) -> Result<()> {
        let path = self
            .dir
            .join(level.to_string())
            .join(format!("{x}_{y}.{}", self.meta.ext));
        save_tile(image, &path)
    }
}

pub fn save_tile(image: &RgbaImage, path: &Path) -> Result<()> {
    let opaque = image.pixels().all(|p| p[3] == 255);
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    if path.extension().is_some_and(|e| e == "jpg") {
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, 88).encode_image(&rgb)?;
    } else if opaque {
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
        rgb.write_with_encoder(image::codecs::png::PngEncoder::new(&mut file))?;
    } else {
        image.write_with_encoder(image::codecs::png::PngEncoder::new(&mut file))?;
    }
    // Dropping the writer would hide a failed final write (a full disk, say).
    std::io::Write::flush(&mut file)?;
    // Start writing it to disk now, without waiting, so `sync_tiles` finds little left to do.
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        unsafe extern "C" {
            fn sync_file_range(fd: std::ffi::c_int, offset: i64, len: i64, flags: u32) -> i32;
        }
        const SYNC_FILE_RANGE_WRITE: u32 = 2;
        // SAFETY: a plain system call on a descriptor we own for the call's duration; only a
        // hint, so its result doesn't matter.
        unsafe { sync_file_range(file.get_ref().as_raw_fd(), 0, 0, SYNC_FILE_RANGE_WRITE) };
    }
    Ok(())
}

/// Waits until the tiles just written to `folder` are on disk, so once map.toml points to them a
/// crash can't leave them empty. Several at a time: the disk commits them together.
fn sync_tiles(folder: &Path) -> Result<()> {
    let mut files = Vec::new();
    for level in std::fs::read_dir(folder)? {
        for tile in std::fs::read_dir(level?.path())? {
            files.push(tile?.path());
        }
    }
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| -> std::io::Result<()> {
                    while let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        // (Windows only flushes a file opened for writing.)
                        std::fs::OpenOptions::new()
                            .write(true)
                            .open(path)?
                            .sync_all()?;
                    }
                    Ok(())
                })
            })
            .collect();
        workers
            .into_iter()
            .try_for_each(|w| w.join().expect("sync thread panicked"))
    })
    .context("syncing the new tiles")
}

/// Halves an image with a 2x2 box filter.
fn downsample(image: &RgbaImage) -> RgbaImage {
    let (w, h) = (image.width() / 2, image.height() / 2);
    RgbaImage::from_fn(w, h, |x, y| {
        let mut sum = [0u32; 4];
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let p = image.get_pixel(2 * x + dx, 2 * y + dy);
            for c in 0..4 {
                sum[c] += u32::from(p[c]);
            }
        }
        image::Rgba(sum.map(|s| ((s + 2) / 4) as u8))
    })
}

/// Imports a single picture of a map (for terrains whose files can't be read), cutting it into
/// the same pyramid format. The picture must cover the whole terrain, north up.
pub fn import_image(id: &str, name: &str, world_size: f64, picture: &Path) -> Result<MapPack> {
    const TILE_PX: u32 = 512;
    if !maps::valid_id(id) {
        anyhow::bail!("the id must be lower-case letters, digits, `_` or `-`, such as `mymap`");
    }
    maps::check_world_size(world_size)?;
    // Squared up and held whole while cutting: 16384 px across is already 1 GB.
    const MAX_SIDE: u32 = 16384;
    let (w, h) = image::ImageReader::open(picture)?
        .with_guessed_format()?
        .into_dimensions()
        .with_context(|| format!("reading {}", picture.display()))?;
    if w.max(h) > MAX_SIDE {
        bail!("the picture is {w}x{h} px; scale it down to at most {MAX_SIDE} px across");
    }
    let mut reader = image::ImageReader::open(picture)?.with_guessed_format()?;
    reader.no_limits();
    let image = reader
        .decode()
        .with_context(|| format!("reading {}", picture.display()))?;
    let grid = image.width().max(image.height()).div_ceil(TILE_PX);
    let side = grid * TILE_PX;
    let image = image
        .resize_exact(side, side, imageops::FilterType::Lanczos3)
        .into_rgba8();
    let max_level = grid.next_power_of_two().trailing_zeros();
    let dir = maps::maps_dir().join(id);
    let (_lock, _) = ImportLock::take(&dir)?;
    // Built beside the old picture, which stays in use until the new map.toml is saved.
    let folder = format!("{PICTURE}-{}", maps::generation());
    let layer = LayerMeta {
        id: PICTURE.into(),
        dir: Some(folder.clone()),
        name: "Picture".into(),
        tile_px: TILE_PX,
        grid,
        max_level,
        ext: "png".into(),
        tile_m: world_size / f64::from(grid),
        origin: [0.0, world_size],
    };
    let work_dir = dir.join(&folder);
    let mut work = RemoveOnDrop(Some(work_dir.clone()));
    let mut current = image;
    for level in (0..=max_level).rev() {
        let level_dir = work_dir.join(level.to_string());
        std::fs::create_dir_all(&level_dir)?;
        let tiles = layer.grid_at(level);
        let padded_side = tiles * TILE_PX;
        if current.width() < padded_side {
            let mut padded = RgbaImage::new(padded_side, padded_side);
            imageops::replace(&mut padded, &current, 0, 0);
            current = padded;
        }
        for (x, y) in grid_coords(tiles) {
            let tile = imageops::crop_imm(&current, x * TILE_PX, y * TILE_PX, TILE_PX, TILE_PX);
            save_tile(&tile.to_image(), &level_dir.join(format!("{x}_{y}.png")))?;
        }
        current = downsample(&current);
    }
    sync_tiles(&work_dir)?;
    let old = maps::load(&dir).ok().map(|p| p.meta);
    let mut meta = old.clone().unwrap_or_else(|| MapMeta {
        id: id.into(),
        name: name.into(),
        world_size,
        format: 0,
        source: String::new(),
        mod_id: None,
        pois_source: String::new(),
        layers: Vec::new(),
        pois: None,
    });
    meta.name = name.into();
    meta.world_size = world_size;
    // The overlay draws the first layer: the picture replaces the satellite view.
    meta.layers.retain(|l| l.id != layer.id);
    meta.layers.insert(0, layer);
    maps::save_meta(&dir, &meta)?;
    work.keep();
    maps::remove_unused(&dir, old.as_ref(), &meta);
    Ok(MapPack { meta, dir })
}
