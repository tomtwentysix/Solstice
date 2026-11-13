use crate::bank::{Bank, ParamDef};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::sync::Arc;

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

fn to_db(v: f32) -> f32 {
    20.0 * v.max(1e-9).log10()
}

/// One-pole smoothing coefficient for a time constant in milliseconds.
fn coef(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp()
}

pub static COMPRESSOR: [ParamDef; 6] = [
    ParamDef::new(0, "Threshold", -60.0, 0.0, -18.0, "dB"),
    ParamDef::new(1, "Ratio", 1.0, 20.0, 4.0, ":1"),
    ParamDef::new(2, "Attack", 0.1, 200.0, 10.0, "ms").log(),
    ParamDef::new(3, "Release", 5.0, 2000.0, 120.0, "ms").log(),
    ParamDef::new(4, "Knee", 0.0, 24.0, 6.0, "dB"),
    ParamDef::new(5, "Make-Up", 0.0, 30.0, 0.0, "dB"),
];

/// Stereo-linked feed-forward compressor with a soft knee.
/// A side-chain key copied in before each `process`: the louder channel per frame.
pub struct Key {
    level: Vec<f32>,
    len: usize,
}

impl Key {
    /// Long enough for any period the engine uses.
    pub fn new() -> Key {
        Key { level: vec![0.0; 8192], len: 0 }
    }

    pub fn set(&mut self, key: Option<[&[f32]; 2]>) {
        self.len = 0;
        if let Some([l, r]) = key {
            self.len = l.len().min(self.level.len());
            for (i, d) in self.level[..self.len].iter_mut().enumerate() {
                *d = l[i].abs().max(r[i].abs());
            }
        }
    }

    /// The detector input for frame `i`: the key if there is one, else the signal itself.
    pub fn at(&self, i: usize, own: f32) -> f32 {
        if i < self.len { self.level[i] } else { own }
    }
}

impl Default for Key {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Compressor {
    bank: Arc<Bank>,
    sr: f32,
    env: f32,
    key: Key,
}

impl Compressor {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Compressor { bank, sr, env: -120.0, key: Key::new() }
    }

    /// Gain change in dB for an input level, from the static curve.
    pub fn curve(level: f32, threshold: f32, ratio: f32, knee: f32) -> f32 {
        let over = level - threshold;
        let slope = 1.0 / ratio - 1.0;
        if knee > 0.0 && over.abs() <= knee / 2.0 {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }
}

impl Plugin for Compressor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let (th, ratio, knee, makeup) = (b.get(0), b.get(1), b.get(4), db(b.get(5)));
        let (att, rel) = (coef(b.get(2), self.sr), coef(b.get(3), self.sr));
        let mut most = 0f32;
        for (i, (a, c)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            let level = to_db(self.key.at(i, a.abs().max(c.abs())));
            let k = if level > self.env { att } else { rel };
            self.env = level + k * (self.env - level);
            let gr = Self::curve(self.env, th, ratio, knee);
            most = most.min(gr);
            let g = db(gr) * makeup;
            *a *= g;
            *c *= g;
        }
        b.set_meter(most);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }

    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        self.key.set(key);
    }
}

pub static LIMITER: [ParamDef; 3] = [
    ParamDef::new(0, "Gain", 0.0, 24.0, 0.0, "dB"),
    ParamDef::new(1, "Output Level", -12.0, 0.0, -0.3, "dB"),
    ParamDef::new(2, "Release", 1.0, 1000.0, 50.0, "ms").log(),
];

pub const LIMITER_LOOKAHEAD_MS: f32 = 1.5;

/// Look-ahead peak limiter: the signal is delayed by the look-ahead, and the gain follows the
/// loudest sample anywhere in that window, so it is already down when a peak comes out.
pub struct Limiter {
    bank: Arc<Bank>,
    sr: f32,
    delay: [Vec<f32>; 2],
    pos: usize,
    /// Sliding-window maximum: a ring of (sample number, level) with levels decreasing.
    window: Vec<(u64, f32)>,
    head: usize,
    len: usize,
    count: u64,
    gain: f32,
}

impl Limiter {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        let n = Self::latency_for(sr) as usize + 1;
        Limiter {
            bank,
            sr,
            delay: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            window: vec![(0, 0.0); n + 1],
            head: 0,
            len: 0,
            count: 0,
            gain: 1.0,
        }
    }

    /// Adds a level and returns the maximum of the last `span` levels.
    fn window_max(&mut self, level: f32, span: u64) -> f32 {
        let cap = self.window.len();
        while self.len > 0 && self.window[(self.head + self.len - 1) % cap].1 <= level {
            self.len -= 1;
        }
        self.window[(self.head + self.len) % cap] = (self.count, level);
        self.len += 1;
        while self.window[self.head].0 + span <= self.count {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.count += 1;
        self.window[self.head].1
    }

    pub fn latency_for(sr: f32) -> u32 {
        (LIMITER_LOOKAHEAD_MS * 0.001 * sr).round() as u32
    }
}

