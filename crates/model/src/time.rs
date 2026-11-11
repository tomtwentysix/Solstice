use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const PPQ: u32 = 960;

/// A tempo from a musical position onwards, in quarter notes per minute.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TempoChange {
    pub tick: u64,
    pub tempo: f64,
}

/// A time signature from the start of a bar (0-based) onwards.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SigChange {
    pub bar: u64,
    pub num: u8,
    pub den: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TempoSeg {
    tick: u64,
    sample: f64,
    spt: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SigSeg {
    bar: u64,
    tick: u64,
    num: u8,
    den: u8,
}

impl SigSeg {
    fn beat(&self) -> u64 {
        PPQ as u64 * 4 / self.den as u64
    }

    fn bar_ticks(&self) -> u64 {
        self.beat() * self.num as u64
    }
}

/// A bar line between two positions, from `TimeMap::bars`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarLine {
    /// 0-based.
    pub bar: u64,
    pub tick: u64,
    pub sample: u64,
    pub num: u8,
    pub den: u8,
}

/// Converts between samples, ticks and bars, following tempo and signature changes. Ticks
/// count from the project start. `tempo`, `sig_num` and `sig_den` are the opening values, and
/// the length helpers without a position (`bar_len`, `beat_len`, ...) use them.
#[derive(Clone, Debug, PartialEq)]
pub struct TimeMap {
    pub sample_rate: u32,
    pub tempo: f64,
    pub sig_num: u8,
    pub sig_den: u8,
    tempos: Arc<[TempoSeg]>,
    sigs: Arc<[SigSeg]>,
}

impl TimeMap {
    pub fn new(sample_rate: u32, tempo: f64, sig_num: u8, sig_den: u8) -> Self {
        Self::with_changes(sample_rate, tempo, sig_num, sig_den, &[], &[])
    }

    pub fn with_changes(sample_rate: u32, tempo: f64, sig_num: u8, sig_den: u8, tempos: &[TempoChange], sigs: &[SigChange]) -> Self {
        let spt = |t: f64| sample_rate as f64 * 60.0 / t.max(1.0) / PPQ as f64;
        let mut tv = vec![TempoSeg { tick: 0, sample: 0.0, spt: spt(tempo) }];
        let mut sorted: Vec<TempoChange> = tempos.to_vec();
        sorted.sort_by_key(|c| c.tick);
        for c in sorted {
            let last = *tv.last().unwrap();
            let seg = TempoSeg { tick: c.tick, sample: last.sample + (c.tick - last.tick) as f64 * last.spt, spt: spt(c.tempo) };
            if c.tick == last.tick {
                *tv.last_mut().unwrap() = seg;
            } else {
                tv.push(seg);
            }
        }
        let mut sv = vec![SigSeg { bar: 0, tick: 0, num: sig_num.max(1), den: sig_den.max(1) }];
        let mut sorted: Vec<SigChange> = sigs.to_vec();
        sorted.sort_by_key(|c| c.bar);
        for c in sorted {
            let last = *sv.last().unwrap();
            let seg = SigSeg { bar: c.bar, tick: last.tick + (c.bar - last.bar) * last.bar_ticks(), num: c.num.max(1), den: c.den.max(1) };
            if c.bar == last.bar {
                *sv.last_mut().unwrap() = seg;
            } else {
                sv.push(seg);
            }
        }
        TimeMap { sample_rate, tempo, sig_num, sig_den, tempos: tv.into(), sigs: sv.into() }
    }

    pub fn has_changes(&self) -> bool {
        self.tempos.len() > 1 || self.sigs.len() > 1
    }

    fn tempo_seg_tick(&self, tick: f64) -> &TempoSeg {
        let i = self.tempos.partition_point(|s| s.tick as f64 <= tick);
        &self.tempos[i.saturating_sub(1)]
    }

    fn tempo_seg_sample(&self, sample: f64) -> &TempoSeg {
        let i = self.tempos.partition_point(|s| s.sample <= sample);
        &self.tempos[i.saturating_sub(1)]
    }

    fn sig_seg(&self, tick: u64) -> &SigSeg {
        let i = self.sigs.partition_point(|s| s.tick <= tick);
        &self.sigs[i.saturating_sub(1)]
    }

    /// Samples per tick at the opening tempo.
    pub fn samples_per_tick(&self) -> f64 {
        self.tempos[0].spt
    }

