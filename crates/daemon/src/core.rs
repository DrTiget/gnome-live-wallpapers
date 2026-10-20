//! Renderer-independent daemon logic: catalog + state + commands to renderer.

use crate::state::{Playlist, State};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use we_core::{Catalog, Project, ProjectType, ValidationIssue};

/// ("ok" | "unsupported" | "broken", human-readable reason) for the GUI.
trait Status {
    fn status(&self) -> (&'static str, String);
}
impl Status for Project {
    fn status(&self) -> (&'static str, String) {
        if !self.kind.is_supported() {
            return ("unsupported", format!("тип «{}» не поддерживается", self.kind.as_str()));
        }
        let fatal = self.validate().into_iter().find(|i| !matches!(i, ValidationIssue::PreviewNotFound(_)));
        match fatal {
            Some(i) => ("broken", format!("{i:?}")),
            None => ("ok", String::new()),
        }
    }
}

/// Wallpaper id -> reason its renderer crashed at start (set by the renderer,
/// cleared once it runs fine); shown as status "broken".
pub static RUNTIME_BROKEN: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Commands the core sends to the active renderer backend.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderCmd {
    /// Show `project` (presets already resolved, overrides applied) on monitor.
    Show { monitor: String, project: Box<Project> },
    Clear { monitor: String },
    /// New values for web wallpapers' applyUserProperties / scene restart.
    Properties { wallpaper: String, user_properties: Value },
    Pointer { monitor: String, x: f64, y: f64, kind: PointerKind, button: u32 },
    Pause { monitor: String, paused: bool },
    Volume { volume: f64, muted: bool },
    /// Frame rate limit for scenes (restarts running scenes).
    Fps(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    Motion,
    Press,
    Release,
}

impl PointerKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "motion" => Some(Self::Motion),
            "press" => Some(Self::Press),
            "release" => Some(Self::Release),
            _ => None,
        }
    }
}

pub trait Renderer: Send {
    fn handle(&mut self, cmd: RenderCmd);
}

/// Backend for builds without GTK: just logs commands (useful for GUI/extension dev).
#[cfg_attr(feature = "gtk", allow(dead_code))]
pub struct LogRenderer;
impl Renderer for LogRenderer {
    fn handle(&mut self, cmd: RenderCmd) {
        eprintln!("[render] {cmd:?}");
    }
}

/// `$XDG_DATA_HOME/wpe/wallpapers` — wallpapers not from Steam.
fn local_wallpaper_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("wpe/wallpapers"))
}

pub struct Core {
    pub state: State,
    pub state_path: PathBuf,
    pub catalog: Catalog,
    pub monitors: Vec<String>,
    pub renderer: Box<dyn Renderer>,
    /// Scene id -> why linux-wallpaperengine can't render it (from its scene file, at rescan).
    pub scene_unsupported: HashMap<String, String>,
    /// From the extension: monitors covered by a maximized/fullscreen window (or locked).
    pub covered: HashSet<String>,
    /// From the extension: a fullscreen app runs on some monitor.
    pub fullscreen_any: bool,
    /// Pause state last sent to the renderer.
    pub paused: HashSet<String>,
    /// Playlist timers: group key (monitor, or "sync:<playlist key>") -> seconds since last change.
    pub pl_elapsed: HashMap<String, f64>,
}

/// Monitors changed together by one playlist timer.
#[derive(Debug, Clone, PartialEq)]
pub struct PlGroup {
    pub key: String,
    pub monitors: Vec<String>,
    pub cfg: Playlist,
}

impl Core {
    pub fn new(state_path: PathBuf, monitors: Vec<String>, renderer: Box<dyn Renderer>) -> Core {
        let state = State::load(&state_path);
        let mut core = Core { state, state_path, catalog: Catalog::default(), monitors, renderer, scene_unsupported: HashMap::new(),
            covered: HashSet::new(), fullscreen_any: false, paused: HashSet::new(), pl_elapsed: HashMap::new() };
        core.rescan();
        core
    }