impl Plugin for Limiter {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = self.bank.clone();
        let (input, ceiling, rel) = (db(b.get(0)), db(b.get(1)), coef(b.get(2), self.sr));
        let n = self.delay[0].len();
        let mut most = 1f32;
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            let (x, y) = (*a * input, *c * input);
            // The window spans every sample still in the delay line, including the one
            // that comes out now.
            let held = self.window_max(x.abs().max(y.abs()), n as u64);
            let target = if held > ceiling { ceiling / held } else { 1.0 };
            // Instant attack keeps the guarantee; release is smoothed.
            self.gain = if target < self.gain { target } else { target + rel * (self.gain - target) };
            most = most.min(self.gain);
            let out = (self.pos + 1) % n;
            *a = self.delay[0][out] * self.gain;
            *c = self.delay[1][out] * self.gain;
            self.delay[0][self.pos] = x;
            self.delay[1][self.pos] = y;
            self.pos = out;
        }
        b.set_meter(to_db(most));
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

pub static GATE: [ParamDef; 5] = [
    ParamDef::new(0, "Threshold", -80.0, 0.0, -40.0, "dB"),
    ParamDef::new(1, "Reduction", -80.0, 0.0, -80.0, "dB"),
    ParamDef::new(2, "Attack", 0.05, 50.0, 1.0, "ms").log(),
    ParamDef::new(3, "Hold", 0.0, 500.0, 50.0, "ms"),
    ParamDef::new(4, "Release", 5.0, 2000.0, 150.0, "ms").log(),
];

pub struct Gate {
    bank: Arc<Bank>,
    sr: f32,
    gain: f32,
    hold: u32,
    key: Key,
}

impl Gate {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Gate { bank, sr, gain: 1.0, hold: 0, key: Key::new() }
    }
}

impl Plugin for Gate {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let (th, floor) = (db(b.get(0)), db(b.get(1)));
        let (att, rel) = (coef(b.get(2), self.sr), coef(b.get(4), self.sr));
        let hold = (b.get(3) * 0.001 * self.sr) as u32;
        for (i, (a, c)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            let open = self.key.at(i, a.abs().max(c.abs())) >= th;
            if open {
                self.hold = hold;
            } else if self.hold > 0 {
                self.hold -= 1;
            }
            let target = if open || self.hold > 0 { 1.0 } else { floor };
            let k = if target > self.gain { att } else { rel };
            self.gain = target + k * (self.gain - target);
            *a *= self.gain;
            *c *= self.gain;
        }
        b.set_meter(to_db(self.gain));
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }

    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        self.key.set(key);
    }
}

/// Note values for tempo-synced delay, in quarter notes.
pub const SYNC_NOTES: [(f32, &str); 8] =
    [(0.0, "Off"), (0.25, "1/16"), (0.375, "1/16 dotted"), (1.0 / 3.0, "1/8 triplet"), (0.5, "1/8"), (0.75, "1/8 dotted"), (1.0, "1/4"), (2.0, "1/2")];

const SYNC_NAMES: [&str; 8] = ["Off", "1/16", "1/16 dotted", "1/8 triplet", "1/8", "1/8 dotted", "1/4", "1/2"];

pub static DELAY: [ParamDef; 8] = [
    ParamDef::new(0, "Time", 1.0, 2000.0, 375.0, "ms").log(),
    ParamDef::new(1, "Sync", 0.0, 7.0, 0.0, "").choices(&SYNC_NAMES),
    ParamDef::new(2, "Feedback", 0.0, 95.0, 35.0, "%"),
    ParamDef::new(3, "Mix", 0.0, 100.0, 30.0, "%"),
    ParamDef::new(4, "Ping-Pong", 0.0, 1.0, 0.0, "").steps(1),
    ParamDef::new(5, "Low Cut", 20.0, 2000.0, 100.0, "Hz").log(),
    ParamDef::new(6, "High Cut", 1000.0, 20000.0, 8000.0, "Hz").log(),
    ParamDef::new(7, "Stereo Offset", 0.0, 50.0, 0.0, "ms"),
];

const MAX_DELAY_S: f32 = 4.0;

pub struct Delay {
    bank: Arc<Bank>,
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    lp: [f32; 2],
    hp: [f32; 2],
}

