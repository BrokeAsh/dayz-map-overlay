//! The Items panel: tick kinds of item (Food, Guns, ...) or single items, and the map shows the
//! buildings they can spawn in, bigger where more of them fit. Its search box borrows the
//! keyboard while you type in it (the hosts give it back on Enter or Esc).

use egui::{Align2, Color32, Pos2, Rect, Stroke};
use std::collections::{BTreeSet, HashSet};

use super::{LoadedPois, OverlayApp, View, panel_frame};
use crate::import::items::Group;
use crate::import::loot::{Item, Loot};
use crate::import::poi;

/// Item spots: gold, brighter where more fit.
const SPOT: Color32 = Color32::from_rgb(255, 196, 40);
const SPOT_FULL: Color32 = Color32::from_rgb(255, 245, 170);

/// One line of the list: the items of one kind that share a name (a backpack's colours, say).
pub(super) struct ItemRow {
    pub group: Group,
    pub name: String,
    /// Indices into the map's items.
    pub items: Vec<usize>,
}

/// The list's lines, by kind and then name.
pub(super) fn rows(loot: &Loot) -> Vec<ItemRow> {
    let mut rows: Vec<ItemRow> = Vec::new();
    let mut order: Vec<usize> = (0..loot.items.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&loot.items[a], &loot.items[b]);
        (a.group, a.name.to_lowercase()).cmp(&(b.group, b.name.to_lowercase()))
    });
    for i in order {
        let item = &loot.items[i];
        match rows.last_mut() {
            Some(row) if row.group == item.group && row.name.eq_ignore_ascii_case(&item.name) => {
                row.items.push(i);
            }
            _ => rows.push(ItemRow {
                group: item.group,
                name: item.name.clone(),
                items: vec![i],
            }),
        }
    }
    rows
}

/// The buildings the picked items can spawn in, on one map.
pub(super) struct Matches {
    map: String,
    picked: BTreeSet<String>,
    /// Building index and how many of the picked items it can hold at once.
    pub buildings: Vec<(usize, u32)>,
    /// Picked items that also drop at heli crashes and other events.
    events: bool,
    /// Picked items that exist on this map.
    count: usize,
}

fn class_key(item: &Item) -> String {
    item.class.to_ascii_lowercase()
}

impl OverlayApp {
    /// Whether the map is showing item spots instead of the loot-building layers.
    pub(super) fn showing_items(&self, loaded: &LoadedPois) -> bool {
        loaded.pois.loot.is_some() && !self.config.view.items.is_empty()
    }

    /// Works out the picked items' buildings again if the map or the picks changed.
    pub(super) fn update_item_matches(&mut self, map: &str, loaded: &LoadedPois) {
        let picked = &self.config.view.items;
        let current = self
            .item_matches
            .as_ref()
            .is_some_and(|m| m.map == map && m.picked == *picked);
        if current {
            return;
        }
        self.item_matches = loaded.pois.loot.as_ref().map(|loot| {
            let wanted: Vec<usize> = (0..loot.items.len())
                .filter(|&i| picked.contains(&class_key(&loot.items[i])))
                .collect();
            // Items with the same rules land in the same spots: check each set of rules once.
            let mut seen = HashSet::new();
            let distinct: Vec<usize> = wanted
                .iter()
                .copied()
                .filter(|&i| {
                    let it = &loot.items[i];
                    seen.insert((it.category, it.tags, it.usages, it.tiers))
                })
                .collect();
            let buildings = if distinct.is_empty() {
                Vec::new()
            } else {
                loot.buildings
                    .iter()
                    .enumerate()
                    .filter_map(|(i, b)| {
                        let n = loot.capacity(b, &distinct);
                        (n > 0).then_some((i, n))
                    })
                    .collect()
            };
            Matches {
                map: map.to_string(),
                picked: picked.clone(),
                buildings,
                events: wanted.iter().any(|&i| loot.items[i].events),
                count: wanted.len(),
            }
        });
    }

    /// Draws the picked items' buildings; returns the one under the pointer, if any.
    pub(super) fn paint_item_spots(
        &self,
        painter: &egui::Painter,
        loaded: &LoadedPois,
        view: &View,
        screen: Rect,
        hover: Option<Pos2>,
    ) -> Option<(Pos2, String)> {
        let loot = loaded.pois.loot.as_ref()?;
        let matches = self.item_matches.as_ref()?;
        let visible = screen.expand(20.0);
        let scale = if view.zoom < 0.12 {
            0.8
        } else if view.zoom < 0.5 {
            1.0
        } else {
            1.4
        };
        let mut nearest: Option<(f32, Pos2, usize, u32)> = None;
        let outline = Stroke::new(1.0, Color32::from_black_alpha(220));
        for &(i, n) in &matches.buildings {
            let b = &loot.buildings[i];
            let p = view.to_screen(screen, f64::from(b.x()), f64::from(b.z()));
            if !visible.contains(p) {
                continue;
            }
            // 1 spot: a small amber dot; 8 or more: a large pale one.
            let t = ((n as f32).ln() / 8f32.ln()).clamp(0.0, 1.0);
            let radius = (3.0 + 3.5 * t) * scale;
            painter.circle(p, radius, lerp(SPOT, SPOT_FULL, t), outline);
            if let Some(h) = hover {
                let d = p.distance(h);
                if d <= radius.max(9.0) && nearest.as_ref().is_none_or(|(best, ..)| d < *best) {
                    nearest = Some((d, p, i, n));
                }
            }
        }
        let (_, p, i, n) = nearest?;
        Some((p, self.spot_label(loot, i, n)))
    }

