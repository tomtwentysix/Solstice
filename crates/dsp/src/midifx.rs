//! MIDI effects for the MIDI FX slots.

use crate::bank::{Bank, ParamDef};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::sync::Arc;

fn push(out: &mut Vec<MidiEvent>, frame: u32, data: [u8; 3]) {
    if out.len() < out.capacity() {
        out.push(MidiEvent { frame, data });
    }
}

fn is_on(d: [u8; 3]) -> bool {
    d[0] & 0xf0 == 0x90 && d[2] > 0
}

fn is_off(d: [u8; 3]) -> bool {
    d[0] & 0xf0 == 0x80 || (d[0] & 0xf0 == 0x90 && d[2] == 0)
}

/// Stable insertion sort by frame; outputs are few and nearly sorted.
fn sort(ev: &mut [MidiEvent]) {
    for j in 1..ev.len() {
        let mut k = j;
        while k > 0 && ev[k - 1].frame > ev[k].frame {
            ev.swap(k - 1, k);
            k -= 1;
        }
    }
}

const RATES: [&str; 7] = ["1/4", "1/8", "1/8 triplet", "1/16", "1/16 triplet", "1/32", "1/2"];
const RATE_BEATS: [f64; 7] = [1.0, 0.5, 1.0 / 3.0, 0.25, 1.0 / 6.0, 0.125, 2.0];

fn rate_samples(index: f32, tempo: f64, sr: f32) -> f64 {
    let beats = RATE_BEATS[(index.round() as usize).min(RATE_BEATS.len() - 1)];
    (beats * 60.0 / tempo.max(1.0) * sr as f64).max(1.0)
}

pub static TRANSPOSER: [ParamDef; 1] = [ParamDef::new(0, "Semitones", -36.0, 36.0, 0.0, "st").steps(72)];

/// Shifts notes by a fixed interval. Note-offs follow the pitch their note-on was sent at, so
/// changing the interval while keys are held leaves nothing hanging.
pub struct Transposer {
    bank: Arc<Bank>,
    sounding: [u8; 128],
}

impl Transposer {
    pub fn new(bank: Arc<Bank>) -> Self {
        Transposer { bank, sounding: [0; 128] }
    }
}

impl Plugin for Transposer {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _: usize, _: &ProcessContext) {
        let shift = self.bank.get(0).round() as i32;
        for e in input {
            let mut d = e.data;
            if is_on(d) {
                let Ok(n) = u8::try_from(d[1] as i32 + shift) else { continue };
                if n > 127 {
                    continue;
                }
                self.sounding[d[1] as usize & 127] = n;
                d[1] = n;
            } else if is_off(d) {
                d[1] = self.sounding[d[1] as usize & 127];
            }
            push(out, e.frame, d);
        }
    }
}

pub static VELOCITY: [ParamDef; 4] = [
    ParamDef::new(0, "Scale", 0.0, 200.0, 100.0, "%"),
    ParamDef::new(1, "Offset", -127.0, 127.0, 0.0, "").steps(254),
    ParamDef::new(2, "Minimum", 1.0, 127.0, 1.0, "").steps(126),
    ParamDef::new(3, "Maximum", 1.0, 127.0, 127.0, "").steps(126),
];

pub struct VelocityProcessor {
    bank: Arc<Bank>,
}

impl VelocityProcessor {
    pub fn new(bank: Arc<Bank>) -> Self {
        VelocityProcessor { bank }
    }

    pub fn map(&self, v: u8) -> u8 {
        let b = &self.bank;
        let (lo, hi) = (b.get(2), b.get(3).max(b.get(2)));
        (v as f32 * b.get(0) / 100.0 + b.get(1)).round().clamp(lo, hi).clamp(1.0, 127.0) as u8
    }
}

impl Plugin for VelocityProcessor {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _: usize, _: &ProcessContext) {
        for e in input {
            let mut d = e.data;
            if is_on(d) {
                d[2] = self.map(d[2]);
            }
            push(out, e.frame, d);
        }
    }
}

const SCALE_NAMES: [&str; 9] = [
    "Major",
    "Natural Minor",
    "Harmonic Minor",
    "Dorian",
    "Mixolydian",
    "Lydian",
    "Major Pentatonic",
    "Minor Pentatonic",
    "Blues",
];
/// Each scale as a bit set of the semitones above the root that belong to it.
pub const SCALES: [u16; 9] = [
    0b1010_1011_0101,
    0b0101_1010_1101,
    0b1001_1010_1101,
    0b0110_1010_1101,
    0b0110_1011_0101,
    0b1010_1101_0101,
    0b0010_1001_0101,
    0b0100_1010_1001,
    0b0100_1110_1001,
];
const ROOTS: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];