impl Delay {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        let n = (MAX_DELAY_S * sr) as usize;
        Delay { bank, sr, buf: [vec![0.0; n], vec![0.0; n]], pos: 0, lp: [0.0; 2], hp: [0.0; 2] }
    }

    /// Delay time in samples for the current settings and tempo.
    pub fn time(&self, tempo: f64) -> usize {
        let b = &self.bank;
        let sync = SYNC_NOTES[(b.get(1).round() as usize).min(SYNC_NOTES.len() - 1)].0;
        let secs = if sync > 0.0 && tempo > 0.0 { sync as f64 * 60.0 / tempo } else { b.get(0) as f64 / 1000.0 };
        ((secs * self.sr as f64) as usize).clamp(1, self.buf[0].len() - 1)
    }
}

impl Plugin for Delay {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], ctx: &ProcessContext) {
        let b = &self.bank;
        let fb = b.get(2) / 100.0;
        let mix = b.get(3) / 100.0;
        let ping = b.get(4) > 0.5;
        let lp = 1.0 - (-std::f32::consts::TAU * b.get(6) / self.sr).exp();
        let hp = 1.0 - (-std::f32::consts::TAU * b.get(5) / self.sr).exp();
        let base = self.time(ctx.tempo);
        let offset = (b.get(7) * 0.001 * self.sr) as usize;
        let n = self.buf[0].len();
        let times = [base, (base + offset).min(n - 1)];
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            let dry = [*a, *c];
            let mut wet = [0f32; 2];
            for ch in 0..2 {
                let read = (self.pos + n - times[ch]) % n;
                let mut w = self.buf[ch][read];
                // Feedback path: high cut (low-pass) then low cut (high-pass).
                self.lp[ch] += lp * (w - self.lp[ch]);
                self.hp[ch] += hp * (self.lp[ch] - self.hp[ch]);
                w = self.lp[ch] - self.hp[ch];
                wet[ch] = w;
            }
            let (in_l, in_r) = if ping { ((dry[0] + dry[1]) * 0.5, 0.0) } else { (dry[0], dry[1]) };
            // Ping-pong crosses the feedback between channels.
            let (fl, fr) = if ping { (wet[1], wet[0]) } else { (wet[0], wet[1]) };
            self.buf[0][self.pos] = in_l + fl * fb;
            self.buf[1][self.pos] = in_r + fr * fb;
            self.pos = (self.pos + 1) % n;
            *a = dry[0] * (1.0 - mix) + wet[0] * mix;
            *c = dry[1] * (1.0 - mix) + wet[1] * mix;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

pub static REVERB: [ParamDef; 5] = [
    ParamDef::new(0, "Size", 0.0, 100.0, 70.0, "%"),
    ParamDef::new(1, "Damping", 0.0, 100.0, 50.0, "%"),
    ParamDef::new(2, "Width", 0.0, 100.0, 100.0, "%"),
    ParamDef::new(3, "Mix", 0.0, 100.0, 25.0, "%"),
    ParamDef::new(4, "Pre-Delay", 0.0, 200.0, 20.0, "ms"),
];

// Schroeder–Moorer reverb: parallel damped combs into series allpasses, as in the public-domain
// Freeverb design. Lengths are for 44.1 kHz and scaled to the sample rate.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

struct Comb {
    buf: Vec<f32>,
    pos: usize,
    store: f32,
}

impl Comb {
    fn tick(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buf[self.pos];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buf[self.pos] = x + self.store * feedback;
        self.pos = (self.pos + 1) % self.buf.len();
        out
    }
}

struct Allpass {
    buf: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn tick(&mut self, x: f32) -> f32 {
        let b = self.buf[self.pos];
        self.buf[self.pos] = x + b * 0.5;
        self.pos = (self.pos + 1) % self.buf.len();
        b - x
    }
}

pub struct Reverb {
    bank: Arc<Bank>,
    sr: f32,
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    pre: [Vec<f32>; 2],
    pre_pos: usize,
}

impl Reverb {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        let scale = sr / 44100.0;
        let len = |n: usize, ch: usize| ((n + ch * SPREAD) as f32 * scale).max(1.0) as usize;
        let combs = [0, 1].map(|ch| COMBS.iter().map(|n| Comb { buf: vec![0.0; len(*n, ch)], pos: 0, store: 0.0 }).collect());
        let allpasses = [0, 1].map(|ch| ALLPASSES.iter().map(|n| Allpass { buf: vec![0.0; len(*n, ch)], pos: 0 }).collect());
        let pre = (0.2 * sr) as usize + 1;
        Reverb { bank, sr, combs, allpasses, pre: [vec![0.0; pre], vec![0.0; pre]], pre_pos: 0 }
    }
}

