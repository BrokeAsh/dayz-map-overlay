//! Finds every importable terrain in the game folder and the Steam Workshop folders.
//!
//! A terrain is put together from several archives, often in the same mod:
//! - a world PBO with the `.wrp` file and `config.bin` (`CfgWorlds`: id, terrain file, place names),
//! - a data PBO with the satellite tiles and their materials,
//! - a Central Economy PBO with loot and event positions.
//!
//! The world id is the `CfgWorlds` class name, which is also what the game reports when it joins
//! a server (`mpmissions\__cur_mp.<world>`). Only archive headers are read while scanning, plus
//! the configs of world archives, so a full scan of hundreds of mods takes about a second.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::pbo::{Pbo, stamp};
use super::poi::Place;
use super::{poi, rap, wrp};
use crate::maps;

/// Bumped when the importer's output changes, so older imports are rebuilt automatically.
pub const IMPORT_VERSION: u32 = 3;
/// Bumped when only the points of interest change; those are rebuilt without the tiles.
pub const POI_VERSION: u32 = 3;

const ECONOMY_FILES: [&str; 4] = [
    "mapgrouppos.xml",
    "mapgroupproto.xml",
    "cfgeventspawns.xml",
    "cfgeffectarea.json",
];

#[derive(Debug, Clone)]
pub struct TileSource {
    pub pbo: Arc<Pbo>,
    /// Entry index for each `(x, y)` tile; `y = 0` is the north edge.
    pub tiles: HashMap<(u32, u32), usize>,
    pub grid: u32,
}

/// Central Economy files in one archive: lower-case file name -> entry index.
#[derive(Debug, Clone)]
pub struct EconomySource {
    pub pbo: Arc<Pbo>,
    pub files: HashMap<String, usize>,
}

impl EconomySource {
    /// Every file that reads. A corrupt one is skipped, but a failing read (a file another
    /// program has locked, say) fails the whole thing, so the caller keeps what it had.
    pub fn read_all(&self) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let mut files = HashMap::new();
        for (name, &i) in &self.files {
            match self.pbo.read(&self.pbo.entries[i]) {
                Ok(data) => {
                    files.insert(name.clone(), data);
                }
                Err(e) if super::pbo::is_io(&e) => return Err(e),
                Err(e) => log::warn!("skipped {name}: {e:#}"),
            }
        }
        Ok(files)
    }
}

/// The world's `.wrp` file.
#[derive(Debug, Clone)]
pub struct TerrainFile {
    pub pbo: Arc<Pbo>,
    pub entry: usize,
}

impl TerrainFile {
    pub fn read(&self) -> anyhow::Result<Vec<u8>> {
        self.pbo.read(&self.pbo.entries[self.entry])
    }
}

#[derive(Debug, Clone)]
pub struct WorldSource {
    /// Lower-case `CfgWorlds` class name, such as `chernarusplus` or `deerisle`.
    pub id: String,
    pub name: String,
    /// Workshop item id (or `@folder` for a local mod); `None` for the base game.
    pub mod_id: Option<String>,
    pub mod_name: Option<String>,
    /// From the `.wrp` header; `None` when the world file can't be read (encrypted `.ebo`).
    pub world_size: Option<f64>,
    pub places: Vec<Place>,
    pub satellite: TileSource,
    pub economy: Option<EconomySource>,
    /// `None` when the world file can't be read (encrypted `.ebo`).
    pub terrain: Option<TerrainFile>,
    /// Archives with scripts that can define drinkable classes: the game's and the map mod's.
    pub scripts: Vec<Arc<Pbo>>,
}

impl WorldSource {
    /// Changes whenever the source files change (a mod update) or the importer does.
    pub fn fingerprint(&self) -> String {
        let mut parts = vec![
            IMPORT_VERSION.to_string(),
            self.satellite.pbo.path.display().to_string(),
        ];
        parts.push(stamp(&self.satellite.pbo.path));
        if let Some(economy) = &self.economy {
            parts.push(stamp(&economy.pbo.path));
        }
        parts.join("|")
    }

