//! GTK4 renderer backend (feature `gtk`). One undecorated window per monitor,
//! sized to the monitor and titled `@wpe!<connector>`. The GNOME extension
//! finds it by title, keeps it minimized and out of the window lists, and
//! shows a Clutter clone of it inside the desktop background actor of that
//! monitor (Hanabi approach). Set `WPE_NOHIDE=1` to get normal decorated
//! windows for debugging without the extension.

use crate::core::{PointerKind, RenderCmd, Renderer};
use gtk4::prelude::*;
use gtk4::{glib, gio};
use std::cell::RefCell;
use std::collections::HashMap;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use we_core::{Project, ProjectType};
use webkit6::prelude::*;

const SHIM: &str = include_str!("shim.js");
use crate::webpath;
use std::collections::HashSet;
use std::path::PathBuf;
use webkit6::soup;

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
    static VOLUME: RefCell<(f64, bool)> = const { RefCell::new((0.5, true)) };
    /// wpe:// host -> wallpaper root directory.
    static ROOTS: RefCell<HashMap<String, PathBuf>> = RefCell::new(HashMap::new());
    /// Latest applyUserProperties JSON per wallpaper id (re-sent after (re)loads).
    static PROPS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Monitors currently paused by the extension.
    static PAUSED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static SCHEME_REGISTERED: RefCell<bool> = const { RefCell::new(false) };
}

/// Windows are only created while the GNOME extension is active; without it
/// they would show up as ordinary windows on top of the desktop.
pub static SHELL_READY: AtomicBool = AtomicBool::new(false);

pub struct GtkRenderer;

impl Renderer for GtkRenderer {
    fn handle(&mut self, cmd: RenderCmd) {
        if matches!(cmd, RenderCmd::Show { .. }) && !SHELL_READY.load(Ordering::SeqCst) {
            return; // re-applied by the extension watcher once it is active
        }
        // D-Bus handlers run on zbus threads; GTK must be touched on the main thread.
        glib::MainContext::default().invoke(move || handle_main(cmd));
    }
}

/// Close every renderer window (extension went away).
pub fn close_all() {
    let monitors: Vec<String> = SURFACES.with(|s| s.borrow().keys().cloned().collect());
    for m in monitors {
        close(&m);
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

fn gdk_monitor(connector: &str) -> Option<gtk4::gdk::Monitor> {
    let display = gtk4::gdk::Display::default()?;
    display
        .monitors()
        .iter::<gtk4::gdk::Monitor>()
        .flatten()
        .find(|m| m.connector().is_some_and(|c| c.as_str() == connector))
}

fn make_window(monitor: &str) -> gtk4::ApplicationWindow {
    let app = APP.with(|a| a.borrow().clone()).expect("application set");
    let (w, h) = gdk_monitor(monitor)
        .map(|m| (m.geometry().width(), m.geometry().height()))
        .unwrap_or((1920, 1080));
    let nohide = std::env::var_os("WPE_NOHIDE").is_some();
    let title = if nohide { format!("WPE renderer {monitor}") } else { format!("@wpe!{monitor}") };
    gtk4::ApplicationWindow::builder()
        .application(&app)
        .title(title)
        .decorated(nohide)
        .default_width(w)
        .default_height(h)
        .build()
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
            let (volume, muted) = VOLUME.with(|v| *v.borrow());
            media.set_volume(volume);
            media.set_muted(muted);
            let pic = gtk4::Picture::for_paintable(&media);
            pic.set_content_fit(gtk4::ContentFit::Cover);
            window.set_child(Some(&pic));
            media.play();
            Content::Video(media)
        }
        ProjectType::Web => {
            ensure_scheme();
            let host = webpath::host_for_root(&project.root);
            ROOTS.with(|r| r.borrow_mut().insert(host.clone(), project.root.clone()));
            PROPS.with(|p| p.borrow_mut().insert(id.clone(), project.properties.to_user_properties().to_string()));
            let view = webkit6::WebView::new();
            view.set_is_muted(VOLUME.with(|v| v.borrow().1));
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
                if std::env::var_os("WPE_DEBUG").is_some() {
                    settings.set_enable_write_console_messages_to_stdout(true);
                    settings.set_enable_developer_extras(true);
                }
            }
            let (wallpaper, mon) = (id.clone(), monitor.clone());
            view.connect_load_changed(move |v, ev| {
                if ev == webkit6::LoadEvent::Finished {
                    if let Some(props) = PROPS.with(|p| p.borrow().get(&wallpaper).cloned()) {
                        web_js(v, &format!("window.__wpe.applyUserProperties({props});"));
                    }
                    if PAUSED.with(|p| p.borrow().contains(&mon)) {
                        web_js(v, "window.__wpe.setPaused(true);");
                    }
                }
            });
            let rel = project.file.as_deref().unwrap_or("index.html");
            view.load_uri(&format!("{}://{host}/{}", webpath::SCHEME, webpath::encode_path(rel)));
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
        window.present();
    }
    SURFACES.with(|s| s.borrow_mut().insert(monitor, Surface { window, wallpaper: id, content }));
}

