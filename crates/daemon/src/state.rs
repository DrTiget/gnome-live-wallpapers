//! Persistent daemon state: which wallpaper is on which monitor, and the
//! user's property overrides per wallpaper. Stored as JSON in
//! `$XDG_CONFIG_HOME/wpe/state.json`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct State {
    /// Monitor connector (e.g. "DP-1") -> wallpaper id. Key "*" = all monitors.
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
    /// Wallpaper id -> {property key -> value}.
    #[serde(default)]
    pub overrides: BTreeMap<String, Map<String, Value>>,
    /// Extra directories with wallpapers besides Steam workshop.
    #[serde(default)]
    pub extra_dirs: Vec<PathBuf>,
    #[serde(default = "default_volume")]
    pub volume: f64,
    /// Wallpapers are silent until the user unmutes them.
    #[serde(default = "default_muted")]
    pub muted: bool,
    /// Frame rate limit for scene wallpapers (linux-wallpaperengine `--fps`).
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Pause wallpapers while a maximized/fullscreen window covers the monitor.
    #[serde(default = "default_true")]
    pub pause_on_maximized: bool,
    /// Pause every monitor while a fullscreen app (game) runs on any monitor.
    #[serde(default = "default_true")]
    pub pause_on_fullscreen_any: bool,
    /// Wallpaper ids in the built-in "favorites" collection.
    #[serde(default)]
    pub favorites: Vec<String>,
    /// User collections (favorites are stored separately).
    #[serde(default)]
    pub collections: Vec<Collection>,
    /// Playlist per monitor connector; "*" = all monitors without their own.
    #[serde(default)]
    pub playlists: BTreeMap<String, Playlist>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Collection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Playlist {
    pub enabled: bool,
    /// "all" | "favorites" | collection id.
    pub source: String,
    pub interval_min: u32,
    /// "shuffle" | "sequential".
    pub order: String,
    pub sync_monitors: bool,
}

impl Default for Playlist {
    fn default() -> Self {
        Playlist { enabled: false, source: "all".into(), interval_min: 30, order: "shuffle".into(), sync_monitors: false }
    }
}

pub const FAVORITES: &str = "favorites";

fn default_true() -> bool {
    true
}

fn default_volume() -> f64 {
    0.5
}

fn default_muted() -> bool {
    true
}

pub const DEFAULT_FPS: u32 = 30;

fn default_fps() -> u32 {
    DEFAULT_FPS
}

