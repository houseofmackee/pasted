//! Short mechanical click sounds for copying and pasting. They are
//! synthesized when first played (a filtered noise burst plus a low thump),
//! so no sound files are shipped.

use std::cell::RefCell;
use std::collections::HashMap;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::NSSound;
use objc2_foundation::NSData;

const RATE: u32 = 44_100;
/// Peak level, about -8 dBFS: noticeable without being loud.
const PEAK: f64 = 0.4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sound {
    /// Something was copied: one crisp, high click.
    Copy,
    /// Pasted from Pasted: a lower press and release, like a key.
    Paste,
}

/// One click: when it starts, its pitch, how fast it fades and how loud it is.
struct Click {
    at_ms: f64,
    freq: f64,
    decay_ms: f64,
    gain: f64,
    /// Low "body" under the click.
    thump_hz: f64,
}

fn clicks(sound: Sound) -> (&'static [Click], f64) {
    match sound {
        Sound::Copy => (&[Click { at_ms: 2.0, freq: 2300.0, decay_ms: 5.0, gain: 1.0, thump_hz: 200.0 }], 45.0),
        Sound::Paste => (
            &[
                Click { at_ms: 2.0, freq: 1500.0, decay_ms: 7.0, gain: 1.0, thump_hz: 150.0 },
                Click { at_ms: 34.0, freq: 1900.0, decay_ms: 5.0, gain: 0.55, thump_hz: 180.0 },
            ],
            80.0,
        ),
    }
}

/// The samples for `sound`, in -1.0..=1.0, deterministic.
pub fn samples(sound: Sound) -> Vec<f64> {
    let (clicks, length_ms) = clicks(sound);
    let n = (RATE as f64 * length_ms / 1000.0) as usize;
    let mut out = vec![0.0; n];
    let mut seed: u32 = 0x2545_F491;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f64 / u32::MAX as f64 * 2.0 - 1.0
    };
    for c in clicks {
        let start = (RATE as f64 * c.at_ms / 1000.0) as usize;
        // Two-pole resonator (band-pass) around the click's pitch.
        let r = 0.94;
        let w = 2.0 * std::f64::consts::PI * c.freq / RATE as f64;
        let (a1, a2) = (2.0 * r * w.cos(), -r * r);
        let (mut y1, mut y2) = (0.0, 0.0);
        for (i, sample) in out.iter_mut().enumerate().skip(start) {
            let t = (i - start) as f64 / RATE as f64 * 1000.0;
            // 0.3 ms attack, then an exponential fade.
            let env = (t / 0.3).min(1.0) * (-t / c.decay_ms).exp();
            let y = noise() * env + a1 * y1 + a2 * y2;
            (y2, y1) = (y1, y);
            let thump = (2.0 * std::f64::consts::PI * c.thump_hz * t / 1000.0).sin() * (-t / 6.0).exp() * 0.35;
            *sample += c.gain * (y * 0.25 + thump * (t / 0.5).min(1.0));
        }
    }
    let peak = out.iter().fold(0.0f64, |m, s| m.max(s.abs())).max(f64::EPSILON);
    let len = out.len();
    for (i, s) in out.iter_mut().enumerate() {
        // Short fade at the end so the sound never stops with a pop.
        let tail = ((len - i) as f64 / (RATE as f64 * 0.004)).min(1.0);
        *s = *s / peak * PEAK * tail;
    }
    out
}

/// 16-bit mono WAV.
pub fn wav(samples: &[f64]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in samples {
        out.extend(((s.clamp(-1.0, 1.0) * i16::MAX as f64).round() as i16).to_le_bytes());
    }
    out
}

thread_local! {
    static CACHE: RefCell<HashMap<Sound, Option<Retained<NSSound>>>> = RefCell::new(HashMap::new());
}

/// Plays `sound`, restarting it if it is still playing (rapid copies).
pub fn play(sound: Sound) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entry = cache.entry(sound).or_insert_with(|| {
            let data = NSData::with_bytes(&wav(&samples(sound)));
            NSSound::initWithData(NSSound::alloc(), &data)
        });
        if let Some(s) = entry {
            s.stop();
            s.play();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_are_short_quiet_and_end_silently() {
        for sound in [Sound::Copy, Sound::Paste] {
            let s = samples(sound);
            let ms = s.len() as f64 / RATE as f64 * 1000.0;
            assert!((40.0..=100.0).contains(&ms), "{sound:?} lasts {ms} ms");
            let peak = s.iter().fold(0.0f64, |m, x| m.max(x.abs()));
            assert!((PEAK - 0.001..=PEAK + 0.001).contains(&peak), "{sound:?} peak {peak}");
            assert!(s[0].abs() < 0.01 && s.last().unwrap().abs() < 0.01, "{sound:?} starts and ends at silence");
            assert_eq!(s, samples(sound), "deterministic");
        }
        assert_ne!(samples(Sound::Copy).len(), samples(Sound::Paste).len(), "copy and paste sound different");
    }

    #[test]
    fn paste_has_a_second_softer_click() {
        let s = samples(Sound::Paste);
        let energy = |from_ms: f64, to_ms: f64| -> f64 {
            let (a, b) = ((from_ms * 44.1) as usize, (to_ms * 44.1) as usize);
            s[a..b].iter().map(|x| x * x).sum()
        };
        let (press, gap, release) = (energy(2.0, 12.0), energy(24.0, 32.0), energy(34.0, 44.0));
        assert!(release > gap * 4.0, "release click stands out from the gap");
        assert!(release < press, "release is softer than the press");
    }

    #[test]
    #[ignore = "writes the sounds to /tmp for listening (afplay /tmp/pasted-copy.wav)"]
    fn write_wavs() {
        std::fs::write("/tmp/pasted-copy.wav", wav(&samples(Sound::Copy))).unwrap();
        std::fs::write("/tmp/pasted-paste.wav", wav(&samples(Sound::Paste))).unwrap();
    }

    #[test]
    fn macos_loads_both_sounds() {
        let _appkit = crate::appkit_test_lock();
        for sound in [Sound::Copy, Sound::Paste] {
            let data = NSData::with_bytes(&wav(&samples(sound)));
            let loaded = NSSound::initWithData(NSSound::alloc(), &data);
            let duration = loaded.as_ref().map(|s| s.duration());
            assert!(duration.is_some_and(|d| d > 0.04 && d < 0.1), "{sound:?}: {duration:?}");
        }
    }

    #[test]
    fn wav_header_matches_the_data() {
        let bytes = wav(&[0.0, 0.5, -0.5, 1.0]);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), RATE);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(i16::from_le_bytes([bytes[50], bytes[51]]), i16::MAX);
        assert_eq!(bytes.len(), 44 + 8);
    }
}
