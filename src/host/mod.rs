//! Platform windowing for the overlay.

mod gpu;

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

/// Starts a fresh copy of the overlay after the graphics device was lost, open again: the loss
/// is noticed while drawing, so the map was open. Called once this one has let go of the
/// control channel and instance lock.
fn restart() {
    log::info!("restarting the overlay");
    match std::env::current_exe() {
        Ok(exe) => {
            let mut command = std::process::Command::new(exe);
            command.args(["run", "--restarted", "--show"]);
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
    /// The hotkey was pressed in the game, whose window is centred at this point.
    #[cfg(target_os = "linux")]
    Hotkey(Option<(i32, i32)>),
    /// Another window took focus from the game.
    #[cfg(target_os = "linux")]
    GameUnfocused,
    /// The game started, stopped, or changed map.
    Session(crate::game::Session),
    /// egui wants a new frame after this delay.
    Repaint(std::time::Duration),
}

#[cfg(target_os = "linux")]
pub fn run(config: Config, show: bool) -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!(
            "this build only supports Wayland desktops with layer-shell (KDE Plasma, Sway, Hyprland)"
        );
    }
    wayland::run(config, show)
}

#[cfg(windows)]
pub fn run(config: Config, show: bool) -> Result<()> {
    windows::run(config, show)
}
