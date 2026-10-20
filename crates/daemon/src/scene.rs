//! Scene wallpapers: command line and control protocol of the
//! linux-wallpaperengine subprocess (our fork adds `--control-stdin`, see
//! third_party/linux-wallpaperengine/). GTK-free, so it is unit-tested.

use crate::core::PointerKind;
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub struct SceneOptions {
    pub width: i32,
    pub height: i32,
    pub fps: u32,
    pub volume: f64,
    pub muted: bool,
    pub assets: Option<PathBuf>,
}

/// `$WPE_SCENE_BIN`, else `~/.local/bin/linux-wallpaperengine` (where
/// scripts/build-scene-renderer.sh installs it; the systemd user unit's PATH
/// usually lacks ~/.local/bin), else whatever is in PATH.
pub fn binary() -> PathBuf {
    if let Some(p) = std::env::var_os("WPE_SCENE_BIN") {
        return p.into();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".local/bin/linux-wallpaperengine");
        if p.exists() {
            return p;
        }
    }
    "linux-wallpaperengine".into()
}

/// `assets/` of the installed Wallpaper Engine app (needed by most scenes).
pub fn assets_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    we_core::wallpaper_engine_dirs(&home).into_iter().map(|d| d.join("assets")).find(|d| d.is_dir())
}

