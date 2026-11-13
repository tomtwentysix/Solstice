use crate::{Note, PPQ};
use serde::{Deserialize, Serialize};

/// A step sequence that a MIDI region's notes are baked from. Times are in ticks: `rate` is
/// ticks per step and `length` steps per loop. A row can override either, and then loops on
/// its own.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pattern {
    pub rate: u32,
    pub length: u32,
    /// 0 to 1: every second step is delayed by up to half a step.
    #[serde(default)]
    pub swing: f32,
    pub rows: Vec<PatternRow>,
    /// The ticks the notes were last baked over.
    #[serde(default)]
    pub span: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PatternRow {
    pub pitch: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swing: Option<f32>,
    /// Only the steps that are on, sorted by index.
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Step {
    pub index: u16,
    pub velocity: u8,
    /// Percent of the step (or of each repeat) the note lasts.
    #[serde(default = "hundred", skip_serializing_if = "is_hundred")]
    pub gate: u8,
    /// Ratchet: 1 to 8 evenly spaced hits within the step.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub repeat: u8,
    /// Percent chance, 1 to 100, rolled for each hit.
    #[serde(default = "hundred", skip_serializing_if = "is_hundred")]
    pub chance: u8,
    /// Percent of a step, -50 to 50, that the hit moves from the grid.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: i8,
    /// Extends the note of the step before through this one instead of playing a new hit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tie: bool,
}

fn hundred() -> u8 {
    100
}

fn is_hundred(v: &u8) -> bool {
    *v == 100
}

fn one() -> u8 {
    1
}

fn is_one(v: &u8) -> bool {
    *v == 1
}

fn is_zero(v: &i8) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Marsaglia's xorshift32. A zero state stays zero, so seeds must not be 0.
#[inline]
pub fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

fn swing_delay(index: u32, rate: u32, swing: f32) -> u32 {
    if index % 2 == 1 { (swing.clamp(0.0, 1.0) * rate as f32 / 2.0) as u32 } else { 0 }
}

fn offset_ticks(offset: i8, rate: u32) -> i64 {
    offset.clamp(-50, 50) as i64 * rate as i64 / 100
}

fn hit_length(gate: u8, rate: u32, repeat: u8) -> u32 {
    (gate.clamp(1, 100) as u32 * rate / (100 * repeat.clamp(1, 8) as u32)).max(1)
}

impl Default for Pattern {
    fn default() -> Self {
        Pattern { rate: PPQ / 4, length: 16, swing: 0.0, rows: Vec::new(), span: 0 }
    }
}

impl Step {
    pub fn new(index: u16, velocity: u8) -> Self {
        Step { index, velocity, gate: 100, repeat: 1, chance: 100, offset: 0, tie: false }
    }
}

impl PatternRow {
    pub fn new(pitch: u8) -> Self {
        PatternRow { pitch, rate: None, length: None, swing: None, steps: Vec::new() }
    }

    pub fn step(&self, index: u16) -> Option<&Step> {
        self.steps.binary_search_by_key(&index, |s| s.index).ok().map(|i| &self.steps[i])
    }

    /// Puts `s` in place of any step at its index.
    pub fn set(&mut self, s: Step) {
        match self.steps.binary_search_by_key(&s.index, |x| x.index) {
            Ok(i) => self.steps[i] = s,
            Err(i) => self.steps.insert(i, s),
        }
    }

    pub fn remove(&mut self, index: u16) {
        self.steps.retain(|s| s.index != index);
    }

    pub fn clear(&mut self) {
        self.steps.clear();
    }

    /// Rotates the first `len` steps by `by`, wrapping round.
    pub fn shift(&mut self, by: i32, len: u32) {
        let len = len.max(1) as i64;
        for s in self.steps.iter_mut().filter(|s| (s.index as i64) < len) {
            s.index = (s.index as i64 + by as i64).rem_euclid(len) as u16;
        }
        self.steps.sort_by_key(|s| s.index);
    }

    /// Whether step `index` is a tie that holds on the note of the step before it, counting
    /// round the loop of `len` steps.
    pub fn joins(&self, index: u16, len: u32) -> bool {
        let len = len.clamp(1, u16::MAX as u32 + 1);
        let prev = ((index as u32 + len - 1) % len) as u16;
        self.step(index).is_some_and(|s| s.tie) && self.step(prev).is_some()
    }

    /// Plays the first `len` steps backwards. A tied note keeps its sound: it moves whole to
    /// end where it began, its steps in the same order, including a tie round the loop.
    pub fn reverse(&mut self, len: u32) {
        let len = len.clamp(1, u16::MAX as u32 + 1);
        let (inside, mut out): (Vec<Step>, Vec<Step>) = self.steps.iter().partition(|s| (s.index as u32) < len);
        let at = |i: u32| inside.binary_search_by_key(&(i as u16), |s| s.index).ok();
        let mut done = vec![false; inside.len()];
        for h in 0..inside.len() {
            if self.joins(inside[h].index, len) {
                continue;
            }
            let mut chain = vec![h];
            while let Some(k) = at((inside[*chain.last().unwrap()].index as u32 + 1) % len).filter(|&k| k != h && inside[k].tie) {
                chain.push(k);
            }
            let head = (2 * len - 1 - inside[h].index as u32 - (chain.len() as u32 - 1)) % len;
            for (j, &k) in chain.iter().enumerate() {
                done[k] = true;
                out.push(Step { index: ((head + j as u32) % len) as u16, tie: j > 0, ..inside[k] });
            }
        }
        // Left over: a loop of ties with no head, which holds one note however it turns.
        for (s, _) in inside.iter().zip(&done).filter(|(_, d)| !**d) {
            out.push(Step { index: (len - 1 - s.index as u32) as u16, ..*s });
        }
        out.sort_by_key(|s| s.index);
        self.steps = out;
    }

    /// Turns on every `n`th step from `from` within the first `len`.
    pub fn fill_every(&mut self, n: u32, from: u32, len: u32) {
        for i in (from..len.min(u16::MAX as u32 + 1)).step_by(n.max(1) as usize) {
            if self.step(i as u16).is_none() {
                self.set(Step::new(i as u16, 100));
            }
        }
    }

    /// Replaces the first `len` steps with `k` hits spread as evenly as possible over every
    /// `n` steps.
    pub fn euclid(&mut self, k: u32, n: u32, len: u32) {
        let n = n.max(1);
        let len = len.min(u16::MAX as u32 + 1);
        self.steps.retain(|s| s.index as u32 >= len);
        for i in 0..len {
            if (i % n) * k % n < k {
                self.set(Step::new(i as u16, 100));
            }
        }
    }

    /// Replaces the first `len` steps with random hits; `density` is the percent that are on.
    pub fn randomise(&mut self, seed: u32, density: u8, len: u32) {
        let mut rng = seed | 1;
        let len = len.min(u16::MAX as u32 + 1);
        self.steps.retain(|s| s.index as u32 >= len);
        for i in 0..len {
            if xorshift(&mut rng) % 100 < density as u32 {
                let velocity = 64 + (xorshift(&mut rng) % 64) as u8;
                self.set(Step::new(i as u16, velocity));
            }
        }
    }
}

impl Pattern {
    /// The row's (rate, length, swing), its own or the pattern's.
    pub fn timing(&self, row: &PatternRow) -> (u32, u32, f32) {
        (row.rate.unwrap_or(self.rate), row.length.unwrap_or(self.length), row.swing.unwrap_or(self.swing))
    }

    /// Twice as many steps, the second half a copy of the first. Rows with their own length
    /// keep looping as they were.
    pub fn double(&mut self) {
        let len = self.length;
        if len * 2 > u16::MAX as u32 + 1 {
            return;
        }
        for row in self.rows.iter_mut().filter(|r| r.length.is_none()) {
            let copies: Vec<Step> = row
                .steps
                .iter()
                .filter(|s| (s.index as u32) < len)
                .map(|s| Step { index: s.index + len as u16, ..*s })
                .collect();
            row.steps.retain(|s| (s.index as u32) < len);
            row.steps.extend(copies);
        }
        self.length = len * 2;
    }

    /// The notes the pattern plays over `span` ticks, sorted by (start, pitch). Each row loops
    /// on its own. A note ends by the next note of its pitch and by `span`, so no two notes
    /// of one pitch overlap.
    pub fn bake(&self, span: u32) -> Vec<Note> {
        let mut out: Vec<Note> = Vec::new();
        let span = span as u64;
        for row in &self.rows {
            let (rate, len, swing) = self.timing(row);
            let period = rate as u64 * len as u64;
            if period == 0 {
                continue;
            }
            // The last step (counted across loops) that sounded, and its note in `out`.
            let mut last: Option<(u64, usize)> = None;
            let mut base = 0;
            while base < span {
                for s in row.steps.iter().filter(|s| (s.index as u32) < len) {
                    let step = base / rate as u64 + s.index as u64;
                    let grid = base + s.index as u64 * rate as u64 + swing_delay(s.index as u32, rate, swing) as u64;
                    if s.tie
                        && let Some((prev, n)) = last
                        && prev + 1 == step
                    {
                        let end = grid + (s.gate.clamp(1, 100) as u64 * rate as u64 / 100).max(1);
                        out[n].length = end.saturating_sub(out[n].start as u64).clamp(1, u32::MAX as u64) as u32;
                        last = Some((step, n));
                        continue;
                    }
                    last = None;
                    let at = (grid as i64 + offset_ticks(s.offset, rate)).max(0) as u64;
                    let repeat = s.repeat.clamp(1, 8) as u64;
                    for j in 0..repeat {
                        let start = at + j * rate as u64 / repeat;
                        if start >= span {
                            break;
                        }
                        out.push(Note {
                            start: start as u32,
                            length: hit_length(s.gate, rate, s.repeat),
                            pitch: row.pitch,
                            velocity: s.velocity.clamp(1, 127),
                            chance: s.chance.clamp(1, 100),
                        });
                        last = Some((step, out.len() - 1));
                    }
                }
                base += period;
            }
        }
        out.sort_by_key(|n| (n.pitch, n.start));
        for i in 0..out.len() {
            let next = out.get(i + 1).filter(|m| m.pitch == out[i].pitch).map_or(span, |m| m.start as u64);
            let n = &mut out[i];
            n.length = (n.length as u64).min(next - n.start as u64) as u32;
        }
        out.retain(|n| n.length > 0);
        out.sort_by_key(|n| (n.start, n.pitch));
        out
    }

    /// A pattern of `rate`-tick steps that plays `notes` over `span` ticks, with each note
    /// on its nearest step that can play it. A row whose odd steps are all late by the same
    /// amount gets swing rather than an offset on each. Also returns how many notes it plays
    /// differently or not at all, so 0 means the conversion is lossless.
    pub fn from_notes(notes: &[Note], rate: u32, span: u32) -> (Pattern, usize) {
        let rate = rate.max(1);
        let length = span.div_ceil(rate).clamp(1, u16::MAX as u32 + 1);
        let mut p = Pattern { rate, length, swing: 0.0, rows: Vec::new(), span };
        let mut pitches: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
        pitches.sort_unstable();
        pitches.dedup();
        let inside: Vec<Note> = notes.iter().filter(|n| n.start < span).copied().collect();
        let overall = swing_of(&inside, rate);
        for pitch in pitches {
            let mut mine: Vec<Note> = notes.iter().filter(|n| n.pitch == pitch && n.start < span).copied().collect();
            mine.sort_by_key(|n| n.start);
            let score = |r: &PatternRow| {
                let alone = Pattern { rate, length, swing: 0.0, rows: vec![r.clone()], span };
                (differing(&mine, &alone.bake(span)), r.steps.iter().filter(|s| s.offset != 0).count())
            };
            let mut row = row_from_notes(pitch, &mine, rate, span, 0.0);
            let mut best = score(&row);
            let own = swing_of(&mine, rate).filter(|s| Some(*s) != overall);
            for swing in [overall, own].into_iter().flatten() {
                if best == (0, 0) {
                    break;
                }
                let swung = PatternRow { swing: Some(swing), ..row_from_notes(pitch, &mine, rate, span, swing) };
                let s = score(&swung);
                if s < best {
                    (row, best) = (swung, s);
                }
            }
            p.rows.push(row);
        }
        // One swing for every row that has any becomes the pattern's.
        if let Some(swing) = p.rows.iter().find_map(|r| r.swing)
            && p.rows.iter().all(|r| r.swing.is_none_or(|s| s == swing))
        {
            p.swing = swing;
            for r in &mut p.rows {
                let odd = r.steps.iter().any(|s| s.index % 2 == 1);
                r.swing = if r.swing.is_none() && odd { Some(0.0) } else { None };
            }
        }
        let moved = differing(&inside, &p.bake(span)) + notes.len() - inside.len();
        (p, moved)
    }
}

/// The swing that delays the odd steps by the delay most of `notes` on odd steps share.
fn swing_of(notes: &[Note], rate: u32) -> Option<f32> {
    let mut delays: Vec<u32> = notes.iter().filter(|n| (n.start / rate) % 2 == 1).map(|n| n.start % rate).filter(|d| (1..=rate / 2).contains(d)).collect();
    delays.sort_unstable();
    let d = delays.chunk_by(|a, b| a == b).max_by_key(|c| c.len())?[0];
    let exact = d as f32 * 2.0 / rate as f32;
    [(exact * 100.0).round() / 100.0, exact, (d as f32 + 0.5) * 2.0 / rate as f32]
        .into_iter()
        .find(|s| *s <= 1.0 && swing_delay(1, rate, *s) == d)
}

/// One row of `rate`-tick steps over `span` ticks, swung by `swing`, that plays `notes`, all
/// of one pitch and sorted by start.
fn row_from_notes(pitch: u8, notes: &[Note], rate: u32, span: u32, swing: f32) -> PatternRow {
    let length = span.div_ceil(rate).clamp(1, u16::MAX as u32 + 1);
    let grid = |c: u32| c as i64 * rate as i64 + swing_delay(c, rate, swing) as i64;
    let mut row = PatternRow::new(pitch);
    let mut i = 0;
    while i < notes.len() {
        let n = notes[i];
        // A ratchet: hits at even spacing through the step, alike but for a last one cut
        // short by the next note.
        let repeat = (2..=8u8)
            .rev()
            .find(|&r| {
                let r = r as usize;
                let cut = |m: Note| m.length < n.length && m.start + m.length == notes.get(i + r).map_or(span, |x| x.start);
                i + r <= notes.len()
                    && (1..r).all(|j| {
                        let m = notes[i + j];
                        m.start == n.start + j as u32 * rate / r as u32
                            && (m.length == n.length || j + 1 == r && cut(m))
                            && (m.velocity, m.chance) == (n.velocity, n.chance)
                    })
            })
            .unwrap_or(1);
        i += repeat as usize;
        // The step before the note with a late offset, or the one after with an early one:
        // whichever is free and lands the note exactly, the earlier if both do, so the next
        // note keeps its step. Failing that, the nearer.
        let below = (n.start / rate).min(length - 1);
        let Some((index, offset, _, _)) = [below, below + 1]
            .into_iter()
            .filter(|&c| c < length && row.step(c as u16).is_none())
            .map(|c| {
                let d = n.start as i64 - grid(c);
                let o = (-50..=50i8).min_by_key(|o| ((offset_ticks(*o, rate) - d).abs(), o.unsigned_abs())).unwrap_or(0);
                let exact = (grid(c) + offset_ticks(o, rate)).max(0) == n.start as i64;
                (c, o, !exact, if exact { (c - below) as i64 } else { d.abs() })
            })
            .min_by_key(|x| (x.2, x.3))
        else {
            continue;
        };
        // The bake cuts a note short at the next one, so any gate that reaches it will do.
        let next = notes.get(i).map_or(span, |m| m.start);
        let room = if repeat == 1 { next.saturating_sub(n.start) } else { u32::MAX };
        let gate = (1..=100u8).min_by_key(|g| (hit_length(*g, rate, repeat).min(room).abs_diff(n.length), 100 - g)).unwrap_or(100);
        row.set(Step { index: index as u16, velocity: n.velocity, gate, repeat, chance: n.chance.clamp(1, 100), offset, tie: false });
        let end = (n.start + n.length) as i64;
        let mut tail = ((end as u32).saturating_sub(1) / rate).min(length - 1);
        if tail > index && grid(tail) >= end {
            tail -= 1;
        }
        if repeat == 1 && n.length > rate && tail > index {
            for t in index + 1..=tail {
                if row.step(t as u16).is_some() {
                    break;
                }
                let rest = (end - grid(t)).max(0) as u32;
                let room = (next as i64 - grid(t)).max(0) as u32;
                let gate = if t < tail { 100 } else { (1..=100u8).min_by_key(|g| ((*g as u32 * rate / 100).max(1).min(room).abs_diff(rest), 100 - g)).unwrap_or(100) };
                row.set(Step { gate, tie: true, ..Step::new(t as u16, n.velocity) });
            }
        }
    }
    row
}

/// How many notes of `a`, or of `b` if more, have no exact match in the other.
fn differing(a: &[Note], b: &[Note]) -> usize {
    let key = |n: &&Note| (n.start, n.pitch, n.length, n.velocity, n.chance);
    let mut x: Vec<&Note> = a.iter().collect();
    let mut y: Vec<&Note> = b.iter().collect();
    x.sort_by_key(key);
    y.sort_by_key(key);
    let (mut i, mut j, mut same) = (0, 0, 0);
    while i < x.len() && j < y.len() {
        match key(&x[i]).cmp(&key(&y[j])) {
            std::cmp::Ordering::Equal => {
                same += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    (x.len() - same).max(y.len() - same)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u32 = PPQ / 4;

    fn row(pitch: u8, steps: &[Step]) -> PatternRow {
        PatternRow { steps: steps.to_vec(), ..PatternRow::new(pitch) }
    }

    fn pattern(rows: Vec<PatternRow>) -> Pattern {
        Pattern { rows, ..Pattern::default() }
    }

    fn on(index: u16) -> Step {
        Step::new(index, 100)
    }

    fn hits(r: &PatternRow, len: u32) -> String {
        (0..len).map(|i| if r.step(i as u16).is_some() { 'x' } else { '-' }).collect()
    }

    #[test]
    fn bakes_steps_and_loops_over_the_span() {
        let p = pattern(vec![row(36, &[on(0), on(4)])]);
        let notes = p.bake(32 * S);
        let starts: Vec<u32> = notes.iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![0, 4 * S, 16 * S, 20 * S]);
        assert!(notes.iter().all(|n| n.length == S && n.pitch == 36 && n.chance == 100));
        assert_eq!(p.bake(17 * S).len(), 3);
    }

    #[test]
    fn swing_delays_odd_steps() {
        let mut p = pattern(vec![row(42, &[on(0), on(1), on(2), on(3)])]);
        p.swing = 0.5;
        let starts: Vec<u32> = p.bake(4 * S).iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![0, S + S / 4, 2 * S, 3 * S + S / 4]);
        p.rows[0].swing = Some(0.0);
        assert_eq!(p.bake(4 * S)[1].start, S);
    }

    #[test]
    fn offset_moves_hits_and_clamps_at_zero() {
        let p = pattern(vec![row(38, &[Step { offset: -50, ..on(0) }, Step { offset: 25, ..on(2) }])]);
        let notes = p.bake(16 * S);
        assert_eq!((notes[0].start, notes[1].start), (0, 2 * S + S / 4));
    }

    #[test]
    fn gate_sets_length() {
        let p = pattern(vec![row(38, &[Step { gate: 50, ..on(0) }, Step { gate: 1, ..on(1) }])]);
        let notes = p.bake(16 * S);
        assert_eq!((notes[0].length, notes[1].length), (S / 2, S / 100));
    }

    #[test]
    fn tie_extends_the_previous_step() {
        let p = pattern(vec![row(
            50,
            &[Step { repeat: 2, ..on(0) }, Step { tie: true, repeat: 4, ..on(1) }, Step { tie: true, gate: 50, ..on(2) }, Step { tie: true, ..on(5) }],
        )]);
        let notes = p.bake(16 * S);
        assert_eq!(notes.len(), 3);
        assert_eq!((notes[0].start, notes[0].length), (0, S / 2));
        assert_eq!((notes[1].start, notes[1].length), (S / 2, 2 * S));
        // A tie with nothing sounding just before plays as a step of its own.
        assert_eq!((notes[2].start, notes[2].length), (5 * S, S));
    }

    #[test]
    fn tie_reaches_across_the_loop() {
        let mut p = pattern(vec![row(50, &[Step { tie: true, ..on(0) }, on(3)])]);
        p.length = 4;
        let notes = p.bake(8 * S);
        assert_eq!(notes.iter().map(|n| (n.start, n.length)).collect::<Vec<_>>(), vec![(0, S), (3 * S, 2 * S), (7 * S, S)]);
    }

    #[test]
    fn ratchets_spread_through_the_step() {
        let p = pattern(vec![row(38, &[Step { repeat: 3, gate: 50, chance: 40, ..on(1) }])]);
        let notes = p.bake(16 * S);
        assert_eq!(notes.iter().map(|n| n.start).collect::<Vec<_>>(), vec![S, S + S / 3, S + 2 * S / 3]);
        assert!(notes.iter().all(|n| n.length == S / 6 && n.chance == 40));
    }

    #[test]
    fn notes_of_one_pitch_never_overlap() {
        let p = pattern(vec![
            row(38, &[Step { offset: 50, ..on(0) }, on(1)]),
            row(38, &[on(1), on(8)]),
            row(40, &[Step { offset: 40, ..on(3) }, Step { offset: -40, ..on(4) }]),
        ]);
        let notes = p.bake(10 * S);
        assert_eq!(notes.iter().filter(|n| n.pitch == 38 && n.start == S).count(), 1);
        for (i, a) in notes.iter().enumerate() {
            assert!(a.start + a.length <= 10 * S);
            for b in &notes[i + 1..] {
                assert!(a.pitch != b.pitch || a.start + a.length <= b.start, "{a:?} overlaps {b:?}");
            }
        }
        let mut sorted = notes.clone();
        sorted.sort_by_key(|n| (n.start, n.pitch));
        assert_eq!(notes, sorted);
    }

    #[test]
    fn rows_loop_on_their_own_length_and_rate() {
        let mut three = row(37, &[on(0)]);
        three.length = Some(3);
        let mut eighths = row(39, &[on(0), on(1)]);
        eighths.rate = Some(2 * S);
        eighths.length = Some(2);
        let p = pattern(vec![row(36, &[on(0)]), three, eighths]);
        let starts = |pitch| p.bake(16 * S).iter().filter(|n| n.pitch == pitch).map(|n| n.start).collect::<Vec<_>>();
        assert_eq!(starts(36), vec![0]);
        assert_eq!(starts(37), (0..6).map(|i| i * 3 * S).collect::<Vec<_>>());
        assert_eq!(starts(39), (0..8).map(|i| i * 2 * S).collect::<Vec<_>>());
    }

    #[test]
    fn steps_past_a_row_length_stay_silent() {
        let mut r = row(36, &[on(0), on(5)]);
        r.length = Some(4);
        assert_eq!(pattern(vec![r]).bake(4 * S).len(), 1);
    }

    #[test]
    fn from_notes_round_trips_a_bake() {
        let mut swung = row(42, &(0..16).map(on).collect::<Vec<_>>());
        swung.swing = Some(0.5);
        let mut short = row(37, &[on(0), Step { velocity: 70, ..on(2) }]);
        short.length = Some(3);
        let p = pattern(vec![
            row(36, &[on(0), Step { velocity: 90, gate: 30, ..on(6) }, Step { offset: -20, chance: 55, ..on(10) }]),
            row(38, &[Step { repeat: 3, gate: 80, ..on(4) }, Step { repeat: 2, offset: 10, ..on(12) }]),
            row(50, &[on(1), Step { tie: true, ..on(2) }, Step { tie: true, gate: 40, ..on(3) }]),
            swung,
            short,
        ]);
        let span = 32 * S;
        let notes = p.bake(span);
        let (q, moved) = Pattern::from_notes(&notes, S, span);
        assert_eq!(moved, 0);
        assert_eq!(q.bake(span), notes);
    }

    #[test]
    fn swung_and_offset_patterns_convert_back() {
        let mut rng = 12345;
        let mut r = |n: u32| xorshift(&mut rng) % n;
        let span = 16 * S;
        let (mut lossy, mut tried) = ([0; 2], [0; 2]);
        for _ in 0..4000 {
            let offsets = r(2) == 1;
            tried[offsets as usize] += 1;
            let mut p = Pattern { swing: r(101) as f32 / 100.0, ..Pattern::default() };
            for pitch in [36, 38, 42].into_iter().take(1 + r(3) as usize) {
                let mut row = PatternRow::new(pitch);
                for i in 0..16 {
                    if r(100) >= 40 {
                        continue;
                    }
                    let mut s = Step::new(i, 1 + r(127) as u8);
                    if r(4) == 0 {
                        s.gate = 1 + r(100) as u8;
                    }
                    if offsets && r(3) == 0 {
                        s.offset = r(101) as i8 - 50;
                    }
                    row.set(s);
                }
                p.rows.push(row);
            }
            let notes = p.bake(span);
            let (q, moved) = Pattern::from_notes(&notes, S, span);
            assert_eq!(moved == 0, q.bake(span) == notes);
            if moved > 0 {
                lossy[offsets as usize] += 1;
            }
        }
        assert_eq!(lossy[0], 0);
        // Where every odd step is offset, the swing can only be told apart to a whole percent
        // of a step, and big offsets can swap two hits round, which no pattern plays back.
        assert!(lossy[1] * 25 < tried[1], "{lossy:?} of {tried:?}");
    }

    #[test]
    fn from_notes_counts_what_moves() {
        let notes = vec![Note::new(0, S, 36, 100), Note::new(2 * S + 5, S, 36, 100), Note::new(S + 9, 12, 38, 100), Note::new(40 * S, S, 36, 100)];
        let (p, moved) = Pattern::from_notes(&notes, S, 16 * S);
        // No whole percent of a step is 5 ticks, and the last note is past the span.
        assert_eq!(moved, 2);
        assert_eq!(p.length, 16);
        assert_eq!(p.rows.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![36, 38]);
        assert_eq!(p.rows[0].steps[1].offset, 2);
        let (_, none) = Pattern::from_notes(&[], S, 16 * S);
        assert_eq!(none, 0);
    }

    #[test]
    fn row_ops() {
        let mut r = row(36, &[on(0), on(3)]);
        r.shift(-1, 8);
        assert_eq!(hits(&r, 8), "--x----x");
        r.shift(1, 8);
        assert_eq!(hits(&r, 8), "x--x----");
        r.reverse(8);
        assert_eq!(hits(&r, 8), "----x--x");
        r.clear();
        r.fill_every(3, 1, 8);
        assert_eq!(hits(&r, 8), "-x--x--x");
        r.euclid(3, 8, 8);
        assert_eq!(hits(&r, 8), "x--x--x-");
        r.euclid(3, 4, 8);
        assert_eq!(hits(&r, 8), "x-xxx-xx");
        r.randomise(7, 50, 16);
        let first = r.clone();
        r.randomise(7, 50, 16);
        assert_eq!(r, first);
        assert!((1..16).contains(&r.steps.len()));
        r.randomise(7, 0, 16);
        assert!(r.steps.is_empty());
    }

    #[test]
    fn reverse_keeps_a_tied_note_whole() {
        let mut r = row(50, &[Step { velocity: 80, ..on(1) }, Step { tie: true, ..on(2) }, Step { tie: true, ..on(3) }]);
        r.reverse(8);
        assert_eq!(hits(&r, 8), "----xxx-");
        assert_eq!((r.steps[0].tie, r.steps[0].velocity), (false, 80));
        assert!(r.steps[1].tie && r.steps[2].tie);
        let notes = pattern(vec![r]).bake(8 * S);
        assert_eq!(notes.iter().map(|n| (n.start, n.length, n.velocity)).collect::<Vec<_>>(), vec![(4 * S, 3 * S, 80)]);
    }

    #[test]
    fn reverse_keeps_a_tie_across_the_loop_whole() {
        // Step 6 is held through 7 and on into step 0 of the next loop.
        let mut r = row(50, &[Step { tie: true, gate: 50, ..on(0) }, Step { velocity: 80, ..on(6) }, Step { tie: true, ..on(7) }]);
        r.length = Some(8);
        let second_loop = |r: &PatternRow| {
            let notes = pattern(vec![r.clone()]).bake(24 * S);
            notes.iter().filter(|n| (8 * S..16 * S).contains(&n.start)).map(|n| (n.start - 8 * S, n.length, n.velocity)).collect::<Vec<_>>()
        };
        assert_eq!(second_loop(&r), vec![(6 * S, 2 * S + S / 2, 80)]);
        assert!(r.joins(0, 8) && r.joins(7, 8) && !r.joins(6, 8));
        r.reverse(8);
        assert_eq!(hits(&r, 8), "xx-----x");
        assert_eq!((r.steps[2].tie, r.steps[2].velocity), (false, 80));
        assert!(r.steps[0].tie && r.steps[1].tie && r.steps[1].gate == 50);
        assert_eq!(second_loop(&r), vec![(7 * S, 2 * S + S / 2, 80)]);
    }

    #[test]
    fn double_copies_rows_on_the_pattern_length() {
        let mut own = row(37, &[on(0)]);
        own.length = Some(3);
        let mut p = pattern(vec![row(36, &[on(1), on(9)]), own]);
        p.double();
        assert_eq!(p.length, 32);
        assert_eq!(p.rows[0].steps.iter().map(|s| s.index).collect::<Vec<_>>(), vec![1, 9, 17, 25]);
        assert_eq!(p.rows[1].steps.len(), 1);
    }

    #[test]
    fn xorshift_never_sticks() {
        let mut s = 1;
        let a = xorshift(&mut s);
        assert_ne!(a, 0);
        assert_ne!(xorshift(&mut s), a);
    }

    #[test]
    fn steps_save_only_what_differs_from_the_defaults() {
        let p = pattern(vec![row(36, &[on(0), Step { chance: 50, tie: true, ..on(1) }])]);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("gate") && !json.contains("repeat") && !json.contains("offset"));
        assert_eq!(json.matches("chance").count(), 1);
        assert_eq!(serde_json::from_str::<Pattern>(&json).unwrap(), p);
    }
}

