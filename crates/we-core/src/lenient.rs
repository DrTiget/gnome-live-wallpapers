//! WE's project.json files are hand-edited and written by many tool versions:
//! numbers come as strings, booleans as 0/1 or "true", etc. These helpers
//! accept every variant seen in the wild.

use serde_json::Value;

pub fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|f| f != 0.0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Some(true),
            "false" | "0" | "no" | "off" | "" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

pub fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Non-empty string field.
pub fn str_field(obj: &Value, key: &str) -> Option<String> {
    obj.get(key).and_then(as_string).filter(|s| !s.trim().is_empty())
}

/// Parse a WE color: "r g b" with components in 0..1 (or 0..255, auto-detected),
/// also "#rrggbb" and "r,g,b".
pub fn parse_color(v: &Value) -> Option<[f32; 3]> {
    let s = as_string(v)?;
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let p = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok().map(|x| x as f32 / 255.0);
            return Some([p(0)?, p(2)?, p(4)?]);
        }
        return None;
    }
    let parts: Vec<f32> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    if parts.len() < 3 {
        return None;
    }
    let mut rgb = [parts[0], parts[1], parts[2]];
    if rgb.iter().any(|c| *c > 1.0) {
        for c in &mut rgb {
            *c /= 255.0;
        }
    }
    for c in &mut rgb {
        *c = c.clamp(0.0, 1.0);
    }
    Some(rgb)
}

pub fn format_color(c: [f32; 3]) -> String {
    let f = |x: f32| {
        let s = format!("{:.5}", x);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        if s.is_empty() { "0".to_string() } else { s.to_string() }
    };
    format!("{} {} {}", f(c[0]), f(c[1]), f(c[2]))
}
