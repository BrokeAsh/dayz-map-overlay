//! Works out which map DayZ is on by following its logs.
//!
//! DayZ writes `script_<date>.log` and `DayZ_x64_<date>.RPT` into its profile folder
//! (`%LOCALAPPDATA%\DayZ`, inside the Proton prefix on Linux). Each time it starts a mission
//! the script log gets a line like
//! `Creating Mission: mpmissions\__cur_mp.deerisle\mission.c`: the mission folder is named
//! `<mission>.<world>`, and `intro.<world>` is the main-menu background. The RPT's third line is
//! the game's command line, with `-connect=ip:port:queryport` and `-mod=...` when a launcher
//! starts it, which gives the server address and its mods.

use std::io::{Read, Seek, SeekFrom};
use std::net::{ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub running: bool,
    /// Lower-case world name while in a mission (on a server or offline).
    pub world: Option<String>,
    /// `ip:port` from the launch command.
    pub server: Option<String>,
    pub server_name: Option<String>,
    /// Workshop ids (or `@folder` names) of the loaded mods.
    pub mods: Vec<String>,
}

const POLL: Duration = Duration::from_secs(1);
/// How often to look for the logs folder again while it's missing.
const RELOCATE: Duration = Duration::from_secs(10);
/// How much of the script log to read at a time (and of an existing one, from its end).
const LOG_TAIL: u64 = 4 << 20;
/// How long to wait before asking a server that didn't answer for its name again.
const QUERY_RETRY: Duration = Duration::from_secs(30);

pub fn spawn(on_change: impl Fn(Session) + Send + 'static) {
    std::thread::Builder::new()
        .name("session".into())
        .spawn(move || {
            let mut watcher = Watcher::default();
            let mut last = Session::default();
            loop {
                let session = watcher.poll();
                if session != last {
                    log::info!(
                        "game: {}",
                        match (&session.running, &session.world) {
                            (false, _) => "not running".to_string(),
                            (true, None) => "main menu".to_string(),
                            (true, Some(w)) => format!(
                                "on {w}{}",
                                session
                                    .server_name
                                    .as_ref()
                                    .map(|n| format!(" ({n})"))
                                    .unwrap_or_default()
                            ),
                        }
                    );
                    on_change(session.clone());
                    last = session;
                }
                std::thread::sleep(POLL);
            }
        })
        .expect("spawning the session watcher");
}

/// The game's current state, read once.
pub fn snapshot() -> Session {
    Watcher::default().poll()
}

#[derive(Default)]
struct Watcher {
    script: Option<PathBuf>,
    offset: u64,
    partial: String,
    rpt: Option<PathBuf>,
    query_port: Option<u16>,
    session: Session,
    /// The last server-name query: its (server, map), the name, and when it ran.
    queried: Option<((String, String), Option<String>, Instant)>,
    relocated: Option<Instant>,
}

impl Watcher {
    fn poll(&mut self) -> Session {
        if !dayz_running() {
            *self = Self::default();
            return Session::default();
        }
        self.session.running = true;
        let mut paths = crate::paths::current();
        // The logs folder appears the first time the game runs.
        if paths.logs.is_empty() && self.relocated.is_none_or(|t| t.elapsed() > RELOCATE) {
            self.relocated = Some(Instant::now());
            paths = crate::paths::refresh(&crate::config::Config::load());
        }
        let dirs: Vec<PathBuf> = paths.logs.iter().map(|l| l.path.clone()).collect();

        if let Some(rpt) = newest(&dirs, "DayZ_x64_", ".RPT")
            && self.rpt.as_ref() != Some(&rpt)
            // The game may not have written the command line yet; try again next time.
            && let Some((server, query, mods)) =
                launch_options(&rpt, paths.game.as_ref().map(|g| g.path.as_path()))
        {
            self.session.server = server;
            self.session.mods = mods;
            self.session.server_name = None;
            self.queried = None;
            self.rpt = Some(rpt);
            self.query_port = query;
        }

        if let Some(script) = newest(&dirs, "script_", ".log") {
            if self.script.as_ref() != Some(&script) {
                self.script = Some(script.clone());
                self.offset = 0;
                self.partial.clear();
                self.session.world = None;
            }
            for line in self.read_new_lines(&script) {
                if let Some(world) = mission_world(&line) {
                    self.session.world = world;
                }
            }
        }

        self.update_server_name();
        self.session.clone()
    }

