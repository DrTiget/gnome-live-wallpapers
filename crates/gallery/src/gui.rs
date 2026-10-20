//! libadwaita gallery. Layout: sidebar (views, favourites, collections, types)
//! | content: monitor scheme (pick the target screen, playlist status) above a
//! grid of large cards; a card click sets the wallpaper on the target screen,
//! the details dialog shows the big preview, project info and the property
//! editor generated from the daemon's `GetProperties` JSON.
//! Talks to the daemon over D-Bus (blocking calls: they are local and short)
//! and polls it every 2 s, so changes made via CLI or another window show up.
//! Collections/playlist methods are optional: an older daemon without them
//! just hides those features.

use adw::prelude::*;
use libadwaita as adw;
use gtk4 as gtk;
use gtk::{gdk, gio, glib};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};

const APP_ID: &str = "io.github.wpe.Gallery";
const ALL: &str = "*";
const THUMB_W: i32 = 640;
const THUMB_H: i32 = 360;
const CARD_MIN: f64 = 200.0;
const CARD_MAX: f64 = 460.0;
const WORKSHOP_URL: &str = "https://steamcommunity.com/app/431960/workshop/";
const INTERVALS: [(i64, &str); 9] = [(5, "5 минут"), (10, "10 минут"), (15, "15 минут"), (30, "30 минут"), (60, "1 час"), (120, "2 часа"), (240, "4 часа"), (720, "12 часов"), (1440, "Сутки")];

const CSS: &str = r#"
flowboxchild.wp-child { padding: 10px; background: none; border-radius: 18px; }
flowboxchild.wp-child:focus-visible { outline: none; }
flowboxchild.wp-child:focus-visible .wp-card { outline: 3px solid var(--accent-color); outline-offset: 2px; }
.wp-card {
  border-radius: 14px;
  background: var(--card-bg-color);
  box-shadow: 0 1px 3px rgba(0,0,0,.20), 0 4px 14px rgba(0,0,0,.10);
  transition: transform 220ms cubic-bezier(.2,.8,.3,1), box-shadow 220ms ease-out;
}
.wp-card:hover { transform: scale(1.035); box-shadow: 0 10px 28px rgba(0,0,0,.32); }
.wp-card.current { box-shadow: 0 0 0 3px var(--accent-bg-color), 0 6px 18px rgba(0,0,0,.25); }
.wp-card.dimmed { opacity: .5; }
.wp-thumb { background: alpha(currentColor, .08); }
.wp-pill {
  background: rgba(0,0,0,.58); color: white; border-radius: 99px;
  padding: 3px 9px; font-size: 0.78em; font-weight: 700;
}
.wp-pill.accent { background: var(--accent-bg-color); color: var(--accent-fg-color); }
.wp-round { min-width: 34px; min-height: 34px; padding: 0; border-radius: 99px; background: rgba(0,0,0,.50); color: white; }
.wp-round:hover { background: rgba(0,0,0,.72); }
.wp-star.on { color: #f8e45c; }
.wp-hover { opacity: 0; transition: opacity 160ms ease-out; }
.wp-card:hover .wp-hover, .wp-hover.on { opacity: 1; }
.wp-title { font-weight: 700; }
.scheme {
  border-radius: 18px; padding: 14px 18px;
  background: linear-gradient(135deg, alpha(var(--accent-bg-color), .16), alpha(var(--accent-bg-color), .04));
}
.monitor-tile {
  padding: 0; border-radius: 9px; background: #1d1d20;
  border: 3px solid alpha(currentColor, .15);
  transition: border-color 160ms, transform 160ms;
}
.monitor-tile:hover { border-color: alpha(var(--accent-bg-color), .6); }
.monitor-tile.selected { border-color: var(--accent-bg-color); }
.monitor-tile picture { border-radius: 6px; }
.wp-tag { background: alpha(var(--accent-bg-color), .15); color: var(--accent-color); border-radius: 99px; padding: 3px 10px; font-size: .85em; }
.big-preview { border-radius: 14px; background: #000; box-shadow: 0 6px 22px rgba(0,0,0,.30); }
.tile-empty { color: rgba(255,255,255,.6); font-size: .85em; }
.nav-count { font-size: .85em; }
"#;

// ---------------------------------------------------------------- D-Bus

struct Daemon {
    conn: Connection,
}

impl Daemon {
    fn connect() -> Option<Daemon> {
        Connection::session().ok().map(|conn| Daemon { conn })
    }
    fn proxy(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.conn, "io.github.wpe.Daemon", "/io/github/wpe/Daemon", "io.github.wpe.Daemon1")
    }
    fn running(&self) -> bool {
        Proxy::new(&self.conn, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus")
            .and_then(|p| p.call::<_, _, bool>("NameHasOwner", &("io.github.wpe.Daemon",)))
            .unwrap_or(false)
    }
    /// Method returning a string (usually JSON).
    fn string<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) -> Result<String, String> {
        self.proxy().and_then(|p| p.call::<_, _, String>(method, body)).map_err(|e| e.to_string())
    }
    fn json(&self, method: &str) -> Option<Value> {
        serde_json::from_str(&self.string(method, &()).ok()?).ok()
    }
    fn call<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) -> Result<(), String> {
        self.proxy().and_then(|p| p.call::<_, _, ()>(method, body)).map_err(|e| e.to_string())
    }
    fn assignments(&self) -> HashMap<String, String> {
        self.proxy().and_then(|p| p.call("GetAssignments", &())).unwrap_or_default()
    }
}

// ---------------------------------------------------------------- model

#[derive(Clone, PartialEq)]
struct Wp {
    id: String,
    title: String,
    kind: String,
    status: String,
    reason: String,
    preview: Option<String>,
    rating: String,
    root: String,
    tags: Vec<String>,
}

impl Wp {
    fn from_json(v: &Value) -> Wp {
        let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
        Wp {
            id: s("id"),
            title: s("title"),
            kind: s("type"),
            status: if v["status"].is_string() { s("status") } else if v["supported"].as_bool().unwrap_or(true) { "ok".into() } else { "unsupported".into() },
            reason: s("reason"),
            preview: v["preview"].as_str().filter(|p| Path::new(p).is_file()).map(String::from),
            rating: s("rating"),
            root: s("root"),
            tags: v["tags"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect(),
        }
    }
    fn ok(&self) -> bool {
        self.status == "ok"
    }
    fn project(&self) -> Value {
        std::fs::read_to_string(Path::new(&self.root).join("project.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}))
    }
    /// Main file of a video wallpaper (for the hover preview).
    fn video_file(&self) -> Option<PathBuf> {
        if self.kind != "video" {
            return None;
        }
        let f = self.project()["file"].as_str().map(|f| Path::new(&self.root).join(f))?;
        f.is_file().then_some(f)
    }
    fn animated_preview(&self) -> Option<String> {
        if let Some(p) = self.preview.as_deref().filter(|p| p.to_lowercase().ends_with(".gif")) {
            return Some(p.to_string());
        }
        let g = Path::new(&self.root).join("preview.gif");
        g.is_file().then(|| g.to_string_lossy().into_owned())
    }
}

#[derive(Clone, PartialEq)]
struct Coll {
    id: String,
    name: String,
    builtin: bool,
    items: Vec<String>,
}

#[derive(Clone, PartialEq)]
struct Mon {
    name: String,
    desc: String,
    geo: (i32, i32, i32, i32),
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "scene" => "Сцена",
        "web" => "Веб",
        "video" => "Видео",
        "preset" => "Пресет",
        "application" => "Приложение",
        _ => "Другое",
    }
}

fn kind_icon(kind: &str) -> &'static str {
    match kind {
        "scene" => "applications-graphics-symbolic",
        "web" => "web-browser-symbolic",
        "video" => "camera-video-symbolic",
        _ => "image-x-generic-symbolic",
    }
}

fn interval_label(min: i64) -> String {
    INTERVALS.iter().find(|i| i.0 == min).map(|i| i.1.to_string()).unwrap_or_else(|| if min % 60 == 0 { format!("{} ч", min / 60) } else { format!("{min} мин") })
}

/// Center-crop to 16:9 and scale: uniform, light thumbnails (GIF: first frame).
fn load_thumb(path: &str) -> Option<gdk::Texture> {
    let pb = gtk::gdk_pixbuf::Pixbuf::from_file(path).ok()?;
    let (w, h) = (pb.width(), pb.height());
    let (cw, ch) = if w * 9 > h * 16 { (h * 16 / 9, h) } else { (w, w * 9 / 16) };
    let (tw, th) = if cw < THUMB_W { (cw.max(1), ch.max(1)) } else { (THUMB_W, THUMB_H) };
    let sub = pb.new_subpixbuf((w - cw) / 2, (h - ch) / 2, cw.max(1), ch.max(1)).scale_simple(tw, th, gtk::gdk_pixbuf::InterpType::Bilinear)?;
    Some(gdk::Texture::for_pixbuf(&sub))
}

// ---------------------------------------------------------------- UI state

struct Ui {
    win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    grid_stack: gtk::Stack,
    flow: gtk::FlowBox,
    empty: adw::StatusPage,
    nav: gtk::ListBox,
    content_title: adw::WindowTitle,
    content_page: adw::NavigationPage,
    split: adw::NavigationSplitView,
    scheme: gtk::Box,
    scheme_fixed: gtk::Fixed,
    target_title: gtk::Label,
    target_sub: gtk::Label,
    all_btn: gtk::ToggleButton,
    pl_btn: gtk::Button,
    next_btn: gtk::Button,
    search: gtk::SearchEntry,
    sort_dd: gtk::DropDown,
    daemon: RefCell<Option<Daemon>>,
    items: RefCell<Vec<Wp>>,
    list_raw: RefCell<String>,
    assign: RefCell<HashMap<String, String>>,
    colls: RefCell<Vec<Coll>>,
    colls_raw: RefCell<Option<String>>,
    playlists: RefCell<HashMap<String, Value>>,
    pl_ok: Cell<bool>,
    mons: RefCell<Vec<Mon>>,
    scheme_fp: RefCell<String>,
    view: RefCell<String>,
    target: RefCell<String>,
    card_w: Cell<i32>,
    thumbs: RefCell<HashMap<String, Option<gdk::Texture>>>,
    pending: RefCell<VecDeque<(gtk::Picture, String)>>,
    loading: Cell<bool>,
    debounce: RefCell<HashMap<String, glib::SourceId>>,
    building: Cell<bool>,
    nav_building: Cell<bool>,
}

impl Ui {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }
    fn call<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) -> bool {
        let r = self.daemon.borrow().as_ref().map(|d| d.call(method, body)).unwrap_or(Err("демон не запущен".into()));
        if let Err(e) = &r {
            self.toast(&format!("Ошибка: {}", e.rsplit(": ").next().unwrap_or(e)));
        }
        r.is_ok()
    }
    fn string<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) -> Result<String, String> {
        self.daemon.borrow().as_ref().map(|d| d.string(method, body)).unwrap_or(Err("демон не запущен".into()))
    }
    fn settings(&self) -> Value {
        self.daemon.borrow().as_ref().and_then(|d| d.json("GetSettings")).unwrap_or(json!({}))
    }
    fn wp(&self, id: &str) -> Option<Wp> {
        self.items.borrow().iter().find(|w| w.id == id).cloned()
    }
    fn colls_ok(&self) -> bool {
        self.colls_raw.borrow().is_some()
    }
    fn is_fav(&self, id: &str) -> bool {
        self.colls.borrow().iter().any(|c| c.id == "favorites" && c.items.iter().any(|i| i == id))
    }
    /// Wallpaper shown on a monitor: its own assignment or the "all monitors" one.
    fn on_monitor(&self, mon: &str) -> Option<String> {
        let a = self.assign.borrow();
        a.get(mon).or_else(|| a.get(ALL)).filter(|s| !s.is_empty()).cloned()
    }
    /// Monitors on which `id` is currently shown.
    fn active_on(&self, id: &str) -> Vec<String> {
        let mons = self.mons.borrow();
        if mons.is_empty() {
            return if self.assign.borrow().values().any(|w| w == id) { vec!["экран".into()] } else { vec![] };
        }
        mons.iter().filter(|m| self.on_monitor(&m.name).as_deref() == Some(id)).map(|m| m.name.clone()).collect()
    }
    fn target_name(&self, t: &str) -> String {
        if t == ALL { "все экраны".into() } else { t.to_string() }
    }
    fn thumb(&self, path: &str) -> Option<gdk::Texture> {
        if let Some(t) = self.thumbs.borrow().get(path) {
            return t.clone();
        }
        let t = load_thumb(path);
        self.thumbs.borrow_mut().insert(path.into(), t.clone());
        t
    }
}

