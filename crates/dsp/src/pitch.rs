//! Pitch detection for the Tuner. YIN on a low-passed, decimated copy of the input, with every
//! buffer allocated up front.

use std::f64::consts::PI;

/// Just under E1, the lowest string of a drop-tuned eight-string guitar or a four-string bass.
pub const LOWEST: f32 = 40.0;
pub const HIGHEST: f32 = 1500.0;
/// The input is decimated to roughly this rate, which leaves room above `HIGHEST` and keeps the
/// difference function cheap.
const ANALYSIS_RATE: u32 = 16_000;
const CUTOFF: f64 = 3000.0;
const THRESHOLD: f32 = 0.15;
/// About −60 dBFS RMS; anything quieter has no note.
const GATE: f32 = 0.001;

pub const NOTE_NAMES: [&str; 12] = ["C", "C♯", "D", "D♯", "E", "F", "F♯", "G", "G♯", "A", "A♯", "B"];

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn lowpass(freq: f64, sr: f64) -> Biquad {
        let k = (PI * freq / sr).tan();
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let a0 = 1.0 + k / q + k * k;
        let b0 = k * k / a0;
        Biquad { b: [b0, 2.0 * b0, b0], a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0], z: [0.0; 2] }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

pub struct Detector {
    rate: f32,
    step: usize,
    phase: usize,
    lowpass: [Biquad; 2],
    /// The last `2 * tau_max` decimated samples, circular.
    history: Vec<f32>,
    pos: usize,
    filled: usize,
    frame: Vec<f32>,
    diff: Vec<f32>,
    norm: Vec<f32>,
    tau_min: usize,
    tau_max: usize,
}

impl Detector {
    pub fn new(sample_rate: u32) -> Detector {
        let step = (sample_rate as f32 / ANALYSIS_RATE as f32).round().max(1.0) as usize;
        let rate = sample_rate as f32 / step as f32;
        let tau_min = ((rate / HIGHEST).floor() as usize).max(2);
        let tau_max = (rate / LOWEST).ceil() as usize + 2;
        let cutoff = CUTOFF.min(rate as f64 * 0.4);
        let lp = Biquad::lowpass(cutoff, sample_rate as f64);
        Detector {
            rate,
            step,
            phase: 0,
            lowpass: [lp, lp],
            history: vec![0.0; 2 * tau_max],
            pos: 0,
            filled: 0,
            frame: vec![0.0; 2 * tau_max],
            diff: vec![0.0; tau_max + 1],
            norm: vec![0.0; tau_max + 1],
            tau_min,
            tau_max,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &x in samples {
            let x = if x.is_finite() { x as f64 } else { 0.0 };
            let [first, second] = &mut self.lowpass;
            let y = second.run(first.run(x));
            self.phase += 1;
            if self.phase == self.step {
                self.phase = 0;
                self.history[self.pos] = y as f32;
                self.pos = (self.pos + 1) % self.history.len();
                self.filled = (self.filled + 1).min(self.history.len());
            }
        }
    }

    pub fn clear(&mut self) {
        self.history.fill(0.0);
        self.filled = 0;
        self.lowpass.iter_mut().for_each(|b| b.z = [0.0; 2]);
    }

    /// The fundamental of what was pushed last, in Hz, or `None` for silence or no clear pitch.
    pub fn detect(&mut self) -> Option<f32> {
        let len = self.history.len();
        if self.filled < len {
            return None;
        }
        let (old, new) = self.history.split_at(self.pos);
        self.frame[..new.len()].copy_from_slice(new);
        self.frame[new.len()..].copy_from_slice(old);
        let x = &self.frame;
        let w = self.tau_max;
        let rms = (x.iter().map(|v| v * v).sum::<f32>() / len as f32).sqrt();
        if rms < GATE {
            return None;
        }
        // The difference function, and its cumulative mean normalised form to pick the lag from.
        self.norm[0] = 1.0;
        let mut sum = 0.0;
        for tau in 1..=self.tau_max {
            let d: f32 = (0..w).map(|j| (x[j] - x[j + tau]).powi(2)).sum();
            sum += d;
            self.diff[tau] = d;
            self.norm[tau] = if sum > 0.0 { d * tau as f32 / sum } else { 1.0 };
        }
        let n = &self.norm;
        let mut tau = (self.tau_min..self.tau_max).find(|&t| n[t] < THRESHOLD)?;
        while tau + 1 < self.tau_max && n[tau + 1] < n[tau] {
            tau += 1;
        }
        // The raw difference interpolates more truly than the normalised one.
        let d = &self.diff;
        let (a, b, c) = (d[tau - 1], d[tau], d[tau + 1]);
        let curve = a + c - 2.0 * b;
        let shift = if curve > 0.0 { (0.5 * (a - c) / curve).clamp(-0.5, 0.5) } else { 0.0 };
        let freq = self.rate / (tau as f32 + shift);
        (LOWEST * 0.97..=HIGHEST * 1.03).contains(&freq).then_some(freq)
    }
}

/// The nearest equal-tempered note to `freq` as a MIDI note number, and how far `freq` is from
/// it in cents.
pub fn nearest_note(freq: f32, a4: f32) -> (i32, f32) {
    let semis = 12.0 * (freq / a4).log2() + 69.0;
    let note = semis.round();
    (note as i32, (semis - note) * 100.0)
}

/// A MIDI note number as a name and octave, such as E1 or A4.
pub fn note_name(note: i32) -> String {
    format!("{}{}", NOTE_NAMES[note.rem_euclid(12) as usize], note.div_euclid(12) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    /// A plucked-string-like tone: the fundamental plus decaying harmonics.
    fn tone(freq: f64, harmonics: &[f64], secs: f64, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f64) as usize)
            .map(|n| {
                let t = n as f64 / sr as f64;
                let v: f64 = harmonics.iter().enumerate().map(|(k, a)| a * (2.0 * PI * freq * (k + 1) as f64 * t + k as f64).sin()).sum();
                (0.2 * v) as f32
            })
            .collect()
    }