    /// Changes when the files the points of interest come from change (including the scripts
    /// that say which buildings are wells).
    pub fn pois_fingerprint(&self) -> String {
        let mut parts = vec![POI_VERSION.to_string()];
        for pbo in self
            .economy
            .iter()
            .map(|e| &e.pbo)
            .chain(self.terrain.iter().map(|t| &t.pbo))
            .chain(&self.scripts)
        {
            parts.push(pbo.path.display().to_string());
            parts.push(stamp(&pbo.path));
        }
        parts.join("|")
    }

    /// Whether an installed pack was built from these files as they are now. (A mod update can
    /// also resize the terrain without touching its tiles.)
    pub fn is_current(&self, pack: &maps::MapPack) -> bool {
        pack.meta.source == self.fingerprint()
            && self
                .world_size
                .is_none_or(|size| (size.round() - pack.meta.world_size).abs() < 1.0)
    }

    /// Whether any of its archives changed since the scan, so their entry offsets are stale.
    pub fn changed_since_scan(&self) -> bool {
        std::iter::once(&self.satellite.pbo)
            .chain(self.economy.iter().map(|e| &e.pbo))
            .chain(self.terrain.iter().map(|t| &t.pbo))
            .chain(&self.scripts)
            .any(|pbo| pbo.changed())
    }

