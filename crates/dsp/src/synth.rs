use crate::bank::{Bank, ParamDef};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::f32::consts::TAU;
use std::sync::Arc;

pub const WAVES: [&str; 4] = ["Saw", "Square", "Triangle", "Sine"];

pub static SYNTH: [ParamDef; 21] = [
    ParamDef::new(0, "Osc 1 Wave", 0.0, 3.0, 0.0, "").choices(&WAVES),
    ParamDef::new(1, "Osc 2 Wave", 0.0, 3.0, 1.0, "").choices(&WAVES),
    ParamDef::new(2, "Osc 2 Semitones", -24.0, 24.0, 0.0, "st").steps(48),
    ParamDef::new(3, "Osc 2 Detune", -100.0, 100.0, 7.0, "ct"),
    ParamDef::new(4, "Osc Mix", 0.0, 100.0, 50.0, "%"),
    ParamDef::new(5, "Cutoff", 20.0, 20000.0, 2400.0, "Hz").log(),
    ParamDef::new(6, "Resonance", 0.0, 100.0, 20.0, "%"),
    ParamDef::new(7, "Filter Env Amount", -100.0, 100.0, 40.0, "%"),
    ParamDef::new(8, "Filter Attack", 0.5, 5000.0, 5.0, "ms").log(),
    ParamDef::new(9, "Filter Decay", 1.0, 5000.0, 300.0, "ms").log(),
    ParamDef::new(10, "Filter Sustain", 0.0, 100.0, 30.0, "%"),
    ParamDef::new(11, "Filter Release", 1.0, 5000.0, 300.0, "ms").log(),
    ParamDef::new(12, "Amp Attack", 0.5, 5000.0, 3.0, "ms").log(),
    ParamDef::new(13, "Amp Decay", 1.0, 5000.0, 200.0, "ms").log(),
    ParamDef::new(14, "Amp Sustain", 0.0, 100.0, 80.0, "%"),
    ParamDef::new(15, "Amp Release", 1.0, 5000.0, 250.0, "ms").log(),
    ParamDef::new(16, "LFO Rate", 0.1, 20.0, 5.0, "Hz").log(),
    ParamDef::new(17, "LFO to Pitch", 0.0, 100.0, 0.0, "ct"),
    ParamDef::new(18, "LFO to Cutoff", 0.0, 100.0, 0.0, "%"),
    ParamDef::new(19, "Glide", 0.0, 1000.0, 0.0, "ms"),
    ParamDef::new(20, "Volume", -40.0, 6.0, -6.0, "dB"),
];

const VOICES: usize = 16;

pub fn note_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// PolyBLEP correction, which takes the aliasing edge off saw and square steps.
fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

fn osc(wave: u32, phase: f32, dt: f32) -> f32 {
    match wave {
        0 => 2.0 * phase - 1.0 - blep(phase, dt),
        1 => {
            let sq = if phase < 0.5 { 1.0 } else { -1.0 };
            sq + blep(phase, dt) - blep((phase + 0.5) % 1.0, dt)
        }
        2 => 4.0 * (phase - 0.5).abs() - 1.0,
        _ => (phase * TAU).sin(),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy)]
struct Env {
    stage: Stage,
    level: f32,
}

impl Env {
    const IDLE: Env = Env { stage: Stage::Idle, level: 0.0 };

    fn tick(&mut self, a: f32, d: f32, s: f32, r: f32, sr: f32) -> f32 {
        let per = |ms: f32| 1.0 / (ms * 0.001 * sr).max(1.0);
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.level += per(a);
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.level -= (1.0 - s) * per(d);
                if self.level <= s {
                    self.level = s;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.level = s,
            Stage::Release => {
                self.level -= self.level.max(0.001) * per(r) * 5.0;
                if self.level <= 1e-4 {
                    *self = Env::IDLE;
                }
            }
        }
        self.level
    }
}

#[derive(Clone, Copy)]
struct Voice {
    note: u8,
    velocity: f32,
    pitch: f32,
    phases: [f32; 2],
    amp: Env,
    filter: Env,
    ic: [f32; 2],
    age: u64,
    held: bool,
}

