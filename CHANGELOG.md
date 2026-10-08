# Changelog

## Unreleased

- Windows 10 and 11 support. The map opens over DayZ in borderless or windowed mode without
  taking focus from the game, finds DayZ through Steam's registry entry and library list, reads
  the logs from `%LOCALAPPDATA%\DayZ`, and `dayz-map autostart on` starts it at sign-in.
- `dayz-map status | head` and similar no longer print a broken-pipe error on Linux.
- Config files saved with a byte-order mark (as Windows PowerShell does) load correctly.
- On Linux, the default `window_match` now matches the game's window class (`steam_app_221100`
  under Proton, `dayz_x64.exe` under Wine) instead of any window titled "DayZ", so a browser
  tab about DayZ no longer opens the map. A config that saved the old default is updated.
- On Linux, M opens the map only once the game itself runs, not in the DayZ launcher (whose
  window has the game's class), and the hotkey can be a Latin character such as `ö`.
- The map opens on the game's monitor with scaled XWayland too, and the hotkey keeps working
  when XWayland restarts or starts after the overlay, or the keyboard layout changes.
- Only one overlay runs, even when two start at once. Each import is written beside the old map
  and synced to disk before replacing it, so a failed or interrupted import, a full disk or a
  power cut leaves the previous map intact; imported pictures are never deleted by a later
  import. Two imports of one map (the overlay and `dayz-map import`) wait for each other, and
  `dayz-map import --all` carries on past a map that fails.
- An update that only changes a mod's loot or vehicle spawns rebuilds just the points of
  interest, and the open overlay picks up maps rebuilt by `dayz-map import` or `import-image`.
- Mod and server detection: quoted launch options and absolute or `!Workshop` mod paths are
  read correctly; logs from before the game started are ignored; the current map is found in
  very long logs; the server name is dropped after switching servers through the main menu.
- Rescan finds a newly created Workshop folder and retries a map that failed to build.
- Wells are found when a mod declares them across several script files, and markers placed off
  the map are dropped.
- Hardened the game-file readers against damaged or crafted mods (bounded memory, no panics),
  and replaced the LZO dependency with a checked decoder.
- The overlay restarts itself if the graphics device is lost, and closes when you switch away
  from the game however it was opened.
- `import-image` refuses pictures over 16384 px across.
- Building needs Rust 1.95 or newer.

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
