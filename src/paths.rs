//! Where DayZ's files are on this machine, found without asking where possible.
//!
//! DayZ is only sold through Steam, so Steam's own library list finds the game. The Workshop
//! folder and the logs then follow from the game's library. Each can also be set in the config
//! (`game_dir`, `log_dir`) for unusual setups, and the overlay offers a folder picker when the
//! game can't be found.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::config::Config;
use crate::steam;

#[derive(Debug, Clone, Default)]
pub struct Paths {
    pub game: Option<Located>,
    /// `<library>/steamapps/workshop/content/221100` folders, one folder per mod inside.
    pub workshop: Vec<Located>,
    /// Folders where DayZ writes `script_*.log` and `*.RPT`.
    pub logs: Vec<Located>,
    /// Problems worth telling the user about, such as a configured folder that doesn't exist.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Located {
    pub path: PathBuf,
    /// How it was found, for `dayz-map status`.
    pub how: &'static str,
}

static CURRENT: RwLock<Option<Arc<Paths>>> = RwLock::new(None);
/// The settings they were found with: a folder the user just picked counts even if saving it
/// to the config file failed.
static SETTINGS: RwLock<Option<Config>> = RwLock::new(None);

/// The paths found most recently (looked up from the saved config the first time).
pub fn current() -> Arc<Paths> {
    if let Some(paths) = CURRENT.read().unwrap().clone() {
        return paths;
    }
    refresh(&Config::load())
}

/// Looks everything up again, for example after the user picks the game folder.
pub fn refresh(config: &Config) -> Arc<Paths> {
    let paths = Arc::new(locate(config));
    *SETTINGS.write().unwrap() = Some(config.clone());
    *CURRENT.write().unwrap() = Some(paths.clone());
    paths
}

/// Looks everything up again with the same settings (Steam may have made a folder since).
pub fn rediscover() -> Arc<Paths> {
    let settings = SETTINGS.read().unwrap().clone();
    refresh(&settings.unwrap_or_else(Config::load))
}

/// True for a DayZ install folder.
pub fn is_game_dir(dir: &Path) -> bool {
    dir.join("Addons").is_dir()
        && (dir.join("DayZ_x64.exe").is_file() || dir.join("DayZ_BE.exe").is_file())
}

pub fn locate(config: &Config) -> Paths {
    let mut paths = Paths::default();
    let libraries = steam::library_folders();

    paths.game = match &config.game_dir {
        Some(dir) if is_game_dir(dir) => Some(Located {
            path: dir.clone(),
            how: "settings",
        }),
        configured => {
            if let Some(dir) = configured {
                paths.warnings.push(format!(
                    "game_dir {} isn't a DayZ folder; looking in Steam instead",
                    dir.display()
                ));
            }
            libraries
                .iter()
                .map(|lib| lib.join("steamapps").join("common").join("DayZ"))
                .find(|dir| is_game_dir(dir))
                .map(|path| Located {
                    path,
                    how: "Steam library",
                })
        }
    };

    // The game's own library first: that's where its Workshop items and Proton prefix live.
    let mut search = Vec::new();
    if let Some(lib) = paths
        .game
        .as_ref()
        .and_then(|g| steam::library_of(&steam::real_path(&g.path)))
    {
        search.push(lib);
    }
    search.extend(libraries);
    let search = steam::dedup_dirs(search);

    paths.workshop = steam::dedup_dirs(
        search
            .iter()
            .map(|lib| {
                lib.join("steamapps")
                    .join("workshop")
                    .join("content")
                    .join(steam::DAYZ_APP)
            })
            .collect(),
    )
    .into_iter()
    .map(|path| Located {
        path,
        how: "Steam library",
    })
    .collect();
    if paths.workshop.is_empty()
        && let Some(game) = &paths.game
    {
        paths.workshop = workshop_from_links(&game.path);
    }

    paths.logs = log_dirs(config, &search, &mut paths.warnings);
    paths
}

/// The game's `!Workshop` folder links each subscribed mod to its Workshop folder; their
/// parents are the Workshop folders (named by item id, which servers list mods by).
fn workshop_from_links(game: &Path) -> Vec<Located> {
    let Ok(entries) = std::fs::read_dir(game.join("!Workshop")) else {
        return Vec::new();
    };
    let parents: Vec<PathBuf> = entries
        .flatten()
        .map(|e| steam::real_path(&e.path()))
        .filter_map(|target| target.parent().map(Path::to_owned))
        .filter(|parent| parent.file_name().is_some_and(|n| n == steam::DAYZ_APP))
        .collect();
    steam::dedup_dirs(parents)
        .into_iter()
        .map(|path| Located {
            path,
            how: "the game's !Workshop links",
        })
        .collect()
}

fn log_dirs(config: &Config, libraries: &[PathBuf], warnings: &mut Vec<String>) -> Vec<Located> {
    if let Some(dir) = std::env::var_os("DAYZ_MAP_LOG_DIR") {
        return vec![Located {
            path: dir.into(),
            how: "DAYZ_MAP_LOG_DIR",
        }];
    }
    if let Some(dir) = &config.log_dir {
        if dir.is_dir() {
            return vec![Located {
                path: dir.clone(),
                how: "settings",
            }];
        }
        warnings.push(format!("log_dir {} doesn't exist", dir.display()));
    }
    default_log_dirs(libraries)
}

/// Proton keeps a Windows user profile per game, in the game's library.
#[cfg(not(windows))]
fn default_log_dirs(libraries: &[PathBuf]) -> Vec<Located> {
    let candidates = libraries
        .iter()
        .map(|lib| {
            lib.join("steamapps/compatdata")
                .join(steam::DAYZ_APP)
                .join("pfx/drive_c/users/steamuser/AppData/Local/DayZ")
        })
        .collect();
    steam::dedup_dirs(candidates)
        .into_iter()
        .map(|path| Located {
            path,
            how: "Proton prefix",
        })
        .collect()
}

#[cfg(windows)]
fn default_log_dirs(_libraries: &[PathBuf]) -> Vec<Located> {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("DayZ"))
        .filter(|d| d.is_dir())
        .map(|path| Located {
            path,
            how: "%LOCALAPPDATA%",
        })
        .into_iter()
        .collect()
}
