//! Watches for the hotkey while the game has focus.
//!
//! DayZ runs through Proton, which draws through XWayland. XWayland only receives keyboard input
//! while one of its windows is focused, and then shares it with any X11 client that asks for raw
//! XInput2 events. So listening there sees the key exactly when it's typed into the game, without
//! grabbing it (the game still gets it) and without root access to input devices.

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::config::Config;

/// XIAllDevices. XWayland reports raw events from the physical (slave) devices only.
const ALL_DEVICES: u16 = 0;

/// How often to try again while XWayland isn't there: not started yet when the overlay
/// autostarts, or restarting after a crash (Wine can take it down).
const RECONNECT: std::time::Duration = std::time::Duration::from_secs(3);

/// Starts the listener thread; `on_press` runs for each hotkey press in a matching window, with
/// the centre of that window in global screen coordinates (to open the overlay on its monitor).
/// `on_unfocus` runs when focus moves from a matching window to anything else. Fails only for a
/// hotkey that can't be understood; without XWayland it keeps trying in the background.
pub fn spawn(
    config: &Config,
    on_press: impl Fn(Option<(i32, i32)>) + Send + 'static,
    on_unfocus: impl Fn() + Send + 'static,
) -> Result<()> {
    let keysym = parse_keysym(&config.hotkey)?;
    let name = config.hotkey.clone();
    let patterns: Vec<String> = config
        .window_match
        .iter()
        .map(|p| p.to_lowercase())
        .collect();
    let matches =
        move |window: &str| patterns.is_empty() || patterns.iter().any(|p| window.contains(p));
    // The first try here, so a problem shows in the log before anything else happens.
    let mut next = Listener::connect(keysym, &name);
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let mut failing: Option<String> = None;
            loop {
                match next {
                    Ok(listener) => {
                        failing = None;
                        log::info!("listening for the hotkey");
                        let e = listener.listen(&matches, &on_press, &on_unfocus);
                        log::warn!("lost the XWayland connection: {e:#}; reconnecting");
                    }
                    Err(e) => {
                        // Once per distinct problem, not every few seconds.
                        let text = format!("{e:#}");
                        if failing.as_ref() != Some(&text) {
                            log::warn!(
                                "hotkey not available yet: {text}. Trying again every few \
                                 seconds; `dayz-map toggle` works meanwhile."
                            );
                            failing = Some(text);
                        }
                    }
                }
                std::thread::sleep(RECONNECT);
                next = Listener::connect(keysym, &name);
            }
        })?;
    Ok(())
}

/// A connection to XWayland set up to report the hotkey and focus changes.
struct Listener {
    conn: RustConnection,
    root: Window,
    hotkey: Vec<u32>,
    /// Modifier bits that stop the hotkey (Ctrl, Alt, Super).
    blocking: u16,
    atoms: Atoms,
}

impl Listener {
    fn connect(keysym: u32, name: &str) -> Result<Self> {
        let (conn, screen) = RustConnection::connect(None).context("connecting to XWayland")?;
        let root = conn.setup().roots[screen].root;
        let version = conn
            .xinput_xi_query_version(2, 2)?
            .reply()
            .context("XInput2 is not available")?;
        log::debug!("XInput {}.{}", version.major_version, version.minor_version);

        let keycodes = Keycodes::load(&conn)?;
        let hotkey = keycodes.find(keysym);
        log::debug!("hotkey keycodes {hotkey:?}");
        if hotkey.is_empty() {
            bail!("no key on the keyboard produces {name:?}");
        }
        // Ctrl, Alt and Super combinations are left alone; Shift is allowed (it's sprint in
        // DayZ).
        let blocking: Vec<u32> = [0xffe3, 0xffe4, 0xffe9, 0xffea, 0xffeb, 0xffec, 0xfe03]
            .into_iter()
            .flat_map(|k| keycodes.find(k))
            .collect();
        let blocking = modifier_mask(&conn, &blocking)?;

        conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: ALL_DEVICES,
                mask: vec![xinput::XIEventMask::RAW_KEY_PRESS],
            }],
        )?
        .check()
        .context("selecting XInput2 raw key events")?;
        let atoms = Atoms::new(&conn)?;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()
        .context("watching the focused window")?;
        Ok(Self {
            conn,
            root,
            hotkey,
            blocking,
            atoms,
        })
    }

    /// Handles events until the connection breaks.
    fn listen(
        &self,
        matches: &dyn Fn(&str) -> bool,
        on_press: &dyn Fn(Option<(i32, i32)>),
        on_unfocus: &dyn Fn(),
    ) -> anyhow::Error {
        let Self {
            conn,
            root,
            hotkey,
            blocking,
            atoms,
        } = self;
        let (root, blocking) = (*root, *blocking);
        let mut game_focused = matches(&atoms.active_window(conn, root).0);
        loop {
            let event = match conn.wait_for_event() {
                Ok(event) => event,
                Err(e) => return e.into(),
            };
            match event {
                // A master device repeats its slave's event; count each press once.
                Event::XinputRawKeyPress(e) if e.deviceid == e.sourceid => {
                    let repeat = e.flags.contains(xinput::KeyEventFlags::KEY_REPEAT);
                    if !hotkey.contains(&e.detail) || repeat {
                        continue;
                    }
                    // Ask the server rather than tracking presses: a release made while
                    // another window had focus (Alt+Tab out of the game) is never seen here.
                    let mods = conn
                        .query_pointer(root)
                        .ok()
                        .and_then(|c| c.reply().ok())
                        .map_or(0, |r| u16::from(r.mask));
                    if mods & blocking != 0 {
                        log::info!("hotkey ignored: Ctrl, Alt or Super is held");
                        continue;
                    }
                    let (window, center) = atoms.active_window(conn, root);
                    let matches = matches(&window);
                    log::info!(
                        "hotkey in {window:?}: {}",
                        if matches {
                            "toggling the map"
                        } else {
                            "not the game"
                        }
                    );
                    if matches {
                        on_press(center);
                    }
                }
                Event::PropertyNotify(e) if e.atom == atoms.active_window => {
                    let focused = matches(&atoms.active_window(conn, root).0);
                    if game_focused && !focused {
                        on_unfocus();
                    }
                    game_focused = focused;
                }
                _ => {}
            }
        }
    }
}