    pub fn rescan(&mut self) {
        let mut roots = std::env::var_os("HOME")
            .map(|h| we_core::workshop_dirs(&PathBuf::from(h)))
            .unwrap_or_default();
        // Built-in projects of the Wallpaper Engine app (mostly scenes).
        if let Some(home) = std::env::var_os("HOME") {
            for we in we_core::wallpaper_engine_dirs(&PathBuf::from(home)) {
                roots.push(we.join("projects/defaultprojects"));
            }
        }
        roots.extend(local_wallpaper_dir());
        roots.extend(self.state.extra_dirs.iter().cloned());
        self.catalog = Catalog::scan(&roots);
        self.scene_unsupported = self
            .catalog
            .projects
            .iter()
            .filter(|p| p.kind == ProjectType::Scene)
            .filter_map(|p| Some((p.id(), crate::scene::unsupported_reason(&p.root, p.file.as_deref())?)))
            .collect();
        for e in &self.catalog.errors {
            eprintln!("[catalog] {}: {}", e.dir.display(), e.error);
        }
        // Wallpapers gone from the catalog silently leave the collections
        // (not on an empty catalog: that is more likely a missing library).
        if !self.catalog.projects.is_empty() {
            let cat = &self.catalog;
            if self.state.retain_known(&|id| cat.get(id).is_some()) {
                self.persist();
            }
        }
    }

    pub fn add_directory(&mut self, dir: PathBuf) -> Result<(), String> {
        if !dir.is_dir() {
            return Err(format!("{} is not a directory", dir.display()));
        }
        if !self.state.extra_dirs.contains(&dir) {
            self.state.extra_dirs.push(dir);
            self.persist();
        }
        self.rescan();
        Ok(())
    }

    /// Project ready to render: preset resolved, user overrides applied.
    pub fn effective_project(&self, id: &str) -> Option<Project> {
        let p = self.catalog.get(id)?;
        let mut p = if p.kind == ProjectType::Preset { self.catalog.resolve_preset(p)? } else { p.clone() };
        if let Some(o) = self.state.overrides.get(id) {
            p.properties.apply_overrides(o);
        }
        Some(p)
    }

    pub fn list_json(&self) -> String {
        let items: Vec<Value> = self
            .catalog
            .projects
            .iter()
            .map(|p| {
                let id = p.id();
                let (status, reason) = self.project_status(p);
                json!({
                    "id": id, "title": p.title, "type": p.kind.as_str(),
                    "supported": p.kind.is_supported(),
                    "preview": p.preview_path(), "root": p.root, "tags": p.tags,
                    "audio": p.supports_audio, "rating": p.content_rating,
                    "status": status, "reason": reason,
                })
            })
            .collect();
        Value::Array(items).to_string()
    }

    fn project_status(&self, p: &Project) -> (&'static str, String) {
        let id = p.id();
        let (mut status, mut reason) = p.status();
        if status == "ok" {
            if let Some(r) = self.scene_unsupported.get(&id) {
                (status, reason) = ("unsupported", r.clone());
            } else if let Some(r) = RUNTIME_BROKEN.lock().unwrap().get(&id) {
                (status, reason) = ("broken", r.clone());
            }
        }
        (status, reason)
    }

    /// Settings for the GUI: {fps, volume, muted, pause_on_maximized, pause_on_fullscreen_any, monitors, directories}.
    pub fn settings_json(&self) -> String {
        json!({
            "fps": self.state.fps, "volume": self.state.volume, "muted": self.state.muted,
            "pause_on_maximized": self.state.pause_on_maximized,
            "pause_on_fullscreen_any": self.state.pause_on_fullscreen_any,
            "monitors": self.monitors, "directories": self.state.extra_dirs,
        })
        .to_string()
    }

    pub fn remove_directory(&mut self, dir: PathBuf) {
        self.state.extra_dirs.retain(|d| d != &dir);
        self.persist();
        self.rescan();
    }

    pub fn set_pause_on_maximized(&mut self, on: bool) {
        self.state.pause_on_maximized = on;
        self.persist();
        self.update_pause();
    }

    pub fn set_pause_on_fullscreen_any(&mut self, on: bool) {
        self.state.pause_on_fullscreen_any = on;
        self.persist();
        self.update_pause();
    }

