# Server browser plan

Draft, 2026-10-08. A server browser and launcher in `dayz-map`, working with the map overlay.

## What it is

- A normal window that stays open after you join a server. Closing it quits it.
- The overlay keeps running as its own background process. The browser starts it if it isn't
  running and the overlay setting is on (the default).
- When you join through the browser, it hands the overlay the server's map, name, address, and
  mods before DayZ starts. The overlay can then build the map early and show the server name at
  once. The game's log still decides: if it reports another map, the hint is dropped, and the
  hint ends when DayZ exits. Joining any other way works as it does now.
- If you close the browser mid-game, the overlay keeps working from the log.

## Settings (in the browser)

- Extra launch arguments, added to every launch. A warning appears if they include ones the
  browser sets itself (`-connect`, `-mod`, `-password`). Quoted values are allowed.
- Map overlay on or off (on by default).
- Overlay hotkey, picked by pressing the key. It's written to the shared config, and the
  running overlay reloads it.
- Optional Steam Web API key, used to get the server list from Steam instead of DZSA. It's
  stored in the system keyring (KWallet or GNOME Keyring, Windows Credential Manager), with a
  user-only file as a fallback. It never goes in the config file, logs, or error messages; the
  request URL contains the key, so errors are scrubbed.

## Server list

- **DZSA (default):** `GET https://dayzsalauncher.com/api/v1/launcher/servers/dayz`, about
  10,000 servers, 17.8 MB raw and 1.4 MB with brotli, in about a second. Each entry includes
  mods with Workshop ids. There's no documentation, terms, or rate limit, so:
  - send `Accept-Encoding: br, gzip`, a clear User-Agent, and a 30 s timeout;
  - cache on disk, refresh only when you ask (with a cooldown) or after 5 minutes or more, and
    fall back to the cache;
  - parse leniently, since fields can be missing or null;
  - ignore `sponsor` (do our own sorting) and show the server's real name rather than
    `nameOverride`.
- **Steam (with your key):** `IGameServersService/GetServerList` with `\appid\221100`. Each call
  returns at most about 10,000 servers, so split it the way DZGUI does: map × empty/not empty,
  about 10 calls, then remove duplicates by address. That's far under 100,000 calls a day.
  There are no mods in it, so get them from the server itself (A2S_RULES) when one is selected.
- **Live data:** use A2S_INFO for player count, time, and queue on rows you can see.
- **Before joining:** take a fresh mod list from the server's A2S_RULES in both modes.

## Reading a server's mods (A2S_RULES)

Write our own decoder; the references are MIT (`dayzquery`, `WoozyMasta/a2s`), and DZGUI is
GPL, so copy none of it.

1. Handle the challenge reply (`0x41`): send the request again with the challenge appended.
   Handle split packets, including bzip2.
2. Join the rules whose key is 2 bytes (chunk number, total) in order.
3. Unescape in one pass: `01 01`→`01`, `01 02`→`00`, `01 03`→`FF`.
4. Parse: version, overflow flags, DLC flags and hashes, then the mods (hash, Workshop id,
   name), signatures, and description.
5. Treat the overflow flag as "may be cut off". Never panic on bad data, and clean control
   characters out of names.

## Ping and location

- Ping is the A2S_INFO round trip on the query port. Time the request that gets the real reply,
  not the challenge exchange. Only rows on screen, at most about 32 to 64 at a time, with a
  0.5 to 1 s timeout. Not ICMP.
- Country comes from the DB-IP Lite database (CC BY 4.0, so the app needs a credit line). This
  can come later.

## Joining

### Linux (Steam, usually through Proton)

- Run `steam -applaunch 221100 -nolauncher -connect=ip:gameport [-password=…] -mod=… <extra>`.
  This is what DZGUI, dayz-ctl, and dayz-linux-cli-launcher all do. Flatpak Steam uses
  `flatpak run com.valvesoftware.Steam …`.
- Steam on Linux hangs when `-applaunch` arguments go past about 1,000 characters
  (steam-for-linux#5753). The fix is short `@name` links in the DayZ folder pointing at the
  Workshop folders, which we create and clean up on each launch. Something to test: DayZ's
  `-par=<file>` parameter file, which would avoid the limit entirely.
- The overlay already maps `-mod` entries back to Workshop ids, so check it follows our links.

### Windows

- Start `DayZ_BE.exe -exe DayZ_x64.exe …` directly, with full Workshop paths in `-mod`. That's
  what the official launcher and DZSA do. Windows has no Steam length problem here, so no links
  are needed.
- Fallback: `steam.exe -applaunch 221100 -nolauncher …` with junctions.

### Mods: missing or out of date

- **Is it up to date?** Compare Steam's `steamapps/workshop/appworkshop_221100.acf`
  (`timeupdated` per item) with `ISteamRemoteStorage/GetPublishedFileDetails`, which needs no
  key.
- **Getting it**, best first:
  1. **Steamworks helper:** a short-lived helper process using the `steamworks` crate as app
     221100. It subscribes, downloads, and reports progress through Steam's own Workshop
     interface, which is what the official launcher does, with no key needed. While it runs,
     Steam thinks DayZ is running, so the helper has to exit before launch. Valve's
     `libsteam_api.so` / `steam_api64.dll` go in the release downloads, not the git repo
     (Steamworks SDK agreement §1.1(b)); on Windows, DayZ's own `steam_api64.dll` could be
     used instead.
  2. **With your key:** the Web API `IPublishedFileService/Subscribe` call with
     `notify_client=1` (what DZGUI does; not documented by Valve).
  3. **Always available:** open the mod's Workshop page in Steam.
- Warn if Steam's "allow downloads during gameplay" is off.

## Building it

- **Window:** eframe/egui, the same toolkit as the overlay. A virtual-scrolling table copes
  with 10,000 rows.
- **New dependencies:**
  - `eframe` (adds winit on Linux)
  - `ureq` (HTTP, rustls, gzip/brotli)
  - `keyring`
  - `steamworks` (helper only)
- **Command:** `dayz-map browse` and a desktop shortcut. Overlay autostart stays as it is.
- **IPC:** new commands to set the server hint and to reload the config.

## Phases

0. **Prove the risky parts** (needs DayZ on the desktop, done together):
   - `steam -applaunch … -nolauncher` with linked mods under Proton, BattlEye included;
   - whether `-par=` works;
   - the steamworks helper (subscribe and download a small mod, Steam's status clears when it
     exits, native and Flatpak Steam);
   - Windows: `DayZ_BE.exe` launched directly (the user's dual boot; the VM has no DayZ).
1. **Browse:** the DZSA list, cache, table, search, filters (map, players, first-person,
   password, mods, ping, version), details panel with the mod list, favorites, history.
2. **Join:** mod check, launching on Linux and Windows, password prompt, extra arguments,
   overlay handoff, overlay on/off, and the hotkey setting.
3. **Missing mods:** the Steamworks helper with progress, and Workshop pages as the fallback.
4. **Steam key mode:** keyring, GetServerList split, and the A2S_RULES decoder (also used
   before every join in phase 2, so it may move earlier).
5. **Polish:** country, queue display, README, release.

## Risks

- DZSA can change or block its API at any time; Steam key mode is the way around that.
- Steam's GetServerList endpoint and its cap aren't documented.
- How `-nolauncher` works isn't documented.
- Valve's redistribution terms for the Steamworks library in an open-source project.
- Steamworks against Flatpak Steam is untested.
- Detecting when a download has finished (Steamworks item state should solve it).
- Steam passes arguments to an already-running instance and sometimes doesn't launch.