    pub fn source_label(&self) -> String {
        match (&self.mod_name, &self.mod_id) {
            (Some(name), _) => name.clone(),
            (None, Some(id)) => format!("mod {id}"),
            (None, None) => "DayZ".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub worlds: Vec<WorldSource>,
    /// Every mod (Workshop item id or `@folder`) the scan looked at.
    pub mods: std::collections::HashSet<String>,
}

impl Catalog {
    /// All candidate sources for a world (several mods can ship the same terrain).
    pub fn candidates<'a>(&'a self, id: &str) -> impl Iterator<Item = &'a WorldSource> + use<'a> {
        let id = id.to_lowercase();
        self.worlds.iter().filter(move |w| w.id == id)
    }

    /// The source to use for a world: one from a mod the server loads, then the base game, then
    /// the most complete.
    pub fn best(&self, id: &str, server_mods: &[String]) -> Option<WorldSource> {
        let mut best = self
            .candidates(id)
            .max_by_key(|w| {
                let on_server = w.mod_id.as_ref().is_some_and(|m| server_mods.contains(m));
                (on_server, w.mod_id.is_none(), w.satellite.tiles.len())
            })?
            .clone();
        // A retexture mod may ship only tiles; borrow the rest from another copy of the map.
        for other in self.candidates(id) {
            if best.world_size.is_none() {
                best.world_size = other.world_size;
            }
            if best.places.is_empty() {
                best.places = other.places.clone();
            }
            if best.economy.is_none() {
                best.economy = other.economy.clone();
            }
            if best.terrain.is_none() {
                best.terrain = other.terrain.clone();
                best.scripts = other.scripts.clone();
            }
        }
        Some(best)
    }

    /// One entry per world id, for listing.
    pub fn unique(&self) -> Vec<&WorldSource> {
        let mut seen = HashMap::new();
        for w in &self.worlds {
            seen.entry(w.id.clone()).or_insert(w);
        }
        let mut list: Vec<_> = seen.into_values().collect();
        list.sort_by_key(|a| a.name.to_lowercase());
        list
    }
}

/// Where to look: the game folder and each Workshop content folder.
pub fn roots(paths: &crate::paths::Paths) -> Vec<(PathBuf, bool)> {
    paths
        .game
        .iter()
        .map(|g| (g.path.clone(), false))
        .chain(paths.workshop.iter().map(|w| (w.path.clone(), true)))
        .collect()
}

struct Archive {
    pbo: Arc<Pbo>,
    mod_id: Option<String>,
    mod_dir: Option<PathBuf>,
}

struct WorldDef {
    class: String,
    terrain: TerrainFile,
    description: Option<String>,
    size: Option<f64>,
    /// Directory of the `.wrp`, lower-case, `\`-separated.
    anchor: String,
    places: Vec<Place>,
    mod_id: Option<String>,
    mod_dir: Option<PathBuf>,
}

pub fn scan(roots: &[(PathBuf, bool)]) -> Catalog {
    let mut archives = Vec::new();
    for (root, workshop) in roots {
        collect(root, *workshop, &mut archives);
    }
    log::debug!("scanning {} archives", archives.len());

    let mut tiles = Vec::new();
    let mut economies = Vec::new();
    let mut defs = Vec::new();
    let mut scripts: HashMap<Option<String>, Vec<Arc<Pbo>>> = HashMap::new();
    for archive in &archives {
        let has_scripts = archive
            .pbo
            .entries
            .iter()
            .any(|e| e.name.to_ascii_lowercase().ends_with(".c"));
        if has_scripts {
            scripts
                .entry(archive.mod_id.clone())
                .or_default()
                .push(archive.pbo.clone());
        }
        if let Some(source) = tile_source(&archive.pbo) {
            tiles.push((archive, source));
        }
        if let Some(economy) = economy_source(&archive.pbo) {
            economies.push((archive, economy));
        }
        defs.extend(world_defs(archive));
    }

    let mut worlds = Vec::new();
    let mut used_tiles = vec![false; tiles.len()];
    for def in &defs {
        let best = tiles
            .iter()
            .enumerate()
            .filter_map(|(i, (archive, source))| {
                let score = link_score(&def.mod_id, &def.anchor, archive, &source.pbo.prefix)?;
                Some((score, i))
            })
            .max();
        let Some((_, i)) = best else {
            log::debug!("{}: no satellite tiles found", def.class);
            continue;
        };
        used_tiles[i] = true;
        let economy = best_economy(&economies, &def.mod_id, &def.anchor);
        let id = def.class.to_lowercase();
        if !maps::valid_id(&id) {
            log::warn!("skipping a world with an unusable name: {:?}", def.class);
            continue;
        }
        let mut world_scripts = scripts.get(&None).cloned().unwrap_or_default();
        if def.mod_id.is_some() {
            world_scripts.extend(scripts.get(&def.mod_id).into_iter().flatten().cloned());
        }
        worlds.push(WorldSource {
            name: display_name(&id, def.description.as_deref()),
            id,
            mod_name: def.mod_dir.as_deref().and_then(mod_name),
            mod_id: def.mod_id.clone(),
            world_size: def.size,
            places: def.places.clone(),
            satellite: tiles[i].1.clone(),
            economy,
            terrain: Some(def.terrain.clone()),
            scripts: world_scripts,
        });
    }
    // Tiles with no readable world config (Sakhal's world file is encrypted): name the world
    // after the archive prefix instead.
    for (i, (archive, source)) in tiles.iter().enumerate() {
        if used_tiles[i] {
            continue;
        }
        let id = world_name(&source.pbo.prefix);
        if !maps::valid_id(&id)
            || worlds
                .iter()
                .any(|w| w.id == id && w.mod_id == archive.mod_id)
        {
            continue;
        }
        let anchor = source.pbo.prefix.to_lowercase();
        worlds.push(WorldSource {
            name: maps::display_name(&id),
            id,
            mod_name: archive.mod_dir.as_deref().and_then(mod_name),
            mod_id: archive.mod_id.clone(),
            world_size: None,
            places: Vec::new(),
            satellite: source.clone(),
            economy: best_economy(&economies, &archive.mod_id, &anchor),
            terrain: None,
            scripts: Vec::new(),
        });
    }
    worlds.sort_by_key(|a| a.name.to_lowercase());
    let mods = archives.iter().filter_map(|a| a.mod_id.clone()).collect();
    Catalog { worlds, mods }
}

/// How well an archive belongs to a world anchored at `anchor`; `None` if it doesn't.
fn link_score(
    mod_id: &Option<String>,
    anchor: &str,
    archive: &Archive,
    prefix: &str,
) -> Option<usize> {
    let shared = shared_components(anchor, &prefix.to_lowercase());
    let same_origin = *mod_id == archive.mod_id;
    let same_mod = same_origin && mod_id.is_some();
    (same_mod || shared >= 3)
        .then_some(usize::from(same_mod) * 100 + usize::from(same_origin) * 50 + shared)
}

fn best_economy(
    economies: &[(&Archive, EconomySource)],
    mod_id: &Option<String>,
    anchor: &str,
) -> Option<EconomySource> {
    economies
        .iter()
        .filter_map(|(archive, economy)| {
            let score = link_score(mod_id, anchor, archive, &economy.pbo.prefix)?;
            // Prefer the archive with the most of the expected files.
            Some((score * 10 + economy.files.len(), economy))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, e)| e.clone())
}

fn shared_components(a: &str, b: &str) -> usize {
    a.split('\\')
        .filter(|s| !s.is_empty())
        .zip(b.split('\\').filter(|s| !s.is_empty()))
        .take_while(|(x, y)| x == y)
        .count()
}

fn collect(root: &Path, workshop: bool, out: &mut Vec<Archive>) {
    if workshop {
        // `<workshop>/<item id>/addons/*.pbo`
        let Ok(items) = std::fs::read_dir(root) else {
            return;
        };
        for item in items.flatten() {
            let dir = item.path();
            let id = item.file_name().to_string_lossy().into_owned();
            let mut pbos = Vec::new();
            find_pbos(&dir, 0, 3, &mut pbos);
            out.extend(open_all(pbos, Some(id), Some(dir)));
        }
    } else {
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name.starts_with('!') {
                // `!Workshop` links to the Workshop folder, which is scanned on its own.
                continue;
            }
            let mut pbos = Vec::new();
            if path.is_dir() {
                find_pbos(&path, 0, 3, &mut pbos);
            } else if is_pbo(&path) {
                pbos.push(path.clone());
            }
            // Locally installed mods live in `@Name` folders.
            let local_mod = name.starts_with('@').then_some(name);
            let mod_dir = local_mod.as_ref().map(|_| path.clone());
            out.extend(open_all(pbos, local_mod, mod_dir));
        }
    }
}

