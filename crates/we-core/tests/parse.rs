use serde_json::json;
use std::path::PathBuf;
use we_core::*;

fn fx(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

#[test]
fn video_project() {
    let p = Project::load(fx("video_1001")).unwrap();
    assert_eq!(p.kind, ProjectType::Video);
    assert_eq!(p.title, "Rain City");
    assert_eq!(p.id(), "1001");
    assert_eq!(p.file.as_deref(), Some("rain.mp4"));
    assert_eq!(p.tags, vec!["Landscape"]);
    assert_eq!(p.content_rating.as_deref(), Some("Everyone"));
    assert!(!p.supports_audio);
    assert!(p.validate().is_empty(), "{:?}", p.validate());
    let c = p.properties.get("schemecolor").unwrap();
    match c.kind {
        PropertyKind::Color { value } => assert!((value[2] - 0.50196).abs() < 1e-4),
        _ => panic!(),
    }
}

#[test]
fn web_project_properties() {
    let p = Project::load(fx("web_1002")).unwrap();
    assert_eq!(p.kind, ProjectType::Web);
    assert_eq!(p.workshop_id.as_deref(), Some("1002")); // numeric in json
    assert!(p.supports_audio);
    let keys: Vec<_> = p.properties.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys[0], "schemecolor");
    assert_eq!(keys.last(), Some(&"weird"));

    // 0..255 color normalised
    assert_eq!(p.properties.get("schemecolor").unwrap().value_json(), json!("1 0.50196 0"));
    let clock = p.properties.get("clockformat").unwrap();
    assert_eq!(clock.condition.as_deref(), Some("showclock.value == true"));
    assert!(matches!(&clock.kind, PropertyKind::Combo { options, value } if options.len() == 2 && value == "24"));
    assert!(matches!(p.properties.get("speed").unwrap().kind, PropertyKind::Slider { value, fraction: true, .. } if value == 2.5));
    // out-of-range default clamped
    assert!(matches!(p.properties.get("particles").unwrap().kind, PropertyKind::Slider { value, step, .. } if value == 500.0 && step == 1.0));
    assert!(matches!(p.properties.get("legacyflag").unwrap().kind, PropertyKind::Bool { value: true }));
    assert!(matches!(&p.properties.get("photos").unwrap().kind, PropertyKind::Directory { mode, value: None } if mode == "ondemand"));
    assert!(matches!(p.properties.get("hdr").unwrap().kind, PropertyKind::Text));
    assert!(matches!(&p.properties.get("weird").unwrap().kind, PropertyKind::Unknown { type_name, .. } if type_name == "hologram"));
}

#[test]
fn user_properties_json() {
    let p = Project::load(fx("web_1002")).unwrap();
    let up = p.properties.to_user_properties();
    assert_eq!(up["showclock"], json!({"type": "bool", "value": true}));
    assert_eq!(up["speed"]["value"], json!(2.5));
    assert_eq!(up["bg"]["value"], json!(null));
    assert!(up.get("hdr").is_none());
    assert!(up.get("grp").is_none());
    let vals = p.properties.values();
    assert_eq!(vals["greeting"], json!("Hello"));
}

#[test]
fn setting_values() {
    let mut p = Project::load(fx("web_1002")).unwrap().properties;
    p.set("particles", &json!(123.4)).unwrap();
    assert_eq!(p.get("particles").unwrap().value_json(), json!(123.0));
    p.set("particles", &json!(-5)).unwrap();
    assert_eq!(p.get("particles").unwrap().value_json(), json!(10.0));
    p.set("speed", &json!("3.3")).unwrap();
    assert_eq!(p.get("speed").unwrap().value_json(), json!(3.3));
    p.set("showclock", &json!("false")).unwrap();
    assert_eq!(p.get("showclock").unwrap().value_json(), json!(false));
    p.set("schemecolor", &json!("#ff0000")).unwrap();
    assert_eq!(p.get("schemecolor").unwrap().value_json(), json!("1 0 0"));
    p.set("bg", &json!("/home/u/pic.png")).unwrap();
    assert!(p.set("clockformat", &json!("99")).is_err());
    assert!(p.set("hdr", &json!(1)).is_err());
    assert!(p.set("speed", &json!("abc")).is_err());
    assert!(matches!(p.set("nope", &json!(1)), Err(Error::UnknownProperty(_))));
}

#[test]
fn localization() {
    let p = Project::load(fx("web_1002")).unwrap();
    assert_eq!(p.localize("ui_clock", "ru-RU"), "Показывать часы");
    assert_eq!(p.localize("ui_clock", "ru"), "Показывать часы");
    assert_eq!(p.localize("ui_speed", "ru-ru"), "Speed");
    assert_eq!(p.localize("Format", "de-de"), "Format");
}

#[test]
fn scenes() {
    let s = Project::load(fx("scene_1003")).unwrap();
    assert_eq!(s.kind, ProjectType::Scene);
    assert!(!s.is_packaged_scene());
    assert!(s.validate().is_empty());
    assert!(matches!(s.properties.get("tex").unwrap().kind, PropertyKind::SceneTexture { value: None }));
    let pkg = Project::load(fx("scenepkg_1004")).unwrap();
    assert!(pkg.is_packaged_scene());
    assert!(pkg.validate().is_empty(), "{:?}", pkg.validate());
}

#[test]
fn application_unsupported() {
    let p = Project::load(fx("app_1006")).unwrap();
    assert!(!p.kind.is_supported());
    let v = p.validate();
    assert!(v.contains(&ValidationIssue::UnsupportedType("application".into())));
    assert!(v.iter().any(|i| matches!(i, ValidationIssue::EntryFileNotFound(_))));
}

#[test]
fn type_inference_and_bom_and_path_escape() {
    let p = Project::load(fx("notype")).unwrap();
    assert_eq!(p.kind, ProjectType::Video);
    assert_eq!(p.id(), "notype");
    let b = Project::load(fx("local_bom")).unwrap();
    assert_eq!(b.workshop_id, None); // "0" treated as local
    assert_eq!(b.entry_path(), None);
    assert!(b.validate().contains(&ValidationIssue::PathEscapesRoot("../../etc/passwd".into())));
}

#[test]
fn broken_and_missing() {
    assert!(matches!(Project::load(fx("broken")), Err(Error::Json { .. })));
    assert!(matches!(Project::load(fx("does_not_exist")), Err(Error::Io { .. })));
    assert!(matches!(Project::from_json_str("[1]", "/x"), Err(Error::NotAnObject)));
    let p = Project::from_json_str("{}", "/tmp/abc").unwrap();
    assert_eq!(p.title, "abc");
    assert!(p.properties.is_empty());
    assert!(p.validate().contains(&ValidationIssue::MissingEntryFile));
}

#[test]
fn type_parse_case_insensitive() {
    assert_eq!(ProjectType::parse("VIDEO"), ProjectType::Video);
    assert_eq!(ProjectType::parse(" Web "), ProjectType::Web);
    assert_eq!(ProjectType::parse("x"), ProjectType::Unknown("x".into()));
}

#[test]
fn exe_without_type_and_typeless_label() {
    // Mirrors the official "sheep" project and a workshop label property.
    let p = Project::load(fx("unity_exe")).unwrap();
    assert_eq!(p.kind, ProjectType::Application);
    assert!(!p.kind.is_supported());
    let label = p.properties.get("imgbst").unwrap();
    assert!(matches!(label.kind, PropertyKind::Text));
    assert_eq!(label.condition, None); // empty condition ignored
    assert!(!label.has_value());
}
