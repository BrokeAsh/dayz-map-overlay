//! Where each item can spawn, from the Central Economy files:
//!
//! - `types.xml` gives each item its loot category (weapons, food, ...), the spot tags it needs
//!   (shelves, floor, ...), the building usages it spawns in (Town, Military, ...), and the
//!   tiers it allows. A missing list allows anything.
//! - `mapgroupproto.xml` gives each building model its usages and loot containers, each with
//!   the categories and tags it takes and how many spots it has.
//! - `mapgrouppos.xml` places the buildings, and `areaflags.map` gives the tier (and extra
//!   usages, such as Coast or Hunting) at each spot on the map.
//!
//! An item can spawn in a building when the building's usages (its own plus the area's) include
//! one of the item's, the area's tier is one the item allows, and one of its containers takes
//! the item's category and tag. This is the map's default economy; servers change it.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::items::{Group, ItemConfigs};
use super::poi::{Tag, Tags, Zone, attr};

/// The vanilla lists, in `cfglimitsdefinition.xml`'s order (which `areaflags.map` follows),
/// for maps that don't ship the file.
const USAGES: [&str; 17] = [
    "Military",
    "Police",
    "Medic",
    "Firefighter",
    "Industrial",
    "Farm",
    "Coast",
    "Town",
    "Village",
    "Hunting",
    "Office",
    "School",
    "Prison",
    "Lunapark",
    "SeasonalEvent",
    "ContaminatedArea",
    "Historical",
];
const TIERS: [&str; 5] = ["Tier1", "Tier2", "Tier3", "Tier4", "Unique"];
const CATEGORIES: [&str; 8] = [
    "tools",
    "containers",
    "clothes",
    "lootdispatch",
    "food",
    "weapons",
    "books",
    "explosives",
];
const TAGS: [&str; 3] = ["floor", "shelves", "ground"];
const CONTAMINATED: &str = "contaminatedarea";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Loot {
    /// Names of the bits in the masks below.
    pub usages: Vec<String>,
    pub categories: Vec<String>,
    pub tags: Vec<String>,
    /// The map's tiers (value flags), as named in its files: bit `i` of a tier mask is
    /// `tiers[i]`. Livonia has Tier1-3 and Unique.
    pub tiers: Vec<String>,
    /// Whether the map's tier map was read; without it every building counts as every tier.
    pub tiered: bool,
    pub items: Vec<Item>,
    pub protos: Vec<Proto>,
    pub buildings: Vec<Building>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub class: String,
    pub name: String,
    pub group: Group,
    /// Masks; 0 allows anything.
    #[serde(default)]
    pub category: u64,
    #[serde(default)]
    pub tags: u64,
    #[serde(default)]
    pub usages: u64,
    #[serde(default)]
    pub tiers: u8,
    pub nominal: u32,
    /// Also spawns at dynamic events: heli crashes, police cars, convoys.
    #[serde(default)]
    pub events: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proto {
    pub name: String,
    pub usages: u64,
    #[serde(default)]
    pub lootmax: Option<u16>,
    pub containers: Vec<Container>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Container {
    /// Masks; 0 takes anything.
    pub categories: u64,
    pub tags: u64,
    pub spots: u16,
    #[serde(default)]
    pub lootmax: Option<u16>,
}

/// A placed building: its proto, position, the area's usages there, and its tier mask (0:
/// unknown).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Building(pub u32, pub f32, pub f32, pub u64, pub u8);

impl Building {
    pub fn proto(&self) -> usize {
        self.0 as usize
    }
    pub fn x(&self) -> f32 {
        self.1
    }
    pub fn z(&self) -> f32 {
        self.2
    }
    pub fn area_usages(&self) -> u64 {
        self.3
    }
    pub fn tier(&self) -> u8 {
        self.4
    }
}

impl Item {
    /// Whether a building with these usages, in an area of this tier, is one it spawns in.
    pub fn allowed(&self, usages: u64, tier: u8) -> bool {
        (self.usages == 0 || self.usages & usages != 0)
            && (self.tiers == 0 || tier == 0 || self.tiers & tier != 0)
    }

    /// Whether a container takes it.
    pub fn fits(&self, c: &Container) -> bool {
        (self.category == 0 || c.categories == 0 || self.category & c.categories != 0)
            && (self.tags == 0 || self.tags & c.tags != 0)
    }
}

impl Loot {
    /// How many items the building can hold at once from those `wanted` (indices into
    /// `items`): its matching spots, capped by its containers' and its own limits. 0 if none
    /// of them spawn there.
    pub fn capacity(&self, building: &Building, wanted: &[usize]) -> u32 {
        let Some(proto) = self.protos.get(building.proto()) else {
            return 0;
        };
        let usages = proto.usages | building.area_usages();
        let allowed: Vec<&Item> = wanted
            .iter()
            .filter_map(|&i| self.items.get(i))
            .filter(|item| item.allowed(usages, building.tier()))
            .collect();
        if allowed.is_empty() {
            return 0;
        }
        let total: u32 = proto
            .containers
            .iter()
            .filter(|c| allowed.iter().any(|item| item.fits(c)))
            .map(|c| u32::from(c.lootmax.map_or(c.spots, |m| m.min(c.spots))))
            .sum();
        proto.lootmax.map_or(total, |m| total.min(u32::from(m)))
    }

    /// The items (indices) that can spawn in a building.
    pub fn items_in(&self, building: &Building) -> Vec<usize> {
        let Some(proto) = self.protos.get(building.proto()) else {
            return Vec::new();
        };
        let usages = proto.usages | building.area_usages();
        (0..self.items.len())
            .filter(|&i| {
                let item = &self.items[i];
                item.allowed(usages, building.tier())
                    && proto.containers.iter().any(|c| item.fits(c))
            })
            .collect()
    }

    /// The tier names in a mask.
    pub fn tier_names(&self, tier: u8) -> Vec<&str> {
        self.tiers
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < 8 && tier & (1 << i) != 0)
            .map(|(_, t)| t.as_str())
            .collect()
    }
}

/// Names to mask bits, starting from a known list (whose order is the tier map's) and adding
/// any others as they're seen, up to 64. Names match in any case, as in the game.
#[derive(Debug)]
struct Bits(Vec<String>);

impl Bits {
    fn new(known: impl IntoIterator<Item = String>) -> Self {
        Self(known.into_iter().collect())
    }

    fn bit(&mut self, name: &str) -> u64 {
        let i = match self.0.iter().position(|n| n.eq_ignore_ascii_case(name)) {
            Some(i) => i,
            None if self.0.len() < 64 => {
                self.0.push(name.to_string());
                self.0.len() - 1
            }
            None => return 0,
        };
        1 << i
    }

    fn get(&self, name: &str) -> u64 {
        self.0
            .iter()
            .position(|n| n.eq_ignore_ascii_case(name))
            .map_or(0, |i| 1 << i)
    }
}

/// The lists in `cfglimitsdefinition.xml`: usages, values (tiers), categories, tags.
fn limits(text: Option<&str>) -> [Vec<String>; 4] {
    let defaults = || {
        [
            USAGES.map(String::from).to_vec(),
            TIERS.map(String::from).to_vec(),
            CATEGORIES.map(String::from).to_vec(),
            TAGS.map(String::from).to_vec(),
        ]
    };
    let Some(text) = text else {
        return defaults();
    };
    let mut lists: [Vec<String>; 4] = Default::default();
    for tag in Tags::new(text) {
        if let Tag::Open(kind, attrs) = tag {
            let list = match kind.to_ascii_lowercase().as_str() {
                "usage" => 0,
                "value" => 1,
                "category" => 2,
                "tag" => 3,
                _ => continue,
            };
            if let Some(name) = attr(&attrs, "name") {
                lists[list].push(name.to_string());
            }
        }
    }
    let defaults = defaults();
    for (list, default) in lists.iter_mut().zip(defaults) {
        if list.is_empty() {
            *list = default;
        }
    }
    lists
}

/// One `types.xml` entry, before its names become bits.
#[derive(Debug, Default)]
struct TypeEntry {
    class: String,
    nominal: u32,
    events: bool,
    category: Option<String>,
    tags: Vec<String>,
    usages: Vec<String>,
    values: Vec<String>,
}

fn types(text: &str) -> Vec<TypeEntry> {
    let mut out = Vec::new();
    let mut current: Option<TypeEntry> = None;
    let mut tags = Tags::new(text);
    while let Some(tag) = tags.next() {
        match tag {
            Tag::Open(name, attrs) => match name.to_ascii_lowercase().as_str() {
                "type" => {
                    current = attr(&attrs, "name").map(|n| TypeEntry {
                        class: n.to_string(),
                        ..Default::default()
                    });
                }
                "nominal" => {
                    if let Some(t) = &mut current {
                        t.nominal = tags.text().parse().unwrap_or(0);
                    }
                }
                "flags" => {
                    if let Some(t) = &mut current {
                        t.events = attr(&attrs, "deloot") == Some("1");
                    }
                }
                list @ ("category" | "tag" | "usage" | "value") => {
                    if let (Some(t), Some(value)) = (&mut current, attr(&attrs, "name")) {
                        let value = value.to_string();
                        match list {
                            "category" => t.category = Some(value),
                            "tag" => t.tags.push(value),
                            "usage" => t.usages.push(value),
                            _ => t.values.push(value),
                        }
                    }
                }
                _ => {}
            },
            Tag::Close(name) if name.eq_ignore_ascii_case("type") => {
                out.extend(current.take());
            }
            Tag::Close(_) => {}
        }
    }
    out
}

/// Building models' usages and containers, keyed by lower-case model name.
fn protos(text: &str, usages: &mut Bits, categories: &mut Bits, tags: &mut Bits) -> Vec<Proto> {
    let mut out = Vec::new();
    let mut group: Option<Proto> = None;
    let mut container: Option<Container> = None;
    let limit = |attrs: &[(&str, &str)]| attr(attrs, "lootmax").and_then(|m| m.parse().ok());
    for tag in Tags::new(text) {
        match tag {
            Tag::Open(name, attrs) => match name.to_ascii_lowercase().as_str() {
                "group" => {
                    group = attr(&attrs, "name").map(|n| Proto {
                        name: n.to_ascii_lowercase(),
                        usages: 0,
                        lootmax: limit(&attrs),
                        containers: Vec::new(),
                    });
                }
                "container" => {
                    container = Some(Container {
                        categories: 0,
                        tags: 0,
                        spots: 0,
                        lootmax: limit(&attrs),
                    });
                }
                "usage" => {
                    // Only the group's own; containers don't list usages.
                    if let (Some(g), None, Some(n)) = (&mut group, &container, attr(&attrs, "name"))
                    {
                        g.usages |= usages.bit(n);
                    }
                }
                "category" => {
                    if let (Some(c), Some(n)) = (&mut container, attr(&attrs, "name")) {
                        c.categories |= categories.bit(n);
                    }
                }
                "tag" => {
                    if let (Some(c), Some(n)) = (&mut container, attr(&attrs, "name")) {
                        c.tags |= tags.bit(n);
                    }
                }
                "point" => {
                    if let Some(c) = &mut container {
                        c.spots = c.spots.saturating_add(1);
                    }
                }
                _ => {}
            },
            Tag::Close(name) => match name.to_ascii_lowercase().as_str() {
                "container" => {
                    if let (Some(g), Some(c)) = (&mut group, container.take())
                        && c.spots > 0
                    {
                        g.containers.push(c);
                    }
                }
                "group" => {
                    container = None;
                    out.extend(group.take().filter(|g| !g.containers.is_empty()));
                }
                _ => {}
            },
        }
    }
    out
}

/// Placed buildings: model name (lower-case), x, z.
pub type Placed = Vec<(String, f32, f32)>;

/// Placed buildings from `mapgrouppos.xml`.
pub fn positions(text: &str) -> Placed {
    Tags::new(text)
        .filter_map(|tag| {
            let Tag::Open(name, attrs) = tag else {
                return None;
            };
            if !name.eq_ignore_ascii_case("group") {
                return None;
            }
            let mut pos = attr(&attrs, "pos")?
                .split_whitespace()
                .map(str::parse::<f32>);
            let (x, _, z) = (pos.next()?.ok()?, pos.next()?.ok()?, pos.next()?.ok()?);
            let model = attr(&attrs, "name")?.to_ascii_lowercase();
            (x.is_finite() && z.is_finite()).then_some((model, x, z))
        })
        .collect()
}

/// `areaflags.map`: a header (cells across and down, metres across and down, ...), then a
/// usage mask per cell (32 bits), then a tier mask per cell: 8 bits, or 4 (two cells to a byte,
/// low half first) on maps with at most four tiers. Rows run south to north.
struct AreaFlags<'a> {
    cells: (usize, usize),
    metres: (f32, f32),
    usages: &'a [u8],
    tiers: &'a [u8],
    packed: bool,
}

