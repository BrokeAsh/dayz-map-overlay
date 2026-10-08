//! Platform windowing for the overlay.

mod gpu;

/// Starts a fresh copy of the overlay (after the graphics device was lost). Called once this
/// one has let go of the control channel and instance lock.
fn restart() {
    log::info!("restarting the overlay");
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).arg("run").spawn() {
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
