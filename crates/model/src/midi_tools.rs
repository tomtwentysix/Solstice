//! Note functions for the piano roll's Functions menu.
//! Each takes the notes to change and returns them changed, so the caller can keep its
//! selection pointing at the results.

use crate::{Groove, Note, xorshift};

/// Pulls note starts towards the grid. `strength` 1.0 snaps fully; `swing` 0.0–1.0 delays every
/// second grid line by up to half a grid step.
pub fn quantize(notes: &[Note], grid: u32, strength: f32, swing: f32) -> Vec<Note> {
    let grid = grid.max(1);
    notes
        .iter()
        .map(|n| {
            let index = (n.start as f64 / grid as f64).round() as i64;
            let mut target = index as f64 * grid as f64;
            if index % 2 == 1 {
                target += swing.clamp(0.0, 1.0) as f64 * grid as f64 / 2.0;
            }
            let moved = n.start as f64 + (target - n.start as f64) * strength.clamp(0.0, 1.0) as f64;
            Note { start: moved.round().max(0.0) as u32, ..*n }
        })
        .collect()
}

/// `quantize` with a grid per pitch; pitches whose grid is 0 are left where they are.
pub fn quantize_rows(notes: &[Note], grid: impl Fn(u8) -> u32, strength: f32, swing: f32) -> Vec<Note> {
    notes
        .iter()
        .map(|n| match grid(n.pitch) {
            0 => *n,
            g => quantize(std::slice::from_ref(n), g, strength, swing)[0],
        })
        .collect()
}

/// Moves each start by up to `max_ticks` either way and scales each velocity by up to
/// `velocity` (0 to 1) either way, the same way every time for the same `seed`.
pub fn humanise(notes: &[Note], max_ticks: u32, velocity: f32, seed: u32) -> Vec<Note> {
    let mut state = seed.max(1);
    let mut unit = move || xorshift(&mut state) as f64 / u32::MAX as f64 * 2.0 - 1.0;
    notes
        .iter()
        .map(|n| {
            let start = (n.start as f64 + unit() * max_ticks as f64).round().max(0.0) as u32;
            let vel = (n.velocity as f64 * (1.0 + unit() * velocity.clamp(0.0, 1.0) as f64)).round().clamp(1.0, 127.0) as u8;
            Note { start, velocity: vel, ..*n }
        })
        .collect()
}

/// The hits a paint stroke from `from` to `to` adds on `pitch`: one on every `grid` step the
/// stroke touches that starts before `end` and has no hit on `pitch` within half a step of it.
#[allow(clippy::too_many_arguments)]
pub fn paint(existing: &[Note], pitch: u8, from: u32, to: u32, grid: u32, length: u32, velocity: u8, end: u32) -> Vec<Note> {
    let grid = grid.max(1);
    let (a, b) = (from.min(to) / grid, from.max(to) / grid);
    (a..=b)
        .map(|k| k * grid)
        .filter(|t| *t < end && !existing.iter().any(|n| n.pitch == pitch && (n.start + grid / 2).checked_sub(*t).is_some_and(|d| d < grid)))
        .map(|t| Note::new(t, length.max(1), pitch, velocity))
        .collect()
}

/// The hits on `pitch` an erase stroke from `from` to `to` passes over, within `tolerance`.
pub fn erase(existing: &[Note], pitch: u8, from: u32, to: u32, tolerance: u32) -> Vec<Note> {
    let (a, b) = (from.min(to).saturating_sub(tolerance), from.max(to) + tolerance);
    existing.iter().filter(|n| n.pitch == pitch && (a..=b).contains(&n.start)).copied().collect()
}

/// Pulls note starts towards the nearest hit of a repeating groove by `strength`, and blends
/// velocities towards the hit's by `velocity` (0 leaves them alone).
pub fn groove_quantize(notes: &[Note], groove: &Groove, strength: f32, velocity: f32) -> Vec<Note> {
    let (Some(first), Some(last)) = (groove.hits.first(), groove.hits.last()) else {
        return notes.to_vec();
    };
    let length = groove.length.max(1) as i64;
    let strength = strength.clamp(0.0, 1.0) as f64;
    let velocity = velocity.clamp(0.0, 1.0) as f64;
    notes
        .iter()
        .map(|n| {
            let pos = n.start as i64 % length;
            let cycle = n.start as i64 - pos;
            let candidates = groove
                .hits
                .iter()
                .map(|h| (h.pos as i64, h))
                .chain([(first.pos as i64 + length, first), (last.pos as i64 - length, last)]);
            let (target, hit) = candidates.min_by_key(|(p, _)| (p - pos).abs()).unwrap();
            let target = (cycle + target) as f64;
            let moved = n.start as f64 + (target - n.start as f64) * strength;
            let vel = n.velocity as f64 + (hit.velocity as f64 - n.velocity as f64) * velocity;
            Note { start: moved.round().max(0.0) as u32, velocity: vel.round().clamp(1.0, 127.0) as u8, ..*n }
        })
        .collect()
}

