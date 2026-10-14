//! Your own markers on the map: drawing them, adding and editing them from the right-click
//! menu, and dragging them. Everything is done with the mouse, since the game keeps the keyboard.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

use super::{OverlayApp, View, diamond, is_event, kind_color, water_badge, water_icon};
use crate::import::poi::Kind;
use crate::markers::{MarkerKind, NAMES, Own};

/// How close (in points) the pointer must be to a marker to pick it.
const REACH: f32 = 12.0;
/// A marker's radius on screen.
const RADIUS: f32 = 10.0;
/// The ring around markers saved for the server you're on; the map's have a white one.
const SERVER_RING: Color32 = Color32::from_rgb(255, 200, 60);

/// What the right-click menu is about.
#[derive(Clone, Copy)]
pub(super) enum MenuTarget {
    /// A spot on the map (world x, z), to add a marker at.
    Spot(f32, f32),
    Marker(u64),
}

impl OverlayApp {
    /// The server new markers can be saved for: the one you're on, while its map is shown.
    fn marker_server(&self, map: &str) -> Option<String> {
        self.session
            .server
            .clone()
            .filter(|_| self.session.world.as_deref() == Some(map))
    }

    /// Your markers on `map` that are switched on, with where they are on screen (a marker being
    /// dragged is where it's being dragged to).
    pub(super) fn user_markers_on_screen(
        &self,
        map: &str,
        view: &View,
        screen: Rect,
    ) -> Vec<(u64, Pos2)> {
        let server = self.marker_server(map);
        let view_cfg = &self.config.view;
        self.markers
            .shown(map, server.as_deref())
            .filter(|m| {
                if m.server.is_some() {
                    view_cfg.show_server_markers
                } else {
                    view_cfg.show_map_markers
                }
            })
            .map(|m| {
                let (x, z) = match self.dragging {
                    Some((id, x, z)) if id == m.id => (x, z),
                    _ => (m.x, m.z),
                };
                (m.id, view.to_screen(screen, f64::from(x), f64::from(z)))
            })
            .collect()
    }

    /// The marker under `p`, if any (the nearest).
    pub(super) fn user_marker_at(on_screen: &[(u64, Pos2)], p: Pos2) -> Option<u64> {
        on_screen
            .iter()
            .map(|&(id, at)| (at.distance(p), id))
            .filter(|(d, _)| *d <= REACH)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }

    /// Draws your markers on top of everything; returns the hovered one's spot and label.
    pub(super) fn paint_user_markers(
        &self,
        painter: &egui::Painter,
        on_screen: &[(u64, Pos2)],
        hover: Option<Pos2>,
    ) -> Option<(Pos2, String)> {
        let hovered = hover.and_then(|p| Self::user_marker_at(on_screen, p));
        let mut text = None;
        for &(id, at) in on_screen {
            let Some(marker) = self.markers.list.iter().find(|m| m.id == id) else {
                continue;
            };
            let Some(kind) = marker.kind() else {
                continue;
            };
            let radius = if hovered == Some(id) {
                RADIUS + 2.0
            } else {
                RADIUS
            };
            paint_marker(painter, at, radius, kind, marker.server.is_some());
            if hovered == Some(id) {
                let scope = if marker.server.is_some() {
                    "this server"
                } else {
                    "every server on this map"
                };
                text = Some((at, format!("{} · {scope}", marker.label())));
            }
        }
        text
    }

    /// The right-click menu: add a marker at a spot, or change or delete one.
    pub(super) fn marker_menu(&mut self, ui: &mut egui::Ui, map: &str) {
        ui.set_min_width(200.0);
        match self.marker_menu {
            Some(MenuTarget::Spot(x, z)) => self.add_menu(ui, map, x, z),
            Some(MenuTarget::Marker(id)) => self.edit_menu(ui, map, id),
            None => {
                ui.close();
            }
        }
    }