pub static SCALE_QUANTIZE: [ParamDef; 2] = [
    ParamDef::new(0, "Root", 0.0, 11.0, 0.0, "").choices(&ROOTS),
    ParamDef::new(1, "Scale", 0.0, 8.0, 0.0, "").choices(&SCALE_NAMES),
];

/// The nearest note in the scale, preferring the one below on a tie.
pub fn snap_to_scale(note: u8, root: u8, scale: u16) -> u8 {
    let in_scale = |n: i32| scale >> ((n - root as i32).rem_euclid(12)) & 1 == 1;
    for d in 0..12 {
        for n in [note as i32 - d, note as i32 + d] {
            if (0..=127).contains(&n) && in_scale(n) {
                return n as u8;
            }
        }
    }
    note
}

pub struct ScaleQuantize {
    bank: Arc<Bank>,
    sounding: [u8; 128],
}

impl ScaleQuantize {
    pub fn new(bank: Arc<Bank>) -> Self {
        ScaleQuantize { bank, sounding: [0; 128] }
    }
}

impl Plugin for ScaleQuantize {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _: usize, _: &ProcessContext) {
        let root = self.bank.get(0).round() as u8 % 12;
        let scale = SCALES[(self.bank.get(1).round() as usize).min(SCALES.len() - 1)];
        for e in input {
            let mut d = e.data;
            if is_on(d) {
                let n = snap_to_scale(d[1], root, scale);
                self.sounding[d[1] as usize & 127] = n;
                d[1] = n;
            } else if is_off(d) {
                d[1] = self.sounding[d[1] as usize & 127];
            }
            push(out, e.frame, d);
        }
    }
}

const CHORD_NAMES: [&str; 10] =
    ["Major", "Minor", "Diminished", "Augmented", "Sus 2", "Sus 4", "Major 7th", "Minor 7th", "Dominant 7th", "Power"];
const CHORDS: [&[u8]; 10] =
    [&[0, 4, 7], &[0, 3, 7], &[0, 3, 6], &[0, 4, 8], &[0, 2, 7], &[0, 5, 7], &[0, 4, 7, 11], &[0, 3, 7, 10], &[0, 4, 7, 10], &[0, 7, 12]];

pub static CHORD_TRIGGER: [ParamDef; 2] = [
    ParamDef::new(0, "Chord", 0.0, 9.0, 0.0, "").choices(&CHORD_NAMES),
    ParamDef::new(1, "Inversion", 0.0, 3.0, 0.0, "").steps(3),
];

/// The notes of a chord on `root`, with the lowest `inversion` notes raised an octave.
pub fn chord_notes(root: u8, chord: usize, inversion: usize) -> impl Iterator<Item = u8> {
    let iv = CHORDS[chord.min(CHORDS.len() - 1)];
    let inv = inversion.min(iv.len() - 1);
    iv.iter().enumerate().filter_map(move |(i, &x)| {
        let n = root as u32 + x as u32 + if i < inv { 12 } else { 0 };
        (n <= 127).then_some(n as u8)
    })
}

/// Plays a chord for every key. Each key's note-off ends the chord it started, whatever the
/// settings are by then.
pub struct ChordTrigger {
    bank: Arc<Bank>,
    started: [(u8, u8); 128],
}

impl ChordTrigger {
    pub fn new(bank: Arc<Bank>) -> Self {
        ChordTrigger { bank, started: [(0, 0); 128] }
    }
}

impl Plugin for ChordTrigger {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _: usize, _: &ProcessContext) {
        let chord = self.bank.get(0).round() as u8;
        let inversion = self.bank.get(1).round() as u8;
        for e in input {
            let d = e.data;
            let key = d[1] as usize & 127;
            if is_on(d) {
                self.started[key] = (chord, inversion);
                for n in chord_notes(d[1], chord as usize, inversion as usize) {
                    push(out, e.frame, [d[0], n, d[2]]);
                }
            } else if is_off(d) {
                let (c, i) = self.started[key];
                for n in chord_notes(d[1], c as usize, i as usize) {
                    push(out, e.frame, [d[0], n, d[2]]);
                }
            } else {
                push(out, e.frame, d);
            }
        }
    }
}

