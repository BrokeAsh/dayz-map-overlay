//! The overlay's contents: the map view with points of interest, the toolbar, and the maps list.
//!
//! The map follows the game: when DayZ joins a server, the library imports that terrain if
//! needed and the overlay switches to it. The maps list is only for browsing other maps.

mod tiles;

use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Id, Margin, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Vec2,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::game::Session;
use crate::import::poi::{Kind, Pois};
use crate::library::Library;
use crate::maps::{self, LayerMeta, MapPack};
use tiles::TileCache;

/// Most zoomed-in view, in screen points per metre.
const MAX_ZOOM: f64 = 8.0;
const TOOLBAR_SPACE: f32 = 64.0;

#[derive(Debug, Clone, Copy)]
struct View {
    /// World position (x east, z north) at the centre of the screen.
    center: [f64; 2],
    /// Screen points per metre.
    zoom: f64,
}

impl View {
    fn fit(world: f64, screen: Rect) -> Self {
        let side = f64::from(screen.width().min(screen.height() - TOOLBAR_SPACE * 2.0)).max(100.0);
        Self {
            center: [world / 2.0, world / 2.0],
            zoom: side / world,
        }
    }

    fn to_screen(self, screen: Rect, x: f64, z: f64) -> Pos2 {
        let c = screen.center();
        Pos2::new(
            (f64::from(c.x) + (x - self.center[0]) * self.zoom) as f32,
            (f64::from(c.y) - (z - self.center[1]) * self.zoom) as f32,
        )
    }

    fn to_world(self, screen: Rect, p: Pos2) -> [f64; 2] {
        let c = screen.center();
        [
            self.center[0] + f64::from(p.x - c.x) / self.zoom,
            self.center[1] - f64::from(p.y - c.y) / self.zoom,
        ]
    }

    /// Zooms by `factor`, keeping the world point under `anchor` in place.
    fn zoom_around(&mut self, screen: Rect, anchor: Pos2, factor: f64, min_zoom: f64) {
        let before = self.to_world(screen, anchor);
        // (A tiny map can't zoom out past MAX_ZOOM; `clamp` panics if min > max.)
        self.zoom = (self.zoom * factor).clamp(min_zoom.min(MAX_ZOOM), MAX_ZOOM);
        let after = self.to_world(screen, anchor);
        self.center[0] += before[0] - after[0];
        self.center[1] += before[1] - after[1];
    }
}

struct LoadedPois {
    pois: Pois,
    counts: HashMap<Kind, usize>,
}

pub struct OverlayApp {
    config: Config,
    maps: Vec<MapPack>,
    current: Option<usize>,
    views: HashMap<String, View>,
    tiles: TileCache,
    pois: HashMap<String, Arc<LoadedPois>>,
    library: Library,
    seen_generation: u64,
    /// Each map.toml's time and size when the maps were last read, to notice imports made by
    /// another program.
    maps_read: Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)>,
    /// When the overlay last looked for those, while open.
    maps_checked: Instant,
    session: Session,
    show_maps_window: bool,
    close_requested: bool,
    /// The folder the user is picking for the game, delivered by the dialog's thread.
    picked_game_dir: Option<crossbeam_channel::Receiver<Option<PathBuf>>>,
    /// Feedback for the Maps window, such as a picked folder that isn't DayZ.
    notice: Option<String>,
}

impl OverlayApp {
    pub fn new(ctx: &egui::Context, config: Config) -> Self {
        let mut style = (*ctx.global_style()).clone();
        style.visuals = egui::Visuals::dark();
        ctx.set_global_style(style);
        let library = Library::new(ctx.clone());
        let mut app = Self {
            config,
            maps: Vec::new(),
            current: None,
            views: HashMap::new(),
            tiles: TileCache::new(ctx),
            pois: HashMap::new(),
            library,
            seen_generation: 0,
            maps_read: Vec::new(),
            maps_checked: Instant::now(),
            session: Session::default(),
            show_maps_window: false,
            close_requested: false,
            picked_game_dir: None,
            notice: None,
        };
        app.reload_maps();
        app
    }

