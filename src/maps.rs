//! Installed map packs.
//!
//! Each map lives in `<data dir>/maps/<id>/` with a `map.toml`, its points of interest, and one
//! tile pyramid per layer: `<layer folder>/<level>/<x>_<y>.<ext>`. Level `max_level` is full
//! resolution with `grid` tiles per side; each level below halves the resolution. Tile `y = 0`
//! is the north edge.
//!
//! Each import writes new folders and files beside the old ones (`satellite-<generation>`),
//! and saving `map.toml` switches to them in one step, so a failed or interrupted import
//! leaves the previous map whole. The old ones are deleted afterwards.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapMeta {
    pub id: String,
    pub name: String,
    /// Terrain size in metres; DayZ coordinates run from 0 to this on both axes.
    pub world_size: f64,
    /// Importer version that built this pack (0 for picture imports).
    #[serde(default)]
    pub format: u32,
    /// Fingerprint of the game files it was built from, to notice mod updates.
    #[serde(default)]
    pub source: String,
    /// Workshop item it came from, if any.
    #[serde(default)]
    pub mod_id: Option<String>,
    /// Fingerprint of what the points of interest were built from; they're rebuilt on their own
    /// when it changes, without redoing the tiles.
    #[serde(default)]
    pub pois_source: String,
    #[serde(default)]
    pub layers: Vec<LayerMeta>,
    /// The points-of-interest file (`pois.json` in maps from before 0.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pois: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerMeta {
    pub id: String,
    /// Its folder, if not named after `id` (maps from before 0.2 are).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    pub name: String,
    pub tile_px: u32,
    /// Tiles per side at full resolution.
    pub grid: u32,
    pub max_level: u32,
    pub ext: String,
    /// Metres covered by one full-resolution tile.
    pub tile_m: f64,
    /// World position (x, z) of the grid's north-west corner.
    pub origin: [f64; 2],
}

impl LayerMeta {
    /// Tiles per side at `level`.
    pub fn grid_at(&self, level: u32) -> u32 {
        self.grid.div_ceil(1 << (self.max_level - level))
    }

    /// Metres covered by one tile side at `level`.
    pub fn tile_metres(&self, level: u32) -> f64 {
        self.tile_m * f64::from(1u32 << (self.max_level - level))
    }
}

#[derive(Debug, Clone)]
pub struct MapPack {
    pub meta: MapMeta,
    pub dir: PathBuf,
}

impl MapPack {
    pub fn pois(&self) -> crate::import::poi::Pois {
        std::fs::read(
            self.dir
                .join(self.meta.pois.as_deref().unwrap_or("pois.json")),
        )
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
    }

    pub fn tile_path(&self, layer: &LayerMeta, level: u32, x: u32, y: u32) -> PathBuf {
        self.dir
            .join(layer.dir.as_deref().unwrap_or(&layer.id))
            .join(level.to_string())
            .join(format!("{x}_{y}.{}", layer.ext))
    }
}

pub fn maps_dir() -> PathBuf {
    crate::config::data_dir().join("maps")
}

/// Whether a world id is safe to use as a folder name. Ids come from mods' configs and archive
/// prefixes, so something like `../x` must not reach the file system.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Checks a terrain size (metres) before it's used for drawing.
pub fn check_world_size(size: f64) -> Result<()> {
    if !(100.0..=200_000.0).contains(&size) {
        anyhow::bail!("implausible world size {size} m (expected 100 to 200000)");
    }
    Ok(())
}

impl MapMeta {
    /// Rejects values that would break drawing (a damaged or hand-edited `map.toml`).
    fn check(&self) -> Result<()> {
        // Version 0.1 let pictures have any id (`Namalsk`); the folder is found by listing, so
        // only a name that could reach outside it is refused. New imports use `valid_id`.
        let id_ok = (1..=64).contains(&self.id.len())
            && !matches!(self.id.as_str(), "." | "..")
            && !self
                .id
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'));
        if !id_ok {
            anyhow::bail!("bad map id {:?}", self.id);
        }
        check_world_size(self.world_size)?;
        if let Some(layer) = self.layers.iter().find(|l| !l.is_valid()) {
            anyhow::bail!("bad layer {:?}", layer.id);
        }
        if let Some(pois) = &self.pois
            && !pois.strip_suffix(".json").is_some_and(valid_id)
        {
            anyhow::bail!("bad points-of-interest file {pois:?}");
        }
        Ok(())
    }
}

impl LayerMeta {
    fn is_valid(&self) -> bool {
        valid_id(&self.id)
            && self.dir.as_deref().is_none_or(valid_id)
            && (1..=8192).contains(&self.tile_px)
            && (1..=4096).contains(&self.grid)
            && self.max_level <= 16
            && self.grid <= 1 << self.max_level
            && self.tile_m.is_finite()
            && self.tile_m > 0.0
            && self.origin.iter().all(|c| c.is_finite())
            && self.ext.bytes().all(|b| b.is_ascii_alphanumeric())
    }
}

