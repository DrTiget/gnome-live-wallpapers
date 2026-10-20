use std::fs;
use std::path::PathBuf;
use we_core::*;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn scan_fixtures() {
    let cat = Catalog::scan(&[fixtures()]);
    assert_eq!(cat.projects.len(), 8);
    assert_eq!(cat.errors.len(), 1);
    assert!(cat.errors[0].dir.ends_with("broken"));
    assert_eq!(cat.by_type(&ProjectType::Scene).count(), 2);
    assert_eq!(cat.supported().count(), 7);
    assert_eq!(cat.type_counts()[&ProjectType::Video], 2);
    assert_eq!(cat.search("audio").count(), 2); // title and tag
    assert_eq!(cat.search("RAINY").count(), 1); // description
    assert_eq!(cat.search("").count(), 8);
}

#[test]
fn preset_resolution() {
    let cat = Catalog::scan(&[fixtures()]);
    let preset = cat.get("1005").unwrap();
    assert_eq!(preset.kind, ProjectType::Preset);
    assert!(preset.validate().is_empty());
    let r = cat.resolve_preset(preset).unwrap();
    assert_eq!(r.kind, ProjectType::Web);
    assert_eq!(r.title, "Audio Bars (fast, no clock)");
    assert_eq!(r.properties.get("speed").unwrap().value_json(), serde_json::json!(7.0));
    assert_eq!(r.properties.get("showclock").unwrap().value_json(), serde_json::json!(false));
    assert_eq!(r.root, fixtures().join("web_1002"));
}

#[test]
fn scan_single_project_dir_and_missing_root() {
    let cat = Catalog::scan(&[fixtures().join("video_1001"), fixtures().join("nope")]);
    assert_eq!(cat.projects.len(), 1);
    assert!(cat.errors.is_empty());
}

#[test]
fn steam_discovery() {
    let home = tempfile::tempdir().unwrap();
    let steam = home.path().join(".local/share/Steam");
    let ws = steam.join("steamapps/workshop/content/431960/42");
    fs::create_dir_all(&ws).unwrap();
    fs::write(ws.join("project.json"), r#"{"type":"video","file":"a.mp4","title":"A"}"#).unwrap();

    let lib2 = home.path().join("games/SteamLibrary");
    let ws2 = lib2.join("steamapps/workshop/content/431960/43");
    fs::create_dir_all(&ws2).unwrap();
    fs::write(ws2.join("project.json"), r#"{"type":"web","file":"index.html","title":"B"}"#).unwrap();
    fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!("\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n", steam.display(), lib2.display()),
    )
    .unwrap();
    // ~/.steam/steam -> ~/.local/share/Steam must not produce duplicates
    fs::create_dir_all(home.path().join(".steam")).unwrap();
    std::os::unix::fs::symlink(&steam, home.path().join(".steam/steam")).unwrap();

    let dirs = workshop_dirs(home.path());
    assert_eq!(dirs.len(), 2, "{dirs:?}");
    let cat = Catalog::scan_steam(home.path());
    let mut titles: Vec<_> = cat.projects.iter().map(|p| p.title.clone()).collect();
    titles.sort();
    assert_eq!(titles, ["A", "B"]);
    assert_eq!(cat.get("42").unwrap().title, "A");
}