const ARP_MODES: [&str; 5] = ["Up", "Down", "Up and Down", "As Played", "Random"];

pub static ARPEGGIATOR: [ParamDef; 5] = [
    ParamDef::new(0, "Rate", 0.0, 6.0, 3.0, "").choices(&RATES),
    ParamDef::new(1, "Mode", 0.0, 4.0, 0.0, "").choices(&ARP_MODES),
    ParamDef::new(2, "Octaves", 1.0, 4.0, 1.0, "").steps(3),
    ParamDef::new(3, "Gate", 5.0, 100.0, 50.0, "%"),
    ParamDef::new(4, "Latch", 0.0, 1.0, 0.0, "").steps(1),
];

const MAX_HELD: usize = 32;

/// Steps through the held keys at a note rate, locked to the bar grid while the transport
/// runs and free-running from the first key while it is stopped.
pub struct Arpeggiator {
    bank: Arc<Bank>,
    sr: f32,
    /// Held keys in the order played.
    held: [(u8, u8); MAX_HELD],
    count: usize,
    /// Keys physically down. With Latch on, `held` keeps the last chord after they go up.
    down: u32,
    step: usize,
    /// Frames from this block's start until the next step, and until the gate closes.
    next: f64,
    gate_end: Option<f64>,
    sounding: Option<u8>,
    rng: u32,
}

impl Arpeggiator {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        Arpeggiator {
            bank,
            sr,
            held: [(0, 0); MAX_HELD],
            count: 0,
            down: 0,
            step: 0,
            next: 0.0,
            gate_end: None,
            sounding: None,
            rng: 0x9e37_79b9,
        }
    }

    fn pick(&mut self) -> Option<(u8, u8)> {
        if self.count == 0 {
            return None;
        }
        let octaves = self.bank.get(2).round().clamp(1.0, 4.0) as usize;
        let mut sorted = [(0u8, 0u8); MAX_HELD];
        sorted[..self.count].copy_from_slice(&self.held[..self.count]);
        let mode = self.bank.get(1).round() as usize;
        if mode != 3 {
            sorted[..self.count].sort_unstable_by_key(|n| n.0);
        }
        let len = self.count * octaves;
        let i = match mode {
            1 => len - 1 - self.step % len,
            2 if len > 1 => {
                let p = self.step % (2 * len - 2);
                if p < len { p } else { 2 * len - 2 - p }
            }
            4 => {
                self.rng ^= self.rng << 13;
                self.rng ^= self.rng >> 17;
                self.rng ^= self.rng << 5;
                self.rng as usize % len
            }
            _ => self.step % len,
        };
        let (note, vel) = sorted[i % self.count];
        let n = note as usize + 12 * (i / self.count);
        (n <= 127).then_some((n as u8, vel))
    }

    fn fire(&mut self, frame: u32, step_len: f64, out: &mut Vec<MidiEvent>) {
        if let Some(n) = self.sounding.take() {
            push(out, frame, [0x80, n, 0]);
        }
        if let Some((n, v)) = self.pick() {
            push(out, frame, [0x90, n, v]);
            self.sounding = Some(n);
            self.gate_end = Some(frame as f64 + step_len * self.bank.get(3) as f64 / 100.0);
        }
        self.step += 1;
    }
}

