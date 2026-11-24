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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SigChange {
    pub bar: u64,
    pub num: u8,
    pub den: u8,
    /// Beats per click group, such as [2, 2, 3] in 7/8; empty for the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<u8>,
}

/// How strongly a beat is felt: the downbeat, the first beat of a later group, another beat of
/// a group, or a beat of a bar whose beats are each their own group, as in simple meters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Beat {
    Bar,
    Group,
    Plain,
    Single,
}

/// The default grouping: threes in compound meters (6/8, 9/8, 12/8, 6/16...), otherwise one
/// group per beat.
pub fn default_groups(num: u8, den: u8) -> Vec<u8> {
    if den >= 8 && num > 3 && num.is_multiple_of(3) { vec![3; num as usize / 3] } else { vec![1; num as usize] }
}

/// Group starts as a bitset over beat indices, so the audio thread can read it without
/// allocating; empty when every beat is its own group. A grouping that does not add up to
/// `num` falls back to the default.
fn group_starts(num: u8, den: u8, groups: &[u8]) -> [u64; 4] {
    let sum: u32 = groups.iter().map(|&g| g as u32).sum();
    let valid = sum == num as u32 && groups.iter().all(|&g| g > 0);
    let default;
    let groups = if valid {
        groups
    } else {
        default = default_groups(num, den);
        &default
    };
    let mut bits = [0u64; 4];
    if groups.iter().all(|&g| g == 1) {
        return bits;
    }
    let mut at = 0usize;
    for &g in groups {
        bits[at / 64] |= 1 << (at % 64);
        at += g as usize;
    }
    bits
}

fn starts_group(bits: &[u64; 4], beat: u64) -> bool {
    beat < 256 && bits[beat as usize / 64] & (1 << (beat % 64)) != 0
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
    starts: [u64; 4],
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
    starts: [u64; 4],
}

impl BarLine {
    /// Whether the 0-based beat starts a click group.
    pub fn starts_group(&self, beat: u64) -> bool {
        starts_group(&self.starts, beat)
    }
}

