mod config;
mod game;
mod host;
mod import;
mod ipc;
mod library;
mod maps;
mod paths;
mod steam;
#[cfg(target_os = "linux")]
mod trigger;
mod ui;
#[cfg(windows)]
mod win;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::config::Config;

/// In-game map overlay for DayZ.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Switch {
    On,
    Off,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the overlay in the background (the default).
    Run {
        /// Open the overlay immediately (useful for testing).
        #[arg(long)]
        show: bool,
        /// Started again after losing the graphics device: keep the log that says why.
        #[arg(long, hide = true)]
        restarted: bool,
    },
    /// Show or hide the running overlay.
    Toggle,
    /// Show the running overlay.
    Show,
    /// Hide the running overlay.
    Hide,
    /// Stop the running overlay.
    Quit,
    /// Start the overlay when you log in (`on`), or stop doing that (`off`).
    Autostart {
        #[arg(value_enum)]
        state: Switch,
    },
    /// List installed maps and the maps found in the game and Workshop folders.
    List,
    /// Show what the game is doing right now (map, server, mods), as the overlay sees it.
    Status,
    /// Import maps from the game and Workshop files (the overlay also does this by itself
    /// when you join a server).
    Import {
        /// World names to import, such as chernarusplus or deerisle.
        worlds: Vec<String>,
        /// Import every map found.
        #[arg(long)]
        all: bool,
    },
    /// Import a picture of a map, for terrains the game-file importer can't read.
    ImportImage {
        /// Short id, such as `namalsk`.
        id: String,
        /// The picture; it must cover the whole terrain with north up.
        picture: PathBuf,
        /// Terrain size in metres, such as 12800.
        #[arg(long)]
        world_size: f64,
        /// Display name (defaults to the id).
        #[arg(long)]
        name: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Cmd::Run {
        show: false,
        restarted: false,
    });
    if let Cmd::Run {
        restarted: true, ..
    } = command
    {
        KEEP_LOG.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let overlay = matches!(command, Cmd::Run { .. });
    // Rust ignores SIGPIPE, so `dayz-map status | head` would panic when `head` exits; quietly
    // stopping is what command-line tools do. The overlay keeps ignoring it, so a closed log
    // pipe can't kill it.
    #[cfg(target_os = "linux")]
    if !overlay {
        // SAFETY: called before any other threads exist.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    let console = !overlay || keep_console();
    let mut logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    let logging_to_file = !console && open_log_file();
    if logging_to_file {
        logger.target(env_logger::Target::Pipe(Box::new(LogFile)));
        // Without a console, panics and errors would otherwise vanish.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("{info}");
            default_hook(info);
        }));
    }
    logger.init();
    let result = run(command);
    if logging_to_file && let Err(e) = &result {
        log::error!("{e:#}");
    }
    result
}

fn run(command: Cmd) -> Result<()> {
    match command {
        Cmd::Run { show, restarted } => host::run(Config::load(), show, restarted),
        Cmd::Toggle => ipc::send(ipc::Command::Toggle),
        Cmd::Show => ipc::send(ipc::Command::Show),
        Cmd::Hide => ipc::send(ipc::Command::Hide),
        Cmd::Quit => ipc::send(ipc::Command::Quit),
        Cmd::List => list(&Config::load()),
        Cmd::Autostart { state } => autostart(matches!(state, Switch::On)),
        Cmd::Status => {
            status(&Config::load());
            Ok(())
        }
        Cmd::Import { worlds, all } => import(&Config::load(), &worlds, all),
        Cmd::ImportImage {
            id,
            picture,
            world_size,
            name,
        } => {
            let pack =
                import::import_image(&id, name.as_deref().unwrap_or(&id), world_size, &picture)?;
            println!("Imported {} into {}", pack.meta.name, pack.dir.display());
            Ok(())
        }
    }
}

/// For the overlay: on Windows, started outside a terminal (double-clicked, or at sign-in), the
/// console window Windows opened is ours alone, so close it and keep running in the background.
/// Returns whether a console remains for the log.
#[cfg(windows)]
fn keep_console() -> bool {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut processes = [0u32; 2];
    // SAFETY: the buffer length is passed along with it.
    let attached = unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 2) };
    if attached > 1 {
        return true;
    }
    // SAFETY: detaching from our own console.
    unsafe { FreeConsole() };
    false
}

#[cfg(not(windows))]
fn keep_console() -> bool {
    true
}

/// Where the log goes without a console: `dayz-map.log` in the data folder (Windows).
static LOG_FILE: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();

/// Opens the log for appending, so a second copy that finds the overlay already running
/// doesn't wipe the running one's log.
fn open_log_file() -> bool {
    let dir = config::data_dir();
    let file = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log_path())
    });
    file.is_ok_and(|file| LOG_FILE.set(file).is_ok())
}