/// Thumbnails are decoded in idle time, one per main-loop turn: the window shows up at once.
fn queue_thumb(ui: &Rc<Ui>, pic: &gtk::Picture, path: &str) {
    if let Some(t) = ui.thumbs.borrow().get(path) {
        pic.set_paintable(t.as_ref());
        return;
    }
    ui.pending.borrow_mut().push_back((pic.clone(), path.to_string()));
    if ui.loading.replace(true) {
        return;
    }
    let u = ui.clone();
    glib::idle_add_local(move || {
        let next = u.pending.borrow_mut().pop_front();
        match next {
            Some((pic, path)) => {
                let t = u.thumb(&path);
                pic.set_paintable(t.as_ref());
                glib::ControlFlow::Continue
            }
            None => {
                u.loading.set(false);
                glib::ControlFlow::Break
            }
        }
    });
}

fn clear_box(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

/// Overlay exactly `w`×`h` with `pic` filling it: a Picture would otherwise
/// ask for its texture size; overlays don't take part in measuring.
fn framed(pic: &gtk::Picture, w: i32, h: i32) -> gtk::Overlay {
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    base.set_size_request(w, h);
    let ov = gtk::Overlay::new();
    ov.set_child(Some(&base));
    ov.add_overlay(pic);
    ov
}

fn pill(text: &str, accent: bool) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("wp-pill");
    if accent {
        l.add_css_class("accent");
    }
    l
}

// ---------------------------------------------------------------- hover previews

/// Animated preview while the pointer is over `pic`: preview.gif frames or the
/// video itself (muted); the static thumbnail comes back on leave.
fn attach_hover_preview(ui: &Rc<Ui>, hover_on: &impl IsA<gtk::Widget>, pic: &gtk::Picture, w: &Wp) {
    let gif = w.animated_preview();
    let video = if gif.is_none() { w.video_file() } else { None };
    if gif.is_none() && video.is_none() {
        return;
    }
    let timer: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let media: Rc<RefCell<Option<gtk::MediaFile>>> = Rc::new(RefCell::new(None));
    let motion = gtk::EventControllerMotion::new();
    {
        let (pic, timer, media, gif, video) = (pic.clone(), timer.clone(), media.clone(), gif.clone(), video.clone());
        motion.connect_enter(move |_, _, _| {
            if let Some(t) = timer.borrow_mut().take() {
                t.remove();
            }
            let (pic2, media2, gif, video, timer2) = (pic.clone(), media.clone(), gif.clone(), video.clone(), timer.clone());
            // Short delay: sweeping the pointer across the grid starts nothing.
            let id = glib::timeout_add_local_once(Duration::from_millis(300), move || {
                timer2.borrow_mut().take();
                if let Some(g) = gif {
                    if let Some(src) = play_gif(&pic2, &g) {
                        *timer2.borrow_mut() = Some(src);
                    }
                } else if let Some(v) = video {
                    let m = gtk::MediaFile::for_filename(&v);
                    m.set_muted(true);
                    m.set_loop(true);
                    m.play();
                    pic2.set_paintable(Some(&m));
                    *media2.borrow_mut() = Some(m);
                }
            });
            *timer.borrow_mut() = Some(id);
        });
    }
    {
        let (pic, ui, path) = (pic.clone(), ui.clone(), w.preview.clone());
        motion.connect_leave(move |_| {
            if let Some(t) = timer.borrow_mut().take() {
                t.remove();
            }
            if let Some(m) = media.borrow_mut().take() {
                m.pause();
            }
            pic.set_paintable(path.as_deref().and_then(|p| ui.thumb(p)).as_ref());
        });
    }
    hover_on.add_controller(motion);
}

/// Step a GIF on `pic`; the returned source keeps advancing it until removed.
fn play_gif(pic: &gtk::Picture, path: &str) -> Option<glib::SourceId> {
    let anim = gtk::gdk_pixbuf::PixbufAnimation::from_file(path).ok()?;
    if anim.is_static_image() {
        return None;
    }
    let it = anim.iter(Some(std::time::SystemTime::now()));
    pic.set_paintable(Some(&gdk::Texture::for_pixbuf(&it.pixbuf())));
    let pic = pic.downgrade();
    Some(glib::timeout_add_local(Duration::from_millis(40), move || {
        let Some(pic) = pic.upgrade() else { return glib::ControlFlow::Break };
        if it.advance(std::time::SystemTime::now()) {
            pic.set_paintable(Some(&gdk::Texture::for_pixbuf(&it.pixbuf())));
        }
        glib::ControlFlow::Continue
    }))
}

// ---------------------------------------------------------------- cards

fn card_menu(ui: &Rc<Ui>, id: &str) -> gio::Menu {
    let menu = gio::Menu::new();
    let w = ui.wp(id);
    let ok = w.as_ref().map(|w| w.ok()).unwrap_or(false);
    let sec = gio::Menu::new();
    if ok {
        let t = ui.target.borrow().clone();
        sec.append(Some(&format!("Поставить на {}", ui.target_name(&t))), Some(&format!("win.apply::{t}\t{id}")));
        let sub = gio::Menu::new();
        sub.append(Some("Все экраны"), Some(&format!("win.apply::{ALL}\t{id}")));
        for m in ui.mons.borrow().iter() {
            let label = if m.desc.is_empty() { m.name.clone() } else { format!("{} — {}", m.name, m.desc) };
            sub.append(Some(&label), Some(&format!("win.apply::{}\t{id}", m.name)));
        }
        sec.append_submenu(Some("Поставить на экран"), &sub);
    }
    sec.append(Some("Подробнее и настройки"), Some(&format!("win.details::{id}")));
    menu.append_section(None, &sec);
    if ui.colls_ok() {
        let sec = gio::Menu::new();
        if ui.is_fav(id) {
            sec.append(Some("Убрать из избранного"), Some(&format!("win.fav::0\t{id}")));
        } else {
            sec.append(Some("В избранное"), Some(&format!("win.fav::1\t{id}")));
        }
        let sub = gio::Menu::new();
        for c in ui.colls.borrow().iter().filter(|c| !c.builtin) {
            let inside = c.items.iter().any(|i| i == id);
            let label = if inside { format!("✓ {}", c.name) } else { c.name.clone() };
            sub.append(Some(&label), Some(&format!("win.coll-toggle::{}\t{id}", c.id)));
        }
        sub.append(Some("Новая коллекция…"), Some(&format!("win.coll-new::{id}")));
        sec.append_submenu(Some("Коллекции"), &sub);
        menu.append_section(None, &sec);
    }
    let sec = gio::Menu::new();
    sec.append(Some("Открыть папку"), Some(&format!("win.open-dir::{id}")));
    menu.append_section(None, &sec);
    menu
}

fn popup_menu(ui: &Rc<Ui>, on: &gtk::Widget, id: &str, at: Option<(f64, f64)>) {
    let pop = gtk::PopoverMenu::from_model(Some(&card_menu(ui, id)));
    pop.set_parent(on);
    if let Some((x, y)) = at {
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
    }
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

fn make_card(ui: &Rc<Ui>, w: &Wp) -> gtk::FlowBoxChild {
    let cw = ui.card_w.get();
    let ch = cw * 9 / 16;
    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.add_css_class("wp-thumb");
    if let Some(p) = &w.preview {
        queue_thumb(ui, &pic, p);
    }
    let overlay = framed(&pic, cw, ch);

    // Top-left: type, current-screen badges.
    let tl = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tl.set_halign(gtk::Align::Start);
    tl.set_valign(gtk::Align::Start);
    tl.set_margin_start(10);
    tl.set_margin_top(10);
    tl.append(&pill(kind_label(&w.kind), false));
    let active = ui.active_on(&w.id);
    if !active.is_empty() {
        let txt = if active.len() == ui.mons.borrow().len() && active.len() > 1 { "✓ На всех экранах".to_string() } else { format!("✓ {}", active.join(", ")) };
        tl.append(&pill(&txt, true));
    }
    overlay.add_overlay(&tl);

    // Top-right: favourite star and the menu.
    let tr = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    tr.set_halign(gtk::Align::End);
    tr.set_valign(gtk::Align::Start);
    tr.set_margin_end(10);
    tr.set_margin_top(10);
    if ui.colls_ok() {
        let fav = ui.is_fav(&w.id);
        let star = gtk::Button::from_icon_name(if fav { "starred-symbolic" } else { "non-starred-symbolic" });
        star.add_css_class("wp-round");
        star.add_css_class("wp-star");
        star.set_tooltip_text(Some(if fav { "Убрать из избранного" } else { "В избранное" }));
        if fav {
            star.add_css_class("on");
        } else {
            star.add_css_class("wp-hover");
        }
        let (u, id) = (ui.clone(), w.id.clone());
        star.connect_clicked(move |_| set_fav(&u, &id, !u.is_fav(&id)));
        tr.append(&star);
    }
    let more = gtk::Button::from_icon_name("view-more-symbolic");
    more.add_css_class("wp-round");
    more.add_css_class("wp-hover");
    more.set_tooltip_text(Some("Действия"));
    let (u, id) = (ui.clone(), w.id.clone());
    more.connect_clicked(move |b| popup_menu(&u, b.upcast_ref(), &id, None));
    tr.append(&more);
    overlay.add_overlay(&tr);

    // Bottom-right on hover: details.
    let info = gtk::Button::from_icon_name("emblem-system-symbolic");
    info.add_css_class("wp-round");
    info.add_css_class("wp-hover");
    info.set_tooltip_text(Some("Подробнее и настройки"));
    info.set_halign(gtk::Align::End);
    info.set_valign(gtk::Align::End);
    info.set_margin_end(10);
    info.set_margin_bottom(10);
    let (u, id) = (ui.clone(), w.id.clone());
    info.connect_clicked(move |_| details_dialog(&u, &id));
    overlay.add_overlay(&info);

    let title = gtk::Label::new(Some(&w.title));
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.set_lines(2);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(1);
    title.set_hexpand(true);
    title.set_xalign(0.0);
    title.add_css_class("wp-title");
    let sub_text = if !w.ok() {
        if w.status == "broken" { "Не работает".to_string() } else { "Не поддерживается".to_string() }
    } else if !w.tags.is_empty() {
        w.tags.join(" · ")
    } else {
        kind_label(&w.kind).to_string()
    };
    let sub = gtk::Label::new(Some(&sub_text));
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    sub.set_max_width_chars(1);
    sub.add_css_class("caption");
    sub.add_css_class(if w.ok() { "dim-label" } else { "error" });
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_margin_start(14);
    text.set_margin_end(14);
    text.set_margin_top(10);
    text.set_margin_bottom(12);
    text.append(&title);
    text.append(&sub);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("wp-card");
    card.set_overflow(gtk::Overflow::Hidden);
    card.set_size_request(cw, -1);
    card.append(&overlay);
    card.append(&text);
    if !active.is_empty() {
        card.add_css_class("current");
    }
    if !w.ok() {
        card.add_css_class("dimmed");
    }
    attach_hover_preview(ui, &card, &pic, w);
    let rc = gtk::GestureClick::new();
    rc.set_button(3);
    let (u, id, c2) = (ui.clone(), w.id.clone(), card.clone());
    rc.connect_pressed(move |g, _, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        popup_menu(&u, c2.upcast_ref(), &id, Some((x, y)));
    });
    card.add_controller(rc);

    let child = gtk::FlowBoxChild::new();
    child.add_css_class("wp-child");
    child.set_child(Some(&card));
    child.set_widget_name(&w.id);
    let tip = if w.ok() { format!("{}\nЩелчок — поставить на {}", w.title, ui.target_name(&ui.target.borrow())) } else { format!("{}\n{}", w.title, w.reason) };
    child.set_tooltip_text(Some(&tip));
    child
}