    fn reload_maps(&mut self) {
        let selected =
            self.current
                .map(|i| self.maps[i].meta.id.clone())
                .or(self.config.view.map.clone());
        // Before reading them: a file saved in between then counts as changed next time.
        self.maps_read = map_files();
        self.maps = maps::load_all();
        self.current = selected
            .and_then(|id| self.maps.iter().position(|m| m.meta.id == id))
            .or((!self.maps.is_empty()).then_some(0));
    }

    fn select(&mut self, id: &str) {
        // Maybe installed since the list was read (by `dayz-map import-image`, say).
        if !self.maps.iter().any(|m| m.meta.id == id) {
            self.reload_maps();
        }
        if let Some(i) = self.maps.iter().position(|m| m.meta.id == id) {
            self.current = Some(i);
            self.config.view.map = Some(id.to_string());
        }
    }

    /// Called when the game's state changes, whether or not the overlay is open.
    pub fn on_session(&mut self, session: Session) {
        // The mods can arrive after the map (the RPT is written later), and they decide which
        // copy of a map the server uses.
        if let Some(world) = &session.world
            && (session.world != self.session.world || session.mods != self.session.mods)
        {
            self.library.ensure(world, &session.mods);
        }
        if session.world.is_none() && self.session.world.is_some() {
            self.library.forget();
        }
        self.session = session;
    }

    /// Applies finished library work: reloads changed maps and switches to a newly ready one.
    fn sync_library(&mut self) {
        let (generation, ready) = {
            let mut state = self.library.state.lock().unwrap();
            (state.generation, state.ready.take())
        };
        if generation != self.seen_generation {
            self.seen_generation = generation;
            self.tiles.clear();
            self.pois.clear();
            self.reload_maps();
        }
        if let Some(id) = ready {
            self.select(&id);
        }
    }

    pub fn on_show(&mut self) {
        self.reload_if_changed();
        self.sync_library();
        self.close_requested = false;
    }

    /// Rereads the maps if another program (`dayz-map import`) rebuilt one, whose old tiles are
    /// gone.
    fn reload_if_changed(&mut self) {
        self.maps_checked = Instant::now();
        if self.maps_changed() {
            self.tiles.clear();
            self.pois.clear();
            self.reload_maps();
        }
    }

    /// Whether any map's `map.toml` was saved, added or removed since the maps were read.
    fn maps_changed(&self) -> bool {
        map_files() != self.maps_read
    }

    pub fn on_hide(&mut self) {
        // Free GPU memory for the game; the overview levels reload instantly next time.
        self.tiles.trim(2);
        if let Err(e) = self.config.save_from_overlay(false) {
            log::warn!("saving settings: {e:#}");
        }
    }

    /// True once the user has asked to close the overlay.
    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        const CHECK_EVERY: Duration = Duration::from_secs(2);
        if self.maps_checked.elapsed() >= CHECK_EVERY {
            self.reload_if_changed();
        }
        // Looked for again then, even if nothing else happens. (A little later: egui wakes a
        // frame early.)
        let next = CHECK_EVERY.saturating_sub(self.maps_checked.elapsed());
        ui.ctx()
            .request_repaint_after(next + Duration::from_millis(50));
        let ctx = ui.ctx().clone();
        let screen = ui.max_rect();
        self.tiles.begin_frame(&ctx);
        self.sync_library();
        self.take_picked_game_dir();

        let dim = (self.config.view.backdrop_opacity.clamp(0.0, 1.0) * 255.0) as u8;
        ui.painter()
            .rect_filled(screen, 0.0, Color32::from_black_alpha(dim));