/// Value of `--set-property key=<value>` as linux-wallpaperengine parses it:
/// bools as 1/0, colors "r g b", numbers in C locale, strings as is.
pub fn property_arg(value: &Value) -> Option<String> {
    match value {
        Value::Bool(b) => Some(if *b { "1" } else { "0" }.into()),
        Value::Number(n) => Some(n.to_string()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Arguments for one monitor. `user_properties` is the
/// `{"key": {"type", "value"}}` object also sent to web wallpapers.
pub fn command_args(root: &Path, user_properties: &Value, o: &SceneOptions) -> Vec<OsString> {
    let mut a: Vec<OsString> = vec![
        "--control-stdin".into(),
        // Fullscreen detection is done by the GNOME extension (SetPaused).
        "--no-fullscreen-pause".into(),
        "--window".into(),
        format!("0x0x{}x{}", o.width, o.height).into(),
        "--scaling".into(),
        "fill".into(),
        "--fps".into(),
        o.fps.to_string().into(),
    ];
    // --silent mutes only the wallpaper's own sounds: linux-wallpaperengine
    // still captures system audio for projects with supportsaudioprocessing
    // (our patch 0002 corks that capture while paused).
    if o.muted || o.volume <= 0.0 {
        a.push("--silent".into());
    } else {
        a.push("--volume".into());
        a.push(((o.volume * 100.0).round() as u32).to_string().into());
    }
    if let Some(dir) = &o.assets {
        a.push("--assets-dir".into());
        a.push(dir.into());
    }
    for (key, prop) in user_properties.as_object().into_iter().flatten() {
        if let Some(v) = prop.get("value").and_then(property_arg) {
            a.push("--set-property".into());
            a.push(format!("{key}={v}").into());
        }
    }
    a.push(root.into());
    a
}

/// One file of a scene: the project directory first, then `scene.pkg`
/// (`PKGV…` archive: u32-length-prefixed strings, entry table of
/// name/offset/size, offsets relative to the end of the table).
pub fn read_scene_file(root: &Path, name: &str) -> Option<Vec<u8>> {
    if let Ok(b) = std::fs::read(root.join(name)) {
        return Some(b);
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::io::BufReader::new(std::fs::File::open(root.join("scene.pkg")).ok()?);
    let u32_ = |f: &mut std::io::BufReader<std::fs::File>| -> Option<u32> {
        let mut b = [0u8; 4];
        f.read_exact(&mut b).ok()?;
        Some(u32::from_le_bytes(b))
    };
    let string = |f: &mut std::io::BufReader<std::fs::File>, len: u32| -> Option<String> {
        if len > 4096 {
            return None;
        }
        let mut b = vec![0u8; len as usize];
        f.read_exact(&mut b).ok()?;
        String::from_utf8(b).ok()
    };
    let len = u32_(&mut f)?;
    if !string(&mut f, len)?.starts_with("PKGV") {
        return None;
    }
    let count = u32_(&mut f)?;
    let mut found = None;
    for _ in 0..count.min(1_000_000) {
        let len = u32_(&mut f)?;
        let entry = string(&mut f, len)?;
        let (offset, size) = (u32_(&mut f)?, u32_(&mut f)?);
        if entry == name {
            found = Some((offset, size));
        }
    }
    let (offset, size) = found?;
    let data_start = f.stream_position().ok()?;
    f.seek(SeekFrom::Start(data_start + offset as u64)).ok()?;
    let mut b = vec![0u8; size as usize];
    f.read_exact(&mut b).ok()?;
    Some(b)
}

/// Why linux-wallpaperengine cannot render this scene, judged by its scene
/// file; `None` if it looks fine or cannot be read (then we just try).
pub fn unsupported_reason(root: &Path, file: Option<&str>) -> Option<String> {
    let bytes = read_scene_file(root, file.unwrap_or("scene.json"))?;
    let scene: Value = serde_json::from_slice(&bytes).ok()?;
    let general = scene.get("general")?;
    if general.get("orthogonalprojection").map_or(true, Value::is_null) {
        return Some("3D-сцена с перспективной камерой (нет general.orthogonalprojection): linux-wallpaperengine такие не рисует".into());
    }
    None
}

/// Why the renderer died. `status` is the raw wait status (glib child
/// watch). Killed by a signal: that signal is the reason (its last output
/// lines are often unrelated warnings, e.g. GLFW "setting the window
/// position" on Wayland). Exited with an error: the last line that looks
/// like an error (GLFW warnings skipped), else the last non-empty one.
pub fn crash_reason(lines: &[String], status: i32) -> String {
    let signal = status & 0x7f;
    if signal != 0 {
        return format!("рендерер сцен упал: {} (сигнал {signal})", signal_name(signal));
    }
    const MARKS: [&str; 7] = ["error", "exception", "failed", "missing", "must ", "cannot", "unsupported"];
    let relevant = |l: &&String| !l.trim().is_empty() && !l.contains("GLFW error");
    let pick = lines
        .iter()
        .rev()
        .filter(relevant)
        .find(|l| MARKS.iter().any(|m| l.to_lowercase().contains(m)))
        .or_else(|| lines.iter().rev().find(relevant));
    let code = (status >> 8) & 0xff;
    match pick {
        Some(l) => {
            let l = l.trim();
            let mut s: String = l.chars().take(300).collect();
            if s.len() < l.len() {
                s.push('…');
            }
            format!("рендерер сцен упал: {s}")
        }
        None => format!("рендерер сцен завершился с кодом {code}"),
    }
}

fn signal_name(sig: i32) -> String {
    match sig {
        4 => "SIGILL".into(),
        6 => "SIGABRT".into(),
        7 => "SIGBUS".into(),
        8 => "SIGFPE".into(),
        9 => "SIGKILL".into(),
        11 => "SIGSEGV".into(),
        15 => "SIGTERM".into(),
        n => format!("сигнал {n}"),
    }
}

/// Automatic restarts of a crashed scene: at most `MAX` within `WINDOW`,
/// with growing delays; after that the scene stays down (status broken).
#[derive(Default)]
pub struct RestartPolicy {
    crashes: Vec<std::time::Instant>,
}

impl RestartPolicy {
    pub const MAX: usize = 3;
    pub const WINDOW: std::time::Duration = std::time::Duration::from_secs(300);
    const DELAYS: [u64; 3] = [2, 5, 15];

    /// A crash at `now`: delay before the restart, or `None` to give up.
    pub fn on_crash(&mut self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.crashes.retain(|t| now.duration_since(*t) < Self::WINDOW);
        if self.crashes.len() >= Self::MAX {
            return None;
        }
        let delay = Self::DELAYS[self.crashes.len()];
        self.crashes.push(now);
        Some(std::time::Duration::from_secs(delay))
    }
}

/// Control-channel lines for a pointer event (window coordinates, top-left origin).
pub fn pointer_lines(kind: PointerKind, x: f64, y: f64, button: u32) -> String {
    let mut s = format!("m {x:.1} {y:.1}\n");
    let which = match button {
        1 => 'l',
        3 => 'r',
        _ => return s,
    };
    match kind {
        PointerKind::Motion => {}
        PointerKind::Press => s.push_str(&format!("b {which} 1\n")),
        PointerKind::Release => s.push_str(&format!("b {which} 0\n")),
    }
    s
}

pub fn pause_line(paused: bool) -> &'static str {
    if paused {
        "p 1\n"
    } else {
        "p 0\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> SceneOptions {
        SceneOptions { width: 1600, height: 900, fps: 30, volume: 0.5, muted: true, assets: Some("/we/assets".into()) }
    }

    fn strs(a: &[OsString]) -> Vec<String> {
        a.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn args_silent_with_properties() {
        let props = json!({
            "schemecolor": {"type": "color", "value": "0.1 0.2 0.3"},
            "rain": {"type": "bool", "value": false},
            "speed": {"type": "slider", "value": 1.5},
            "file": {"type": "file", "value": null},
        });
        let a = strs(&command_args(Path::new("/ws/123"), &props, &opts()));
        assert_eq!(&a[..8], ["--control-stdin", "--no-fullscreen-pause", "--window", "0x0x1600x900", "--scaling", "fill", "--fps", "30"]);
        assert!(a.contains(&"--silent".into()));
        assert!(!a.contains(&"--volume".into()));
        assert!(a.windows(2).any(|w| w == ["--assets-dir", "/we/assets"]));
        assert!(a.windows(2).any(|w| w == ["--set-property", "schemecolor=0.1 0.2 0.3"]));
        assert!(a.windows(2).any(|w| w == ["--set-property", "rain=0"]));
        assert!(a.windows(2).any(|w| w == ["--set-property", "speed=1.5"]));
        assert!(!a.iter().any(|x| x.starts_with("file=")));
        assert_eq!(a.last().unwrap(), "/ws/123");
    }

    #[test]
    fn args_with_volume() {
        let o = SceneOptions { muted: false, volume: 0.42, assets: None, ..opts() };
        let a = strs(&command_args(Path::new("/x"), &json!({}), &o));
        assert!(a.windows(2).any(|w| w == ["--volume", "42"]));
        assert!(!a.contains(&"--silent".into()));
        assert!(!a.contains(&"--assets-dir".into()));
    }

    #[test]
    fn detects_perspective_scenes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.json"), r#"{"camera":{"eye":"0 0 3"},"general":{"clearcolor":"0 0 0"},"objects":[]}"#).unwrap();
        std::fs::write(d.path().join("scene.json"), r#"{"general":{"orthogonalprojection":{"width":1920,"height":1080}}}"#).unwrap();
        assert!(unsupported_reason(d.path(), Some("a.json")).unwrap().contains("orthogonalprojection"));
        assert_eq!(unsupported_reason(d.path(), None), None);
        assert_eq!(unsupported_reason(d.path(), Some("missing.json")), None);
    }

    #[test]
    fn reads_scene_from_pkg() {
        let d = tempfile::tempdir().unwrap();
        let files: [(&str, &[u8]); 2] = [("materials/x.json", b"{}"), ("scene.json", br#"{"general":{}}"#)];
        let s = |b: &mut Vec<u8>, t: &[u8]| {
            b.extend((t.len() as u32).to_le_bytes());
            b.extend(t);
        };
        let mut pkg = vec![];
        s(&mut pkg, b"PKGV0022");
        pkg.extend((files.len() as u32).to_le_bytes());
        let mut off = 0u32;
        for (n, data) in files {
            s(&mut pkg, n.as_bytes());
            pkg.extend(off.to_le_bytes());
            pkg.extend((data.len() as u32).to_le_bytes());
            off += data.len() as u32;
        }
        for (_, data) in files {
            pkg.extend(data);
        }
        std::fs::write(d.path().join("scene.pkg"), pkg).unwrap();
        assert_eq!(read_scene_file(d.path(), "scene.json").unwrap(), br#"{"general":{}}"#);
        assert!(unsupported_reason(d.path(), Some("scene.json")).is_some());
        assert_eq!(read_scene_file(d.path(), "nope.json"), None);
    }

    #[test]
    fn crash_reasons() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // exit(1): raw wait status 256
        assert_eq!(
            crash_reason(&l(&["Running with: x", "General section must have orthogonal projection info", "bye"]), 256),
            "рендерер сцен упал: General section must have orthogonal projection info"
        );
        assert_eq!(crash_reason(&l(&["a", "last line", ""]), 256), "рендерер сцен упал: last line");
        assert_eq!(crash_reason(&[], 256), "рендерер сцен завершился с кодом 1");
        // SIGSEGV with core (139): the signal, not the last GLFW warning
        let glfw = l(&["GLFW error 65548: Wayland: The platform does not support setting the window position"]);
        assert_eq!(crash_reason(&glfw, 139), "рендерер сцен упал: SIGSEGV (сигнал 11)");
        assert_eq!(crash_reason(&glfw, 256), "рендерер сцен завершился с кодом 1");
    }

    #[test]
    fn restart_backoff() {
        use std::time::{Duration, Instant};
        let mut p = RestartPolicy::default();
        let t0 = Instant::now();
        assert_eq!(p.on_crash(t0), Some(Duration::from_secs(2)));
        assert_eq!(p.on_crash(t0 + Duration::from_secs(10)), Some(Duration::from_secs(5)));
        assert_eq!(p.on_crash(t0 + Duration::from_secs(30)), Some(Duration::from_secs(15)));
        assert_eq!(p.on_crash(t0 + Duration::from_secs(60)), None, "4th crash within 5 min: give up");
        // the window slides: old crashes are forgotten
        assert_eq!(p.on_crash(t0 + Duration::from_secs(331)), Some(Duration::from_secs(2)));
        assert_eq!(p.on_crash(t0 + Duration::from_secs(340)), Some(Duration::from_secs(5)));
        assert_eq!(p.on_crash(t0 + Duration::from_secs(700)), Some(Duration::from_secs(2)));
    }

    #[test]
    fn pointer_protocol() {
        assert_eq!(pointer_lines(PointerKind::Motion, 10.0, 20.25, 0), "m 10.0 20.2\n");
        assert_eq!(pointer_lines(PointerKind::Press, 1.0, 2.0, 1), "m 1.0 2.0\nb l 1\n");
        assert_eq!(pointer_lines(PointerKind::Release, 1.0, 2.0, 3), "m 1.0 2.0\nb r 0\n");
        assert_eq!(pointer_lines(PointerKind::Press, 1.0, 2.0, 2), "m 1.0 2.0\n");
        assert_eq!(pause_line(true), "p 1\n");
    }
}
