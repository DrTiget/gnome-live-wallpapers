//! libadwaita gallery: grid of previews, monitor selection, property editor
//! generated from the daemon's `GetProperties` JSON, global settings.
//! Talks to the daemon over D-Bus (blocking calls: they are local and short)
//! and polls it every 2 s, so changes made via CLI or another window show up.

use adw::prelude::*;
use libadwaita as adw;
use gtk4 as gtk;
use gtk::{gdk, gio, glib};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use zbus::blocking::{Connection, Proxy};

const APP_ID: &str = "io.github.wpe.Gallery";
const ALL: &str = "*";
const CARD_W: i32 = 256;
const CARD_H: i32 = 144;

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
    fn json(&self, method: &str) -> Option<Value> {
        let s: String = self.proxy().ok()?.call(method, &()).ok()?;
        serde_json::from_str(&s).ok()
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
            preview: v["preview"].as_str().map(String::from),
            rating: s("rating"),
        }
    }
    fn ok(&self) -> bool {
        self.status == "ok"
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "scene" => "Scene",
        "web" => "Web",
        "video" => "Видео",
        "preset" => "Пресет",
        "application" => "Приложение",
        _ => "Другое",
    }
}

// ---------------------------------------------------------------- UI state

struct Ui {
    win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    flow: gtk::FlowBox,
    split: adw::OverlaySplitView,
    side: gtk::Box,
    search: gtk::SearchEntry,
    type_dd: gtk::DropDown,
    sort_dd: gtk::DropDown,
    show_unsupported: gtk::Switch,
    empty: adw::StatusPage,
    daemon: RefCell<Option<Daemon>>,
    items: RefCell<Vec<Wp>>,
    list_raw: RefCell<String>,
    assign: RefCell<HashMap<String, String>>,
    selected: RefCell<Option<String>>,
    monitor_dd: gtk::DropDown,
    monitors: RefCell<Vec<String>>,
    debounce: RefCell<HashMap<String, glib::SourceId>>,
    building: Cell<bool>,
}