impl Plugin for Reverb {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let feedback = 0.7 + b.get(0) / 100.0 * 0.28;
        let damp = b.get(1) / 100.0 * 0.4;
        let width = b.get(2) / 100.0;
        let mix = b.get(3) / 100.0;
        let n = self.pre[0].len();
        let delay = ((b.get(4) * 0.001 * self.sr) as usize).min(n - 1);
        let (w1, w2) = (width / 2.0 + 0.5, (1.0 - width) / 2.0);
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            let dry = [*a, *c];
            self.pre[0][self.pre_pos] = dry[0];
            self.pre[1][self.pre_pos] = dry[1];
            let read = (self.pre_pos + n - delay) % n;
            self.pre_pos = (self.pre_pos + 1) % n;
            let input = (self.pre[0][read] + self.pre[1][read]) * 0.015;
            let mut wet = [0f32; 2];
            for ch in 0..2 {
                let mut acc = 0.0;
                for comb in &mut self.combs[ch] {
                    acc += comb.tick(input, feedback, damp);
                }
                for ap in &mut self.allpasses[ch] {
                    acc = ap.tick(acc);
                }
                wet[ch] = acc;
            }
            let (wl, wr) = (wet[0] * w1 + wet[1] * w2, wet[1] * w1 + wet[0] * w2);
            *a = dry[0] * (1.0 - mix) + wl * mix * 3.0;
            *c = dry[1] * (1.0 - mix) + wr * mix * 3.0;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

const SATURATION_MODES: [&str; 3] = ["Tape", "Tube", "Hard"];

pub static SATURATION: [ParamDef; 5] = [
    ParamDef::new(0, "Mode", 0.0, 2.0, 0.0, "").choices(&SATURATION_MODES),
    ParamDef::new(1, "Drive", 0.0, 36.0, 6.0, "dB"),
    ParamDef::new(2, "Tone", -6.0, 6.0, 0.0, "dB"),
    ParamDef::new(3, "Output", -24.0, 6.0, 0.0, "dB"),
    ParamDef::new(4, "Mix", 0.0, 100.0, 100.0, "%"),
];

// The 2x oversampler is a 47-tap linear-phase half-band FIR. Every other tap is zero apart from
// the centre (0.5), so only the 24 even-indexed taps are stored. Interpolation and decimation
// each delay by 11.5 samples, 23 in all.
const HALF_TAPS: usize = 24;
const HALF_CENTRE: usize = 23;

fn half_band() -> [f32; HALF_TAPS] {
    // Kaiser window, beta 7: about 70 dB of stopband.
    fn i0(x: f64) -> f64 {
        let (mut sum, mut term) = (1.0, 1.0);
        for k in 1..30 {
            term *= (x / (2.0 * k as f64)).powi(2);
            sum += term;
        }
        sum
    }
    let beta = 7.0;
    let c = HALF_CENTRE as f64;
    let mut taps = [0f32; HALF_TAPS];
    for (i, t) in taps.iter_mut().enumerate() {
        let k = 2.0 * i as f64 - c;
        let sinc = (std::f64::consts::PI * k / 2.0).sin() / (std::f64::consts::PI * k);
        let w = i0(beta * (1.0 - (k / c).powi(2)).max(0.0).sqrt()) / i0(beta);
        *t = (sinc * w) as f32;
    }
    let sum: f32 = taps.iter().sum();
    taps.map(|t| t * 0.5 / sum)
}

/// The last 32 samples of a signal.
#[derive(Clone, Copy, Default)]
struct History {
    buf: [f32; 32],
    pos: usize,
}

impl History {
    fn push(&mut self, x: f32) {
        self.pos = (self.pos + 1) & 31;
        self.buf[self.pos] = x;
    }

    fn ago(&self, k: usize) -> f32 {
        self.buf[self.pos.wrapping_sub(k) & 31]
    }

