use super::*;

/// One cycle of a Solstice Synth waveform: 0 saw, 1 square, 2 triangle, 3 sine.
fn wave(kind: u32, phase: f32) -> f32 {
    match kind {
        0 => 1.0 - 2.0 * phase,
        1 => if phase < 0.5 { 1.0 } else { -1.0 },
        2 => 1.0 - 4.0 * (phase - 0.5).abs(),
        _ => (phase * std::f32::consts::TAU).sin(),
    }
}

/// An ADSR shape. Times are drawn on a square-root scale so short and long both read well.
#[allow(clippy::too_many_arguments)]
fn adsr(c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32, a: f32, d: f32, sustain: f32, r: f32) {
    let (l, rt, t, b) = (10.0, w - 10.0, 10.0, h - 10.0);
    let (a, d, r) = (a.sqrt(), d.sqrt(), r.sqrt());
    let hold = 18.0;
    let unit = (rt - l - 3.0 * hold) / (a + d + r).max(1.0);
    let x1 = l + a * unit + hold;
    let x2 = x1 + d * unit + hold;
    let x3 = x2 + hold;
    let x4 = x3 + r * unit + hold;
    let sy = b - (b - t) * sustain;
    let mut pts = vec![(l, b)];
    for k in 1..=16 {
        let f = k as f32 / 16.0;
        pts.push((l + (x1 - hold - l) * f, b - (b - t) * f));
    }
    pts.push((x1, t));
    for k in 1..=16 {
        let f = k as f32 / 16.0;
        pts.push((x1 + (x2 - hold - x1) * f, t + (sy - t) * (1.0 - (-4.0 * f).exp()) / (1.0 - (-4f32).exp())));
    }
    pts.push((x3, sy));
    for k in 1..=16 {
        let f = k as f32 / 16.0;
        pts.push((x3 + (x4 - hold - x3) * f, sy + (b - sy) * (1.0 - (-4.0 * f).exp()) / (1.0 - (-4f32).exp())));
    }
    pts.push((x4, b));
    fill_under(s, &pts, b, &alpha(&canvas::accent(c), 0.15));
    stroke(s, &pts, 2.0, &canvas::accent(c));
    for (x, name) in [(l, "A"), (x1, "D"), (x2, "S"), (x3, "R")] {
        label(s, c, x + 3.0, t - 2.0, name, 0.45);
    }
}

