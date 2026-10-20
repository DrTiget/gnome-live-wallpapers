//! Renderer-independent daemon logic: catalog + state + commands to renderer.

use crate::state::State;
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
}

impl Core {
    pub fn new(state_path: PathBuf, monitors: Vec<String>, renderer: Box<dyn Renderer>) -> Core {
        let state = State::load(&state_path);
        let mut core = Core { state, state_path, catalog: Catalog::default(), monitors, renderer, scene_unsupported: HashMap::new(),
            covered: HashSet::new(), fullscreen_any: false, paused: HashSet::new() };
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
                let (mut status, mut reason) = p.status();
                if status == "ok" {
                    if let Some(r) = self.scene_unsupported.get(&id) {
                        (status, reason) = ("unsupported", r.clone());
                    } else if let Some(r) = RUNTIME_BROKEN.lock().unwrap().get(&id) {
                        (status, reason) = ("broken", r.clone());
                    }
                }
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
        if monitor == "*" {
            self.apply_all();
        } else {
            self.apply_monitor(monitor);
        }
        Ok(())
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
}