        if let Some(index) = self.current {
            self.map_view(ui, index, screen);
        }
        self.toolbar(&ctx);
        let game_missing = crate::paths::current().game.is_none();
        if self.show_maps_window || self.maps.is_empty() || game_missing {
            self.maps_window(&ctx);
        }
        self.tiles.end_frame();
    }

    /// Opens the system folder picker. The overlay closes meanwhile (it would cover the dialog)
    /// and reopens with the result.
    fn pick_game_dir(&mut self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.picked_game_dir = Some(rx);
        self.close_requested = true;
        let start = crate::steam::library_folders().into_iter().next();
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Choose your DayZ folder");
            if let Some(dir) = start {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            let _ = crate::ipc::send(crate::ipc::Command::Show);
        });
    }

    fn take_picked_game_dir(&mut self) {
        let Some(picked) = self
            .picked_game_dir
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        self.picked_game_dir = None;
        let Some(dir) = picked else {
            return;
        };
        // Accept the library or `common` folder too.
        let found = [
            dir.clone(),
            dir.join("DayZ"),
            dir.join("common/DayZ"),
            dir.join("steamapps/common/DayZ"),
        ]
        .into_iter()
        .find(|d| crate::paths::is_game_dir(d));
        match found {
            Some(game) => {
                log::info!("using the DayZ folder {}", game.display());
                self.notice = None;
                self.config.game_dir = Some(game);
                if let Err(e) = self.config.save_from_overlay(true) {
                    log::warn!("saving settings: {e:#}");
                }
                self.library.relocate(&self.config);
            }
            None => {
                self.notice = Some(format!(
                    "{} isn't a DayZ folder (it should contain DayZ_x64.exe and Addons).",
                    dir.display()
                ));
            }
        }
    }

    fn current_pois(&mut self) -> Option<Arc<LoadedPois>> {
        let pack = &self.maps[self.current?];
        let loaded = self.pois.entry(pack.meta.id.clone()).or_insert_with(|| {
            let pois = pack.pois();
            let mut counts = HashMap::new();
            for m in &pois.markers {
                *counts.entry(m.kind).or_default() += 1;
            }
            *counts.entry(Kind::ToxicZone).or_default() += pois.zones.len();
            Arc::new(LoadedPois { pois, counts })
        });
        Some(loaded.clone())
    }

    fn layer_shown(&self, kind: Kind) -> bool {
        self.config
            .view
            .layers
            .get(&kind.id())
            .copied()
            .unwrap_or(kind.shown_by_default())
    }

    fn map_view(&mut self, ui: &mut egui::Ui, index: usize, screen: Rect) {
        let ctx = ui.ctx().clone();
        let pack = &self.maps[index];
        let world = pack.meta.world_size;
        let Some(layer) = pack.meta.layers.first().cloned() else {
            return;
        };
        let fit = View::fit(world, screen);
        let view = self.views.entry(pack.meta.id.clone()).or_insert(fit);
        let min_zoom = fit.zoom * 0.5;

        // Panning and zooming.
        let response = ui.interact(screen, Id::new("map"), Sense::click_and_drag());
        if response.dragged() {
            let d = response.drag_delta();
            view.center[0] -= f64::from(d.x) / view.zoom;
            view.center[1] += f64::from(d.y) / view.zoom;
        }
        if response.double_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            view.zoom_around(screen, p, 2.0, min_zoom);
        }
        if let Some(p) = response.hover_pos() {
            let (scroll, pinch) = ctx.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = f64::from(pinch) * (f64::from(scroll) * 0.003).exp();
            if factor != 1.0 {
                view.zoom_around(screen, p, factor, min_zoom);
            }
        }
        view.center = view.center.map(|c| c.clamp(0.0, world));
        let view = *view;

        let map_rect = Rect::from_two_pos(
            view.to_screen(screen, 0.0, world),
            view.to_screen(screen, world, 0.0),
        );
        // The satellite grid can reach past the terrain edge; only draw the terrain.
        let map_painter = ui.painter_at(map_rect.intersect(screen));
        self.paint_tiles(&map_painter, &ctx, index, &layer, &view, screen);
        let painter = ui.painter_at(screen);
        painter.rect_stroke(
            map_rect,
            0.0,
            Stroke::new(1.0, Color32::from_white_alpha(70)),
            StrokeKind::Outside,
        );
        if self.config.view.show_grid {
            paint_grid(&painter, &view, screen, world);
        }

        let hover = response.hover_pos();
        let mut hovered: Option<(Pos2, String)> = None;
        if let Some(loaded) = self.current_pois() {
            hovered = self.paint_pois(&map_painter, &loaded.pois, &view, screen, hover);
        }

        if let Some(p) = hover {
            if let Some((at, text)) = hovered {
                label_box(
                    &painter,
                    at + Vec2::new(12.0, -12.0),
                    &text,
                    Align2::LEFT_BOTTOM,
                );
            }
            let [x, z] = view.to_world(screen, p);
            if (0.0..=world).contains(&x) && (0.0..=world).contains(&z) {
                let text = format!("X {x:.0}   Z {z:.0}");
                label_box(
                    &painter,
                    screen.left_bottom() + Vec2::new(16.0, -16.0),
                    &text,
                    Align2::LEFT_BOTTOM,
                );
            }
        }
    }

    fn paint_tiles(
        &mut self,
        painter: &egui::Painter,
        ctx: &egui::Context,
        index: usize,
        layer: &LayerMeta,
        view: &View,
        screen: Rect,
    ) {
        let pack = &self.maps[index];
        // Pick the level whose resolution best matches the screen.
        let full_px_per_m = f64::from(layer.tile_px) / layer.tile_m;
        let screen_px_per_m = view.zoom * f64::from(ctx.pixels_per_point());
        let down = (full_px_per_m / screen_px_per_m).log2().round().max(0.0) as u32;
        let level = layer.max_level.saturating_sub(down);
        let tile_m = layer.tile_metres(level);
        let n = layer.grid_at(level);
        let [left, top] = layer.origin;

        let [min_x, max_z] = view.to_world(screen, screen.left_top());
        let [max_x, min_z] = view.to_world(screen, screen.right_bottom());
        let span = tile_m * f64::from(n);
        if max_x < left || min_x > left + span || max_z < top - span || min_z > top {
            return;
        }
        let index_of = |m: f64| (m / tile_m).floor().clamp(0.0, f64::from(n - 1)) as u32;
        let (x0, x1) = (index_of(min_x - left), index_of(max_x - left));
        let (y0, y1) = (index_of(top - max_z), index_of(top - min_z));

        let tint = Color32::WHITE.gamma_multiply(self.config.view.map_opacity.clamp(0.0, 1.0));
        let full_uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        // Keep the whole-map tile around as a fallback while sharper tiles load.
        let _ = self.tiles.get(&pack.tile_path(layer, 0, 0, 0));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (tile_left, tile_top) =
                    (left + f64::from(x) * tile_m, top - f64::from(y) * tile_m);
                let rect = Rect::from_min_max(
                    view.to_screen(screen, tile_left, tile_top),
                    view.to_screen(screen, tile_left + tile_m, tile_top - tile_m),
                );
                match self.tiles.get(&pack.tile_path(layer, level, x, y)) {
                    Some(Some(texture)) => {
                        painter.image(texture, rect, full_uv, tint);
                    }
                    Some(None) => {}
                    None => {
                        // Stretch the closest loaded ancestor over this tile meanwhile.
                        for up in 1..=level {
                            let n = 1u32 << up;
                            let path = pack.tile_path(layer, level - up, x >> up, y >> up);
                            if let Some(texture) = self.tiles.peek(&path) {
                                let size = 1.0 / n as f32;
                                let min = Pos2::new((x % n) as f32 * size, (y % n) as f32 * size);
                                let uv = Rect::from_min_size(min, Vec2::splat(size));
                                painter.image(texture, rect, uv, tint);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Draws zones, markers, and place names; returns the marker under the pointer, if any.
    fn paint_pois(
        &self,
        painter: &egui::Painter,
        pois: &Pois,
        view: &View,
        screen: Rect,
        hover: Option<Pos2>,
    ) -> Option<(Pos2, String)> {
        let visible = screen.expand(20.0);
        let mut nearest: Option<(f32, Pos2, String)> = None;
        let mut consider = |p: Pos2, text: &dyn Fn() -> String, reach: f32| {
            if let Some(h) = hover {
                let d = p.distance(h);
                if d <= reach && nearest.as_ref().is_none_or(|(best, _, _)| d < *best) {
                    nearest = Some((d, p, text()));
                }
            }
        };

        if self.layer_shown(Kind::ToxicZone) {
            let color = kind_color(Kind::ToxicZone);
            for zone in &pois.zones {
                let center = view.to_screen(screen, f64::from(zone.x), f64::from(zone.z));
                let radius = (f64::from(zone.radius) * view.zoom) as f32;
                if !visible.expand(radius).contains(center) {
                    continue;
                }
                painter.circle(
                    center,
                    radius.max(3.0),
                    color.gamma_multiply(0.25),
                    Stroke::new(1.5, color),
                );
                consider(
                    center,
                    &|| format!("{} (contaminated zone)", zone.label),
                    radius.max(8.0),
                );
            }
        }

        let size = if view.zoom < 0.12 {
            3.0
        } else if view.zoom < 0.5 {
            4.0
        } else {
            5.5
        };
        // Water last, so it stays on top of the busier layers.
        let mut kinds = Kind::ALL;
        kinds.sort_by_key(|&k| water_icon(k).is_some());
        for kind in kinds {
            if !self.layer_shown(kind) {
                continue;
            }
            let color = kind_color(kind);
            let outline = Stroke::new(1.0, Color32::from_black_alpha(200));
            for marker in pois.markers.iter().filter(|m| m.kind == kind) {
                let p = view.to_screen(screen, f64::from(marker.x), f64::from(marker.z));
                if !visible.contains(p) {
                    continue;
                }
                if let Some(icon) = water_icon(kind) {
                    water_badge(painter, p, size * 1.7, icon);
                } else if is_event(kind) {
                    // Event spawns are diamonds; buildings are dots.
                    painter.add(diamond(p, size + 1.0, color, outline));
                } else {
                    painter.circle(p, size, color, outline);
                }
                consider(p, &|| format!("{} · {}", marker.label, kind.label()), 9.0);
            }
        }

        if self.config.view.show_places {
            let max_rank = match view.zoom {
                z if z >= 0.5 => 3,
                z if z >= 0.15 => 2,
                z if z >= 0.06 => 1,
                _ => 0,
            };
            for place in pois.places.iter().filter(|p| p.rank() <= max_rank) {
                let p = view.to_screen(screen, f64::from(place.x), f64::from(place.z));
                if !visible.contains(p) {
                    continue;
                }
                let (size, color) = match place.rank() {
                    0 => (17.0, Color32::WHITE),
                    1 => (14.5, Color32::WHITE),
                    2 => (12.5, Color32::from_gray(235)),
                    _ => (11.0, Color32::from_rgb(220, 215, 190)),
                };
                outlined_text(painter, p, &place.name, size, color);
            }
        }
        nearest.map(|(_, p, text)| (p, text))
    }

    fn status_text(&self) -> String {
        let s = &self.session;
        if !s.running {
            return "DayZ not running".into();
        }
        match (&s.world, &s.server_name, &s.server) {
            (None, _, _) => "Main menu".into(),
            (Some(_), Some(name), _) => truncate(name, 48),
            (Some(_), None, Some(address)) => address.clone(),
            (Some(_), None, None) => "In game".into(),
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let (job, message, scanning) = {
            let s = self.library.state.lock().unwrap();
            (s.job.clone(), s.message.clone(), s.scanning)
        };
        let pois = self.current_pois();
        egui::Area::new(Id::new("toolbar"))
            .anchor(Align2::CENTER_TOP, [0.0, 12.0])
            .show(ctx, |ui| {
                panel_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let name = self
                            .current
                            .map_or("No map".to_string(), |i| self.maps[i].meta.name.clone());
                        ui.label(egui::RichText::new(name).strong().size(15.0));
                        match &job {
                            Some(job) => {
                                ui.spinner();
                                let percent = job.done * 100 / job.total.max(1);
                                ui.label(format!("Importing {} {percent}%", job.name));
                            }
                            None if scanning => {
                                ui.spinner();
                                ui.label("Looking for maps…");
                            }
                            None => {
                                ui.label(egui::RichText::new(self.status_text()).weak());
                            }
                        }
                        ui.separator();
                        ui.menu_button("Layers", |ui| self.layers_menu(ui, pois.as_deref()));
                        ui.label("Opacity");
                        ui.add(
                            egui::Slider::new(&mut self.config.view.map_opacity, 0.1..=1.0)
                                .show_value(false),
                        );
                        ui.label("Dim");
                        ui.add(
                            egui::Slider::new(&mut self.config.view.backdrop_opacity, 0.0..=0.9)
                                .show_value(false),
                        );
                        ui.checkbox(&mut self.config.view.show_grid, "Grid");
                        ui.separator();
                        if ui.button("Fit").clicked()
                            && let Some(i) = self.current
                        {
                            self.views.remove(&self.maps[i].meta.id);
                        }
                        if ui
                            .selectable_label(self.show_maps_window, "Maps…")
                            .clicked()
                        {
                            self.show_maps_window = !self.show_maps_window;
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} to close",
                                self.config.hotkey.to_uppercase()
                            ))
                            .weak(),
                        );
                        if ui.button("×").on_hover_text("Close").clicked() {
                            self.close_requested = true;
                        }
                    });
                    if let Some(message) = &message {
                        ui.colored_label(Color32::from_rgb(255, 170, 120), message);
                    }
                });
            });
    }

    fn layers_menu(&mut self, ui: &mut egui::Ui, pois: Option<&LoadedPois>) {
        ui.set_min_width(230.0);
        let places = pois.map_or(0, |p| p.pois.places.len());
        ui.checkbox(
            &mut self.config.view.show_places,
            format!("Place names ({places})"),
        );
        ui.separator();
        let mut any = false;
        for kind in Kind::ALL {
            let count = pois.and_then(|p| p.counts.get(&kind)).copied().unwrap_or(0);
            if count == 0 {
                continue;
            }
            any = true;
            let mut shown = self.layer_shown(kind);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                if let Some(icon) = water_icon(kind) {
                    water_badge(ui.painter(), rect.center(), 7.5, icon);
                } else if is_event(kind) {
                    ui.painter()
                        .add(diamond(rect.center(), 5.0, kind_color(kind), Stroke::NONE));
                } else {
                    ui.painter()
                        .circle_filled(rect.center(), 5.0, kind_color(kind));
                }
                if ui
                    .checkbox(&mut shown, format!("{} ({count})", kind.label()))
                    .changed()
                {
                    self.config.view.layers.insert(kind.id(), shown);
                }
            });
        }
        if !any {
            ui.label(egui::RichText::new("This map has no point-of-interest data.").weak());
        }
        ui.separator();
        ui.label(
            egui::RichText::new(
                "From the map's default files; servers can change loot and events.",
            )
            .weak()
            .small(),
        );
    }

    fn maps_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut window = egui::Window::new("Maps")
            .collapsible(false)
            .resizable(false)
            .frame(panel_frame())
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0]);
        if !self.maps.is_empty() {
            window = window.open(&mut open);
        }
        let (catalog, job, scanning) = {
            let s = self.library.state.lock().unwrap();
            (s.catalog.clone(), s.job.clone(), s.scanning)
        };
        let mut view = None;
        let mut import = None;
        window.show(ctx, |ui| {
            ui.set_min_width(420.0);
            ui.label("The map switches automatically when you join a server. You can also look at other maps here.");
            ui.add_space(6.0);
            if crate::paths::current().game.is_none() {
                ui.colored_label(
                    Color32::from_rgb(255, 170, 120),
                    "DayZ wasn't found in your Steam libraries.",
                );
                if ui
                    .add_enabled(self.picked_game_dir.is_none(), egui::Button::new("Choose the DayZ folder…"))
                    .clicked()
                {
                    self.pick_game_dir();
                }
                ui.add_space(6.0);
            }
            if let Some(notice) = &self.notice {
                ui.colored_label(Color32::from_rgb(255, 170, 120), notice);
                ui.add_space(6.0);
            }
            let Some(catalog) = &catalog else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking for maps in the game and Workshop folders…");
                });
                return;
            };
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                for world in catalog.unique() {
                    ui.horizontal(|ui| {
                        ui.label(&world.name);
                        ui.label(egui::RichText::new(world.source_label()).weak().small());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let installed = self.maps.iter().any(|m| m.meta.id == world.id);
                            match &job {
                                Some(j) if j.id == world.id => {
                                    let fraction = j.done as f32 / j.total.max(1) as f32;
                                    ui.add(egui::ProgressBar::new(fraction).desired_width(100.0));
                                }
                                _ if installed => {
                                    if ui.button("View").clicked() {
                                        view = Some(world.id.clone());
                                    }
                                }
                                _ => {
                                    if ui.add_enabled(job.is_none(), egui::Button::new("Import")).clicked() {
                                        import = Some(world.id.clone());
                                    }
                                }
                            }
                        });
                    });
                }
                // Maps with no files in the game or Workshop folders, such as imported pictures.
                let unique = catalog.unique();
                for map in self.maps.iter().filter(|m| !unique.iter().any(|w| w.id == m.meta.id)) {
                    ui.horizontal(|ui| {
                        ui.label(&map.meta.name);
                        let label = if map.meta.format == 0 { "picture" } else { "installed" };
                        ui.label(egui::RichText::new(label).weak().small());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("View").clicked() {
                                view = Some(map.meta.id.clone());
                            }
                        });
                    });
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!scanning, egui::Button::new("Rescan")).clicked() {
                    self.library.rescan();
                }
                ui.label(egui::RichText::new(format!("{} maps found", catalog.unique().len())).weak());
            });
        });
        if !open {
            self.show_maps_window = false;
        }
        if let Some(id) = view {
            self.select(&id);
        }
        if let Some(id) = import {
            self.library.import(&id);
        }
    }
}

