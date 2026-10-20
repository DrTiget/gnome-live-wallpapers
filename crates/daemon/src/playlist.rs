//! Playlist logic without I/O: choosing the next wallpaper.

/// Next wallpaper from `candidates` (already filtered to playable ones).
/// Sequential: the one after `current` (wrapping; first if `current` is not
/// in the list). Shuffle: a random one different from `current` (`rnd` is any
/// random number). `None` if there is nothing to show.
pub fn pick_next(candidates: &[String], current: Option<&str>, shuffle: bool, rnd: u64) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let pos = current.and_then(|c| candidates.iter().position(|x| x == c));
    if shuffle {
        let others: Vec<&String> = candidates.iter().filter(|x| Some(x.as_str()) != current).collect();
        if others.is_empty() {
            return Some(candidates[0].clone());
        }
        return Some(others[(rnd % others.len() as u64) as usize].clone());
    }
    Some(candidates[pos.map_or(0, |p| (p + 1) % candidates.len())].clone())
}

pub fn random_u64() -> u64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    // splitmix64 finalizer
    let mut z = t.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sequential_wraps() {
        let c = v(&["a", "b", "c"]);
        assert_eq!(pick_next(&c, Some("a"), false, 0).as_deref(), Some("b"));
        assert_eq!(pick_next(&c, Some("c"), false, 0).as_deref(), Some("a"));
        assert_eq!(pick_next(&c, Some("zzz"), false, 0).as_deref(), Some("a"));
        assert_eq!(pick_next(&c, None, false, 0).as_deref(), Some("a"));
    }

    #[test]
    fn shuffle_avoids_current() {
        let c = v(&["a", "b", "c"]);
        for r in 0..20 {
            assert_ne!(pick_next(&c, Some("b"), true, r).as_deref(), Some("b"));
        }
        assert_eq!(pick_next(&v(&["a"]), Some("a"), true, 5).as_deref(), Some("a"));
        assert_eq!(pick_next(&[], None, true, 5), None);
    }
}