fn repopulate(ui: &Rc<Ui>) {
    ui.pending.borrow_mut().clear();
    while let Some(c) = ui.flow.first_child() {
        ui.flow.remove(&c);
    }
    for w in ui.items.borrow().iter() {
        ui.flow.append(&make_card(ui, w));
    }
    refilter(ui);
}

fn refilter(ui: &Rc<Ui>) {
    ui.flow.invalidate_filter();
    ui.flow.invalidate_sort();
    update_empty(ui);
}

fn update_empty(ui: &Rc<Ui>) {
    let n = ui.items.borrow().iter().filter(|w| ui.wp_visible(w)).count();
    let view = ui.view.borrow().clone();
    let (title, sub) = view_title(ui, &view);
    ui.content_title.set_title(&title);
    ui.content_page.set_title(&title);
    ui.content_title.set_subtitle(&if n == 0 { sub } else { format!("{n} {} · {sub}", plural(n, "обои", "обоев", "обоев")) });
    if n > 0 {
        ui.grid_stack.set_visible_child_name("grid");
        return;
    }
    ui.grid_stack.set_visible_child_name("empty");
    let e = &ui.empty;
    e.set_child(None::<&gtk::Widget>);
    if !ui.search.text().is_empty() {
        e.set_icon_name(Some("system-search-symbolic"));
        e.set_title("Ничего не найдено");
        e.set_description(Some("Попробуйте другое название или тег."));
    } else if ui.items.borrow().is_empty() {
        e.set_icon_name(Some("preferences-desktop-wallpaper-symbolic"));
        e.set_title("Обоев пока нет");
        e.set_description(Some("Подпишитесь на обои в Steam Workshop для Wallpaper Engine — Steam скачает их сам, а галерея найдёт автоматически. Можно также добавить папку с обоями (видео, веб-страницы, сцены)."));
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.set_halign(gtk::Align::Center);
        let ws = gtk::Button::with_label("Открыть Steam Workshop");
        ws.add_css_class("pill");
        ws.add_css_class("suggested-action");
        let u = ui.clone();
        ws.connect_clicked(move |_| { gtk::UriLauncher::new(WORKSHOP_URL).launch(Some(&u.win), gio::Cancellable::NONE, |_| {}); });
        let add = gtk::Button::with_label("Добавить папку…");
        add.add_css_class("pill");
        add.set_action_name(Some("win.add-dir"));
        b.append(&ws);
        b.append(&add);
        e.set_child(Some(&b));
    } else if view == "fav" {
        e.set_icon_name(Some("starred-symbolic"));
        e.set_title("В избранном пусто");
        e.set_description(Some("Нажмите звёздочку на карточке обоев, чтобы они появились здесь."));
    } else if view.starts_with("c:") {
        e.set_icon_name(Some("folder-symbolic"));
        e.set_title("Коллекция пуста");
        e.set_description(Some("Добавляйте обои через меню карточки (⋮ или правый щелчок) → «Коллекции»."));
    } else if view == "cur" {
        e.set_icon_name(Some("video-display-symbolic"));
        e.set_title("Обои не установлены");
        e.set_description(Some("Выберите экран на схеме и щёлкните по любым обоям."));
    } else {
        e.set_icon_name(Some("image-x-generic-symbolic"));
        e.set_title("Здесь пусто");
        e.set_description(Some("Обоев этого типа нет в библиотеке."));
    }
}

fn plural(n: usize, one: &'static str, few: &'static str, many: &'static str) -> &'static str {
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 { one } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) { few } else { many }
}

fn view_title(ui: &Ui, view: &str) -> (String, String) {
    match view {
        "all" => ("Все обои".into(), "библиотека".into()),
        "fav" => ("Избранное".into(), "отмеченные звёздочкой".into()),
        "cur" => ("На экранах".into(), "установленные сейчас".into()),
        "t:scene" => ("Сцены".into(), "Wallpaper Engine".into()),
        "t:web" => ("Веб".into(), "HTML-обои".into()),
        "t:video" => ("Видео".into(), "видеообои".into()),
        "t:bad" => ("Неподдерживаемые".into(), "пока не работают".into()),
        v => {
            let id = v.strip_prefix("c:").unwrap_or(v);
            let name = ui.colls.borrow().iter().find(|c| c.id == id).map(|c| c.name.clone()).unwrap_or_default();
            (name, "коллекция".into())
        }
    }
}

impl Ui {
    fn wp_visible(&self, w: &Wp) -> bool {
        let view = self.view.borrow();
        let ok_needed = view.as_str() != "t:bad";
        if ok_needed && !w.ok() {
            return false;
        }
        let q = self.search.text().to_lowercase();
        if !q.is_empty() && !w.title.to_lowercase().contains(&q) && !w.tags.iter().any(|t| t.to_lowercase().contains(&q)) {
            return false;
        }
        match view.as_str() {
            "all" => true,
            "fav" => self.is_fav(&w.id),
            "cur" => !self.active_on(&w.id).is_empty(),
            "t:scene" => w.kind == "scene",
            "t:web" => w.kind == "web",
            "t:video" => w.kind == "video",
            "t:bad" => !w.ok(),
            v => {
                let id = v.strip_prefix("c:").unwrap_or(v);
                self.colls.borrow().iter().any(|c| c.id == id && c.items.iter().any(|i| i == &w.id))
            }
        }
    }
}

// ---------------------------------------------------------------- sidebar

fn nav_row(key: &str, icon: &str, label: &str, count: Option<usize>) -> gtk::ListBoxRow {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    b.append(&gtk::Image::from_icon_name(icon));
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    b.append(&l);
    if let Some(n) = count {
        let c = gtk::Label::new(Some(&n.to_string()));
        c.add_css_class("dim-label");
        c.add_css_class("nav-count");
        b.append(&c);
    }
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&b));
    row.set_widget_name(key);
    row
}

fn nav_header(text: &str) -> gtk::ListBoxRow {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("heading");
    l.add_css_class("dim-label");
    l.set_margin_top(14);
    l.set_margin_start(6);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&l));
    row.set_activatable(false);
    row.set_selectable(false);
    row
}

fn rebuild_nav(ui: &Rc<Ui>) {
    ui.nav_building.set(true);
    while let Some(c) = ui.nav.first_child() {
        ui.nav.remove(&c);
    }
    let items = ui.items.borrow().clone();
    let ok: Vec<&Wp> = items.iter().filter(|w| w.ok()).collect();
    let count = |f: &dyn Fn(&Wp) -> bool| ok.iter().filter(|w| f(w)).count();
    ui.nav.append(&nav_row("all", "view-grid-symbolic", "Все обои", Some(ok.len())));
    if ui.colls_ok() {
        ui.nav.append(&nav_row("fav", "starred-symbolic", "Избранное", Some(count(&|w| ui.is_fav(&w.id)))));
    }
    ui.nav.append(&nav_row("cur", "video-display-symbolic", "На экранах", Some(count(&|w| !ui.active_on(&w.id).is_empty()))));
    if ui.colls_ok() {
        ui.nav.append(&nav_header("Коллекции"));
        for c in ui.colls.borrow().iter().filter(|c| !c.builtin) {
            let row = nav_row(&format!("c:{}", c.id), "folder-symbolic", &c.name, Some(c.items.len()));
            let rc = gtk::GestureClick::new();
            rc.set_button(3);
            let (u, id, r) = (ui.clone(), c.id.clone(), row.clone());
            rc.connect_pressed(move |_, _, x, y| {
                let m = gio::Menu::new();
                m.append(Some("Переименовать…"), Some(&format!("win.coll-rename::{id}")));
                m.append(Some("Удалить"), Some(&format!("win.coll-delete::{id}")));
                let pop = gtk::PopoverMenu::from_model(Some(&m));
                pop.set_parent(&r);
                pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                pop.set_has_arrow(false);
                pop.connect_closed(|p| { let p = p.clone(); glib::idle_add_local_once(move || p.unparent()); });
                pop.popup();
                let _ = &u;
            });
            row.add_controller(rc);
            ui.nav.append(&row);
        }
        ui.nav.append(&nav_row("new-coll", "list-add-symbolic", "Новая коллекция…", None));
    }
    ui.nav.append(&nav_header("Типы"));
    ui.nav.append(&nav_row("t:scene", kind_icon("scene"), "Сцены", Some(count(&|w| w.kind == "scene"))));
    ui.nav.append(&nav_row("t:web", kind_icon("web"), "Веб", Some(count(&|w| w.kind == "web"))));
    ui.nav.append(&nav_row("t:video", kind_icon("video"), "Видео", Some(count(&|w| w.kind == "video"))));
    let bad = items.iter().filter(|w| !w.ok()).count();
    if bad > 0 {
        ui.nav.append(&nav_row("t:bad", "dialog-warning-symbolic", "Неподдерживаемые", Some(bad)));
    }
    // Keep the selection; a deleted collection falls back to "all".
    let view = ui.view.borrow().clone();
    let mut found = None;
    let mut c = ui.nav.first_child();
    while let Some(w) = c {
        if w.widget_name() == view.as_str() {
            found = w.clone().downcast::<gtk::ListBoxRow>().ok();
        }
        c = w.next_sibling();
    }
    if found.is_none() {
        *ui.view.borrow_mut() = "all".into();
        found = ui.nav.row_at_index(0);
    }
    ui.nav.select_row(found.as_ref());
    ui.nav_building.set(false);
}

// ---------------------------------------------------------------- monitor scheme

fn gdk_monitors() -> Vec<Mon> {
    let Some(d) = gdk::Display::default() else { return vec![] };
    let list = d.monitors();
    (0..list.n_items())
        .filter_map(|i| list.item(i)?.downcast::<gdk::Monitor>().ok())
        .filter_map(|m| {
            let g = m.geometry();
            Some(Mon { name: m.connector()?.to_string(), desc: m.description().map(|s| s.to_string()).unwrap_or_default(), geo: (g.x(), g.y(), g.width(), g.height()) })
        })
        .collect()
}

/// Monitors in the daemon's list with real geometry from GDK; monitors only
/// the daemon knows about are laid out in a row after them.
fn current_monitors(daemon_mons: &[String]) -> Vec<Mon> {
    let mut g = gdk_monitors();
    if !daemon_mons.is_empty() {
        g.retain(|m| daemon_mons.contains(&m.name));
        let mut x = g.iter().map(|m| m.geo.0 + m.geo.2).max().unwrap_or(0);
        for n in daemon_mons {
            if !g.iter().any(|m| &m.name == n) {
                g.push(Mon { name: n.clone(), desc: String::new(), geo: (x, 0, 1920, 1080) });
                x += 1920;
            }
        }
    }
    g.sort_by_key(|m| (m.geo.0, m.geo.1));
    g
}