impl Ui {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }
    fn call<B: serde::Serialize + zbus::zvariant::DynamicType>(&self, method: &str, body: &B) -> bool {
        let r = self.daemon.borrow().as_ref().map(|d| d.call(method, body)).unwrap_or(Err("демон не запущен".into()));
        if let Err(e) = &r {
            self.toast(&format!("Ошибка: {e}"));
        }
        r.is_ok()
    }
    fn settings(&self) -> Value {
        self.daemon.borrow().as_ref().and_then(|d| d.json("GetSettings")).unwrap_or(json!({}))
    }
    fn wp(&self, id: &str) -> Option<Wp> {
        self.items.borrow().iter().find(|w| w.id == id).cloned()
    }
    /// Monitors on which `id` is currently set.
    fn active_on(&self, id: &str) -> Vec<String> {
        let a = self.assign.borrow();
        let mut v: Vec<String> = a.iter().filter(|(_, w)| w.as_str() == id).map(|(m, _)| if m == ALL { "все мониторы".to_string() } else { m.clone() }).collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------- cards

fn make_card(ui: &Rc<Ui>, w: &Wp) -> gtk::FlowBoxChild {
    let overlay = gtk::Overlay::new();
    let pic = gtk::Picture::new();
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_size_request(CARD_W, CARD_H);
    pic.add_css_class("card");
    if let Some(p) = w.preview.as_deref().filter(|p| std::path::Path::new(p).is_file()) {
        // Center-crop to 16:9 and scale: uniform cards, light grid; GIF previews show the first frame.
        if let Ok(pb) = gtk::gdk_pixbuf::Pixbuf::from_file(p) {
            let (w, h) = (pb.width(), pb.height());
            let (cw, ch) = if w * 9 > h * 16 { (h * 16 / 9, h) } else { (w, w * 9 / 16) };
            if let Some(sub) = pb.new_subpixbuf((w - cw) / 2, (h - ch) / 2, cw.max(1), ch.max(1)).scale_simple(CARD_W, CARD_H, gtk::gdk_pixbuf::InterpType::Bilinear) {
                pic.set_paintable(Some(&gdk::Texture::for_pixbuf(&sub)));
            }
        }
    }
    overlay.set_child(Some(&pic));

    let badge = gtk::Label::new(Some(kind_label(&w.kind)));
    badge.add_css_class("osd");
    badge.add_css_class("caption-heading");
    badge.set_halign(gtk::Align::Start);
    badge.set_valign(gtk::Align::Start);
    badge.set_margin_start(6);
    badge.set_margin_top(6);
    badge.set_margin_end(6);
    overlay.add_overlay(&badge);

    let active = ui.active_on(&w.id);
    if !active.is_empty() {
        let cur = gtk::Label::new(Some("✓ Текущие"));
        cur.add_css_class("osd");
        cur.add_css_class("caption-heading");
        cur.set_halign(gtk::Align::End);
        cur.set_valign(gtk::Align::Start);
        cur.set_margin_top(6);
        cur.set_margin_end(6);
        overlay.add_overlay(&cur);
    }

    let title = gtk::Label::new(Some(&w.title));
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_xalign(0.0);
    title.add_css_class("heading");
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);
    vbox.append(&overlay);
    vbox.append(&title);
    if !w.ok() {
        let note = gtk::Label::new(Some(if w.status == "broken" { "Не работает" } else { "Не поддерживается" }));
        note.set_xalign(0.0);
        note.add_css_class("caption");
        note.add_css_class("error");
        vbox.append(&note);
        vbox.set_opacity(0.45);
    }
    let child = gtk::FlowBoxChild::new();
    child.set_child(Some(&vbox));
    child.set_widget_name(&w.id);
    let tip = if w.ok() { w.title.clone() } else { format!("{}\n{}", w.title, w.reason) };
    child.set_tooltip_text(Some(&tip));
    child
}

fn repopulate(ui: &Rc<Ui>) {
    let keep = ui.selected.borrow().clone();
    while let Some(c) = ui.flow.first_child() {
        ui.flow.remove(&c);
    }
    for w in ui.items.borrow().iter() {
        let card = make_card(ui, w);
        ui.flow.append(&card);
        if keep.as_deref() == Some(w.id.as_str()) {
            ui.flow.select_child(&card);
        }
    }
    ui.flow.invalidate_filter();
    ui.flow.invalidate_sort();
    update_empty(ui);
}

fn update_empty(ui: &Rc<Ui>) {
    let mut any = false;
    let mut c = ui.flow.first_child();
    while let Some(w) = c {
        any |= w.is_child_visible() && ui.flow_filter(&w);
        c = w.next_sibling();
    }
    let has_items = !ui.items.borrow().is_empty();
    ui.empty.set_visible(!any);
    ui.empty.set_title(if has_items { "Ничего не найдено" } else { "Обоев пока нет" });
    ui.empty.set_description(Some(if has_items { "Измените поиск или фильтры, либо включите показ неподдерживаемых обоев." } else { "Добавьте каталог с обоями в настройках (меню → Настройки)." }));
}

