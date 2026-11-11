use solstice_model::{ChannelEq, EqBand, EqKind};
use std::f64::consts::PI;

/// Fixed state slots per band, so a band keeps its filter memory while others change:
/// cuts get four (up to 48 dB/oct), shelves and bells one each.
pub const SLOTS: usize = 14;

fn slot_range(band: usize) -> std::ops::Range<usize> {
    match band {
        0 => 0..4,
        1 => 4..5,
        2..=5 => band + 3..band + 4,
        6 => 9..10,
        _ => 10..14,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn norm(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Biquad {
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    // Coefficients from the RBJ Audio EQ Cookbook.
    fn peak(fs: f64, f: f64, q: f64, db: f64) -> Biquad {
        let (w, a) = (2.0 * PI * f / fs, 10f64.powf(db / 40.0));
        let (c, alpha) = (w.cos(), w.sin() / (2.0 * q));
        Biquad::norm(1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a)
    }

    fn shelf(fs: f64, f: f64, q: f64, db: f64, high: bool) -> Biquad {
        let (w, a) = (2.0 * PI * f / fs, 10f64.powf(db / 40.0));
        let (c, alpha) = (w.cos(), w.sin() / (2.0 * q));
        let k = 2.0 * a.sqrt() * alpha;
        if high {
            Biquad::norm(
                a * ((a + 1.0) + (a - 1.0) * c + k),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - k),
                (a + 1.0) - (a - 1.0) * c + k,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - k,
            )
        } else {
            Biquad::norm(
                a * ((a + 1.0) - (a - 1.0) * c + k),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - k),
                (a + 1.0) + (a - 1.0) * c + k,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - k,
            )
        }
    }

    fn pass(fs: f64, f: f64, q: f64, high: bool) -> Biquad {
        let w = 2.0 * PI * f / fs;
        let (c, alpha) = (w.cos(), w.sin() / (2.0 * q));
        let b = if high { (1.0 + c) / 2.0 } else { (1.0 - c) / 2.0 };
        let b1 = if high { -(1.0 + c) } else { 1.0 - c };
        Biquad::norm(b, b1, b, 1.0 + alpha, -2.0 * c, 1.0 - alpha)
    }

    fn first_order(fs: f64, f: f64, high: bool) -> Biquad {
        let k = (PI * f / fs).tan();
        let a1 = (k - 1.0) / (k + 1.0);
        if high {
            let b0 = 1.0 / (1.0 + k);
            Biquad { b0, b1: -b0, b2: 0.0, a1, a2: 0.0 }
        } else {
            let b0 = k / (1.0 + k);
            Biquad { b0, b1: b0, b2: 0.0, a1, a2: 0.0 }
        }
    }

    /// |H|² at angular frequency `w` (radians per sample).
    fn mag2(&self, w: f64) -> f64 {
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        (nr * nr + ni * ni) / (dr * dr + di * di)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EqCoeffs {
    pub slots: [Option<Biquad>; SLOTS],
}

/// A cut as a Butterworth cascade: a first-order section for odd orders, then biquads.
fn cut(fs: f64, f: f64, slope: u8, high_pass: bool, out: &mut [Option<Biquad>]) {
    let n = (slope / 6).clamp(1, 8) as usize;
    let mut i = 0;
    if n % 2 == 1 {
        out[i] = Some(Biquad::first_order(fs, f, high_pass));
        i += 1;
    }
    for k in 1..=n / 2 {
        let theta = if n % 2 == 0 { (2 * k - 1) as f64 * PI / (2 * n) as f64 } else { k as f64 * PI / n as f64 };
        out[i] = Some(Biquad::pass(fs, f, 1.0 / (2.0 * theta.cos()), high_pass));
        i += 1;
    }
}

pub fn band_sections(band: usize, b: &EqBand, fs: f64) -> [Option<Biquad>; 4] {
    let mut out = [None; 4];
    if !b.on {
        return out;
    }
    let f = (b.freq as f64).clamp(10.0, fs * 0.49);
    let q = (b.q as f64).clamp(0.1, 100.0);
    let g = b.gain_db as f64;
    match EqKind::of(band) {
        EqKind::LowCut => cut(fs, f, b.slope, true, &mut out),
        EqKind::HighCut => cut(fs, f, b.slope, false, &mut out),
        EqKind::LowShelf => out[0] = Some(Biquad::shelf(fs, f, q, g, false)),
        EqKind::HighShelf => out[0] = Some(Biquad::shelf(fs, f, q, g, true)),
        EqKind::Bell => out[0] = Some(Biquad::peak(fs, f, q, g)),
    }
    out
}

pub fn design(eq: &ChannelEq, fs: f64) -> EqCoeffs {
    let mut c = EqCoeffs::default();
    for (i, b) in eq.bands.iter().enumerate() {
        let secs = band_sections(i, b, fs);
        for (slot, s) in slot_range(i).zip(secs) {
            c.slots[slot] = s;
        }
    }
    c
}

pub fn sections_db(sections: &[Option<Biquad>], freq: f64, fs: f64) -> f64 {
    let w = 2.0 * PI * freq / fs;
    sections.iter().flatten().map(|s| 10.0 * s.mag2(w).max(1e-30).log10()).sum()
}

/// Response of the whole EQ in dB at `freq`, for drawing curves.
pub fn response_db(c: &EqCoeffs, freq: f64, fs: f64) -> f64 {
    sections_db(&c.slots, freq, fs)
}

pub fn band_response_db(band: usize, b: &EqBand, freq: f64, fs: f64) -> f64 {
    sections_db(&band_sections(band, b, fs), freq, fs)
}

/// Filter memory, transposed direct form II, per slot and channel.
#[derive(Clone, Copy, Default)]
pub struct EqState {
    z: [[[f64; 2]; SLOTS]; 2],
}

impl EqState {
    pub fn process(&mut self, c: &EqCoeffs, l: &mut [f32], r: &mut [f32]) {
        for (ch, buf) in [l, r].into_iter().enumerate() {
            for (slot, bq) in c.slots.iter().enumerate() {
                let Some(q) = bq else { continue };
                let z = &mut self.z[ch][slot];
                for x in buf.iter_mut() {
                    let input = *x as f64;
                    let y = q.b0 * input + z[0];
                    z[0] = q.b1 * input - q.a1 * y + z[1];
                    z[1] = q.b2 * input - q.a2 * y;
                    *x = y as f32;
                }
                // Keep decaying tails out of the denormal range.
                for v in z.iter_mut() {
                    if v.abs() < 1e-25 {
                        *v = 0.0;
                    }
                }
            }
        }
    }
}

pub fn is_flat(eq: &ChannelEq) -> bool {
    eq.bands.iter().enumerate().all(|(i, b)| !b.on || (!EqKind::of(i).is_cut() && b.gain_db == 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48000.0;

    fn eq_with(band: usize, f: impl FnOnce(&mut EqBand)) -> ChannelEq {
        let mut eq = ChannelEq::default();
        f(&mut eq.bands[band]);
        eq
    }

    #[test]
    fn default_is_flat() {
        let c = design(&ChannelEq::default(), FS);
        for f in [20.0, 100.0, 1000.0, 10000.0, 20000.0] {
            assert!(response_db(&c, f, FS).abs() < 1e-6, "{f}");
        }
    }

    #[test]
    fn bell_and_shelves_hit_their_gain() {
        let c = design(&eq_with(3, |b| b.gain_db = 9.0), FS);
        assert!((response_db(&c, 500.0, FS) - 9.0).abs() < 0.01);
        assert!(response_db(&c, 50.0, FS).abs() < 0.1);
        let c = design(&eq_with(1, |b| b.gain_db = -6.0), FS);
        assert!((response_db(&c, 20.0, FS) + 6.0).abs() < 0.2);
        let c = design(&eq_with(6, |b| b.gain_db = 4.0), FS);
        assert!((response_db(&c, 20000.0, FS) - 4.0).abs() < 0.3);
    }

    #[test]
    fn cuts_are_butterworth() {
        for slope in solstice_model::EQ_SLOPES {
            let c = design(
                &eq_with(0, |b| {
                    b.on = true;
                    b.freq = 200.0;
                    b.slope = slope;
                }),
                FS,
            );
            assert!((response_db(&c, 200.0, FS) + 3.01).abs() < 0.05, "slope {slope}");
            let octave_down = response_db(&c, 100.0, FS) - response_db(&c, 200.0, FS);
            assert!(octave_down < -(slope as f64) * 0.6, "slope {slope}: {octave_down}");
            assert!(response_db(&c, 10000.0, FS).abs() < 0.05);
        }
    }

    #[test]
    fn processing_matches_response() {
        let c = design(&eq_with(4, |b| b.gain_db = 12.0), FS);
        let mut st = EqState::default();
        let n = 48000;
        let mut l: Vec<f32> = (0..n).map(|i| (2.0 * PI * 1500.0 * i as f64 / FS).sin() as f32 * 0.1).collect();
        let mut r = l.clone();
        st.process(&c, &mut l, &mut r);
        let peak = l[n / 2..].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((20.0 * (peak / 0.1).log10() - 12.0).abs() < 0.1, "{peak}");
    }
}
