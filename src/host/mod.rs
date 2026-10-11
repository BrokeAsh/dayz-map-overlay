//! Platform windowing for the overlay.

mod gpu;

/// The overlay's egui context.
fn egui_context() -> egui::Context {
    let ctx = egui::Context::default();
    // Shift normally turns the mouse wheel sideways, but players hold it to sprint: the wheel
    // still zooms the map and scrolls the lists.
    ctx.options_mut(|o| o.input_options.horizontal_scroll_modifier = egui::Modifiers::NONE);
    ctx
}

/// Input that ends any click or drag in progress. Closing the overlay mid-drag means it never
/// sees the button come up, and egui would keep dragging on the next mouse move.
fn release_pointer() -> Vec<egui::Event> {
    let release = |button| egui::Event::PointerButton {
        pos: egui::Pos2::ZERO,
        button,
        pressed: false,
        modifiers: Default::default(),
    };
    vec![
        release(egui::PointerButton::Primary),
        release(egui::PointerButton::Secondary),
        release(egui::PointerButton::Middle),
        egui::Event::PointerGone,
    ]
}

/// When this copy started, if it was started by [`restart`].
static RESTARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// Starts a fresh copy of the overlay after the graphics device was lost, open again: the loss
/// is noticed while drawing, so the map was open. Called once this one has let go of the
/// control channel and instance lock.
fn restart() {
    let mut args = vec!["run", "--restarted", "--show"];
    // Lost again straight away (video memory still full, say): opening again would only repeat
    // it, each time taking more of the game's memory. Wait for the hotkey instead.
    if RESTARTED
        .get()
        .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60))
    {
        log::warn!("the graphics device was lost again soon after a restart");
        args.pop();
    }
    log::info!("restarting the overlay");
    match std::env::current_exe() {
        Ok(exe) => {
            let mut command = std::process::Command::new(exe);
            command.args(args);
            // Without a console of our own, Windows would open one for the child.
            #[cfg(windows)]
            std::os::windows::process::CommandExt::creation_flags(&mut command, 0x0800_0000); // CREATE_NO_WINDOW
            if let Err(e) = command.spawn() {
                log::error!("restarting the overlay: {e}");
            }
        }
        Err(e) => log::error!("restarting the overlay: {e}"),
    }
}
#[cfg(target_os = "linux")]
mod wayland;
#[cfg(windows)]
mod windows;

use anyhow::Result;

use crate::config::Config;

/// What the background threads (hotkey listener, control socket, egui) ask the host to do.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Command(crate::ipc::Command),
    /// The hotkey was pressed in the game, whose window is here.
    #[cfg(target_os = "linux")]
    Hotkey(Option<crate::trigger::GameSpot>),
    /// Another window took focus from the game.
    #[cfg(target_os = "linux")]
    GameUnfocused,
    /// The game started, stopped, or changed map.
    Session(crate::game::Session),
    /// egui wants a new frame after this delay.
    Repaint(std::time::Duration),
}

/// Runs the overlay; `restarted` when [`restart`] started it.
pub fn run(config: Config, show: bool, restarted: bool) -> Result<()> {
    if restarted {
        RESTARTED.get_or_init(std::time::Instant::now);
    }
    platform_run(config, show)
}

#[cfg(target_os = "linux")]
fn platform_run(config: Config, show: bool) -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!(
            "this build only supports Wayland desktops with layer-shell (KDE Plasma, Sway, Hyprland)"
        );
    }
    wayland::run(config, show)
}

#[cfg(windows)]
fn platform_run(config: Config, show: bool) -> Result<()> {
    windows::run(config, show)
}

#[cfg(test)]
mod tests {
    /// Shift (held to sprint) mustn't turn the wheel sideways, which would stop it zooming.
    #[test]
    fn shift_keeps_the_wheel_vertical() {
        let ctx = super::egui_context();
        let mut scrolled = egui::Vec2::ZERO;
        for frame in 0..30 {
            let mut input = egui::RawInput::default();
            if frame == 0 {
                input.events.push(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 1.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::SHIFT,
                });
            }
            let mut output =
                ctx.run_ui(input, |ui| scrolled += ui.input(|i| i.smooth_scroll_delta));
            output.textures_delta.clear();
        }
        assert!(scrolled.y > 0.0, "{scrolled:?}");
        assert_eq!(scrolled.x, 0.0);
    }
}