    fn detect(signal: &[f32], sr: u32) -> Option<f32> {
        let mut d = Detector::new(sr);
        assert_no_alloc(|| {
            d.push(signal);
            d.detect()
        })
    }

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    #[test]
    fn finds_low_and_high_strings_with_harmonics() {
        let guitar = [1.0, 0.7, 0.5, 0.35, 0.25, 0.15];
        // Bass and low guitar notes whose second harmonic is stronger than the fundamental.
        let weak = [0.5, 1.0, 0.6, 0.4, 0.3, 0.2, 0.1];
        for sr in [44100, 48000, 96000] {
            for (freq, harmonics) in [(41.2034, &weak[..]), (61.7354, &weak), (82.4069, &guitar), (440.0, &guitar), (1318.51, &[1.0, 0.3][..])] {
                let got = detect(&tone(freq, harmonics, 0.5, sr), sr).unwrap_or_else(|| panic!("no pitch for {freq} Hz at {sr}"));
                let off = cents(got, freq as f32);
                assert!(off.abs() < 1.0, "{freq} Hz at {sr} read as {got} ({off:+.2} cents)");
            }
        }
    }

    #[test]
    fn silence_noise_and_near_silence_have_no_note() {
        assert_eq!(detect(&vec![0.0; 48000], 48000), None);
        assert_eq!(detect(&vec![1e-5; 48000], 48000), None);
        assert_eq!(detect(&tone(110.0, &[1.0], 0.5, 48000).iter().map(|x| x * 1e-3).collect::<Vec<_>>(), 48000), None);
        assert_eq!(detect(&tone(110.0, &[1.0], 0.01, 48000), 48000), None, "not enough heard yet");
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..48000)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                (seed >> 8) as f32 / (1 << 24) as f32 * 0.4 - 0.2
            })
            .collect();
        assert_eq!(detect(&noise, 48000), None, "noise");
    }

    #[test]
    fn names_notes_against_the_reference_pitch() {
        assert_eq!(nearest_note(440.0, 440.0), (69, 0.0));
        assert_eq!(note_name(69), "A4");
        let (n, c) = nearest_note(41.2034, 440.0);
        assert_eq!((note_name(n).as_str(), c.abs() < 0.01), ("E1", true));
        assert_eq!(note_name(nearest_note(30.8677, 440.0).0), "B0");
        let (n, c) = nearest_note(440.0, 432.0);
        assert_eq!(n, 69);
        assert!((c - 31.77).abs() < 0.05, "{c}");
        assert_eq!(note_name(nearest_note(466.16, 440.0).0), "A♯4");
    }
}
