use rtrb::Consumer;
use solstice_dsp::loudness;
use solstice_engine::Handle;
use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;

/// One second at 192 kHz, so a display stall of up to a second loses nothing.
const TAP_FRAMES: usize = 192_000;
const CHUNK: usize = 4096;
const SCOPE: usize = 8192;
/// Short-term readings for the history graph: one every 200 ms, five minutes of them.
pub const HISTORY_STEP: f64 = 0.2;
pub const HISTORY_LEN: usize = 1500;
/// 1/6-octave bands from 20 Hz to 20.48 kHz.
pub const BANDS: usize = 61;
pub const FLOOR_DB: f32 = -120.0;

pub fn band_freq(k: usize) -> f32 {
    20.0 * 2f32.powf(k as f32 / 6.0)
}

/// The Loudness window's measurement of the master output. The engine only copies the output
/// into a ring; everything here runs on the UI thread.
pub struct MasterMeter {
    ring: Option<Consumer<f32>>,
    sample_rate: u32,
    pub loudness: loudness::Meter,
    buf: [Vec<f32>; 2],
    /// Smoothed L·R, L² and R² for the correlation meter.
    products: [f64; 3],
    smoothing: f64,
    scope: Vec<f32>,
    scope_pos: usize,
    pub history: VecDeque<f32>,
    since_history: usize,
    pub bands: [f32; BANDS],
    pub peaks: [f32; BANDS],
    /// Audio went unmeasured because the ring overflowed; stays set until a reset.
    pub interrupted: bool,
    was_playing: bool,
}

impl MasterMeter {
    pub fn new(sample_rate: u32) -> MasterMeter {
        MasterMeter {
            ring: None,
            sample_rate,
            loudness: loudness::Meter::new(sample_rate as f64),
            buf: [Vec::with_capacity(CHUNK), Vec::with_capacity(CHUNK)],
            products: [0.0; 3],
            // A 300 ms time constant.
            smoothing: (-1.0 / (0.3 * sample_rate as f64)).exp(),
            scope: vec![0.0; SCOPE],
            scope_pos: 0,
            history: VecDeque::with_capacity(HISTORY_LEN),
            since_history: 0,
            bands: [FLOOR_DB; BANDS],
            peaks: [FLOOR_DB; BANDS],
            interrupted: false,
            was_playing: false,
        }
    }

    pub fn reset(&mut self) {
        self.loudness.reset();
        self.products = [0.0; 3];
        self.history.clear();
        self.since_history = 0;
        self.peaks = [FLOOR_DB; BANDS];
        self.interrupted = false;
    }

    /// Runs every frame while the window is open: attaches to the engine (again after the audio
    /// server reconnects), resets on play, then measures what the master has played since.
    pub fn run(&mut self, handle: &mut Handle, sample_rate: u32, reset_on_play: bool) {
        if sample_rate != self.sample_rate {
            handle.untap_master();
            *self = MasterMeter::new(sample_rate);
        }
        if self.ring.as_ref().is_none_or(|r| r.is_abandoned()) {
            self.ring = Some(handle.tap_master(TAP_FRAMES));
        }
        self.transport(handle.shared.playing.load(Relaxed), reset_on_play);
        if handle.shared.master_tap_dropped.swap(0, Relaxed) > 0 {
            self.interrupted = true;
        }
        if let Some(mut ring) = self.ring.take() {
            self.drain(&mut ring);
            self.ring = Some(ring);
        }
    }

    pub fn detach(&mut self, handle: &mut Handle) {
        handle.untap_master();
        self.ring = None;
    }

    fn transport(&mut self, playing: bool, reset_on_play: bool) {
        if playing && !self.was_playing && reset_on_play {
            self.reset();
        }
        self.was_playing = playing;
    }

    /// Measures every whole stereo frame waiting in `ring`.
    pub fn drain(&mut self, ring: &mut Consumer<f32>) {
        let Ok(chunk) = ring.read_chunk(ring.slots() & !1) else { return };
        let mut it = chunk.into_iter();
        while let (Some(l), Some(r)) = (it.next(), it.next()) {
            // A plug-in's NaN would otherwise hold the correlation meter for good.
            self.buf[0].push(if l.is_finite() { l } else { 0.0 });
            self.buf[1].push(if r.is_finite() { r } else { 0.0 });
            if self.buf[0].len() == CHUNK {
                self.flush();
            }
        }
        self.flush();
    }

    fn flush(&mut self) {
        let [l, r] = std::mem::take(&mut self.buf);
        self.feed(&l, &r);
        self.buf = [l, r];
        self.buf.iter_mut().for_each(Vec::clear);
    }

    pub fn feed(&mut self, l: &[f32], r: &[f32]) {
        if l.is_empty() {
            return;
        }
        self.loudness.push(l, r);
        let a = self.smoothing;
        for (&x, &y) in l.iter().zip(r) {
            let (x, y) = (x as f64, y as f64);
            for (p, v) in self.products.iter_mut().zip([x * y, x * x, y * y]) {
                *p = a * *p + (1.0 - a) * v;
            }
            self.scope[self.scope_pos] = ((x + y) * 0.5) as f32;
            self.scope_pos = (self.scope_pos + 1) % SCOPE;
        }
        let step = (self.sample_rate as f64 * HISTORY_STEP) as usize;
        self.since_history += l.len();
        while self.since_history >= step {
            self.since_history -= step;
            if self.history.len() == HISTORY_LEN {
                self.history.pop_front();
            }
            self.history.push_back(self.loudness.short_term() as f32);
        }
    }

