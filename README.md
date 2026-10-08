# DayZ Map Overlay

A see-through map that opens over DayZ when you press **M**, similar to the Steam overlay.
It switches to the map of the server you're on by itself, building that map from the game's
own files (or the server's Workshop mods) the first time, and marks points of interest:
towns, water pumps, fuel, loot buildings by type, heli crash sites, vehicle spawns, and more.

It's written in Rust and runs on Windows 10 and 11, and on Linux desktops that support
layer-shell overlays (KDE Plasma 6, Sway, Hyprland) with DayZ running through Steam/Proton. It
never touches the game process: it reads the game's files and logs, draws its own window above
the game, and listens for the hotkey like any other desktop app, so BattlEye has nothing to see.

## Requirements

Windows:

- Windows 10 or 11 with DirectX 12.
- DayZ in **borderless** or **windowed** mode. Nothing can draw over exclusive fullscreen.
- DayZ through Steam. It's found through Steam's library list, so there's usually nothing to set.

Linux:

- Linux with a Wayland desktop that supports layer-shell overlays: KDE Plasma 6, Sway,
  Hyprland, and other wlroots desktops. GNOME and X11 sessions aren't supported yet.
- A GPU with Vulkan (any gaming GPU and driver from the last several years).
- DayZ through Steam (Proton). DayZ is found through Steam's library list, including extra
  library drives and the Flatpak and Snap versions of Steam, so there's usually nothing to set.

## Install

### Windows

Download `dayz-map-<version>-windows-x86_64.zip` from the
[releases](https://github.com/BrokeAsh/dayz-map-overlay/releases) and extract it somewhere it can
stay, such as `%LOCALAPPDATA%\Programs\dayz-map`. Then, in a terminal in that folder:

```powershell
.\dayz-map status          # check that it found DayZ, the Workshop folder and the logs
.\dayz-map autostart on    # start when you sign in (`off` to undo)
start .\dayz-map.exe      # or start it now (double-clicking dayz-map.exe works too)
```

Started that way (or at sign-in), the overlay runs in the background without a window of its
own and logs to `%APPDATA%\dayz-map-overlay\data\dayz-map.log`; `dayz-map quit` stops it.
Started as plain `.\dayz-map`, it stays in that terminal and logs there instead, and closing the
terminal stops it. The program isn't signed yet, so SmartScreen may
warn the first time you run it.

### Linux

Download `dayz-map-<version>-linux-x86_64.tar.gz` from the
[releases](https://github.com/BrokeAsh/dayz-map-overlay/releases), then:

```sh
tar xzf dayz-map-*-linux-x86_64.tar.gz
install -Dm755 dayz-map-*/dayz-map ~/.local/bin/dayz-map
dayz-map status          # check that it found DayZ, the Workshop folder and the logs
dayz-map autostart on    # start with your desktop (`off` to undo)
dayz-map &               # or start it now
```

`~/.local/bin` must be on your `PATH` for the short commands; the autostart entry uses the full
path either way.

To build it yourself, install Rust and the Wayland and xkbcommon development packages
(`libwayland-dev libxkbcommon-dev` on Debian and Ubuntu), then run `cargo build --release`.

## Use

Join a server and press **M**. The first time you join a map it takes a couple of seconds
(during the loading screen) to build it. Like the Steam overlay, the game stays focused and keeps
running underneath, so the keyboard still goes to DayZ. The map takes the mouse whenever the
game's cursor is free over it:

| Action | How |
| --- | --- |
| Pan | drag |
| Zoom | scroll wheel, double-click |
| Reset view | **Fit** |
| Points of interest | **Layers** |
| Other maps | **Maps…** (the overlay switches back when you join a server) |
| Close | **M**, the **×** button, or switching away from the game |

The toolbar shows the map, the server's name, and sliders for map opacity and how much the
game is dimmed. Hover a marker for its name. The cursor's in-game coordinates (X east, Z north,
in metres) show in the bottom-left corner; grid squares count from the north-west corner.

The overlay opens on the monitor the game is on. DayZ must be the focused window, and it works
best in borderless or windowed mode (on Windows, it only works in those). If DayZ binds its own action to M, rebind that action in
the game or change `hotkey`.

Other commands:

- `dayz-map toggle | show | hide | quit` control the running overlay.
- `dayz-map status` shows the folders it found (and how), and what the game is doing: running,
  map, server, and mods.
- `dayz-map autostart on | off` starts the overlay when you sign in, or stops doing that.
- `dayz-map list` shows installed maps and every terrain found in the game and Workshop folders.
- `dayz-map import <world>…` or `--all` builds maps ahead of time.

## Maps

Maps come from the game folder and every Steam library's Workshop folder
(`steamapps/workshop/content/221100`), so new map mods are picked up when you join a server
that uses them. A terrain is assembled from:

- the world archive: the `.wrp` terrain (its size) and `config.bin` (`CfgWorlds`: the world's
  id and place names),
- the satellite tiles (`layers\S_XXX_YYY_lco.paa`) and their materials (`P_*.rvmat`), which give
  the tile overlap and where the grid sits,
- the Central Economy files that every map ships for its default mission.

The world id is the `CfgWorlds` class name, which is what the game reports when it joins a
server. When several mods ship the same terrain (an official and an experimental build, say),
the one the server loads wins. A map is rebuilt when its mod updates. Maps are stored in
`~/.local/share/dayz-map-overlay/maps/<world>/` (on Windows,
`%APPDATA%\dayz-map-overlay\data\maps\<world>\`), about 40–80 MB and one or two seconds each.

For a terrain whose files can't be read, import a picture of the whole map, north up:

```sh
dayz-map import-image mymap mymap.png --world-size 12800 --name "My Map"
```

## Points of interest

| Layer | Source |
| --- | --- |
| Place names | `CfgWorlds >> <world> >> Names` (Chernarus's Cyrillic names are transliterated) |
| Water pumps and wells (blue, tap icon) | the terrain's `.wrp`: every building whose class the game's or the map mod's scripts make a `Well` (pumps, plus mod additions such as Namalsk's lab sinks) |
| Fresh water (blue, wave icon) | the terrain's `.wrp`: pond, lake, river, stream and spring objects, one marker per area (about 400 m) |
| Fuel stations | buildings in `mapgrouppos.xml` |
| Military, police, medical, fire station, hunting, industrial, civilian loot | buildings in `mapgrouppos.xml`, typed by their loot `usage`/`tag` in `mapgroupproto.xml` |
| Heli crashes, police cars, convoys, trains, vehicle and boat spawns, toxic zone spawns | `cfgeventspawns.xml` |
| Static contaminated zones | `cfgeffectarea.json` |

These are each map's defaults; a server can change its loot and events. Water comes from the
terrain itself, so it's complete, except on maps whose world file is encrypted (Sakhal), which
fall back to the wells listed in the economy files. Points of interest are rebuilt on their own
(in well under a second) when the importer's version or the map's files change.

## Settings

`~/.config/dayz-map-overlay/config.toml` (on Windows,
`%APPDATA%\dayz-map-overlay\config\config.toml`) is written when the overlay closes:

```toml
hotkey = "m"                                  # a character or f1–f24 (on Linux, also a keysym like 0x6d)
window_match = ["steam_app_221100", "DayZ"]   # opens only when the focused window matches
                                              # (on Windows: ["dayz_x64.exe"], the game's program)
# game_dir = "/path/to/steamapps/common/DayZ" # only if DayZ isn't found automatically
# log_dir = "/path/to/DayZ/logs"              # only if `dayz-map status` can't find the logs

[view]
map = "chernarusplus"   # last map shown
map_opacity = 0.9
backdrop_opacity = 0.35
show_grid = true
show_places = true

[view.layers]           # point-of-interest layers you switched on or off
civilian = false
```

If DayZ isn't in any Steam library, the Maps window says so and offers **Choose the DayZ
folder…**, which saves `game_dir` for you. The Workshop folder and the logs are then worked out
from the game's library (or from the game's `!Workshop` links), so `log_dir` is rarely needed.
`DAYZ_MAP_LOG_DIR` overrides the log folder for one run (useful for testing with recorded logs).

## Troubleshooting

- **M does nothing (Windows):** check that DayZ is in borderless or windowed mode, then look in
  `dayz-map.log` (see [Install](#windows)). It logs `hotkey in "…": toggling the map` for every
  press while the overlay runs; `…: not the game` means `window_match` doesn't match the game's
  window (the text in quotes is what to match). If DayZ runs as administrator, the overlay must
  too: Windows doesn't pass keys from an elevated window to a normal one.
- **M does nothing (Linux):** run `dayz-map` in a terminal, focus DayZ, and press M. It logs
  `hotkey in "…": toggling the map` for every press; if nothing appears, the key isn't reaching
  it (for example with Proton's experimental Wayland mode, `PROTON_ENABLE_WAYLAND=1`). As a
  workaround, bind `dayz-map toggle` to a key in your desktop's shortcut settings.
- **The map doesn't follow the server:** `dayz-map status` should show a Logs folder and, while
  you're on a server, the map and server name.
- **A map is missing:** `dayz-map list` shows every terrain found; Rescan in the Maps window
  picks up newly downloaded mods.

## How it works

- **Which map** (`src/game.rs`): DayZ writes `script_<date>.log` in `%LOCALAPPDATA%\DayZ` (on
  Linux, inside its Proton prefix). Each
  mission start logs `Creating Mission: mpmissions\__cur_mp.<world>\mission.c`; `intro.<world>`
  is the main menu. The RPT log's command line has the launcher's `-connect=ip:port:query` and
  `-mod=` list. A Steam server query (A2S_INFO) fetches the server name, kept only if the server
  reports the same map. If the game runs with `-nologs`, there's nothing to follow.
- **Library** (`src/library.rs`, `src/import/catalog.rs`): scans the game and Workshop folders
  (archive headers only, about half a second for hundreds of mods), imports the map the game is
  on if it's missing or out of date, and rebuilds maps made by older versions.
- **Overlay window on Linux** (`src/host/wayland.rs`): a full-screen `wlr-layer-shell` surface on the
  `overlay` layer, which compositors draw above fullscreen windows, placed on the monitor with
  the game window. It never takes keyboard focus, so the game stays the active window and
  keeps running; clicks on it don't activate it either. Its namespace is `on-screen-display`,
  which KWin treats like the volume OSD: it fades in and out rather than animating like a window.
  Rendering is egui on wgpu (Vulkan).
- **Windows** (`src/host/windows.rs`): a topmost, non-activating tool window
  (`WS_EX_NOACTIVATE`) covering the game's monitor, drawn with DirectX 12 through DirectComposition
  so it can be see-through. Clicks never activate it, so DayZ stays the foreground window. The
  hotkey comes from raw keyboard input, which Windows delivers to background windows without a
  hook, and the overlay hides when another window comes to the front.
- **Hotkey on Linux** (`src/trigger/`): Proton games draw through XWayland, which only receives keyboard
  input while one of its windows is focused. The app listens for raw XInput2 key events from
  XWayland and checks the focused window against `window_match`: no key grab, no root access, and
  no false triggers while typing in other apps. Because the game keeps focus, the same listener
  sees the second press that closes the map, and it hides the map when focus leaves the game.
- **Map view** (`src/ui/`): tiles stream from disk on worker threads into an LRU texture cache at
  the pyramid level that matches the zoom. Closing the overlay frees all but the overview tiles.
- **Importer** (`src/import/`): PBO archives (including LZSS-compressed entries), PAA textures
  (DXT1/5 + LZO), rapified configs, `.wrp` world files (header, classed buildings, and the 60-byte object
  table, found by its record shape), and the economy files.

## Not done yet

- Server-specific loot and events (servers don't share their economy files).
- X11-only Linux desktops (the UI and importer are portable; they need a window host).
- GNOME, which doesn't support layer-shell.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option.

Maps and points of interest are built on your machine from the game files and mods you
installed; no map data ships with this program. DayZ is a trademark of Bohemia Interactive,
which isn't affiliated with this project.