impl Ui {
    fn flow_filter(&self, child: &gtk::Widget) -> bool {
        let Some(w) = self.wp(&child.widget_name()) else { return false };
        if !w.ok() && !self.show_unsupported.is_active() {
            return false;
        }
        let q = self.search.text().to_lowercase();
        if !q.is_empty() && !w.title.to_lowercase().contains(&q) {
            return false;
        }
        match self.type_dd.selected() {
            1 => w.kind == "scene",
            2 => w.kind == "web",
            3 => w.kind == "video",
            _ => true,
        }
    }
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
    let (raw, assign, mons) = {
        let d = ui.daemon.borrow();
        let d = d.as_ref().unwrap();
        let raw: String = d.proxy().and_then(|p| p.call("ListWallpapers", &())).unwrap_or_default();
        let st = d.json("GetSettings").unwrap_or(json!({}));
        let mons: Vec<String> = st["monitors"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(String::from)).collect();
        (raw, d.assignments(), mons)
    };
    if ui.stack.visible_child_name().as_deref() != Some("gallery") {
        ui.stack.set_visible_child_name("gallery");
    }
    if *ui.monitors.borrow() != mons {
        let mut labels = vec!["Все мониторы".to_string()];
        labels.extend(mons.iter().cloned());
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        ui.monitor_dd.set_model(Some(&gtk::StringList::new(&refs)));
        *ui.monitors.borrow_mut() = mons;
    }
    let changed_assign = *ui.assign.borrow() != assign;
    if raw != *ui.list_raw.borrow() || changed_assign || force {
        let items: Vec<Wp> = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.as_array().map(|a| a.iter().map(Wp::from_json).collect())).unwrap_or_default();
        *ui.items.borrow_mut() = items;
        *ui.assign.borrow_mut() = assign;
        *ui.list_raw.borrow_mut() = raw;
        repopulate(ui);
        if let Some(id) = ui.selected.borrow().clone() {
            update_header(ui, &id);
        }
    }
}

// ---------------------------------------------------------------- side panel

fn clear(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

fn update_header(ui: &Rc<Ui>, id: &str) {
    // Re-render the side panel only if the wallpaper vanished; titles rarely change.
    if ui.wp(id).is_none() {
        *ui.selected.borrow_mut() = None;
        ui.split.set_show_sidebar(false);
    }
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
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
        let mut send = Some(send);
        let src = glib::timeout_add_local(std::time::Duration::from_millis(delay_ms), move || {
            if let Some(s) = send.take() {
                s();
            }
            glib::ControlFlow::Break
        });
        ui.debounce.borrow_mut().insert(tag, src);
    }
}

