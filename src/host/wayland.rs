//! Shows the overlay as a full-screen layer-shell surface on the `overlay` layer, which the
//! compositor draws above everything, fullscreen games included. While visible it takes the
//! keyboard exclusively; hiding destroys the surface so focus returns to the game.

use anyhow::{Context, Result};
use egui_wgpu::wgpu;
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, channel},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{Shm, ShmHandler},
};
use std::ptr::NonNull;
use std::time::{Duration, Instant};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
};

use super::HostEvent;
use crate::config::Config;
use crate::ipc::{self, Command};
use crate::ui::OverlayApp;

/// Ignore the hotkey this soon after closing, in case the game sees the closing key press.
const REOPEN_GRACE: Duration = Duration::from_millis(400);

pub fn run(config: Config, show: bool) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, event_queue) = registry_queue_init(&conn)?;
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<Host> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let (tx, rx) = channel::channel::<HostEvent>();
    event_loop
        .handle()
        .insert_source(rx, |event, _, host| {
            if let channel::Event::Msg(event) = event {
                host.handle(event);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let ipc_tx = tx.clone();
    ipc::listen(move |command| {
        let _ = ipc_tx.send(HostEvent::Command(command));
    })?;

    let hotkey_tx = tx.clone();
    let unfocus_tx = tx.clone();
    if let Err(e) = crate::trigger::spawn(
        &config,
        move |center| {
            let _ = hotkey_tx.send(HostEvent::Hotkey(center));
        },
        move || {
            let _ = unfocus_tx.send(HostEvent::GameUnfocused);
        },
    ) {
        log::warn!("hotkey disabled: {e:#}. Use `dayz-map toggle` instead.");
    }

    let hotkey = config.hotkey.to_uppercase();
    let session_tx = tx.clone();
    crate::game::spawn(move |session| {
        let _ = session_tx.send(HostEvent::Session(session));
    });

    let egui_ctx = egui::Context::default();
    let repaint_tx = std::sync::Mutex::new(tx);
    egui_ctx.set_request_repaint_callback(move |info| {
        let _ = repaint_tx
            .lock()
            .unwrap()
            .send(HostEvent::Repaint(info.delay));
    });

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor is not available")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("the compositor doesn't support layer-shell overlays (GNOME doesn't)")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm is not available")?;

    let mut host = Host {
        conn: conn.clone(),
        qh: qh.clone(),
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor,
        layer_shell,
        shm,
        keyboard: None,
        pointer: None,
        cursor: CursorIcon::Default,
        gpu: Gpu::new()?,
        overlay: None,
        app: OverlayApp::new(&egui_ctx, config),
        egui_ctx,
        events: Vec::new(),
        modifiers: egui::Modifiers::NONE,
        pointer_pos: None,
        keyboard_focus: false,
        start: Instant::now(),
        needs_redraw: false,
        next_repaint: None,
        last_hide: Instant::now() - REOPEN_GRACE,
        shown_at: Instant::now(),
        fade_in: own_fade(),
        game_center: None,
        exit: false,
    };
    if show {
        host.show();
    }
    log::info!("ready; press {hotkey} in DayZ to open the map");

    while !host.exit {
        let timeout = host
            .next_repaint
            .map(|t| t.saturating_duration_since(Instant::now()));
        event_loop.dispatch(timeout, &mut host)?;
        if host.next_repaint.is_some_and(|t| t <= Instant::now()) {
            host.next_repaint = None;
            host.needs_redraw = true;
        }
        if host.needs_redraw {
            host.render();
        }
    }
    host.hide();
    ipc::cleanup();
    Ok(())
}

struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Created with the first surface, once the output format is known.
    renderer: Option<(egui_wgpu::Renderer, wgpu::TextureFormat)>,
}

impl Gpu {
    fn new() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no Vulkan GPU found")?;
        log::info!("rendering with {}", adapter.get_info().name);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("dayz-map"),
                ..Default::default()
            }))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            renderer: None,
        })
    }
}

