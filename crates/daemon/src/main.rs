mod core;
mod dbus;
mod state;
#[cfg(feature = "gtk")]
mod gtk_renderer;

use std::sync::{Arc, Mutex};

fn serve(core: Arc<Mutex<core::Core>>) -> zbus::Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::session()?
        .name(dbus::BUS_NAME)?
        .serve_at(dbus::OBJECT_PATH, dbus::Service { core })?
        .build()
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

#[cfg(feature = "gtk")]
fn main() -> gtk4::glib::ExitCode {
    use gtk4::prelude::*;
    let app = gtk4::Application::builder().application_id("io.github.wpe.Renderer").build();
    app.connect_activate(|app| {
        let _hold = app.hold();
        std::mem::forget(_hold);
        gtk_renderer::set_application(app);
        let display = gtk4::gdk::Display::default().expect("display");
        let monitors: Vec<String> = display
            .monitors()
            .iter::<gtk4::gdk::Monitor>()
            .flatten()
            .filter_map(|m| m.connector().map(|c| c.to_string()))
            .collect();
        let core = core::Core::new(state::default_path(), monitors, Box::new(gtk_renderer::GtkRenderer));
        let core = Arc::new(Mutex::new(core));
        core.lock().unwrap().apply_all();
        match serve(core) {
            Ok(conn) => std::mem::forget(conn),
            Err(e) => eprintln!("[wpe-daemon] D-Bus: {e}"),
        }
    });
    app.run_with_args::<&str>(&[])
}