    pub fn ticks_per_beat(&self) -> u64 {
        self.sigs[0].beat()
    }

    pub fn ticks_per_bar(&self) -> u64 {
        self.sigs[0].bar_ticks()
    }

    /// The sample position of an absolute tick.
    pub fn ticks_to_samples(&self, ticks: u64) -> u64 {
        self.tick_to_sample_f(ticks as f64).round() as u64
    }

    pub fn samples_to_ticks(&self, samples: u64) -> u64 {
        self.sample_to_tick_f(samples as f64).round() as u64
    }

    pub fn tick_to_sample_f(&self, tick: f64) -> f64 {
        let s = self.tempo_seg_tick(tick);
        s.sample + (tick - s.tick as f64) * s.spt
    }

    pub fn sample_to_tick_f(&self, sample: f64) -> f64 {
        let s = self.tempo_seg_sample(sample);
        s.tick as f64 + (sample - s.sample) / s.spt
    }

    /// Where `ticks` after the sample position `start` falls, following tempo changes.
    pub fn at(&self, start: u64, ticks: u64) -> u64 {
        self.tick_to_sample_f(self.sample_to_tick_f(start as f64) + ticks as f64).round() as u64
    }

    /// Ticks from sample position `start` to `end`.
    pub fn ticks_between(&self, start: u64, end: u64) -> u64 {
        (self.sample_to_tick_f(end as f64) - self.sample_to_tick_f(start as f64)).max(0.0).round() as u64
    }

    /// Length of a beat at the opening tempo and signature.
    pub fn beat_len(&self) -> u64 {
        (self.ticks_per_beat() as f64 * self.samples_per_tick()).round() as u64
    }

    pub fn bar_len(&self) -> u64 {
        (self.ticks_per_bar() as f64 * self.samples_per_tick()).round() as u64
    }

    pub fn tempo_at(&self, samples: u64) -> f64 {
        self.sample_rate as f64 * 60.0 / PPQ as f64 / self.tempo_seg_sample(samples as f64).spt
    }

    pub fn sig_at(&self, samples: u64) -> (u8, u8) {
        let s = self.sig_seg(self.samples_to_ticks(samples));
        (s.num, s.den)
    }

    /// The tick a 0-based bar starts at.
    pub fn bar_tick(&self, bar: u64) -> u64 {
        let i = self.sigs.partition_point(|s| s.bar <= bar);
        let s = &self.sigs[i.saturating_sub(1)];
        s.tick + (bar - s.bar) * s.bar_ticks()
    }

    pub fn bar_start(&self, bar: u64) -> u64 {
        self.ticks_to_samples(self.bar_tick(bar))
    }

    /// The 0-based bar containing a sample position.
    pub fn bar_at(&self, samples: u64) -> u64 {
        let tick = self.sample_to_tick_f(samples as f64).max(0.0) as u64;
        let s = self.sig_seg(tick);
        s.bar + (tick - s.tick) / s.bar_ticks()
    }

    /// Bar lines from the bar containing `from` up to `to`.
    pub fn bars(&self, from: u64, to: u64) -> Vec<BarLine> {
        let mut out = Vec::new();
        let mut bar = self.bar_at(from);
        loop {
            let tick = self.bar_tick(bar);
            let sample = self.ticks_to_samples(tick);
            if sample > to && !out.is_empty() {
                break;
            }
            let s = self.sig_seg(tick);
            out.push(BarLine { bar, tick, sample, num: s.num, den: s.den });
            bar += 1;
            if out.len() > 100_000 {
                break;
            }
        }
        out
    }

    /// The first beat at or after `samples`: its position and whether it starts a bar.
    pub fn next_beat(&self, samples: u64) -> (u64, bool) {
        let tick = self.sample_to_tick_f(samples as f64).max(0.0);
        let s = self.sig_seg(tick.ceil() as u64);
        let beat = s.beat() as f64;
        let n = ((tick - s.tick as f64) / beat - 1e-9).ceil().max(0.0) as u64;
        let at = s.tick + n * s.beat();
        // A signature change can fall inside the beat grid of the previous one.
        let at = match self.sigs.iter().find(|x| x.tick > s.tick && x.tick < at) {
            Some(next) => next.tick,
            None => at,
        };
        let seg = self.sig_seg(at);
        let downbeat = (at - seg.tick) % seg.bar_ticks() == 0;
        (self.ticks_to_samples(at), downbeat)
    }