fn rebuild_scheme(ui: &Rc<Ui>) {
    let mons = ui.mons.borrow().clone();
    while let Some(c) = ui.scheme_fixed.first_child() {
        ui.scheme_fixed.remove(&c);
    }
    let target = ui.target.borrow().clone();
    if target != ALL && !mons.iter().any(|m| m.name == target) {
        *ui.target.borrow_mut() = ALL.into();
        return rebuild_scheme(ui);
    }
    ui.scheme.set_visible(!mons.is_empty());
    if mons.is_empty() {
        return;
    }
    let (x0, y0) = (mons.iter().map(|m| m.geo.0).min().unwrap(), mons.iter().map(|m| m.geo.1).min().unwrap());
    let x1 = mons.iter().map(|m| m.geo.0 + m.geo.2).max().unwrap();
    let y1 = mons.iter().map(|m| m.geo.1 + m.geo.3).max().unwrap();
    let scale = (480.0 / (x1 - x0) as f64).min(140.0 / (y1 - y0) as f64);
    ui.scheme_fixed.set_size_request(((x1 - x0) as f64 * scale) as i32, ((y1 - y0) as f64 * scale) as i32);
    for m in &mons {
        let (w, h) = (((m.geo.2 as f64) * scale) as i32 - 8, ((m.geo.3 as f64) * scale) as i32 - 8);
        let pic = gtk::Picture::new();
        pic.set_content_fit(gtk::ContentFit::Cover);
        let wp = ui.on_monitor(&m.name).and_then(|id| ui.wp(&id));
        if let Some(p) = wp.as_ref().and_then(|w| w.preview.clone()) {
            queue_thumb(ui, &pic, &p);
        }
        let ov = framed(&pic, w.max(20) - 6, h.max(20) - 6);
        let lab = pill(&format!("{} · {}×{}", m.name, m.geo.2, m.geo.3), false);
        lab.set_halign(gtk::Align::Start);
        lab.set_valign(gtk::Align::End);
        lab.set_margin_start(6);
        lab.set_margin_bottom(6);
        lab.set_ellipsize(gtk::pango::EllipsizeMode::End);
        ov.add_overlay(&lab);
        if let Some(pl) = ui.playlists.borrow().get(&m.name).filter(|p| p["enabled"].as_bool().unwrap_or(false)) {
            let ind = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            ind.add_css_class("wp-pill");
            ind.add_css_class("accent");
            ind.append(&gtk::Image::from_icon_name(if pl["order"] == "sequential" { "media-playlist-repeat-symbolic" } else { "media-playlist-shuffle-symbolic" }));
            ind.append(&gtk::Label::new(Some(&interval_label(pl["interval_min"].as_i64().unwrap_or(30)))));
            ind.set_halign(gtk::Align::End);
            ind.set_valign(gtk::Align::Start);
            ind.set_margin_end(6);
            ind.set_margin_top(6);
            ind.set_tooltip_text(Some("Обои меняются по таймеру"));
            ov.add_overlay(&ind);
        }
        if wp.is_none() {
            let l = gtk::Label::new(Some("Нет обоев"));
            l.add_css_class("tile-empty");
            ov.add_overlay(&l);
        }
        ov.set_can_target(false); // the whole tile is the button
        let btn = gtk::Button::new();
        btn.add_css_class("monitor-tile");
        btn.set_child(Some(&ov));
        btn.set_size_request(w.max(20), h.max(20));
        if target == ALL || target == m.name {
            btn.add_css_class("selected");
        }
        let tip = format!("{}{}\n{}", m.name, if m.desc.is_empty() { String::new() } else { format!(" — {}", m.desc) }, wp.map(|w| w.title).unwrap_or_else(|| "обои не выбраны".into()));
        btn.set_tooltip_text(Some(&tip));
        let (u, name) = (ui.clone(), m.name.clone());
        btn.connect_clicked(move |_| set_target(&u, &name));
        ui.scheme_fixed.put(&btn, (m.geo.0 - x0) as f64 * scale + 4.0, (m.geo.1 - y0) as f64 * scale + 4.0);
    }
    update_target_info(ui);
}

fn set_target(ui: &Rc<Ui>, t: &str) {
    *ui.target.borrow_mut() = t.to_string();
    rebuild_scheme(ui);
    let mut c = ui.flow.first_child();
    let name = ui.target_name(t);
    while let Some(w) = c {
        if let Some(wp) = ui.wp(&w.widget_name()).filter(|w| w.ok()) {
            w.set_tooltip_text(Some(&format!("{}\nЩелчок — поставить на {name}", wp.title)));
        }
        c = w.next_sibling();
    }
}

fn update_target_info(ui: &Rc<Ui>) {
    let t = ui.target.borrow().clone();
    ui.building.set(true);
    ui.all_btn.set_active(t == ALL);
    ui.building.set(false);
    let n = ui.mons.borrow().len();
    if t == ALL {
        ui.target_title.set_label("Все экраны");
        ui.target_sub.set_label(&format!("Щелчок по обоям поставит их на {n} {}. Выберите экран на схеме, чтобы менять его отдельно.", plural(n, "экран", "экрана", "экранов")));
    } else {
        let desc = ui.mons.borrow().iter().find(|m| m.name == t).map(|m| m.desc.clone()).unwrap_or_default();
        ui.target_title.set_label(&if desc.is_empty() { t.clone() } else { format!("{t} — {desc}") });
        let cur = ui.on_monitor(&t).and_then(|id| ui.wp(&id)).map(|w| format!("Сейчас: «{}». ", w.title)).unwrap_or_default();
        ui.target_sub.set_label(&format!("{cur}Щелчок по обоям поставит их на этот экран."));
    }
    let pl = ui.playlists.borrow().get(&t).cloned();
    ui.pl_btn.set_visible(ui.pl_ok.get());
    let on = pl.as_ref().map(|p| p["enabled"].as_bool().unwrap_or(false)).unwrap_or(false);
    ui.pl_btn.set_child(Some(&adw::ButtonContent::builder().icon_name("media-playlist-shuffle-symbolic").label(if on { format!("Каждые {}", interval_label(pl.unwrap()["interval_min"].as_i64().unwrap_or(30))) } else { "Смена по таймеру…".into() }).build()));
    ui.next_btn.set_visible(on);
}

// ---------------------------------------------------------------- refresh

fn refresh(ui: &Rc<Ui>, force: bool) {
    let up = {
        let mut d = ui.daemon.borrow_mut();
        if d.is_none() {
            *d = Daemon::connect();
        }
        d.as_ref().map(|d| d.running()).unwrap_or(false)
    };
    if !up {
        ui.stack.set_visible_child_name("down");
        ui.list_raw.borrow_mut().clear();
        return;
    }
    let (raw, assign, dmons, colls) = {
        let d = ui.daemon.borrow();
        let d = d.as_ref().unwrap();
        let raw = d.string("ListWallpapers", &()).unwrap_or_default();
        let st = d.json("GetSettings").unwrap_or(json!({}));
        let mons: Vec<String> = st["monitors"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(String::from)).collect();
        (raw, d.assignments(), mons, d.string("GetCollections", &()).ok())
    };
    let mons = current_monitors(&dmons);
    let mut playlists = HashMap::new();
    let mut pl_ok = true;
    for m in mons.iter().map(|m| m.name.as_str()).chain([ALL]) {
        match ui.string("GetPlaylist", &(m,)) {
            Ok(s) => {
                playlists.insert(m.to_string(), serde_json::from_str::<Value>(&s).unwrap_or(json!({})));
            }
            Err(_) => {
                pl_ok = false;
                break;
            }
        }
    }
    if ui.stack.visible_child_name().as_deref() != Some("gallery") {
        ui.stack.set_visible_child_name("gallery");
    }
    let items_changed = raw != *ui.list_raw.borrow();
    let colls_changed = colls != *ui.colls_raw.borrow();
    let assign_changed = *ui.assign.borrow() != assign;
    let mons_changed = *ui.mons.borrow() != mons;
    if items_changed {
        let items: Vec<Wp> = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.as_array().map(|a| a.iter().map(Wp::from_json).collect())).unwrap_or_default();
        *ui.items.borrow_mut() = items;
        *ui.list_raw.borrow_mut() = raw;
    }
    if colls_changed {
        let list: Vec<Coll> = colls.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().map(|c| Coll {
            id: c["id"].as_str().unwrap_or("").into(),
            name: c["name"].as_str().unwrap_or("").into(),
            builtin: c["builtin"].as_bool().unwrap_or(false),
            items: c["items"].as_array().into_iter().flatten().filter_map(|i| i.as_str().map(String::from)).collect(),
        }).collect();
        *ui.colls.borrow_mut() = list;
        *ui.colls_raw.borrow_mut() = colls;
    }
    *ui.assign.borrow_mut() = assign;
    *ui.mons.borrow_mut() = mons;
    ui.pl_ok.set(pl_ok);
    *ui.playlists.borrow_mut() = playlists;
    let fp = format!("{:?}{:?}{:?}{:?}", ui.assign.borrow().iter().collect::<std::collections::BTreeMap<_, _>>(), ui.mons.borrow().iter().map(|m| &m.name).collect::<Vec<_>>(), ui.playlists.borrow().iter().map(|(k, v)| format!("{k}{v}")).collect::<std::collections::BTreeSet<_>>(), ui.target.borrow());
    if force || items_changed || colls_changed || assign_changed || mons_changed {
        repopulate(ui);
        rebuild_nav(ui);
    }
    if force || *ui.scheme_fp.borrow() != fp {
        *ui.scheme_fp.borrow_mut() = fp;
        rebuild_scheme(ui);
    }
}

// ---------------------------------------------------------------- actions

fn apply_wallpaper(ui: &Rc<Ui>, mon: &str, id: &str) {
    let Some(w) = ui.wp(id) else { return };
    if !w.ok() {
        ui.toast(&format!("«{}» не поддерживается: {}", w.title, w.reason));
        return;
    }
    let prev = if mon == ALL { None } else { Some(ui.assign.borrow().get(mon).cloned().unwrap_or_default()) };
    if !ui.call("SetWallpaper", &(mon, id)) {
        return;
    }
    let toast = adw::Toast::builder().title(format!("«{}» — на {}", w.title, ui.target_name(mon))).timeout(4).build();
    if let Some(prev) = prev.filter(|p| p != id) {
        toast.set_button_label(Some("Отменить"));
        let (u, mon) = (ui.clone(), mon.to_string());
        toast.connect_button_clicked(move |_| {
            if u.call("SetWallpaper", &(mon.as_str(), prev.as_str())) {
                refresh(&u, false);
            }
        });
    }
    ui.toasts.add_toast(toast);
    refresh(ui, false);
}

fn set_fav(ui: &Rc<Ui>, id: &str, on: bool) {
    if ui.call("SetFavorite", &(id, on)) {
        let t = ui.wp(id).map(|w| w.title).unwrap_or_default();
        ui.toast(&if on { format!("«{t}» в избранном") } else { format!("«{t}» убраны из избранного") });
        refresh(ui, false);
    }
}

