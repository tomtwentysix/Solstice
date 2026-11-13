//! The Neural Amp Modeler effect: a capture of an amp or pedal run on the strip's mono sum.

use crate::bank::{Bank, ParamDef};
use crate::resample::Around;
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_nam::Model;
use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub static NAM: [ParamDef; 2] = [ParamDef::new(0, "Input", -24.0, 24.0, 0.0, "dB"), ParamDef::new(1, "Output", -40.0, 12.0, 0.0, "dB")];

/// Most session frames per pass; longer blocks are split.
const BLOCK: usize = 1024;
/// Most frames handed to the model at once.
const MODEL_BLOCK: usize = 512;

/// A capture as saved: `file` as the project holds it (relative to its audio folder once
/// copied in) and `source`, the file it came from, for when `file` is missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureDesc {
    pub file: PathBuf,
    pub source: PathBuf,
}

impl CaptureDesc {
    pub fn name(&self) -> String {
        self.file.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    }
}

struct Runner {
    model: Model,
    around: Option<Around>,
    scratch: Vec<f32>,
}

/// A loaded capture. Loading and freeing allocate; `run` does not.
pub struct Capture {
    pub desc: CaptureDesc,
    /// The rate it was trained at, when it says.
    pub rate: Option<f64>,
    /// Frames added by running it at its own rate.
    pub latency: u32,
    run: UnsafeCell<Runner>,
}

// Only the effect's processor reaches `run`, on one thread at a time; the rest is read-only.
unsafe impl Sync for Capture {}

impl Capture {
    /// Loads the capture at `path` for a session at `sample_rate`. A capture made at another
    /// rate runs at that rate, resampled on the way in and out.
    pub fn load(path: &Path, desc: CaptureDesc, sample_rate: f64) -> Result<Capture, String> {
        let model = Model::load(path, 0.0, MODEL_BLOCK)?;
        let rate = model.expected_rate();
        let around = rate.filter(|r| (r - sample_rate).abs() > 0.5).map(|r| Around::new(sample_rate, r, BLOCK));
        let latency = around.as_ref().map_or(0, |a| a.latency);
        Ok(Capture { desc, rate, latency, run: UnsafeCell::new(Runner { model, around, scratch: vec![0.0; MODEL_BLOCK] }) })
    }

    /// Processes at most `BLOCK` frames in place. Only the effect's processor calls this.
    fn run(&self, io: &mut [f32]) {
        let Runner { model, around, scratch } = unsafe { &mut *self.run.get() };
        match around {
            Some(a) => a.run(io, scratch, |chunk| model.process_in_place(chunk)),
            None => io.chunks_mut(MODEL_BLOCK).for_each(|c| model.process_in_place(c)),
        }
    }
}

/// The capture an effect plays, shared with the RT thread. Replaced captures are kept here until
/// the RT thread has let go of them, so it never frees one.
#[derive(Default)]
pub struct Captures {
    current: Mutex<Option<Arc<Capture>>>,
    retired: Mutex<Vec<Arc<Capture>>>,
    /// A saved capture that could not be loaded, kept so saving does not forget it.
    missing: Mutex<Option<(CaptureDesc, String)>>,
}

impl Captures {
    pub fn set(&self, capture: Option<Capture>) {
        *self.missing.lock().unwrap() = None;
        let new = capture.map(Arc::new);
        let old = std::mem::replace(&mut *self.current.lock().unwrap(), new);
        let mut retired = self.retired.lock().unwrap();
        retired.extend(old);
        retired.retain(|c| Arc::strong_count(c) > 1);
    }

    pub fn current(&self) -> Option<Arc<Capture>> {
        self.current.lock().unwrap().clone()
    }

    /// Plays nothing, remembering `desc` and why it could not be loaded.
    pub fn set_missing(&self, desc: CaptureDesc, error: String) {
        self.set(None);
        *self.missing.lock().unwrap() = Some((desc, error));
    }

    pub fn missing(&self) -> Option<(CaptureDesc, String)> {
        self.missing.lock().unwrap().clone()
    }

    pub fn desc(&self) -> Option<CaptureDesc> {
        self.current().map(|c| c.desc.clone()).or_else(|| self.missing().map(|m| m.0))
    }

    pub fn latency(&self) -> u32 {
        self.current().map_or(0, |c| c.latency)
    }

    /// For the RT thread: the current capture, unless it is being swapped right now.
    fn try_current(&self) -> Option<Option<Arc<Capture>>> {
        self.current.try_lock().ok().map(|c| c.clone())
    }
}

/// Saved state: the parameter bank, then the capture's file and source.
pub fn state(bank: &Bank, desc: Option<&CaptureDesc>) -> Vec<u8> {
    let mut s = String::from_utf8(bank.state()).unwrap_or_default();
    if let Some(d) = desc {
        s.push_str(&format!("#capture\n{}\n{}\n", d.file.display(), d.source.display()));
    }
    s.into_bytes()
}

/// Restores the bank and returns the capture, which the caller loads.
pub fn set_state(bank: &Bank, data: &[u8]) -> Option<CaptureDesc> {
    let text = String::from_utf8_lossy(data);
    let (params, capture) = text.split_once("#capture\n").unwrap_or((&text, ""));
    bank.set_state(params.as_bytes());
    let mut lines = capture.lines();
    let file = PathBuf::from(lines.next().filter(|l| !l.is_empty())?);
    Some(CaptureDesc { file, source: lines.next().map(PathBuf::from).unwrap_or_default() })
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
        if let Some(c) = self.captures.try_current() {
            let same = match (&c, &self.current) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            if !same {
                self.current = c;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        let bank = Bank::new(&NAM);
        bank.set(0, 6.0);
        let desc = CaptureDesc { file: "Clean Amp.nam".into(), source: "/lib/Guitar/Clean Amp.nam".into() };
        let data = state(&bank, Some(&desc));
        let back = Bank::new(&NAM);
        assert_eq!(set_state(&back, &data), Some(desc));
        assert_eq!(back.get(0), 6.0);
        assert_eq!(set_state(&back, &state(&bank, None)), None);
    }
}
