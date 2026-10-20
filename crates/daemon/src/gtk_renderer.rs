//! GTK4 renderer backend (feature `gtk`). One undecorated window per monitor,
//! sized to the monitor and titled `@wpe!<connector>`. The GNOME extension
//! finds it by title, keeps it minimized and out of the window lists, and
//! shows a Clutter clone of it inside the desktop background actor of that
//! monitor (Hanabi approach). Set `WPE_NOHIDE=1` to get normal decorated
//! windows for debugging without the extension.
//!
//! Scenes are rendered by a linux-wallpaperengine subprocess in its own
//! window; the extension finds that window by PID (GetRendererPids) and
//! treats it like ours. Pointer and pause go to it over `--control-stdin`.

use crate::core::{PointerKind, RenderCmd, Renderer};
use gtk4::prelude::*;
use gtk4::{glib, gio};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::process::{Child, ChildStdin, Stdio};
use std::rc::Rc;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use we_core::{Project, ProjectType};
use webkit6::prelude::*;

const SHIM: &str = include_str!("shim.js");
use crate::{audio, dbus, scene, webpath};
use std::collections::HashSet;
use std::path::PathBuf;
use webkit6::soup;

enum Content {
    Video(crate::video::Video),
    Web(webkit6::WebView),
    /// linux-wallpaperengine subprocess with its own window.
    Scene(SceneProc),
}

struct SceneProc {
    child: Child,
    stdin: Option<ChildStdin>,
    root: PathBuf,
    /// Cleared by the child watch once the process has exited (and was reaped).
    alive: Rc<Cell<bool>>,
}

impl SceneProc {
    fn send(&mut self, line: &str) {
        if let Some(stdin) = &mut self.stdin {
            if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
                self.stdin = None;
            }
        }
    }

    /// Ask it to quit (also implied by EOF on stdin); kill if it hangs.
    fn stop(mut self) {
        self.send("q\n");
        self.stdin = None;
        let SceneProc { mut child, alive, .. } = self;
        glib::timeout_add_local_once(std::time::Duration::from_secs(3), move || {
            if alive.get() {
                eprintln!("[scene] pid {} did not quit, killing", child.id());
                let _ = child.kill();
            }
        });
    }
}

struct Surface {
    /// Our window; `None` for scenes (the subprocess has its own).
    window: Option<gtk4::ApplicationWindow>,
    wallpaper: String,
    content: Content,
    /// Web page registered a wallpaperRegisterAudioListener.
    audio: bool,
}

