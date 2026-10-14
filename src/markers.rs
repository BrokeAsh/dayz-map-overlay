//! Markers the player puts on maps: their own (home, enemy base, stash, ...) and any of the
//! map's kinds of point of interest. Each is saved either for one server, so it only shows while
//! playing there, or for the map, so it shows on every server with that map.
//!
//! They're kept in `markers.json` in the data folder, written whole (to a new file, then renamed
//! over the old one) after every change. A file that can't be read is kept as `markers.json.bak`
//! before the first save replaces it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::import::poi::Kind;

/// The player's own kinds of marker, next to the map's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Own {
    Home,
    EnemyBase,
    FriendlyBase,
    Stash,
    Vehicle,
    Danger,
    Meeting,
    Death,
}

impl Own {
    pub const ALL: [Own; 8] = [
        Own::Home,
        Own::EnemyBase,
        Own::FriendlyBase,
        Own::Stash,
        Own::Vehicle,
        Own::Danger,
        Own::Meeting,
        Own::Death,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Own::Home => "Home",
            Own::EnemyBase => "Enemy base",
            Own::FriendlyBase => "Friendly base",
            Own::Stash => "Stash",
            Own::Vehicle => "My vehicle",
            Own::Danger => "Danger",
            Own::Meeting => "Meeting point",
            Own::Death => "Where I died",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Own::Home => "home",
            Own::EnemyBase => "enemy_base",
            Own::FriendlyBase => "friendly_base",
            Own::Stash => "stash",
            Own::Vehicle => "my_vehicle",
            Own::Danger => "danger",
            Own::Meeting => "meeting_point",
            Own::Death => "death",
        }
    }
}

/// What a marker marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    Own(Own),
    /// One of the kinds the map's own points of interest have.
    Map(Kind),
}

impl MarkerKind {
    pub fn label(self) -> &'static str {
        match self {
            MarkerKind::Own(own) => own.label(),
            MarkerKind::Map(kind) => kind.marker_label(),
        }
    }

    /// The name it's saved under.
    pub fn id(self) -> String {
        match self {
            MarkerKind::Own(own) => own.id().to_string(),
            MarkerKind::Map(kind) => kind.id(),
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Own::ALL
            .into_iter()
            .find(|o| o.id() == id)
            .map(MarkerKind::Own)
            .or_else(|| {
                Kind::ALL
                    .into_iter()
                    .find(|k| k.id() == id)
                    .map(MarkerKind::Map)
            })
    }
}

/// Names that can replace a marker's number (there's no typing in the overlay: the game keeps
/// the keyboard).
pub const NAMES: [&str; 12] = [
    "Main",
    "Backup",
    "Active",
    "Raided",
    "Abandoned",
    "Empty",
    "North",
    "South",
    "East",
    "West",
    "Coast",
    "Inland",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: u64,
    /// The map's id, such as `chernarusplus`.
    pub map: String,
    /// The server (`ip:port`, as the game was started with) it's saved for; `None` for the
    /// whole map.
    #[serde(default)]
    pub server: Option<String>,
    /// [`MarkerKind::id`]. Kept as text, so a kind from a newer version survives (unshown).
    pub kind: String,
    /// Told apart from others of its kind by this number, unless it has a name.
    pub number: u32,
    #[serde(default)]
    pub name: Option<String>,
    pub x: f32,
    pub z: f32,
}

impl Marker {
    pub fn kind(&self) -> Option<MarkerKind> {
        MarkerKind::from_id(&self.kind)
    }

