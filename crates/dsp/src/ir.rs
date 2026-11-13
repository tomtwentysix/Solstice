//! The impulse response loader: convolution with a WAV file, most often a guitar cabinet.

use crate::Native;
use crate::bank::{Bank, ParamDef};
use crate::convolve::{Convolver, Fft};
use crate::resample::Stream;
use crate::slot::{FileDesc, Loaded, Slot};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::cell::UnsafeCell;
use std::path::Path;
use std::sync::Arc;

pub static IR: [ParamDef; 2] = [ParamDef::new(0, "Level", -24.0, 24.0, 0.0, "dB"), ParamDef::new(1, "Mix", 0.0, 100.0, 100.0, "%")];

/// Most frames per pass; longer blocks are split.
const BLOCK: usize = 1024;
/// Longer responses are cut to this.
const MAX_SECONDS: f64 = 10.0;
/// Points on the frequency response, spaced evenly in pitch from 20 Hz to 20 kHz.
pub const RESPONSE_POINTS: usize = 160;

/// A loaded impulse response. Loading and freeing allocate; `run` does not.
pub struct Ir {
    pub desc: FileDesc,
    /// The file's own rate.
    pub rate: u32,
    pub channels: usize,
    pub seconds: f64,
    /// Its magnitude response in dB at `RESPONSE_POINTS` frequencies.
    pub response: Vec<f32>,
    run: UnsafeCell<[Convolver; 2]>,
}

// Only the effect's processor reaches `run`, on one thread at a time; the rest is read-only.
unsafe impl Sync for Ir {}

pub type Irs = Slot<Ir>;

impl Loaded for Ir {
    const TAG: &'static str = "#ir";

    /// Loads the WAV file at `path`, resampled to `sample_rate`. Its first two channels are used;
    /// a mono response is applied to both sides.
    fn load(path: &Path, desc: FileDesc, sample_rate: f64) -> Result<Ir, String> {
        let (channels, rate) = read(path)?;
        let mut taps: Vec<Vec<f32>> = channels.iter().map(|c| resample(c, rate as f64, sample_rate)).collect();
        let longest = (MAX_SECONDS * sample_rate) as usize;
        // Trailing silence costs as much as anything else to convolve.
        let peak = taps.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        let end = taps.iter().filter_map(|c| c.iter().rposition(|v| v.abs() > peak * 1.6e-5)).max().map_or(0, |e| e + 1);
        if end == 0 {
            return Err("the file is silent".into());
        }
        taps.iter_mut().for_each(|c| c.truncate(end.min(longest)));
        // Responses come at every level; scaling to unit energy keeps white noise at the level it
        // went in, so changing response does not jump in loudness.
        let energy = taps.iter().map(|c| c.iter().map(|v| v * v).sum::<f32>()).fold(0.0, f32::max);
        let scale = energy.sqrt().recip();
        taps.iter_mut().flatten().for_each(|v| *v *= scale);
        let response = response(&taps[0], sample_rate);
        let run = [Convolver::new(&taps[0]), Convolver::new(taps.last().unwrap())];
        Ok(Ir { desc, rate, channels: taps.len(), seconds: taps[0].len() as f64 / sample_rate, response, run: UnsafeCell::new(run) })
    }

    fn desc(&self) -> &FileDesc {
        &self.desc
    }

    fn slot(native: &Native) -> Option<&Arc<Irs>> {
        native.irs.as_ref()
    }
}

impl Ir {
    /// Processes in place. Only the effect's processor calls this.
    fn run(&self, l: &mut [f32], r: &mut [f32]) {
        let [cl, cr] = unsafe { &mut *self.run.get() };
        cl.run(l);
        cr.run(r);
    }
}