struct Overlay {
    layer: LayerSurface,
    surface: Option<wgpu::Surface<'static>>,
    /// Size in surface (logical) coordinates.
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    frame_pending: bool,
}

/// KWin fades on-screen displays in itself (see `show`); elsewhere the overlay fades itself in.
fn own_fade() -> Option<Duration> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let kde = desktop.split(':').any(|d| d.eq_ignore_ascii_case("KDE"));
    (!kde).then_some(Duration::from_millis(150))
}

struct Host {
    conn: Connection,
    qh: QueueHandle<Host>,
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
    cursor: CursorIcon,
    gpu: Gpu,
    overlay: Option<Overlay>,
    app: OverlayApp,
    egui_ctx: egui::Context,
    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    pointer_pos: Option<egui::Pos2>,
    keyboard_focus: bool,
    start: Instant,
    needs_redraw: bool,
    next_repaint: Option<Instant>,
    last_hide: Instant,
    shown_at: Instant,
    fade_in: Option<Duration>,
    /// Where the game window was last seen, to open the overlay on its monitor.
    game_center: Option<(i32, i32)>,
    exit: bool,
}

impl Host {
    fn handle(&mut self, event: HostEvent) {
        match event {
            HostEvent::Command(Command::Show) => self.show(),
            HostEvent::Command(Command::Hide) => self.hide(),
            HostEvent::Command(Command::Toggle) => {
                if self.overlay.is_some() {
                    self.hide()
                } else {
                    self.show()
                }
            }
            HostEvent::Command(Command::Quit) => self.exit = true,
            HostEvent::Hotkey(center) => {
                if center.is_some() {
                    self.game_center = center;
                }
                if self.overlay.is_some() {
                    self.hide();
                } else if self.last_hide.elapsed() >= REOPEN_GRACE {
                    self.show();
                }
            }
            // The map belongs to the game; don't leave it over whatever the user switched to.
            HostEvent::GameUnfocused => self.hide(),
            HostEvent::Session(session) => {
                self.app.on_session(session);
                self.needs_redraw = true;
            }
            HostEvent::Repaint(delay) => {
                if delay.is_zero() {
                    self.needs_redraw = true;
                } else if delay < Duration::from_secs(3600) {
                    let at = Instant::now() + delay;
                    self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
                }
            }
        }
    }

