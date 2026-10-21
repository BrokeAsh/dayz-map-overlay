//! What the game's items are called and what kind they are, from its configs
//! (`CfgVehicles`, `CfgWeapons`, `CfgMagazines`) and its string table: `Mosin9130` is the
//! "Mosin 91/30", a gun.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::pbo::Pbo;
use super::rap;

/// The kinds of item the overlay lets you pick from, in the order it lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Group {
    Guns,
    Ammo,
    Magazines,
    Attachments,
    Food,
    Drinks,
    Medical,
    Clothing,
    Backpacks,
    Vests,
    Tools,
    Containers,
    Explosives,
    VehicleParts,
    Books,
    Other,
}

impl Group {
    pub const ALL: [Group; 16] = [
        Group::Guns,
        Group::Ammo,
        Group::Magazines,
        Group::Attachments,
        Group::Food,
        Group::Drinks,
        Group::Medical,
        Group::Clothing,
        Group::Backpacks,
        Group::Vests,
        Group::Tools,
        Group::Containers,
        Group::Explosives,
        Group::VehicleParts,
        Group::Books,
        Group::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::Guns => "Guns",
            Group::Ammo => "Ammo",
            Group::Magazines => "Magazines",
            Group::Attachments => "Attachments",
            Group::Food => "Food",
            Group::Drinks => "Drinks",
            Group::Medical => "Medical",
            Group::Clothing => "Clothing",
            Group::Backpacks => "Backpacks",
            Group::Vests => "Vests",
            Group::Tools => "Tools and melee",
            Group::Containers => "Tents and storage",
            Group::Explosives => "Explosives",
            Group::VehicleParts => "Vehicle parts",
            Group::Books => "Books",
            Group::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    Vehicles,
    Weapons,
    Magazines,
}

#[derive(Debug, Default)]
struct Definition {
    /// Lower-case.
    parent: String,
    /// Where the config defining it lives, lower-case (`dz\gear\medical`): the game files
    /// items by kind.
    folder: String,
    display: Option<String>,
    /// `inventorySlot[]`, lower-case.
    slots: Option<Vec<String>>,
}

/// The game's item classes and English strings.
#[derive(Debug, Default)]
pub struct ItemConfigs {
    classes: HashMap<String, (Root, Definition)>,
    strings: HashMap<String, String>,
}

impl ItemConfigs {
    /// Reads every `config.bin` and `stringtable.csv` in `pbos`. Unreadable ones are skipped:
    /// names then fall back to the class name.
    pub fn read(pbos: &[Arc<Pbo>]) -> Self {
        let mut configs = Self::default();
        for pbo in pbos {
            for entry in &pbo.entries {
                let lower = entry.name.to_ascii_lowercase();
                let file = lower.rsplit('\\').next().unwrap_or(&lower);
                if file != "config.bin" && file != "stringtable.csv" {
                    continue;
                }
                let data = match pbo.read(entry) {
                    Ok(data) => data,
                    Err(e) => {
                        log::debug!("{} {}: {e:#}", pbo.path.display(), entry.name);
                        continue;
                    }
                };
                if file == "stringtable.csv" {
                    configs.add_strings(&String::from_utf8_lossy(&data));
                } else {
                    let folder = match lower.rsplit_once('\\') {
                        Some((dir, _)) => format!("{}\\{dir}", pbo.prefix),
                        None => pbo.prefix.clone(),
                    };
                    match rap::parse(&data) {
                        Ok(config) => configs.add_config(&config, &folder.to_ascii_lowercase()),
                        Err(e) => log::debug!("{} {}: {e:#}", pbo.path.display(), entry.name),
                    }
                }
            }
        }
        configs
    }

    fn add_config(&mut self, config: &rap::Class, folder: &str) {
        let roots = [
            ("CfgVehicles", Root::Vehicles),
            ("CfgWeapons", Root::Weapons),
            ("CfgMagazines", Root::Magazines),
        ];
        for (name, root) in roots {
            let Some(classes) = config.class(name) else {
                continue;
            };
            for (class, body) in classes.classes() {
                let (_, def) = self
                    .classes
                    .entry(class.to_ascii_lowercase())
                    .or_insert_with(|| (root, Definition::default()));
                // A patch repeats the class to change some of it: keep what it doesn't say.
                if !body.parent.is_empty() {
                    def.parent = body.parent.to_ascii_lowercase();
                    // Where the game files it, even when a mod redefines it.
                    if !def.folder.starts_with("dz\\") {
                        def.folder = folder.to_string();
                    }
                }
                if let Some(display) = body.value("displayName").and_then(|v| v.as_str()) {
                    def.display = Some(display.to_string());
                }
                if let Some(slots) = body.value("inventorySlot") {
                    let slots = match slots {
                        rap::Value::Array(items) => items
                            .iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_ascii_lowercase)
                            .collect(),
                        other => other
                            .as_str()
                            .map(|s| vec![s.to_ascii_lowercase()])
                            .unwrap_or_default(),
                    };
                    def.slots = Some(slots);
                }
            }
        }
    }

