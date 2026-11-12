use super::*;
use solstice_dsp::effects::Compressor;

const FLOOR: f32 = -60.0;

/// An input/output level graph from `FLOOR` to 0 dB, with `out(in)` drawn over a unity line.
fn transfer(c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32, threshold: f32, out: impl Fn(f32) -> f32) {
    let fg = canvas::fg(c);
    let (l, t, r, b) = (34.0, 12.0, w - 12.0, h - 22.0);
    let x = |db: f32| l + (r - l) * (db - FLOOR) / -FLOOR;
    let y = |db: f32| b - (b - t) * (db.clamp(FLOOR, 0.0) - FLOOR) / -FLOOR;
    for db in [-48.0, -36.0, -24.0, -12.0, 0.0] {
        canvas::rect(s, x(db), t, 1.0, b - t, &alpha(&fg, 0.06));
        canvas::rect(s, l, y(db), r - l, 1.0, &alpha(&fg, 0.06));
        label_right(s, c, l - 4.0, y(db) - 6.0, &format!("{db:.0}"), 0.45);
        label(s, c, x(db) - 8.0, b + 4.0, &format!("{db:.0}"), 0.45);
    }
    stroke(s, &[(x(FLOOR), y(FLOOR)), (x(0.0), y(0.0))], 1.0, &alpha(&fg, 0.25));
    canvas::rect(s, x(threshold), t, 1.0, b - t, &canvas::rgba(0xe5a50a, 0.6));
    let pts: Vec<(f32, f32)> = (0..=120).map(|i| FLOOR + i as f32 * 0.5).map(|db| (x(db), y(out(db)))).collect();
    fill_under(s, &pts, b, &alpha(&canvas::accent(c), 0.15));
    stroke(s, &pts, 2.5, &canvas::accent(c));
    label(s, c, l + 6.0, t + 2.0, "Output", 0.5);
    label_right(s, c, r - 4.0, b - 16.0, "Input", 0.5);
}

pub fn compressor(b: &Arc<Bank>) -> gtk::Widget {
    let graph = {
        let b = b.clone();
        picture(220, move |c, s, w, h| {
            let (th, ratio, knee, makeup) = (b.get(0), b.get(1), b.get(4), b.get(5));
            transfer(c, s, w, h, th, |db| db + Compressor::curve(db, th, ratio, knee) + makeup);
            label_right(s, c, w - 16.0, 16.0, &format!("{ratio:.1}:1"), 0.8);
        })
    };
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&graph);
    top.append(&reduction_meter("GR", b, 24.0));
    stack(&[
        top.upcast_ref(),
        columns(&[section("Threshold", None, b, &[0, 1, 4]), section("Timing", None, b, &[2, 3]), section("Output", None, b, &[5])])
            .upcast_ref(),
    ])
    .upcast()
}

pub fn limiter(b: &Arc<Bank>) -> gtk::Widget {
    let graph = {
        let b = b.clone();
        picture(200, move |c, s, w, h| {
            let (gain, ceiling) = (b.get(0), b.get(1));
            transfer(c, s, w, h, ceiling - gain, |db| (db + gain).min(ceiling));
            label_right(s, c, w - 16.0, 16.0, &format!("Ceiling {ceiling:.1} dB"), 0.8);
        })
    };
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&graph);
    top.append(&reduction_meter("GR", b, 24.0));
    stack(&[top.upcast_ref(), section("Limiter", None, b, &[0, 1, 2]).upcast_ref()]).upcast()
}

pub fn gate(b: &Arc<Bank>) -> gtk::Widget {
    let graph = {
        let b = b.clone();
        picture(200, move |c, s, w, h| {
            let (th, floor) = (b.get(0), b.get(1));
            transfer(c, s, w, h, th, |db| if db >= th { db } else { db + floor });
            let open = b.meter() > -1.0;
            let col = if open { canvas::rgba(0x33d17a, 0.9) } else { alpha(&canvas::fg(c), 0.25) };
            rounded(s, w - 70.0, 10.0, 56.0, 18.0, 9.0, &col);
            label(s, c, w - 58.0, 12.0, if open { "Open" } else { "Closed" }, 0.9);
        })
    };
    // Attack, hold and release as the gate's envelope around one burst of signal.
    let envelope = {
        let b = b.clone();
        picture(70, move |c, s, w, h| {
            let (att, hold, rel, floor) = (b.get(2), b.get(3), b.get(4), b.get(1));
            let (l, r, t, bot) = (12.0, w - 12.0, 10.0, h - 10.0);
            let lo_y = t + (bot - t) * (floor / -80.0).clamp(0.0, 1.0);
            let total = att + 200.0 + hold + rel * 3.0;
            let x = |ms: f32| l + (r - l) * ms / total;
            let mut pts = vec![(x(0.0), lo_y)];
            for k in 0..=20 {
                let f = k as f32 / 20.0;
                pts.push((x(att * f * 3.0).min(x(att * 3.0)), lo_y + (t - lo_y) * (1.0 - (-3.0 * f).exp())));
            }
            pts.push((x(att * 3.0 + 200.0 + hold), t));
            for k in 0..=30 {
                let f = k as f32 / 30.0;
                pts.push((x(att * 3.0 + 200.0 + hold + rel * 3.0 * f), t + (lo_y - t) * (1.0 - (-3.0 * f).exp())));
            }
            fill_under(s, &pts, bot, &alpha(&canvas::accent(c), 0.15));
            stroke(s, &pts, 2.0, &canvas::accent(c));
            canvas::rect(s, x(att * 3.0 + 200.0), t, x(att * 3.0 + 200.0 + hold) - x(att * 3.0 + 200.0), 2.0, &canvas::rgba(0xe5a50a, 0.8));
            label(s, c, x(att * 3.0 + 200.0) + 2.0, t + 4.0, "Hold", 0.6);
        })
    };
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    top.append(&graph);
    top.append(&reduction_meter("Gain", b, 80.0));
    stack(&[
        top.upcast_ref(),
        columns(&[section("Threshold", None, b, &[0, 1]), section("Envelope", Some(&envelope), b, &[2, 3, 4])]).upcast_ref(),
    ])
    .upcast()
}
