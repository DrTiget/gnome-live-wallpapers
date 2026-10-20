//! Renderer-independent daemon logic: catalog + state + commands to renderer.

use crate::state::State;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use we_core::{Catalog, Project, ProjectType};

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
}

impl Core {
    pub fn new(state_path: PathBuf, monitors: Vec<String>, renderer: Box<dyn Renderer>) -> Core {
        let state = State::load(&state_path);
        let mut core = Core { state, state_path, catalog: Catalog::default(), monitors, renderer };
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
                json!({
                    "id": p.id(), "title": p.title, "type": p.kind.as_str(),
                    "supported": p.kind.is_supported(),
                    "preview": p.preview_path(), "root": p.root, "tags": p.tags,
                    "audio": p.supports_audio, "rating": p.content_rating,
                })
            })
            .collect();
        Value::Array(items).to_string()
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