/// The first two channels of a WAV file, and its rate.
fn read(path: &Path) -> Result<(Vec<Vec<f32>>, u32), String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let all = spec.channels.max(1) as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let full = (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 / full)).collect::<Result<_, _>>()
        }
    }
    .map_err(|e| e.to_string())?;
    if samples.len() < all {
        return Err("the file holds no audio".into());
    }
    let channels = (0..all.min(2)).map(|c| samples.iter().skip(c).step_by(all).copied().collect()).collect();
    Ok((channels, spec.sample_rate))
}

fn resample(input: &[f32], from: f64, to: f64) -> Vec<f32> {
    if (from - to).abs() < 0.5 {
        return input.to_vec();
    }
    let want = (input.len() as f64 * to / from).ceil() as usize;
    let mut s = Stream::new(from, to, 16, 0.0);
    let mut out = Vec::with_capacity(want);
    for x in input.iter().copied().chain(std::iter::repeat(0.0)) {
        s.push(x);
        while s.ready() && out.len() < want {
            out.push(s.pull());
        }
        if out.len() == want {
            break;
        }
    }
    out
}

/// The magnitude response in dB, each point the power average of the bins nearest it.
fn response(taps: &[f32], sample_rate: f64) -> Vec<f32> {
    let n = taps.len().next_power_of_two().clamp(4096, 1 << 16);
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    taps.iter().zip(&mut re).for_each(|(t, r)| *r = *t);
    Fft::new(n).forward(&mut re, &mut im);
    let power: Vec<f32> = (0..=n / 2).map(|b| re[b] * re[b] + im[b] * im[b]).collect();
    let bin = |hz: f64| (hz * n as f64 / sample_rate).clamp(0.0, (n / 2) as f64);
    let hz = |i: f64| 20.0 * 1000f64.powf(i / (RESPONSE_POINTS - 1) as f64);
    (0..RESPONSE_POINTS)
        .map(|i| {
            let lo = bin(hz(i as f64 - 0.5)).round() as usize;
            let hi = (bin(hz(i as f64 + 0.5)).round() as usize).max(lo + 1).min(n / 2 + 1);
            let lo = lo.min(hi - 1);
            let mean = power[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
            10.0 * mean.max(1e-12).log10()
        })
        .collect()
}

/// The frequency of a point on `Ir::response`.
pub fn response_hz(i: usize) -> f32 {
    20.0 * 1000f32.powf(i as f32 / (RESPONSE_POINTS - 1) as f32)
}

fn db(v: f32) -> f32 {
    10f32.powf(v / 20.0)
}

pub struct ImpulseResponse {
    bank: Arc<Bank>,
    irs: Arc<Irs>,
    current: Option<Arc<Ir>>,
    dry: [Vec<f32>; 2],
    /// Level as a gain, and mix from 0 to 1.
    gains: [f32; 2],
}

impl ImpulseResponse {
    pub fn new(bank: Arc<Bank>, irs: Arc<Irs>) -> ImpulseResponse {
        let current = irs.current();
        let gains = [db(bank.get(0)), bank.get(1) / 100.0];
        ImpulseResponse { bank, irs, current, dry: [vec![0.0; BLOCK], vec![0.0; BLOCK]], gains }
    }
}

impl Plugin for ImpulseResponse {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        self.irs.follow(&mut self.current);
        let target = [db(self.bank.get(0)), self.bank.get(1) / 100.0];
        let Some(ir) = &self.current else {
            self.gains = target;
            return;
        };
        let mut at = 0;
        while at < l.len() {
            let n = (l.len() - at).min(BLOCK);
            let (l, r) = (&mut l[at..at + n], &mut r[at..at + n]);
            self.dry[0][..n].copy_from_slice(l);
            self.dry[1][..n].copy_from_slice(r);
            ir.run(l, r);
            let [g0, m0] = self.gains;
            let step = [(target[0] - g0) / n as f32, (target[1] - m0) / n as f32];
            for (side, dry) in [l, r].into_iter().zip(&self.dry) {
                for (i, (y, d)) in side.iter_mut().zip(dry).enumerate() {
                    let (g, m) = (g0 + step[0] * i as f32, m0 + step[1] * i as f32);
                    *y = d * (1.0 - m) + *y * g * m;
                }
            }
            self.gains = target;
            at += n;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

    fn write(path: &Path, rate: u32, channels: &[Vec<f32>], int16: bool) {
        let spec = if int16 {
            hound::WavSpec { channels: channels.len() as u16, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int }
        } else {
            solstice_engine::audio::wav_spec(channels.len() as u16, rate)
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for f in 0..channels[0].len() {
            for c in channels {
                if int16 { w.write_sample((c[f] * 32767.0) as i16).unwrap() } else { w.write_sample(c[f]).unwrap() }
            }
        }
        w.finalize().unwrap();
    }

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("solstice-ir-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn load(path: &Path, sr: f64) -> Ir {
        Ir::load(path, FileDesc { file: path.into(), source: Default::default() }, sr).unwrap()
    }

    #[test]
    fn loads_resamples_trims_and_levels() {
        let d = dir("load");
        let mut cab: Vec<f32> = (0..400).map(|i| 0.5 * (-0.6f32).powi(i)).collect();
        cab.extend([0.0; 8000]);
        write(&d.join("cab.wav"), 44100, &[cab], true);
        let ir = load(&d.join("cab.wav"), 48000.0);
        assert_eq!((ir.rate, ir.channels), (44100, 1));
        assert!(ir.seconds < 0.01, "trailing silence trimmed: {}", ir.seconds);
        assert_eq!(ir.response.len(), RESPONSE_POINTS);
        // Alternating signs make it bright: 1 / |1 + 0.6e^-jw| is about 4.5 dB higher at 15 kHz.
        let at = |hz: f32| (0..RESPONSE_POINTS).min_by_key(|&i| (response_hz(i) - hz).abs() as u32).unwrap();
        assert!(ir.response[at(15000.0)] > ir.response[at(100.0)] + 3.0);

        write(&d.join("silent.wav"), 48000, &[vec![0.0; 100]], false);
        assert!(Ir::load(&d.join("silent.wav"), FileDesc::default(), 48000.0).is_err());
        assert!(Ir::load(&d.join("missing.wav"), FileDesc::default(), 48000.0).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_mono_response_processes_both_sides_and_mix_blends() {
        let d = dir("mono");
        write(&d.join("unit.wav"), 48000, &[vec![1.0]], false);
        let bank = Arc::new(Bank::new(&IR));
        let irs = Arc::new(Irs::default());
        let mut fx = ImpulseResponse::new(bank.clone(), irs.clone());
        irs.set(Some(load(&d.join("unit.wav"), 48000.0)));
        let input: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.05).sin()).collect();
        let (mut l, mut r) = (input.clone(), input.iter().map(|v| -0.5 * v).collect::<Vec<_>>());
        fx.process(&mut l, &mut r, &[], &CTX);
        for i in 0..input.len() {
            assert!((l[i] - input[i]).abs() < 1e-5 && (r[i] + 0.5 * input[i]).abs() < 1e-5, "at {i}");
        }

        // A stereo response keeps its sides apart: silence on the right empties it.
        write(&d.join("left.wav"), 48000, &[vec![1.0, 0.0], vec![0.0, 0.0]], false);
        let st = load(&d.join("left.wav"), 48000.0);
        assert_eq!(st.channels, 2);
        irs.set(Some(st));
        bank.set(0, -6.0);
        bank.set(1, 50.0);
        fx.process(&mut [0.0; 64], &mut [0.0; 64], &[], &CTX);
        let (mut l, mut r) = (input.clone(), input.clone());
        fx.process(&mut l, &mut r, &[], &CTX);
        let wet = db(-6.0);
        for i in 0..input.len() {
            assert!((l[i] - input[i] * (0.5 + 0.5 * wet)).abs() < 1e-5 && (r[i] - 0.5 * input[i]).abs() < 1e-5, "at {i}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