    fn read_new_lines(&mut self, path: &Path) -> Vec<String> {
        let Ok(mut file) = std::fs::File::open(path) else {
            return Vec::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        // A long modded session's log can be hundreds of MB: start at its latest mission.
        if self.offset == 0 && len > LOG_TAIL {
            self.offset = latest_mission(&mut file, len);
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        if file.take(LOG_TAIL).read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        let complete = self.partial.rfind('\n').map_or(0, |i| i + 1);
        let lines = self.partial[..complete]
            .lines()
            .map(str::to_string)
            .collect();
        self.partial.drain(..complete);
        lines
    }

    /// Asks the server for its name once per server and map (again later if it didn't answer),
    /// and only keeps it if the server reports the map the game is on (the launch address is
    /// stale if the player switched servers in game).
    fn update_server_name(&mut self) {
        let (Some(server), Some(world), Some(query)) =
            (&self.session.server, &self.session.world, self.query_port)
        else {
            self.session.server_name = None;
            return;
        };
        let key = (server.clone(), world.clone());
        if let Some((queried, name, at)) = &self.queried
            && *queried == key
            && (name.is_some() || at.elapsed() < QUERY_RETRY)
        {
            // Also restores the name after a trip to the main menu.
            self.session.server_name = name.clone();
            return;
        }
        let host = server.rsplit_once(':').map_or(server.as_str(), |(h, _)| h);
        let name = match a2s_info(host, query) {
            Some((name, map)) if map.eq_ignore_ascii_case(world) => Some(name),
            Some((_, map)) => {
                log::debug!("launch server reports {map}, not {world}; ignoring its name");
                None
            }
            None => None,
        };
        self.queried = Some((key, name.clone(), Instant::now()));
        self.session.server_name = name;
    }
}

/// Where to start reading an existing script log: the line with its latest mission (searching
/// backward a chunk at a time, so memory stays bounded), or the end if there's none.
fn latest_mission(file: &mut std::fs::File, len: u64) -> u64 {
    const MARK: &[u8] = b"Creating Mission:";
    let mut end = len;
    loop {
        let start = end.saturating_sub(LOG_TAIL);
        let mut chunk = vec![0; (end - start) as usize];
        if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut chunk).is_err() {
            return len;
        }
        if let Some(at) = chunk.windows(MARK.len()).rposition(|w| w == MARK) {
            // The start of that line (or of the chunk: the rest of the line still parses).
            return chunk[..at]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(start, |nl| start + nl as u64 + 1);
        }
        if start == 0 {
            return len;
        }
        // Overlap the chunks so a mark split between them is still found.
        end = start + MARK.len() as u64;
    }
}

/// `mpmissions\__cur_mp.deerisle\mission.c` -> `Some(Some("deerisle"))`; the main-menu intro
/// mission -> `Some(None)`; anything else -> `None`.
fn mission_world(line: &str) -> Option<Option<String>> {
    let path = line.split_once("Creating Mission:")?.1.trim();
    let folder = path
        .trim_end_matches(|c| c != '\\' && c != '/')
        .trim_end_matches(['\\', '/']);
    let folder = folder.rsplit(['\\', '/']).next()?.to_lowercase();
    let (mission, world) = folder.rsplit_once('.')?;
    if !crate::maps::valid_id(world) {
        return None;
    }
    Some((mission != "intro" && !world.is_empty()).then(|| world.to_string()))
}

/// Server address, query port, and mod ids from the RPT's command-line line; `None` until the
/// game has written enough of the file to tell.
fn launch_options(
    rpt: &Path,
    game: Option<&Path>,
) -> Option<(Option<String>, Option<u16>, Vec<String>)> {
    let mut head = String::new();
    if let Ok(file) = std::fs::File::open(rpt) {
        let _ = file.take(64 * 1024).read_to_string(&mut head);
    }
    // Only whole lines: the last one may still be being written.
    let complete = &head[..head.rfind('\n').map_or(0, |i| i + 1)];
    let Some(line) = complete
        .lines()
        .take(10)
        .find(|l| l.contains("-connect=") || l.contains("-mod="))
    else {
        // Started without a server or mods, or not written yet.
        return (complete.lines().count() >= 10).then_some((None, None, Vec::new()));
    };
    let args = arguments(line);
    let option = |name: &str| -> Option<&str> { args.iter().find_map(|a| a.strip_prefix(name)) };
    let mut server = None;
    let mut query = None;
    if let Some(connect) = option("-connect=") {
        let parts: Vec<&str> = connect.split(':').collect();
        let port = parts.get(1).copied().or_else(|| option("-port="));
        server = port
            .map(|p| format!("{}:{p}", parts[0]))
            .or(Some(parts[0].to_string()));
        query = parts.get(2).and_then(|q| q.parse().ok());
    }
    let mods = option("-mod=")
        .map(|list| {
            list.split(';')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .filter_map(|m| mod_id(m, game))
                .collect()
        })
        .unwrap_or_default();
    Some((server, query, mods))
}

/// The id the catalog knows a `-mod=` entry by. Launchers pass links such as
/// `!Workshop\@Deer Isle`, relative to the game folder, that point at the Workshop item's folder
/// (`workshop/content/221100/<item id>`), so follow them; otherwise use the folder name.
fn mod_id(entry: &str, game: Option<&Path>) -> Option<String> {
    let name = entry.rsplit(['\\', '/']).next()?.to_string();
    let join = |base: PathBuf, rest: &str| {
        rest.split(['\\', '/'])
            .filter(|c| !c.is_empty())
            .fold(base, |p, c| p.join(c))
    };
    // Under Proton, Windows paths on drive Z: are the Linux file system.
    let proton = if cfg!(windows) {
        None
    } else {
        entry
            .strip_prefix(['Z', 'z'])
            .and_then(|r| r.strip_prefix(":\\").or_else(|| r.strip_prefix(":/")))
    };
    let path = if let Some(rest) = proton {
        Some(join(PathBuf::from("/"), rest))
    } else if Path::new(entry).is_absolute() {
        Some(PathBuf::from(entry))
    } else if entry.contains(':') {
        None // another drive under Proton
    } else {
        game.map(|game| join(game.to_path_buf(), entry))
    };
    if let Some(path) = path.filter(|p| p.exists())
        && let Some(real) = crate::steam::real_path(&path).file_name()
    {
        return Some(real.to_string_lossy().into_owned());
    }
    (!name.is_empty()).then_some(name)
}

/// A command line's arguments: split at spaces outside double quotes, which are removed. An
/// unquoted piece that doesn't start with `-` continues the argument before it, as in
/// `-mod=!Workshop\@Deer Isle` written without quotes.
fn arguments(line: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let (mut in_quotes, mut quoted) = (false, false);
    for c in line.chars().chain([' ']) {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                quoted = true;
            }
            c if c.is_whitespace() && !in_quotes => {
                if current.is_empty() && !quoted {
                    continue;
                }
                let piece = std::mem::take(&mut current);
                match args.last_mut() {
                    Some(last) if !quoted && !piece.starts_with('-') => {
                        last.push(' ');
                        last.push_str(&piece);
                    }
                    _ => args.push(piece),
                }
                quoted = false;
            }
            c => current.push(c),
        }
    }
    args
}