thread_local! {
    static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
    static APP: RefCell<Option<gtk4::Application>> = const { RefCell::new(None) };
    static VOLUME: RefCell<(f64, bool)> = const { RefCell::new((0.5, true)) };
    static FPS: Cell<u32> = const { Cell::new(crate::state::DEFAULT_FPS) };
    /// wpe:// host -> wallpaper root directory.
    static ROOTS: RefCell<HashMap<String, PathBuf>> = RefCell::new(HashMap::new());
    /// Latest applyUserProperties JSON per wallpaper id (re-sent after (re)loads).
    static PROPS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Monitors currently paused by the extension.
    static PAUSED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static SCHEME_REGISTERED: RefCell<bool> = const { RefCell::new(false) };
    /// Running system audio capture (only while a visible web wallpaper listens).
    static AUDIO: RefCell<Option<audio::Capture>> = const { RefCell::new(None) };
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
        if let Some(surf) = s.borrow_mut().remove(monitor) {
            // A destroyed view keeps its web process (and the page's timers) alive.
            if let Content::Web(v) = &surf.content {
                v.terminate_web_process();
            }
            if let Some(w) = surf.window {
                w.destroy();
            }
            if let Content::Scene(p) = surf.content {
                dbus::set_renderer_pid(monitor, None);
                p.stop();
            }
        }
    });
    update_audio();
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
    let id = project.id();
    if project.kind == ProjectType::Scene {
        let props = project.properties.to_user_properties();
        PROPS.with(|p| p.borrow_mut().insert(id.clone(), props.to_string()));
        if let Some(p) = start_scene(&monitor, &id, &project.root, &props) {
            SURFACES.with(|s| s.borrow_mut().insert(monitor, Surface { window: None, wallpaper: id, content: Content::Scene(p), audio: false }));
        }
        return;
    }
    let window = make_window(&monitor);
    let content = match project.kind {
        ProjectType::Video => {
            let path = project.entry_path().expect("validated");
            let (volume, muted) = VOLUME.with(|v| *v.borrow());
            let (video, paintable) = crate::video::Video::new(&path, volume, muted);
            if PAUSED.with(|p| p.borrow().contains(&monitor)) {
                video.set_playing(false);
            }
            let pic = gtk4::Picture::for_paintable(&paintable);
            pic.set_content_fit(gtk4::ContentFit::Cover);
            window.set_child(Some(&pic));
            Content::Video(video)
        }
        ProjectType::Web => {
            ensure_scheme();
            let host = webpath::host_for_root(&project.root);
            ROOTS.with(|r| r.borrow_mut().insert(host.clone(), project.root.clone()));
            PROPS.with(|p| p.borrow_mut().insert(id.clone(), project.properties.to_user_properties().to_string()));
            let view = webkit6::WebView::new();
            view.set_is_muted(VOLUME.with(|v| v.borrow().1));
            if let Some(ucm) = view.user_content_manager() {
                // The frame rate limit applies to requestAnimationFrame as well.
                ucm.add_script(&webkit6::UserScript::new(
                    &format!("window.__wpeFps = {};\n{SHIM}", FPS.get()),
                    webkit6::UserContentInjectedFrames::TopFrame,
                    webkit6::UserScriptInjectionTime::Start,
                    &[],
                    &[],
                ));
                ucm.register_script_message_handler("wpe", None);
                let mon = monitor.clone();
                ucm.connect_script_message_received(Some("wpe"), move |_, msg| {
                    if msg.to_str() == "audio" {
                        set_audio_listener(&mon, true);
                    }
                });
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
                if ev == webkit6::LoadEvent::Started {
                    // New document: listeners are registered again by the page.
                    set_audio_listener(&mon, false);
                }
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
        _ => return,
    };
    window.present();
    SURFACES.with(|s| s.borrow_mut().insert(monitor, Surface { window: Some(window), wallpaper: id, content, audio: false }));
}

fn start_scene(monitor: &str, wallpaper: &str, root: &std::path::Path, props: &serde_json::Value) -> Option<SceneProc> {
    let (width, height) = gdk_monitor(monitor)
        .map(|m| (m.geometry().width(), m.geometry().height()))
        .unwrap_or((1920, 1080));
    let (volume, muted) = VOLUME.with(|v| *v.borrow());
    let opts = scene::SceneOptions { width, height, fps: FPS.get(), volume, muted, assets: scene::assets_dir() };
    let bin = scene::binary();
    let args = scene::command_args(root, props, &opts);
    if std::env::var_os("WPE_DEBUG").is_some() {
        eprintln!("[scene] {} {:?}", bin.display(), args);
    }
    let mut child = match std::process::Command::new(&bin)
        .args(&args)
        // Property values (std::stof) and shaders must not see a comma decimal separator.
        .env("LC_NUMERIC", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[scene] cannot start {}: {e} (build it with scripts/build-scene-renderer.sh)", bin.display());
            return None;
        }
    };
    let pid = child.id();
    eprintln!("[scene] {monitor}: pid {pid} {}", root.display());
    // Pass its output through to our log, keeping the tail for the crash reason.
    let tail = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<String>::new()));
    let outputs: [Option<Box<dyn std::io::Read + Send>>; 2] =
        [child.stdout.take().map(|o| Box::new(o) as _), child.stderr.take().map(|e| Box::new(e) as _)];
    for out in outputs.into_iter().flatten() {
        let tail = tail.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                eprintln!("{line}");
                let mut t = tail.lock().unwrap();
                if t.len() == 40 {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });
    }
    let alive = Rc::new(Cell::new(true));
    let (alive2, mon, id) = (alive.clone(), monitor.to_string(), wallpaper.to_string());
    glib::child_watch_add_local(glib::Pid(pid as i32), move |_, status| {
        alive2.set(false);
        eprintln!("[scene] {mon}: pid {pid} exited, status {status}");
        let current = SURFACES.with(|s| {
            matches!(s.borrow().get(&mon), Some(Surface { content: Content::Scene(p), .. }) if p.child.id() == pid)
        });
        if current {
            // Crashed on its own: forget it, the desktop falls back to the normal background.
            SURFACES.with(|s| s.borrow_mut().remove(&mon));
            dbus::set_renderer_pid(&mon, None);
            // Give the output readers a moment to drain, then report it as broken.
            let (tail, id) = (tail.clone(), id.clone());
            glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                let lines: Vec<String> = tail.lock().unwrap().iter().cloned().collect();
                let reason = scene::crash_reason(&lines, status);
                eprintln!("[scene] {id}: broken: {reason}");
                crate::core::RUNTIME_BROKEN.lock().unwrap().insert(id, reason);
                dbus::emit_catalog_changed();
            });
        }
    });
    // Survived the start: no longer broken (e.g. fixed files, other settings).
    let (alive3, id) = (alive.clone(), wallpaper.to_string());
    glib::timeout_add_local_once(std::time::Duration::from_secs(20), move || {
        if alive3.get() && crate::core::RUNTIME_BROKEN.lock().unwrap().remove(&id).is_some() {
            dbus::emit_catalog_changed();
        }
    });
    let mut p = SceneProc { stdin: child.stdin.take(), child, root: root.to_path_buf(), alive };
    if PAUSED.with(|p| p.borrow().contains(monitor)) {
        p.send(scene::pause_line(true));
    }
    dbus::set_renderer_pid(monitor, Some(pid));
    Some(p)
}