    /// Facts from the extension; the pause decision (with settings) is made here.
    pub fn set_covered(&mut self, monitor: &str, covered: bool) {
        if covered {
            self.covered.insert(monitor.into());
        } else {
            self.covered.remove(monitor);
        }
        self.update_pause();
    }

    pub fn set_fullscreen_any(&mut self, on: bool) {
        self.fullscreen_any = on;
        self.update_pause();
    }

    /// Monitor paused if covered (setting pause_on_maximized) or a fullscreen
    /// app runs anywhere (setting pause_on_fullscreen_any); sends changes only.
    fn update_pause(&mut self) {
        let all = self.fullscreen_any && self.state.pause_on_fullscreen_any;
        let mut names: Vec<String> = self.monitors.clone();
        names.extend(self.covered.iter().chain(self.paused.iter()).cloned());
        names.sort();
        names.dedup();
        for m in names {
            let want = all || (self.state.pause_on_maximized && self.covered.contains(&m));
            if want != self.paused.contains(&m) {
                if want {
                    self.paused.insert(m.clone());
                } else {
                    self.paused.remove(&m);
                }
                eprintln!("[pause] {m}: {}", if want { "paused" } else { "playing" });
                self.renderer.handle(RenderCmd::Pause { monitor: m, paused: want });
            }
        }
    }

    fn persist(&self) {
        if let Err(e) = self.state.save(&self.state_path) {
            eprintln!("[state] save failed: {e}");
        }
    }

    /// Re-send Show/Clear for every known monitor.
    pub fn apply_all(&mut self) {
        self.renderer.handle(RenderCmd::Volume { volume: self.state.volume, muted: self.state.muted });
        self.renderer.handle(RenderCmd::Fps(self.state.fps));
        for m in self.monitors.clone() {
            self.apply_monitor(&m);
        }
    }

    fn apply_monitor(&mut self, monitor: &str) {
        let cmd = match self.state.wallpaper_for(monitor).and_then(|id| self.effective_project(id)) {
            Some(project) => RenderCmd::Show { monitor: monitor.into(), project: Box::new(project) },
            None => RenderCmd::Clear { monitor: monitor.into() },
        };
        self.renderer.handle(cmd);
    }

    pub fn set_wallpaper(&mut self, monitor: &str, id: &str) -> Result<(), String> {
        if id.is_empty() {
            self.state.assignments.remove(monitor);
        } else {
            let p = self.catalog.get(id).ok_or_else(|| format!("unknown wallpaper {id}"))?;
            if !p.kind.is_supported() {
                return Err(format!("unsupported type {}", p.kind.as_str()));
            }
            if let Some(r) = self.scene_unsupported.get(id) {
                return Err(r.clone());
            }
            self.state.assignments.insert(monitor.into(), id.into());
        }
        self.persist();
        self.reset_playlist_timer(monitor);
        if monitor == "*" {
            self.apply_all();
        } else {
            self.apply_monitor(monitor);
        }
        Ok(())
    }

    // ---- collections ----

    pub fn collections_json(&self) -> String {
        self.state.collections_json()
    }

    pub fn set_favorite(&mut self, id: &str, on: bool) {
        self.state.set_favorite(id, on);
        self.persist();
    }

    pub fn create_collection(&mut self, name: &str) -> String {
        let id = self.state.create_collection(name);
        self.persist();
        id
    }

    pub fn rename_collection(&mut self, id: &str, name: &str) -> Result<(), String> {
        self.state.rename_collection(id, name)?;
        self.persist();
        Ok(())
    }

    pub fn delete_collection(&mut self, id: &str) -> Result<(), String> {
        self.state.delete_collection(id)?;
        self.persist();
        Ok(())
    }

    pub fn add_to_collection(&mut self, id: &str, wallpaper: &str) -> Result<(), String> {
        if self.catalog.get(wallpaper).is_none() {
            return Err(format!("unknown wallpaper {wallpaper}"));
        }
        self.state.add_to_collection(id, wallpaper)?;
        self.persist();
        Ok(())
    }

