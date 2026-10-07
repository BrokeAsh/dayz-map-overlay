//! Drinking water: wells and pumps, plus fresh-water ponds, lakes, rivers and springs.
//!
//! The game decides what you can drink from in script: buildings whose class extends `Well`
//! (the blue and yellow pumps, and mod additions like Namalsk's lab sinks). Those classes are
//! read from the scripts in the game and the map's mod, and matched against the terrain's
//! classed objects. Natural fresh water is the terrain's water models (the sea is not an object).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::pbo::Pbo;
use super::poi::{Kind, Marker};
use super::wrp;

/// The base game's pumps, in case its scripts can't be read.
const BASE_WELLS: [&str; 2] = ["land_misc_well_pump_blue", "land_misc_well_pump_yellow"];

/// Fresh-water objects are merged into one marker per this many metres.
const CLUSTER_M: f32 = 400.0;

/// x, z, and what kind of water.
type WaterPoint<'a> = (f32, f32, &'a str);

/// Lower-case class names that extend `Well` in any of these archives' scripts.
pub fn well_classes(scripts: &[Arc<Pbo>]) -> HashSet<String> {
    let mut parents: HashMap<String, String> = HashMap::new();
    for pbo in scripts {
        for entry in &pbo.entries {
            if !entry.name.to_ascii_lowercase().ends_with(".c") {
                continue;
            }
            let Ok(data) = pbo.read(entry) else {
                continue;
            };
            // Every file: a class can extend `Well` through others declared elsewhere
            // (`LabTap extends Sink`, with `Sink extends Well` in another file).
            let text = String::from_utf8_lossy(&data);
            for (class, parent) in class_declarations(&text) {
                parents.insert(class, parent);
            }
        }
    }
    let mut wells: HashSet<String> = BASE_WELLS.iter().map(|s| s.to_string()).collect();
    wells.insert("well".into());
    // Follow `extends` chains until nothing new joins.
    loop {
        let before = wells.len();
        for (class, parent) in &parents {
            if wells.contains(parent) {
                wells.insert(class.clone());
            }
        }
        if wells.len() == before {
            break;
        }
    }
    wells.remove("well");
    wells
}

/// `class A extends B` and `class A : B`, lower-cased.
fn class_declarations(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let tokens: Vec<&str> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .filter(|t| !t.is_empty())
        .collect();
    for i in 0..tokens.len().saturating_sub(2) {
        if tokens[i] != "class" {
            continue;
        }
        let class = tokens[i + 1].trim_end_matches(':');
        let parent = match (tokens[i + 1].ends_with(':'), tokens[i + 2]) {
            (true, parent) => Some(parent),
            (false, "extends" | ":") => tokens.get(i + 3).copied(),
            _ => None,
        };
        if let Some(parent) = parent {
            found.push((class.to_ascii_lowercase(), parent.to_ascii_lowercase()));
        }
    }
    found
}

/// What a water model is, or `None` if it isn't fresh water (streambeds are dry, ice is
/// frozen, and the sea isn't an object).
pub fn fresh_water(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    let watery = m.contains("water") || m.contains("\\ponds\\") || m.contains("river");
    if !watery || m.contains("streambed") || m.contains("\\ice_") || m.contains("volcanic") {
        return None;
    }
    if m.contains("spring") || m.contains("\\fresh\\") {
        Some("Spring")
    } else if m.contains("river") {
        Some("River")
    } else if m.contains("\\streams\\") {
        Some("Stream")
    } else if m.contains("lake") || m.contains("waterclear") {
        Some("Lake")
    } else if m.contains("pond") {
        Some("Pond")
    } else {
        None
    }
}

/// Markers for every well (`Kind::Water`) and fresh-water area (`Kind::FreshWater`).
pub fn markers(objects: &wrp::Objects, wells: &HashSet<String>) -> Vec<Marker> {
    let is_well = |class: &str| {
        wells.contains(class)
            // Config variants of a scripted class, like `..._pump_yellow_metro`.
            || wells.iter().any(|w| class.strip_prefix(w.as_str()).is_some_and(|r| r.starts_with('_')))
    };
    let mut markers: Vec<Marker> = objects
        .classed
        .iter()
        .filter(|o| is_well(&o.class))
        .map(|o| Marker {
            kind: Kind::Water,
            x: o.x,
            z: o.z,
            label: if o.class.contains("pump") {
                "Water pump".into()
            } else {
                "Drinking water".into()
            },
        })
        .collect();

    let fresh: Vec<WaterPoint> = without_lone_tiles(objects)
        .into_iter()
        .filter_map(|(model, x, z)| Some((x, z, fresh_water(&objects.models[model as usize])?)))
        .collect();
    markers.extend(cluster(&fresh));
    markers
}

