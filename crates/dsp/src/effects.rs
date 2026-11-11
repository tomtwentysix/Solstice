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
pub struct Compressor {
    bank: Arc<Bank>,
    sr: f32,
    env: f32,
}

impl Compressor {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Compressor { bank, sr, env: -120.0 }
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
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            let level = to_db(a.abs().max(c.abs()));
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
}

impl Gate {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Gate { bank, sr, gain: 1.0, hold: 0 }
    }
}

impl Plugin for Gate {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        let b = &self.bank;
        let (th, floor) = (db(b.get(0)), db(b.get(1)));
        let (att, rel) = (coef(b.get(2), self.sr), coef(b.get(4), self.sr));
        let hold = (b.get(3) * 0.001 * self.sr) as u32;
        for (a, c) in l.iter_mut().zip(r.iter_mut()) {
            let open = a.abs().max(c.abs()) >= th;
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
}

/// Note values for tempo-synced delay, in quarter notes.
pub const SYNC_NOTES: [(f32, &str); 8] =
    [(0.0, "Off"), (0.25, "1/16"), (0.375, "1/16 dotted"), (1.0 / 3.0, "1/8 triplet"), (0.5, "1/8"), (0.75, "1/8 dotted"), (1.0, "1/4"), (2.0, "1/2")];

pub static DELAY: [ParamDef; 8] = [
    ParamDef::new(0, "Time", 1.0, 2000.0, 375.0, "ms").log(),
    ParamDef::new(1, "Sync", 0.0, 7.0, 0.0, "").steps(7),
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

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;
    const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

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
}
