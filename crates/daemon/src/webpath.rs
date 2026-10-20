//! Pure helpers for the `wpe://` URI scheme that serves web wallpapers.
//!
//! Web wallpapers are loaded as `wpe://<host>/<path>` instead of `file://`:
//! frameworks such as Angular call `history.replaceState()` with the
//! directory URL, which WebKit forbids for `file:` URLs (blank page). The
//! host identifies the wallpaper root directory.

use std::path::{Component, Path, PathBuf};

pub const SCHEME: &str = "wpe";

/// Stable host name for a wallpaper root (hex hash, valid in URLs).
pub fn host_for_root(root: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    format!("w{:016x}", h.finish())
}

fn unescape(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Map a URL path ("/a/b.js", percent-encoded) to a file under `root`.
/// Rejects anything escaping the root; directories resolve to index.html.
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let path = url_path.split(['?', '#']).next().unwrap_or("");
    let decoded = unescape(path)?;
    let rel = Path::new(decoded.trim_start_matches('/'));
    if !rel.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return None;
    }
    let mut p = root.join(rel);
    if p.is_dir() {
        p = p.join("index.html");
    }
    p.is_file().then_some(p)
}

/// Percent-encode a relative path for use in a `wpe://` URL (keeps `/`).
pub fn encode_path(rel: &str) -> String {
    let mut out = String::with_capacity(rel.len());
    for b in rel.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parse a single `Range: bytes=a-b` / `bytes=a-` / `bytes=-n` header for a
/// file of `len` bytes. Returns inclusive (start, end).
pub fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => (s.parse().ok()?, len - 1),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(len - 1)),
    };
    (start <= end && start < len).then_some((start, end))
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("mp4" | "m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("ogg" | "ogv") => "video/ogg",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_inside_root_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub dir")).unwrap();
        std::fs::write(dir.path().join("index.html"), "x").unwrap();
        std::fs::write(dir.path().join("sub dir/a.js"), "x").unwrap();
        let r = dir.path();
        assert_eq!(resolve(r, "/"), Some(r.join("index.html")));
        assert_eq!(resolve(r, ""), Some(r.join("index.html")));
        assert_eq!(resolve(r, "/sub%20dir/a.js?v=1#x"), Some(r.join("sub dir/a.js")));
        assert_eq!(resolve(r, "/../etc/passwd"), None);
        assert_eq!(resolve(r, "/sub%20dir/..%2F..%2Fx"), None);
        assert_eq!(resolve(r, "/missing.js"), None);
        assert_eq!(resolve(r, "/%zz"), None);
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }

    #[test]
    fn host_is_stable_and_url_safe() {
        let h = host_for_root(Path::new("/a/b"));
        assert_eq!(h, host_for_root(Path::new("/a/b")));
        assert_ne!(h, host_for_root(Path::new("/a/c")));
        assert!(h.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(mime_for(Path::new("x/MAIN.JS")), "text/javascript");
        assert_eq!(encode_path("a b/ü.html"), "a%20b/%C3%BC.html");
    }
}