impl<'a> AreaFlags<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        let word = |i: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                data.get(i * 4..i * 4 + 4)?.try_into().ok()?,
            ))
        };
        let (w, h) = (word(0)? as usize, word(1)? as usize);
        let (mw, mh) = (word(2)? as f32, word(3)? as f32);
        let n = w.checked_mul(h)?;
        let tiers_len = data.len().checked_sub(24 + n.checked_mul(4)?)?;
        let packed = match tiers_len {
            _ if n == 0 || mw <= 0.0 || mh <= 0.0 => return None,
            t if t == n => false,
            t if t == n.div_ceil(2) => true,
            _ => return None,
        };
        Some(Self {
            cells: (w, h),
            metres: (mw, mh),
            usages: &data[24..24 + n * 4],
            tiers: &data[24 + n * 4..],
            packed,
        })
    }

    fn at(&self, x: f32, z: f32) -> (u32, u8) {
        let (w, h) = self.cells;
        let cx = ((x / self.metres.0 * w as f32) as usize).min(w - 1);
        let cz = ((z / self.metres.1 * h as f32) as usize).min(h - 1);
        let i = cz * w + cx;
        let usage = u32::from_le_bytes(self.usages[i * 4..i * 4 + 4].try_into().unwrap());
        let tier = match (self.packed, i % 2) {
            (false, _) => self.tiers[i],
            (true, 0) => self.tiers[i / 2] & 0xf,
            (true, _) => self.tiers[i / 2] >> 4,
        };
        (usage, tier)
    }
}