impl Plugin for Arpeggiator {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, frames: usize, ctx: &ProcessContext) {
        let step_len = rate_samples(self.bank.get(0), ctx.tempo, self.sr);
        let latch = self.bank.get(4) > 0.5;
        if ctx.playing {
            let pos = ctx.position as f64;
            self.next = (pos / step_len).ceil() * step_len - pos;
        }
        let mut i = 0;
        loop {
            let ev_frame = input.get(i).map_or(f64::INFINITY, |e| e.frame as f64);
            let gate = self.gate_end.unwrap_or(f64::INFINITY);
            let t = ev_frame.min(self.next).min(gate);
            if t >= frames as f64 {
                break;
            }
            if gate <= t && gate < self.next.min(ev_frame) {
                self.gate_end = None;
                if let Some(n) = self.sounding.take() {
                    push(out, gate as u32, [0x80, n, 0]);
                }
                continue;
            }
            if ev_frame <= self.next {
                let e = input[i];
                i += 1;
                let d = e.data;
                if is_on(d) {
                    if latch && self.down == 0 {
                        self.count = 0;
                    }
                    self.down += 1;
                    let was_empty = self.count == 0;
                    if self.count < MAX_HELD {
                        self.held[self.count] = (d[1], d[2]);
                        self.count += 1;
                    }
                    if was_empty {
                        self.step = 0;
                        if !ctx.playing {
                            self.next = e.frame as f64;
                        }
                    }
                } else if is_off(d) {
                    self.down = self.down.saturating_sub(1);
                    if latch {
                        continue;
                    }
                    if let Some(k) = self.held[..self.count].iter().position(|h| h.0 == d[1]) {
                        self.held.copy_within(k + 1..self.count, k);
                        self.count -= 1;
                    }
                    if self.count == 0
                        && let Some(n) = self.sounding.take()
                    {
                        push(out, e.frame, [0x80, n, 0]);
                        self.gate_end = None;
                    }
                } else {
                    push(out, e.frame, d);
                }
                continue;
            }
            let f = self.next as u32;
            self.fire(f, step_len, out);
            self.next += step_len;
        }
        // Latch switched off with no keys down lets the latched chord go.
        if !latch && self.down == 0 && self.count > 0 {
            self.count = 0;
            if let Some(n) = self.sounding.take() {
                push(out, frames.saturating_sub(1) as u32, [0x80, n, 0]);
                self.gate_end = None;
            }
        }
        self.next -= frames as f64;
        if let Some(g) = self.gate_end.as_mut() {
            *g -= frames as f64;
        }
        if !ctx.playing && self.count == 0 {
            self.next = 0.0;
        }
        sort(out);
    }
}

pub static NOTE_REPEATER: [ParamDef; 4] = [
    ParamDef::new(0, "Delay", 0.0, 6.0, 1.0, "").choices(&RATES),
    ParamDef::new(1, "Repeats", 1.0, 8.0, 3.0, "").steps(7),
    ParamDef::new(2, "Velocity", -50.0, 0.0, -20.0, "%"),
    ParamDef::new(3, "Transpose", -12.0, 12.0, 0.0, "st").steps(24),
];

const MAX_PENDING: usize = 512;

/// Echoes each note a number of times at a note rate, each echo softer and optionally shifted.
pub struct NoteRepeater {
    bank: Arc<Bank>,
    sr: f32,
    /// Echoes waiting to be sent, as absolute frame and MIDI bytes.
    pending: [(u64, [u8; 3]); MAX_PENDING],
    count: usize,
    now: u64,
    /// Settings each key's echoes were scheduled with, so its note-offs match.
    started: [(u8, i8, u32); 128],
}

impl NoteRepeater {
    pub fn new(bank: Arc<Bank>, sr: f32) -> Self {
        NoteRepeater { bank, sr, pending: [(0, [0; 3]); MAX_PENDING], count: 0, now: 0, started: [(0, 0, 0); 128] }
    }

    fn schedule(&mut self, at: u64, data: [u8; 3]) {
        if self.count < MAX_PENDING {
            self.pending[self.count] = (at, data);
            self.count += 1;
        }
    }
}