    fn fir(&self, taps: &[f32; HALF_TAPS]) -> f32 {
        taps.iter().enumerate().map(|(i, t)| t * self.ago(i)).sum()
    }
}

#[derive(Clone, Copy, Default)]
struct Oversampler {
    input: History,
    even: History,
    odd: History,
}

impl Oversampler {
    /// Runs `f` at twice the sample rate; the result is `HALF_CENTRE` samples late.
    fn run(&mut self, x: f32, taps: &[f32; HALF_TAPS], f: impl Fn(f32) -> f32) -> f32 {
        self.input.push(x);
        let even = 2.0 * self.input.fir(taps);
        let odd = self.input.ago(HALF_CENTRE / 2);
        self.even.push(f(even));
        self.odd.push(f(odd));
        self.even.fir(taps) + 0.5 * self.odd.ago(HALF_CENTRE / 2 + 1)
    }
}

/// Saturation with 2x oversampling around the curve, a tilt tone control and a dry mix.
/// The bank's meter carries the input peak as a linear level, for the interface's dot.
pub struct Saturation {
    bank: Arc<Bank>,
    sr: f32,
    taps: [f32; HALF_TAPS],
    os: [Oversampler; 2],
    dry: [History; 2],
    tape: [f32; 2],
    tilt: [f32; 2],
    dc: [(f32, f32); 2],
    peak: f32,
}

impl Saturation {
    pub const LATENCY: u32 = HALF_CENTRE as u32;

    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Saturation {
            bank,
            sr,
            taps: half_band(),
            os: Default::default(),
            dry: Default::default(),
            tape: [0.0; 2],
            tilt: [0.0; 2],
            dc: [(0.0, 0.0); 2],
            peak: 0.0,
        }
    }

    /// The curve for a mode (0 Tape, 1 Tube, 2 Hard), with unity gain for small signals.
    pub fn shape(mode: usize, x: f32) -> f32 {
        match mode {
            // Biased tanh: soft, and a little asymmetric for a touch of even harmonics.
            0 => (x + 0.1).tanh() - 0.1f32.tanh(),
            // Rounds off positive peaks earlier and harder than negative ones.
            1 => {
                if x >= 0.0 {
                    1.0 - (-x).exp()
                } else {
                    ((1.5 * x).exp() - 1.0) / 1.5
                }
            }
            _ => x.clamp(-1.0, 1.0),
        }
    }

    /// Output for a steady input sample, ignoring tone: what the interface draws.
    pub fn transfer(mode: usize, drive_db: f32, output_db: f32, mix: f32, x: f32) -> f32 {
        (Self::shape(mode, x * db(drive_db)) * mix + x * (1.0 - mix)) * db(output_db)
    }
}

impl Plugin for Saturation {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let mode = b.get(0).round() as usize;
        let (drive, tone, out, mix) = (db(b.get(1)), b.get(2), db(b.get(3)), b.get(4) / 100.0);
        let (low, high) = (db(-tone / 2.0), db(tone / 2.0));
        let pivot = 1.0 - (-std::f32::consts::TAU * 1000.0 / self.sr).exp();
        let roll_off = 1.0 - (-std::f32::consts::TAU * 14000.0 / self.sr).exp();
        let dc = 1.0 - std::f32::consts::TAU * 10.0 / self.sr;
        let fall = coef(300.0, self.sr);
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            self.peak = a.abs().max(c.abs()).max(self.peak * fall);
            for (ch, s) in [a, c].into_iter().enumerate() {
                let x = *s;
                let mut y = self.os[ch].run(x * drive, &self.taps, |u| Self::shape(mode, u));
                if mode == 0 {
                    self.tape[ch] += roll_off * (y - self.tape[ch]);
                    y = self.tape[ch];
                }
                // The asymmetric curves leave a DC offset.
                let (last_in, last_out) = self.dc[ch];
                let blocked = y - last_in + dc * last_out;
                self.dc[ch] = (y, blocked);
                self.tilt[ch] += pivot * (blocked - self.tilt[ch]);
                y = self.tilt[ch] * low + (blocked - self.tilt[ch]) * high;
                self.dry[ch].push(x);
                *s = (self.dry[ch].ago(HALF_CENTRE) * (1.0 - mix) + y * mix) * out;
            }
        }
        b.set_meter(self.peak);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

const DE_ESSER_MODES: [&str; 2] = ["Split", "Wide"];

pub static DE_ESSER: [ParamDef; 5] = [
    ParamDef::new(0, "Frequency", 2000.0, 12000.0, 6500.0, "Hz").log(),
    ParamDef::new(1, "Threshold", -60.0, 0.0, -24.0, "dB"),
    ParamDef::new(2, "Range", 0.0, 24.0, 6.0, "dB"),
    ParamDef::new(3, "Mode", 0.0, 1.0, 0.0, "").choices(&DE_ESSER_MODES),
    ParamDef::new(4, "Listen", 0.0, 1.0, 0.0, "").steps(1),
];

const DE_ESSER_Q: f32 = 1.5;

/// Band-pass with 0 dB at the centre, as (b0, a1, a2) normalised by a0; b1 is zero and b2 is -b0.
fn band_pass(centre: f32, sr: f32) -> (f32, f32, f32) {
    let w = std::f32::consts::TAU * centre.min(sr * 0.45) / sr;
    let alpha = w.sin() / (2.0 * DE_ESSER_Q);
    let a0 = 1.0 + alpha;
    (alpha / a0, -2.0 * w.cos() / a0, (1.0 - alpha) / a0)
}