    fn show(&mut self) {
        if self.overlay.is_some() {
            return;
        }
        let surface = self.compositor.create_surface(&self.qh);
        let output = self.game_output();
        // KWin types layer surfaces by namespace. As an on-screen display it fades in and out
        // (Fading Popups) instead of getting the open/close animation of a normal window.
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("on-screen-display"),
            output.as_ref(),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        // Like the Steam overlay, leave the game focused: an unfocused game stops taking input
        // and Proton treats it as in the background. The hotkey listener still sees M to close.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        self.overlay = Some(Overlay {
            layer,
            surface: None,
            width: 0,
            height: 0,
            scale: 1,
            configured: false,
            frame_pending: false,
        });
        self.shown_at = Instant::now();
        self.app.on_show();
        log::info!("overlay shown");
    }

    /// The monitor showing the game window; `None` lets the compositor pick (the active one).
    fn game_output(&self) -> Option<wl_output::WlOutput> {
        let (x, y) = self.game_center?;
        self.output_state.outputs().find(|output| {
            let Some(info) = self.output_state.info(output) else {
                return false;
            };
            let (Some((ox, oy)), Some((w, h))) = (info.logical_position, info.logical_size) else {
                return false;
            };
            (ox..ox + w).contains(&x) && (oy..oy + h).contains(&y)
        })
    }

    fn hide(&mut self) {
        let Some(mut overlay) = self.overlay.take() else {
            return;
        };
        // The GPU surface must go before the Wayland surface it draws to.
        overlay.surface.take();
        drop(overlay);
        self.events.clear();
        self.pointer_pos = None;
        self.keyboard_focus = false;
        self.last_hide = Instant::now();
        self.app.on_hide();
        self.release_textures();
        log::info!("overlay hidden");
    }

    /// Runs an empty egui pass so textures the app dropped are freed on the GPU now, rather
    /// than the next time the overlay opens.
    fn release_textures(&mut self) {
        let Gpu {
            device,
            queue,
            renderer,
            ..
        } = &mut self.gpu;
        let Some((renderer, _)) = renderer.as_mut() else {
            return;
        };
        let output = self.egui_ctx.run_ui(egui::RawInput::default(), |_| {});
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        for id in &output.textures_delta.free {
            renderer.free_texture(id);
        }
    }

    fn configure_surface(&mut self) {
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        let gpu = &mut self.gpu;
        if overlay.surface.is_none() {
            let display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(self.conn.backend().display_ptr().cast()).expect("display pointer"),
            ));
            let window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
                NonNull::new(overlay.layer.wl_surface().id().as_ptr().cast())
                    .expect("surface pointer"),
            ));
            // SAFETY: the surface is dropped in `hide` before the layer surface it points to.
            let surface = unsafe {
                gpu.instance
                    .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                        raw_display_handle: Some(display),
                        raw_window_handle: window,
                    })
            };
            match surface {
                Ok(surface) => overlay.surface = Some(surface),
                Err(e) => {
                    log::error!("creating the GPU surface: {e}");
                    return;
                }
            }
        }
        let surface = overlay.surface.as_ref().unwrap();
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = match &gpu.renderer {
            Some((_, format)) => *format,
            None => {
                let format = egui_wgpu::preferred_framebuffer_format(&caps.formats)
                    .unwrap_or(caps.formats[0]);
                let renderer = egui_wgpu::Renderer::new(
                    &gpu.device,
                    format,
                    egui_wgpu::RendererOptions::default(),
                );
                gpu.renderer = Some((renderer, format));
                format
            }
        };
        let alpha_mode = if caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            log::warn!(
                "the GPU surface can't be transparent here ({:?})",
                caps.alpha_modes
            );
            caps.alpha_modes[0]
        };
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        let scale = overlay.scale.max(1) as u32;
        surface.configure(
            &gpu.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                color_space: Default::default(),
                width: overlay.width.max(1) * scale,
                height: overlay.height.max(1) * scale,
                present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: vec![],
            },
        );
        overlay.layer.wl_surface().set_buffer_scale(overlay.scale);
        overlay.configured = true;
        self.needs_redraw = true;
    }

    fn render(&mut self) {
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        if !overlay.configured || overlay.frame_pending {
            return;
        }
        let Some(surface) = overlay.surface.as_ref() else {
            return;
        };
        let Gpu {
            device,
            queue,
            renderer,
            ..
        } = &mut self.gpu;
        let Some((renderer, _)) = renderer.as_mut() else {
            return;
        };
        self.needs_redraw = false;

        let pixels_per_point = overlay.scale as f32;
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(overlay.width as f32, overlay.height as f32),
            )),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.events),
            focused: self.keyboard_focus,
            max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
            ..Default::default()
        };
        raw.viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);

        let app = &mut self.app;
        let output = self.egui_ctx.run_ui(raw, |ui| app.ui(ui));

        if let Some(viewport) = output.viewport_output.get(&egui::ViewportId::ROOT) {
            if viewport.repaint_delay.is_zero() {
                self.needs_redraw = true;
            } else if viewport.repaint_delay < Duration::from_secs(3600) {
                let at = Instant::now() + viewport.repaint_delay;
                self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
            }
        }
        let cursor = cursor_icon(output.platform_output.cursor_icon);
        if cursor != self.cursor {
            self.cursor = cursor;
            if let Some(pointer) = &self.pointer {
                let _ = pointer.set_cursor(&self.conn, cursor);
            }
        }

        let mut primitives = self
            .egui_ctx
            .tessellate(output.shapes, output.pixels_per_point);
        if let Some(fade) = self.fade_in {
            // Colours are premultiplied, so scaling every channel fades the whole frame.
            let t = self.shown_at.elapsed().as_secs_f32() / fade.as_secs_f32();
            if t < 1.0 {
                for primitive in &mut primitives {
                    if let egui::epaint::Primitive::Mesh(mesh) = &mut primitive.primitive {
                        for vertex in &mut mesh.vertices {
                            vertex.color = vertex.color.linear_multiply(t);
                        }
                    }
                }
                self.needs_redraw = true;
            }
        }
        for (id, deltas) in &output.textures_delta.set {
            for delta in deltas {
                renderer.update_texture(device, queue, *id, delta);
            }
        }
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => {
                log::debug!(
                    "skipping frame: {}",
                    match other {
                        wgpu::CurrentSurfaceTexture::Timeout => "timeout",
                        wgpu::CurrentSurfaceTexture::Occluded => "occluded",
                        _ => "surface outdated",
                    }
                );
                self.needs_redraw = true;
                if !matches!(
                    other,
                    wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded
                ) {
                    self.configure_surface();
                }
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [frame.texture.width(), frame.texture.height()],
            pixels_per_point,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let mut commands =
            renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("overlay"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render(&mut pass, &primitives, &screen);
        }
        commands.push(encoder.finish());
        queue.submit(commands);
        // Ask for a frame callback in the same commit as this frame, to pace redraws.
        let wl_surface = overlay.layer.wl_surface();
        wl_surface.frame(&self.qh, FrameCallbackData(wl_surface.clone()));
        overlay.frame_pending = true;
        queue.present(frame);
        for id in &output.textures_delta.free {
            renderer.free_texture(id);
        }

        if self.app.take_close_request() {
            self.hide();
        }
    }

    fn push_event(&mut self, event: egui::Event) {
        self.events.push(event);
        self.needs_redraw = true;
    }

    fn is_overlay(&self, surface: &wl_surface::WlSurface) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|o| o.layer.wl_surface() == surface)
    }
}