fn monitor_geometry(connector: &str) -> Option<String> {
    let g = gdk_monitor(connector)?.geometry();
    Some(format!("{}x{}x{}x{}", g.x(), g.y(), g.width(), g.height()))
}

/// Call into the shim; a no-op while the page (and thus the shim) is not loaded yet.
fn web_js(view: &webkit6::WebView, js: &str) {
    let js = format!("if (window.__wpe) {{ {js} }}");
    view.evaluate_javascript(&js, None, None, None::<&gio::Cancellable>, |_| {});
}

fn ensure_scheme() {
    if SCHEME_REGISTERED.with(|r| r.replace(true)) {
        return;
    }
    let Some(ctx) = webkit6::WebContext::default() else { return };
    ctx.register_uri_scheme(webpath::SCHEME, serve_scheme);
    if let Some(sm) = ctx.security_manager() {
        sm.register_uri_scheme_as_secure(webpath::SCHEME);
        sm.register_uri_scheme_as_cors_enabled(webpath::SCHEME);
    }
}

/// Serve `wpe://<host>/<path>` from the wallpaper directory, with byte ranges
/// (needed by media elements for seeking/looping).
fn serve_scheme(req: &webkit6::URISchemeRequest) {
    use gio::prelude::*;
    let uri = req.uri().map(|u| u.to_string()).unwrap_or_default();
    let rest = uri.strip_prefix(&format!("{}://", webpath::SCHEME)).unwrap_or("");
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let file = ROOTS.with(|r| r.borrow().get(host).cloned()).and_then(|root| webpath::resolve(&root, &path));
    let Some(file) = file else {
        req.finish_error(&mut glib::Error::new(gio::IOErrorEnum::NotFound, &format!("not found: {uri}")));
        return;
    };
    let len = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    let stream = match gio::File::for_path(&file).read(None::<&gio::Cancellable>) {
        Ok(s) => s,
        Err(mut e) => return req.finish_error(&mut e),
    };
    let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
    headers.append("Accept-Ranges", "bytes");
    headers.append("Access-Control-Allow-Origin", "*");
    let range = req
        .http_headers()
        .and_then(|h| h.one("Range"))
        .and_then(|r| webpath::parse_range(&r, len));
    let (status, body_len, input): (u32, u64, gio::InputStream) = match range {
        Some((start, end)) => {
            if let Err(mut e) = stream.seek(start as i64, glib::SeekType::Set, None::<&gio::Cancellable>) {
                return req.finish_error(&mut e);
            }
            headers.append("Content-Range", &format!("bytes {start}-{end}/{len}"));
            let n = end - start + 1;
            let input: gio::InputStream = if end + 1 == len {
                stream.upcast()
            } else {
                let mut buf = vec![0u8; n as usize];
                match stream.read_all(&mut buf, None::<&gio::Cancellable>) {
                    Ok((read, _)) => buf.truncate(read),
                    Err(mut e) => return req.finish_error(&mut e),
                }
                gio::MemoryInputStream::from_bytes(&glib::Bytes::from_owned(buf)).upcast()
            };
            (206, n, input)
        }
        None => (200, len, stream.upcast()),
    };
    let resp = webkit6::URISchemeResponse::new(&input, body_len as i64);
    resp.set_status(status, None);
    resp.set_content_type(webpath::mime_for(&file));
    resp.set_http_headers(headers);
    req.finish_with_response(&resp);
}

fn handle_main(cmd: RenderCmd) {
    if std::env::var_os("WPE_DEBUG").is_some() {
        eprintln!("[render] {cmd:?}");
    }
    match cmd {
        RenderCmd::Show { monitor, project } => show(monitor, *project),
        RenderCmd::Clear { monitor } => close(&monitor),
        RenderCmd::Properties { wallpaper, user_properties } => SURFACES.with(|s| {
            PROPS.with(|p| p.borrow_mut().insert(wallpaper.clone(), user_properties.to_string()));
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
            PAUSED.with(|p| {
                let mut p = p.borrow_mut();
                if paused {
                    p.insert(monitor.clone());
                } else {
                    p.remove(&monitor);
                }
            });
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
        RenderCmd::Volume { volume, muted } => {
            VOLUME.with(|v| *v.borrow_mut() = (volume, muted));
            SURFACES.with(|s| {
                for surf in s.borrow().values() {
                    match &surf.content {
                        Content::Video(m) => {
                            m.set_volume(volume);
                            m.set_muted(muted);
                        }
                        Content::Web(v) => v.set_is_muted(muted),
                        Content::Scene(_) => {}
                    }
                }
            })
        }
    }
}
