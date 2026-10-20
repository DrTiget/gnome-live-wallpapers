//! Gallery. Stage 1: CLI over the daemon's D-Bus API (works without GTK).
//! Stage 3: libadwaita UI (feature `gtk`) — grid of previews, per-monitor
//! selection, property editor generated from we_core::PropertyKind.

use zbus::blocking::{Connection, Proxy};

const USAGE: &str = "usage: wpe-gallery list | scan <dir> | set <monitor|*> <id> | props <id> | prop <id> <key> <json>";

fn proxy(conn: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(conn, "io.github.wpe.Daemon", "/io/github/wpe/Daemon", "io.github.wpe.Daemon1")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        // Offline: scan a directory directly with we-core.
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
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
    Ok(())
}
