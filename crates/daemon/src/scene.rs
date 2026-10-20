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
    fn pointer_protocol() {
        assert_eq!(pointer_lines(PointerKind::Motion, 10.0, 20.25, 0), "m 10.0 20.2\n");
        assert_eq!(pointer_lines(PointerKind::Press, 1.0, 2.0, 1), "m 1.0 2.0\nb l 1\n");
        assert_eq!(pointer_lines(PointerKind::Release, 1.0, 2.0, 3), "m 1.0 2.0\nb r 0\n");
        assert_eq!(pointer_lines(PointerKind::Press, 1.0, 2.0, 2), "m 1.0 2.0\n");
        assert_eq!(pause_line(true), "p 1\n");
    }
}