    /// "House 1w03 · Tier2 · Mosin 91/30, ... · 2 spots": the spots that can hold them.
    fn spot_label(&self, loot: &Loot, building: usize, capacity: u32) -> String {
        let b = &loot.buildings[building];
        let model = loot.protos.get(b.proto()).map_or("", |p| p.name.as_str());
        let mut names: Vec<&str> = loot
            .items_in(b)
            .into_iter()
            .filter(|&i| self.config.view.items.contains(&class_key(&loot.items[i])))
            .map(|i| loot.items[i].name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        let shown = names.len().min(4);
        let mut list = names[..shown].join(", ");
        if names.len() > shown {
            list.push_str(&format!(" and {} more", names.len() - shown));
        }
        let mut parts = vec![poi::building_name(model)];
        let tiers = loot.tier_names(b.tier());
        if !tiers.is_empty() {
            parts.push(tiers.join("/"));
        }
        parts.push(list);
        parts.push(format!(
            "{capacity} {}",
            if capacity == 1 { "spot" } else { "spots" }
        ));
        parts.join(" · ")
    }

    pub(super) fn items_window(&mut self, ctx: &egui::Context, loaded: Option<&LoadedPois>) {
        let mut open = true;
        egui::Window::new("Items")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .frame(panel_frame())
            .anchor(Align2::LEFT_TOP, [16.0, 64.0])
            .show(ctx, |ui| {
                ui.set_width(330.0);
                match loaded.and_then(|l| l.pois.loot.as_ref().map(|loot| (l, loot))) {
                    Some((loaded, loot)) => self.items_panel(ui, loaded, loot),
                    None => {
                        ui.label(
                            egui::RichText::new(
                                "This map has no loot data. Maps with economy files (and those \
                                 built from the base game's buildings) get it when imported.",
                            )
                            .weak(),
                        );
                    }
                }
            });
        if !open {
            self.items_open = false;
        }
    }

    fn items_panel(&mut self, ui: &mut egui::Ui, loaded: &LoadedPois, loot: &Loot) {
        ui.horizontal(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.item_search)
                    .hint_text("Search items, such as mosin")
                    .desired_width(250.0),
            );
            if search.has_focus() {
                ui.ctx().request_repaint();
            }
            if ui.button("Clear").clicked() {
                self.item_search.clear();
                self.config.view.items.clear();
            }
        });
        let typing = ui.ctx().text_edit_focused();
        ui.label(
            egui::RichText::new(if typing {
                "Typing goes to the map. Enter or Esc gives the keys back to DayZ."
            } else {
                "Click the box to type; the game gets the keys back after."
            })
            .weak()
            .small(),
        );
        if let Some(m) = &self.item_matches
            && m.count > 0
        {
            let mut text = format!(
                "{} picked · {} buildings",
                m.count,
                thousands(m.buildings.len())
            );
            if m.events {
                text.push_str(" · some also drop at heli crashes and events");
            }
            ui.label(egui::RichText::new(text).color(SPOT));
        }
        ui.separator();

        let query = self.item_search.trim().to_lowercase();
        let matching = |row: &ItemRow| {
            query.is_empty()
                || row.name.to_lowercase().contains(&query)
                // "food" shows the Food group, but "mo" doesn't show all the Ammo.
                || row
                    .group
                    .label()
                    .to_lowercase()
                    .split(' ')
                    .any(|word| word.starts_with(&query))
                // Class names by their parts: "akm" finds `AKM`, but "mo" not every `Ammo_*`.
                || row.items.iter().any(|&i| {
                    loot.items[i]
                        .class
                        .to_lowercase()
                        .split('_')
                        .any(|part| part.starts_with(&query))
                })
        };
        let height = (ui.ctx().content_rect().height() * 0.6).max(200.0);
        egui::ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut any = false;
                for group in Group::ALL {
                    let rows: Vec<&ItemRow> = loaded
                        .rows
                        .iter()
                        .filter(|r| r.group == group && matching(r))
                        .collect();
                    if rows.is_empty() {
                        continue;
                    }
                    any = true;
                    self.group_rows(ui, loot, group, &rows, !query.is_empty());
                }
                if !any {
                    ui.label(egui::RichText::new("No items match.").weak());
                }
            });
        ui.separator();
        ui.label(
            egui::RichText::new(
                "Where the map's default loot rules let them spawn; servers can change these.",
            )
            .weak()
            .small(),
        );
    }

    fn group_rows(
        &mut self,
        ui: &mut egui::Ui,
        loot: &Loot,
        group: Group,
        rows: &[&ItemRow],
        searching: bool,
    ) {
        let picked = &mut self.config.view.items;
        let is_picked = |row: &ItemRow, picked: &BTreeSet<String>| {
            row.items
                .iter()
                .all(|&i| picked.contains(&class_key(&loot.items[i])))
        };
        let count = rows.iter().filter(|r| is_picked(r, picked)).count();
        let expanded = searching || self.items_expanded.contains(&group);
        ui.horizontal(|ui| {
            let arrow = if expanded { "⏷" } else { "⏵" };
            if ui.small_button(arrow).clicked() && !searching {
                if expanded {
                    self.items_expanded.remove(&group);
                } else {
                    self.items_expanded.insert(group);
                }
            }
            let mut all = count == rows.len();
            let label = format!("{} ({})", group.label(), rows.len());
            let response = ui.add(
                egui::Checkbox::new(&mut all, label).indeterminate(count > 0 && count < rows.len()),
            );
            if response.changed() {
                for row in rows {
                    for &i in &row.items {
                        let key = class_key(&loot.items[i]);
                        if all {
                            picked.insert(key);
                        } else {
                            picked.remove(&key);
                        }
                    }
                }
            }
        });
        if !expanded {
            return;
        }
        ui.indent(("items", group), |ui| {
            for row in rows {
                let mut on = is_picked(row, picked);
                let response = ui.checkbox(&mut on, &row.name);
                if response.changed() {
                    for &i in &row.items {
                        let key = class_key(&loot.items[i]);
                        if on {
                            picked.insert(key);
                        } else {
                            picked.remove(&key);
                        }
                    }
                }
                response.on_hover_ui(|ui| item_details(ui, loot, row));
            }
        });
    }
}