    pub fn remove_from_collection(&mut self, id: &str, wallpaper: &str) -> Result<(), String> {
        self.state.remove_from_collection(id, wallpaper)?;
        self.persist();
        Ok(())
    }

    // ---- playlists ----

    /// Playlist in effect for a monitor: its own, else "*", else disabled default.
    pub fn playlist_for(&self, monitor: &str) -> Playlist {
        self.state.playlists.get(monitor).or_else(|| self.state.playlists.get("*")).cloned().unwrap_or_default()
    }

    pub fn playlist_json(&self, monitor: &str) -> String {
        serde_json::to_string(&self.playlist_for(monitor)).unwrap_or_default()
    }

    pub fn set_playlist(&mut self, monitor: &str, json: &str) -> Result<(), String> {
        let p: Playlist = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if p.interval_min == 0 {
            return Err("interval_min must be >= 1".into());
        }
        if p.order != "shuffle" && p.order != "sequential" {
            return Err(format!("unknown order {}", p.order));
        }
        if p.source != "all" && self.state.source_items(&p.source).is_none() {
            return Err(format!("unknown source {}", p.source));
        }
        if monitor == "*" {
            self.state.playlists.clear();
        }
        self.state.playlists.insert(monitor.into(), p);
        self.persist();
        self.reset_playlist_timer(monitor);
        Ok(())
    }

    /// Timer groups of the known monitors (disabled playlists only if asked).
    pub fn playlist_groups(&self, include_disabled: bool) -> Vec<PlGroup> {
        let mut out: Vec<PlGroup> = vec![];
        for m in &self.monitors {
            let cfg = self.playlist_for(m);
            if !cfg.enabled && !include_disabled {
                continue;
            }
            if cfg.sync_monitors {
                let key = format!("sync:{}", if self.state.playlists.contains_key(m) { m } else { "*" });
                if let Some(g) = out.iter_mut().find(|g| g.key == key) {
                    g.monitors.push(m.clone());
                    continue;
                }
                out.push(PlGroup { key, monitors: vec![m.clone()], cfg });
            } else {
                out.push(PlGroup { key: m.clone(), monitors: vec![m.clone()], cfg });
            }
        }
        out
    }

    /// Wallpapers a playlist may show right now (supported, not broken).
    pub fn playlist_candidates(&self, source: &str) -> Vec<String> {
        let ids: Vec<String> = if source == "all" {
            self.catalog.projects.iter().map(|p| p.id()).collect()
        } else {
            self.state.source_items(source).map(|s| s.to_vec()).unwrap_or_default()
        };
        ids.into_iter().filter(|id| self.catalog.get(id).is_some_and(|p| self.project_status(p).0 == "ok")).collect()
    }

    /// Manual change on `monitor` ("*" = all) restarts the interval.
    fn reset_playlist_timer(&mut self, monitor: &str) {
        if monitor == "*" {
            self.pl_elapsed.clear();
            return;
        }
        for g in self.playlist_groups(true) {
            if g.monitors.iter().any(|m| m == monitor) {
                self.pl_elapsed.remove(&g.key);
            }
        }
        self.pl_elapsed.remove(monitor);
    }

    /// Switch a group to its next wallpaper; returns the changed monitors.
    fn advance_group(&mut self, g: &PlGroup, rnd: u64) -> Vec<String> {
        self.pl_elapsed.remove(&g.key);
        let cands = self.playlist_candidates(&g.cfg.source);
        let current = g.monitors.first().and_then(|m| self.state.wallpaper_for(m)).map(str::to_string);
        let Some(next) = crate::playlist::pick_next(&cands, current.as_deref(), g.cfg.order == "shuffle", rnd) else {
            return vec![];
        };
        for m in &g.monitors {
            self.state.assignments.insert(m.clone(), next.clone());
            self.apply_monitor(m);
        }
        self.persist();
        g.monitors.clone()
    }

    /// NextWallpaper: now, regardless of the timer ("*" = every monitor).
    pub fn next_wallpaper(&mut self, monitor: &str) -> Vec<String> {
        let mut changed = vec![];
        for g in self.playlist_groups(true) {
            if monitor == "*" || g.monitors.iter().any(|m| m == monitor) {
                changed.extend(self.advance_group(&g, crate::playlist::random_u64()));
            }
        }
        changed
    }

