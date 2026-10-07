# DayZ Map Overlay

A see-through map that opens over DayZ when you press **M**, similar to the Steam overlay.
It switches to the map of the server you're on by itself, building that map from the game's
own files (or the server's Workshop mods) the first time, and marks points of interest:
towns, water pumps, fuel, loot buildings by type, heli crash sites, vehicle spawns, and more.

It's written in Rust and currently targets Linux desktops that support layer-shell overlays
(KDE Plasma 6, Sway, Hyprland), with DayZ running through Steam/Proton. It never touches the
game process: it reads the game's files and logs, draws its own window above the game, and
listens for the hotkey the way any X11 app can, so BattlEye has nothing to see.

## Use

```sh
cargo build --release
./target/release/dayz-map          # run in the background
```

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
| Close | **M**, the **✕** button, or switching away from the game |

The toolbar shows the map, the server's name, and sliders for map opacity and how much the
game is dimmed. Hover a marker for its name. The cursor's in-game coordinates (X east, Z north,
in metres) show in the bottom-left corner; grid squares count from the north-west corner.

The overlay opens on the monitor the game is on. DayZ must be the focused window, and it works
best in borderless or windowed mode. If DayZ binds its own action to M, rebind that action in
the game or change `hotkey`.

Other commands:

- `dayz-map toggle | show | hide | quit` control the running overlay.
- `dayz-map status` shows what the overlay sees: game running, map, server, and mods.
- `dayz-map list` shows installed maps and every terrain found in the game and Workshop folders.
- `dayz-map import <world>…` or `--all` builds maps ahead of time.

### Start automatically

Copy `packaging/dayz-map-overlay.desktop` to `~/.config/autostart/` and put the `dayz-map`
binary on your `PATH` (for example `~/.local/bin`).

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
`~/.local/share/dayz-map-overlay/maps/<world>/`, about 40–80 MB and one or two seconds each.

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

`~/.config/dayz-map-overlay/config.toml` is written when the overlay closes:

```toml
hotkey = "m"                                  # a character, f1–f24, or a keysym like 0x6d
window_match = ["steam_app_221100", "DayZ"]   # opens only when the focused window matches
# game_dir = "/path/to/steamapps/common/DayZ" # if Steam auto-detection fails

[view]
map = "chernarusplus"   # last map shown
map_opacity = 0.9
backdrop_opacity = 0.35
show_grid = true
show_places = true

[view.layers]           # point-of-interest layers you switched on or off
civilian = false
```

`DAYZ_MAP_LOG_DIR` points the session watcher at a different log folder (useful for testing
with recorded logs).

## How it works

- **Which map** (`src/game.rs`): DayZ writes `script_<date>.log` in its Proton prefix. Each
  mission start logs `Creating Mission: mpmissions\__cur_mp.<world>\mission.c`; `intro.<world>`
  is the main menu. The RPT log's command line has the launcher's `-connect=ip:port:query` and
  `-mod=` list. A Steam server query (A2S_INFO) fetches the server name, kept only if the server
  reports the same map. If the game runs with `-nologs`, there's nothing to follow.
- **Library** (`src/library.rs`, `src/import/catalog.rs`): scans the game and Workshop folders
  (archive headers only, about half a second for hundreds of mods), imports the map the game is
  on if it's missing or out of date, and rebuilds maps made by older versions.
- **Overlay window** (`src/host/wayland.rs`): a full-screen `wlr-layer-shell` surface on the
  `overlay` layer, which compositors draw above fullscreen windows, placed on the monitor with
  the game window. It never takes keyboard focus, so the game stays the active window and
  keeps running; clicks on it don't activate it either. Its namespace is `on-screen-display`,
  which KWin treats like the volume OSD: it fades in and out rather than animating like a window. Rendering is egui on wgpu (Vulkan).
- **Hotkey** (`src/trigger/`): Proton games draw through XWayland, which only receives keyboard
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

- Every well and other non-loot objects: these need the terrain's full object list from the
  `.wrp`.
- Server-specific loot and events (servers don't share their economy files).
- Windows and X11-only desktops (the UI and importer are portable; they need a window host).
- GNOME, which doesn't support layer-shell.