fn open_all(paths: Vec<PathBuf>, mod_id: Option<String>, mod_dir: Option<PathBuf>) -> Vec<Archive> {
    paths
        .into_iter()
        .filter_map(|path| match Pbo::open(&path) {
            Ok(pbo) => Some(Archive {
                pbo: Arc::new(pbo),
                mod_id: mod_id.clone(),
                mod_dir: mod_dir.clone(),
            }),
            Err(e) => {
                log::debug!("skipping {}: {e:#}", path.display());
                None
            }
        })
        .collect()
}

fn find_pbos(dir: &Path, depth: usize, max_depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() && depth < max_depth {
            find_pbos(&path, depth + 1, max_depth, out);
        } else if is_pbo(&path) {
            out.push(path);
        }
    }
}

fn is_pbo(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pbo"))
}

fn tile_source(pbo: &Arc<Pbo>) -> Option<TileSource> {
    let mut tiles = HashMap::new();
    for (index, entry) in pbo.entries.iter().enumerate() {
        let lower = entry.name.to_ascii_lowercase();
        let Some((dir, file)) = lower.rsplit_once('\\') else {
            continue;
        };
        if !(dir == "layers" || dir.ends_with("\\layers")) {
            continue;
        }
        let Some(coords) = file
            .strip_prefix("s_")
            .and_then(|f| f.strip_suffix("_lco.paa"))
        else {
            continue;
        };
        let Some((x, y)) = coords.split_once('_') else {
            continue;
        };
        // Real terrains have at most a few dozen tiles per side; a stray name like
        // `s_99999_000` mustn't make a grid of billions.
        if let (Ok(x @ 0..1024), Ok(y @ 0..1024)) = (x.parse::<u32>(), y.parse::<u32>()) {
            tiles.insert((x, y), index);
        }
    }
    let grid = tiles
        .keys()
        .map(|&(x, y): &(u32, u32)| x.max(y) + 1)
        .max()?;
    // A few stray tiles aren't a terrain.
    (tiles.len() >= 16).then(|| TileSource {
        pbo: pbo.clone(),
        tiles,
        grid,
    })
}