/// Name prompt for a new or renamed collection.
fn name_dialog(ui: &Rc<Ui>, heading: &str, initial: &str, ok_label: &str, done: impl Fn(String) + 'static) {
    let entry = gtk::Entry::new();
    entry.set_text(initial);
    entry.set_placeholder_text(Some("Например: Природа"));
    entry.set_activates_default(true);
    let d = adw::AlertDialog::new(Some(heading), None);
    d.add_response("cancel", "Отмена");
    d.add_response("ok", ok_label);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    d.set_extra_child(Some(&entry));
    let e2 = entry.clone();
    d.set_response_enabled("ok", !initial.trim().is_empty());
    let d2 = d.clone();
    entry.connect_changed(move |e| d2.set_response_enabled("ok", !e.text().trim().is_empty()));
    d.connect_response(None, move |_, r| {
        if r == "ok" {
            done(e2.text().trim().to_string());
        }
    });
    d.present(Some(&ui.win));
    entry.grab_focus();
}

fn new_collection(ui: &Rc<Ui>, add: Option<String>) {
    let u = ui.clone();
    name_dialog(ui, "Новая коллекция", "", "Создать", move |name| {
        let Ok(cid) = u.string("CreateCollection", &(name.as_str(),)) else { return u.toast("Не удалось создать коллекцию") };
        if let Some(id) = &add {
            u.call("AddToCollection", &(cid.as_str(), id.as_str()));
            u.toast(&format!("Коллекция «{name}» создана, обои добавлены"));
        } else {
            *u.view.borrow_mut() = format!("c:{cid}");
            u.toast(&format!("Коллекция «{name}» создана"));
        }
        refresh(&u, true);
    });
}

fn add_actions(ui: &Rc<Ui>) {
    let win = &ui.win;
    let add = |name: &str, f: Box<dyn Fn(&str)>| {
        let a = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        a.connect_activate(move |_, p| f(p.and_then(|v| v.str()).unwrap_or("")));
        win.add_action(&a);
    };
    let split2 = |s: &str| -> (String, String) {
        let (a, b) = s.split_once('\t').unwrap_or(("", s));
        (a.to_string(), b.to_string())
    };
    let u = ui.clone();
    add("apply", Box::new(move |p| { let (m, id) = split2(p); apply_wallpaper(&u, &m, &id) }));
    let u = ui.clone();
    add("details", Box::new(move |id| details_dialog(&u, id)));
    let u = ui.clone();
    add("fav", Box::new(move |p| { let (on, id) = split2(p); set_fav(&u, &id, on == "1") }));
    let u = ui.clone();
    add("coll-toggle", Box::new(move |p| {
        let (cid, id) = split2(p);
        let (inside, name) = u.colls.borrow().iter().find(|c| c.id == cid).map(|c| (c.items.contains(&id), c.name.clone())).unwrap_or_default();
        let ok = if inside { u.call("RemoveFromCollection", &(cid.as_str(), id.as_str())) } else { u.call("AddToCollection", &(cid.as_str(), id.as_str())) };
        if ok {
            u.toast(&if inside { format!("Убрано из «{name}»") } else { format!("Добавлено в «{name}»") });
            refresh(&u, false);
        }
    }));
    let u = ui.clone();
    add("coll-new", Box::new(move |id| new_collection(&u, (!id.is_empty()).then(|| id.to_string()))));
    let u = ui.clone();
    add("coll-rename", Box::new(move |cid| {
        let name = u.colls.borrow().iter().find(|c| c.id == cid).map(|c| c.name.clone()).unwrap_or_default();
        let (u2, cid) = (u.clone(), cid.to_string());
        name_dialog(&u, "Переименовать коллекцию", &name, "Переименовать", move |n| {
            if u2.call("RenameCollection", &(cid.as_str(), n.as_str())) {
                refresh(&u2, true);
            }
        });
    }));
    let u = ui.clone();
    add("coll-delete", Box::new(move |cid| {
        let name = u.colls.borrow().iter().find(|c| c.id == cid).map(|c| c.name.clone()).unwrap_or_default();
        let d = adw::AlertDialog::new(Some(&format!("Удалить коллекцию «{name}»?")), Some("Сами обои останутся в библиотеке."));
        d.add_response("cancel", "Отмена");
        d.add_response("del", "Удалить");
        d.set_response_appearance("del", adw::ResponseAppearance::Destructive);
        d.set_close_response("cancel");
        let (u2, cid) = (u.clone(), cid.to_string());
        d.connect_response(Some("del"), move |_, _| {
            if u2.call("DeleteCollection", &(cid.as_str(),)) {
                u2.toast(&format!("Коллекция «{name}» удалена"));
                refresh(&u2, true);
            }
        });
        d.present(Some(&u.win));
    }));
    let u = ui.clone();
    add("open-dir", Box::new(move |id| {
        if let Some(w) = u.wp(id) {
            gtk::FileLauncher::new(Some(&gio::File::for_path(&w.root))).launch(Some(&u.win), gio::Cancellable::NONE, |_| {});
        }
    }));
    let a = gio::SimpleAction::new("add-dir", None);
    let u = ui.clone();
    a.connect_activate(move |_, _| add_directory(&u, None));
    win.add_action(&a);
}

fn add_directory(ui: &Rc<Ui>, after: Option<Rc<dyn Fn()>>) {
    let fd = gtk::FileDialog::new();
    fd.set_title("Каталог с обоями");
    let u = ui.clone();
    fd.select_folder(Some(&ui.win), gio::Cancellable::NONE, move |res| {
        if let Some(p) = res.ok().and_then(|f| f.path()) {
            if u.call("AddDirectory", &(p.to_string_lossy().as_ref(),)) {
                if let Some(f) = &after { f() }
                refresh(&u, true);
                u.toast("Папка добавлена");
            }
        }
    });
}

// ---------------------------------------------------------------- playlist

fn playlist_dialog(ui: &Rc<Ui>) {
    let mon = ui.target.borrow().clone();
    let cur = ui.playlists.borrow().get(&mon).cloned().unwrap_or(json!({}));
    let dlg = adw::PreferencesDialog::new();
    dlg.set_title(&format!("Смена обоев — {}", ui.target_name(&mon)));
    dlg.set_search_enabled(false);
    let page = adw::PreferencesPage::new();
    page.set_icon_name(Some("media-playlist-shuffle-symbolic"));
    let g = adw::PreferencesGroup::new();
    g.set_description(Some("Обои будут сами меняться через заданный промежуток времени, выбираясь из выбранного набора."));
    let en = adw::SwitchRow::new();
    en.set_title("Менять обои по таймеру");
    en.set_active(cur["enabled"].as_bool().unwrap_or(false));
    g.add(&en);

    let srcs: Vec<(String, String)> = [("all".to_string(), "Все обои".to_string()), ("favorites".into(), "Избранное".into())]
        .into_iter()
        .chain(ui.colls.borrow().iter().filter(|c| !c.builtin).map(|c| (c.id.clone(), format!("Коллекция «{}»", c.name))))
        .collect();
    let src = adw::ComboRow::new();
    src.set_title("Откуда брать обои");
    let labels: Vec<&str> = srcs.iter().map(|s| s.1.as_str()).collect();
    src.set_model(Some(&gtk::StringList::new(&labels)));
    src.set_selected(srcs.iter().position(|s| Some(s.0.as_str()) == cur["source"].as_str()).unwrap_or(0) as u32);
    src.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));

    let iv = adw::ComboRow::new();
    iv.set_title("Как часто");
    let ilabels: Vec<&str> = INTERVALS.iter().map(|i| i.1).collect();
    iv.set_model(Some(&gtk::StringList::new(&ilabels)));
    let cur_iv = cur["interval_min"].as_i64().unwrap_or(30);
    iv.set_selected(INTERVALS.iter().position(|i| i.0 >= cur_iv).unwrap_or(3) as u32);
    iv.add_prefix(&gtk::Image::from_icon_name("alarm-symbolic"));

    let shuf = adw::SwitchRow::new();
    shuf.set_title("Перемешивать");
    shuf.set_subtitle("Случайный порядок вместо алфавитного");
    shuf.set_active(cur["order"].as_str() != Some("sequential"));
    shuf.add_prefix(&gtk::Image::from_icon_name("media-playlist-shuffle-symbolic"));

    let sync = adw::SwitchRow::new();
    sync.set_title("Синхронно на всех экранах");
    sync.set_subtitle("Менять обои на всех экранах одновременно");
    sync.set_active(cur["sync_monitors"].as_bool().unwrap_or(false));
    sync.add_prefix(&gtk::Image::from_icon_name("video-display-symbolic"));

    let g2 = adw::PreferencesGroup::new();
    g2.set_title("Настройки");
    for r in [src.upcast_ref::<gtk::Widget>(), iv.upcast_ref(), shuf.upcast_ref(), sync.upcast_ref()] {
        g2.add(r);
    }
    let next = adw::ButtonRow::new();
    next.set_title("Следующие обои сейчас");
    next.set_start_icon_name(Some("media-skip-forward-symbolic"));
    let g3 = adw::PreferencesGroup::new();
    g3.add(&next);

    let save: Rc<dyn Fn()> = {
        let (ui, mon, en, src, iv, shuf, sync, g2, g3) = (ui.clone(), mon.clone(), en.clone(), src.clone(), iv.clone(), shuf.clone(), sync.clone(), g2.clone(), g3.clone());
        Rc::new(move || {
            g2.set_sensitive(en.is_active());
            g3.set_sensitive(en.is_active());
            let v = json!({
                "enabled": en.is_active(),
                "source": srcs.get(src.selected() as usize).map(|s| s.0.clone()).unwrap_or("all".into()),
                "interval_min": INTERVALS.get(iv.selected() as usize).map(|i| i.0).unwrap_or(30),
                "order": if shuf.is_active() { "shuffle" } else { "sequential" },
                "sync_monitors": sync.is_active(),
            });
            if ui.call("SetPlaylist", &(mon.as_str(), v.to_string().as_str())) {
                refresh(&ui, false);
            }
        })
    };
    g2.set_sensitive(en.is_active());
    g3.set_sensitive(en.is_active());
    let s = save.clone();
    en.connect_active_notify(move |_| s());
    let s = save.clone();
    src.connect_selected_notify(move |_| s());
    let s = save.clone();
    iv.connect_selected_notify(move |_| s());
    let s = save.clone();
    shuf.connect_active_notify(move |_| s());
    let s = save.clone();
    sync.connect_active_notify(move |_| s());
    let (u, m) = (ui.clone(), mon.clone());
    next.connect_activated(move |_| next_wallpaper(&u, &m));
    page.add(&g);
    page.add(&g2);
    page.add(&g3);
    dlg.add(&page);
    dlg.present(Some(&ui.win));
}

fn next_wallpaper(ui: &Rc<Ui>, mon: &str) {
    if ui.call("NextWallpaper", &(mon,)) {
        refresh(ui, false);
        let t = ui.on_monitor(if mon == ALL { ui.mons.borrow().first().map(|m| m.name.clone()).unwrap_or_default() } else { mon.to_string() }.as_str()).and_then(|id| ui.wp(&id)).map(|w| w.title);
        ui.toast(&t.map(|t| format!("Следующие: «{t}»")).unwrap_or("Следующие обои".into()));
    }
}

// ---------------------------------------------------------------- details dialog

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' | '[' => in_tag = true,
            '>' | ']' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out.trim().to_string();
    // Localisation keys like ui_browse_properties_scheme_color.
    if let Some(k) = out.strip_prefix("ui_browse_properties_") {
        return match k {
            "scheme_color" => "Основной цвет".into(),
            "rate" => "Скорость".into(),
            "volume" => "Громкость".into(),
            _ => { let t = k.replace('_', " "); let mut c = t.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }
        };
    }
    out
}