    fn add_menu(&mut self, ui: &mut egui::Ui, map: &str, x: f32, z: f32) {
        ui.label(egui::RichText::new("Add a marker here").strong());
        let server = self.marker_server(map);
        let mut for_map = self.config.view.markers_for_map || server.is_none();
        if scope_switch(ui, &mut for_map, server.is_some()).changed() {
            self.config.view.markers_for_map = for_map;
        }
        ui.separator();
        let chosen = kind_choice(ui, None);
        if let Some(kind) = chosen {
            // Shown, even if this side was switched off in Layers.
            if for_map {
                self.config.view.show_map_markers = true;
            } else {
                self.config.view.show_server_markers = true;
            }
            let server = if for_map { None } else { server };
            let result = self.markers.add(map, server.as_deref(), kind, x, z);
            self.saved(result.map(|_| ()));
            ui.close();
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui, map: &str, id: u64) {
        let Some(marker) = self.markers.list.iter().find(|m| m.id == id).cloned() else {
            ui.close();
            return;
        };
        ui.label(egui::RichText::new(marker.label()).strong());
        // A marker saved for the server you're on can go to the whole map and back; one saved
        // for the map can only go to a server while you're on one with this map.
        let server = marker.server.clone().or_else(|| self.marker_server(map));
        let mut for_map = marker.server.is_none();
        if scope_switch(ui, &mut for_map, server.is_some()).changed() {
            let to = if for_map { None } else { server };
            let result = self.markers.edit(id, |m| m.server = to);
            self.saved(result);
        }
        ui.separator();
        ui.menu_button("Name", |ui| {
            let mut name = None;
            if ui
                .selectable_label(marker.name.is_none(), format!("Number ({})", marker.number))
                .clicked()
            {
                name = Some(None);
            }
            for preset in NAMES {
                if ui
                    .selectable_label(marker.name.as_deref() == Some(preset), preset)
                    .clicked()
                {
                    name = Some(Some(preset.to_string()));
                }
            }
            if let Some(name) = name {
                let result = self.markers.edit(id, |m| m.name = name);
                self.saved(result);
                ui.close();
            }
        });
        ui.menu_button("Change type", |ui| {
            if let Some(kind) = kind_choice(ui, marker.kind()) {
                let result = self.markers.edit(id, |m| m.kind = kind.id());
                self.saved(result);
                ui.close();
            }
        });
        ui.separator();
        if ui.button("Delete").clicked() {
            let result = self.markers.remove(id);
            self.saved(result);
            ui.close();
        }
    }

    /// Notes a failed save, to show in the toolbar (the change stays until the overlay quits).
    pub(super) fn saved(&mut self, result: anyhow::Result<()>) {
        match result {
            Ok(()) => self.marker_error = None,
            Err(e) => {
                log::warn!("saving markers: {e:#}");
                self.marker_error = Some(format!("Couldn't save your markers: {e:#}"));
            }
        }
    }

    /// The Layers menu's part: show or hide your markers.
    pub(super) fn markers_layers(&mut self, ui: &mut egui::Ui, map: Option<&str>) {
        ui.label(egui::RichText::new("Your markers").strong());
        let server = map.and_then(|m| self.marker_server(m));
        let (mut on_server, mut on_map) = (0, 0);
        if let Some(map) = map {
            for m in self.markers.shown(map, server.as_deref()) {
                if m.server.is_some() {
                    on_server += 1;
                } else {
                    on_map += 1;
                }
            }
        }
        let view = &mut self.config.view;
        if server.is_some() {
            ui.horizontal(|ui| {
                ring_swatch(ui, SERVER_RING);
                ui.checkbox(
                    &mut view.show_server_markers,
                    format!("For this server ({on_server})"),
                );
            });
        }
        ui.horizontal(|ui| {
            ring_swatch(ui, Color32::WHITE);
            ui.checkbox(
                &mut view.show_map_markers,
                format!("For this map ({on_map})"),
            );
        });
        ui.label(
            egui::RichText::new("Right-click the map to add one, or a marker to change it. Drag a marker to move it.")
                .weak()
                .small(),
        );
    }
}

/// Your kinds of marker, then the map's under Map features; returns the one clicked.
fn kind_choice(ui: &mut egui::Ui, current: Option<MarkerKind>) -> Option<MarkerKind> {
    let mut chosen = None;
    for own in Own::ALL {
        let kind = MarkerKind::Own(own);
        if kind_button(ui, kind, current == Some(kind)).clicked() {
            chosen = Some(kind);
        }
    }
    let is_map_kind = matches!(current, Some(MarkerKind::Map(_)));
    let features = egui::Button::new("Map features")
        .right_text(egui::containers::menu::SubMenuButton::RIGHT_ARROW)
        .selected(is_map_kind);
    egui::containers::menu::SubMenuButton::from_button(features).ui(ui, |ui| {
        for kind in Kind::ALL.map(MarkerKind::Map) {
            if kind_button(ui, kind, current == Some(kind)).clicked() {
                chosen = Some(kind);
            }
        }
    });
    chosen
}

/// Server on the left, the whole map on the right. Off to the right when there's no server.
fn scope_switch(ui: &mut egui::Ui, for_map: &mut bool, server: bool) -> egui::Response {
    ui.horizontal(|ui| {
        let weak = ui.visuals().weak_text_color();
        let strong = ui.visuals().strong_text_color();
        let color = |on: bool| if on { strong } else { weak };
        ui.label(egui::RichText::new("Server").color(color(!*for_map && server)));
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::new(34.0, 18.0), if server { Sense::click() } else { Sense::hover() });
        if response.clicked() {
            *for_map = !*for_map;
            response.mark_changed();
        }
        let t = ui.ctx().animate_bool_responsive(response.id, *for_map);
        let painter = ui.painter();
        let track = if *for_map { Color32::from_gray(150) } else { SERVER_RING };
        let track = if server { track } else { track.gamma_multiply(0.4) };
        painter.rect_filled(rect, 9.0, track.gamma_multiply(0.7));
        let knob = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), t);
        painter.circle(
            Pos2::new(knob, rect.center().y),
            7.0,
            Color32::WHITE,
            Stroke::new(1.0, Color32::from_black_alpha(120)),
        );
        ui.label(egui::RichText::new("Map").color(color(*for_map || !server)));
        if server {
            response.on_hover_text(
                "Left: only on this server. Right: on every server with this map.",
            )
        } else {
            response.on_hover_text(
                "Saved for the whole map. Markers for one server can be added while you're playing on it.",
            )
        }
    })
    .inner
}

