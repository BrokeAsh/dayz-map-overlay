//! Builds map packs from the game's own files.
//!
//! Every DayZ terrain ships its satellite texture as a grid of overlapping PAA tiles
//! (`layers\S_XXX_YYY_lco.paa`). Importing crops the overlap, writes a tile pyramid that the
//! overlay can stream at any zoom level, and extracts points of interest.

pub mod catalog;
pub mod layout;
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
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::maps::{self, LayerMeta, MapMeta, MapPack};
pub use catalog::{Catalog, TileSource, WorldSource};
use layout::TileLayout;

pub const SATELLITE: &str = "satellite";

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
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
    log::info!(
        "importing {} from {}: {world_size:.0} m, {} tiles per side",
        source.name,
        source.source_label(),
        tiles.grid
    );

    let dir = maps::maps_dir().join(&source.id);
    std::fs::create_dir_all(&dir)?;
    let layer = build_layer(tiles, &layout, &dir, progress)?;

    write_pois(source, &dir)?;

    let meta = MapMeta {
        id: source.id.clone(),
        name: source.name.clone(),
        world_size,
        format: catalog::IMPORT_VERSION,
        source: source.fingerprint(),
        mod_id: source.mod_id.clone(),
        pois_source: source.pois_fingerprint(),
        layers: vec![layer],
    };
    maps::save_meta(&dir, &meta)?;
    Ok(MapPack { meta, dir })
}

/// Rebuilds only the points of interest of an imported map.
pub fn refresh_pois(source: &WorldSource, pack: &mut MapPack) -> Result<()> {
    write_pois(source, &pack.dir)?;
    pack.meta.pois_source = source.pois_fingerprint();
    maps::save_meta(&pack.dir, &pack.meta)
}

fn write_pois(source: &WorldSource, dir: &Path) -> Result<()> {
    let (mut markers, zones) = source
        .economy
        .as_ref()
        .map(|e| poi::from_economy(&e.read_all()))
        .unwrap_or_default();
    let water = water_markers(source);
    if water.iter().any(|m| m.kind == poi::Kind::Water) {
        // The terrain lists every well; the economy only the ones that spawn loot.
        markers.retain(|m| m.kind != poi::Kind::Water);
    }
    markers.extend(water);
    let pois = poi::Pois {
        places: source.places.clone(),
        markers,
        zones,
    };
    log::info!(
        "{}: {} places, {} markers, {} zones",
        source.name,
        pois.places.len(),
        pois.markers.len(),
        pois.zones.len()
    );
    std::fs::write(dir.join("pois.json"), serde_json::to_vec(&pois)?)?;
    Ok(())
}

fn water_markers(source: &WorldSource) -> Vec<poi::Marker> {
    let Some(terrain) = &source.terrain else {
        return Vec::new();
    };
    let data = match terrain.read() {
        Ok(data) => data,
        Err(e) => {
            log::warn!("{}: reading the world file: {e:#}", source.name);
            return Vec::new();
        }
    };
    let Some(objects) = wrp::objects(&data, |m| water::fresh_water(m).is_some()) else {
        log::warn!("{}: couldn't read the world file's objects", source.name);
        return Vec::new();
    };
    drop(data);
    let wells = water::well_classes(&source.scripts);
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
    markers
}

fn build_layer(
    source: &TileSource,
    layout: &TileLayout,
    map_dir: &Path,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<LayerMeta> {
    // Empty tiles are stored tiny, so take the size from the largest one.
    let sample = source
        .tiles
        .values()
        .max_by_key(|&&i| source.pbo.entries[i].size)
        .context("no tiles")?;
    let src_px = paa::decode(&source.pbo.read(&source.pbo.entries[*sample])?)?.width();
    let overlap_px = (layout.overlap_frac * f64::from(src_px)).round() as u32;
    let tile_px = src_px - 2 * overlap_px;
    let max_level = source.grid.next_power_of_two().trailing_zeros();
    let meta = LayerMeta {
        id: SATELLITE.into(),
        name: "Satellite".into(),
        tile_px,
        grid: source.grid,
        max_level,
        ext: "jpg".into(),
        tile_m: layout.step_m,
        origin: [layout.left, layout.top],
    };

    let final_dir = map_dir.join(SATELLITE);
    let work_dir = map_dir.join(format!("{SATELLITE}.importing"));
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir)?;
    }
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
    let results = Mutex::new(HashMap::new());
    let threads = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    std::thread::scope(|scope| -> Result<()> {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                scope.spawn(|| -> Result<()> {
                    while let Some(&(x, y)) = tasks.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let image = builder.build(split, x, y, None)?;
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

    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&work_dir, &final_dir)?;
    Ok(meta)
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
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    if path.extension().is_some_and(|e| e == "jpg") {
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 88).encode_image(&rgb)?;
    } else if opaque {
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
        rgb.write_with_encoder(image::codecs::png::PngEncoder::new(file))?;
    } else {
        image.write_with_encoder(image::codecs::png::PngEncoder::new(file))?;
    }
    Ok(())
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
    let layer = LayerMeta {
        id: "picture".into(),
        name: "Picture".into(),
        tile_px: TILE_PX,
        grid,
        max_level,
        ext: "png".into(),
        tile_m: world_size / f64::from(grid),
        origin: [0.0, world_size],
    };
    let dir = maps::maps_dir().join(id);
    let layer_dir = dir.join(&layer.id);
    if layer_dir.exists() {
        std::fs::remove_dir_all(&layer_dir)?;
    }
    let mut current = image;
    for level in (0..=max_level).rev() {
        let level_dir = layer_dir.join(level.to_string());
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
    let mut meta = maps::load(&dir)
        .map(|p| p.meta)
        .unwrap_or_else(|_| MapMeta {
            id: id.into(),
            name: name.into(),
            world_size,
            format: 0,
            source: String::new(),
            mod_id: None,
            pois_source: String::new(),
            layers: Vec::new(),
        });
    meta.name = name.into();
    meta.world_size = world_size;
    meta.layers.retain(|l| l.id != layer.id);
    meta.layers.push(layer);
    maps::save_meta(&dir, &meta)?;
    Ok(MapPack { meta, dir })
}
