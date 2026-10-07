//! Steam's install folders and library list.

use std::path::{Path, PathBuf};

/// DayZ's Steam app id.
pub const DAYZ_APP: &str = "221100";

/// Where Steam itself may be installed.
fn steam_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    vec![
        home.join(".local/share/Steam"),
        home.join(".steam/steam"),
        home.join(".steam/root"),
        // Flatpak and Snap packages of Steam.
        home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"),
        home.join("snap/steam/common/.local/share/Steam"),
        PathBuf::from(r"C:\Program Files (x86)\Steam"),
    ]
}

/// Every Steam library folder (each has a `steamapps` folder), from each Steam install's
/// `libraryfolders.vdf`.
pub fn library_folders() -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for root in steam_roots() {
        let vdf = root.join("steamapps/libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            libraries.extend(parse_paths(&text));
        }
        libraries.push(root);
    }
    dedup_dirs(libraries)
}

/// Keeps existing folders, each once (`~/.steam/steam` usually links to `~/.local/share/Steam`).
pub fn dedup_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter()
        .filter(|d| d.is_dir() && seen.insert(d.canonicalize().unwrap_or_else(|_| d.clone())))
        .collect()
}

/// The library a game folder (`<library>/steamapps/common/<game>`) belongs to.
pub fn library_of(game_dir: &Path) -> Option<PathBuf> {
    let common = game_dir.parent()?;
    let steamapps = common.parent()?;
    let named = |p: &Path, name: &str| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
    };
    (named(common, "common") && named(steamapps, "steamapps"))
        .then(|| steamapps.parent().map(Path::to_owned))
        .flatten()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_library_of_a_game() {
        assert_eq!(
            library_of(Path::new("/games/SteamLibrary/steamapps/common/DayZ")),
            Some(PathBuf::from("/games/SteamLibrary"))
        );
        assert_eq!(library_of(Path::new("/opt/DayZ")), None);
    }
}