impl Voice {
    const OFF: Voice =
        Voice { note: 0, velocity: 0.0, pitch: 0.0, phases: [0.0; 2], amp: Env::IDLE, filter: Env::IDLE, ic: [0.0; 2], age: 0, held: false };

    fn active(&self) -> bool {
        self.amp.stage != Stage::Idle
    }
}

/// A polyphonic subtractive synth: two oscillators into a resonant low-pass, with envelopes,
/// an LFO and glide.
pub struct Synth {
    bank: Arc<Bank>,
    sr: f32,
    voices: [Voice; VOICES],
    clock: u64,
    lfo: f32,
    bend: f32,
    modwheel: f32,
    sustain: bool,
    last_pitch: f32,
}

impl Synth {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Synth { bank, sr, voices: [Voice::OFF; VOICES], clock: 0, lfo: 0.0, bend: 0.0, modwheel: 0.0, sustain: false, last_pitch: 60.0 }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    fn note_on(&mut self, note: u8, vel: u8) {
        self.clock += 1;
        // A free voice, else the oldest one.
        let i = self
            .voices
            .iter()
            .position(|v| !v.active())
            .unwrap_or_else(|| (0..VOICES).min_by_key(|i| self.voices[*i].age).unwrap());
        let glide = self.bank.get(19) > 0.0;
        let start = if glide { self.last_pitch } else { note as f32 };
        self.voices[i] = Voice {
            note,
            velocity: vel as f32 / 127.0,
            pitch: start,
            phases: [0.0, 0.25],
            amp: Env { stage: Stage::Attack, level: 0.0 },
            filter: Env { stage: Stage::Attack, level: 0.0 },
            ic: [0.0; 2],
            age: self.clock,
            held: true,
        };
        self.last_pitch = note as f32;
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.note == note && v.held) {
            v.held = false;
            if !self.sustain {
                v.amp.stage = Stage::Release;
                v.filter.stage = Stage::Release;
            }
        }
    }

    fn event(&mut self, d: [u8; 3]) {
        match d[0] & 0xf0 {
            0x90 if d[2] > 0 => self.note_on(d[1], d[2]),
            0x80 | 0x90 => self.note_off(d[1]),
            0xB0 => match d[1] {
                1 => self.modwheel = d[2] as f32 / 127.0,
                64 => {
                    self.sustain = d[2] >= 64;
                    if !self.sustain {
                        for v in self.voices.iter_mut().filter(|v| !v.held && v.active()) {
                            v.amp.stage = Stage::Release;
                            v.filter.stage = Stage::Release;
                        }
                    }
                }
                120 | 123 => {
                    for v in self.voices.iter_mut() {
                        v.held = false;
                        v.amp.stage = Stage::Release;
                        v.filter.stage = Stage::Release;
                    }
                }
                _ => {}
            },
            0xE0 => self.bend = (((d[2] as i32) << 7 | d[1] as i32) - 8192) as f32 / 8192.0 * 2.0,
            _ => {}
        }
    }
}

