use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Key that opens the overlay while the game has focus (an X11 keysym name such as `m`).
    pub hotkey: String,
    /// The overlay only opens when the focused window's class or title contains one of these
    /// (case-insensitive). Empty means any window.
    pub window_match: Vec<String>,
    /// DayZ install folder; found through Steam when unset.
    pub game_dir: Option<PathBuf>,
    /// Folder where DayZ writes its logs (`script_*.log`), used to tell which server you're on;
    /// found next to the game when unset.
    pub log_dir: Option<PathBuf>,
    pub view: ViewConfig,
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
            window_match: vec!["steam_app_221100".into(), "DayZ".into()],
            game_dir: None,
            log_dir: None,
            view: ViewConfig::default(),
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
            Ok(text) => toml::from_str(text.trim_start_matches('\u{feff}')).unwrap_or_else(|e| {
                log::warn!("ignoring invalid {}: {e}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
    }
}