pub fn synth(b: &Arc<Bank>) -> gtk::Widget {
    let osc = {
        let b = b.clone();
        picture(100, move |c, s, w, h| {
            // One lane per oscillator, faded by how much of it the mix lets through; Osc 2 is
            // drawn at its pitch relative to Osc 1.
            let (w1, w2, semis, mix) = (b.get(0) as u32, b.get(1) as u32, b.get(2).round(), b.get(4) / 100.0);
            let (l, r) = (60.0, w - 8.0);
            let lane = (h - 12.0) / 2.0;
            let ratio = 2f32.powf(semis / 12.0);
            let fg = canvas::fg(c);
            for (k, (kind, speed, level, name)) in [(w1, 1.0, 1.0 - mix, "Osc 1"), (w2, ratio, mix, "Osc 2")].into_iter().enumerate() {
                let mid = 6.0 + lane * (k as f32 + 0.5);
                let amp = lane / 2.0 - 5.0;
                let pts: Vec<(f32, f32)> = (0..=240)
                    .map(|i| {
                        let f = i as f32 / 240.0;
                        (l + (r - l) * f, mid - amp * wave(kind, (f * 2.0 * speed).fract()))
                    })
                    .collect();
                canvas::rect(s, l, mid, r - l, 1.0, &alpha(&fg, 0.12));
                stroke(s, &pts, 2.0, &alpha(&canvas::accent(c), 0.25 + 0.75 * level));
                label(s, c, 10.0, mid - 6.0, name, 0.6);
            }
        })
    };
    // The low-pass response of the synth's state-variable filter: k = 2 - 1.9 × resonance.
    let filter = {
        let b = b.clone();
        picture(80, move |c, s, w, h| {
            let (fc, res) = (b.get(5), b.get(6) / 100.0);
            let k = 2.0 - 1.9 * res;
            let (l, r, t, bot) = (8.0, w - 8.0, 8.0, h - 8.0);
            let y = |db: f32| t + (bot - t) * ((24.0 - db) / 60.0).clamp(0.0, 1.0);
            let pts: Vec<(f32, f32)> = (0..=160)
                .map(|i| {
                    let f = 20.0 * 1000f32.powf(i as f32 / 160.0);
                    let x = f / fc;
                    let mag = 1.0 / ((1.0 - x * x).powi(2) + (k * x).powi(2)).sqrt();
                    (l + (r - l) * i as f32 / 160.0, y(20.0 * mag.log10()))
                })
                .collect();
            canvas::rect(s, l, y(0.0), r - l, 1.0, &alpha(&canvas::fg(c), 0.12));
            fill_under(s, &pts, bot, &alpha(&canvas::accent(c), 0.15));
            stroke(s, &pts, 2.0, &canvas::accent(c));
            label_right(s, c, r - 2.0, t, &b.text(5, fc), 0.7);
        })
    };
    let fenv = {
        let b = b.clone();
        picture(80, move |c, s, w, h| adsr(c, s, w, h, b.get(8), b.get(9), b.get(10) / 100.0, b.get(11)))
    };
    let aenv = {
        let b = b.clone();
        picture(80, move |c, s, w, h| adsr(c, s, w, h, b.get(12), b.get(13), b.get(14) / 100.0, b.get(15)))
    };
    let lfo = {
        let b = b.clone();
        picture(80, move |c, s, w, h| {
            let rate = b.get(16);
            let depth = (b.get(17) / 100.0).max(b.get(18) / 100.0).max(0.1);
            let (l, r, mid, amp) = (8.0, w - 8.0, h / 2.0, (h / 2.0 - 10.0) * depth);
            // A one-second window, so the rate reads as cycles across it.
            let cycles = rate.min(12.0);
            let pts: Vec<(f32, f32)> =
                (0..=200).map(|i| i as f32 / 200.0).map(|f| (l + (r - l) * f, mid - amp * (f * cycles * std::f32::consts::TAU).sin())).collect();
            canvas::rect(s, l, mid, r - l, 1.0, &alpha(&canvas::fg(c), 0.12));
            stroke(s, &pts, 2.0, &canvas::accent(c));
            label_right(s, c, r - 2.0, 4.0, "1 s", 0.5);
        })
    };
    let grid = gtk::Grid::builder().row_spacing(12).column_spacing(12).column_homogeneous(true).build();
    grid.attach(&section("Oscillators", Some(&osc), b, &[0, 1, 2, 3, 4]), 0, 0, 2, 1);
    grid.attach(&section("Filter", Some(&filter), b, &[5, 6, 7]), 0, 1, 1, 1);
    grid.attach(&section("LFO", Some(&lfo), b, &[16, 17, 18]), 1, 1, 1, 1);
    grid.attach(&section("Filter Envelope", Some(&fenv), b, &[8, 9, 10, 11]), 0, 2, 1, 1);
    grid.attach(&section("Amp Envelope", Some(&aenv), b, &[12, 13, 14, 15]), 1, 2, 1, 1);
    grid.attach(&section("Output", None, b, &[19, 20]), 0, 3, 2, 1);
    grid.upcast()
}

/// The Controls page of the Sampler and Drum Kit.
pub fn sampler(b: &Arc<Bank>) -> gtk::Widget {
    let env = {
        let b = b.clone();
        picture(110, move |c, s, w, h| adsr(c, s, w, h, b.get(0), 1.0, 1.0, b.get(1)))
    };
    // How loud each velocity plays: gain = 1 - amount + amount × velocity / 127.
    let vel = {
        let b = b.clone();
        picture(110, move |c, s, w, h| {
            let amount = b.get(3) / 100.0;
            let (l, r, t, bot) = (10.0, w - 10.0, 10.0, h - 18.0);
            let pts: Vec<(f32, f32)> =
                (0..=127).map(|v| (l + (r - l) * v as f32 / 127.0, bot - (bot - t) * (1.0 - amount + amount * v as f32 / 127.0))).collect();
            fill_under(s, &pts, bot, &alpha(&canvas::accent(c), 0.15));
            stroke(s, &pts, 2.0, &canvas::accent(c));
            label(s, c, l, bot + 2.0, "Soft", 0.5);
            label_right(s, c, r, bot + 2.0, "Hard", 0.5);
        })
    };
    columns(&[section("Envelope", Some(&env), b, &[0, 1]), section("Output", Some(&vel), b, &[2, 3])]).upcast()
}