/// Random UUID v4 from /dev/urandom (time-based fallback).
fn new_id() -> String {
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b)).is_ok();
    if !ok {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        b = t.to_le_bytes();
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("wpe/state.json")
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_else(|| State { volume: default_volume(), muted: default_muted(), fps: default_fps(), pause_on_maximized: true, pause_on_fullscreen_any: true, ..Default::default() })
    }

    /// Atomic write (tmp + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// Items of a playlist source ("all" is resolved by the caller).
    pub fn source_items(&self, source: &str) -> Option<&[String]> {
        if source == FAVORITES {
            return Some(&self.favorites);
        }
        self.collections.iter().find(|c| c.id == source).map(|c| c.items.as_slice())
    }

    pub fn set_favorite(&mut self, id: &str, on: bool) {
        self.favorites.retain(|x| x != id);
        if on {
            self.favorites.push(id.into());
        }
    }

    pub fn create_collection(&mut self, name: &str) -> String {
        let id = new_id();
        self.collections.push(Collection { id: id.clone(), name: name.into(), items: vec![] });
        id
    }

    pub fn rename_collection(&mut self, id: &str, name: &str) -> Result<(), String> {
        self.collections.iter_mut().find(|c| c.id == id).map(|c| c.name = name.into()).ok_or_else(|| format!("unknown collection {id}"))
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<(), String> {
        if id == FAVORITES {
            return Err("favorites can't be deleted".into());
        }
        let n = self.collections.len();
        self.collections.retain(|c| c.id != id);
        if n == self.collections.len() {
            return Err(format!("unknown collection {id}"));
        }
        // Playlists fed by it fall back to everything.
        for p in self.playlists.values_mut().filter(|p| p.source == id) {
            p.source = "all".into();
        }
        Ok(())
    }

    pub fn add_to_collection(&mut self, id: &str, wallpaper: &str) -> Result<(), String> {
        if id == FAVORITES {
            if !self.favorites.iter().any(|x| x == wallpaper) {
                self.favorites.push(wallpaper.into());
            }
            return Ok(());
        }
        let c = self.collections.iter_mut().find(|c| c.id == id).ok_or_else(|| format!("unknown collection {id}"))?;
        if !c.items.iter().any(|x| x == wallpaper) {
            c.items.push(wallpaper.into());
        }
        Ok(())
    }

    pub fn remove_from_collection(&mut self, id: &str, wallpaper: &str) -> Result<(), String> {
        if id == FAVORITES {
            self.favorites.retain(|x| x != wallpaper);
            return Ok(());
        }
        let c = self.collections.iter_mut().find(|c| c.id == id).ok_or_else(|| format!("unknown collection {id}"))?;
        c.items.retain(|x| x != wallpaper);
        Ok(())
    }

    /// Drop ids that are no longer in the catalog; true if anything changed.
    pub fn retain_known(&mut self, known: &dyn Fn(&str) -> bool) -> bool {
        let before: usize = self.favorites.len() + self.collections.iter().map(|c| c.items.len()).sum::<usize>();
        self.favorites.retain(|i| known(i));
        for c in &mut self.collections {
            c.items.retain(|i| known(i));
        }
        before != self.favorites.len() + self.collections.iter().map(|c| c.items.len()).sum::<usize>()
    }

    /// JSON for GetCollections.
    pub fn collections_json(&self) -> String {
        let mut v = vec![serde_json::json!({"id": FAVORITES, "name": "Избранное", "builtin": true, "items": self.favorites})];
        v.extend(self.collections.iter().map(|c| serde_json::json!({"id": c.id, "name": c.name, "builtin": false, "items": c.items})));
        Value::Array(v).to_string()
    }

    /// Wallpaper for a monitor: explicit assignment, else the "*" default.
    pub fn wallpaper_for(&self, monitor: &str) -> Option<&str> {
        self.assignments.get(monitor).or_else(|| self.assignments.get("*")).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x/state.json");
        assert_eq!(State::load(&path).volume, 0.5);
        assert!(State::load(&path).muted);
        assert_eq!(State::load(&path).fps, 30);
        let mut s = State::load(&path);
        s.assignments.insert("*".into(), "1".into());
        s.assignments.insert("DP-1".into(), "2".into());
        s.save(&path).unwrap();
        let s2 = State::load(&path);
        assert_eq!(s, s2);
        assert_eq!(s2.wallpaper_for("DP-1"), Some("2"));
        assert_eq!(s2.wallpaper_for("HDMI-1"), Some("1"));
    }

    #[test]
    fn collections_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut s = State::load(&path);
        s.set_favorite("1", true);
        s.set_favorite("2", true);
        s.set_favorite("1", true);
        s.set_favorite("2", false);
        assert_eq!(s.favorites, ["1"]);
        let id = s.create_collection("Космос");
        assert_ne!(id, s.create_collection("Другое"));
        s.add_to_collection(&id, "7").unwrap();
        s.add_to_collection(&id, "7").unwrap();
        s.add_to_collection(&id, "8").unwrap();
        s.remove_from_collection(&id, "8").unwrap();
        s.rename_collection(&id, "Space").unwrap();
        assert!(s.add_to_collection("nope", "1").is_err());
        assert!(s.delete_collection(FAVORITES).is_err());
        s.playlists.insert("*".into(), Playlist { source: id.clone(), ..Default::default() });
        s.save(&path).unwrap();
        let mut s2 = State::load(&path);
        assert_eq!(s, s2);
        let v: Value = serde_json::from_str(&s2.collections_json()).unwrap();
        assert_eq!(v[0]["id"], "favorites");
        assert_eq!(v[0]["builtin"], true);
        assert_eq!(v[1]["name"], "Space");
        assert_eq!(v[1]["items"], serde_json::json!(["7"]));
        assert!(s2.retain_known(&|i| i == "1"));
        assert_eq!(s2.favorites, ["1"]);
        assert!(s2.collections[0].items.is_empty());
        s2.delete_collection(&id).unwrap();
        assert_eq!(s2.playlists["*"].source, "all");
    }
}
