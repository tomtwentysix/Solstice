use super::*;
use solstice_dsp::midifx::{SCALES, chord_notes};

const NAMES: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];

fn note_name(n: i32) -> String {
    format!("{}{}", NAMES[n.rem_euclid(12) as usize], n.div_euclid(12) - 1)
}

fn is_black(n: i32) -> bool {
    matches!(n.rem_euclid(12), 1 | 3 | 6 | 8 | 10)
}

/// A keyboard from `first` to `last` (both C), with `lit` keys in the accent colour and `marked`
/// keys outlined.
#[allow(clippy::too_many_arguments)]
fn keyboard(c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32, first: i32, last: i32, lit: &[i32], marked: &[i32]) {
    let accent = canvas::accent(c);
    let whites: Vec<i32> = (first..=last).filter(|n| !is_black(*n)).collect();
    let (l, t, b) = (8.0, 8.0, h - 8.0);
    let kw = (w - 16.0) / whites.len() as f32;
    let x_of = |n: i32| l + whites.iter().position(|m| *m == n).unwrap_or(0) as f32 * kw;
    for n in &whites {
        let x = x_of(*n);
        let col = if lit.contains(n) { accent } else { gdk::RGBA::new(0.92, 0.92, 0.92, 1.0) };
        rounded(s, x + 1.0, t, kw - 2.0, b - t, 3.0, &col);
        if marked.contains(n) {
            rounded(s, x + kw / 2.0 - 4.0, b - 14.0, 8.0, 8.0, 4.0, &canvas::rgba(0xe5a50a, 1.0));
        }
        if n.rem_euclid(12) == 0 {
            text(s, c, x + 4.0, b - 16.0, &note_name(*n), &gdk::RGBA::new(0.0, 0.0, 0.0, 0.5), Some(7.0));
        }
    }
    for n in (first..=last).filter(|n| is_black(*n)) {
        let x = x_of(n - 1) + kw * 0.68;
        let col = if lit.contains(&n) { accent } else { gdk::RGBA::new(0.12, 0.12, 0.12, 1.0) };
        rounded(s, x, t, kw * 0.64, (b - t) * 0.6, 2.0, &col);
        if marked.contains(&n) {
            rounded(s, x + kw * 0.32 - 3.0, t + (b - t) * 0.6 - 10.0, 6.0, 6.0, 3.0, &canvas::rgba(0xe5a50a, 1.0));
        }
    }
}

/// The Arpeggiator's order for a held C major chord, as the plug-in picks it.
fn arp_steps(mode: usize, octaves: usize, count: usize) -> Vec<i32> {
    let held = [60, 64, 67];
    let order: Vec<i32> = if mode == 3 { vec![64, 60, 67] } else { held.to_vec() };
    let len = held.len() * octaves;
    let mut rng: u32 = 0x9e37_79b9;
    (0..count)
        .map(|step| {
            let i = match mode {
                1 => len - 1 - step % len,
                2 if len > 1 => {
                    let p = step % (2 * len - 2);
                    if p < len { p } else { 2 * len - 2 - p }
                }
                4 => {
                    rng ^= rng << 13;
                    rng ^= rng >> 17;
                    rng ^= rng << 5;
                    rng as usize % len
                }
                _ => step % len,
            };
            order[i % 3] + 12 * (i / 3) as i32
        })
        .collect()
}

/// Notes on a pitch grid: (step, pitch, length in steps, strength 0..1).
fn note_grid(c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32, steps: usize, notes: &[(f32, i32, f32, f32)]) {
    let fg = canvas::fg(c);
    let lo = notes.iter().map(|n| n.1).min().unwrap_or(60) - 1;
    let hi = notes.iter().map(|n| n.1).max().unwrap_or(72) + 1;
    let (l, r, t, b) = (40.0, w - 10.0, 10.0, h - 10.0);
    let row = (b - t) / (hi - lo + 1) as f32;
    let col = (r - l) / steps as f32;
    for p in lo..=hi {
        let y = b - (p - lo + 1) as f32 * row;
        if is_black(p) {
            canvas::rect(s, l, y, r - l, row, &alpha(&fg, 0.04));
        }
        if p.rem_euclid(12) == 0 {
            label(s, c, 6.0, y + row / 2.0 - 6.0, &note_name(p), 0.5);
        }
    }
    for k in 0..=steps {
        canvas::rect(s, l + k as f32 * col, t, 1.0, b - t, &alpha(&fg, if k % 4 == 0 { 0.12 } else { 0.05 }));
    }
    for (step, pitch, len, strength) in notes {
        let y = b - (pitch - lo + 1) as f32 * row;
        rounded(s, l + step * col + 1.0, y + 1.0, (len * col - 2.0).max(3.0), row - 2.0, 3.0, &alpha(&canvas::accent(c), 0.35 + 0.65 * strength));
    }
}

pub fn arpeggiator(b: &Arc<Bank>) -> gtk::Widget {
    let pattern = {
        let b = b.clone();
        picture(200, move |c, s, w, h| {
            let (mode, octaves, gate) = (b.get(1).round() as usize, b.get(2).round() as usize, b.get(3) / 100.0);
            let notes: Vec<(f32, i32, f32, f32)> = arp_steps(mode, octaves.max(1), 16).into_iter().enumerate().map(|(i, p)| (i as f32, p, gate, 1.0)).collect();
            note_grid(c, s, w, h, 16, &notes);
            label_right(s, c, w - 14.0, 12.0, &format!("Holding C major, {}", b.text(0, b.get(0))), 0.6);
        })
    };
    stack(&[pattern.upcast_ref(), columns(&[section("Pattern", None, b, &[1, 0, 2]), section("Notes", None, b, &[3, 4])]).upcast_ref()]).upcast()
}