/// Extends each note to the start of the next later note; the last note keeps its length.
pub fn legato(notes: &[Note]) -> Vec<Note> {
    let mut starts: Vec<u32> = notes.iter().map(|n| n.start).collect();
    starts.sort_unstable();
    starts.dedup();
    notes
        .iter()
        .map(|n| match starts.iter().find(|s| **s > n.start) {
            Some(next) => Note { length: next - n.start, ..*n },
            None => *n,
        })
        .collect()
}

pub fn fixed_length(notes: &[Note], length: u32) -> Vec<Note> {
    notes.iter().map(|n| Note { length: length.max(1), ..*n }).collect()
}

/// Moves notes by semitones, leaving any that would leave the MIDI range where they are.
pub fn transpose(notes: &[Note], by: i32) -> Vec<Note> {
    let fits = notes.iter().all(|n| (0..=127).contains(&(n.pitch as i32 + by)));
    if !fits {
        return notes.to_vec();
    }
    notes.iter().map(|n| Note { pitch: (n.pitch as i32 + by) as u8, ..*n }).collect()
}

/// Moves notes in time by `ticks`, stopping at the region start.
pub fn shift(notes: &[Note], ticks: i64) -> Vec<Note> {
    let min = notes.iter().map(|n| n.start as i64).min().unwrap_or(0);
    let ticks = ticks.max(-min);
    notes.iter().map(|n| Note { start: (n.start as i64 + ticks) as u32, ..*n }).collect()
}