/// Restart scene subprocesses (new properties, fps or volume); `only` limits it to one wallpaper.
fn restart_scenes(only: Option<&str>) {
    let targets: Vec<(String, String)> = SURFACES.with(|s| {
        s.borrow()
            .iter()
            .filter(|(_, surf)| matches!(surf.content, Content::Scene(_)) && only.map_or(true, |w| w == surf.wallpaper))
            .map(|(m, surf)| (m.clone(), surf.wallpaper.clone()))
            .collect()
    });
    for (monitor, wallpaper) in targets {
        let Some(Surface { content: Content::Scene(old), .. }) = SURFACES.with(|s| s.borrow_mut().remove(&monitor)) else {
            continue;
        };
        let root = old.root.clone();
        old.stop();
        let props = PROPS
            .with(|p| p.borrow().get(&wallpaper).cloned())
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or(serde_json::Value::Null);
        match start_scene(&monitor, &wallpaper, &root, &props) {
            Some(p) => SURFACES.with(|s| s.borrow_mut().insert(monitor, Surface { window: None, wallpaper, content: Content::Scene(p), audio: false })),
            None => {
                dbus::set_renderer_pid(&monitor, None);
                None
            }
        };
    }
}

fn set_audio_listener(monitor: &str, on: bool) {
    SURFACES.with(|s| {
        if let Some(surf) = s.borrow_mut().get_mut(monitor) {
            surf.audio = on;
        }
    });
    update_audio();
}

/// Web views that should get the spectrum now: listening and not paused.
fn audio_views() -> Vec<webkit6::WebView> {
    let paused = PAUSED.with(|p| p.borrow().clone());
    SURFACES.with(|s| {
        s.borrow()
            .iter()
            .filter(|(m, surf)| surf.audio && !paused.contains(*m))
            .filter_map(|(_, surf)| match &surf.content {
                Content::Web(v) => Some(v.clone()),
                _ => None,
            })
            .collect()
    })
}

