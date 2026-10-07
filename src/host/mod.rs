//! Platform windowing for the overlay.

mod wayland;

use anyhow::{Result, bail};

use crate::config::Config;

/// What the background threads (hotkey listener, control socket, egui) ask the host to do.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Command(crate::ipc::Command),
    /// The hotkey was pressed in the game, whose window is centred at this point.
    Hotkey(Option<(i32, i32)>),
    /// Another window took focus from the game.
    GameUnfocused,
    /// The game started, stopped, or changed map.
    Session(crate::game::Session),
    /// egui wants a new frame after this delay.
    Repaint(std::time::Duration),
}

pub fn run(config: Config, show: bool) -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        bail!(
            "this build only supports Wayland desktops with layer-shell (KDE Plasma, Sway, Hyprland)"
        );
    }
    wayland::run(config, show)
}