/// Split-band de-esser: a band-pass around the sibilance drives a stereo-linked detector, and the
/// reduction is applied to that band alone (Split) or to the whole signal (Wide).
/// The bank's meter carries the gain reduction in dB.
pub struct DeEsser {
    bank: Arc<Bank>,
    sr: f32,
    band: [(f32, f32); 2],
    env: f32,
}

impl DeEsser {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        DeEsser { bank, sr, band: [(0.0, 0.0); 2], env: 0.0 }
    }

    /// The detection band's response at `freq`, as a complex number (analogue prototype).
    fn band_at(freq: f32, centre: f32) -> (f32, f32) {
        let w = freq / centre;
        let (re, im) = (1.0 - w * w, w / DE_ESSER_Q);
        let d = re * re + im * im;
        // (j·im) / (re + j·im)
        (im * im / d, im * re / d)
    }

    /// Linear gain of the detection band at `freq`.
    pub fn band_gain(freq: f32, centre: f32) -> f32 {
        let (re, im) = Self::band_at(freq, centre);
        re.hypot(im)
    }

    /// Linear gain at `freq` in Split mode while the band is turned down by `gain`.
    pub fn split_gain(freq: f32, centre: f32, gain: f32) -> f32 {
        let (re, im) = Self::band_at(freq, centre);
        (1.0 + (gain - 1.0) * re).hypot((gain - 1.0) * im)
    }
}

