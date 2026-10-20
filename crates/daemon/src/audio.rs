//! Audio spectrum for `wallpaperRegisterAudioListener`: captures the monitor of
//! the default PipeWire sink with a `pw-record` subprocess (no libpipewire
//! bindings to build; killed whenever no visible wallpaper listens) and turns
//! it into 128 values, 64 bands of the left channel then 64 of the right,
//! 0..1, ~30 times a second, like Wallpaper Engine. GTK-free, unit-tested.

use std::io::Read;
use std::process::{Child, Command, Stdio};

pub const RATE: usize = 24_000;
pub const FFT_SIZE: usize = 1024;
pub const BANDS: usize = 64;
/// Frames between two spectra: 24000 / 800 = 30 Hz.
pub const HOP: usize = 800;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 11_000.0;

/// In-place iterative radix-2 FFT; `re.len()` must be a power of two.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (ang * k as f32).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

pub struct Analyzer {
    /// Last FFT_SIZE frames per channel (ring buffers).
    ring: [Vec<f32>; 2],
    pos: usize,
    since_last: usize,
    window: Vec<f32>,
    /// FFT bin range of every band.
    bands: Vec<(usize, usize)>,
    out: [f32; 2 * BANDS],
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Default for Analyzer {
    fn default() -> Self {
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos())
            .collect();
        let hz_per_bin = RATE as f32 / FFT_SIZE as f32;
        let bands = (0..BANDS)
            .map(|b| {
                let f = |x: usize| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(x as f32 / BANDS as f32);
                let lo = ((f(b) / hz_per_bin).round() as usize).max(1);
                let hi = ((f(b + 1) / hz_per_bin).round() as usize).max(lo + 1);
                (lo, hi)
            })
            .collect();
        Analyzer {
            ring: [vec![0.0; FFT_SIZE], vec![0.0; FFT_SIZE]],
            pos: 0,
            since_last: 0,
            window,
            bands,
            out: [0.0; 2 * BANDS],
            re: vec![0.0; FFT_SIZE],
            im: vec![0.0; FFT_SIZE],
        }
    }
}

impl Analyzer {
    /// Feed interleaved stereo frames; returns a spectrum every HOP frames.
    pub fn push(&mut self, interleaved: &[f32], mut on_spectrum: impl FnMut(&[f32; 2 * BANDS])) {
        for fr in interleaved.chunks_exact(2) {
            self.ring[0][self.pos] = fr[0];
            self.ring[1][self.pos] = fr[1];
            self.pos = (self.pos + 1) % FFT_SIZE;
            self.since_last += 1;
            if self.since_last == HOP {
                self.since_last = 0;
                self.analyze();
                on_spectrum(&self.out);
            }
        }
    }

    fn analyze(&mut self) {
        // Amplitude of a full-scale sine = 1 after this scaling (Hann gain 0.5).
        let scale = 4.0 / FFT_SIZE as f32;
        for ch in 0..2 {
            for i in 0..FFT_SIZE {
                self.re[i] = self.ring[ch][(self.pos + i) % FFT_SIZE] * self.window[i];
                self.im[i] = 0.0;
            }
            fft(&mut self.re, &mut self.im);
            for (b, &(lo, hi)) in self.bands.iter().enumerate() {
                let peak = (lo..hi).map(|k| self.re[k].hypot(self.im[k])).fold(0.0f32, f32::max) * scale;
                // -60 dB .. 0 dB -> 0 .. 1, highs tilted up a bit (music falls off with frequency).
                let tilt = 6.0 * b as f32 / BANDS as f32;
                let target = ((20.0 * peak.max(1e-9).log10() + tilt + 60.0) / 60.0).clamp(0.0, 1.0);
                // Fast attack, slower decay, like WE's smoothing.
                let v = &mut self.out[ch * BANDS + b];
                *v += (target - *v) * if target > *v { 0.7 } else { 0.25 };
                if *v < 1e-3 {
                    *v = 0.0;
                }
            }
        }
    }
}

/// JS array literal for `window.__wpe.audio(...)`.
pub fn js_array(s: &[f32]) -> String {
    let mut out = String::with_capacity(s.len() * 6 + 2);
    out.push('[');
    for (i, v) in s.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        if *v == 0.0 {
            out.push('0');
        } else {
            out.push_str(&format!("{v:.3}"));
        }
    }
    out.push(']');
    out
}

/// `pw-record` capturing the default sink's monitor as raw f32 stereo.
/// `$WPE_AUDIO_TARGET` picks another node (tests use a null sink).
pub fn command() -> Command {
    let mut c = Command::new("pw-record");
    c.args(["-P", "{ stream.capture.sink=true node.name=wpe-audio-spectrum node.description=\"Live wallpaper spectrum\" }"]);
    if let Some(t) = std::env::var_os("WPE_AUDIO_TARGET") {
        c.arg("--target").arg(t);
    }
    c.args(["--raw", "--format", "f32", "--channels", "2", "--latency", "400"])
        .arg("--rate")
        .arg(RATE.to_string())
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    c
}