fn cursor_icon(icon: egui::CursorIcon) -> CursorIcon {
    use egui::CursorIcon as E;
    match icon {
        E::PointingHand => CursorIcon::Pointer,
        E::Grab => CursorIcon::Grab,
        E::Grabbing => CursorIcon::Grabbing,
        E::Text => CursorIcon::Text,
        E::ResizeHorizontal | E::ResizeColumn => CursorIcon::EwResize,
        E::ResizeVertical | E::ResizeRow => CursorIcon::NsResize,
        E::Move | E::AllScroll => CursorIcon::Move,
        E::NotAllowed | E::NoDrop => CursorIcon::NotAllowed,
        E::Wait => CursorIcon::Wait,
        E::Crosshair => CursorIcon::Crosshair,
        E::ZoomIn => CursorIcon::ZoomIn,
        E::ZoomOut => CursorIcon::ZoomOut,
        _ => CursorIcon::Default,
    }
}

fn egui_key(keysym: Keysym) -> Option<egui::Key> {
    use egui::Key;
    Some(match keysym {
        Keysym::Escape => Key::Escape,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Delete => Key::Delete,
        Keysym::space => Key::Space,
        Keysym::Left => Key::ArrowLeft,
        Keysym::Right => Key::ArrowRight,
        Keysym::Up => Key::ArrowUp,
        Keysym::Down => Key::ArrowDown,
        Keysym::Home => Key::Home,
        Keysym::End => Key::End,
        Keysym::Prior => Key::PageUp,
        Keysym::Next => Key::PageDown,
        Keysym::plus | Keysym::KP_Add => Key::Plus,
        Keysym::minus | Keysym::KP_Subtract => Key::Minus,
        Keysym::equal => Key::Equals,
        _ => {
            let c = keysym.key_char()?.to_ascii_uppercase();
            if !c.is_ascii_alphanumeric() {
                return None;
            }
            return Key::from_name(&c.to_string());
        }
    })
}

impl Host {
    fn key(&mut self, event: &KeyEvent, pressed: bool, repeat: bool) {
        if let Some(key) = egui_key(event.keysym) {
            self.push_event(egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat,
                modifiers: self.modifiers,
            });
        }
        if pressed
            && !self.modifiers.ctrl
            && !self.modifiers.alt
            && let Some(text) = event
                .utf8
                .as_ref()
                .filter(|t| t.chars().all(|c| !c.is_control()))
        {
            self.push_event(egui::Event::Text(text.clone()));
        }
    }
}