    /// The English column of a string table (`"STR_x","original","english",...`).
    fn add_strings(&mut self, csv: &str) {
        let mut rows = csv_rows(csv);
        let Some(header) = rows.next() else {
            return;
        };
        let column = header
            .iter()
            .position(|h| h.eq_ignore_ascii_case("english"))
            .or_else(|| {
                header
                    .iter()
                    .position(|h| h.eq_ignore_ascii_case("original"))
            })
            .unwrap_or(1);
        for row in rows {
            if let (Some(key), Some(text)) = (row.first(), row.get(column))
                && !key.is_empty()
                && !text.is_empty()
            {
                self.strings.insert(key.to_ascii_lowercase(), text.clone());
            }
        }
    }

    /// The class and its ancestors, lower-case, nearest first.
    fn lineage(&self, class: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut next = Some(class.to_ascii_lowercase());
        while let Some(name) = next.take() {
            if out.contains(&name) || out.len() > 64 {
                break;
            }
            next = self
                .classes
                .get(&name)
                .map(|(_, d)| d.parent.clone())
                .filter(|p| !p.is_empty());
            out.push(name);
        }
        out
    }

    /// The item's English name and kind. `category` and `usages` are from `types.xml`
    /// (lower-case), for items the configs don't settle.
    pub fn describe(
        &self,
        class: &str,
        category: Option<&str>,
        usages: &[String],
    ) -> (String, Group) {
        let lineage = self.lineage(class);
        let defs: Vec<&(Root, Definition)> =
            lineage.iter().filter_map(|c| self.classes.get(c)).collect();
        let name = defs
            .iter()
            .find_map(|(_, d)| d.display.as_deref())
            .and_then(|d| self.text(d))
            .unwrap_or_else(|| readable(class));
        let root = defs.first().map(|(r, _)| *r);
        let slots: &[String] = defs
            .iter()
            .find_map(|(_, d)| d.slots.as_deref())
            .unwrap_or(&[]);
        let is = |base: &str| lineage.iter().any(|c| c == base);
        let folder = defs.first().map_or("", |(_, d)| d.folder.as_str());
        let in_folder = |name: &str| folder.contains(&format!("gear\\{name}"));
        let lower = class.to_ascii_lowercase();
        let group = match root {
            Some(Root::Weapons) => Group::Guns,
            _ if is("ammunition_base") || lower.starts_with("ammobox") => Group::Ammo,
            Some(Root::Magazines) if is("magazine_base") || lower.starts_with("mag_") => {
                Group::Magazines
            }
            _ if is("clothing_base") || is("clothing") => {
                if slots.iter().any(|s| s == "back") {
                    Group::Backpacks
                } else if slots.iter().any(|s| s == "vest") {
                    Group::Vests
                } else {
                    Group::Clothing
                }
            }
            _ if slots.iter().any(|s| s.starts_with("weapon"))
                || is("itemoptics_base")
                || is("itemoptics")
                || is("itemsuppressor") =>
            {
                Group::Attachments
            }
            _ if in_folder("medical") => Group::Medical,
            _ if in_folder("drinks") => Group::Drinks,
            _ if in_folder("food") => Group::Food,
            // Mod items, filed elsewhere: by their base class. (A cooking pot is a bottle too.)
            _ if is("sodacan_colorbase")
                || (is("bottle_base") && matches!(category, None | Some("food"))) =>
            {
                Group::Drinks
            }
            _ if is("edible_base") => Group::Food,
            _ => match category {
                // Mod items: what hospitals stock. (The game's own are filed by folder.)
                _ if usages.iter().any(|u| u == "medic") && !folder.starts_with("dz\\") => {
                    Group::Medical
                }
                // (Older maps' economies say `industrialfood` and `vehiclesparts`.)
                Some("food" | "industrialfood") => Group::Food,
                Some("weapons") => Group::Tools,
                Some("tools") => Group::Tools,
                Some("clothes") => Group::Clothing,
                Some("containers") => Group::Containers,
                Some("explosives") => Group::Explosives,
                Some("lootdispatch" | "vehiclesparts") => Group::VehicleParts,
                Some("books") => Group::Books,
                _ => Group::Other,
            },
        };
        (name, group)
    }

    /// A `$STR_` reference in English, or the text itself.
    fn text(&self, display: &str) -> Option<String> {
        let display = display.trim();
        let text = match display.strip_prefix('$') {
            Some(key) => self.strings.get(&key.to_ascii_lowercase())?.clone(),
            None => display.to_string(),
        };
        (!text.trim().is_empty() && !text.starts_with('#')).then(|| text.trim().to_string())
    }
}

