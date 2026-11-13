use super::*;
use solstice_dsp::effects::SYNC_NOTES;

/// Frequency to x across a 20 Hz – 20 kHz log axis.
pub(super) fn freq_x(f: f32, l: f32, r: f32) -> f32 {
    l + (r - l) * (f.max(20.0) / 20.0).ln() / 1000f32.ln()
}

pub fn delay(b: &Arc<Bank>) -> gtk::Widget {
    // Each repeat as a bar, left channel above the line and right below; ping-pong alternates.
    let taps = {
        let b = b.clone();
        picture(200, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (time, sync, fb, mix, ping, offset) = (b.get(0), b.get(1) as usize, b.get(2) / 100.0, b.get(3) / 100.0, b.get(4) > 0.5, b.get(7));
            let (l, r, mid) = (24.0, w - 16.0, h / 2.0);
            let span = h / 2.0 - 22.0;
            canvas::rect(s, l, mid, r - l, 1.0, &alpha(&fg, 0.2));
            label(s, c, 6.0, mid - span - 2.0, "L", 0.5);
            label(s, c, 6.0, mid + span - 10.0, "R", 0.5);
            let repeats = (1..=16).take_while(|k| fb.powi(*k as i32 - 1) > 0.02 || *k == 1).count().max(1);
            let step = (r - l - 20.0) / (repeats as f32 + 1.0);
            let shift = if sync == 0 { (offset / time.max(1.0)).min(0.5) * step } else { 0.0 };
            canvas::rect(s, l + 2.0, mid - span * (1.0 - mix), 6.0, span * (1.0 - mix), &alpha(&fg, 0.5));
            canvas::rect(s, l + 2.0, mid, 6.0, span * (1.0 - mix), &alpha(&fg, 0.5));
            let accent = canvas::accent(c);
            for k in 1..=repeats {
                let a = mix * fb.powi(k as i32 - 1);
                let x = l + 2.0 + step * k as f32;
                let (left, right) = if ping { if k % 2 == 1 { (a, 0.0) } else { (0.0, a) } } else { (a, a) };
                canvas::rect(s, x, mid - span * left, 6.0, span * left, &accent);
                canvas::rect(s, x + shift, mid, 6.0, span * right, &alpha(&accent, 0.75));
            }
            let what = if sync == 0 { b.text(0, time) } else { format!("{} note", SYNC_NOTES[sync.min(7)].1) };
            label_right(s, c, r, 6.0, &format!("Repeats every {what}"), 0.75);
        })
    };
    // The band the repeats keep, between the low and high cuts.
    let band = {
        let b = b.clone();
        picture(44, move |c, s, w, h| {
            let (l, r) = (12.0, w - 12.0);
            let (lo, hi) = (freq_x(b.get(5), l, r), freq_x(b.get(6), l, r));
            canvas::rect(s, lo, 8.0, hi - lo, h - 16.0, &alpha(&canvas::accent(c), 0.3));
            for f in [100.0, 1000.0, 10000.0] {
                canvas::rect(s, freq_x(f, l, r), 8.0, 1.0, h - 16.0, &alpha(&canvas::fg(c), 0.15));
            }
            label(s, c, freq_x(100.0, l, r) + 2.0, h - 20.0, "100", 0.5);
            label(s, c, freq_x(1000.0, l, r) + 2.0, h - 20.0, "1k", 0.5);
            label(s, c, freq_x(10000.0, l, r) + 2.0, h - 20.0, "10k", 0.5);
        })
    };
    let time_knob = section("Time", None, b, &[0, 1, 7, 4]);
    {
        // Time and stereo offset only apply while Sync is off.
        let b = b.clone();
        time_knob.add_tick_callback(move |sec, _| {
            let free = b.get(1) < 0.5;
            if let Some(row) = sec.last_child()
                && let Some(knob) = row.first_child()
                && knob.is_sensitive() != free
            {
                knob.set_sensitive(free);
            }
            glib::ControlFlow::Continue
        });
    }
    stack(&[
        taps.upcast_ref(),
        columns(&[time_knob, section("Feedback", None, b, &[2, 3]), section("Filter", Some(&band), b, &[5, 6])]).upcast_ref(),
    ])
    .upcast()
}

/// Seconds for the tail to fall 60 dB with this comb feedback (Freeverb's average comb loop is
/// about 31 ms).
fn decay_seconds(feedback: f32) -> f32 {
    3.0 * 0.0312 / -feedback.log10()
}

pub fn reverb(b: &Arc<Bank>) -> gtk::Widget {
    let tail = {
        let b = b.clone();
        picture(210, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let feedback = 0.7 + b.get(0) / 100.0 * 0.28;
            let damp = b.get(1) / 100.0 * 0.4;
            let (width, mix, pre) = (b.get(2) / 100.0, b.get(3) / 100.0, b.get(4) / 1000.0);
            let low = decay_seconds(feedback);
            // At the top of the spectrum the damping filter takes (1-d)/(1+d) out of every loop.
            let high = decay_seconds(feedback * (1.0 - damp) / (1.0 + damp));
            let total = (pre + low * 1.15).max(0.5);
            let (l, r, t, bot) = (16.0, w - 16.0, 26.0, h - 40.0);
            let x = |sec: f32| l + (r - l) * sec / total;
            let curve = |t60: f32| -> Vec<(f32, f32)> {
                (0..=120)
                    .map(|i| {
                        let sec = pre + (total - pre) * i as f32 / 120.0;
                        let db = -60.0 * (sec - pre) / t60;
                        (x(sec), t + (bot - t) * (-db / 60.0).min(1.0))
                    })
                    .collect()
            };
            canvas::rect(s, x(0.0), t, 3.0, bot - t, &alpha(&fg, 0.6));
            let lows = curve(low);
            fill_under(s, &lows, bot, &alpha(&canvas::accent(c), 0.18 + 0.3 * mix));
            stroke(s, &lows, 2.5, &canvas::accent(c));
            stroke(s, &curve(high), 1.5, &canvas::rgba(0xe5a50a, 0.8));
            canvas::rect(s, x(pre), t, 1.0, bot - t, &alpha(&fg, 0.3));
            label(s, c, l, 6.0, &format!("Decay {low:.1} s"), 0.8);
            label(s, c, l + 90.0, 6.0, "Highs", 0.6);
            canvas::rect(s, l + 78.0, 12.0, 8.0, 2.0, &canvas::rgba(0xe5a50a, 0.8));
            label_right(s, c, r, 6.0, &format!("Pre-delay {:.0} ms", pre * 1000.0), 0.7);
            // Stereo width as a bar centred under the graph.
            let half = (r - l) / 2.0;
            canvas::rect(s, l, h - 22.0, r - l, 6.0, &alpha(&fg, 0.08));
            canvas::rect(s, l + half * (1.0 - width.max(0.03)), h - 22.0, 2.0 * half * width.max(0.03), 6.0, &alpha(&canvas::accent(c), 0.7));
            label(s, c, l, h - 14.0, "Width", 0.5);
        })
    };
    stack(&[tail.upcast_ref(), columns(&[section("Room", None, b, &[0, 1, 4]), section("Output", None, b, &[2, 3])]).upcast_ref()]).upcast()
}
