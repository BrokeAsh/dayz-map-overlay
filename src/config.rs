use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Key that opens the overlay while the game has focus (an X11 keysym name such as `m`).
    pub hotkey: String,
    /// The overlay only opens when the focused window's class or title contains one of these
    /// (case-insensitive); one ending in `.exe` must be the program itself (on Linux, its window
    /// class). Empty means any window.
    pub window_match: Vec<String>,
    /// DayZ install folder; found through Steam when unset.
    pub game_dir: Option<PathBuf>,
    /// Folder where DayZ writes its logs (`script_*.log`), used to tell which server you're on;
    /// found next to the game when unset.
    pub log_dir: Option<PathBuf>,
    pub view: ViewConfig,
    /// Set when the file couldn't be read, so saving keeps a copy instead of silently
    /// replacing the user's settings with defaults.
    #[serde(skip)]
    unreadable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewConfig {
    pub map: Option<String>,
    /// Opacity of the map itself.
    pub map_opacity: f32,
    /// How much to darken the game behind the map.
    pub backdrop_opacity: f32,
    pub show_grid: bool,
    pub show_places: bool,
    /// Point-of-interest layers the user turned on or off (by id); others use their defaults.
    pub layers: std::collections::BTreeMap<String, bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "m".into(),
            // Not the title "DayZ": a browser tab or chat channel could match. On Windows the
            // game's executable; on Linux its window class under Proton, and under plain Wine.
            window_match: if cfg!(windows) {
                vec!["dayz_x64.exe".into()]
            } else {
                vec!["steam_app_221100".into(), "dayz_x64.exe".into()]
            },
            game_dir: None,
            log_dir: None,
            view: ViewConfig::default(),
            unreadable: false,
        }
    }
}

impl Default for ViewConfig {
    fn default() -> Self {
        Self {
            map: None,
            map_opacity: 0.9,
            backdrop_opacity: 0.35,
            show_grid: true,
            show_places: true,
            layers: Default::default(),
        }
    }
}

fn dirs() -> directories::ProjectDirs {
    directories::ProjectDirs::from("", "", "dayz-map-overlay").expect("home directory is known")
}

pub fn data_dir() -> PathBuf {
    dirs().data_dir().to_owned()
}

pub fn config_path() -> PathBuf {
    dirs().config_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            // Windows editors may start the file with a byte-order mark.
            Ok(text) => toml::from_str(text.trim_start_matches('\u{feff}'))
                .map(Self::upgrade)
                .unwrap_or_else(|e| {
                    log::warn!("ignoring invalid {}: {e}", path.display());
                    Self {
                        unreadable: true,
                        ..Self::default()
                    }
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            // Not UTF-8 (Windows PowerShell can write UTF-16), or unreadable.
            Err(e) => {
                log::warn!("ignoring {}: {e}", path.display());
                Self {
                    unreadable: true,
                    ..Self::default()
                }
            }
        }
    }

    /// Replaces defaults that older versions saved and that have since changed.
    fn upgrade(mut self) -> Self {
        if !cfg!(windows) && self.window_match == ["steam_app_221100", "DayZ"] {
            self.window_match = Self::default().window_match;
        }
        self
    }

    /// Saves what the overlay itself changes (view settings, and the game folder when picked),
    /// keeping anything else edited in the file while it ran.
    pub fn save_from_overlay(&self, game_dir_picked: bool) -> Result<()> {
        let mut on_disk = Self::load();
        if on_disk.unreadable {
            // Broken since the overlay started: keep a copy before replacing it.
            return Self {
                unreadable: true,
                ..self.clone()
            }
            .save();
        }
        on_disk.view = self.view.clone();
        if game_dir_picked {
            on_disk.game_dir = self.game_dir.clone();
        }
        on_disk.save()
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        // (Only while the file on disk is still the unreadable one: after the first save, it's
        // ours.)
        let still_unreadable = || match std::fs::read(&path) {
            Ok(bytes) => Ok(std::str::from_utf8(&bytes).map_or(true, |text| {
                toml::from_str::<Config>(text.trim_start_matches('\u{feff}')).is_err()
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            // Not even readable (owned by root after a `sudo` edit, say): there'd be no copy.
            Err(e) => Err(e)
                .with_context(|| format!("can't read {}, so leaving it as it is", path.display())),
        };
        if self.unreadable && still_unreadable()? {
            let backup = path.with_extension("toml.bak");
            std::fs::copy(&path, &backup)
                .with_context(|| format!("keeping a copy of {}", path.display()))?;
            log::warn!(
                "{} had errors; kept it as {}",
                path.display(),
                backup.display()
            );
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}
