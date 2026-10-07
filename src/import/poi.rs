//! Points of interest from a terrain's own files.
//!
//! - Place names come from the world config (`CfgWorlds >> <world> >> Names`).
//! - Buildings come from the Central Economy files every map ships for its default mission:
//!   `mapgrouppos.xml` places each lootable building, and `mapgroupproto.xml` says what kind of
//!   loot it spawns (through `usage` or `tag` entries).
//! - Wells and fresh water come from the terrain itself (see `water`), since the economy files
//!   only list the pumps that also spawn loot.
//! - Event spawns (heli crashes, vehicles, ...) come from `cfgeventspawns.xml`, and static
//!   contaminated zones from `cfgeffectarea.json`.
//!
//! These are the map's defaults; a server can change its loot and events.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::rap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Water,
    FreshWater,
    Fuel,
    Military,
    Police,
    Medical,
    Firefighter,
    Hunting,
    Industrial,
    Civilian,
    HeliCrash,
    PoliceCar,
    Convoy,
    Train,
    Vehicle,
    Boat,
    ToxicZone,
}

impl Kind {
    pub const ALL: [Kind; 17] = [
        Kind::Water,
        Kind::FreshWater,
        Kind::Fuel,
        Kind::Military,
        Kind::Police,
        Kind::Medical,
        Kind::Firefighter,
        Kind::Hunting,
        Kind::Industrial,
        Kind::Civilian,
        Kind::HeliCrash,
        Kind::PoliceCar,
        Kind::Convoy,
        Kind::Train,
        Kind::Vehicle,
        Kind::Boat,
        Kind::ToxicZone,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Water => "Water pumps",
            Kind::FreshWater => "Fresh water",
            Kind::Fuel => "Fuel stations",
            Kind::Military => "Military loot",
            Kind::Police => "Police loot",
            Kind::Medical => "Medical loot",
            Kind::Firefighter => "Fire stations",
            Kind::Hunting => "Hunting loot",
            Kind::Industrial => "Industrial loot",
            Kind::Civilian => "Civilian loot",
            Kind::HeliCrash => "Heli crash sites",
            Kind::PoliceCar => "Police car wrecks",
            Kind::Convoy => "Military convoys",
            Kind::Train => "Train wrecks",
            Kind::Vehicle => "Vehicle spawns",
            Kind::Boat => "Boat spawns",
            Kind::ToxicZone => "Toxic zone spawns",
        }
    }

    pub fn id(self) -> String {
        format!("{self:?}").to_lowercase()
    }

    /// Shown until the user changes it. The dense categories start hidden.
    pub fn shown_by_default(self) -> bool {
        matches!(
            self,
            Kind::Water | Kind::FreshWater | Kind::Fuel | Kind::Military | Kind::HeliCrash
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Place {
    pub name: String,
    /// The config's type: Capital, City, Village, Local, Hill, Camp, ...
    pub kind: String,
    pub x: f32,
    pub z: f32,
}

impl Place {
    /// 0 for the biggest places; higher ranks only show when zoomed in.
    pub fn rank(&self) -> u8 {
        match self.kind.to_ascii_lowercase().as_str() {
            "capital" | "namecitycapital" => 0,
            "city" | "namecity" | "airport" => 1,
            "village" | "namevillage" | "strongpointarea" => 2,
            _ => 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub kind: Kind,
    pub x: f32,
    pub z: f32,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub x: f32,
    pub z: f32,
    pub radius: f32,
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pois {
    pub places: Vec<Place>,
    pub markers: Vec<Marker>,
    pub zones: Vec<Zone>,
}

/// Place names from a world's config class.
pub fn places(world: &rap::Class) -> Vec<Place> {
    let Some(names) = world.class("Names") else {
        return Vec::new();
    };
    names
        .classes()
        .filter_map(|(_, place)| {
            let name = transliterate(place.value("name")?.as_str()?.trim());
            let position = place.value("position")?.as_array()?;
            let (x, z) = (position.first()?.as_f32()?, position.get(1)?.as_f32()?);
            let kind = place
                .value("type")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            // Skip unnamed or localisation-only entries, and decorative types.
            let skip = name.is_empty() || name.starts_with('$') || name.starts_with('#');
            let decorative = matches!(
                kind.to_ascii_lowercase().as_str(),
                "viewpoint"
                    | "rockarea"
                    | "flatarea"
                    | "flatareacity"
                    | "flatareacitysmall"
                    | "bordercrossing"
                    | "vegetationbroadleaf"
                    | "vegetationfir"
                    | "vegetationpalm"
                    | "vegetationvineyard"
            );
            (!skip && !decorative).then_some(Place { name, kind, x, z })
        })
        .collect()
}

/// Markers and zones from the Central Economy files (keyed by lower-case file name).
pub fn from_economy(files: &HashMap<String, Vec<u8>>) -> (Vec<Marker>, Vec<Zone>) {
    let text = |name: &str| {
        files
            .get(name)
            .map(|d| String::from_utf8_lossy(d).into_owned())
    };
    let mut markers = Vec::new();
    if let Some(positions) = text("mapgrouppos.xml") {
        let loot = text("mapgroupproto.xml")
            .map(|p| loot_kinds(&p))
            .unwrap_or_default();
        markers.extend(buildings(&positions, &loot));
    }
    if let Some(events) = text("cfgeventspawns.xml") {
        markers.extend(events_markers(&events));
    }
    let zones = text("cfgeffectarea.json")
        .map(|j| effect_zones(&j))
        .unwrap_or_default();
    (markers, zones)
}

/// Each building prototype's loot kinds (`usage` names and container `tag` names), lower-case.
fn loot_kinds(proto: &str) -> HashMap<String, HashSet<String>> {
    let mut kinds: HashMap<String, HashSet<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for tag in Tags::new(proto) {
        match tag {
            Tag::Open(name, attrs) if name.eq_ignore_ascii_case("group") => {
                current = attr(&attrs, "name").map(str::to_lowercase);
            }
            Tag::Open(name, attrs)
                if name.eq_ignore_ascii_case("usage") || name.eq_ignore_ascii_case("tag") =>
            {
                if let (Some(group), Some(value)) = (&current, attr(&attrs, "name")) {
                    kinds
                        .entry(group.clone())
                        .or_default()
                        .insert(value.to_lowercase());
                }
            }
            Tag::Close(name) if name.eq_ignore_ascii_case("group") => current = None,
            _ => {}
        }
    }
    kinds
}

fn building_kind(name: &str, loot: Option<&HashSet<String>>) -> Option<Kind> {
    let lower = name.to_lowercase();
    if lower.contains("well") {
        return Some(Kind::Water);
    }
    if lower.contains("fuelstation")
        || lower.contains("fuel_station")
        || lower.contains("gasstation")
    {
        return Some(Kind::Fuel);
    }
    let has = |k: &str| loot.is_some_and(|l| l.contains(k));
    let by_loot = [
        ("military", Kind::Military),
        ("police", Kind::Police),
        ("prison", Kind::Police),
        ("medic", Kind::Medical),
        ("firefighter", Kind::Firefighter),
        ("hunting", Kind::Hunting),
        ("industrial", Kind::Industrial),
    ];
    if let Some((_, kind)) = by_loot.iter().find(|(k, _)| has(k)) {
        return Some(*kind);
    }
    // Buildings without a prototype: guess from the model name.
    if lower.contains("mil_") || lower.contains("military") || lower.contains("barracks") {
        return Some(Kind::Military);
    }
    if lower.contains("hospital") || lower.contains("clinic") || lower.contains("medical") {
        return Some(Kind::Medical);
    }
    if lower.contains("police") {
        return Some(Kind::Police);
    }
    if lower.contains("firestation") || lower.contains("fire_station") {
        return Some(Kind::Firefighter);
    }
    loot.filter(|l| !l.is_empty()).map(|_| Kind::Civilian)
}

fn buildings(positions: &str, loot: &HashMap<String, HashSet<String>>) -> Vec<Marker> {
    Tags::new(positions)
        .filter_map(|tag| {
            let Tag::Open(name, attrs) = tag else {
                return None;
            };
            if !name.eq_ignore_ascii_case("group") {
                return None;
            }
            let model = attr(&attrs, "name")?;
            let mut pos = attr(&attrs, "pos")?
                .split_whitespace()
                .map(|n| n.parse::<f32>());
            let (x, _height, z) = (pos.next()?.ok()?, pos.next()?.ok()?, pos.next()?.ok()?);
            let kind = building_kind(model, loot.get(&model.to_lowercase()))?;
            Some(Marker {
                kind,
                x,
                z,
                label: pretty(model),
            })
        })
        .collect()
}

fn event_kind(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    Some(
        if lower.contains("helicrash") || lower.starts_with("staticheli") {
            Kind::HeliCrash
        } else if lower.starts_with("staticpolice") {
            Kind::PoliceCar
        } else if lower.contains("convoy") {
            Kind::Convoy
        } else if lower.contains("train") && lower.starts_with("static") {
            Kind::Train
        } else if lower.contains("contaminated") {
            Kind::ToxicZone
        } else if lower.starts_with("vehicle") && lower.contains("boat") {
            Kind::Boat
        } else if lower.starts_with("vehicle") {
            Kind::Vehicle
        } else {
            return None;
        },
    )
}

fn events_markers(events: &str) -> Vec<Marker> {
    let mut markers = Vec::new();
    let mut current: Option<(Kind, String)> = None;
    for tag in Tags::new(events) {
        match tag {
            Tag::Open(name, attrs) if name.eq_ignore_ascii_case("event") => {
                current = attr(&attrs, "name").and_then(|n| Some((event_kind(n)?, pretty(n))));
            }
            Tag::Open(name, attrs) if name.eq_ignore_ascii_case("pos") => {
                let Some((kind, label)) = &current else {
                    continue;
                };
                let coord = |k: &str| attr(&attrs, k).and_then(|v| v.trim().parse::<f32>().ok());
                if let (Some(x), Some(z)) = (coord("x"), coord("z")) {
                    markers.push(Marker {
                        kind: *kind,
                        x,
                        z,
                        label: label.clone(),
                    });
                }
            }
            Tag::Close(name) if name.eq_ignore_ascii_case("event") => current = None,
            _ => {}
        }
    }
    markers
}

fn effect_zones(json: &str) -> Vec<Zone> {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(areas) = root.get("Areas").and_then(|a| a.as_array()) else {
        return Vec::new();
    };
    areas
        .iter()
        .filter_map(|area| {
            let data = area.get("Data")?;
            let pos = data.get("Pos")?.as_array()?;
            let (x, z) = (pos.first()?.as_f64()?, pos.get(2)?.as_f64()?);
            let radius = data.get("Radius")?.as_f64()?;
            let label = area
                .get("AreaName")
                .and_then(|n| n.as_str())
                .unwrap_or("Contaminated zone");
            Some(Zone {
                x: x as f32,
                z: z as f32,
                radius: radius as f32,
                label: label.replace(['-', '_'], " "),
            })
        })
        .collect()
}

/// Chernarus names its places in Cyrillic; players know them by their Latin spellings
/// (Черногорск -> Chernogorsk).
fn transliterate(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let lower = c.to_lowercase().next().unwrap_or(c);
        let latin = match lower {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "kh",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ъ' | 'ь' => "",
            'ы' => "y",
            'э' => "e",
            'ю' => "yu",
            'я' => "ya",
            _ => {
                out.push(c);
                continue;
            }
        };
        if c != lower {
            let mut chars = latin.chars();
            out.extend(chars.next().map(|f| f.to_ascii_uppercase()));
            out.extend(chars);
        } else {
            out.push_str(latin);
        }
    }
    out
}

/// `Land_Mil_Barracks1` -> `Mil Barracks1`, `StaticHeliCrash` -> `Heli Crash`.
fn pretty(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut name = name;
    for prefix in ["land_", "static", "vehicle"] {
        if lower.starts_with(prefix) && name.len() > prefix.len() {
            name = &name[prefix.len()..];
            break;
        }
    }
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = c.is_lowercase();
        out.push(c);
    }
    out.trim().to_string()
}

fn attr<'a>(attrs: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| *v)
}

enum Tag<'a> {
    Open(&'a str, Vec<(&'a str, &'a str)>),
    Close(&'a str),
}

/// A forgiving XML tag scanner: mod-made economy files are often not strictly valid XML.
struct Tags<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Tags<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }
}

impl<'a> Iterator for Tags<'a> {
    type Item = Tag<'a>;

    fn next(&mut self) -> Option<Tag<'a>> {
        loop {
            let rest = &self.text[self.pos..];
            let start = rest.find('<')?;
            let body_start = self.pos + start + 1;
            if self.text[body_start..].starts_with("!--") {
                let end = self.text[body_start..]
                    .find("-->")
                    .map_or(self.text.len(), |e| body_start + e + 3);
                self.pos = end;
                continue;
            }
            let end = self.text[body_start..]
                .find('>')
                .map_or(self.text.len(), |e| body_start + e);
            self.pos = (end + 1).min(self.text.len());
            let body = self.text[body_start..end].trim().trim_end_matches('/');
            if body.starts_with('?') || body.starts_with('!') {
                continue;
            }
            if let Some(name) = body.strip_prefix('/') {
                return Some(Tag::Close(name.trim()));
            }
            let (name, rest) = body.split_once(char::is_whitespace).unwrap_or((body, ""));
            return Some(Tag::Open(name, parse_attrs(rest)));
        }
    }
}

fn parse_attrs(mut s: &str) -> Vec<(&str, &str)> {
    let mut attrs = Vec::new();
    while let Some(eq) = s.find('=') {
        let key = s[..eq].trim();
        let after = s[eq + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(close) = after[1..].find(quote) else {
            break;
        };
        attrs.push((key, &after[1..1 + close]));
        s = &after[close + 2..];
    }
    attrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transliterates() {
        assert_eq!(transliterate("Черногорск"), "Chernogorsk");
        assert_eq!(transliterate("Электрозаводск"), "Elektrozavodsk");
        assert_eq!(transliterate("Bielawa"), "Bielawa");
    }

    #[test]
    fn scans_loose_xml() {
        let xml = r#"<group name="Land_A" pos="1 2 3"/><!-- <group name="x"> --><event name='StaticHeliCrash'><pos x="5" z="6" /></event>"#;
        let markers = events_markers(xml);
        assert_eq!(markers.len(), 1);
        assert_eq!((markers[0].x, markers[0].z), (5.0, 6.0));
        assert_eq!(pretty("Land_Mil_Barracks1"), "Mil Barracks1");
        assert_eq!(pretty("StaticHeliCrash"), "Heli Crash");
    }
}