    /// −1 (out of phase) to +1 (mono); 0 for silence.
    pub fn correlation(&self) -> f32 {
        let [lr, ll, rr] = self.products;
        let d = (ll * rr).sqrt();
        if d < 1e-10 { 0.0 } else { (lr / d).clamp(-1.0, 1.0) as f32 }
    }

    /// Recomputes the 1/6-octave spectrum from the latest audio. Levels rise at once and fall
    /// back gradually; the peak line holds until a reset.
    pub fn update_spectrum(&mut self) {
        let ordered: Vec<f32> = (0..SCOPE).map(|i| self.scope[(self.scope_pos + i) % SCOPE]).collect();
        let db = crate::fft::spectrum_db(&ordered);
        let bin_hz = self.sample_rate as f32 / SCOPE as f32;
        let half_band = 2f32.powf(1.0 / 12.0);
        for k in 0..BANDS {
            let f = band_freq(k);
            let (k0, k1) = ((f / half_band / bin_hz).ceil() as usize, (f * half_band / bin_hz).floor() as usize);
            let level = if f * half_band >= self.sample_rate as f32 / 2.0 {
                FLOOR_DB
            } else if k1 >= k0 {
                // The band's power, divided by the Hann window's noise bandwidth of 1.5 bins so a
                // sine reads its own level. Low bands narrower than a bin take the nearest bin.
                let power = db[k0..=k1].iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / 1.5;
                10.0 * power.max(1e-12).log10()
            } else {
                db[((f / bin_hz).round() as usize).min(db.len() - 1)]
            };
            let shown = &mut self.bands[k];
            *shown = if level > *shown { level } else { *shown + (level - *shown) * 0.25 };
            self.peaks[k] = self.peaks[k].max(*shown);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;

    fn sine(db: f64, secs: f64, sr: u32) -> Vec<f32> {
        let a = 10f64.powf(db / 20.0);
        (0..(secs * sr as f64) as usize).map(|n| (a * (std::f64::consts::TAU * 1000.0 * n as f64 / sr as f64).sin()) as f32).collect()
    }

    #[test]
    fn drains_interleaved_frames_from_the_ring() {
        let sr = 48000;
        let mut m = MasterMeter::new(sr);
        let (mut tx, mut rx) = RingBuffer::new(sr as usize * 2);
        let s = sine(-23.0, 4.0, sr);
        for chunk in s.chunks(sr as usize / 4) {
            for &v in chunk {
                tx.push(v).unwrap();
                tx.push(v).unwrap();
            }
            m.drain(&mut rx);
        }
        assert!(rx.is_empty());
        assert!((m.loudness.integrated() + 23.0).abs() < 0.1, "{}", m.loudness.integrated());
        assert!((m.correlation() - 1.0).abs() < 1e-3);
        assert_eq!(m.history.len(), 20);
    }

    #[test]
    fn a_half_frame_waits_for_its_other_half() {
        let mut m = MasterMeter::new(48000);
        let (mut tx, mut rx) = RingBuffer::new(16);
        for v in [0.5, -0.5, 0.25] {
            tx.push(v).unwrap();
        }
        m.drain(&mut rx);
        assert_eq!(rx.slots(), 1);
        assert!(m.correlation() < -0.99);
    }

    #[test]
    fn a_nan_frame_does_not_hold_the_correlation() {
        let mut m = MasterMeter::new(48000);
        let (mut tx, mut rx) = RingBuffer::new(96002);
        tx.push(f32::NAN).unwrap();
        tx.push(f32::NAN).unwrap();
        for v in sine(-12.0, 1.0, 48000) {
            tx.push(v).unwrap();
            tx.push(v).unwrap();
        }
        m.drain(&mut rx);
        assert!((m.correlation() - 1.0).abs() < 1e-3, "{}", m.correlation());
    }

    #[test]
    fn opposite_channels_read_minus_one() {
        let mut m = MasterMeter::new(48000);
        let l = sine(-12.0, 1.0, 48000);
        let r: Vec<f32> = l.iter().map(|v| -v).collect();
        m.feed(&l, &r);
        assert!((m.correlation() + 1.0).abs() < 1e-3);
    }

    #[test]
    fn play_from_a_stop_resets_only_when_asked() {
        let mut m = MasterMeter::new(48000);
        let s = sine(-20.0, 1.0, 48000);
        m.feed(&s, &s);
        m.interrupted = true;
        m.transport(true, false);
        assert!(m.loudness.integrated().is_finite());
        m.transport(false, true);
        m.transport(true, true);
        assert!(!m.loudness.integrated().is_finite());
        assert!(!m.interrupted);
        m.feed(&s, &s);
        m.transport(true, true);
        assert!(m.loudness.integrated().is_finite(), "carrying on playing is not a new start");
    }

    #[test]
    fn a_sine_shows_in_its_band() {
        let sr = 48000;
        let mut m = MasterMeter::new(sr);
        let s = sine(-6.0, 0.5, sr);
        m.feed(&s, &s);
        m.update_spectrum();
        let k = (0..BANDS).max_by(|&a, &b| m.bands[a].total_cmp(&m.bands[b])).unwrap();
        assert!((band_freq(k) - 1000.0).abs() < 100.0, "{}", band_freq(k));
        assert!((m.bands[k] + 6.0).abs() < 0.5, "{}", m.bands[k]);
        assert_eq!(m.peaks[k], m.bands[k]);
    }
}