fn newest(dirs: &[PathBuf], prefix: &str, suffix: &str) -> Option<PathBuf> {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(prefix) && name.ends_with(suffix)
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(modified, _): &(SystemTime, PathBuf)| *modified)
        .map(|(_, path)| path)
}

#[cfg(windows)]
fn dayz_running() -> bool {
    crate::win::process_running("DayZ_x64.exe")
}

#[cfg(not(windows))]
fn dayz_running() -> bool {
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return false;
    };
    procs.flatten().any(|p| {
        p.file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
            && std::fs::read(p.path().join("cmdline")).is_ok_and(|c| {
                c.windows(12)
                    .any(|w| w.eq_ignore_ascii_case(b"DayZ_x64.exe"))
            })
    })
}

/// Steam server query (A2S_INFO): returns the server name and map.
fn a2s_info(host: &str, port: u16) -> Option<(String, String)> {
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let request = b"\xff\xff\xff\xffTSource Engine Query\0";
    socket.send_to(request, addr).ok()?;
    let mut buf = [0u8; 1400];
    let mut len = socket.recv(&mut buf).ok()?;
    if len >= 9 && buf[4] == 0x41 {
        // Challenge: repeat the request with the token appended.
        let mut retry = request.to_vec();
        retry.extend_from_slice(&buf[5..9]);
        socket.send_to(&retry, addr).ok()?;
        len = socket.recv(&mut buf).ok()?;
    }
    let data = &buf[..len];
    if data.len() < 6 || data[4] != 0x49 {
        return None;
    }
    let mut fields = data[6..]
        .split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned());
    Some((fields.next()?, fields.next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn workshop_links() {
        let root = std::env::temp_dir().join(format!("dzm-test-{}", std::process::id()));
        let item = root.join("workshop/content/221100/1602372402");
        let links = root.join("DayZ/!Workshop");
        std::fs::create_dir_all(&item).unwrap();
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(&item, links.join("@Deer Isle")).unwrap();
        let game = root.join("DayZ");
        let id = |entry| mod_id(entry, Some(&game));
        assert_eq!(id(r"!Workshop\@Deer Isle").as_deref(), Some("1602372402"));
        assert_eq!(id("@CF").as_deref(), Some("@CF"));
        assert_eq!(
            id(r"Z:\steam\workshop\content\221100\1559212036").as_deref(),
            Some("1559212036")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mission_far_back_in_a_long_log() {
        let path = std::env::temp_dir().join(format!("dzm-log-{}.log", std::process::id()));
        let mission = "SCRIPT : Creating Mission: mpmissions\\__cur_mp.namalsk\\mission.c\n";
        let filler = "SCRIPT : something else happened\n".repeat(200_000); // ~6.6 MB
        std::fs::write(&path, format!("start\n{mission}{filler}")).unwrap();
        let mut watcher = Watcher::default();
        let mut world = None;
        loop {
            let lines = watcher.read_new_lines(&path);
            if lines.is_empty() {
                break;
            }
            for line in lines {
                if let Some(w) = mission_world(&line) {
                    world = w;
                }
            }
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(world.as_deref(), Some("namalsk"));
    }

    #[test]
    fn launch_arguments() {
        let line = r#"Command line: "C:\Games\DayZ\DayZ_x64.exe" "-connect=1.2.3.4:2302:27016" "-mod=!Workshop\@Deer Isle;@CF" -nolauncher"#;
        let args = arguments(line);
        assert!(args.contains(&"-connect=1.2.3.4:2302:27016".to_string()));
        assert!(args.contains(&r"-mod=!Workshop\@Deer Isle;@CF".to_string()));
        let unquoted =
            r"DayZ_x64.exe -connect=1.2.3.4:2302 -mod=!Workshop\@Deer Isle;@CF -port=2302";
        assert!(arguments(unquoted).contains(&r"-mod=!Workshop\@Deer Isle;@CF".to_string()));
    }

    #[test]
    fn missions() {
        let joined =
            r"18:34:44.1 SCRIPT       : Creating Mission: mpmissions\__cur_mp.deerisle\mission.c";
        assert_eq!(mission_world(joined), Some(Some("deerisle".into())));
        let menu = r"SCRIPT       : Creating Mission: dz\Worlds\ChernarusPlus\data\scenes\intro.ChernarusPlus\mission.c";
        assert_eq!(mission_world(menu), Some(None));
        assert_eq!(mission_world("SCRIPT : something else"), None);
    }
}
