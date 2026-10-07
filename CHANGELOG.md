# Changelog

## Unreleased

- Windows 10 and 11 support. The map opens over DayZ in borderless or windowed mode without
  taking focus from the game, finds DayZ through Steam's registry entry and library list, reads
  the logs from `%LOCALAPPDATA%\DayZ`, and `dayz-map autostart on` starts it at sign-in.
- `dayz-map status | head` and similar no longer print a broken-pipe error on Linux.
- Config files saved with a byte-order mark (as Windows PowerShell does) load correctly.

## 0.1.0

First release, for Linux Wayland desktops with layer-shell overlays (KDE Plasma 6, Sway,
Hyprland) and DayZ through Steam/Proton.

- Press **M** in DayZ to open a see-through map over the game; the game stays focused and keeps
  running underneath. It opens on the game's monitor and fades in.
- Follows the server you join: the map is picked from the game's logs, and built from the game
  files or the server's Workshop mods the first time (a few seconds, during the loading screen).
  Newly downloaded map mods are found automatically.
- Points of interest: towns, water pumps and wells, fresh water, fuel, loot buildings by type,
  heli crashes, vehicle spawns, contaminated zones, and more, each as a layer you can toggle.
- Finds DayZ, the Workshop folder and the logs through Steam (including extra library drives
  and the Flatpak and Snap versions of Steam), with a folder picker if that fails.
- `dayz-map status` shows what was found; `dayz-map autostart on` starts it with your desktop.
