//! Loudness to ITU-R BS.1770-4 / EBU R128, loudness range to EBU Tech 3342, and true peak.

use std::f64::consts::PI;

const SUB_BLOCKS_MOMENTARY: usize = 4;
const SUB_BLOCKS_SHORT_TERM: usize = 30;

/// Histogram bins: 0.1 LU from the −70 LUFS absolute gate up to +5 LUFS (louder lands in the top bin).
const LOW: f64 = -70.0;
const STEP: f64 = 0.1;
const BINS: usize = 750;

const PHASES: usize = 4;
const TAPS: usize = 13;

pub struct Summary {
    pub integrated: f64,
    pub range: f64,
    pub true_peak: f64,
    pub sample_peak: f64,
}

pub fn measure(l: &[f32], r: &[f32], sample_rate: f64) -> Summary {
    let mut m = Meter::new(sample_rate);
    m.push(l, r);
    Summary { integrated: m.integrated(), range: m.range(), true_peak: m.true_peak(), sample_peak: m.sample_peak() }
}

fn loudness(energy: f64) -> f64 {
    if energy > 0.0 { -0.691 + 10.0 * energy.log10() } else { f64::NEG_INFINITY }
}

fn to_db(v: f64) -> f64 {
    if v > 0.0 { 20.0 * v.log10() } else { f64::NEG_INFINITY }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// The K-weighting pre-filter and RLB high-pass, derived from their analogue prototypes so that
/// they match the 48 kHz coefficients in BS.1770 at every sample rate.
fn k_weighting(sr: f64) -> [Biquad; 2] {
    let (f0, gain, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
    let k = (PI * f0 / sr).tan();
    let vh = 10f64.powf(gain / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b: [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / sr).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b: [1.0, -2.0, 1.0],
        a: [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        z: [0.0; 2],
    };
    [shelf, high_pass]
}

/// Block loudness values binned rather than kept in a growing list, so memory stays constant
/// however long the meter runs. Each bin also sums its blocks' energy, so gated means are exact
/// and only the gate threshold and percentiles are quantised to the 0.1 LU bin width.
struct Histogram {
    count: Vec<u64>,
    energy: Vec<f64>,
}

impl Histogram {
    fn new() -> Histogram {
        Histogram { count: vec![0; BINS], energy: vec![0.0; BINS] }
    }

    fn clear(&mut self) {
        self.count.fill(0);
        self.energy.fill(0.0);
    }

    fn add(&mut self, e: f64) {
        let l = loudness(e);
        if l <= LOW {
            return;
        }
        let i = (((l - LOW) / STEP) as usize).min(BINS - 1);
        self.count[i] += 1;
        self.energy[i] += e;
    }

    /// The first bin above the given relative gate, or None when nothing passes the absolute gate.
    fn gate(&self, relative: f64) -> Option<usize> {
        let n: u64 = self.count.iter().sum();
        if n == 0 {
            return None;
        }
        let threshold = loudness(self.energy.iter().sum::<f64>() / n as f64) + relative;
        Some(((threshold - LOW) / STEP - 0.5).ceil().clamp(0.0, BINS as f64) as usize)
    }

    fn integrated(&self) -> f64 {
        let Some(first) = self.gate(-10.0) else { return f64::NEG_INFINITY };
        let n: u64 = self.count[first..].iter().sum();
        if n == 0 {
            return f64::NEG_INFINITY;
        }
        loudness(self.energy[first..].iter().sum::<f64>() / n as f64)
    }

    fn range(&self) -> f64 {
        let Some(first) = self.gate(-20.0) else { return 0.0 };
        let counts = &self.count[first..];
        let n: u64 = counts.iter().sum();
        if n == 0 {
            return 0.0;
        }
        let percentile = |p: f64| {
            let k = ((n - 1) as f64 * p).round() as u64;
            let mut seen = 0;
            for (i, &c) in counts.iter().enumerate() {
                seen += c;
                if seen > k {
                    return LOW + (first + i) as f64 * STEP + STEP / 2.0;
                }
            }
            LOW + BINS as f64 * STEP
        };
        percentile(0.95) - percentile(0.10)
    }
}

/// A 4x oversampling interpolator for true peak: a 49-tap Kaiser-windowed sinc split into four
/// phases, the first of which passes the original samples through.
struct TruePeak {
    phases: [[f64; TAPS]; PHASES],
    history: [[f64; TAPS]; 2],
    peak: f64,
}

impl TruePeak {
    fn new() -> TruePeak {
        let len = PHASES * (TAPS - 1) + 1;
        let centre = (len - 1) as f64 / 2.0;
        let beta = 5.0;
        let mut phases = [[0.0; TAPS]; PHASES];
        for n in 0..len {
            let t = (n as f64 - centre) / PHASES as f64;
            let sinc = if t == 0.0 { 1.0 } else { (PI * t).sin() / (PI * t) };
            let x = 2.0 * n as f64 / (len - 1) as f64 - 1.0;
            let window = bessel_i0(beta * (1.0 - x * x).sqrt()) / bessel_i0(beta);
            phases[n % PHASES][n / PHASES] = sinc * window;
        }
        for p in &mut phases {
            let sum: f64 = p.iter().sum();
            p.iter_mut().for_each(|c| *c /= sum);
        }
        TruePeak { phases, history: [[0.0; TAPS]; 2], peak: 0.0 }
    }

    fn clear(&mut self) {
        self.history = [[0.0; TAPS]; 2];
        self.peak = 0.0;
    }

    fn push(&mut self, ch: usize, x: f64) {
        let h = &mut self.history[ch];
        h.copy_within(0..TAPS - 1, 1);
        h[0] = x;
        for p in &self.phases {
            let y: f64 = p.iter().zip(h.iter()).map(|(c, x)| c * x).sum();
            self.peak = self.peak.max(y.abs());
        }
    }
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..50 {
        term *= (x / (2.0 * k as f64)).powi(2);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

/// A streaming stereo loudness meter. Readouts update every 100 ms of audio pushed; pushing
/// never allocates.
pub struct Meter {
    filters: [[Biquad; 2]; 2],
    hop: usize,
    acc: f64,
    fill: usize,
    /// Mean square of the last 30 100 ms sub-blocks, a ring with `head` the next to be written.
    sub_blocks: [f64; SUB_BLOCKS_SHORT_TERM],
    head: usize,
    seen: usize,
    momentary: Histogram,
    short_term: Histogram,
    true_peak: TruePeak,
    sample_peak: f64,
}

impl Meter {
    pub fn new(sample_rate: f64) -> Meter {
        let k = k_weighting(sample_rate);
        Meter {
            filters: [k, k],
            hop: ((sample_rate * 0.1).round() as usize).max(1),
            acc: 0.0,
            fill: 0,
            sub_blocks: [0.0; SUB_BLOCKS_SHORT_TERM],
            head: 0,
            seen: 0,
            momentary: Histogram::new(),
            short_term: Histogram::new(),
            true_peak: TruePeak::new(),
            sample_peak: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for ch in &mut self.filters {
            ch.iter_mut().for_each(|f| f.z = [0.0; 2]);
        }
        self.acc = 0.0;
        self.fill = 0;
        self.sub_blocks = [0.0; SUB_BLOCKS_SHORT_TERM];
        self.head = 0;
        self.seen = 0;
        self.momentary.clear();
        self.short_term.clear();
        self.true_peak.clear();
        self.sample_peak = 0.0;
    }

    pub fn push(&mut self, l: &[f32], r: &[f32]) {
        for (&a, &b) in l.iter().zip(r) {
            let mut sum = 0.0;
            for (ch, x) in [a as f64, b as f64].into_iter().enumerate() {
                // A NaN or infinity would stay in the filter state for good, so it counts as silence.
                let x = if x.is_finite() { x } else { 0.0 };
                let [shelf, high_pass] = &mut self.filters[ch];
                let y = high_pass.run(shelf.run(x));
                sum += y * y;
                self.sample_peak = self.sample_peak.max(x.abs());
                self.true_peak.push(ch, x);
            }
            self.acc += sum;
            self.fill += 1;
            if self.fill == self.hop {
                self.end_sub_block();
            }
        }
    }

    fn end_sub_block(&mut self) {
        self.sub_blocks[self.head] = self.acc / self.hop as f64;
        self.head = (self.head + 1) % SUB_BLOCKS_SHORT_TERM;
        self.seen += 1;
        self.acc = 0.0;
        self.fill = 0;
        if self.seen >= SUB_BLOCKS_MOMENTARY {
            self.momentary.add(self.mean(SUB_BLOCKS_MOMENTARY));
        }
        if self.seen >= SUB_BLOCKS_SHORT_TERM {
            self.short_term.add(self.mean(SUB_BLOCKS_SHORT_TERM));
        }
    }

    fn mean(&self, n: usize) -> f64 {
        (1..=n).map(|i| self.sub_blocks[(self.head + SUB_BLOCKS_SHORT_TERM - i) % SUB_BLOCKS_SHORT_TERM]).sum::<f64>()
            / n as f64
    }

    /// LUFS over the last 400 ms.
    pub fn momentary(&self) -> f64 {
        loudness(self.mean(SUB_BLOCKS_MOMENTARY))
    }

    /// LUFS over the last 3 s.
    pub fn short_term(&self) -> f64 {
        loudness(self.mean(SUB_BLOCKS_SHORT_TERM))
    }

    /// Gated LUFS since the last reset; −∞ until a block passes the gates.
    pub fn integrated(&self) -> f64 {
        self.momentary.integrated()
    }

    /// Loudness range in LU.
    pub fn range(&self) -> f64 {
        self.short_term.range()
    }

    /// Maximum true peak in dBTP since the last reset.
    pub fn true_peak(&self) -> f64 {
        to_db(self.true_peak.peak.max(self.sample_peak))
    }

    /// Maximum sample peak in dBFS since the last reset.
    pub fn sample_peak(&self) -> f64 {
        to_db(self.sample_peak)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;

    fn sine(dbfs: f64, freq: f64, phase: f64, secs: f64, sr: f64) -> Vec<f32> {
        let a = 10f64.powf(dbfs / 20.0);
        (0..(secs * sr).round() as usize).map(|n| (a * (2.0 * PI * freq * n as f64 / sr + phase).sin()) as f32).collect()
    }

    fn sections(parts: &[(f64, f64)], sr: f64) -> Vec<f32> {
        parts.iter().flat_map(|&(db, secs)| sine(db, 1000.0, 0.0, secs, sr)).collect()
    }

    fn near(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} is not within {tol} of {b}");
    }

    #[test]
    fn steady_sine_reads_its_level() {
        for db in [-23.0, -33.0] {
            let s = sine(db, 1000.0, 0.0, 20.0, 48000.0);
            let mut m = Meter::new(48000.0);
            m.push(&s, &s);
            near(m.momentary(), db, 0.1);
            near(m.short_term(), db, 0.1);
            near(m.integrated(), db, 0.1);
        }
    }

    #[test]
    fn gating_ignores_silence_and_quiet_passages() {
        // EBU Tech 3341 case 4.
        let s = sections(&[(-72.0, 10.0), (-26.0, 10.0), (-20.0, 10.0), (-26.0, 10.0), (-72.0, 10.0)], 48000.0);
        near(measure(&s, &s, 48000.0).integrated, -23.0, 0.1);
        // Case 5.
        let s = sections(&[(-26.0, 20.0), (-20.0, 20.1), (-26.0, 20.0)], 48000.0);
        near(measure(&s, &s, 48000.0).integrated, -23.0, 0.1);
    }

    #[test]
    fn loudness_range_of_two_levels() {
        // EBU Tech 3342 case 1.
        let s = sections(&[(-20.0, 20.0), (-30.0, 20.0)], 48000.0);
        near(measure(&s, &s, 48000.0).range, 10.0, 1.0);
    }

    #[test]
    fn true_peak_finds_the_inter_sample_peak() {
        let sr = 48000.0;
        let s = sine(-3.0103, sr / 4.0, PI / 4.0, 1.0, sr);
        let m = measure(&s, &s, sr);
        near(m.sample_peak, -6.0206, 0.01);
        near(m.true_peak, -3.0, 0.2);
    }

    #[test]
    fn sample_rate_does_not_change_loudness() {
        let at = |freq: f64, sr: f64| {
            let s = sine(-23.0, freq, 0.0, 10.0, sr);
            measure(&s, &s, sr).integrated
        };
        for freq in [50.0, 1000.0, 10000.0] {
            let reference = at(freq, 48000.0);
            for sr in [44100.0, 96000.0] {
                near(at(freq, sr), reference, 0.1);
            }
        }
        near(at(1000.0, 44100.0), -23.0, 0.1);
    }

    #[test]
    fn silence_has_no_loudness() {
        let s = vec![0.0f32; 48000];
        let m = measure(&s, &s, 48000.0);
        assert_eq!(m.integrated, f64::NEG_INFINITY);
        assert_eq!(m.range, 0.0);
    }

    #[test]
    fn reset_starts_again() {
        let s = sine(-10.0, 1000.0, 0.0, 5.0, 48000.0);
        let mut m = Meter::new(48000.0);
        m.push(&s, &s);
        m.reset();
        let s = sine(-30.0, 1000.0, 0.0, 5.0, 48000.0);
        m.push(&s, &s);
        near(m.integrated(), -30.0, 0.1);
        near(m.sample_peak(), -30.0, 0.01);
    }

    #[test]
    fn a_non_finite_sample_does_not_stop_the_meter() {
        let mut s = sine(-23.0, 1000.0, 0.0, 10.0, 48000.0);
        s[1000] = f32::NAN;
        s[2000] = f32::INFINITY;
        let mut m = Meter::new(48000.0);
        m.push(&s, &s);
        near(m.short_term(), -23.0, 0.1);
        near(m.integrated(), -23.0, 0.1);
        assert!(m.true_peak() < -21.0, "{}", m.true_peak());
    }

    #[test]
    fn pushing_does_not_allocate() {
        let s = sine(-23.0, 1000.0, 0.0, 4.0, 48000.0);
        let mut m = Meter::new(48000.0);
        assert_no_alloc(|| {
            for (l, r) in s.chunks(256).zip(s.chunks(256)) {
                m.push(l, r);
            }
            m.reset();
            m.push(&s, &s);
        });
        near(m.integrated(), -23.0, 0.1);
    }
}
