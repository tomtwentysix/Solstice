//! The Neural Amp Modeler effect: a capture of an amp or pedal run on the strip's mono sum.

use crate::bank::{Bank, ParamDef};
use crate::resample::Around;
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_nam::Model;
use crate::Native;
use crate::slot::{FileDesc, Loaded, Slot};
use std::cell::UnsafeCell;
use std::path::Path;
use std::sync::Arc;

pub static NAM: [ParamDef; 2] = [ParamDef::new(0, "Input", -24.0, 24.0, 0.0, "dB"), ParamDef::new(1, "Output", -40.0, 12.0, 0.0, "dB")];

/// Most session frames per pass; longer blocks are split.
const BLOCK: usize = 1024;
/// Most frames handed to the model at once.
const MODEL_BLOCK: usize = 512;

struct Runner {
    model: Model,
    around: Option<Around>,
    scratch: Vec<f32>,
}

/// A loaded capture. Loading and freeing allocate; `run` does not.
pub struct Capture {
    pub desc: FileDesc,
    /// The rate it was trained at, when it says.
    pub rate: Option<f64>,
    /// Frames added by running it at its own rate.
    pub latency: u32,
    run: UnsafeCell<Runner>,
}

// Only the effect's processor reaches `run`, on one thread at a time; the rest is read-only.
unsafe impl Sync for Capture {}

pub type Captures = Slot<Capture>;

impl Loaded for Capture {
    const TAG: &'static str = "#capture";

    /// Loads the capture at `path` for a session at `sample_rate`. A capture made at another
    /// rate runs at that rate, resampled on the way in and out.
    fn load(path: &Path, desc: FileDesc, sample_rate: f64) -> Result<Capture, String> {
        let model = Model::load(path, 0.0, MODEL_BLOCK)?;
        let rate = model.expected_rate();
        let around = rate.filter(|r| (r - sample_rate).abs() > 0.5).map(|r| Around::new(sample_rate, r, BLOCK));
        let latency = around.as_ref().map_or(0, |a| a.latency);
        Ok(Capture { desc, rate, latency, run: UnsafeCell::new(Runner { model, around, scratch: vec![0.0; MODEL_BLOCK] }) })
    }

    fn desc(&self) -> &FileDesc {
        &self.desc
    }

    fn slot(native: &Native) -> Option<&Arc<Captures>> {
        native.captures.as_ref()
    }

    fn latency(&self) -> u32 {
        self.latency
    }
}

impl Capture {
    /// Processes at most `BLOCK` frames in place. Only the effect's processor calls this.
    fn run(&self, io: &mut [f32]) {
        let Runner { model, around, scratch } = unsafe { &mut *self.run.get() };
        match around {
            Some(a) => a.run(io, scratch, |chunk| model.process_in_place(chunk)),
            None => io.chunks_mut(MODEL_BLOCK).for_each(|c| model.process_in_place(c)),
        }
    }
}

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

pub struct Nam {
    bank: Arc<Bank>,
    captures: Arc<Captures>,
    current: Option<Arc<Capture>>,
    mono: Vec<f32>,
    gains: [f32; 2],
    peak: f32,
    sr: f32,
}

impl Nam {
    pub fn new(bank: Arc<Bank>, captures: Arc<Captures>, sr: f32) -> Nam {
        let current = captures.current();
        let gains = [db(bank.get(0)), db(bank.get(1))];
        Nam { bank, captures, current, mono: vec![0.0; BLOCK], gains, peak: 0.0, sr }
    }
}

impl Plugin for Nam {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        self.captures.follow(&mut self.current);
        let target = [db(self.bank.get(0)), db(self.bank.get(1))];
        // The meter shows the level going into the capture, falling back over about a second.
        self.peak *= (-(l.len() as f32) / self.sr).exp();
        let Some(capture) = &self.current else {
            self.gains = target;
            self.bank.set_meter(20.0 * self.peak.max(1e-9).log10());
            return;
        };
        let mut at = 0;
        while at < l.len() {
            let n = (l.len() - at).min(BLOCK);
            let [g0, g1] = self.gains;
            let step = [(target[0] - g0) / n as f32, (target[1] - g1) / n as f32];
            for i in 0..n {
                let x = (l[at + i] + r[at + i]) * 0.5 * (g0 + step[0] * i as f32);
                self.peak = self.peak.max(x.abs());
                self.mono[i] = x;
            }
            capture.run(&mut self.mono[..n]);
            for i in 0..n {
                let y = self.mono[i] * (g1 + step[1] * i as f32);
                l[at + i] = y;
                r[at + i] = y;
            }
            self.gains = target;
            at += n;
        }
        self.bank.set_meter(20.0 * self.peak.max(1e-9).log10());
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}