impl Plugin for DeEsser {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let (th, range) = (b.get(1), b.get(2));
        let (wide, listen) = (b.get(3) > 0.5, b.get(4) > 0.5);
        let (b0, a1, a2) = band_pass(b.get(0), self.sr);
        let (att, rel) = (coef(0.5, self.sr), coef(60.0, self.sr));
        let mut most = 0f32;
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            // Transposed direct form II, with b1 = 0 and b2 = -b0.
            let mut bp = [0f32; 2];
            for (ch, x) in [*a, *c].into_iter().enumerate() {
                let (s1, s2) = self.band[ch];
                let y = b0 * x + s1;
                self.band[ch] = (s2 - a1 * y, -b0 * x - a2 * y);
                bp[ch] = y;
            }
            let level = bp[0].abs().max(bp[1].abs());
            let k = if level > self.env { att } else { rel };
            self.env = level + k * (self.env - level);
            let gr = -(to_db(self.env) - th).clamp(0.0, range);
            most = most.min(gr);
            let g = db(gr);
            if listen {
                (*a, *c) = (bp[0], bp[1]);
            } else if wide {
                (*a, *c) = (*a * g, *c * g);
            } else {
                *a += (g - 1.0) * bp[0];
                *c += (g - 1.0) * bp[1];
            }
        }
        b.set_meter(most);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::{AllocDisabler, assert_no_alloc};

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    const SR: f32 = 48000.0;
    const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

    #[test]
    fn gate_opens_from_its_side_chain() {
        let bank = Arc::new(Bank::new(&GATE));
        bank.set(4, 20.0);
        let mut g = Gate::new(bank, SR);
        let quiet = vec![0.001f32; 48000];
        let shut = run(&mut g, &quiet);
        assert!(shut[40000].abs() < 1e-6);
        let loud = vec![1.0f32; 256];
        let (mut l, mut r) = (quiet[..256].to_vec(), quiet[..256].to_vec());
        for _ in 0..40 {
            g.sidechain(Some([&loud, &loud]));
            g.process(&mut l, &mut r, &[], &CTX);
            l.fill(0.001);
            r.fill(0.001);
        }
        g.sidechain(Some([&loud, &loud]));
        g.process(&mut l, &mut r, &[], &CTX);
        assert!((l[255] - 0.001).abs() < 0.0001, "the key holds the gate open");
    }

    fn run(p: &mut dyn Plugin, input: &[f32]) -> Vec<f32> {
        let (mut l, mut r) = (input.to_vec(), input.to_vec());
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            p.process(a, b, &[], &CTX);
        }
        l
    }

    fn sine(amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.05).sin() * amp).collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0f32, |m, v| m.max(v.abs()))
    }

    #[test]
    fn compressor_follows_ratio() {
        assert_eq!(Compressor::curve(-30.0, -20.0, 4.0, 0.0), 0.0);
        assert!((Compressor::curve(-8.0, -20.0, 4.0, 0.0) + 9.0).abs() < 1e-4);
        let bank = Arc::new(Bank::new(&COMPRESSOR));
        bank.set(4, 0.0);
        let mut c = Compressor::new(bank.clone(), SR);
        // A signal at a constant 0 dBFS, 18 dB over the threshold at 4:1, settles 13.5 dB down.
        let square: Vec<f32> = (0..48000).map(|i| if (i / 50) % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let out = run(&mut c, &square);
        let settled = to_db(peak(&out[40000..]));
        assert!((settled + 13.5).abs() < 0.6, "{settled}");
        assert!(bank.meter() < -13.0);
    }

    #[test]
    fn limiter_holds_the_ceiling() {
        let bank = Arc::new(Bank::new(&LIMITER));
        bank.set(0, 12.0);
        let mut lim = Limiter::new(bank, SR);
        let mut input: Vec<f32> = sine(0.9, 20000);
        input[5000] = 1.0;
        input[5001] = -1.0;
        let out = run(&mut lim, &input);
        assert!(peak(&out) <= db(-0.3) + 1e-6, "{}", peak(&out));
        assert!(peak(&out[2000..]) > db(-1.5));
        assert_eq!(Limiter::latency_for(SR), 72);
    }

    #[test]
    fn gate_closes_below_threshold() {
        let bank = Arc::new(Bank::new(&GATE));
        bank.set(4, 20.0);
        let mut g = Gate::new(bank, SR);
        let quiet = run(&mut g, &sine(0.001, 48000));
        assert!(peak(&quiet[30000..]) < 1e-6);
        let loud = run(&mut g, &sine(0.5, 4800));
        assert!(peak(&loud[2000..]) > 0.45);
    }

    #[test]
    fn delay_echoes_on_time() {
        let bank = Arc::new(Bank::new(&DELAY));
        bank.set(0, 100.0);
        bank.set(3, 100.0);
        bank.set(2, 0.0);
        bank.set(5, 20.0);
        bank.set(6, 20000.0);
        let mut d = Delay::new(bank.clone(), SR);
        let mut input = vec![0.0; 10000];
        input[0] = 1.0;
        let out = run(&mut d, &input);
        let at = out.iter().position(|v| v.abs() > 0.2).unwrap();
        assert_eq!(at, 4800);
        // Synced to a quarter note at 120 bpm: half a second.
        bank.set(1, 6.0);
        assert_eq!(d.time(120.0), 24000);
    }

    #[test]
    fn reverb_has_a_finite_tail() {
        let bank = Arc::new(Bank::new(&REVERB));
        let mut rv = Reverb::new(bank, SR);
        let mut input = vec![0.0; SR as usize * 6];
        input[0] = 1.0;
        let out = run(&mut rv, &input);
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(peak(&out[2000..20000]) > 1e-3);
        assert!(peak(&out[out.len() - 1000..]) < peak(&out[2000..20000]) * 0.05);
    }

    fn saturate(mode: f32, drive: f32, input: &[f32]) -> Vec<f32> {
        let bank = Arc::new(Bank::new(&SATURATION));
        bank.set(0, mode);
        bank.set(1, drive);
        run(&mut Saturation::new(bank, SR), input)
    }

    /// Level of the `k`th harmonic of a 750 Hz tone, in dB relative to the fundamental.
    fn harmonic(x: &[f32], k: usize) -> f32 {
        let at = |h: usize| {
            let w = std::f32::consts::TAU * 750.0 * h as f32 / SR;
            let (re, im) = x.iter().enumerate().fold((0f32, 0f32), |(re, im), (i, v)| (re + v * (w * i as f32).cos(), im + v * (w * i as f32).sin()));
            re.hypot(im)
        };
        to_db(at(k) / at(1))
    }

    #[test]
    fn saturation_harmonics_follow_the_mode() {
        let tone: Vec<f32> = (0..9600).map(|i| (std::f32::consts::TAU * 750.0 * i as f32 / SR).sin() * 0.5).collect();
        // 4800 samples after the start, a whole number of cycles.
        let tail = |out: Vec<f32>| out[4800..].to_vec();
        let tube = tail(saturate(1.0, 12.0, &tone));
        assert!(harmonic(&tube, 2) > -30.0, "tube 2nd {}", harmonic(&tube, 2));
        let hard = tail(saturate(2.0, 12.0, &tone));
        assert!(harmonic(&hard, 2) < -80.0, "hard 2nd {}", harmonic(&hard, 2));
        assert!(harmonic(&hard, 3) > -20.0, "hard 3rd {}", harmonic(&hard, 3));
        let tape = tail(saturate(0.0, 12.0, &tone));
        assert!(harmonic(&tape, 3) > harmonic(&tape, 2));
        assert!(harmonic(&tape, 2) > -60.0);
    }

    #[test]
    fn saturation_passes_silence() {
        for mode in [0.0, 1.0, 2.0] {
            assert!(saturate(mode, 36.0, &[0.0; 4096]).iter().all(|v| *v == 0.0));
        }
    }

    #[test]
    fn saturation_reports_its_latency() {
        let (native, mut p) = crate::create("solstice:saturation", SR as f64).unwrap();
        assert_eq!(native.latency, Saturation::LATENCY);
        native.bank.set(0, 2.0);
        native.bank.set(1, 0.0);
        native.bank.set(4, 50.0);
        // Below the clip point the curve is a straight line, so wet and dry line up, so the output is the input, delayed.
        let input: Vec<f32> = (0..4800).map(|i| (std::f32::consts::TAU * 2000.0 * i as f32 / SR).sin() * 0.01).collect();
        let out = run(p.as_mut(), &input);
        let n = Saturation::LATENCY as usize;
        let err = out[2400..].iter().zip(&input[2400 - n..]).fold(0f32, |m, (o, i)| m.max((o - i).abs()));
        assert!(err < 0.0002, "{err}");
    }

    #[test]
    fn saturation_processes_without_allocating() {
        let (native, mut p) = crate::create("solstice:saturation", SR as f64).unwrap();
        let (mut l, mut r) = (sine(0.8, 512), sine(0.8, 512));
        for mode in [0.0, 1.0, 2.0] {
            native.bank.set(0, mode);
            assert_no_alloc(|| p.process(&mut l, &mut r, &[], &CTX));
        }
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(native.bank.meter() > 0.5);
    }

    fn tone(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (std::f32::consts::TAU * freq * i as f32 / SR).sin() * amp).collect()
    }

    fn de_ess(wide: bool, input: &[f32]) -> (Vec<f32>, f32) {
        let bank = Arc::new(Bank::new(&DE_ESSER));
        bank.set(0, 7000.0);
        bank.set(1, -24.0);
        bank.set(2, 12.0);
        bank.set(3, if wide { 1.0 } else { 0.0 });
        let out = run(&mut DeEsser::new(bank.clone(), SR), input);
        (out, bank.meter())
    }

    #[test]
    fn de_esser_reduces_sibilance_only() {
        let mut input = vec![0.0; 2400];
        input.extend(tone(7000.0, 0.5, 9600));
        let (out, gr) = de_ess(false, &input);
        let reduced = to_db(peak(&out[7200..12000]) / 0.5);
        assert!(reduced < -10.0, "{reduced}");
        assert!(gr < -11.0, "{gr}");
        let (out, gr) = de_ess(true, &input);
        assert!(to_db(peak(&out[7200..12000]) / 0.5) < -10.0);
        assert!(gr < -11.0);

        let low = tone(500.0, 0.5, 9600);
        for wide in [false, true] {
            let (out, gr) = de_ess(wide, &low);
            assert_eq!(gr, 0.0);
            let err = out.iter().zip(&low).fold(0f32, |m, (o, i)| m.max((o - i).abs()));
            assert!(err < 1e-5, "{err}");
        }
    }

    #[test]
    fn de_esser_listens_to_the_band() {
        let bank = Arc::new(Bank::new(&DE_ESSER));
        bank.set(4, 1.0);
        let mut d = DeEsser::new(bank, SR);
        let at_centre = run(&mut d, &tone(6500.0, 0.5, 9600));
        assert!((to_db(peak(&at_centre[4800..]) / 0.5)).abs() < 0.2);
        let low = run(&mut DeEsser::new(Arc::new(Bank::new(&DE_ESSER)), SR), &tone(500.0, 0.5, 9600));
        assert!(to_db(peak(&low[4800..]) / 0.5) > -0.1, "Listen off passes the signal");
        assert!((to_db(DeEsser::band_gain(500.0, 6500.0)) + 26.0).abs() < 2.0);
        assert!((DeEsser::split_gain(6500.0, 6500.0, 0.5) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn de_esser_processes_without_allocating() {
        let (native, mut p) = crate::create("solstice:de-esser", SR as f64).unwrap();
        native.bank.set(1, -40.0);
        let (mut l, mut r) = (tone(7000.0, 0.8, 512), tone(7000.0, 0.8, 512));
        for (mode, listen) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)] {
            native.bank.set(3, mode);
            native.bank.set(4, listen);
            assert_no_alloc(|| p.process(&mut l, &mut r, &[], &CTX));
        }
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        assert!(native.bank.meter() < 0.0);
    }
}
