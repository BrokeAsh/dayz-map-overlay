//! Shows the overlay as a borderless, always-on-top, see-through window on Windows.
//!
//! The window never takes focus (`WS_EX_NOACTIVATE`), so DayZ stays the active window and keeps
//! running, like the Steam overlay. It draws through a DirectComposition swapchain, which is what
//! lets DX12 output be transparent. The hotkey comes from raw keyboard input, which Windows
//! delivers to background windows too (`RIDEV_INPUTSINK`, through winit's device events); the
//! press still reaches the game. DayZ must run in windowed or borderless mode: nothing can draw
//! over an exclusive-fullscreen game without hooking into it.

use anyhow::{Context, Result};
use egui_wgpu::wgpu;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, VK_CONTROL, VK_F1, VK_LWIN, VK_MENU,
    VK_RWIN, VK_TAB, VkKeyScanW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongPtrW, HWND_TOPMOST, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop, EventLoopProxy};
use winit::keyboard::PhysicalKey;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use super::HostEvent;
use super::gpu::{Fade, Frame, Gpu, repaint_after, repaint_in};
use crate::config::Config;
use crate::ipc::{self, Command};
use crate::ui::OverlayApp;
use crate::win;

/// Ignore the hotkey this soon after closing, in case the game sees the closing key press.
const REOPEN_GRACE: Duration = Duration::from_millis(400);
/// How often to check, while the map is open, that the game is still in front.
const FOCUS_CHECK: Duration = Duration::from_millis(250);
const FADE: Duration = Duration::from_millis(150);
/// How soon to try again when Windows has nowhere to show a frame (the screen is off or locked).
const SKIPPED_RETRY: Duration = Duration::from_millis(100);
/// Longer than Windows' longest key-repeat delay (one second): a "press" of a key that's already
/// down after this long is a new press whose release was missed (say, behind a UAC prompt).
const REPEAT_WINDOW: Duration = Duration::from_millis(1100);

pub fn run(config: Config, show: bool) -> Result<()> {
    let event_loop = EventLoop::<HostEvent>::with_user_event()
        .build()
        .context("starting the window system")?;
    // Raw keyboard input while other windows are in front: the hotkey.
    event_loop.listen_device_events(DeviceEvents::Always);
    win::stop_background_mouse();
    let proxy = event_loop.create_proxy();

    let ipc_proxy = proxy.clone();
    let _control = ipc::listen(move |command| {
        let _ = ipc_proxy.send_event(HostEvent::Command(command));
    })?;
    let session_proxy = proxy.clone();
    crate::game::spawn(move |session| {
        let _ = session_proxy.send_event(HostEvent::Session(session));
    });

    let egui_ctx = egui::Context::default();
    let repaint_proxy = std::sync::Mutex::new(proxy.clone());
    egui_ctx.set_request_repaint_callback(move |info| {
        let _ = repaint_proxy
            .lock()
            .unwrap()
            .send_event(HostEvent::Repaint(info.delay));
    });

    let hotkey = hotkey_key(&config.hotkey);
    if hotkey.is_none() {
        log::warn!(
            "hotkey disabled: no key produces {:?}. Use `dayz-map toggle` instead.",
            config.hotkey
        );
    }
    let patterns = config
        .window_match
        .iter()
        .map(|p| p.to_lowercase())
        .collect();
    let gpu = Gpu::new(wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                // A composition swapchain is the one that can be transparent.
                presentation_system: wgpu::wgt::Dx12SwapchainKind::DxgiFromVisual,
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    }))?;
    let mut host = Host {
        app: OverlayApp::new(&egui_ctx, config.clone()),
        egui_ctx,
        gpu,
        window: None,
        hotkey,
        patterns,
        hotkey_seen: None,
        visible: false,
        follow_focus: false,
        game: None,
        next_repaint: None,
        next_focus_check: Instant::now(),
        last_hide: Instant::now() - REOPEN_GRACE,
        fade: Fade::new(Some(FADE)),
        outdated: 0,
        restart: false,
        show_at_start: show,
        _proxy: proxy,
    };
    log::info!(
        "ready; press {} in DayZ to open the map",
        config.hotkey.to_uppercase()
    );
    event_loop.run_app(&mut host)?;
    if host.restart {
        drop(_control);
        super::restart();
    }
    Ok(())
}

struct OverlayWindow {
    window: Arc<Window>,
    hwnd: HWND,
    surface: wgpu::Surface<'static>,
    input: egui_winit::State,
}

struct Host {
    app: OverlayApp,
    egui_ctx: egui::Context,
    gpu: Gpu,
    window: Option<OverlayWindow>,
    hotkey: Option<PhysicalKey>,
    patterns: Vec<String>,
    /// When the hotkey was last pressed or repeated while held down, to ignore key repeat.
    hotkey_seen: Option<Instant>,
    visible: bool,
    /// Close when the game stops being the front window (set when opened over the game).
    follow_focus: bool,
    /// The game window the hotkey was pressed in, to open on its monitor.
    game: Option<HWND>,
    next_repaint: Option<Instant>,
    next_focus_check: Instant,
    last_hide: Instant,
    fade: Fade,
    /// Frames in a row that found the surface out of date.
    outdated: u32,
    /// Start over when the loop ends (the graphics device was lost).
    restart: bool,
    show_at_start: bool,
    _proxy: EventLoopProxy<HostEvent>,
}

