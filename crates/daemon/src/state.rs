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
}

fn default_volume() -> f64 {
    0.5
}

fn default_muted() -> bool {
    true
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
            .unwrap_or_else(|| State { volume: default_volume(), muted: default_muted(), ..Default::default() })
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
        let mut s = State::load(&path);
        s.assignments.insert("*".into(), "1".into());
        s.assignments.insert("DP-1".into(), "2".into());
        s.save(&path).unwrap();
        let s2 = State::load(&path);
        assert_eq!(s, s2);
        assert_eq!(s2.wallpaper_for("DP-1"), Some("2"));
        assert_eq!(s2.wallpaper_for("HDMI-1"), Some("1"));
    }
}
