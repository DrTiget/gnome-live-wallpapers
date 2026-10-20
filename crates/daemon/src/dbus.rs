//! D-Bus API `io.github.wpe.Daemon1` at `/io/github/wpe/Daemon`.
//! See docs/ARCHITECTURE.md for the full contract.

use crate::core::{Core, PointerKind, RenderCmd};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use zbus::{fdo, interface, object_server::SignalEmitter};

pub const BUS_NAME: &str = "io.github.wpe.Daemon";
pub const OBJECT_PATH: &str = "/io/github/wpe/Daemon";
const INTERFACE: &str = "io.github.wpe.Daemon1";

/// Monitor connector -> PID of the process whose window renders it (scenes:
/// linux-wallpaperengine). The extension matches those windows by PID.
pub static RENDERER_PIDS: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());
static CONNECTION: OnceLock<zbus::blocking::Connection> = OnceLock::new();

pub fn set_connection(conn: zbus::blocking::Connection) {
    let _ = CONNECTION.set(conn);
}

#[cfg_attr(not(feature = "gtk"), allow(dead_code))]
pub fn set_renderer_pid(monitor: &str, pid: Option<u32>) {
    {
        let mut pids = RENDERER_PIDS.lock().unwrap();
        let changed = match pid {
            Some(pid) => pids.insert(monitor.into(), pid) != Some(pid),
            None => pids.remove(monitor).is_some(),
        };
        if !changed {
            return;
        }
    }
    if let Some(conn) = CONNECTION.get() {
        let _ = conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "RendererPidsChanged", &());
    }
}

/// Status of some wallpaper changed outside a D-Bus call (renderer crash).
#[cfg_attr(not(feature = "gtk"), allow(dead_code))]
pub fn emit_catalog_changed() {
    if let Some(conn) = CONNECTION.get() {
        let _ = conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "CatalogChanged", &());
    }
}

fn emit(signal: &str) {
    if let Some(conn) = CONNECTION.get() {
        let _ = conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, signal, &());
    }
}

/// Playlist timer: once a second advances the countdowns, announces changes.
pub fn spawn_playlist_timer(core: Arc<Mutex<Core>>) {
    std::thread::spawn(move || {
        let mut last = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = std::time::Instant::now();
            let dt = now.duration_since(last).as_secs_f64();
            last = now;
            let changed = core.lock().unwrap().playlist_tick(dt);
            if !changed.is_empty() {
                emit("AssignmentsChanged");
            }
        }
    });
}

pub struct Service {
    pub core: Arc<Mutex<Core>>,
}

fn err(e: String) -> fdo::Error {
    fdo::Error::Failed(e)
}

#[interface(name = "io.github.wpe.Daemon1")]
impl Service {
    /// JSON array of wallpapers: [{id,title,type,supported,status(ok|unsupported|broken),reason,preview,root,tags,audio,rating}].
    fn list_wallpapers(&self) -> String {
        self.core.lock().unwrap().list_json()
    }

    async fn rescan(&self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().rescan();
        Self::catalog_changed(&em).await?;
        Ok(())
    }

    /// Add a directory with wallpapers (or a single wallpaper) and rescan.
    async fn add_directory(&self, path: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().add_directory(path.into()).map_err(err)?;
        Self::catalog_changed(&em).await?;
        Ok(())
    }

    /// Monitor connector name ("DP-1") or "*" for all; empty id clears.
    async fn set_wallpaper(&self, monitor: &str, id: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().set_wallpaper(monitor, id).map_err(err)?;
        Self::assignments_changed(&em).await?;
        Ok(())
    }

    fn get_assignments(&self) -> HashMap<String, String> {
        self.core.lock().unwrap().state.assignments.clone().into_iter().collect()
    }

    /// JSON of we_core::Properties with user overrides applied.
    fn get_properties(&self, id: &str) -> fdo::Result<String> {
        self.core.lock().unwrap().properties_json(id).map_err(err)
    }

    /// `value_json` is a JSON value: true, 3.5, "0.1 0.2 0.3", ...
    async fn set_property(&self, id: &str, key: &str, value_json: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        let v = serde_json::from_str(value_json).map_err(|e| err(e.to_string()))?;
        self.core.lock().unwrap().set_property(id, key, v).map_err(err)?;
        Self::properties_changed(&em, id).await?;
        Ok(())
    }

    async fn reset_properties(&self, id: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().reset_properties(id);
        Self::properties_changed(&em, id).await?;
        Ok(())
    }

    /// From the GNOME extension: pointer over empty desktop. x,y in monitor-local logical px.
    /// kind: "motion" | "press" | "release".
    fn pointer_event(&self, monitor: &str, x: f64, y: f64, kind: &str, button: u32) -> fdo::Result<()> {
        let kind = PointerKind::parse(kind).ok_or_else(|| fdo::Error::InvalidArgs(kind.into()))?;
        self.core.lock().unwrap().renderer.handle(RenderCmd::Pointer { monitor: monitor.into(), x, y, kind, button });
        Ok(())
    }