/// A row with a kind's marker and its name, for the menus; `current` is highlighted.
fn kind_button(ui: &mut egui::Ui, kind: MarkerKind, current: bool) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        paint_marker(ui.painter(), rect.center(), 10.0, kind, false);
        ui.add(
            egui::Button::new(kind.label())
                .frame_when_inactive(current)
                .selected(current),
        )
    })
    .inner
}

fn ring_swatch(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
    ui.painter()
        .circle_stroke(rect.center(), 5.0, Stroke::new(2.0, color));
}

/// A marker: a disc in the kind's colour with its symbol, ringed gold if it's saved for the
/// server, white if for the map.
fn paint_marker(painter: &egui::Painter, at: Pos2, radius: f32, kind: MarkerKind, server: bool) {
    let ring = Stroke::new(2.0, if server { SERVER_RING } else { Color32::WHITE });
    match kind {
        MarkerKind::Own(own) => {
            painter.circle(at, radius, own_color(own), ring);
            // A dark edge between the disc and its ring, so the gold ring shows on any colour
            // (Danger's yellow is close to it).
            painter.circle_stroke(
                at,
                radius - 1.5,
                Stroke::new(1.0, Color32::from_black_alpha(170)),
            );
            let ink = if own == Own::Danger {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            painter.text(
                at,
                Align2::CENTER_CENTER,
                own_symbol(own),
                FontId::proportional(radius * 1.25),
                ink,
            );
        }
        MarkerKind::Map(kind) => {
            if let Some(icon) = water_icon(kind) {
                water_badge(painter, at, radius, icon);
                painter.circle_stroke(at, radius, ring);
            } else {
                painter.circle(at, radius, Color32::from_rgb(25, 25, 30), ring);
                let color = kind_color(kind);
                if is_event(kind) {
                    painter.add(diamond(at, radius * 0.6, color, Stroke::NONE));
                } else {
                    painter.circle_filled(at, radius * 0.5, color);
                }
            }
        }
    }
}

fn own_color(own: Own) -> Color32 {
    match own {
        Own::Home => Color32::from_rgb(60, 170, 90),
        Own::EnemyBase => Color32::from_rgb(210, 40, 40),
        Own::FriendlyBase => Color32::from_rgb(50, 120, 240),
        Own::Stash => Color32::from_rgb(170, 120, 50),
        Own::Vehicle => Color32::from_rgb(30, 150, 160),
        Own::Danger => Color32::from_rgb(245, 200, 40),
        Own::Meeting => Color32::from_rgb(150, 90, 240),
        Own::Death => Color32::from_rgb(90, 90, 100),
    }
}

/// Symbols egui's own fonts have.
fn own_symbol(own: Own) -> &'static str {
    match own {
        Own::Home => "🏠",
        Own::EnemyBase => "⚔",
        Own::FriendlyBase => "🛡",
        Own::Stash => "📦",
        Own::Vehicle => "🚗",
        Own::Danger => "⚠",
        Own::Meeting => "🚩",
        Own::Death => "💀",
    }
}