fn is_event(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::HeliCrash
            | Kind::PoliceCar
            | Kind::Convoy
            | Kind::Train
            | Kind::Vehicle
            | Kind::Boat
            | Kind::ToxicZone
    )
}

fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Water | Kind::FreshWater => WATER_BLUE,
        Kind::Fuel => Color32::from_rgb(255, 150, 30),
        Kind::Military => Color32::from_rgb(230, 50, 50),
        Kind::Police => Color32::from_rgb(255, 115, 45),
        Kind::Medical => Color32::from_rgb(255, 120, 190),
        Kind::Firefighter => Color32::from_rgb(200, 70, 20),
        Kind::Hunting => Color32::from_rgb(90, 200, 90),
        Kind::Industrial => Color32::from_rgb(220, 200, 60),
        Kind::Civilian => Color32::from_rgb(180, 180, 180),
        Kind::HeliCrash => Color32::from_rgb(230, 60, 230),
        Kind::PoliceCar => Color32::from_rgb(150, 200, 255),
        Kind::Convoy => Color32::from_rgb(160, 30, 30),
        Kind::Train => Color32::from_rgb(170, 120, 80),
        Kind::Vehicle => Color32::from_rgb(60, 220, 220),
        Kind::Boat => Color32::from_rgb(40, 160, 150),
        Kind::ToxicZone => Color32::from_rgb(170, 255, 40),
    }
}