fn log_path() -> PathBuf {
    config::data_dir().join("dayz-map.log")
}

/// Set for a restart, whose log should continue the previous run's.
static KEEP_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Starts the log over; called once this is known to be the only overlay running. (Through a
/// second handle: Windows can't truncate through an append-only one. Appends then continue
/// from the new end.)
pub fn fresh_log() {
    if LOG_FILE.get().is_some() && !KEEP_LOG.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .open(log_path())
            .and_then(|file| file.set_len(0));
    }
}

struct LogFile;

impl std::io::Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        LOG_FILE.get().expect("log file is open").write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        LOG_FILE.get().expect("log file is open").flush()
    }
}

#[cfg(windows)]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "DayZ Map Overlay";

/// Adds or removes the sign-in entry (the `Run` registry key) that starts this program.
#[cfg(windows)]
fn autostart(on: bool) -> Result<()> {
    use win::{HKEY_CURRENT_USER, delete_registry_value, set_registry_string};
    if on {
        let exe = steam::real_path(&std::env::current_exe()?);
        // Running straight from the zip, Explorer extracts to a temporary folder that's
        // cleaned up later, which would leave the sign-in entry pointing at nothing.
        if exe.starts_with(steam::real_path(&std::env::temp_dir())) {
            anyhow::bail!(
                "{} is in a temporary folder; extract the zip to a folder that will stay, \
                 then run `dayz-map autostart on` from there",
                exe.display()
            );
        }
        let command = format!("\"{}\" run", exe.display());
        set_registry_string(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, &command)
            .context("writing the sign-in entry")?;
        println!("The overlay will start when you sign in.");
    } else if delete_registry_value(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE)? {
        println!("Removed the sign-in entry.");
    } else {
        println!("Autostart was off");
    }
    Ok(())
}