    /// Advance timers by `dt` seconds; returns monitors whose wallpaper changed.
    /// Paused groups (all their monitors paused) don't count time.
    pub fn playlist_tick(&mut self, dt: f64) -> Vec<String> {
        let mut changed = vec![];
        let groups = self.playlist_groups(false);
        self.pl_elapsed.retain(|k, _| groups.iter().any(|g| &g.key == k));
        for g in groups {
            if g.monitors.iter().all(|m| self.paused.contains(m)) {
                continue;
            }
            let e = self.pl_elapsed.entry(g.key.clone()).or_insert(0.0);
            *e += dt;
            if *e >= g.cfg.interval_min as f64 * 60.0 {
                changed.extend(self.advance_group(&g, crate::playlist::random_u64()));
            }
        }
        changed
    }

    pub fn set_fps(&mut self, fps: u32) -> Result<(), String> {
        if !(1..=240).contains(&fps) {
            return Err(format!("fps must be 1..240, got {fps}"));
        }
        self.state.fps = fps;
        self.persist();
        self.renderer.handle(RenderCmd::Fps(fps));
        Ok(())
    }

    pub fn properties_json(&self, id: &str) -> Result<String, String> {
        let p = self.effective_project(id).ok_or_else(|| format!("unknown wallpaper {id}"))?;
        serde_json::to_string(&p.properties).map_err(|e| e.to_string())
    }

    pub fn set_property(&mut self, id: &str, key: &str, value: Value) -> Result<(), String> {
        let mut p = self.effective_project(id).ok_or_else(|| format!("unknown wallpaper {id}"))?;
        p.properties.set(key, &value).map_err(|e| e.to_string())?;
        let stored = p.properties.get(key).map(|x| x.value_json()).unwrap_or(Value::Null);
        self.state.overrides.entry(id.into()).or_insert_with(Map::new).insert(key.into(), stored);
        self.persist();
        self.renderer.handle(RenderCmd::Properties { wallpaper: id.into(), user_properties: p.properties.to_user_properties() });
        Ok(())
    }