/// Start the capture when some visible web wallpaper listens, stop it otherwise.
/// Scenes capture sound themselves (linux-wallpaperengine, pulse API).
fn update_audio() {
    let wanted = !audio_views().is_empty();
    AUDIO.with(|a| {
        let mut a = a.borrow_mut();
        if wanted == a.is_some() {
            return;
        }
        if !wanted {
            *a = None;
            // Listeners keep their last values: let them fall to silence.
            eprintln!("[audio] capture stopped");
            return;
        }
        let deliver = |spectrum: Vec<f32>| {
            glib::MainContext::default().invoke(move || {
                if AUDIO.with(|a| a.borrow().is_none()) {
                    return; // stale frame after stop
                }
                let js = format!("window.__wpe.audio({});", audio::js_array(&spectrum));
                for v in audio_views() {
                    web_js(&v, &js);
                }
            });
        };
        match audio::Capture::start(audio::command(), deliver) {
            Ok(c) => {
                eprintln!("[audio] capture started");
                *a = Some(c);
            }
            Err(e) => eprintln!("[audio] cannot start pw-record: {e}"),
        }
    });
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
        RenderCmd::Properties { wallpaper, user_properties } => {
            let new = user_properties.to_string();
            let changed = PROPS.with(|p| p.borrow_mut().insert(wallpaper.clone(), new.clone())) != Some(new);
            SURFACES.with(|s| {
                for surf in s.borrow().values().filter(|s| s.wallpaper == wallpaper) {
                    if let Content::Web(v) = &surf.content {
                        web_js(v, &format!("window.__wpe.applyUserProperties({user_properties});"));
                    }
                }
            });
            // linux-wallpaperengine takes properties on the command line only.
            if changed {
                restart_scenes(Some(&wallpaper));
            }
        }
        RenderCmd::Pointer { monitor, x, y, kind, button } => SURFACES.with(|s| match s.borrow_mut().get_mut(&monitor) {
            Some(Surface { content: Content::Web(v), .. }) => {
                let k = match kind {
                    PointerKind::Motion => "motion",
                    PointerKind::Press => "press",
                    PointerKind::Release => "release",
                };
                web_js(v, &format!("window.__wpe.pointer('{k}',{x},{y},{button});"));
            }
            Some(Surface { content: Content::Scene(p), .. }) => p.send(&scene::pointer_lines(kind, x, y, button)),
            _ => {}
        }),
        RenderCmd::Pause { monitor, paused } => {
            SURFACES.with(|s| {
            PAUSED.with(|p| {
                let mut p = p.borrow_mut();
                if paused {
                    p.insert(monitor.clone());
                } else {
                    p.remove(&monitor);
                }
            });
            if let Some(surf) = s.borrow_mut().get_mut(&monitor) {
                match &mut surf.content {
                    Content::Video(m) => m.set_playing(!paused),
                    Content::Web(v) => web_js(v, &format!("window.__wpe.setPaused({paused});")),
                    // Not SIGSTOP: a stopped Wayland client can't answer compositor pings.
                    Content::Scene(p) => p.send(scene::pause_line(paused)),
                }
            }
        });
            update_audio();
        }
        RenderCmd::Fps(fps) => {
            SURFACES.with(|s| {
                for surf in s.borrow().values() {
                    if let Content::Web(v) = &surf.content {
                        web_js(v, &format!("window.__wpe.setFps({fps});"));
                    }
                }
            });
            if FPS.replace(fps) != fps {
                restart_scenes(None);
            }
        }
        RenderCmd::Volume { volume, muted } => {
            let old = VOLUME.with(|v| v.replace((volume, muted)));
            SURFACES.with(|s| {
                for surf in s.borrow().values() {
                    match &surf.content {
                        Content::Video(m) => m.set_volume(volume, muted),
                        Content::Web(v) => v.set_is_muted(muted),
                        Content::Scene(_) => {}
                    }
                }
            });
            if old != (volume, muted) {
                restart_scenes(None);
            }
        }
    }
}