impl Plugin for NoteRepeater {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}

    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, frames: usize, ctx: &ProcessContext) {
        let b = self.bank.clone();
        let delay = rate_samples(b.get(0), ctx.tempo, self.sr) as u64;
        let repeats = b.get(1).round().clamp(1.0, 8.0) as u8;
        let vel_step = b.get(2) / 100.0;
        let shift = b.get(3).round() as i8;
        for e in input {
            push(out, e.frame, e.data);
            let d = e.data;
            let key = d[1] as usize & 127;
            let at = self.now + e.frame as u64;
            let (reps, sh, dl) = if is_on(d) {
                self.started[key] = (repeats, shift, delay as u32);
                (repeats, shift, delay)
            } else if is_off(d) {
                let (r, s, dl) = self.started[key];
                (r, s, dl as u64)
            } else {
                continue;
            };
            for r in 1..=reps as i32 {
                let n = d[1] as i32 + sh as i32 * r;
                if !(0..=127).contains(&n) {
                    continue;
                }
                let v = if is_on(d) { (d[2] as f32 * (1.0 + vel_step * r as f32)).round().clamp(1.0, 127.0) as u8 } else { d[2] };
                self.schedule(at + dl * r as u64, [d[0], n as u8, v]);
            }
        }
        let end = self.now + frames as u64;
        let mut k = 0;
        while k < self.count {
            let (at, d) = self.pending[k];
            if at < end {
                push(out, (at - self.now) as u32, d);
                self.count -= 1;
                self.pending[k] = self.pending[self.count];
            } else {
                k += 1;
            }
        }
        self.now = end;
        sort(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &mut dyn Plugin, input: &[MidiEvent], frames: usize, ctx: &ProcessContext) -> Vec<MidiEvent> {
        let mut out = Vec::with_capacity(256);
        p.process_midi(input, &mut out, frames, ctx);
        out
    }

    fn ev(frame: u32, data: [u8; 3]) -> MidiEvent {
        MidiEvent { frame, data }
    }

    fn ctx() -> ProcessContext {
        ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: false }
    }

    #[test]
    fn transposer_keeps_note_offs_matched() {
        let bank = Arc::new(Bank::new(&TRANSPOSER));
        bank.set(0, 5.0);
        let mut t = Transposer::new(bank.clone());
        let on = run(&mut t, &[ev(0, [0x90, 60, 100])], 64, &ctx());
        assert_eq!(on[0].data, [0x90, 65, 100]);
        bank.set(0, -3.0);
        let off = run(&mut t, &[ev(3, [0x80, 60, 0])], 64, &ctx());
        assert_eq!(off[0].data, [0x80, 65, 0]);
    }

    #[test]
    fn scales_and_chords() {
        // C major: C♯ snaps down to C, F♯ down to F.
        assert_eq!(snap_to_scale(61, 0, SCALES[0]), 60);
        assert_eq!(snap_to_scale(66, 0, SCALES[0]), 65);
        // A minor pentatonic (A C D E G): B goes to the nearer C; G♯ is a tie and goes down.
        assert_eq!(snap_to_scale(71, 9, SCALES[7]), 72);
        assert_eq!(snap_to_scale(68, 9, SCALES[7]), 67);
        assert_eq!(chord_notes(60, 0, 0).collect::<Vec<_>>(), vec![60, 64, 67]);
        assert_eq!(chord_notes(60, 1, 1).collect::<Vec<_>>(), vec![72, 63, 67]);
    }

    #[test]
    fn velocity_processor_scales_and_limits() {
        let bank = Arc::new(Bank::new(&VELOCITY));
        bank.set(0, 50.0);
        bank.set(1, 10.0);
        bank.set(3, 40.0);
        let v = VelocityProcessor::new(bank);
        assert_eq!(v.map(40), 30);
        assert_eq!(v.map(127), 40);
    }

    #[test]
    fn arpeggiator_steps_on_the_grid() {
        let bank = Arc::new(Bank::new(&ARPEGGIATOR));
        let mut a = Arpeggiator::new(bank, 48000.0);
        // 1/16 at 120 bpm is 6000 samples.
        let mut c = ProcessContext { playing: true, position: 0, ..ctx() };
        let chord = [ev(0, [0x90, 64, 90]), ev(0, [0x90, 60, 100])];
        let out = run(&mut a, &chord, 12_000, &c);
        let ons: Vec<_> = out.iter().filter(|e| is_on(e.data)).map(|e| (e.frame, e.data[1])).collect();
        assert_eq!(ons, vec![(0, 60), (6000, 64)]);
        // Half-step gates close each note before the next.
        assert!(out.iter().any(|e| e.frame == 3000 && is_off(e.data)));
        c.position = 12_000;
        let out = run(&mut a, &[], 6000, &c);
        assert_eq!(out.iter().find(|e| is_on(e.data)).map(|e| (e.frame, e.data[1])), Some((0, 60)));
    }

    #[test]
    fn note_repeater_echoes_across_blocks() {
        let bank = Arc::new(Bank::new(&NOTE_REPEATER));
        bank.set(0, 3.0);
        bank.set(1, 2.0);
        let mut r = NoteRepeater::new(bank, 48000.0);
        let first = run(&mut r, &[ev(10, [0x90, 60, 100])], 4000, &ctx());
        assert_eq!(first.len(), 1);
        let second = run(&mut r, &[], 16_000, &ctx());
        let echoes: Vec<_> = second.iter().map(|e| (e.frame, e.data[2])).collect();
        assert_eq!(echoes, vec![(6010 - 4000, 80), (12_010 - 4000, 60)]);
    }
}