fn build_props(ui: &Rc<Ui>, id: &str) -> Option<gtk::Widget> {
    let props: Value = ui.daemon.borrow().as_ref()?.proxy().ok()?.call::<_, _, String>("GetProperties", &(id,)).ok().and_then(|s| serde_json::from_str(&s).ok())?;
    let list = props.as_array().cloned()?;
    let mut groups: Vec<adw::PreferencesGroup> = Vec::new();
    let mut cur = adw::PreferencesGroup::new();
    cur.set_title("Свойства");
    let mut cur_rows = 0;
    for p in &list {
        let key = p["key"].as_str().unwrap_or("").to_string();
        let text = strip_tags(p["text"].as_str().unwrap_or(&key));
        let kind = &p["kind"];
        let ty = kind["type"].as_str().unwrap_or("");
        let id = id.to_string();
        match ty {
            "group" => {
                if cur_rows > 0 {
                    groups.push(cur);
                    cur = adw::PreferencesGroup::new();
                }
                cur.set_title(&text);
                continue;
            }
            "text" => {
                if text.is_empty() {
                    continue;
                }
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.set_title_lines(0);
                row.add_css_class("dim-label");
                cur.add(&row);
            }
            "bool" => {
                let row = adw::SwitchRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.set_active(kind["value"].as_bool().unwrap_or(false));
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_active_notify(move |r| {
                    if !ui.building.get() { send_prop(&ui, &id, &key, json!(r.is_active()), 0) }
                });
                cur.add(&row);
            }
            "slider" => {
                let (min, max) = (kind["min"].as_f64().unwrap_or(0.0), kind["max"].as_f64().unwrap_or(100.0));
                let step = kind["step"].as_f64().unwrap_or(1.0);
                let frac = kind["fraction"].as_bool().unwrap_or(false);
                let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max.max(min + step), step);
                scale.set_value(kind["value"].as_f64().unwrap_or(min));
                scale.set_draw_value(true);
                scale.set_digits(if frac { 2 } else { 0 });
                scale.set_hexpand(true);
                scale.set_size_request(160, -1);
                scale.set_valign(gtk::Align::Center);
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.add_suffix(&scale);
                let (ui, key) = (ui.clone(), key.clone());
                scale.connect_value_changed(move |s| {
                    if !ui.building.get() { send_prop(&ui, &id, &key, json!(s.value()), 120) }
                });
                cur.add(&row);
            }
            "combo" => {
                let opts: Vec<(String, String)> = kind["options"].as_array().into_iter().flatten().map(|o| (o["label"].as_str().unwrap_or("").to_string(), o["value"].as_str().unwrap_or("").to_string())).collect();
                let labels: Vec<String> = opts.iter().map(|o| strip_tags(&o.0)).collect();
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let row = adw::ComboRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.set_model(Some(&gtk::StringList::new(&refs)));
                let curv = kind["value"].as_str().unwrap_or("");
                row.set_selected(opts.iter().position(|o| o.1 == curv).unwrap_or(0) as u32);
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_selected_notify(move |r| {
                    if ui.building.get() { return }
                    if let Some(o) = opts.get(r.selected() as usize) { send_prop(&ui, &id, &key, json!(o.1), 0) }
                });
                cur.add(&row);
            }
            "color" => {
                let c = &kind["value"];
                let rgb: Vec<f32> = (0..3).map(|i| c[i].as_f64().unwrap_or(0.0) as f32).collect();
                let btn = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
                btn.set_rgba(&gdk::RGBA::new(rgb[0], rgb[1], rgb[2], 1.0));
                btn.set_valign(gtk::Align::Center);
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.add_suffix(&btn);
                let (ui, key) = (ui.clone(), key.clone());
                btn.connect_rgba_notify(move |b| {
                    if ui.building.get() { return }
                    let c = b.rgba();
                    send_prop(&ui, &id, &key, json!(format!("{:.4} {:.4} {:.4}", c.red(), c.green(), c.blue())), 150);
                });
                cur.add(&row);
            }
            "textinput" => {
                let row = adw::EntryRow::new();
                row.set_title(&glib::markup_escape_text(&text));
                row.set_text(kind["value"].as_str().unwrap_or(""));
                let (ui, key) = (ui.clone(), key.clone());
                row.connect_changed(move |r| {
                    if !ui.building.get() { send_prop(&ui, &id, &key, json!(r.text().to_string()), 400) }
                });
                cur.add(&row);
            }
            _ => continue, // file / directory / scenetexture / unknown: not editable here
        }
        cur_rows += 1;
    }
    if cur_rows > 0 {
        groups.push(cur);
    }
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    for g in &groups {
        b.append(g);
    }
    if groups.is_empty() {
        let l = gtk::Label::new(Some("У этих обоев нет настраиваемых свойств."));
        l.add_css_class("dim-label");
        l.set_wrap(true);
        b.append(&l);
    } else {
        let reset = gtk::Button::with_label("Сбросить свойства");
        reset.set_halign(gtk::Align::Start);
        let (ui2, id2) = (ui.clone(), ui.selected.borrow().clone().unwrap_or_default());
        reset.connect_clicked(move |_| {
            if ui2.call("ResetProperties", &(id2.as_str(),)) {
                show_side(&ui2, &id2);
            }
        });
        b.append(&reset);
    }
    Some(b.upcast())
}