impl Host {
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let attributes = WindowAttributes::default()
            .with_title("DayZ Map Overlay")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_visible(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_no_redirection_bitmap(true);
        let window = Arc::new(event_loop.create_window(attributes)?);
        let RawWindowHandle::Win32(handle) = window.window_handle()?.as_raw() else {
            anyhow::bail!("not a Win32 window");
        };
        let hwnd = handle.hwnd.get() as HWND;
        // SAFETY: changing the extended style of our own window.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                hwnd,
                GWL_EXSTYLE,
                style | (WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) as isize,
            );
        }
        let surface = self
            .gpu
            .instance
            .create_surface(window.clone())
            .context("creating the GPU surface")?;
        let size = window.inner_size();
        self.gpu.configure(&surface, size.width, size.height)?;
        let input = egui_winit::State::new(
            self.egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(self.gpu.device.limits().max_texture_dimension_2d as usize),
        );
        self.window = Some(OverlayWindow {
            window,
            hwnd,
            surface,
            input,
        });
        Ok(())
    }

    fn matches_game(&self, description: &str) -> bool {
        self.patterns.is_empty() || self.patterns.iter().any(|p| description.contains(p))
    }

    fn show(&mut self, follow_focus: bool) {
        let Some(overlay) = &self.window else {
            return;
        };
        if self.visible {
            return;
        }
        // Cover the monitor the game is on (or the front window's, or our own).
        let anchor = self
            .game
            .filter(|&hwnd| win::is_window(hwnd))
            .or_else(|| win::foreground().map(|f| f.hwnd))
            .unwrap_or(overlay.hwnd);
        let Some(rect) = win::monitor_rect(anchor) else {
            log::warn!("no monitor to show the overlay on");
            return;
        };
        // SAFETY: positioning and showing our own window, without activating it. Twice: moving
        // to a monitor with different scaling makes Windows rescale the window afterwards
        // (WM_DPICHANGED); the second call, already on that monitor, sets the size for real.
        for _ in 0..2 {
            unsafe {
                SetWindowPos(
                    overlay.hwnd,
                    HWND_TOPMOST,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
        self.visible = true;
        self.follow_focus = follow_focus;
        self.next_focus_check = Instant::now() + FOCUS_CHECK;
        self.fade.restart();
        self.app.on_show();
        overlay.window.request_redraw();
        log::info!("overlay shown");
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        if let Some(overlay) = &self.window {
            // SAFETY: hiding our own window.
            unsafe { ShowWindow(overlay.hwnd, SW_HIDE) };
        }
        self.visible = false;
        self.last_hide = Instant::now();
        self.app.on_hide();
        self.gpu.release_textures(&self.egui_ctx);
        log::info!("overlay hidden");
    }

    fn toggle(&mut self, follow_focus: bool) {
        if self.visible {
            self.hide();
        } else {
            self.show(follow_focus);
        }
    }

    fn request_redraw(&self) {
        if let Some(overlay) = &self.window {
            overlay.window.request_redraw();
        }
    }

    fn on_key(&mut self, key: PhysicalKey, state: ElementState) {
        if Some(key) != self.hotkey {
            return;
        }
        if state == ElementState::Released {
            self.hotkey_seen = None;
            return;
        }
        let repeat = self
            .hotkey_seen
            .is_some_and(|seen| seen.elapsed() < REPEAT_WINDOW);
        self.hotkey_seen = Some(Instant::now());
        if repeat {
            return;
        }
        // SAFETY: plain key-state queries.
        let modifier = [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0);
        if modifier {
            log::info!("hotkey ignored: Ctrl, Alt or Windows is held");
            return;
        }
        let Some(front) = win::foreground() else {
            return;
        };
        if front.ours {
            // Clicking the map doesn't activate it, but a dialog of ours might be in front.
            self.hide();
            return;
        }
        let matches = self.matches_game(&front.description);
        log::info!(
            "hotkey in {:?}: {}",
            front.description,
            if matches {
                "toggling the map"
            } else {
                "not the game"
            }
        );
        if !matches {
            return;
        }
        self.game = Some(front.hwnd);
        if self.visible {
            self.hide();
        } else if self.last_hide.elapsed() >= REOPEN_GRACE {
            self.show(true);
        }
    }

    fn render(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_lost() {
            self.hide();
            self.restart = true;
            event_loop.exit();
            return;
        }
        let Some(overlay) = self.window.as_mut() else {
            return;
        };
        if !self.visible {
            return;
        }
        let window = overlay.window.clone();
        let raw = overlay.input.take_egui_input(&window);
        let app = &mut self.app;
        let mut output = self.egui_ctx.run_ui(raw, |ui| app.ui(ui));
        overlay
            .input
            .handle_platform_output(&window, std::mem::take(&mut output.platform_output));
        match repaint_after(&output) {
            Some(None) => window.request_redraw(),
            Some(Some(at)) => {
                self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
            }
            None => {}
        }
        let (opacity, fading) = self.fade.opacity();
        match self
            .gpu
            .paint(&overlay.surface, &self.egui_ctx, output, opacity, || {})
        {
            Frame::Presented if fading => {
                self.outdated = 0;
                window.request_redraw();
            }
            Frame::Presented => self.outdated = 0,
            Frame::Skipped => {
                let at = Instant::now() + SKIPPED_RETRY;
                self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
            }
            Frame::Outdated => {
                self.outdated += 1;
                let size = window.inner_size();
                match self
                    .gpu
                    .configure(&overlay.surface, size.width, size.height)
                {
                    // Redraw right away after a resize, but don't spin if it keeps happening.
                    Ok(()) if self.outdated <= 2 => window.request_redraw(),
                    Ok(()) => {
                        let at = Instant::now() + SKIPPED_RETRY;
                        self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
                    }
                    Err(e) => log::error!("{e:#}"),
                }
            }
        }
        if self.app.take_close_request() {
            self.hide();
        }
    }
}

impl ApplicationHandler<HostEvent> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.create_window(event_loop) {
            log::error!("creating the overlay window: {e:#}");
            event_loop.exit();
            return;
        }
        if std::mem::take(&mut self.show_at_start) {
            self.show(false);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: HostEvent) {
        match event {
            HostEvent::Command(Command::Show) => self.show(false),
            HostEvent::Command(Command::Hide) => self.hide(),
            HostEvent::Command(Command::Toggle) => self.toggle(false),
            HostEvent::Command(Command::Quit) => {
                self.hide();
                event_loop.exit();
            }
            HostEvent::Session(session) => {
                self.app.on_session(session);
                self.request_redraw();
            }
            HostEvent::Repaint(delay) => match repaint_in(delay) {
                Some(None) => self.request_redraw(),
                Some(Some(at)) => {
                    self.next_repaint = Some(self.next_repaint.map_or(at, |t| t.min(at)));
                }
                None => {}
            },
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(overlay) = self.window.as_mut() else {
            return;
        };
        let response = overlay.input.on_window_event(&overlay.window, &event);
        match event {
            WindowEvent::RedrawRequested => self.render(event_loop),
            WindowEvent::Resized(size) => {
                match self
                    .gpu
                    .configure(&overlay.surface, size.width, size.height)
                {
                    Ok(()) => overlay.window.request_redraw(),
                    Err(e) => log::error!("{e:#}"),
                }
            }
            // Not closable from the taskbar (it isn't on it) or Alt+F4 (it never has focus).
            WindowEvent::CloseRequested => {}
            _ if response.repaint => overlay.window.request_redraw(),
            _ => {}
        }
    }

    fn device_event(&mut self, _: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::Key(key) = event {
            self.on_key(key.physical_key, key.state);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if self.next_repaint.is_some_and(|t| t <= now) {
            self.next_repaint = None;
            self.request_redraw();
        }
        if self.visible && self.follow_focus && self.next_focus_check <= now {
            self.next_focus_check = now + FOCUS_CHECK;
            // The map belongs to the game; don't leave it over whatever the user switched to.
            // (Checking the window handle first skips the process lookup in the usual case.)
            if self.game != Some(win::foreground_window())
                && let Some(front) = win::foreground()
                && !front.ours
                && !self.matches_game(&front.description)
            {
                self.hide();
            }
        }
        let mut wake = self.next_repaint;
        if self.visible && self.follow_focus {
            wake = Some(wake.map_or(self.next_focus_check, |t| t.min(self.next_focus_check)));
        }
        event_loop.set_control_flow(match wake {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}

/// The physical key for the configured hotkey (a character, f1-f24, `tab`, or `grave`), as laid
/// out on this keyboard.
fn hotkey_key(name: &str) -> Option<PhysicalKey> {
    let lower = name.trim().to_lowercase();
    let mut chars = lower.chars();
    // SAFETY: plain keyboard-layout queries.
    let from_char = |c: char| -> Option<u16> {
        let scan = unsafe { VkKeyScanW(c as u16) };
        (scan != -1).then_some((scan & 0xff) as u16)
    };
    let vk = match (chars.next(), chars.next()) {
        (Some(c), None) => from_char(c)?,
        _ => match lower.as_str() {
            "tab" => VK_TAB,
            "grave" | "backtick" => from_char('`')?,
            f => {
                let n: u16 = f.strip_prefix('f')?.parse().ok()?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                VK_F1 + n - 1
            }
        },
    };
    // SAFETY: plain keyboard-layout query.
    let scancode = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX) };
    (scancode != 0).then(|| PhysicalKey::from_scancode(scancode))
}