/// Converts between samples, ticks and bars, following tempo and signature changes. Ticks
/// count from the project start. `tempo`, `sig_num` and `sig_den` are the opening values, and
/// the length helpers without a position (`bar_len`, `beat_len`, ...) use them; `sig_groups`
/// is the opening signature's grouping.
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
        Self::with_changes(sample_rate, tempo, sig_num, sig_den, &[], &[], &[])
    }

    pub fn with_changes(
        sample_rate: u32,
        tempo: f64,
        sig_num: u8,
        sig_den: u8,
        sig_groups: &[u8],
        tempos: &[TempoChange],
        sigs: &[SigChange],
    ) -> Self {
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
        let (num, den) = (sig_num.max(1), sig_den.max(1));
        let mut sv = vec![SigSeg { bar: 0, tick: 0, num, den, starts: group_starts(num, den, sig_groups) }];
        let mut sorted: Vec<&SigChange> = sigs.iter().collect();
        sorted.sort_by_key(|c| c.bar);
        for c in sorted {
            let last = *sv.last().unwrap();
            let (num, den) = (c.num.max(1), c.den.max(1));
            let tick = last.tick + (c.bar - last.bar) * last.bar_ticks();
            let seg = SigSeg { bar: c.bar, tick, num, den, starts: group_starts(num, den, &c.groups) };
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
            out.push(BarLine { bar, tick, sample, num: s.num, den: s.den, starts: s.starts });
            bar += 1;
            if out.len() > 100_000 {
                break;
            }
        }
        out
    }

    /// The first beat at or after `samples`: its position and level.
    pub fn next_beat(&self, samples: u64) -> (u64, Beat) {
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
        let rel = (at - seg.tick) % seg.bar_ticks();
        let level = if rel == 0 {
            Beat::Bar
        } else if seg.starts == [0; 4] {
            Beat::Single
        } else if starts_group(&seg.starts, rel / seg.beat()) {
            Beat::Group
        } else {
            Beat::Plain
        };
        (self.ticks_to_samples(at), level)
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
        let t = TimeMap::with_changes(48000, 120.0, 4, 4, &[], &[TempoChange { tick: bar, tempo: 60.0 }], &[]);
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
        let t = TimeMap::with_changes(48000, 120.0, 4, 4, &[], &[], &[SigChange { bar: 2, num: 3, den: 4, groups: vec![] }]);
        assert_eq!(t.bar_start(2), 192000);
        assert_eq!(t.bar_start(3), 192000 + 72000);
        assert_eq!(t.position(192000 + 72000 + 24000), (4, 2, 1, 0));
        assert_eq!(t.sig_at(200_000), (3, 4));
        let lines: Vec<u64> = t.bars(0, 300_000).iter().map(|b| b.sample).collect();
        assert_eq!(lines, vec![0, 96000, 192000, 264000]);
        assert_eq!(t.next_beat(192000 + 1), (192000 + 24000, Beat::Single));
        assert_eq!(t.next_beat(192000 + 48001), (264000, Beat::Bar));
        // Bar snapping follows the 3/4 bars.
        assert_eq!(t.snap(264000 + 30000, t.ticks_per_bar()), 264000);
    }

    /// The levels of the beats in the bar starting at `start`, an eighth note (480 ticks) apart.
    fn eighths(t: &TimeMap, start: u64, n: u64) -> Vec<Beat> {
        let eighth = t.ticks_to_samples(PPQ as u64 / 2);
        (0..n).map(|k| t.next_beat(start + k * eighth)).inspect(|b| assert_eq!(b.0 % eighth, 0)).map(|b| b.1).collect()
    }

    #[test]
    fn default_grouping() {
        use Beat::*;
        let six = TimeMap::new(48000, 120.0, 6, 8);
        assert_eq!(eighths(&six, 0, 7), vec![Bar, Plain, Plain, Group, Plain, Plain, Bar]);
        let twelve = TimeMap::new(48000, 120.0, 12, 8);
        assert_eq!(eighths(&twelve, 0, 12), vec![Bar, Plain, Plain, Group, Plain, Plain, Group, Plain, Plain, Group, Plain, Plain]);
        // Simple meters and 3/8 click every beat.
        let three = TimeMap::new(48000, 120.0, 3, 8);
        assert_eq!(eighths(&three, 0, 4), vec![Bar, Single, Single, Bar]);
        let four = TimeMap::new(48000, 120.0, 4, 4);
        assert_eq!(four.next_beat(1).1, Single);
        assert!(!four.bars(0, 0)[0].starts_group(1), "no beat line stands out");
        assert_eq!(default_groups(6, 4), vec![1; 6]);
    }

    #[test]
    fn explicit_grouping() {
        use Beat::*;
        let seven = TimeMap::with_changes(48000, 120.0, 7, 8, &[2, 2, 3], &[], &[]);
        assert_eq!(eighths(&seven, 0, 8), vec![Bar, Plain, Group, Plain, Group, Plain, Plain, Bar]);
        // Groups that don't add up to the numerator are ignored.
        let bad = TimeMap::with_changes(48000, 120.0, 7, 8, &[2, 2], &[], &[]);
        assert_eq!(eighths(&bad, 0, 3), vec![Bar, Single, Single]);
        let zero = TimeMap::with_changes(48000, 120.0, 6, 8, &[0, 6], &[], &[]);
        assert_eq!(eighths(&zero, 0, 4), vec![Bar, Plain, Plain, Group]);
    }

    #[test]
    fn grouping_follows_signature_changes() {
        use Beat::*;
        // A bar of 4/4, then 7/8 as 3+2+2, then 6/8 with the default grouping.
        let t = TimeMap::with_changes(
            48000,
            120.0,
            4,
            4,
            &[],
            &[],
            &[SigChange { bar: 1, num: 7, den: 8, groups: vec![3, 2, 2] }, SigChange { bar: 2, num: 6, den: 8, groups: vec![] }],
        );
        assert_eq!(eighths(&t, 0, 2), vec![Bar, Single]);
        assert_eq!(eighths(&t, 96000, 8), vec![Bar, Plain, Plain, Group, Plain, Group, Plain, Bar]);
        let six = t.bar_start(2);
        assert_eq!(eighths(&t, six, 4), vec![Bar, Plain, Plain, Group]);
        let line = t.bars(96000, 96000)[0];
        assert!(line.starts_group(3) && !line.starts_group(1) && line.starts_group(5));
    }
}