const WATER_BLUE: Color32 = Color32::from_rgb(30, 120, 235);

#[derive(Clone, Copy)]
enum WaterIcon {
    Spigot,
    Wave,
}

fn water_icon(kind: Kind) -> Option<WaterIcon> {
    match kind {
        Kind::Water => Some(WaterIcon::Spigot),
        Kind::FreshWater => Some(WaterIcon::Wave),
        _ => None,
    }
}

/// A blue disc with a white tap (wells and pumps) or waves (fresh water). Below about 6 px the
/// icon wouldn't be legible, so only the disc is drawn.
fn water_badge(painter: &egui::Painter, center: Pos2, radius: f32, icon: WaterIcon) {
    painter.circle(
        center,
        radius,
        WATER_BLUE,
        Stroke::new(1.0, Color32::from_black_alpha(200)),
    );
    if radius < 6.0 {
        return;
    }
    let at = |x: f32, y: f32| center + Vec2::new(x, y) * radius;
    let white = Color32::WHITE;
    match icon {
        WaterIcon::Wave => {
            let stroke = Stroke::new((radius * 0.17).max(1.2), white);
            for row in [-0.24, 0.24] {
                let points = (0..=12)
                    .map(|i| {
                        let t = i as f32 / 12.0;
                        let y = row + 0.14 * (t * std::f32::consts::TAU * 1.5).sin();
                        at(-0.6 + 1.2 * t, y)
                    })
                    .collect();
                painter.add(Shape::line(points, stroke));
            }
        }
        WaterIcon::Spigot => {
            let fill = |x0: f32, y0: f32, x1: f32, y1: f32| {
                painter.rect_filled(Rect::from_two_pos(at(x0, y0), at(x1, y1)), 0.0, white);
            };
            fill(-0.2, -0.62, 0.3, -0.5); // handle
            fill(0.0, -0.55, 0.1, -0.3); // stem
            fill(-0.62, -0.32, 0.36, -0.06); // pipe
            fill(0.12, -0.1, 0.36, 0.18); // spout
            painter.circle_filled(at(0.24, 0.45), radius * 0.13, white); // drop
        }
    }
}

