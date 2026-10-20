//! GTK4 renderer backend (feature `gtk`). One undecorated window per monitor,
//! titled `@wpe!<connector>` so the GNOME extension can find it and move it
//! into the background layer (Hanabi approach).
//!
//! NOTE: written against gtk4 0.11 / webkit6 0.6 but not compiled on the dev
//! server (no GTK dev libraries there) — verify on the desktop.

use crate::core::{PointerKind, RenderCmd, Renderer};
use gtk4::prelude::*;
use gtk4::{glib, gio};
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Child;
use we_core::{Project, ProjectType};
use webkit6::prelude::*;

const SHIM: &str = include_str!("shim.js");

enum Content {
    Video(gtk4::MediaFile),
    Web(webkit6::WebView),
    /// linux-wallpaperengine subprocess (stage 2).
    Scene(Child),
}

struct Surface {
    window: gtk4::ApplicationWindow,
    wallpaper: String,
    content: Content,
}

thread_local! {
    static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
    static APP: RefCell<Option<gtk4::Application>> = const { RefCell::new(None) };
}

pub struct GtkRenderer;

impl Renderer for GtkRenderer {
    fn handle(&mut self, cmd: RenderCmd) {
        // D-Bus handlers run on zbus threads; GTK must be touched on the main thread.
        glib::MainContext::default().invoke(move || handle_main(cmd));
    }
}

pub fn set_application(app: &gtk4::Application) {
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
}

fn close(monitor: &str) {
    SURFACES.with(|s| {
        if let Some(mut surf) = s.borrow_mut().remove(monitor) {
            if let Content::Scene(child) = &mut surf.content {
                let _ = child.kill();
            }
            surf.window.destroy();
        }
    });
}

fn make_window(monitor: &str) -> gtk4::ApplicationWindow {
    let app = APP.with(|a| a.borrow().clone()).expect("application set");
    let w = gtk4::ApplicationWindow::builder()
        .application(&app)
        .title(format!("@wpe!{monitor}"))
        .decorated(false)
        .build();
    w
}

fn show(monitor: String, project: Project) {
    close(&monitor);
    let window = make_window(&monitor);
    let id = project.id();
    let content = match project.kind {
        ProjectType::Video => {
            let path = project.entry_path().expect("validated");
            let media = gtk4::MediaFile::for_file(&gio::File::for_path(path));
            media.set_loop(true);
            media.set_muted(true);
            let pic = gtk4::Picture::for_paintable(&media);
            pic.set_content_fit(gtk4::ContentFit::Cover);
            window.set_child(Some(&pic));
            media.play();
            Content::Video(media)
        }
        ProjectType::Web => {
            let view = webkit6::WebView::new();
            if let Some(ucm) = view.user_content_manager() {
                ucm.add_script(&webkit6::UserScript::new(
                    SHIM,
                    webkit6::UserContentInjectedFrames::TopFrame,
                    webkit6::UserScriptInjectionTime::Start,
                    &[],
                    &[],
                ));
            }
            if let Some(settings) = webkit6::prelude::WebViewExt::settings(&view) {
                settings.set_allow_file_access_from_file_urls(true);
                settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Always);
            }
            let props = project.properties.to_user_properties().to_string();
            view.connect_load_changed(move |v, ev| {
                if ev == webkit6::LoadEvent::Finished {
                    let js = format!("window.__wpe.applyUserProperties({props});");
                    v.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
                }
            });
            let uri = gio::File::for_path(project.entry_path().expect("validated")).uri();
            view.load_uri(&uri);
            window.set_child(Some(&view));
            Content::Web(view)
        }
        ProjectType::Scene => {
            // Stage 2: see docs/ARCHITECTURE.md "Scene integration".
            // Runs in window mode; the extension matches the window by PID
            // (GetRendererPids, stage 2) and moves it into the background.
            // TODO: check geometry syntax against `linux-wallpaperengine --help`.
            let child = std::process::Command::new("linux-wallpaperengine")
                .args(["--silent", "--fps", "30"])
                .arg("--window")
                .arg(monitor_geometry(&monitor).unwrap_or_else(|| "0x0x1920x1080".into()))
                .arg(&project.root)
                .spawn();
            match child {
                Ok(c) => Content::Scene(c),
                Err(e) => {
                    eprintln!("[scene] cannot start linux-wallpaperengine: {e}");
                    return;
                }
            }
        }
        _ => return,
    };
    if !matches!(content, Content::Scene(_)) {
        window.fullscreen();
        window.present();
    }
    SURFACES.with(|s| s.borrow_mut().insert(monitor, Surface { window, wallpaper: id, content }));
}

fn monitor_geometry(connector: &str) -> Option<String> {
    let display = gtk4::gdk::Display::default()?;
    display.monitors().iter::<gtk4::gdk::Monitor>().flatten().find_map(|m| {
        (m.connector()?.as_str() == connector).then(|| {
            let g = m.geometry();
            format!("{}x{}x{}x{}", g.x(), g.y(), g.width(), g.height())
        })
    })
}

fn web_js(view: &webkit6::WebView, js: &str) {
    view.evaluate_javascript(js, None, None, None::<&gio::Cancellable>, |_| {});
}

fn handle_main(cmd: RenderCmd) {
    match cmd {
        RenderCmd::Show { monitor, project } => show(monitor, *project),
        RenderCmd::Clear { monitor } => close(&monitor),
        RenderCmd::Properties { wallpaper, user_properties } => SURFACES.with(|s| {
            for surf in s.borrow().values().filter(|s| s.wallpaper == wallpaper) {
                if let Content::Web(v) = &surf.content {
                    web_js(v, &format!("window.__wpe.applyUserProperties({user_properties});"));
                }
            }
        }),
        RenderCmd::Pointer { monitor, x, y, kind, button } => SURFACES.with(|s| {
            if let Some(Surface { content: Content::Web(v), .. }) = s.borrow().get(&monitor) {
                let k = match kind {
                    PointerKind::Motion => "motion",
                    PointerKind::Press => "press",
                    PointerKind::Release => "release",
                };
                web_js(v, &format!("window.__wpe.pointer('{k}',{x},{y},{button});"));
            }
        }),
        RenderCmd::Pause { monitor, paused } => SURFACES.with(|s| {
            if let Some(surf) = s.borrow().get(&monitor) {
                match &surf.content {
                    Content::Video(m) => m.set_playing(!paused),
                    Content::Web(v) => web_js(v, &format!("window.__wpe.setPaused({paused});")),
                    Content::Scene(c) => {
                        let sig = if paused { "-STOP" } else { "-CONT" };
                        let _ = std::process::Command::new("kill").arg(sig).arg(c.id().to_string()).status();
                    }
                }
            }
        }),
        RenderCmd::Volume { volume, muted } => SURFACES.with(|s| {
            for surf in s.borrow().values() {
                if let Content::Video(m) = &surf.content {
                    m.set_volume(volume);
                    m.set_muted(muted);
                }
            }
        }),
    }
}