    pub fn reset_properties(&mut self, id: &str) {
        self.state.overrides.remove(id);
        self.persist();
        if let Some(p) = self.effective_project(id) {
            self.renderer.handle(RenderCmd::Properties { wallpaper: id.into(), user_properties: p.properties.to_user_properties() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Rec(Arc<Mutex<Vec<(String, bool)>>>);
    impl Renderer for Rec {
        fn handle(&mut self, cmd: RenderCmd) {
            if let RenderCmd::Pause { monitor, paused } = cmd {
                self.0.lock().unwrap().push((monitor, paused));
            }
        }
    }

    fn core() -> (Core, Arc<Mutex<Vec<(String, bool)>>>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(Mutex::new(vec![]));
        let mut state = State::load(&dir.path().join("none.json"));
        state.extra_dirs.clear();
        let mut c = Core {
            state,
            state_path: dir.path().join("state.json"),
            catalog: Catalog::default(),
            monitors: vec!["DP-1".into(), "HDMI-1".into()],
            renderer: Box::new(Rec(log.clone())),
            scene_unsupported: HashMap::new(),
            covered: HashSet::new(),
            fullscreen_any: false,
            paused: HashSet::new(),
            pl_elapsed: HashMap::new(),
        };
        c.update_pause();
        (c, log, dir)
    }

    fn take(log: &Arc<Mutex<Vec<(String, bool)>>>) -> Vec<(String, bool)> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[test]
    fn covered_monitor_pauses_only_itself() {
        let (mut c, log, _d) = core();
        assert!(take(&log).is_empty());
        c.set_covered("DP-1", true);
        c.set_covered("DP-1", true);
        assert_eq!(take(&log), [("DP-1".into(), true)]);
        c.set_pause_on_maximized(false);
        assert_eq!(take(&log), [("DP-1".into(), false)]);
        c.set_pause_on_maximized(true);
        assert_eq!(take(&log), [("DP-1".into(), true)]);
        c.set_covered("DP-1", false);
        assert_eq!(take(&log), [("DP-1".into(), false)]);
    }

    #[test]
    fn fullscreen_app_pauses_every_monitor() {
        let (mut c, log, _d) = core();
        c.set_covered("HDMI-1", true);
        take(&log);
        c.set_fullscreen_any(true);
        assert_eq!(take(&log), [("DP-1".into(), true)]);
        c.set_pause_on_fullscreen_any(false);
        assert_eq!(take(&log), [("DP-1".into(), false)]);
        c.set_pause_on_fullscreen_any(true);
        c.set_fullscreen_any(false);
        let l = take(&log);
        assert_eq!(l, [("DP-1".into(), true), ("DP-1".into(), false)]);
        assert!(c.paused.contains("HDMI-1"));
    }

    fn mk(dir: &std::path::Path, id: &str, kind: &str) {
        let d = dir.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.mp4"), b"").unwrap();
        std::fs::write(d.join("project.json"), format!(r#"{{"title":"{id}","type":"{kind}","file":"a.mp4"}}"#)).unwrap();
    }

    #[test]
    fn playlist_ticks_pauses_and_resets() {
        let (mut c, _log, d) = core();
        let wp = d.path().join("wp");
        mk(&wp, "100", "video");
        mk(&wp, "101", "video");
        mk(&wp, "102", "application");
        c.catalog = Catalog::scan(&[&wp]);
        let ids: Vec<String> = c.catalog.projects.iter().map(|p| p.id()).collect();
        assert_eq!(c.playlist_candidates("all").len(), ids.len() - 1, "unsupported skipped");
        c.set_favorite("101", true);
        assert_eq!(c.playlist_candidates("favorites"), ["101"]);
        assert!(c.set_playlist("*", r#"{"enabled":true,"source":"nope"}"#).is_err());
        c.set_playlist("*", r#"{"enabled":true,"source":"all","interval_min":1,"order":"sequential"}"#).unwrap();
        c.set_wallpaper("DP-1", "100").unwrap();
        c.set_wallpaper("HDMI-1", "100").unwrap();
        // paused monitor doesn't count time
        c.paused.insert("DP-1".into());
        assert!(c.playlist_tick(59.0).is_empty());
        c.paused.remove("DP-1");
        assert!(c.playlist_tick(0.5).is_empty());
        // manual change resets the timer of that monitor only
        c.set_wallpaper("DP-1", "100").unwrap();
        assert_eq!(c.playlist_tick(0.6), ["HDMI-1"]);
        assert_eq!(c.state.wallpaper_for("HDMI-1"), Some("101"));
        assert_eq!(c.state.wallpaper_for("DP-1"), Some("100"));
        // next on one monitor
        assert_eq!(c.next_wallpaper("DP-1"), ["DP-1"]);
        assert_eq!(c.state.wallpaper_for("DP-1"), Some("101"));
        // sync: one wallpaper for all
        c.set_playlist("*", r#"{"enabled":true,"source":"all","interval_min":1,"order":"sequential","sync_monitors":true}"#).unwrap();
        assert_eq!(c.playlist_groups(false).len(), 1);
        c.next_wallpaper("*");
        assert_eq!(c.state.wallpaper_for("DP-1"), c.state.wallpaper_for("HDMI-1"));
        // disabled: no ticks
        c.set_playlist("*", r#"{"enabled":false}"#).unwrap();
        assert!(c.playlist_tick(10_000.0).is_empty());
    }

    #[test]
    fn rescan_drops_removed_wallpapers() {
        let (mut c, _log, d) = core();
        let wp = d.path().join("wp");
        mk(&wp, "100", "video");
        c.state.extra_dirs = vec![wp.clone()];
        c.rescan();
        let id = c.catalog.projects.iter().map(|p| p.id()).find(|i| i.contains("100")).unwrap();
        c.set_favorite(&id, true);
        c.set_favorite("gone", true);
        c.rescan();
        assert_eq!(c.state.favorites, [id]);
    }
}