/// Icon for a property row, guessed from its label and type.
fn prop_icon(text: &str, ty: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| t.contains(w));
    Some(if ty == "color" || has(&["цвет", "color", "colour"]) {
        "color-select-symbolic"
    } else if has(&["volume", "громк", "sound", "audio", "звук", "music", "музык"]) {
        "audio-volume-high-symbolic"
    } else if has(&["speed", "rate", "скорост", "fps"]) {
        "power-profile-performance-symbolic"
    } else if has(&["bright", "ярк", "light", "свет"]) {
        "display-brightness-symbolic"
    } else if has(&["clock", "time", "date", "врем", "час", "дата"]) {
        "preferences-system-time-symbolic"
    } else if has(&["text", "текст", "font", "шрифт"]) {
        "insert-text-symbolic"
    } else if has(&["image", "picture", "background", "фон", "картин"]) {
        "image-x-generic-symbolic"
    } else if has(&["particle", "частиц", "snow", "rain", "снег", "дожд"]) {
        "weather-snow-symbolic"
    } else if ty == "bool" {
        "object-select-symbolic"
    } else {
        return None;
    })
}

/// Send a property value; sliders/text are debounced so dragging doesn't flood the daemon.
fn send_prop(ui: &Rc<Ui>, id: &str, key: &str, value: Value, delay_ms: u64) {
    let tag = format!("{id}/{key}");
    if let Some(old) = ui.debounce.borrow_mut().remove(&tag) {
        old.remove();
    }
    let (ui2, id, key, tag2) = (ui.clone(), id.to_string(), key.to_string(), tag.clone());
    let send = move || {
        ui2.debounce.borrow_mut().remove(&tag2);
        ui2.call("SetProperty", &(id.as_str(), key.as_str(), value.to_string().as_str()));
    };
    if delay_ms == 0 {
        send();
    } else {
        let src = glib::timeout_add_local_once(Duration::from_millis(delay_ms), send);
        ui.debounce.borrow_mut().insert(tag, src);
    }
}

fn build_props(ui: &Rc<Ui>, id: &str, into: &gtk::Box) {
    clear_box(into);
    ui.building.set(true);
    let list = ui.string("GetProperties", &(id,)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut groups: Vec<adw::PreferencesGroup> = Vec::new();
    let mut cur = adw::PreferencesGroup::new();
    cur.set_title("Настройки обоев");
    cur.set_description(Some("Изменения применяются сразу"));
    let mut cur_rows = 0;
    for p in &list {
        let key = p["key"].as_str().unwrap_or("").to_string();
        let text = strip_tags(p["text"].as_str().unwrap_or(&key));
        let text = if text.is_empty() { key.clone() } else { text };
        let kind = &p["kind"];
        let ty = kind["type"].as_str().unwrap_or("");
        let title = glib::markup_escape_text(&text);
        let id = id.to_string();
        let row: gtk::Widget = match ty {
            "group" => {
                if cur_rows > 0 {
                    groups.push(cur);
                    cur = adw::PreferencesGroup::new();
                    cur_rows = 0;
                }
                cur.set_title(&title);
                cur.set_description(None);
                continue;
            }
            "text" => {
                let row = adw::ActionRow::new();
                row.set_title(&title);
                row.set_title_lines(0);
                row.add_css_class("dim-label");
                row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
                row.upcast()
            }
            "bool" => {
                let row = adw::SwitchRow::new();
                row.set_title(&title);
                row.set_active(kind["value"].as_bool().unwrap_or(false));
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_active_notify(move |r| if !ui.building.get() { send_prop(&ui, &id, &key, json!(r.is_active()), 0) });
                row.upcast()
            }
            "slider" => {
                let (min, max) = (kind["min"].as_f64().unwrap_or(0.0), kind["max"].as_f64().unwrap_or(100.0));
                let step = kind["step"].as_f64().unwrap_or(1.0);
                let frac = kind["fraction"].as_bool().unwrap_or(false);
                let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max.max(min + step), step);
                scale.set_value(kind["value"].as_f64().unwrap_or(min));
                scale.set_draw_value(true);
                scale.set_value_pos(gtk::PositionType::Right);
                scale.set_digits(if frac { 2 } else { 0 });
                scale.set_hexpand(true);
                scale.set_size_request(200, -1);
                scale.set_valign(gtk::Align::Center);
                let row = adw::ActionRow::new();
                row.set_title(&title);
                row.add_suffix(&scale);
                let (ui, key) = (ui.clone(), key.clone());
                scale.connect_value_changed(move |s| if !ui.building.get() { send_prop(&ui, &id, &key, json!(s.value()), 120) });
                row.upcast()
            }
            "combo" => {
                let opts: Vec<(String, String)> = kind["options"].as_array().into_iter().flatten().map(|o| (o["label"].as_str().unwrap_or("").to_string(), o["value"].as_str().map(String::from).unwrap_or_else(|| o["value"].to_string()))).collect();
                let labels: Vec<String> = opts.iter().map(|o| strip_tags(&o.0)).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let row = adw::ComboRow::new();
                row.set_title(&title);
                row.set_model(Some(&gtk::StringList::new(&refs)));
                let curv = kind["value"].as_str().map(String::from).unwrap_or_else(|| kind["value"].to_string());
                row.set_selected(opts.iter().position(|o| o.1 == curv).unwrap_or(0) as u32);
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_selected_notify(move |r| {
                    if ui.building.get() { return }
                    if let Some(o) = opts.get(r.selected() as usize) { send_prop(&ui, &id, &key, json!(o.1), 0) }
                });
                row.upcast()
            }
            "color" => {
                let c = &kind["value"];
                let rgb: Vec<f32> = (0..3).map(|i| c[i].as_f64().unwrap_or(0.0) as f32).collect();
                let btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
                btn.set_rgba(&gdk::RGBA::new(rgb[0], rgb[1], rgb[2], 1.0));
                btn.set_valign(gtk::Align::Center);
                let row = adw::ActionRow::new();
                row.set_title(&title);
                row.add_suffix(&btn);
                row.set_activatable_widget(Some(&btn));
                let (ui, key) = (ui.clone(), key.clone());
                btn.connect_rgba_notify(move |b| {
                    if ui.building.get() { return }
                    let c = b.rgba();
                    send_prop(&ui, &id, &key, json!(format!("{:.4} {:.4} {:.4}", c.red(), c.green(), c.blue())), 150);
                });
                row.upcast()
            }
            "textinput" => {
                let row = adw::EntryRow::new();
                row.set_title(&title);
                row.set_text(kind["value"].as_str().unwrap_or(""));
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_changed(move |r| if !ui.building.get() { send_prop(&ui, &id, &key, json!(r.text().to_string()), 400) });
                row.upcast()
            }
            _ => continue, // file / directory / scenetexture / unknown: not editable here
        };
        if ty != "text" && ty != "textinput" {
            if let (Some(icon), Some(r)) = (prop_icon(&text, ty), row.downcast_ref::<adw::ActionRow>()) {
                r.add_prefix(&gtk::Image::from_icon_name(icon));
            }
        }
        cur.add(&row);
        cur_rows += 1;
    }
    if cur_rows > 0 {
        groups.push(cur);
    }
    if let Some(first) = groups.first() {
        let reset = gtk::Button::with_label("Сбросить");
        reset.add_css_class("flat");
        reset.set_valign(gtk::Align::Center);
        reset.set_tooltip_text(Some("Вернуть значения по умолчанию"));
        let (ui2, id2, into2) = (ui.clone(), id.to_string(), into.clone());
        reset.connect_clicked(move |_| {
            if ui2.call("ResetProperties", &(id2.as_str(),)) {
                build_props(&ui2, &id2, &into2);
                ui2.toast("Настройки сброшены");
            }
        });
        first.set_header_suffix(Some(&reset));
    } else {
        let st = adw::StatusPage::new();
        st.set_icon_name(Some("emblem-system-symbolic"));
        st.set_title("Без настроек");
        st.set_description(Some("У этих обоев нет настраиваемых свойств."));
        st.add_css_class("compact");
        into.append(&st);
    }
    for g in &groups {
        into.append(g);
    }
    ui.building.set(false);
}

fn details_dialog(ui: &Rc<Ui>, id: &str) {
    let Some(w) = ui.wp(id) else { return };
    let proj = w.project();
    let dlg = adw::Dialog::new();
    dlg.set_title(&w.title);
    dlg.set_content_width(680);
    dlg.set_content_height(860);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_top(6);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);

    // Big preview: animated when possible.
    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_size_request(-1, 340);
    pic.set_can_shrink(true);
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("big-preview");
    frame.set_overflow(gtk::Overflow::Hidden);
    frame.append(&pic);
    if let Some(p) = &w.preview {
        pic.set_paintable(ui.thumb(p).as_ref());
    }
    let anim: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let media: Rc<RefCell<Option<gtk::MediaFile>>> = Rc::new(RefCell::new(None));
    if let Some(g) = w.animated_preview() {
        *anim.borrow_mut() = play_gif(&pic, &g);
    } else if let Some(v) = w.video_file() {
        let m = gtk::MediaFile::for_filename(&v);
        m.set_muted(true);
        m.set_loop(true);
        m.play();
        pic.set_paintable(Some(&m));
        *media.borrow_mut() = Some(m);
    }
    dlg.connect_closed(move |_| {
        if let Some(a) = anim.borrow_mut().take() { a.remove() }
        if let Some(m) = media.borrow_mut().take() { m.pause() }
    });
    body.append(&frame);

    // Title and facts.
    let title = gtk::Label::new(Some(&w.title));
    title.add_css_class("title-1");
    title.set_wrap(true);
    title.set_xalign(0.0);
    let mut facts = vec![kind_label(&w.kind).to_string()];
    if !w.rating.is_empty() {
        facts.push(if w.rating == "Everyone" { "Для всех".into() } else { w.rating.clone() });
    }
    if let Some(a) = proj["author"].as_str().or(proj["workshopauthor"].as_str()).filter(|a| !a.is_empty()) {
        facts.push(format!("Автор: {a}"));
    }
    let workshop = proj["workshopid"].as_str().map(String::from).or_else(|| proj["workshopid"].as_u64().map(|n| n.to_string())).filter(|s| !s.is_empty() && s != "0");
    if workshop.is_some() {
        facts.push("Steam Workshop".into());
    }
    let sub = gtk::Label::new(Some(&facts.join(" · ")));
    sub.add_css_class("dim-label");
    sub.set_xalign(0.0);
    sub.set_wrap(true);
    let head = gtk::Box::new(gtk::Orientation::Vertical, 4);
    head.append(&title);
    head.append(&sub);
    body.append(&head);

    if !w.tags.is_empty() {
        let tags = gtk::FlowBox::new();
        tags.set_selection_mode(gtk::SelectionMode::None);
        tags.set_column_spacing(6);
        tags.set_row_spacing(6);
        tags.set_max_children_per_line(10);
        for t in &w.tags {
            let l = gtk::Label::new(Some(t));
            l.add_css_class("wp-tag");
            tags.append(&l);
        }
        body.append(&tags);
    }

    // Actions.
    let target = ui.target.borrow().clone();
    let btns = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    if w.ok() {
        let here = gtk::Button::with_label(&if target == ALL { "Поставить на все экраны".to_string() } else { format!("Поставить на {target}") });
        here.add_css_class("pill");
        here.add_css_class("suggested-action");
        let (u, t, i) = (ui.clone(), target.clone(), w.id.clone());
        here.connect_clicked(move |_| apply_wallpaper(&u, &t, &i));
        btns.append(&here);
        if target != ALL && ui.mons.borrow().len() > 1 {
            let all = gtk::Button::with_label("На все экраны");
            all.add_css_class("pill");
            let (u, i) = (ui.clone(), w.id.clone());
            all.connect_clicked(move |_| apply_wallpaper(&u, ALL, &i));
            btns.append(&all);
        }
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    btns.append(&spacer);
    if ui.colls_ok() {
        let star = gtk::ToggleButton::new();
        star.set_icon_name(if ui.is_fav(&w.id) { "starred-symbolic" } else { "non-starred-symbolic" });
        star.set_active(ui.is_fav(&w.id));
        star.add_css_class("circular");
        star.set_valign(gtk::Align::Center);
        star.set_tooltip_text(Some("Избранное"));
        let (u, i) = (ui.clone(), w.id.clone());
        star.connect_toggled(move |b| {
            b.set_icon_name(if b.is_active() { "starred-symbolic" } else { "non-starred-symbolic" });
            set_fav(&u, &i, b.is_active());
        });
        btns.append(&star);
    }
    let more = gtk::MenuButton::new();
    more.set_icon_name("view-more-symbolic");
    more.add_css_class("circular");
    more.set_valign(gtk::Align::Center);
    more.set_tooltip_text(Some("Ещё"));
    let (u, i) = (ui.clone(), w.id.clone());
    more.set_create_popup_func(move |b| b.set_menu_model(Some(&card_menu(&u, &i))));
    btns.append(&more);
    body.append(&btns);

    let active = ui.active_on(&w.id);
    if !active.is_empty() {
        let l = gtk::Label::new(Some(&format!("✓ Сейчас на экранах: {}", active.join(", "))));
        l.add_css_class("accent");
        l.add_css_class("heading");
        l.set_xalign(0.0);
        body.append(&l);
    }
    if !w.ok() {
        let l = gtk::Label::new(Some(&format!("Эти обои пока не поддерживаются: {}", w.reason)));
        l.add_css_class("error");
        l.set_wrap(true);
        l.set_xalign(0.0);
        body.append(&l);
    }
    if let Some(desc) = proj["description"].as_str().map(strip_tags).filter(|d| !d.is_empty()) {
        let l = gtk::Label::new(Some(&desc));
        l.set_wrap(true);
        l.set_xalign(0.0);
        l.set_selectable(true);
        l.set_lines(6);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.add_css_class("body");
        body.append(&l);
    }
    if let Some(ws) = workshop {
        let b = gtk::LinkButton::with_label(&format!("https://steamcommunity.com/sharedfiles/filedetails/?id={ws}"), "Открыть в Steam Workshop");
        b.set_halign(gtk::Align::Start);
        body.append(&b);
    }

    if w.ok() {
        let props = gtk::Box::new(gtk::Orientation::Vertical, 18);
        build_props(ui, &w.id, &props);
        body.append(&props);
    }

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(760);
    clamp.set_child(Some(&body));
    let scroll = gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let tv = adw::ToolbarView::new();
    let hb = adw::HeaderBar::new();
    hb.set_show_title(false);
    tv.add_top_bar(&hb);
    tv.set_content(Some(&scroll));
    dlg.set_child(Some(&tv));
    dlg.present(Some(&ui.win));
}