/// `Mosin9130_Black` -> `Mosin9130 Black`, for items without a name.
fn readable(class: &str) -> String {
    class.replace('_', " ")
}

/// The rows of a CSV file: quoted fields may hold commas, doubled quotes and line breaks.
fn csv_rows(text: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        chars.peek()?;
        let mut row = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = !quoted,
                ',' if !quoted => row.push(std::mem::take(&mut field)),
                '\n' if !quoted => break,
                '\r' if !quoted => {}
                c => field.push(c),
            }
        }
        row.push(field);
        Some(row)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rap::{Class, Entry, Value};

    fn class(parent: &str, values: Vec<(&str, Value)>) -> Class {
        Class {
            parent: parent.into(),
            entries: values
                .into_iter()
                .map(|(k, v)| (k.to_string(), Entry::Value(v)))
                .collect(),
        }
    }

    fn root(name: &str, classes: Vec<(&str, Class)>) -> Class {
        let inner = Class {
            parent: String::new(),
            entries: classes
                .into_iter()
                .map(|(k, c)| (k.to_string(), Entry::Class(c)))
                .collect(),
        };
        Class {
            parent: String::new(),
            entries: vec![(name.to_string(), Entry::Class(inner))],
        }
    }

    #[test]
    fn reads_csv() {
        let rows: Vec<_> =
            csv_rows("\"a\",\"b, c\"\r\n\"x \"\"y\"\"\",\"line\nbreak\"\n").collect();
        assert_eq!(rows[0], ["a", "b, c"]);
        assert_eq!(rows[1], ["x \"y\"", "line\nbreak"]);
    }

    #[test]
    fn names_and_groups_from_configs() {
        let mut configs = ItemConfigs::default();
        configs.add_strings(
            "\"Language\",\"original\",\"english\"\n\"STR_mosin\",\"Mosin\",\"Mosin 91/30\"\n",
        );
        configs.add_config(
            &root(
                "CfgWeapons",
                vec![
                    ("Rifle_Base", class("", vec![])),
                    (
                        "Mosin9130_Base",
                        class(
                            "Rifle_Base",
                            vec![("displayName", Value::Str("$STR_mosin".into()))],
                        ),
                    ),
                    ("Mosin9130", class("Mosin9130_Base", vec![])),
                ],
            ),
            "dz\\weapons\\firearms\\mosin9130",
        );
        configs.add_config(
            &root(
                "CfgVehicles",
                vec![
                    ("Clothing_Base", class("", vec![])),
                    (
                        "AliceBag_ColorBase",
                        class(
                            "Clothing_Base",
                            vec![
                                ("displayName", Value::Str("Field backpack".into())),
                                (
                                    "inventorySlot",
                                    Value::Array(vec![Value::Str("Back".into())]),
                                ),
                            ],
                        ),
                    ),
                    ("AliceBag_Green", class("AliceBag_ColorBase", vec![])),
                    ("Bottle_Base", class("", vec![])),
                    ("Edible_Base", class("", vec![])),
                    ("WaterBottle", class("Bottle_Base", vec![])),
                    ("Pot", class("Bottle_Base", vec![])),
                ],
            ),
            "dz\\gear",
        );
        // Pills are food to the configs; the game files them with the medicine.
        configs.add_config(
            &root(
                "CfgVehicles",
                vec![("CharcoalTablets", class("Edible_Base", vec![]))],
            ),
            "dz\\gear\\medical",
        );
        assert_eq!(
            configs.describe("CharcoalTablets", Some("tools"), &[]).1,
            Group::Medical
        );
        // A mod redefining it doesn't move it out of the game's folder.
        configs.add_config(
            &root(
                "CfgVehicles",
                vec![("CharcoalTablets", class("Edible_Base", vec![]))],
            ),
            "deerisle\\scripts",
        );
        assert_eq!(
            configs.describe("CharcoalTablets", Some("tools"), &[]).1,
            Group::Medical
        );
        assert_eq!(
            configs.describe("Fwinkies", Some("industrialfood"), &[]).1,
            Group::Food
        );
        assert_eq!(configs.describe("Pot", Some("tools"), &[]).1, Group::Tools);
        assert_eq!(
            configs.describe("Mosin9130", Some("weapons"), &[]),
            ("Mosin 91/30".into(), Group::Guns)
        );
        assert_eq!(
            configs.describe("AliceBag_Green", Some("containers"), &[]),
            ("Field backpack".into(), Group::Backpacks)
        );
        assert_eq!(configs.describe("WaterBottle", None, &[]).1, Group::Drinks);
        // Not in any config: the class name, grouped by its loot category.
        assert_eq!(
            configs.describe("Morphine", Some("tools"), &["medic".into()]),
            ("Morphine".into(), Group::Medical)
        );
        assert_eq!(
            configs.describe("Some_Door", Some("lootdispatch"), &[]),
            ("Some Door".into(), Group::VehicleParts)
        );
    }
}