/// Replaces `old` notes in `all` with `new` (same order), returning the new ones.
pub fn replace(all: &mut Vec<Note>, old: &[Note], new: &[Note]) -> Vec<Note> {
    for (o, n) in old.iter().zip(new) {
        if let Some(slot) = all.iter_mut().find(|x| *x == o) {
            *slot = *n;
        }
    }
    all.sort_by_key(|n| (n.start, n.pitch));
    new.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GrooveHit;

    fn n(start: u32, length: u32, pitch: u8) -> Note {
        Note::new(start, length, pitch, 100)
    }

    #[test]
    fn rows_quantize_to_their_own_grid_and_off_rows_stay() {
        let notes = [n(130, 10, 36), n(130, 10, 42), n(130, 10, 49)];
        let grid = |p: u8| match p {
            36 => 240,
            42 => 320,
            _ => 0,
        };
        let q = quantize_rows(&notes, grid, 1.0, 0.0);
        assert_eq!(q.iter().map(|n| n.start).collect::<Vec<_>>(), [240, 0, 130]);
    }

    #[test]
    fn iterative_quantize_moves_part_of_the_way() {
        let q = quantize_rows(&[n(200, 10, 36)], |_| 240, 0.5, 0.0);
        assert_eq!(q[0].start, 220);
    }

    #[test]
    fn humanise_is_repeatable_and_bounded() {
        let notes: Vec<Note> = (0..50).map(|i| n(1000 + i * 240, 10, 38)).collect();
        let a = humanise(&notes, 20, 0.1, 7);
        assert_eq!(a, humanise(&notes, 20, 0.1, 7));
        for (o, h) in notes.iter().zip(&a) {
            assert!((h.start as i64 - o.start as i64).abs() <= 20);
            assert!((90..=110).contains(&h.velocity));
        }
        assert_ne!(a, notes);
    }

    #[test]
    fn painting_fills_empty_steps_up_to_the_region_end() {
        let existing = [n(240, 10, 36)];
        let hits = paint(&existing, 36, 10, 1000, 240, 240, 90, 900);
        assert_eq!(hits.iter().map(|n| n.start).collect::<Vec<_>>(), [0, 480, 720]);
        assert!(hits.iter().all(|n| n.pitch == 36 && n.velocity == 90 && n.length == 240));
        assert_eq!(paint(&existing, 36, 1000, 10, 240, 240, 90, 900), hits);
        assert_eq!(paint(&existing, 38, 250, 250, 240, 240, 90, 900).len(), 1);
    }

    #[test]
    fn painting_skips_steps_whose_hit_sits_early() {
        let existing = [n(236, 10, 36)];
        let hits = paint(&existing, 36, 0, 500, 240, 240, 90, 900);
        assert_eq!(hits.iter().map(|n| n.start).collect::<Vec<_>>(), [0, 480]);
    }

    #[test]
    fn erasing_takes_hits_on_one_row_only() {
        let existing = [n(0, 10, 36), n(240, 10, 36), n(240, 10, 38), n(960, 10, 36)];
        assert_eq!(erase(&existing, 36, 0, 300, 5), [n(0, 10, 36), n(240, 10, 36)]);
        assert_eq!(erase(&existing, 36, 300, 3, 5), [n(0, 10, 36), n(240, 10, 36)]);
    }

    #[test]
    fn quantize_strength_and_swing() {
        let notes = [n(10, 100, 60), n(250, 100, 62)];
        let q = quantize(&notes, 240, 1.0, 0.0);
        assert_eq!((q[0].start, q[1].start), (0, 240));
        let half = quantize(&notes, 240, 0.5, 0.0);
        assert_eq!((half[0].start, half[1].start), (5, 245));
        // The second grid line is the off-beat: full swing pushes it half a step late.
        let swung = quantize(&notes, 240, 1.0, 1.0);
        assert_eq!(swung[1].start, 360);
    }

    fn swing16() -> Groove {
        Groove { name: "Swing".into(), length: 480, hits: vec![
            GrooveHit { pos: 0, velocity: 120 },
            GrooveHit { pos: 316, velocity: 60 },
        ] }
    }

    #[test]
    fn groove_moves_off_sixteenths() {
        let notes = [n(0, 10, 60), n(240, 10, 60), n(480, 10, 60), n(720, 10, 60)];
        let q = groove_quantize(&notes, &swing16(), 1.0, 0.0);
        assert_eq!(q.iter().map(|x| x.start).collect::<Vec<_>>(), vec![0, 316, 480, 796]);
        assert!(q.iter().all(|x| x.velocity == 100));
        assert_eq!(groove_quantize(&notes, &swing16(), 0.0, 0.0), notes.to_vec());
    }

    #[test]
    fn groove_wraps_to_next_cycle() {
        let q = groove_quantize(&[n(470, 10, 60)], &swing16(), 1.0, 0.0);
        assert_eq!(q[0].start, 480);
        let g = Groove { name: "Late".into(), length: 480, hits: vec![GrooveHit { pos: 20, velocity: 100 }, GrooveHit { pos: 240, velocity: 100 }] };
        assert_eq!(groove_quantize(&[n(470, 10, 60)], &g, 1.0, 0.0)[0].start, 500);
        assert_eq!(groove_quantize(&[n(5, 10, 60)], &g, 1.0, 0.0)[0].start, 20);
    }

    #[test]
    fn groove_blends_velocity() {
        let q = groove_quantize(&[n(0, 10, 60), n(240, 10, 60)], &swing16(), 1.0, 0.5);
        assert_eq!((q[0].velocity, q[1].velocity), (110, 80));
        let empty = Groove { name: "Empty".into(), length: 480, hits: Vec::new() };
        assert_eq!(groove_quantize(&[n(7, 10, 60)], &empty, 1.0, 1.0)[0].start, 7);
    }

    #[test]
    fn legato_and_lengths() {
        let notes = [n(0, 10, 60), n(0, 10, 64), n(480, 10, 62)];
        let l = legato(&notes);
        assert_eq!((l[0].length, l[1].length, l[2].length), (480, 480, 10));
        assert!(fixed_length(&notes, 120).iter().all(|x| x.length == 120));
    }

    #[test]
    fn transpose_and_shift() {
        let notes = [n(100, 10, 60), n(200, 10, 126)];
        assert_eq!(transpose(&notes, 1)[1].pitch, 127);
        assert_eq!(transpose(&notes, 2), notes.to_vec());
        let s = shift(&notes, -500);
        assert_eq!((s[0].start, s[1].start), (0, 100));
    }

    #[test]
    fn replace_keeps_order() {
        let mut all = vec![n(0, 10, 60), n(100, 10, 62)];
        let old = [all[1]];
        let new = [n(50, 10, 62)];
        replace(&mut all, &old, &new);
        assert_eq!(all.iter().map(|x| x.start).collect::<Vec<_>>(), vec![0, 50]);
    }
}