// ---------------------------------------------------------------- settings

fn settings_dialog(ui: &Rc<Ui>) {
    let st = ui.settings();
    let dlg = adw::PreferencesDialog::new();
    dlg.set_title("Настройки");
    let page = adw::PreferencesPage::new();

    let g = adw::PreferencesGroup::new();
    g.set_title("Воспроизведение");
    let fps = adw::SpinRow::with_range(1.0, 240.0, 1.0);
    fps.set_title("Лимит кадров в секунду");
    fps.set_subtitle("Для сцен; меньше — тише и холоднее видеокарта");
    fps.set_value(st["fps"].as_f64().unwrap_or(30.0));
    fps.add_prefix(&gtk::Image::from_icon_name("power-profile-performance-symbolic"));
    let u = ui.clone();
    fps.connect_changed(move |r| { u.call("SetFps", &(r.value() as u32,)); });
    g.add(&fps);

    let vol = st["volume"].as_f64().unwrap_or(0.5);
    let muted = st["muted"].as_bool().unwrap_or(true);
    let snd = adw::SwitchRow::new();
    snd.set_title("Звук обоев");
    snd.set_subtitle("По умолчанию выключен");
    snd.set_active(!muted);
    snd.add_prefix(&gtk::Image::from_icon_name("audio-volume-high-symbolic"));
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
    scale.set_value(vol);
    scale.set_size_request(180, -1);
    scale.set_valign(gtk::Align::Center);
    scale.set_sensitive(!muted);
    let vrow = adw::ActionRow::new();
    vrow.set_title("Громкость");
    vrow.add_suffix(&scale);
    let (s2, sc2) = (ui.clone(), scale.clone());
    let push = Rc::new(move |s: &adw::SwitchRow, sc: &gtk::Scale| {
        sc.set_sensitive(s.is_active());
        s2.call("SetVolume", &(sc.value(), !s.is_active()));
    });
    let p1 = push.clone();
    let sc3 = sc2.clone();
    snd.connect_active_notify(move |s| p1(s, &sc3));
    let snd2 = snd.clone();
    scale.connect_value_changed(move |sc| push(&snd2, sc));
    g.add(&snd);
    g.add(&vrow);
    page.add(&g);

    let gp = adw::PreferencesGroup::new();
    gp.set_title("Экономия ресурсов");
    let pause = adw::SwitchRow::new();
    pause.set_title("Пауза под развёрнутым окном");
    pause.set_subtitle("Останавливать обои экрана, закрытого развёрнутым окном");
    pause.set_active(st["pause_on_maximized"].as_bool().unwrap_or(true));
    pause.add_prefix(&gtk::Image::from_icon_name("media-playback-pause-symbolic"));
    let u = ui.clone();
    pause.connect_active_notify(move |r| { u.call("SetPauseOnMaximized", &(r.is_active(),)); });
    gp.add(&pause);

    let pause_fs = adw::SwitchRow::new();
    pause_fs.set_title("Пауза в играх и полноэкранных приложениях");
    pause_fs.set_subtitle("Останавливать обои на всех экранах, пока открыто приложение на весь экран");
    pause_fs.set_active(st["pause_on_fullscreen_any"].as_bool().unwrap_or(true));
    pause_fs.add_prefix(&gtk::Image::from_icon_name("input-gaming-symbolic"));
    let u = ui.clone();
    pause_fs.connect_active_notify(move |r| { u.call("SetPauseOnFullscreenAny", &(r.is_active(),)); });
    gp.add(&pause_fs);
    page.add(&gp);

    let dirs = adw::PreferencesGroup::new();
    dirs.set_title("Библиотеки обоев");
    dirs.set_description(Some("Steam Workshop и локальный каталог ~/.local/share/wpe/wallpapers находятся автоматически."));
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
    let fill: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    {
        let (ui, dirs, rows, fill2) = (ui.clone(), dirs.clone(), rows.clone(), fill.clone());
        *fill.borrow_mut() = Some(Rc::new(move || {
            for r in rows.borrow_mut().drain(..) {
                dirs.remove(&r);
            }
            let st = ui.settings();
            for d in st["directories"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(d));
                row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
                let rm = gtk::Button::from_icon_name("user-trash-symbolic");
                rm.add_css_class("flat");
                rm.set_valign(gtk::Align::Center);
                rm.set_tooltip_text(Some("Убрать каталог"));
                let (ui2, path, f) = (ui.clone(), d.to_string(), fill2.clone());
                rm.connect_clicked(move |_| {
                    if ui2.call("RemoveDirectory", &(path.as_str(),)) {
                        let f = f.borrow().clone();
                        if let Some(f) = f { f() }
                        refresh(&ui2, true);
                    }
                });
                row.add_suffix(&rm);
                dirs.add(&row);
                rows.borrow_mut().push(row);
            }
        }));
    }
    let f0 = fill.borrow().clone();
    if let Some(f) = f0 { f() }
    let add = adw::ButtonRow::new();
    add.set_title("Добавить папку…");
    add.set_start_icon_name(Some("folder-new-symbolic"));
    let (u, f) = (ui.clone(), fill.clone());
    add.connect_activated(move |_| add_directory(&u, f.borrow().clone()));
    let rescan = adw::ButtonRow::new();
    rescan.set_title("Пересканировать");
    rescan.set_start_icon_name(Some("view-refresh-symbolic"));
    let u = ui.clone();
    rescan.connect_activated(move |_| {
        if u.call("Rescan", &()) {
            refresh(&u, true);
            u.toast("Каталог обновлён");
        }
    });
    let ws = adw::ButtonRow::new();
    ws.set_title("Найти обои в Steam Workshop");
    ws.set_start_icon_name(Some("web-browser-symbolic"));
    let u = ui.clone();
    ws.connect_activated(move |_| { gtk::UriLauncher::new(WORKSHOP_URL).launch(Some(&u.win), gio::Cancellable::NONE, |_| {}); });
    let btns = adw::PreferencesGroup::new();
    btns.add(&add);
    btns.add(&rescan);
    btns.add(&ws);
    page.add(&dirs);
    page.add(&btns);
    dlg.add(&page);
    dlg.present(Some(&ui.win));
}

// ---------------------------------------------------------------- window