impl KeyboardHandler for Host {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        if self.is_overlay(surface) {
            self.keyboard_focus = true;
            self.push_event(egui::Event::WindowFocused(true));
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.is_overlay(surface) {
            self.keyboard_focus = false;
            self.push_event(egui::Event::WindowFocused(false));
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, true, false);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, true, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, false, false);
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: modifiers.ctrl,
        };
        self.push_event(egui::Event::ModifiersChanged(self.modifiers));
    }
}

impl PointerHandler for Host {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if !self.is_overlay(&event.surface) {
                continue;
            }
            let pos = egui::pos2(event.position.0 as f32, event.position.1 as f32);
            match &event.kind {
                PointerEventKind::Enter { .. } => {
                    if let Some(pointer) = &self.pointer {
                        let _ = pointer.set_cursor(conn, self.cursor);
                    }
                    self.pointer_pos = Some(pos);
                    self.push_event(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Leave { .. } => {
                    self.pointer_pos = None;
                    self.push_event(egui::Event::PointerGone);
                }
                PointerEventKind::Motion { .. } => {
                    self.pointer_pos = Some(pos);
                    self.push_event(egui::Event::PointerMoved(pos));
                }
                PointerEventKind::Press { button, .. }
                | PointerEventKind::Release { button, .. } => {
                    let button = match *button {
                        0x110 => egui::PointerButton::Primary,
                        0x111 => egui::PointerButton::Secondary,
                        0x112 => egui::PointerButton::Middle,
                        0x113 => egui::PointerButton::Extra1,
                        0x114 => egui::PointerButton::Extra2,
                        _ => continue,
                    };
                    let pressed = matches!(event.kind, PointerEventKind::Press { .. });
                    self.push_event(egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: self.modifiers,
                    });
                }
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    let lines = |a: &smithay_client_toolkit::seat::pointer::AxisScroll| {
                        if a.value120 != 0 {
                            a.value120 as f32 / 120.0
                        } else {
                            a.discrete as f32
                        }
                    };
                    let (unit, delta) = if lines(horizontal) != 0.0 || lines(vertical) != 0.0 {
                        (
                            egui::MouseWheelUnit::Line,
                            egui::vec2(-lines(horizontal), -lines(vertical)),
                        )
                    } else {
                        (
                            egui::MouseWheelUnit::Point,
                            egui::vec2(-horizontal.absolute as f32, -vertical.absolute as f32),
                        )
                    };
                    if delta != egui::Vec2::ZERO {
                        self.push_event(egui::Event::MouseWheel {
                            unit,
                            delta,
                            phase: egui::TouchPhase::Move,
                            modifiers: self.modifiers,
                        });
                    }
                }
            }
        }
    }
}

impl CompositorHandler for Host {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if self.is_overlay(surface) {
            if let Some(overlay) = self.overlay.as_mut() {
                overlay.scale = factor;
            }
            if self.overlay.as_ref().is_some_and(|o| o.configured) {
                self.configure_surface();
            }
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.is_overlay(surface)
            && let Some(overlay) = self.overlay.as_mut()
        {
            overlay.frame_pending = false;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Host {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.hide();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(overlay) = self.overlay.as_mut() else {
            return;
        };
        let (width, height) = configure.new_size;
        if width == 0 || height == 0 {
            return;
        }
        overlay.width = width;
        overlay.height = height;
        self.configure_surface();
    }
}

impl SeatHandler for Host {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = self.seat_state.get_keyboard(qh, &seat, None).ok();
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            let cursor_surface = self.compositor.create_surface(qh);
            self.pointer = self
                .seat_state
                .get_pointer_with_theme::<Host, ()>(
                    qh,
                    &seat,
                    self.shm.wl_shm(),
                    cursor_surface,
                    ThemeSpec::System,
                )
                .ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer {
            self.pointer = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for Host {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Host {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(Host);

impl ProvidesRegistryState for Host {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(Host);