fn show_side(ui: &Rc<Ui>, id: &str) {
    let Some(w) = ui.wp(id) else { return };
    ui.building.set(true);
    ui.monitor_dd.unparent();
    clear(&ui.side);
    let title = gtk::Label::new(Some(&w.title));
    title.add_css_class("title-2");
    title.set_wrap(true);
    title.set_xalign(0.0);
    ui.side.append(&title);
    let mut sub = kind_label(&w.kind).to_string();
    if !w.rating.is_empty() {
        sub += &format!(" · {}", w.rating);
    }
    let subl = gtk::Label::new(Some(&sub));
    subl.add_css_class("dim-label");
    subl.set_xalign(0.0);
    ui.side.append(&subl);
    let active = ui.active_on(id);
    if !active.is_empty() {
        let l = gtk::Label::new(Some(&format!("Сейчас установлены: {}", active.join(", "))));
        l.add_css_class("accent");
        l.set_xalign(0.0);
        l.set_wrap(true);
        ui.side.append(&l);
    }
    if w.ok() {
        let mon_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        ui.monitor_dd.set_hexpand(true);
        mon_row.append(&ui.monitor_dd);
        let apply = gtk::Button::with_label("Применить");
        apply.add_css_class("suggested-action");
        let (ui2, id2) = (ui.clone(), id.to_string());
        apply.connect_clicked(move |_| apply_wallpaper(&ui2, &id2));
        mon_row.append(&apply);
        ui.side.append(&mon_row);
    } else {
        let l = gtk::Label::new(Some(&format!("Не поддерживается: {}", w.reason)));
        l.add_css_class("error");
        l.set_wrap(true);
        l.set_xalign(0.0);
        ui.side.append(&l);
    }
    if w.ok() {
        if let Some(props) = build_props(ui, id) {
            ui.side.append(&props);
        }
    }
    ui.building.set(false);
}

fn apply_wallpaper(ui: &Rc<Ui>, id: &str) {
    let sel = ui.monitor_dd.selected() as usize;
    let mon = if sel == 0 { ALL.to_string() } else { ui.monitors.borrow().get(sel - 1).cloned().unwrap_or_else(|| ALL.into()) };
    if ui.call("SetWallpaper", &(mon.as_str(), id)) {
        let t = ui.wp(id).map(|w| w.title).unwrap_or_default();
        ui.toast(&format!("Применено: {t}"));
        refresh(ui, true);
        show_side(ui, id);
    }
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
    fps.set_title("Лимит FPS");
    fps.set_subtitle("Для сцен; перезапускает запущенные сцены");
    fps.set_value(st["fps"].as_f64().unwrap_or(30.0));
    let u = ui.clone();
    fps.connect_changed(move |r| { u.call("SetFps", &(r.value() as u32,)); });
    g.add(&fps);

    let vol = st["volume"].as_f64().unwrap_or(0.5);
    let muted = st["muted"].as_bool().unwrap_or(true);
    let snd = adw::SwitchRow::new();
    snd.set_title("Звук обоев");
    snd.set_subtitle("По умолчанию выключен");
    snd.set_active(!muted);
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

    let pause = adw::SwitchRow::new();
    pause.set_title("Пауза при развёрнутом окне");
    pause.set_subtitle("Останавливать обои, когда монитор закрыт окном на весь экран");
    pause.set_active(st["pause_on_maximized"].as_bool().unwrap_or(true));
    let u = ui.clone();
    pause.connect_active_notify(move |r| { u.call("SetPauseOnMaximized", &(r.is_active(),)); });
    g.add(&pause);
    page.add(&g);

    let dirs = adw::PreferencesGroup::new();
    dirs.set_title("Библиотеки обоев");
    dirs.set_description(Some("Steam Workshop и локальный каталог ~/.local/share/wpe/wallpapers находятся автоматически."));
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
    let fill: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    {
        let (ui, dirs, rows, fill2) = (ui.clone(), dirs.clone(), rows.clone(), fill.clone());
        *fill.borrow_mut() = Some(Box::new(move || {
            for r in rows.borrow_mut().drain(..) {
                dirs.remove(&r);
            }
            let st = ui.settings();
            for d in st["directories"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                let row = adw::ActionRow::new();
                row.set_title(&glib::markup_escape_text(d));
                let rm = gtk::Button::from_icon_name("user-trash-symbolic");
                rm.add_css_class("flat");
                rm.set_valign(gtk::Align::Center);
                rm.set_tooltip_text(Some("Убрать каталог"));
                let (ui2, path, f) = (ui.clone(), d.to_string(), fill2.clone());
                rm.connect_clicked(move |_| {
                    if ui2.call("RemoveDirectory", &(path.as_str(),)) {
                        if let Some(f) = f.borrow().as_ref() { f() }
                        refresh(&ui2, true);
                    }
                });
                row.add_suffix(&rm);
                dirs.add(&row);
                rows.borrow_mut().push(row);
            }
        }));
    }
    if let Some(f) = fill.borrow().as_ref() { f() }
    let add = adw::ButtonRow::new();
    add.set_title("Добавить папку…");
    add.set_start_icon_name(Some("folder-new-symbolic"));
    let (u, f, dlg2) = (ui.clone(), fill.clone(), dlg.clone());
    add.connect_activated(move |_| {
        let fd = gtk::FileDialog::new();
        fd.set_title("Каталог с обоями");
        let (u, f) = (u.clone(), f.clone());
        fd.select_folder(Some(&u.win.clone()), gio::Cancellable::NONE, move |res| {
            if let Ok(file) = res {
                if let Some(p) = file.path() {
                    if u.call("AddDirectory", &(p.to_string_lossy().as_ref(),)) {
                        if let Some(f) = f.borrow().as_ref() { f() }
                        refresh(&u, true);
                    }
                }
            }
        });
        let _ = &dlg2;
    });
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
    let btns = adw::PreferencesGroup::new();
    btns.add(&add);
    btns.add(&rescan);
    page.add(&dirs);
    page.add(&btns);
    dlg.add(&page);
    dlg.present(Some(&ui.win));
}

