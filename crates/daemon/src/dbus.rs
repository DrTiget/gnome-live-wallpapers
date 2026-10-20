//! D-Bus API `io.github.wpe.Daemon1` at `/io/github/wpe/Daemon`.
//! See docs/ARCHITECTURE.md for the full contract.

use crate::core::{Core, PointerKind, RenderCmd};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::{fdo, interface, object_server::SignalEmitter};

pub const BUS_NAME: &str = "io.github.wpe.Daemon";
pub const OBJECT_PATH: &str = "/io/github/wpe/Daemon";

pub struct Service {
    pub core: Arc<Mutex<Core>>,
}

fn err(e: String) -> fdo::Error {
    fdo::Error::Failed(e)
}

#[interface(name = "io.github.wpe.Daemon1")]
impl Service {
    /// JSON array of wallpapers: [{id,title,type,supported,preview,root,tags,audio,rating}].
    fn list_wallpapers(&self) -> String {
        self.core.lock().unwrap().list_json()
    }

    async fn rescan(&self, #[zbus(signal_emitter)] em: SignalEmitter<'_>) -> fdo::Result<()> {
        self.core.lock().unwrap().rescan();
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

    /// From the GNOME extension: monitor covered by fullscreen/maximized window.
    fn set_paused(&self, monitor: &str, paused: bool) {
        self.core.lock().unwrap().renderer.handle(RenderCmd::Pause { monitor: monitor.into(), paused });
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
}
