//! Gallery: without arguments — libadwaita UI (feature `gtk`, see gui.rs),
//! with arguments — CLI over the daemon's D-Bus API (works without GTK).

#[cfg(feature = "gtk")]
mod gui;

use zbus::blocking::{Connection, Proxy};

const USAGE: &str = "usage: wpe-gallery list | scan [-v] <dir> | add-dir <dir> | volume <0..1> <muted:true|false> | fps [1..240] | set <monitor|*> <id> | props <id> | prop <id> <key> <json> | fav <id> [on|off] | collections | playlist <monitor> [--source all|favorites|<id>] [--interval N] [--shuffle|--sequential] [--off] | next [monitor]";

fn proxy(conn: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(conn, "io.github.wpe.Daemon", "/io/github/wpe/Daemon", "io.github.wpe.Daemon1")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "gtk")]
    if args.is_empty() {
        std::process::exit(gui::run());
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        // Offline: scan a directory directly with we-core.
        ["scan", "-v", dir] => {
            let cat = we_core::Catalog::scan(&[dir]);
            for p in &cat.projects {
                let issues = p.validate();
                let unknown: Vec<String> = p
                    .properties
                    .iter()
                    .filter_map(|x| match &x.kind {
                        we_core::PropertyKind::Unknown { type_name, .. } => Some(format!("{}:{}", x.key, type_name)),
                        _ => None,
                    })
                    .collect();
                println!(
                    "{:<12} {:<11} props={:<3} audio={} {}{}{}",
                    p.id(),
                    p.kind.as_str(),
                    p.properties.len(),
                    p.supports_audio as u8,
                    p.title,
                    if issues.is_empty() { String::new() } else { format!("  ISSUES {issues:?}") },
                    if unknown.is_empty() { String::new() } else { format!("  UNKNOWN {unknown:?}") },
                );
            }
            for e in &cat.errors {
                eprintln!("error: {}: {}", e.dir.display(), e.error);
            }
        }
        ["scan", dir] => {
            let cat = we_core::Catalog::scan(&[dir]);
            for p in &cat.projects {
                println!("{:<12} {:<8} {}", p.id(), p.kind.as_str(), p.title);
            }
            for e in &cat.errors {
                eprintln!("error: {}: {}", e.dir.display(), e.error);
            }
        }
        ["list"] => {
            let conn = Connection::session()?;
            let json: String = proxy(&conn)?.call("ListWallpapers", &())?;
            let v: serde_json::Value = serde_json::from_str(&json)?;
            for w in v.as_array().into_iter().flatten() {
                println!("{:<12} {:<8} {}", w["id"].as_str().unwrap_or(""), w["type"].as_str().unwrap_or(""), w["title"].as_str().unwrap_or(""));
            }
        }
        ["add-dir", dir] => {
            let conn = Connection::session()?;
            let abs = std::fs::canonicalize(dir)?;
            proxy(&conn)?.call::<_, _, ()>("AddDirectory", &(abs.to_string_lossy().as_ref(),))?;
        }
        ["volume", vol, muted] => {
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("SetVolume", &(vol.parse::<f64>()?, muted.parse::<bool>()?))?;
        }
        ["fps"] => {
            let conn = Connection::session()?;
            let fps: u32 = proxy(&conn)?.call("GetFps", &())?;
            println!("{fps}");
        }
        ["fps", n] => {
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("SetFps", &(n.parse::<u32>()?,))?;
        }
        ["set", mon, id] => {
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("SetWallpaper", &(mon, id))?;
        }
        ["props", id] => {
            let conn = Connection::session()?;
            let json: String = proxy(&conn)?.call("GetProperties", &(id,))?;
            println!("{}", serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(&json)?)?);
        }
        ["prop", id, key, val] => {
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("SetProperty", &(id, key, val))?;
        }
        ["fav", id] | ["fav", id, "on"] | ["fav", id, "off"] => {
            let on = a.get(2) != Some(&"off");
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("SetFavorite", &(id, on))?;
        }
        ["collections"] => {
            let conn = Connection::session()?;
            let json: String = proxy(&conn)?.call("GetCollections", &())?;
            let v: serde_json::Value = serde_json::from_str(&json)?;
            for c in v.as_array().into_iter().flatten() {
                let items: Vec<&str> = c["items"].as_array().into_iter().flatten().filter_map(|x| x.as_str()).collect();
                println!("{:<36} {} ({}): {}", c["id"].as_str().unwrap_or(""), c["name"].as_str().unwrap_or(""), items.len(), items.join(" "));
            }
        }
        ["playlist", mon, opts @ ..] => {
            let conn = Connection::session()?;
            let p = proxy(&conn)?;
            let json: String = p.call("GetPlaylist", &(mon,))?;
            let mut cfg: serde_json::Value = serde_json::from_str(&json)?;
            if opts.is_empty() {
                println!("{}", serde_json::to_string_pretty(&cfg)?);
                return Ok(());
            }
            cfg["enabled"] = true.into();
            let mut it = opts.iter();
            while let Some(o) = it.next() {
                match *o {
                    "--source" => cfg["source"] = it.next().ok_or("--source needs a value")?.to_string().into(),
                    "--interval" => cfg["interval_min"] = it.next().ok_or("--interval needs minutes")?.parse::<u32>()?.into(),
                    "--shuffle" => cfg["order"] = "shuffle".into(),
                    "--sequential" => cfg["order"] = "sequential".into(),
                    "--sync" => cfg["sync_monitors"] = true.into(),
                    "--off" => cfg["enabled"] = false.into(),
                    other => return Err(format!("unknown option {other}").into()),
                }
            }
            p.call::<_, _, ()>("SetPlaylist", &(mon, cfg.to_string()))?;
        }
        ["next"] | ["next", _] => {
            let conn = Connection::session()?;
            proxy(&conn)?.call::<_, _, ()>("NextWallpaper", &(a.get(1).copied().unwrap_or("*"),))?;
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    Ok(())
}