fn economy_source(pbo: &Arc<Pbo>) -> Option<EconomySource> {
    let mut files = HashMap::new();
    for (index, entry) in pbo.entries.iter().enumerate() {
        let lower = entry.name.to_ascii_lowercase();
        let file = lower.rsplit('\\').next().unwrap_or(&lower);
        if ECONOMY_FILES.contains(&file) {
            // The shallowest copy wins (some mods keep variants in subfolders).
            let depth = lower.matches('\\').count();
            let keep = files.get(file).is_none_or(|&(_, d)| depth < d);
            if keep {
                files.insert(file.to_string(), (index, depth));
            }
        }
    }
    files
        .contains_key("mapgrouppos.xml")
        .then(|| EconomySource {
            pbo: pbo.clone(),
            files: files.into_iter().map(|(k, (i, _))| (k, i)).collect(),
        })
}

/// Worlds defined by an archive that also contains their `.wrp`.
fn world_defs(archive: &Archive) -> Vec<WorldDef> {
    let pbo = &archive.pbo;
    let wrps: HashMap<String, usize> = pbo
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.name.to_ascii_lowercase().ends_with(".wrp"))
        .map(|(i, e)| (pbo.full_path(e), i))
        .collect();
    if wrps.is_empty() {
        return Vec::new();
    }
    // The world config is usually `config.bin` at the root, but can sit next to the `.wrp`.
    let configs: Vec<rap::Class> = pbo
        .entries
        .iter()
        .filter(|e| e.name.to_ascii_lowercase().rsplit('\\').next() == Some("config.bin"))
        .filter_map(|e| match pbo.read(e).map(|d| rap::parse(&d)) {
            Ok(Ok(config)) => Some(config),
            Ok(Err(err)) | Err(err) => {
                log::debug!("{} {}: {err:#}", pbo.path.display(), e.name);
                None
            }
        })
        .collect();
    configs
        .iter()
        .filter_map(|config| config.class("CfgWorlds"))
        .flat_map(|worlds| worlds.classes())
        .filter_map(|(class, world)| {
            let wrp_path = world
                .value("worldName")?
                .as_str()?
                .trim_start_matches('\\')
                .to_lowercase();
            let index = *wrps.get(&wrp_path)?;
            let size = pbo
                .read_prefix(&pbo.entries[index], wrp::HEADER_LEN)
                .ok()
                .and_then(|h| wrp::terrain_size(&h));
            let anchor = wrp_path
                .rsplit_once('\\')
                .map_or(String::new(), |(dir, _)| dir.to_string());
            let description = world
                .value("description")
                .and_then(|d| d.as_str())
                .map(str::to_string);
            Some(WorldDef {
                class: class.to_string(),
                terrain: TerrainFile {
                    pbo: pbo.clone(),
                    entry: index,
                },
                description,
                size,
                anchor,
                places: poi::places(world),
                mod_id: archive.mod_id.clone(),
                mod_dir: archive.mod_dir.clone(),
            })
        })
        .collect()
}

fn display_name(id: &str, description: Option<&str>) -> String {
    match description {
        Some(d)
            if !d.is_empty()
                && !d.starts_with('$')
                && !d.starts_with('#')
                && !d.eq_ignore_ascii_case(id)
                && !matches!(id, "chernarusplus" | "enoch" | "sakhal") =>
        {
            d.to_string()
        }
        _ => maps::display_name(id),
    }
}

/// `name = "DeerIsle";` from the mod's `meta.cpp`.
fn mod_name(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("meta.cpp")).ok()?;
    let line = text
        .lines()
        .find(|l| l.trim_start().to_lowercase().starts_with("name"))?;
    let value = line
        .split_once('=')?
        .1
        .trim()
        .trim_end_matches(';')
        .trim()
        .trim_matches('"');
    (!value.is_empty()).then(|| value.to_string())
}

/// `DZ\worlds\enoch\data` -> `enoch`.
fn world_name(prefix: &str) -> String {
    let parts: Vec<String> = prefix.split('\\').map(|p| p.to_ascii_lowercase()).collect();
    if let Some(i) = parts.iter().position(|p| p == "worlds")
        && let Some(name) = parts.get(i + 1)
    {
        return name.clone();
    }
    parts
        .into_iter()
        .rev()
        .map(|p| p.trim_end_matches("_data").to_string())
        .find(|p| !p.is_empty() && p != "data")
        .unwrap_or_default()
}