fn diamond(center: Pos2, size: f32, fill: Color32, stroke: Stroke) -> Shape {
    let points = vec![
        center + Vec2::new(0.0, -size),
        center + Vec2::new(size, 0.0),
        center + Vec2::new(0.0, size),
        center + Vec2::new(-size, 0.0),
    ];
    Shape::convex_polygon(points, fill, stroke)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{}…", cut.trim_end())
}

fn label_box(painter: &egui::Painter, pos: Pos2, text: &str, align: Align2) {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::monospace(13.0), Color32::WHITE);
    let rect = align.anchor_size(pos, galley.size());
    painter.rect_filled(rect.expand(6.0), 4.0, Color32::from_black_alpha(200));
    painter.galley(rect.min, galley, Color32::WHITE);
}

fn outlined_text(painter: &egui::Painter, pos: Pos2, text: &str, size: f32, color: Color32) {
    let font = FontId::proportional(size);
    let shadow = Color32::from_black_alpha(220);
    for offset in [
        Vec2::new(1.0, 1.0),
        Vec2::new(-1.0, 1.0),
        Vec2::new(1.0, -1.0),
        Vec2::new(-1.0, -1.0),
    ] {
        painter.text(
            pos + offset,
            Align2::CENTER_CENTER,
            text,
            font.clone(),
            shadow,
        );
    }
    painter.text(pos, Align2::CENTER_CENTER, text, font, color);
}