// ---------------------------------------------------------------- window

fn build(app: &adw::Application) {
    let toasts = adw::ToastOverlay::new();
    let win = adw::ApplicationWindow::builder().application(app).title("Живые обои").default_width(1280).default_height(780).content(&toasts).build();

    let search = gtk::SearchEntry::builder().placeholder_text("Поиск обоев").hexpand(true).build();
    let type_dd = gtk::DropDown::from_strings(&["Все типы", "Scene", "Web", "Видео"]);
    let sort_dd = gtk::DropDown::from_strings(&["По названию А–Я", "По названию Я–А", "По типу", "Сначала текущие"]);
    let show_unsupported = gtk::Switch::new();
    show_unsupported.set_valign(gtk::Align::Center);
    let monitor_dd = gtk::DropDown::from_strings(&["Все мониторы"]);

    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::Single);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(12);
    flow.set_activate_on_single_click(false);
    flow.set_row_spacing(6);
    flow.set_column_spacing(6);
    flow.set_margin_top(12);
    flow.set_margin_bottom(12);
    flow.set_margin_start(12);
    flow.set_margin_end(12);

    let empty = adw::StatusPage::new();
    empty.set_icon_name(Some("image-x-generic-symbolic"));
    empty.set_visible(false);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&empty);
    content.append(&flow);
    let scroller = gtk::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();

    let side = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for f in [gtk::Box::set_margin_top, gtk::Box::set_margin_bottom, gtk::Box::set_margin_start, gtk::Box::set_margin_end] {
        f(&side, 14);
    }
    let side_scroll = gtk::ScrolledWindow::builder().child(&side).hscrollbar_policy(gtk::PolicyType::Never).build();
    let side_view = adw::ToolbarView::new();
    let side_head = adw::HeaderBar::new();
    side_head.set_show_title(false);
    side_view.add_top_bar(&side_head);
    side_view.set_content(Some(&side_scroll));

    let split = adw::OverlaySplitView::new();
    split.set_sidebar_position(gtk::PackType::End);
    split.set_show_sidebar(false);
    split.set_min_sidebar_width(340.0);
    split.set_max_sidebar_width(420.0);
    split.set_content(Some(&scroller));
    split.set_sidebar(Some(&side_view));

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
    stack.add_named(&split, Some("gallery"));
    stack.add_named(&down, Some("down"));
    stack.set_visible_child_name("down");

    // Filter row.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.set_margin_start(12);
    bar.set_margin_end(12);
    bar.set_margin_top(6);
    bar.set_margin_bottom(6);
    bar.append(&search);
    bar.append(&type_dd);
    bar.append(&sort_dd);
    let un_label = gtk::Label::new(Some("Показать неподдерживаемые"));
    bar.append(&un_label);
    bar.append(&show_unsupported);

    let menu = gio::Menu::new();
    menu.append(Some("Настройки"), Some("app.settings"));
    menu.append(Some("Пересканировать"), Some("app.rescan"));
    menu.append(Some("О приложении"), Some("app.about"));
    let menu_btn = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Живые обои", "")));
    header.pack_end(&menu_btn);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&bar);
    view.set_content(Some(&stack));
    toasts.set_child(Some(&view));

    let ui = Rc::new(Ui {
        win: win.clone(), toasts, stack, flow: flow.clone(), split, side, search: search.clone(), type_dd: type_dd.clone(), sort_dd: sort_dd.clone(),
        show_unsupported: show_unsupported.clone(), empty, daemon: RefCell::new(None), items: RefCell::new(vec![]), list_raw: RefCell::new(String::new()),
        assign: RefCell::new(HashMap::new()), selected: RefCell::new(None), monitor_dd, monitors: RefCell::new(vec![]), debounce: RefCell::new(HashMap::new()),
        building: Cell::new(false),
    });

    let u = ui.clone();
    flow.set_filter_func(move |c| u.flow_filter(c.upcast_ref()));
    let u = ui.clone();
    flow.set_sort_func(move |a, b| {
        let (Some(x), Some(y)) = (u.wp(&a.widget_name()), u.wp(&b.widget_name())) else { return gtk::Ordering::Equal };
        let by_title = || x.title.to_lowercase().cmp(&y.title.to_lowercase());
        let ord = match u.sort_dd.selected() {
            1 => by_title().reverse(),
            2 => x.kind.cmp(&y.kind).then_with(by_title),
            3 => u.active_on(&y.id).len().cmp(&u.active_on(&x.id).len()).then_with(by_title),
            _ => by_title(),
        };
        ord.into()
    });
    let refilter = {
        let u = ui.clone();
        move || {
            u.flow.invalidate_filter();
            u.flow.invalidate_sort();
            update_empty(&u);
        }
    };
    let r = refilter.clone();
    search.connect_search_changed(move |_| r());
    let r = refilter.clone();
    type_dd.connect_selected_notify(move |_| r());
    let r = refilter.clone();
    sort_dd.connect_selected_notify(move |_| r());
    let r = refilter.clone();
    show_unsupported.connect_active_notify(move |_| r());

    let u = ui.clone();
    flow.connect_selected_children_changed(move |f| {
        match f.selected_children().first() {
            Some(c) => {
                let id = c.widget_name().to_string();
                if u.selected.borrow().as_deref() != Some(id.as_str()) {
                    *u.selected.borrow_mut() = Some(id.clone());
                    show_side(&u, &id);
                }
                u.split.set_show_sidebar(true);
            }
            None => {}
        }
    });
    let u = ui.clone();
    flow.connect_child_activated(move |_, c| {
        let id = c.widget_name().to_string();
        if u.wp(&id).map(|w| w.ok()).unwrap_or(false) {
            apply_wallpaper(&u, &id);
        }
    });

    start.connect_clicked({
        let u = ui.clone();
        move |_| {
            match std::process::Command::new("systemctl").args(["--user", "start", "wpe-daemon"]).status() {
                Ok(s) if s.success() => u.toast("Запускаем демон…"),
                _ => u.toast("Не удалось запустить: systemctl --user start wpe-daemon"),
            }
            let u2 = u.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || refresh(&u2, true));
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

    let u = ui.clone();
    glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
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