/// Adds or removes a login entry (`~/.config/autostart`) that runs this binary.
/// One argument for a desktop entry's `Exec=`, quoted. Two layers: inside the quotes, `"`,
/// `` ` ``, `$` and `\` take a backslash; then the key is a string value, whose own escapes
/// double every backslash. A literal `%` is `%%`.
#[cfg(not(windows))]
fn desktop_exec_arg(arg: &str) -> String {
    let mut out = String::from("\"");
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' => out.extend(['\\', '\\', c]),
            '\\' => out.push_str("\\\\\\\\"),
            '%' => out.push_str("%%"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(not(windows))]
fn autostart(on: bool) -> Result<()> {
    let dirs = directories::BaseDirs::new().context("no home directory")?;
    let entry = dirs.config_dir().join("autostart/dayz-map-overlay.desktop");
    if !on {
        match std::fs::remove_file(&entry) {
            Ok(()) => println!("Removed {}", entry.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("Autostart was off"),
            Err(e) => return Err(e).context(format!("removing {}", entry.display())),
        }
        return Ok(());
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    let quoted = desktop_exec_arg(&exe.to_string_lossy());
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=DayZ Map Overlay\n\
         Comment=Press M in DayZ to open a see-through map\nExec={quoted} run\n\
         Icon=map-flat\nTerminal=false\nNoDisplay=true\nX-KDE-autostart-phase=2\n"
    );
    std::fs::create_dir_all(entry.parent().unwrap())?;
    std::fs::write(&entry, text).with_context(|| format!("writing {}", entry.display()))?;
    println!(
        "The overlay will start when you log in ({}).",
        entry.display()
    );
    Ok(())
}

/// What the overlay found on this machine, and what the game is doing.
fn status(config: &Config) {
    let paths = paths::refresh(config);
    let show = |label: &str, found: &[paths::Located], missing: &str| {
        if found.is_empty() {
            println!("{label:<10}{missing}");
        }
        for (i, f) in found.iter().enumerate() {
            let label = if i == 0 { label } else { "" };
            println!("{label:<10}{} (from {})", f.path.display(), f.how);
        }
    };
    show(
        "DayZ",
        paths.game.as_slice(),
        "not found; set game_dir in the config",
    );
    show(
        "Workshop",
        &paths.workshop,
        "not found (only the base game's maps are available)",
    );
    show(
        "Logs",
        &paths.logs,
        "not found yet (DayZ creates them the first time it runs); set log_dir if this persists",
    );
    println!("{:<10}{}", "Config", config::config_path().display());
    println!("{:<10}{}", "Maps", maps::maps_dir().display());
    for warning in &paths.warnings {
        println!("warning: {warning}");
    }
    let session = game::snapshot();
    let state = match (&session.running, &session.world) {
        (false, _) => "not running".to_string(),
        (true, None) => "main menu".to_string(),
        (true, Some(world)) => {
            let server = session
                .server_name
                .clone()
                .or(session.server.clone())
                .map(|s| format!(" on {s}"))
                .unwrap_or_default();
            format!("playing {world}{server}")
        }
    };
    println!("{:<10}{state}", "Game");
    if !session.mods.is_empty() {
        println!("{:<10}{}", "Mods", session.mods.join(", "));
    }
}

fn scan(config: &Config) -> import::Catalog {
    let paths = paths::refresh(config);
    for warning in &paths.warnings {
        eprintln!("warning: {warning}");
    }
    if paths.game.is_none() {
        eprintln!(
            "DayZ wasn't found in any Steam library; set game_dir in {}.",
            config::config_path().display()
        );
    }
    import::catalog::scan(&import::catalog::roots(&paths))
}

fn list(config: &Config) -> Result<()> {
    println!("Installed maps ({}):", maps::maps_dir().display());
    for pack in maps::load_all() {
        println!(
            "  {:<16} {:<22} {:>6.0} m",
            pack.meta.id, pack.meta.name, pack.meta.world_size
        );
    }
    println!("\nFound in the game and Workshop folders:");
    for world in scan(config).worlds {
        let size = world.world_size.map_or("?".into(), |s| format!("{s:.0} m"));
        let economy = if world.economy.is_some() {
            "loot data"
        } else {
            "no loot data"
        };
        println!(
            "  {:<16} {:<22} {:>8}  {:>3} places  {:<12}  {}",
            world.id,
            world.name,
            size,
            world.places.len(),
            economy,
            world.source_label()
        );
    }
    Ok(())
}

fn import(config: &Config, worlds: &[String], all: bool) -> Result<()> {
    let catalog = scan(config);
    // Stay with the mod an installed map came from.
    let installed_mod = |id: &str| -> Vec<String> {
        maps::load(&maps::maps_dir().join(id.to_lowercase()))
            .ok()
            .and_then(|p| p.meta.mod_id)
            .into_iter()
            .collect()
    };
    let selected: Vec<import::WorldSource> = if all {
        catalog
            .unique()
            .iter()
            .filter_map(|w| catalog.best(&w.id, &installed_mod(&w.id)))
            .collect()
    } else {
        worlds
            .iter()
            .map(|w| {
                catalog.best(w, &installed_mod(w)).with_context(|| {
                    let names: Vec<_> = catalog.unique().iter().map(|w| w.id.clone()).collect();
                    format!("no map named {w}; found: {}", names.join(", "))
                })
            })
            .collect::<Result<_>>()?
    };
    if selected.is_empty() {
        anyhow::bail!("name the maps to import, or pass --all");
    }
    // One damaged map mod mustn't stop the rest.
    let mut failed = Vec::new();
    for world in &selected {
        if let Err(e) = import_one(world) {
            eprintln!();
            eprintln!("{}: {e:#}", world.name);
            failed.push(world.name.clone());
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("couldn't import {}", failed.join(", "));
    }
    Ok(())
}

/// Imports one world, or only rebuilds its points of interest if its tiles are up to date.
fn import_one(world: &import::catalog::WorldSource) -> Result<()> {
    let start = std::time::Instant::now();
    if let Ok(mut pack) = maps::load(&maps::maps_dir().join(&world.id))
        && world.is_current(&pack)
    {
        if pack.meta.pois_source == world.pois_fingerprint() {
            println!("{} is up to date", world.name);
            return Ok(());
        }
        if !import::refresh_pois(world, &mut pack)? {
            println!("{} is up to date", world.name);
            return Ok(());
        }
        println!(
            "{} is up to date; rebuilt its points of interest in {:.0?}",
            world.name,
            start.elapsed()
        );
        return Ok(());
    }
    let last = std::sync::Mutex::new(101);
    let pack = import::import_world(world, &|p| {
        let percent = p.done * 100 / p.total;
        let mut last = last.lock().unwrap();
        if percent != *last {
            *last = percent;
            eprint!("\r{}: {percent:3}%", world.name);
        }
    })?;
    eprintln!();
    println!(
        "Imported {} into {} in {:.0?}",
        world.name,
        pack.dir.display(),
        start.elapsed()
    );
    Ok(())
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn desktop_exec_quoting() {
        assert_eq!(
            super::desktop_exec_arg("/home/a/100% \"ready\"/dayz-map"),
            r#""/home/a/100%% \\"ready\\"/dayz-map""#
        );
        assert_eq!(super::desktop_exec_arg(r"/a\b$c"), r#""/a\\\\b\\$c""#);
    }
}