/// The modifier-state bits (Mod1, Mod4, ...) that any of `keycodes` sets.
fn modifier_mask(conn: &RustConnection, keycodes: &[u32]) -> Result<u16> {
    let reply = conn.get_modifier_mapping()?.reply()?;
    let per_modifier = usize::from(reply.keycodes_per_modifier()).max(1);
    let mut mask = 0;
    for (bit, codes) in reply.keycodes.chunks(per_modifier).enumerate() {
        if codes
            .iter()
            .any(|&c| c != 0 && keycodes.contains(&u32::from(c)))
        {
            mask |= 1 << bit;
        }
    }
    Ok(mask)
}

/// Accepts a single character (`m`), a function key (`f1`), or a raw keysym (`0x6d`).
fn parse_keysym(name: &str) -> Result<u32> {
    let lower = name.trim().to_lowercase();
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_graphic()
    {
        return Ok(c as u32);
    }
    if let Some(n) = lower
        .strip_prefix('f')
        .and_then(|n| n.parse::<u32>().ok())
        .filter(|n| (1..=24).contains(n))
    {
        return Ok(0xffbe + n - 1);
    }
    if let Some(hex) = lower.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).context("invalid keysym");
    }
    match lower.as_str() {
        "tab" => Ok(0xff09),
        "grave" | "backtick" => Ok(0x60),
        _ => {
            bail!("unknown hotkey {name:?}; use a single character, f1-f24, or a keysym like 0x6d")
        }
    }
}

struct Keycodes {
    min: u8,
    per_keycode: usize,
    keysyms: Vec<u32>,
}

impl Keycodes {
    fn load(conn: &RustConnection) -> Result<Self> {
        let setup = conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
        Ok(Self {
            min,
            per_keycode: reply.keysyms_per_keycode as usize,
            keysyms: reply.keysyms,
        })
    }

    /// Keycodes whose unshifted or shifted symbol is `keysym` (letters match either case) in
    /// the first layout, or else the second: with `ru,us`, the Latin letters are in the second.
    fn find(&self, keysym: u32) -> Vec<u32> {
        let lower = char::from_u32(keysym).map_or(keysym, |c| c.to_ascii_lowercase() as u32);
        let in_layout = |layout: usize| -> Vec<u32> {
            self.keysyms
                .chunks(self.per_keycode.max(1))
                .enumerate()
                .filter(|(_, syms)| {
                    syms.iter()
                        .skip(layout * 2)
                        .take(2)
                        .any(|&s| s == keysym || s == lower)
                })
                .map(|(i, _)| u32::from(self.min) + i as u32)
                .collect()
        };
        let first = in_layout(0);
        if first.is_empty() {
            in_layout(1)
        } else {
            first
        }
    }
}

struct Atoms {
    active_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
}

impl Atoms {
    fn new(conn: &RustConnection) -> Result<Self> {
        let atom =
            |name: &[u8]| -> Result<u32> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        Ok(Self {
            active_window: atom(b"_NET_ACTIVE_WINDOW")?,
            net_wm_name: atom(b"_NET_WM_NAME")?,
            utf8_string: atom(b"UTF8_STRING")?,
        })
    }

    /// Lower-cased "class instance title" of the focused X11 window (or empty), and its centre.
    fn active_window(&self, conn: &RustConnection, root: Window) -> (String, Option<(i32, i32)>) {
        let property = |window: Window, name: u32, kind: u32| -> Option<Vec<u8>> {
            let reply = conn
                .get_property(false, window, name, kind, 0, 1024)
                .ok()?
                .reply()
                .ok()?;
            Some(reply.value)
        };
        let Some(window) = conn
            .get_property(false, root, self.active_window, AtomEnum::WINDOW, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .filter(|&w| w != 0)
        else {
            return (String::new(), None);
        };
        let center = conn
            .get_geometry(window)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|geometry| {
                let origin = conn
                    .translate_coordinates(window, root, 0, 0)
                    .ok()?
                    .reply()
                    .ok()?;
                Some((
                    i32::from(origin.dst_x) + i32::from(geometry.width) / 2,
                    i32::from(origin.dst_y) + i32::from(geometry.height) / 2,
                ))
            });
        let mut parts = Vec::new();
        if let Some(class) = property(window, AtomEnum::WM_CLASS.into(), AtomEnum::STRING.into()) {
            parts.extend(
                class
                    .split(|&b| b == 0)
                    .map(|s| String::from_utf8_lossy(s).into_owned()),
            );
        }
        let title = property(window, self.net_wm_name, self.utf8_string)
            .filter(|t| !t.is_empty())
            .or_else(|| property(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()));
        if let Some(title) = title {
            parts.push(String::from_utf8_lossy(&title).into_owned());
        }
        (parts.join(" ").trim().to_lowercase(), center)
    }
}