    /// Returns 1-based (bar, beat, sixteenth, tick) as shown in the LCD.
    pub fn position(&self, samples: u64) -> (u64, u64, u64, u64) {
        let ticks = self.sample_to_tick_f(samples as f64).max(0.0) as u64;
        let s = self.sig_seg(ticks);
        let rel = ticks - s.tick;
        let bar = s.bar + rel / s.bar_ticks();
        let rem = rel % s.bar_ticks();
        let beat = rem / s.beat();
        let rem = rem % s.beat();
        let six = rem / (PPQ as u64 / 4);
        (bar + 1, beat + 1, six + 1, rem % (PPQ as u64 / 4))
    }

    pub fn seconds(&self, samples: u64) -> f64 {
        samples as f64 / self.sample_rate as f64
    }

    /// Rounds `samples` to the nearest multiple of `grid_ticks`, counted from the start of the
    /// signature in force there so bar grids follow bar lines.
    pub fn snap(&self, samples: u64, grid_ticks: u64) -> u64 {
        if grid_ticks == 0 {
            return samples;
        }
        let ticks = self.sample_to_tick_f(samples as f64).max(0.0);
        let s = self.sig_seg(ticks as u64);
        // A whole-bar grid in the opening signature means "bars" wherever it's used.
        let grid = if grid_ticks == self.ticks_per_bar() { s.bar_ticks() } else { grid_ticks };
        let snapped = s.tick + ((ticks - s.tick as f64) / grid as f64).round() as u64 * grid;
        self.ticks_to_samples(snapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        let t = TimeMap::new(48000, 120.0, 4, 4);
        assert_eq!(t.beat_len(), 24000);
        assert_eq!(t.bar_len(), 96000);
        assert_eq!(t.position(0), (1, 1, 1, 0));
        assert_eq!(t.position(96000 + 24000 + 6000), (2, 2, 2, 0));
        assert_eq!(t.snap(25000, PPQ as u64), 24000);
        let six_eight = TimeMap::new(48000, 120.0, 6, 8);
        assert_eq!(six_eight.bar_len(), 72000);
    }

    #[test]
    fn tempo_changes() {
        // 120 bpm for a bar, then 60 bpm: the second bar is twice as long.
        let bar = 4 * PPQ as u64;
        let t = TimeMap::with_changes(48000, 120.0, 4, 4, &[TempoChange { tick: bar, tempo: 60.0 }], &[]);
        assert_eq!(t.ticks_to_samples(bar), 96000);
        assert_eq!(t.ticks_to_samples(2 * bar), 96000 + 192000);
        assert_eq!(t.samples_to_ticks(96000 + 48000), bar + PPQ as u64);
        assert_eq!(t.position(96000 + 48000), (2, 2, 1, 0));
        assert_eq!(t.tempo_at(100), 120.0);
        assert_eq!(t.tempo_at(100_000), 60.0);
        // A beat from just before the change spans both tempos.
        assert_eq!(t.at(72000, PPQ as u64 * 2), 96000 + 48000);
        assert_eq!(t.ticks_between(72000, 96000 + 48000), PPQ as u64 * 2);
    }

    #[test]
    fn signature_changes() {
        // Two bars of 4/4, then 3/4.
        let t = TimeMap::with_changes(48000, 120.0, 4, 4, &[], &[SigChange { bar: 2, num: 3, den: 4 }]);
        assert_eq!(t.bar_start(2), 192000);
        assert_eq!(t.bar_start(3), 192000 + 72000);
        assert_eq!(t.position(192000 + 72000 + 24000), (4, 2, 1, 0));
        assert_eq!(t.sig_at(200_000), (3, 4));
        let lines: Vec<u64> = t.bars(0, 300_000).iter().map(|b| b.sample).collect();
        assert_eq!(lines, vec![0, 96000, 192000, 264000]);
        assert_eq!(t.next_beat(192000 + 1), (192000 + 24000, false));
        assert_eq!(t.next_beat(192000 + 48001), (264000, true));
        // Bar snapping follows the 3/4 bars.
        assert_eq!(t.snap(264000 + 30000, t.ticks_per_bar()), 264000);
    }
}