/// Running capture; dropping it kills `pw-record`, which ends the reader thread.
pub struct Capture {
    child: Child,
}

impl Capture {
    /// `on_spectrum` runs on the reader thread. Consecutive silent spectra
    /// after the first one are not reported.
    pub fn start(mut cmd: Command, on_spectrum: impl Fn(Vec<f32>) + Send + 'static) -> std::io::Result<Capture> {
        let mut child = cmd.spawn()?;
        let mut stdout = child.stdout.take().expect("piped");
        std::thread::Builder::new().name("wpe-audio".into()).spawn(move || {
            let mut analyzer = Analyzer::default();
            let mut bytes = vec![0u8; HOP * 8];
            let mut samples = Vec::with_capacity(HOP * 2);
            let mut was_silent = false;
            let mut carry = 0;
            loop {
                let n = match stdout.read(&mut bytes[carry..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n + carry,
                };
                let whole = n - n % 4;
                samples.clear();
                samples.extend(bytes[..whole].chunks_exact(4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])));
                bytes.copy_within(whole..n, 0);
                carry = n - whole;
                analyzer.push(&samples, |s| {
                    let silent = s.iter().all(|v| *v == 0.0);
                    if !(silent && was_silent) {
                        on_spectrum(s.to_vec());
                    }
                    was_silent = silent;
                });
            }
        })?;
        Ok(Capture { child })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz_l: f32, hz_r: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / RATE as f32;
                let w = 2.0 * std::f32::consts::PI;
                [(w * hz_l * t).sin() * 0.5, (w * hz_r * t).sin() * 0.5]
            })
            .collect()
    }

    fn last_spectrum(input: &[f32]) -> (usize, Vec<f32>) {
        let mut a = Analyzer::default();
        let (mut n, mut last) = (0, vec![]);
        a.push(input, |s| {
            n += 1;
            last = s.to_vec();
        });
        (n, last)
    }

    fn argmax(s: &[f32]) -> usize {
        (0..s.len()).max_by(|&a, &b| s[a].total_cmp(&s[b])).unwrap()
    }

    fn band_of(hz: f32) -> usize {
        ((hz / LOW_HZ).ln() / (HIGH_HZ / LOW_HZ).ln() * BANDS as f32) as usize
    }

    #[test]
    fn fft_matches_dft() {
        let x: Vec<f32> = (0..16).map(|i| ((i * 7) % 5) as f32 - 2.0).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; 16]);
        fft(&mut re, &mut im);
        for k in 0..16 {
            let (mut dr, mut di) = (0.0f32, 0.0f32);
            for (n, v) in x.iter().enumerate() {
                let a = -2.0 * std::f32::consts::PI * (k * n) as f32 / 16.0;
                dr += v * a.cos();
                di += v * a.sin();
            }
            assert!((re[k] - dr).abs() < 1e-3 && (im[k] - di).abs() < 1e-3, "bin {k}");
        }
    }

    #[test]
    fn rate_and_band_placement() {
        let (n, s) = last_spectrum(&tone(220.0, 3000.0, RATE));
        assert_eq!(n, 30, "30 spectra per second");
        assert_eq!(s.len(), 128);
        let (l, r) = (argmax(&s[..BANDS]), argmax(&s[BANDS..]));
        assert!(l.abs_diff(band_of(220.0)) <= 1, "left peak {l}");
        assert!(r.abs_diff(band_of(3000.0)) <= 1, "right peak {r}");
        assert!(s[l] > 0.8 && s[BANDS + r] > 0.8);
        // far from the tone: quiet
        assert!(s[BANDS - 1] < 0.3 && s[BANDS + 2] < 0.3);
    }

    #[test]
    fn silence_is_zero_and_decays() {
        let (_, s) = last_spectrum(&vec![0.0; RATE * 2]);
        assert!(s.iter().all(|v| *v == 0.0));
        let mut input = tone(500.0, 500.0, RATE / 2);
        input.extend(vec![0.0; RATE * 2]);
        let (_, s) = last_spectrum(&input);
        assert!(s.iter().all(|v| *v == 0.0), "decays to exactly zero");
    }

    #[test]
    fn bands_cover_increasing_bins() {
        let a = Analyzer::default();
        assert_eq!(a.bands.len(), BANDS);
        for w in a.bands.windows(2) {
            assert!(w[0].0 <= w[1].0 && w[0].1 <= w[1].1);
        }
        assert!(a.bands.last().unwrap().1 <= FFT_SIZE / 2);
    }

    #[test]
    fn js_literal() {
        assert_eq!(js_array(&[0.0, 0.5, 1.0]), "[0,0.500,1.000]");
    }
}