    /// From the GNOME extension: monitor covered by a maximized/fullscreen
    /// window (or a window over 90% of it), or the screen is locked. The
    /// daemon decides whether to pause (settings).
    fn set_paused(&self, monitor: &str, paused: bool) {
        self.core.lock().unwrap().set_covered(monitor, paused);
    }

    /// From the GNOME extension: a fullscreen app (fullscreen window, or the
    /// focused window covering over 90% of its monitor) is on some monitor.
    fn set_fullscreen_active(&self, active: bool) {
        self.core.lock().unwrap().set_fullscreen_any(active);
    }

    fn set_pause_on_fullscreen_any(&self, on: bool) {
        self.core.lock().unwrap().set_pause_on_fullscreen_any(on);
    }

    /// JSON {fps, volume, muted, pause_on_maximized, pause_on_fullscreen_any, monitors, directories}.
    fn get_settings(&self) -> String {
        self.core.lock().unwrap().settings_json()
    }

    fn set_pause_on_maximized(&self, on: bool) {
        self.core.lock().unwrap().set_pause_on_maximized(on);
    }

    /// Remove a directory added with AddDirectory and rescan.
    async fn remove_directory(&self, path: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().remove_directory(path.into());
        Self::catalog_changed(&em).await?;
        Ok(())
    }

    fn set_volume(&self, volume: f64, muted: bool) {
        let mut c = self.core.lock().unwrap();
        c.state.volume = volume.clamp(0.0, 1.0);
        c.state.muted = muted;
        let _ = c.state.save(&c.state_path);
        let volume = c.state.volume;
        c.renderer.handle(RenderCmd::Volume { volume, muted });
    }

    /// From the extension when the monitor layout changes.
    fn set_monitors(&self, monitors: Vec<String>) {
        let mut c = self.core.lock().unwrap();
        c.monitors = monitors;
        c.apply_all();
    }

    /// Frame rate limit for scene wallpapers (1..240, default 30).
    fn set_fps(&self, fps: u32) -> fdo::Result<()> {
        self.core.lock().unwrap().set_fps(fps).map_err(err)
    }

    fn get_fps(&self) -> u32 {
        self.core.lock().unwrap().state.fps
    }

    /// Monitor connector -> PID of the process rendering it in its own window
    /// (scene wallpapers). Changes are announced by RendererPidsChanged.
    fn get_renderer_pids(&self) -> HashMap<String, u32> {
        RENDERER_PIDS.lock().unwrap().clone().into_iter().collect()
    }

    /// JSON [{id,name,builtin,items}]; "favorites" comes first.
    fn get_collections(&self) -> String {
        self.core.lock().unwrap().collections_json()
    }

    async fn set_favorite(&self, wallpaper_id: &str, on: bool, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().set_favorite(wallpaper_id, on);
        Self::collections_changed(&em).await?;
        Ok(())
    }

    async fn create_collection(&self, name: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<String> {
        let id = self.core.lock().unwrap().create_collection(name);
        Self::collections_changed(&em).await?;
        Ok(id)
    }

    async fn rename_collection(&self, id: &str, name: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().rename_collection(id, name).map_err(err)?;
        Self::collections_changed(&em).await?;
        Ok(())
    }

    async fn delete_collection(&self, id: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().delete_collection(id).map_err(err)?;
        Self::collections_changed(&em).await?;
        Ok(())
    }

    async fn add_to_collection(&self, collection_id: &str, wallpaper_id: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().add_to_collection(collection_id, wallpaper_id).map_err(err)?;
        Self::collections_changed(&em).await?;
        Ok(())
    }

    async fn remove_from_collection(&self, collection_id: &str, wallpaper_id: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().remove_from_collection(collection_id, wallpaper_id).map_err(err)?;
        Self::collections_changed(&em).await?;
        Ok(())
    }

    /// JSON {enabled, source: all|favorites|<collection id>, interval_min, order: shuffle|sequential, sync_monitors}.
    fn get_playlist(&self, monitor: &str) -> String {
        self.core.lock().unwrap().playlist_json(monitor)
    }

    /// `monitor` "*" = all monitors.
    async fn set_playlist(&self, monitor: &str, json: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().set_playlist(monitor, json).map_err(err)?;
        Self::playlist_changed(&em, monitor).await?;
        Ok(())
    }

    /// Switch to the next wallpaper of the playlist now ("*" = all monitors).
    async fn next_wallpaper(&self, monitor: &str, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        let changed = self.core.lock().unwrap().next_wallpaper(monitor);
        if !changed.is_empty() {
            Self::assignments_changed(&em).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    #[zbus(signal)]
    async fn catalog_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn assignments_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn properties_changed(em: &SignalEmitter<'_>, id: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn collections_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn playlist_changed(em: &SignalEmitter<'_>, monitor: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn renderer_pids_changed(em: &SignalEmitter<'_>) -> zbus::Result<()>;
}