fn build(app: &adw::Application) {
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
    let css = gtk::CssProvider::new();
    css.load_from_string(CSS);
    if let Some(d) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&d, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    let toasts = adw::ToastOverlay::new();
    let win = adw::ApplicationWindow::builder().application(app).title("Живые обои").default_width(1360).default_height(900).width_request(360).height_request(400).content(&toasts).build();

    // ---- sidebar
    let nav = gtk::ListBox::new();
    nav.add_css_class("navigation-sidebar");
    nav.set_selection_mode(gtk::SelectionMode::Single);
    let nav_scroll = gtk::ScrolledWindow::builder().child(&nav).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    let menu = gio::Menu::new();
    let s1 = gio::Menu::new();
    s1.append(Some("Добавить папку с обоями…"), Some("win.add-dir"));
    s1.append(Some("Пересканировать"), Some("app.rescan"));
    menu.append_section(None, &s1);
    let s2 = gio::Menu::new();
    s2.append(Some("Настройки"), Some("app.settings"));
    s2.append(Some("О приложении"), Some("app.about"));
    menu.append_section(None, &s2);
    let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).tooltip_text("Главное меню").build();
    let side_head = adw::HeaderBar::new();
    side_head.pack_end(&menu_btn);
    let side_tv = adw::ToolbarView::new();
    side_tv.add_top_bar(&side_head);
    side_tv.set_content(Some(&nav_scroll));
    let side_page = adw::NavigationPage::new(&side_tv, "Живые обои");

    // ---- monitor scheme
    let scheme_fixed = gtk::Fixed::new();
    scheme_fixed.set_halign(gtk::Align::Center);
    scheme_fixed.set_valign(gtk::Align::Center);
    let target_title = gtk::Label::new(None);
    target_title.add_css_class("title-3");
    target_title.set_xalign(0.0);
    target_title.set_wrap(true);
    let target_sub = gtk::Label::new(None);
    target_sub.add_css_class("dim-label");
    target_sub.set_xalign(0.0);
    target_sub.set_wrap(true);
    target_sub.set_max_width_chars(36);
    let all_btn = gtk::ToggleButton::new();
    all_btn.set_child(Some(&adw::ButtonContent::builder().icon_name("view-dual-symbolic").label("Все экраны").build()));
    all_btn.add_css_class("pill");
    let pl_btn = gtk::Button::new();
    pl_btn.add_css_class("pill");
    pl_btn.set_tooltip_text(Some("Менять обои выбранного экрана автоматически"));
    let next_btn = gtk::Button::from_icon_name("media-skip-forward-symbolic");
    next_btn.add_css_class("circular");
    next_btn.set_valign(gtk::Align::Center);
    next_btn.set_tooltip_text(Some("Следующие обои из плейлиста"));
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.append(&all_btn);
    btn_row.append(&pl_btn);
    btn_row.append(&next_btn);
    let info = gtk::Box::new(gtk::Orientation::Vertical, 8);
    info.set_valign(gtk::Align::Center);
    info.set_hexpand(true);
    let cap = gtk::Label::new(Some("КУДА СТАВИТЬ ОБОИ"));
    cap.add_css_class("caption-heading");
    cap.add_css_class("accent");
    cap.set_xalign(0.0);
    info.append(&cap);
    info.append(&target_title);
    info.append(&target_sub);
    info.append(&btn_row);
    let scheme = gtk::Box::new(gtk::Orientation::Horizontal, 28);
    scheme.add_css_class("scheme");
    scheme.set_margin_start(20);
    scheme.set_margin_end(20);
    scheme.set_margin_top(12);
    let fixed_wrap = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    fixed_wrap.set_hexpand(true);
    fixed_wrap.append(&scheme_fixed);
    scheme.append(&fixed_wrap);
    scheme.append(&info);

    // ---- grid
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(12);
    flow.set_activate_on_single_click(true);
    flow.set_row_spacing(8);
    flow.set_column_spacing(8);
    flow.set_valign(gtk::Align::Start);
    flow.set_margin_top(10);
    flow.set_margin_bottom(24);
    flow.set_margin_start(14);
    flow.set_margin_end(14);
    let empty = adw::StatusPage::new();
    empty.set_vexpand(true);
    let grid_stack = gtk::Stack::new();
    grid_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    grid_stack.add_named(&flow, Some("grid"));
    grid_stack.add_named(&empty, Some("empty"));
    grid_stack.set_vhomogeneous(false);

    let sort_dd = gtk::DropDown::from_strings(&["По названию", "Сначала текущие", "По типу", "Сначала избранные"]);
    sort_dd.set_tooltip_text(Some("Сортировка"));
    let zoom = gtk::Scale::with_range(gtk::Orientation::Horizontal, CARD_MIN, CARD_MAX, 10.0);
    zoom.set_value(300.0);
    zoom.set_size_request(140, -1);
    zoom.set_draw_value(false);
    zoom.set_tooltip_text(Some("Размер карточек"));
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    tools.set_margin_start(24);
    tools.set_margin_end(24);
    tools.set_margin_top(16);
    let hint = gtk::Label::new(Some("Щелчок — поставить, правый щелчок — меню"));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_hexpand(true);
    hint.set_xalign(0.0);
    tools.append(&hint);
    tools.append(&gtk::Image::from_icon_name("zoom-out-symbolic"));
    tools.append(&zoom);
    tools.append(&gtk::Image::from_icon_name("zoom-in-symbolic"));
    tools.append(&sort_dd);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&scheme);
    content.append(&tools);
    content.append(&grid_stack);
    let scroller = gtk::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();

    let search = gtk::SearchEntry::builder().placeholder_text("Название или тег").build();
    let search_bar = gtk::SearchBar::new();
    let sclamp = adw::Clamp::new();
    sclamp.set_maximum_size(480);
    sclamp.set_child(Some(&search));
    search_bar.set_child(Some(&sclamp));
    search_bar.connect_entry(&search);
    search_bar.set_key_capture_widget(Some(&win));
    let search_btn = gtk::ToggleButton::new();
    search_btn.set_icon_name("system-search-symbolic");
    search_btn.set_tooltip_text(Some("Поиск (Ctrl+F)"));
    search_btn.bind_property("active", &search_bar, "search-mode-enabled").bidirectional().sync_create().build();

    let content_title = adw::WindowTitle::new("Все обои", "");
    let head = adw::HeaderBar::new();
    head.set_title_widget(Some(&content_title));
    head.pack_start(&search_btn);
    let ctv = adw::ToolbarView::new();
    ctv.add_top_bar(&head);
    ctv.add_top_bar(&search_bar);
    ctv.set_content(Some(&scroller));
    let content_page = adw::NavigationPage::new(&ctv, "Все обои");

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&side_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(220.0);
    split.set_max_sidebar_width(280.0);
    split.set_show_content(true);

    // Daemon-down page.
    let down = adw::StatusPage::new();
    down.set_icon_name(Some("dialog-warning-symbolic"));
    down.set_title("Демон обоев не запущен");
    down.set_description(Some("Для работы галереи нужен фоновый процесс wpe-daemon."));
    let start = gtk::Button::with_label("Запустить демон");
    start.add_css_class("suggested-action");
    start.add_css_class("pill");
    start.set_halign(gtk::Align::Center);
    down.set_child(Some(&start));

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.add_named(&split, Some("gallery"));
    stack.add_named(&down, Some("down"));
    stack.set_visible_child_name("down");
    toasts.set_child(Some(&stack));

    // Only the last matching breakpoint applies: the narrow one repeats the scheme setter.
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1180sp").unwrap());
    bp.add_setter(&scheme, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
    win.add_breakpoint(bp);
    let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 820sp").unwrap());
    bp.add_setter(&split, "collapsed", Some(&true.to_value()));
    bp.add_setter(&scheme, "orientation", Some(&gtk::Orientation::Vertical.to_value()));
    win.add_breakpoint(bp);

    let ui = Rc::new(Ui {
        win: win.clone(), toasts, stack, grid_stack, flow: flow.clone(), empty, nav: nav.clone(), content_title, content_page, split: split.clone(),
        scheme, scheme_fixed, target_title, target_sub, all_btn: all_btn.clone(), pl_btn: pl_btn.clone(), next_btn: next_btn.clone(),
        search: search.clone(), sort_dd: sort_dd.clone(),
        daemon: RefCell::new(None), items: RefCell::new(vec![]), list_raw: RefCell::new(String::new()), assign: RefCell::new(HashMap::new()),
        colls: RefCell::new(vec![]), colls_raw: RefCell::new(None), playlists: RefCell::new(HashMap::new()), pl_ok: Cell::new(false),
        mons: RefCell::new(vec![]), scheme_fp: RefCell::new(String::new()), view: RefCell::new("all".into()), target: RefCell::new(ALL.into()),
        card_w: Cell::new(300), thumbs: RefCell::new(HashMap::new()), pending: RefCell::new(VecDeque::new()), loading: Cell::new(false),
        debounce: RefCell::new(HashMap::new()), building: Cell::new(false), nav_building: Cell::new(false),
    });
    add_actions(&ui);

    let u = ui.clone();
    flow.set_filter_func(move |c| u.wp(&c.widget_name()).map(|w| u.wp_visible(&w)).unwrap_or(false));
    let u = ui.clone();
    flow.set_sort_func(move |a, b| {
        let (Some(x), Some(y)) = (u.wp(&a.widget_name()), u.wp(&b.widget_name())) else { return gtk::Ordering::Equal };
        let by_title = || x.title.to_lowercase().cmp(&y.title.to_lowercase());
        let ord = match u.sort_dd.selected() {
            1 => u.active_on(&y.id).len().cmp(&u.active_on(&x.id).len()).then_with(by_title),
            2 => x.kind.cmp(&y.kind).then_with(by_title),
            3 => u.is_fav(&y.id).cmp(&u.is_fav(&x.id)).then_with(by_title),
            _ => by_title(),
        };
        ord.into()
    });
    let u = ui.clone();
    search.connect_search_changed(move |_| refilter(&u));
    let u = ui.clone();
    sort_dd.connect_selected_notify(move |_| refilter(&u));
    let u = ui.clone();
    let zoom_src: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    zoom.connect_value_changed(move |z| {
        if let Some(s) = zoom_src.borrow_mut().take() {
            s.remove();
        }
        let (u2, v, zs) = (u.clone(), z.value() as i32, zoom_src.clone());
        *zoom_src.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(150), move || {
            zs.borrow_mut().take();
            u2.card_w.set(v);
            repopulate(&u2);
        }));
    });
    let u = ui.clone();
    flow.connect_child_activated(move |_, c| {
        let t = u.target.borrow().clone();
        let id = c.widget_name().to_string();
        if u.wp(&id).map(|w| w.ok()).unwrap_or(false) {
            apply_wallpaper(&u, &t, &id);
        } else {
            details_dialog(&u, &id);
        }
    });
    let u = ui.clone();
    nav.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        if u.nav_building.get() {
            return;
        }
        let key = row.widget_name().to_string();
        if key == "new-coll" {
            rebuild_nav(&u); // restores the previous selection
            new_collection(&u, None);
            return;
        }
        *u.view.borrow_mut() = key;
        refilter(&u);
        u.split.set_show_content(true);
    });
    let u = ui.clone();
    all_btn.connect_toggled(move |b| {
        if u.building.get() {
            return;
        }
        if b.is_active() {
            set_target(&u, ALL);
        } else {
            // "All" can't be unselected by itself: pick the first screen instead.
            let first = u.mons.borrow().first().map(|m| m.name.clone());
            if let Some(f) = first { set_target(&u, &f) } else { b.set_active(true) }
        }
    });
    let u = ui.clone();
    pl_btn.connect_clicked(move |_| playlist_dialog(&u));
    let u = ui.clone();
    next_btn.connect_clicked(move |_| { let t = u.target.borrow().clone(); next_wallpaper(&u, &t) });

    start.connect_clicked({
        let u = ui.clone();
        move |_| {
            match std::process::Command::new("systemctl").args(["--user", "start", "wpe-daemon"]).status() {
                Ok(s) if s.success() => u.toast("Запускаем демон…"),
                _ => u.toast("Не удалось запустить: systemctl --user start wpe-daemon"),
            }
            let u2 = u.clone();
            glib::timeout_add_local_once(Duration::from_millis(1500), move || refresh(&u2, true));
        }
    });

    let act = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        app.add_action(&a);
    };
    let u = ui.clone();
    act("settings", Box::new(move || settings_dialog(&u)));
    let u = ui.clone();
    act("rescan", Box::new(move || {
        if u.call("Rescan", &()) {
            refresh(&u, true);
            u.toast("Каталог обновлён");
        }
    }));
    let sb = search_btn.clone();
    act("search", Box::new(move || sb.set_active(!sb.is_active())));
    let w = win.clone();
    act("about", Box::new(move || {
        let a = adw::AboutDialog::new();
        a.set_application_name("Живые обои");
        a.set_application_icon(APP_ID);
        a.set_version(env!("CARGO_PKG_VERSION"));
        a.set_comments("Живые обои для GNOME: видео, web и сцены Wallpaper Engine");
        a.set_license_type(gtk::License::Gpl30);
        a.present(Some(&w));
    }));
    app.set_accels_for_action("app.settings", &["<Control>comma"]);
    app.set_accels_for_action("app.search", &["<Control>f"]);

    // Monitor hot-plug: re-read the layout.
    if let Some(d) = gdk::Display::default() {
        let u = ui.clone();
        d.monitors().connect_items_changed(move |_, _, _, _| {
            let u = u.clone();
            glib::timeout_add_local_once(Duration::from_millis(300), move || refresh(&u, false));
        });
    }
    let u = ui.clone();
    glib::timeout_add_local(Duration::from_secs(2), move || {
        refresh(&u, false);
        glib::ControlFlow::Continue
    });
    refresh(&ui, true);
    win.present();
}

pub fn run() -> i32 {
    glib::set_application_name("Живые обои");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build);
    app.run_with_args::<&str>(&[]).get() as i32
}