impl Plugin for Synth {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], events: &[MidiEvent], _: &ProcessContext) {
        let b = self.bank.clone();
        let waves = [b.get(0) as u32, b.get(1) as u32];
        let osc2 = b.get(2) + b.get(3) / 100.0;
        let mix = b.get(4) / 100.0;
        let cutoff = b.get(5);
        let res = b.get(6) / 100.0;
        let env_amt = b.get(7) / 100.0;
        let (fa, fd, fs, fr) = (b.get(8), b.get(9), b.get(10) / 100.0, b.get(11));
        let (aa, ad, as_, ar) = (b.get(12), b.get(13), b.get(14) / 100.0, b.get(15));
        let lfo_rate = b.get(16);
        let lfo_pitch = b.get(17) / 100.0 * (1.0 + self.modwheel);
        let lfo_cut = b.get(18) / 100.0;
        let glide = b.get(19);
        let volume = 10f32.powf(b.get(20) / 20.0);
        let k = 2.0 - 1.9 * res;
        let glide_k = if glide > 0.0 { 1.0 - (-1.0 / (glide * 0.001 * self.sr)).exp() } else { 1.0 };
        let mut next = 0;
        for i in 0..l.len() {
            while next < events.len() && events[next].frame as usize <= i {
                self.event(events[next].data);
                next += 1;
            }
            self.lfo = (self.lfo + lfo_rate / self.sr) % 1.0;
            let lfo = (self.lfo * TAU).sin();
            let mut out = 0.0;
            for v in self.voices.iter_mut().filter(|v| v.active()) {
                v.pitch += (v.note as f32 - v.pitch) * glide_k;
                let base = v.pitch + self.bend + lfo * lfo_pitch;
                let mut s = 0.0;
                for (o, semis) in [(0usize, 0.0), (1, osc2)] {
                    let dt = note_hz(base + semis) / self.sr;
                    let w = osc(waves[o], v.phases[o], dt);
                    v.phases[o] = (v.phases[o] + dt) % 1.0;
                    s += w * if o == 0 { 1.0 - mix } else { mix };
                }
                let fenv = v.filter.tick(fa, fd, fs, fr, self.sr);
                let amp = v.amp.tick(aa, ad, as_, ar, self.sr);
                let fc = (cutoff * 2f32.powf(env_amt * fenv * 6.0 + lfo * lfo_cut * 2.0)).clamp(20.0, self.sr * 0.45);
                // Zavalishin's topology-preserving state-variable filter, low-pass output.
                let g = (std::f32::consts::PI * fc / self.sr).tan();
                let a1 = 1.0 / (1.0 + g * (g + k));
                let v3 = s - v.ic[1];
                let v1 = a1 * v.ic[0] + g * a1 * v3;
                let v2 = v.ic[1] + g * v1;
                v.ic[0] = 2.0 * v1 - v.ic[0];
                v.ic[1] = 2.0 * v2 - v.ic[1];
                out += v2 * amp * v.velocity;
            }
            let o = out * volume * 0.3;
            l[i] += o;
            r[i] += o;
        }
        while next < events.len() {
            self.event(events[next].data);
            next += 1;
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

    fn synth() -> Synth {
        let bank = Arc::new(Bank::new(&SYNTH));
        // A plain sine with an open filter, so the pitch is easy to measure.
        bank.set(0, 3.0);
        bank.set(4, 0.0);
        bank.set(5, 20000.0);
        bank.set(7, 0.0);
        Synth::new(bank, SR)
    }

    fn render(s: &mut Synth, events: &[MidiEvent], n: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        s.process(&mut l, &mut r, events, &CTX);
        l
    }

    #[test]
    fn plays_the_right_pitch() {
        let mut s = synth();
        let out = render(&mut s, &[MidiEvent { frame: 0, data: [0x90, 69, 100] }], 48000);
        let crossings = out[4800..].windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        // A4 is 440 Hz; 0.9 s of it has about 396 rising zero crossings.
        assert!((crossings as i32 - 396).abs() <= 2, "{crossings}");
    }

    #[test]
    fn releases_and_steals_voices() {
        let mut s = synth();
        let ons: Vec<MidiEvent> = (0..20).map(|i| MidiEvent { frame: 0, data: [0x90, 40 + i, 100] }).collect();
        render(&mut s, &ons, 256);
        assert_eq!(s.active_voices(), VOICES);
        let offs: Vec<MidiEvent> = (0..20).map(|i| MidiEvent { frame: 0, data: [0x80, 40 + i, 0] }).collect();
        render(&mut s, &offs, 48000);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn sustain_pedal_holds_notes() {
        let mut s = synth();
        let ev = [
            MidiEvent { frame: 0, data: [0xB0, 64, 127] },
            MidiEvent { frame: 1, data: [0x90, 60, 100] },
            MidiEvent { frame: 2, data: [0x80, 60, 0] },
        ];
        render(&mut s, &ev, 48000);
        assert_eq!(s.active_voices(), 1);
        render(&mut s, &[MidiEvent { frame: 0, data: [0xB0, 64, 0] }], 48000);
        assert_eq!(s.active_voices(), 0);
    }
}
