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

/// Short crash reason from the renderer's last output lines: the last line
/// that looks like an error, else the last non-empty one.
pub fn crash_reason(lines: &[String], status: i32) -> String {
    const MARKS: [&str; 7] = ["error", "exception", "failed", "missing", "must ", "cannot", "unsupported"];
    let pick = lines
        .iter()
        .rev()
        .find(|l| MARKS.iter().any(|m| l.to_lowercase().contains(m)))
        .or_else(|| lines.iter().rev().find(|l| !l.trim().is_empty()));
    match pick {
        Some(l) => {
            let l = l.trim();
            let mut s: String = l.chars().take(300).collect();
            if s.len() < l.len() {
                s.push('…');
            }
            format!("рендерер сцен упал: {s}")
        }
        None => format!("рендерер сцен завершился при запуске (статус {status})"),
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
        assert_eq!(
            crash_reason(&l(&["Running with: x", "General section must have orthogonal projection info", "bye"]), 1),
            "рендерер сцен упал: General section must have orthogonal projection info"
        );
        assert_eq!(crash_reason(&l(&["a", "last line", ""]), 1), "рендерер сцен упал: last line");
        assert!(crash_reason(&[], 139).contains("139"));
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
