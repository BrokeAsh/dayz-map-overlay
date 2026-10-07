//! Locates the DayZ install through Steam's library list.

use std::path::{Path, PathBuf};

pub fn find_dayz() -> Option<PathBuf> {
    library_folders()
        .into_iter()
        .map(|lib| lib.join("steamapps/common/DayZ"))
        .find(|d| d.join("Addons").is_dir())
}

fn library_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let roots = [
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
    ];
    let mut libraries = Vec::new();
    for root in roots {
        let vdf = root.join("steamapps/libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            libraries.extend(parse_paths(&text));
        }
        libraries.push(root);
    }
    // `~/.steam/steam` usually links to `~/.local/share/Steam`; keep each folder once.
    let mut seen = std::collections::HashSet::new();
    libraries.retain(|l| l.is_dir() && seen.insert(l.canonicalize().unwrap_or_else(|_| l.clone())));
    libraries
}

/// Each library's DayZ Workshop folder (`steamapps/workshop/content/221100`).
pub fn workshop_dirs() -> Vec<PathBuf> {
    library_folders()
        .into_iter()
        .map(|lib| lib.join("steamapps/workshop/content/221100"))
        .filter(|d| d.is_dir())
        .collect()
}

/// Each library's Proton prefix folder where DayZ writes its logs. `DAYZ_MAP_LOG_DIR`
/// overrides this (for unusual setups, or to test with recorded logs).
pub fn log_dirs() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("DAYZ_MAP_LOG_DIR") {
        return vec![PathBuf::from(dir)];
    }
    library_folders()
        .into_iter()
        .map(|lib| {
            lib.join("steamapps/compatdata/221100/pfx/drive_c/users/steamuser/AppData/Local/DayZ")
        })
        .filter(|d| d.is_dir())
        .collect()
}

/// Pulls the `"path"  "..."` values out of `libraryfolders.vdf`.
fn parse_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|line| {
            let mut quoted = line.split('"').skip(1).step_by(2);
            (quoted.next()? == "path").then(|| quoted.next()).flatten()
        })
        .map(|p| Path::new(&p.replace("\\\\", "\\")).to_owned())
        .collect()
}