/// Everything the loot view needs, from a map's economy files (keyed by lower-case file name):
/// `types` and `proto` may come from the base game when the map has none. `placed` are the
/// buildings (from `mapgrouppos.xml`, or the terrain); `zones` are the static contaminated
/// zones, whose buildings take ContaminatedArea loot.
pub fn build(
    files: &HashMap<String, Vec<u8>>,
    base: Option<&HashMap<String, Vec<u8>>>,
    placed: &[(String, f32, f32)],
    zones: &[Zone],
    configs: &ItemConfigs,
) -> Option<Loot> {
    let text = |name: &str| {
        files
            .get(name)
            .or_else(|| base.and_then(|b| b.get(name)))
            .map(|d| String::from_utf8_lossy(d).into_owned())
    };
    let proto_text = text("mapgroupproto.xml")?;
    // Some maps ship a placeholder item list (Namalsk's is empty): then the base game's.
    let spawnable = |entries: Vec<TypeEntry>| {
        let entries: Vec<TypeEntry> = entries.into_iter().filter(|t| t.nominal > 0).collect();
        (!entries.is_empty()).then_some(entries)
    };
    let read = |files: &HashMap<String, Vec<u8>>| {
        files
            .get("types.xml")
            .and_then(|d| spawnable(types(&String::from_utf8_lossy(d))))
    };
    let entries = read(files).or_else(|| base.and_then(read))?;
    let [usage_names, tier_names, category_names, tag_names] = limits(
        files
            .get("cfglimitsdefinition.xml")
            .map(|d| String::from_utf8_lossy(d).into_owned())
            .as_deref(),
    );
    let mut usages = Bits::new(usage_names);
    let mut categories = Bits::new(category_names);
    let mut tags = Bits::new(tag_names);
    let tier_bit = |name: &str| {
        tier_names
            .iter()
            .position(|t| t.eq_ignore_ascii_case(name))
            .filter(|&i| i < 8)
            .map_or(0, |i| 1u8 << i)
    };

    let protos = protos(&proto_text, &mut usages, &mut categories, &mut tags);
    let index: HashMap<&str, u32> = protos
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i as u32))
        .collect();

    let mut items: Vec<Item> = entries
        .into_iter()
        .map(|t| {
            let lower_usages: Vec<String> =
                t.usages.iter().map(|u| u.to_ascii_lowercase()).collect();
            let category = t.category.as_deref().map(str::to_ascii_lowercase);
            let (name, group) = configs.describe(&t.class, category.as_deref(), &lower_usages);
            Item {
                name,
                group,
                category: t.category.as_deref().map_or(0, |c| categories.bit(c)),
                tags: t.tags.iter().fold(0, |m, n| m | tags.bit(n)),
                usages: t.usages.iter().fold(0, |m, n| m | usages.bit(n)),
                tiers: t.values.iter().fold(0, |m, n| m | tier_bit(n)),
                nominal: t.nominal,
                events: t.events,
                class: t.class,
            }
        })
        .collect();
    items.sort_by_key(|i| i.name.to_lowercase());

    // The tier map only belongs to the map's own buildings, not the base game's.
    let flags = files.get("areaflags.map").and_then(|d| AreaFlags::parse(d));
    let contaminated = usages.get(CONTAMINATED);
    let buildings: Vec<Building> = placed
        .iter()
        .filter_map(|(model, x, z)| {
            let proto = *index.get(model.as_str())?;
            let (mut area, tier) = flags.as_ref().map_or((0, 0), |f| {
                let (u, t) = f.at(*x, *z);
                (u64::from(u), t)
            });
            let inside = |zone: &Zone| (zone.x - x).hypot(zone.z - z) <= zone.radius;
            if zones.iter().any(inside) {
                area |= contaminated;
            }
            Some(Building(proto, *x, *z, area, tier))
        })
        .collect();
    if buildings.is_empty() || items.is_empty() {
        return None;
    }
    Some(Loot {
        usages: usages.0,
        categories: categories.0,
        tags: tags.0,
        tiers: tier_names,
        tiered: flags.is_some(),
        items,
        protos,
        buildings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TYPES: &str = r#"<types>
        <type name="Mosin9130">
            <nominal>45</nominal>
            <flags count_in_map="1" deloot="0"/>
            <category name="weapons"/>
            <usage name="Town"/><usage name="Village"/>
            <value name="Tier2"/><value name="Tier3"/>
        </type>
        <type name="Ammo_762x54">
            <nominal>60</nominal>
            <category name="weapons"/><tag name="shelves"/>
            <usage name="Hunting"/>
        </type>
        <type name="WaterBottle"><nominal>100</nominal></type>
        <type name="Apple"><nominal>0</nominal><category name="food"/></type>
        <type name="SVD"><nominal>4</nominal><flags deloot="1"/><category name="weapons"/>
            <usage name="ContaminatedArea"/></type>
    </types>"#;

    const PROTO: &str = r#"<prototype><group name="Land_House">
            <usage name="Village" />
            <container name="lootFloor"><category name="tools"/><tag name="floor"/>
                <point pos="0 0 0"/><point pos="1 0 0"/></container>
            <container name="lootweapons" lootmax="2"><category name="weapons"/>
                <point pos="0 0 0"/><point pos="1 0 0"/><point pos="2 0 0"/></container>
        </group>
        <group name="Land_DeerStand" lootmax="1">
            <usage name="Hunting" />
            <container name="lootshelves"><category name="weapons"/><tag name="shelves"/>
                <point pos="0 0 0"/><point pos="1 0 0"/></container>
        </group></prototype>"#;

    /// A 2x2 tier map over 1000 m: Tier1 in the south-west, Tier3 elsewhere.
    fn area_flags() -> Vec<u8> {
        let mut data = Vec::new();
        for v in [2u32, 2, 1000, 1000, 32, 0] {
            data.extend(v.to_le_bytes());
        }
        data.extend([0u8; 16]);
        data.extend([1u8, 4, 4, 4]);
        data
    }

    fn loot(zones: &[Zone]) -> Loot {
        let files = HashMap::from([
            ("types.xml".to_string(), TYPES.as_bytes().to_vec()),
            ("mapgroupproto.xml".to_string(), PROTO.as_bytes().to_vec()),
            ("areaflags.map".to_string(), area_flags()),
        ]);
        let placed = [
            ("land_house".to_string(), 100.0, 100.0),
            ("land_house".to_string(), 900.0, 900.0),
            ("land_deerstand".to_string(), 900.0, 100.0),
            ("land_unknown".to_string(), 500.0, 500.0),
        ];
        build(&files, None, &placed, zones, &ItemConfigs::default()).unwrap()
    }

    fn find(loot: &Loot, class: &str) -> usize {
        loot.items.iter().position(|i| i.class == class).unwrap()
    }

    #[test]
    fn where_items_spawn() {
        let loot = loot(&[]);
        assert!(loot.tiered);
        // Nominal 0 doesn't spawn; unknown models have no loot.
        assert!(loot.items.iter().all(|i| i.class != "Apple"));
        assert_eq!(loot.buildings.len(), 3);
        let [south_west, north_east, stand] =
            [loot.buildings[0], loot.buildings[1], loot.buildings[2]];
        assert_eq!((south_west.tier(), north_east.tier()), (1, 4));

        let mosin = find(&loot, "Mosin9130");
        // Village house, but Tier1 there.
        assert_eq!(loot.capacity(&south_west, &[mosin]), 0);
        // Its weapon container's three spots, capped at two.
        assert_eq!(loot.capacity(&north_east, &[mosin]), 2);
        // Hunting only, and only shelves.
        let ammo = find(&loot, "Ammo_762x54");
        assert_eq!(loot.capacity(&north_east, &[ammo]), 0);
        assert_eq!(loot.capacity(&stand, &[ammo]), 1);
        // No usage, category, tag or tier: anywhere.
        let water = find(&loot, "WaterBottle");
        assert_eq!(loot.capacity(&south_west, &[water]), 4);
        assert_eq!(
            loot.items_in(&stand),
            vec![find(&loot, "Ammo_762x54"), water]
        );
        // Both: the containers either one fits.
        assert_eq!(loot.capacity(&north_east, &[mosin, water]), 4);
        assert!(loot.items[find(&loot, "SVD")].events);
        assert_eq!(loot.tier_names(5), ["Tier1", "Tier3"]);
    }

    #[test]
    fn reads_packed_tiers() {
        // 2x2 cells, four bits each: Tier1, Tier3 / Unique, Tier2.
        let mut data = Vec::new();
        for v in [2u32, 2, 100, 100, 32, 0] {
            data.extend(v.to_le_bytes());
        }
        data.extend([0u8; 16]);
        data.extend([0x41u8, 0x28]);
        let flags = AreaFlags::parse(&data).unwrap();
        assert_eq!(flags.at(10.0, 10.0).1, 1);
        assert_eq!(flags.at(90.0, 10.0).1, 4);
        assert_eq!(flags.at(10.0, 90.0).1, 8);
        assert_eq!(flags.at(90.0, 90.0).1, 2);
        assert!(AreaFlags::parse(&data[..data.len() - 1]).is_none());
    }

    #[test]
    fn contaminated_zones_take_their_loot() {
        let svd = |loot: &Loot| find(loot, "SVD");
        let plain = loot(&[]);
        assert!(
            plain
                .buildings
                .iter()
                .all(|b| plain.capacity(b, &[svd(&plain)]) == 0)
        );
        let zone = Zone {
            x: 900.0,
            z: 900.0,
            radius: 50.0,
            label: "zone".into(),
        };
        let zoned = loot(&[zone]);
        assert_eq!(zoned.capacity(&zoned.buildings[1], &[svd(&zoned)]), 2);
        assert_eq!(zoned.capacity(&zoned.buildings[0], &[svd(&zoned)]), 0);
    }

    #[test]
    fn empty_item_lists_fall_back_to_the_base_game() {
        let files = HashMap::from([
            ("types.xml".to_string(), b"<types>\n</types>".to_vec()),
            ("mapgroupproto.xml".to_string(), PROTO.as_bytes().to_vec()),
        ]);
        let base = HashMap::from([("types.xml".to_string(), TYPES.as_bytes().to_vec())]);
        let placed = [("land_house".to_string(), 1.0, 1.0)];
        let loot = build(&files, Some(&base), &placed, &[], &ItemConfigs::default()).unwrap();
        assert!(loot.items.iter().any(|i| i.class == "Mosin9130"));
    }

    #[test]
    fn needs_types_and_prototypes() {
        let files = HashMap::from([("types.xml".to_string(), TYPES.as_bytes().to_vec())]);
        let placed = [("land_house".to_string(), 1.0, 1.0)];
        assert!(build(&files, None, &placed, &[], &ItemConfigs::default()).is_none());
        let base = HashMap::from([("mapgroupproto.xml".to_string(), PROTO.as_bytes().to_vec())]);
        let loot = build(&files, Some(&base), &placed, &[], &ItemConfigs::default()).unwrap();
        assert!(!loot.tiered);
        assert_eq!(loot.buildings[0].tier(), 0);
    }
}