pub fn chord_trigger(b: &Arc<Bank>) -> gtk::Widget {
    let keys = {
        let b = b.clone();
        picture(130, move |c, s, w, h| {
            let notes: Vec<i32> = chord_notes(60, b.get(0).round() as usize, b.get(1).round() as usize).map(|n| n as i32).collect();
            keyboard(c, s, w, h - 18.0, 48, 84, &notes, &[60]);
            let names: Vec<String> = notes.iter().map(|n| note_name(*n)).collect();
            label(s, c, 10.0, h - 16.0, &format!("Playing C4 sounds {}", names.join(" ")), 0.7);
        })
    };
    stack(&[keys.upcast_ref(), section("Chord", None, b, &[0, 1]).upcast_ref()]).upcast()
}

pub fn note_repeater(b: &Arc<Bank>) -> gtk::Widget {
    let echoes = {
        let b = b.clone();
        picture(200, move |c, s, w, h| {
            let (repeats, vel, shift) = (b.get(1).round() as i32, b.get(2) / 100.0, b.get(3).round() as i32);
            let mut notes = vec![(0.0, 60, 0.8, 1.0)];
            for r in 1..=repeats {
                let strength = (1.0 + vel * r as f32).clamp(0.05, 1.0);
                notes.push((r as f32 * 2.0, 60 + shift * r, 0.8, strength));
            }
            note_grid(c, s, w, h, (repeats as usize * 2 + 2).max(8), &notes);
            label_right(s, c, w - 14.0, 12.0, &format!("Every {} note", b.text(0, b.get(0))), 0.6);
        })
    };
    stack(&[echoes.upcast_ref(), section("Repeats", None, b, &[0, 1, 2, 3]).upcast_ref()]).upcast()
}

pub fn scale_quantize(b: &Arc<Bank>) -> gtk::Widget {
    let keys = {
        let b = b.clone();
        picture(130, move |c, s, w, h| {
            let (root, scale) = (b.get(0).round() as i32, b.get(1).round() as usize);
            let bits = SCALES[scale.min(SCALES.len() - 1)];
            let lit: Vec<i32> = (48..=84).filter(|n| bits & (1 << (n - root).rem_euclid(12)) != 0).collect();
            let roots: Vec<i32> = (48..=84).filter(|n| (n - root).rem_euclid(12) == 0).collect();
            keyboard(c, s, w, h - 18.0, 48, 84, &lit, &roots);
            label(s, c, 10.0, h - 16.0, &format!("{} {}: other notes move to the nearest of these", NAMES[root as usize % 12], b.text(1, b.get(1))), 0.7);
        })
    };
    stack(&[keys.upcast_ref(), section("Scale", None, b, &[0, 1]).upcast_ref()]).upcast()
}

pub fn transposer(b: &Arc<Bank>) -> gtk::Widget {
    let keys = {
        let b = b.clone();
        picture(130, move |c, s, w, h| {
            let to = 60 + b.get(0).round() as i32;
            keyboard(c, s, w, h - 18.0, 24, 96, &[to], &[60]);
            label(s, c, 10.0, h - 16.0, &format!("C4 plays {}", note_name(to)), 0.7);
        })
    };
    stack(&[keys.upcast_ref(), section("Transpose", None, b, &[0]).upcast_ref()]).upcast()
}

pub fn velocity(b: &Arc<Bank>) -> gtk::Widget {
    // In against out, as the plug-in maps it: scale, offset, then the minimum and maximum.
    let graph = {
        let b = b.clone();
        picture(220, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (l, r, t, bot) = (34.0, w - 12.0, 12.0, h - 22.0);
            let x = |v: f32| l + (r - l) * v / 127.0;
            let y = |v: f32| bot - (bot - t) * v / 127.0;
            for v in [32.0, 64.0, 96.0, 127.0] {
                canvas::rect(s, x(v), t, 1.0, bot - t, &alpha(&fg, 0.06));
                canvas::rect(s, l, y(v), r - l, 1.0, &alpha(&fg, 0.06));
                label_right(s, c, l - 4.0, y(v) - 6.0, &format!("{v:.0}"), 0.45);
                label(s, c, x(v) - 8.0, bot + 4.0, &format!("{v:.0}"), 0.45);
            }
            stroke(s, &[(x(1.0), y(1.0)), (x(127.0), y(127.0))], 1.0, &alpha(&fg, 0.25));
            let (lo, hi) = (b.get(2), b.get(3).max(b.get(2)));
            let pts: Vec<(f32, f32)> = (1..=127)
                .map(|v| {
                    let out = (v as f32 * b.get(0) / 100.0 + b.get(1)).round().clamp(lo, hi).clamp(1.0, 127.0);
                    (x(v as f32), y(out))
                })
                .collect();
            stroke(s, &pts, 2.5, &canvas::accent(c));
            label(s, c, l + 6.0, t + 2.0, "Out", 0.5);
            label_right(s, c, r - 4.0, bot - 16.0, "In", 0.5);
        })
    };
    stack(&[graph.upcast_ref(), columns(&[section("Curve", None, b, &[0, 1]), section("Range", None, b, &[2, 3])]).upcast_ref()]).upcast()
}