    pub fn label(&self) -> String {
        let kind = self.kind().map_or("Marker", MarkerKind::label);
        match &self.name {
            Some(name) => format!("{kind}: {name}"),
            None => format!("{kind} {}", self.number),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    markers: Vec<Marker>,
}

#[derive(Debug, Default)]
pub struct Markers {
    pub list: Vec<Marker>,
    path: PathBuf,
    /// The file on disk couldn't be read: keep a copy of it before saving over it.
    unreadable: bool,
}

pub fn default_path() -> PathBuf {
    crate::config::data_dir().join("markers.json")
}

impl Markers {
    pub fn load(path: &Path) -> Self {
        let mut markers = Self {
            path: path.to_owned(),
            ..Self::default()
        };
        match std::fs::read(path) {
            Ok(data) => match serde_json::from_slice::<File>(&data) {
                Ok(file) => markers.list = file.markers,
                Err(e) => {
                    log::warn!("ignoring unreadable {}: {e}", path.display());
                    markers.unreadable = true;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                log::warn!("couldn't read {}: {e}", path.display());
                markers.unreadable = true;
            }
        }
        markers
    }

    /// The markers that show on `map` while playing on `server` (if any).
    pub fn shown<'a>(
        &'a self,
        map: &'a str,
        server: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Marker> + 'a {
        self.list.iter().filter(move |m| {
            m.map == map
                && m.kind().is_some()
                && (m.server.is_none() || m.server.as_deref() == server)
        })
    }

    /// Adds a marker, numbered after the others of its kind saved the same way, and saves.
    pub fn add(
        &mut self,
        map: &str,
        server: Option<&str>,
        kind: MarkerKind,
        x: f32,
        z: f32,
    ) -> Result<u64> {
        let id = self.list.iter().map(|m| m.id).max().map_or(1, |n| n + 1);
        let number = self.free_number(map, server, kind, None);
        self.list.push(Marker {
            id,
            map: map.to_string(),
            server: server.map(str::to_string),
            kind: kind.id(),
            number,
            name: None,
            x,
            z,
        });
        self.save()?;
        Ok(id)
    }

    /// Changes a marker (`change` returns whether it changed anything) and saves. A marker that
    /// moved to another kind or to the server/map side is numbered again there.
    pub fn edit(&mut self, id: u64, change: impl FnOnce(&mut Marker)) -> Result<()> {
        let Some(index) = self.list.iter().position(|m| m.id == id) else {
            return Ok(());
        };
        let before = self.list[index].clone();
        change(&mut self.list[index]);
        let after = &self.list[index];
        if *after == before {
            return Ok(());
        }
        if (after.kind != before.kind || after.server != before.server)
            && let Some(kind) = after.kind()
        {
            let (map, server) = (after.map.clone(), after.server.clone());
            self.list[index].number = self.free_number(&map, server.as_deref(), kind, Some(id));
        }
        self.save()
    }

    pub fn remove(&mut self, id: u64) -> Result<()> {
        let before = self.list.len();
        self.list.retain(|m| m.id != id);
        if self.list.len() == before {
            return Ok(());
        }
        self.save()
    }

    /// The lowest number no other marker of `kind` saved the same way has.
    fn free_number(
        &self,
        map: &str,
        server: Option<&str>,
        kind: MarkerKind,
        skip: Option<u64>,
    ) -> u32 {
        let id = kind.id();
        let taken: Vec<u32> = self
            .list
            .iter()
            .filter(|m| {
                Some(m.id) != skip && m.map == map && m.server.as_deref() == server && m.kind == id
            })
            .map(|m| m.number)
            .collect();
        (1..).find(|n| !taken.contains(n)).unwrap_or(1)
    }

    fn save(&mut self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if self.unreadable && self.path.exists() {
            let backup = self.path.with_extension("json.bak");
            std::fs::copy(&self.path, &backup)
                .with_context(|| format!("keeping a copy of {}", self.path.display()))?;
            log::warn!(
                "{} had errors; kept it as {}",
                self.path.display(),
                backup.display()
            );
        }
        let json = serde_json::to_vec_pretty(&File {
            markers: self.list.clone(),
        })?;
        let tmp = self.path.with_extension("json.tmp");
        crate::maps::write_synced(&tmp, &json)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("writing {}", self.path.display()))?;
        self.unreadable = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dzm-markers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("markers.json")
    }

    #[test]
    fn kinds_round_trip() {
        for own in Own::ALL {
            assert_eq!(
                MarkerKind::from_id(&MarkerKind::Own(own).id()),
                Some(MarkerKind::Own(own))
            );
        }
        for kind in Kind::ALL {
            assert_eq!(
                MarkerKind::from_id(&MarkerKind::Map(kind).id()),
                Some(MarkerKind::Map(kind))
            );
        }
        assert_eq!(MarkerKind::from_id("spaceship"), None);
    }

    #[test]
    fn server_and_map_markers() {
        let path = temp("scope");
        let mut markers = Markers::load(&path);
        let base = MarkerKind::Own(Own::EnemyBase);
        let a = markers
            .add("chernarusplus", Some("1.2.3.4:2302"), base, 10.0, 20.0)
            .unwrap();
        markers
            .add("chernarusplus", Some("1.2.3.4:2302"), base, 11.0, 21.0)
            .unwrap();
        markers
            .add("chernarusplus", None, base, 12.0, 22.0)
            .unwrap();
        markers
            .add("chernarusplus", Some("5.6.7.8:2302"), base, 13.0, 23.0)
            .unwrap();
        markers
            .add("enoch", None, MarkerKind::Own(Own::Home), 1.0, 1.0)
            .unwrap();

        // Numbered per kind, map, and server (or the map as a whole).
        let labels = |server| -> Vec<String> {
            markers
                .shown("chernarusplus", server)
                .map(Marker::label)
                .collect()
        };
        assert_eq!(
            labels(Some("1.2.3.4:2302")),
            ["Enemy base 1", "Enemy base 2", "Enemy base 1"]
        );
        assert_eq!(
            labels(Some("5.6.7.8:2302")),
            ["Enemy base 1", "Enemy base 1"]
        );
        assert_eq!(labels(None), ["Enemy base 1"]);

        // Saved, and read back the same.
        let again = Markers::load(&path);
        assert_eq!(again.list, markers.list);

        // Named; moved to the map as a whole, where it takes the next free number; removed.
        markers.edit(a, |m| m.name = Some("Raided".into())).unwrap();
        assert_eq!(markers.list[0].label(), "Enemy base: Raided");
        markers.edit(a, |m| m.server = None).unwrap();
        assert_eq!(markers.list[0].number, 2);
        markers.remove(a).unwrap();
        assert_eq!(Markers::load(&path).list.len(), 4);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A broken file isn't lost: it's kept beside the new one.
    #[test]
    fn keeps_an_unreadable_file() {
        let path = temp("broken");
        std::fs::write(&path, b"{ not json").unwrap();
        let mut markers = Markers::load(&path);
        assert!(markers.list.is_empty());
        markers
            .add("namalsk", None, MarkerKind::Map(Kind::Military), 1.0, 2.0)
            .unwrap();
        let backup = std::fs::read(path.with_extension("json.bak")).unwrap();
        assert_eq!(backup, b"{ not json");
        assert_eq!(Markers::load(&path).list.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A kind from a newer version stays in the file but isn't shown.
    #[test]
    fn keeps_unknown_kinds() {
        let path = temp("unknown");
        std::fs::write(
            &path,
            br#"{"markers":[{"id":1,"map":"enoch","kind":"spaceship","number":1,"x":1,"z":2}]}"#,
        )
        .unwrap();
        let mut markers = Markers::load(&path);
        assert_eq!(markers.shown("enoch", None).count(), 0);
        markers
            .add("enoch", None, MarkerKind::Own(Own::Stash), 3.0, 4.0)
            .unwrap();
        assert_eq!(Markers::load(&path).list.len(), 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