fn panel_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(18, 20, 24, 230))
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(30)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
}

/// 1 km grid with square numbers along the top and left edges; 100 m lines when zoomed in.
fn paint_grid(painter: &egui::Painter, view: &View, screen: Rect, world: f64) {
    let km = (world / 1000.0).ceil() as i32;
    let fine = 100.0 * view.zoom >= 24.0;
    let map = Rect::from_two_pos(
        view.to_screen(screen, 0.0, world),
        view.to_screen(screen, world, 0.0),
    );
    let visible = map.intersect(screen);
    if !visible.is_positive() {
        return;
    }
    let step = if fine { 100.0 } else { 1000.0 };
    let count = (world / step).ceil() as i32;
    for i in 0..=count {
        let m = (f64::from(i) * step).min(world);
        let major = i % if fine { 10 } else { 1 } == 0;
        let stroke = Stroke::new(1.0, Color32::from_white_alpha(if major { 60 } else { 22 }));
        let x = view.to_screen(screen, m, 0.0).x;
        if (visible.left()..=visible.right()).contains(&x) {
            painter.vline(x, visible.y_range(), stroke);
        }
        // Rows run from the north edge, like the square numbers (the last row may be short).
        let y = view.to_screen(screen, 0.0, world - m).y;
        if (visible.top()..=visible.bottom()).contains(&y) {
            painter.hline(visible.x_range(), y, stroke);
        }
    }
    // Square numbers count from the north-west corner, like most DayZ map sites.
    let font = FontId::proportional(12.0);
    let color = Color32::from_white_alpha(170);
    let label_top = visible.top().max(screen.top() + TOOLBAR_SPACE);
    let label_left = visible.left() + 4.0;
    for k in 0..km {
        let centre = f64::from(k) * 1000.0 + 500.0;
        let x = view.to_screen(screen, centre, 0.0).x;
        if (visible.left() + 12.0..=visible.right() - 12.0).contains(&x) {
            painter.text(
                Pos2::new(x, label_top + 4.0),
                Align2::CENTER_TOP,
                format!("{k:02}"),
                font.clone(),
                color,
            );
        }
        let y = view.to_screen(screen, 0.0, world - centre).y;
        if (label_top + 24.0..=visible.bottom() - 8.0).contains(&y) {
            painter.text(
                Pos2::new(label_left, y),
                Align2::LEFT_CENTER,
                format!("{k:02}"),
                font.clone(),
                color,
            );
        }
    }
}

/// Every map.toml, with its modification time and size (`None` if it can't be read).
fn map_files() -> Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)> {
    let mut files: Vec<_> = std::fs::read_dir(maps::maps_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path().join("map.toml");
            let stamp = std::fs::metadata(&path)
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())));
            (path, stamp)
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}