/// Lakes are laid out from square water tiles (`pond_50x50`, `lake_50x50`). A tile on its own
/// isn't visible water (Chernarus has dozens under airfields and forest), so keep tiles only
/// next to another tile. Shaped ponds and rivers are kept as they are.
fn without_lone_tiles(objects: &wrp::Objects) -> Vec<(u32, f32, f32)> {
    const NEIGHBOUR_M: f32 = 75.0;
    let is_tile = |model: u32| objects.models[model as usize].contains("50x50");
    let cell = |x: f32, z: f32| {
        (
            (x / NEIGHBOUR_M).floor() as i32,
            (z / NEIGHBOUR_M).floor() as i32,
        )
    };
    let mut tiles: HashMap<(i32, i32), Vec<(f32, f32)>> = HashMap::new();
    for &(model, x, z) in &objects.placed {
        if is_tile(model) {
            tiles.entry(cell(x, z)).or_default().push((x, z));
        }
    }
    let has_neighbour = |x: f32, z: f32| {
        let (cx, cz) = cell(x, z);
        (cx - 1..=cx + 1).any(|i| {
            (cz - 1..=cz + 1).any(|j| {
                tiles.get(&(i, j)).is_some_and(|list| {
                    list.iter().any(|&(ox, oz)| {
                        let d = (ox - x).hypot(oz - z);
                        d > 1.0 && d <= NEIGHBOUR_M
                    })
                })
            })
        })
    };
    objects
        .placed
        .iter()
        .copied()
        .filter(|&(model, x, z)| !is_tile(model) || has_neighbour(x, z))
        .collect()
}

/// One marker per occupied cell, at the object nearest the cell's centre of mass, then drops
/// markers crowding a bigger neighbour.
fn cluster(points: &[WaterPoint]) -> Vec<Marker> {
    let mut cells: HashMap<(i32, i32), Vec<WaterPoint>> = HashMap::new();
    for &p in points {
        let key = (
            (p.0 / CLUSTER_M).floor() as i32,
            (p.1 / CLUSTER_M).floor() as i32,
        );
        cells.entry(key).or_default().push(p);
    }
    let mut groups: Vec<(usize, Marker)> = cells
        .into_values()
        .map(|members| {
            let n = members.len() as f32;
            let cx = members.iter().map(|p| p.0).sum::<f32>() / n;
            let cz = members.iter().map(|p| p.1).sum::<f32>() / n;
            let near = members
                .iter()
                .min_by(|a, b| {
                    let da = (a.0 - cx).powi(2) + (a.1 - cz).powi(2);
                    let db = (b.0 - cx).powi(2) + (b.1 - cz).powi(2);
                    da.total_cmp(&db)
                })
                .copied()
                .unwrap();
            let mut labels: HashMap<&str, usize> = HashMap::new();
            for p in &members {
                *labels.entry(p.2).or_default() += 1;
            }
            let label = labels.into_iter().max_by_key(|&(_, c)| c).unwrap().0;
            (
                members.len(),
                Marker {
                    kind: Kind::FreshWater,
                    x: near.0,
                    z: near.1,
                    label: label.into(),
                },
            )
        })
        .collect();
    groups.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.x.total_cmp(&b.1.x)));
    let mut kept: Vec<Marker> = Vec::new();
    let spacing = CLUSTER_M * 0.6;
    for (_, marker) in groups {
        let crowded = kept
            .iter()
            .any(|k| (k.x - marker.x).hypot(k.z - marker.z) < spacing);
        if !crowded {
            kept.push(marker);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_well_subclasses() {
        let text = "class Land_Misc_Well_Pump_Blue extends Well\n{\n};\nclass land_a3_lab_sink extends Well {}\nclass Sink2: land_a3_lab_sink {}\nclass Other extends House {}";
        let parents: HashMap<_, _> = class_declarations(text).into_iter().collect();
        assert_eq!(parents["land_a3_lab_sink"], "well");
        assert_eq!(parents["sink2"], "land_a3_lab_sink");
        assert_eq!(parents["other"], "house");
    }

    #[test]
    fn classifies_water_models() {
        assert_eq!(
            fresh_water(r"dz\water\ponds\pond_big_35_01.p3d"),
            Some("Pond")
        );
        assert_eq!(fresh_water(r"nst\ns\water\lakewater.p3d"), Some("Lake"));
        assert_eq!(
            fresh_water(r"dz\water_bliss\river\enoch_river_1.p3d"),
            Some("River")
        );
        assert_eq!(
            fresh_water(r"dz\water\streambed\streambed_leaf_long_straight.p3d"),
            None
        );
        assert_eq!(
            fresh_water(r"dz\water_sakhal\ice_lake\sakhal_50x50_ice_lake_clear.p3d"),
            None
        );
        assert_eq!(
            fresh_water(r"dz\structures\industrial\houses\water_station.p3d"),
            None
        );
    }
}