/// Where a row's items spawn, for its tooltip.
fn item_details(ui: &mut egui::Ui, loot: &Loot, row: &ItemRow) {
    ui.set_max_width(320.0);
    ui.label(egui::RichText::new(&row.name).strong());
    let items: Vec<&Item> = row.items.iter().map(|&i| &loot.items[i]).collect();
    let usages = items.iter().fold(0u64, |m, i| m | i.usages);
    let tiers = items.iter().fold(0u8, |m, i| m | i.tiers);
    let any_usage = items.iter().any(|i| i.usages == 0);
    let any_tier = items.iter().any(|i| i.tiers == 0);
    let names = |mask: u64, list: &[String]| {
        list.iter()
            .enumerate()
            .filter(|(i, _)| *i < 64 && mask & (1 << i) != 0)
            .map(|(_, n)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    ui.label(if any_usage || usages == 0 {
        "In any kind of building".to_string()
    } else {
        format!("In {} buildings", names(usages, &loot.usages))
    });
    if !any_tier && tiers != 0 {
        ui.label(format!("Tiers: {}", loot.tier_names(tiers).join(", ")));
    }
    let indices: Vec<usize> = row.items.clone();
    let buildings = loot
        .buildings
        .iter()
        .filter(|b| loot.capacity(b, &indices) > 0)
        .count();
    if buildings > 0 {
        ui.label(format!("{} buildings on this map", thousands(buildings)));
    } else {
        ui.label(
            egui::RichText::new("Doesn't spawn in buildings on this map")
                .color(Color32::from_rgb(255, 170, 120)),
        );
    }
    if items.iter().any(|i| i.events) {
        ui.label("Also drops at heli crashes and other events");
    }
    let classes: Vec<&str> = items.iter().map(|i| i.class.as_str()).collect();
    ui.label(egui::RichText::new(classes.join(", ")).weak().small());
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn rows_merge_colours() {
        let item = |class: &str, name: &str, group| Item {
            class: class.into(),
            name: name.into(),
            group,
            category: 0,
            tags: 0,
            usages: 0,
            tiers: 0,
            nominal: 1,
            events: false,
        };
        let loot = Loot {
            items: vec![
                item("AliceBag_Green", "Field Backpack", Group::Backpacks),
                item("Mosin9130", "Mosin 91/30", Group::Guns),
                item("AliceBag_Black", "Field Backpack", Group::Backpacks),
            ],
            ..Default::default()
        };
        let rows = rows(&loot);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].group, rows[0].name.as_str()),
            (Group::Guns, "Mosin 91/30")
        );
        assert_eq!(rows[1].items, vec![0, 2]);
    }
}
