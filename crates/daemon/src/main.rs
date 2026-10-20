#[cfg_attr(not(feature = "gtk"), allow(dead_code))]
mod audio;
mod core;
mod dbus;
#[cfg_attr(not(feature = "gtk"), allow(dead_code))]
mod scene;
mod state;
#[cfg_attr(not(feature = "gtk"), allow(dead_code))]
mod webpath;
#[cfg(feature = "gtk")]
mod gtk_renderer;
#[cfg(feature = "gtk")]
mod video;

use std::sync::{Arc, Mutex};

fn serve(core: Arc<Mutex<core::Core>>) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(dbus::BUS_NAME)?
        .serve_at(dbus::OBJECT_PATH, dbus::Service { core })?
        .build()
        .inspect(|conn| dbus::set_connection(conn.clone()))
}

#[cfg(not(feature = "gtk"))]
fn main() -> zbus::Result<()> {
    // Headless build: D-Bus API + catalog + state, renderer only logs.
    let monitors = std::env::args().skip(1).collect::<Vec<_>>();
    let core = core::Core::new(state::default_path(), monitors, Box::new(core::LogRenderer));
    eprintln!("[wpe-daemon] headless, {} wallpapers", core.catalog.projects.len());
    let core = Arc::new(Mutex::new(core));
    core.lock().unwrap().apply_all();
    let _conn = serve(core)?;
    loop {
        std::thread::park();
    }
}

/// Rendering setup for the NVIDIA proprietary driver, unless overridden in
/// the environment. WebKitGTK disables its DMA-BUF renderer there and sends
/// every frame through shared memory, which the UI process copies on the CPU
/// (~100% of a core for one animated 1440p page); GTK's default Vulkan
/// renderer imports WebKit's DMA-BUFs slowly on that driver as well. Forced
/// DMA-BUF + the GL renderer keeps frames on the GPU (measured: daemon and
/// web process 170% -> under 40% for two animated 60 fps pages).
#[cfg(feature = "gtk")]
fn gpu_env_defaults() {
    if !std::path::Path::new("/proc/driver/nvidia/version").exists() {
        return;
    }
    for (k, v) in [("WEBKIT_FORCE_DMABUF_RENDERER", "1"), ("GSK_RENDERER", "ngl")] {
        if std::env::var_os(k).is_none() {
            // Still single-threaded: GTK/WebKit are not initialized yet.
            std::env::set_var(k, v);
        }
    }
}

#[cfg(feature = "gtk")]
fn main() -> gtk4::glib::ExitCode {
    use gtk4::prelude::*;
    gpu_env_defaults();
    let app = gtk4::Application::builder().application_id("io.github.wpe.Renderer").build();
    app.connect_activate(|app| {
        let _hold = app.hold();
        std::mem::forget(_hold);
        gtk_renderer::set_application(app);
        let display = gtk4::gdk::Display::default().expect("display");
        let core = core::Core::new(state::default_path(), connectors(&display), Box::new(gtk_renderer::GtkRenderer));
        eprintln!("[wpe-daemon] {} wallpapers, monitors {:?}", core.catalog.projects.len(), core.monitors);
        let core = Arc::new(Mutex::new(core));
        watch_extension(core.clone());
        // Hotplug: connectors are filled in asynchronously, so re-read on idle.
        let c2 = core.clone();
        display.monitors().connect_items_changed(move |_, _, _, _| {
            let c3 = c2.clone();
            gtk4::glib::idle_add_local_once(move || {
                let Some(display) = gtk4::gdk::Display::default() else { return };
                let mut c = c3.lock().unwrap();
                let new = connectors(&display);
                if new != c.monitors {
                    for gone in c.monitors.iter().filter(|m| !new.contains(m)).cloned().collect::<Vec<_>>() {
                        c.renderer.handle(core::RenderCmd::Clear { monitor: gone });
                    }
                    c.monitors = new;
                    c.apply_all();
                }
            });
        });
        match serve(core) {
            Ok(conn) => std::mem::forget(conn),
            Err(e) => eprintln!("[wpe-daemon] D-Bus: {e}"),
        }
    });
    app.run_with_args::<&str>(&[])
}

#[cfg(feature = "gtk")]
const EXTENSION_UUID: &str = "wpe@wpe.github.io";

/// `state` of our extension from org.gnome.Shell.Extensions (1 = ACTIVE).
#[cfg(feature = "gtk")]
fn extension_active(conn: &zbus::blocking::Connection) -> bool {
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;
    let reply = conn.call_method(
        Some("org.gnome.Shell.Extensions"),
        "/org/gnome/Shell/Extensions",
        Some("org.gnome.Shell.Extensions"),
        "GetExtensionInfo",
        &(EXTENSION_UUID,),
    );
    let Ok(reply) = reply else { return false };
    let Ok(info) = reply.body().deserialize::<HashMap<String, OwnedValue>>() else { return false };
    info.get("state").and_then(|v| f64::try_from(v.clone()).ok()) == Some(1.0)
}

/// Show wallpapers only while the extension is active; hide them if it is
/// disabled. `WPE_NOHIDE=1` (debug windows) skips the check.
#[cfg(feature = "gtk")]
fn watch_extension(core: Arc<Mutex<core::Core>>) {
    use gtk_renderer::SHELL_READY;
    use std::sync::atomic::Ordering;
    let apply = move |ready: bool| {
        let core = core.clone();
        gtk4::glib::MainContext::default().invoke(move || {
            if ready {
                core.lock().unwrap().apply_all();
            } else {
                gtk_renderer::close_all();
            }
        });
    };
    if std::env::var_os("WPE_NOHIDE").is_some() {
        SHELL_READY.store(true, Ordering::SeqCst);
        apply(true);
        return;
    }
    std::thread::spawn(move || {
        let Ok(conn) = zbus::blocking::Connection::session() else { return };
        let mut warned = false;
        loop {
            let active = extension_active(&conn);
            if active != SHELL_READY.load(Ordering::SeqCst) {
                eprintln!("[wpe-daemon] extension {EXTENSION_UUID} {}", if active { "active: showing wallpapers" } else { "inactive: hiding wallpapers" });
                SHELL_READY.store(active, Ordering::SeqCst);
                apply(active);
            } else if !active && !warned {
                eprintln!("[wpe-daemon] waiting for GNOME Shell extension {EXTENSION_UUID} to be enabled");
            }
            warned = true;
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
}

#[cfg(feature = "gtk")]
fn connectors(display: &gtk4::gdk::Display) -> Vec<String> {
    use gtk4::prelude::*;
    display
        .monitors()
        .iter::<gtk4::gdk::Monitor>()
        .flatten()
        .filter_map(|m| m.connector().map(|c| c.to_string()))
        .collect()
}