pub fn load_all() -> Vec<MapPack> {
    let mut packs: Vec<MapPack> = std::fs::read_dir(maps_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| match load(&entry.path()) {
            Ok(pack) if !pack.meta.layers.is_empty() => Some(pack),
            Ok(_) => None,
            Err(e) => {
                log::debug!("skipping {}: {e:#}", entry.path().display());
                None
            }
        })
        .collect();
    packs.sort_by(|a, b| a.meta.name.cmp(&b.meta.name));
    packs
}

pub fn load(dir: &Path) -> Result<MapPack> {
    let path = dir.join("map.toml");
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let meta: MapMeta =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    meta.check()
        .with_context(|| format!("checking {}", path.display()))?;
    Ok(MapPack {
        meta,
        dir: dir.to_owned(),
    })
}

/// The layers a map's metadata lists, including metadata set aside after a failed save, so a
/// picture the user imported isn't forgotten.
pub fn recorded_layers(dir: &Path) -> Vec<LayerMeta> {
    ["map.toml", "map.toml.stale"]
        .iter()
        .find_map(|name| {
            let text = std::fs::read_to_string(dir.join(name)).ok()?;
            toml::from_str::<MapMeta>(&text).ok()
        })
        .map(|meta| meta.layers)
        .unwrap_or_default()
        .into_iter()
        // A damaged one would make the new map.toml unsaveable too.
        .filter(LayerMeta::is_valid)
        .collect()
}

pub fn save_meta(dir: &Path, meta: &MapMeta) -> Result<()> {
    // Never save what `load` would refuse (it would be re-imported on every join).
    meta.check()?;
    std::fs::create_dir_all(dir)?;
    write_atomic(
        &dir.join("map.toml"),
        toml::to_string_pretty(meta)?.as_bytes(),
    )
}

/// A name suffix for one import's folders and files.
pub fn generation() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}", now.as_micros())
}

/// The folders and file a map's metadata points to.
fn used(meta: &MapMeta) -> impl Iterator<Item = &str> {
    meta.layers
        .iter()
        .map(|l| l.dir.as_deref().unwrap_or(&l.id))
        .chain([meta.pois.as_deref().unwrap_or("pois.json")])
}

/// Whether a name is one an import generates (`satellite-65d4add37c560`, `pois-….json`), or
/// a leftover of an older version's import.
fn generated(name: &str) -> bool {
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    name.strip_prefix("satellite-").is_some_and(hex)
        || name
            .strip_prefix("pois-")
            .and_then(|n| n.strip_suffix(".json"))
            .is_some_and(hex)
        || matches!(name, "satellite.importing" | "map.toml.stale")
        // `write_atomic`'s temporary file, left by a crash.
        || name
            .strip_prefix("map.toml.")
            .and_then(|n| n.strip_suffix(".tmp"))
            .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// After `new` was saved over `old`, deletes what `old` used and `new` doesn't, and generated
/// leftovers of failed imports (see `generated`). Pictures go only when a new picture replaced
/// them: one can't be rebuilt from the game's files. Anything else stays, including a picture
/// folder nothing lists any more (a crashed `import-image`), which the user can delete.
/// Best effort: a generated leftover that can't be deleted now goes after a later import.
pub fn remove_unused(dir: &Path, old: Option<&MapMeta>, new: &MapMeta) {
    let keep: Vec<&str> = used(new).collect();
    let new_picture = new.layers.iter().any(|l| l.id == "picture");
    let old_used: Vec<&str> = old.map(|old| used(old).collect()).unwrap_or_default();
    let old_pictures: Vec<&str> = old
        .map(|old| {
            old.layers
                .iter()
                .filter(|l| l.id == "picture")
                .map(|l| l.dir.as_deref().unwrap_or(&l.id))
                .collect()
        })
        .unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if keep.contains(&name.as_str()) {
            continue;
        }
        let remove = if old_pictures.contains(&name.as_str()) {
            new_picture
        } else {
            old_used.contains(&name.as_str()) || generated(&name)
        };
        if !remove {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Writes a file through a temporary one (named for this process, so two programs saving at
/// once don't share it), so readers see the old or the new file, never part of one. Synced
/// before the swap, so a crash can't leave the new name on an empty file.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let result = write_synced(Path::new(&tmp), data).and_then(|()| std::fs::rename(&tmp, path));
    // And the rename itself (on Unix, a folder can be synced; Windows commits it already).
    #[cfg(unix)]
    if result.is_ok()
        && let Some(dir) = path.parent()
    {
        let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    }
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

/// Writes a file and waits until it's on disk.
pub fn write_synced(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(data)?;
    file.sync_all()
}

/// Friendly names for the official maps; modded maps fall back to their world name.
pub fn display_name(world: &str) -> String {
    match world {
        "chernarusplus" => "Chernarus+".into(),
        "enoch" => "Livonia".into(),
        "sakhal" => "Sakhal".into(),
        "deerisle" => "Deer Isle".into(),
        "takistanplus" => "Takistan+".into(),
        other => {
            let mut chars = other.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}
