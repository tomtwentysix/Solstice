use super::*;
use crate::library::{CAPTURES, IRS};
use crate::studio::Ctx;
use gtk::gio;
use solstice_dsp::ir::{Ir, RESPONSE_POINTS, response_hz};
use solstice_dsp::nam::Capture;

const TONE3000: &str = "https://www.tone3000.com/";

pub fn nam(ctx: &Ctx, native: &Arc<Native>) -> gtk::Widget {
    let b = &native.bank;
    let web = gtk::Button::with_label("Open TONE3000");
    web.set_tooltip_text(Some("Find captures to download in your browser"));
    web.connect_clicked(|b| {
        let win = b.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(TONE3000).launch(win.as_ref(), gio::Cancellable::NONE, |_| {});
    });

    let meter = {
        let b = b.clone();
        picture(34, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (lo, hi) = (-48.0, 6.0);
            let x = |db: f32| 8.0 + (w - 16.0) * ((db - lo) / (hi - lo)).clamp(0.0, 1.0);
            let level = b.meter();
            canvas::rect(s, x(lo), h - 14.0, x(hi) - x(lo), 6.0, &alpha(&fg, 0.08));
            let colour = if level > 0.0 { canvas::rgba(0xe01b24, 0.9) } else { canvas::accent(c) };
            canvas::rect(s, x(lo), h - 14.0, x(level) - x(lo), 6.0, &colour);
            canvas::rect(s, x(0.0), h - 16.0, 1.0, 10.0, &alpha(&fg, 0.4));
            label(s, c, 8.0, 3.0, "Into the capture", 0.6);
            let text = if level <= lo { "−∞".to_string() } else { format!("{level:.1} dB") };
            label_right(s, c, w - 8.0, 3.0, &text, 0.8);
        })
    };
    let levels = section("Levels", Some(&meter), b, &[0, 1]);
    let content = stack(&[levels.upcast_ref()]);
    let describe = |n: &Native| {
        files::empty_or_missing::<Capture>(n, &CAPTURES).unwrap_or_else(|| {
            let cap = n.captures.as_ref().and_then(|c| c.current()).unwrap();
            let detail = match cap.rate {
                Some(r) if cap.latency > 0 => format!("Made at {:.1} kHz, resampled, adding {} samples of latency", r / 1000.0, cap.latency),
                Some(r) => format!("Made at {:.1} kHz", r / 1000.0),
                None => "Sample rate not stated".into(),
            };
            (cap.desc.name(), detail)
        })
    };
    files::chooser::<Capture>(ctx, native, &CAPTURES, describe, &[web.upcast()], &content);
    content.upcast()
}

pub fn ir(ctx: &Ctx, native: &Arc<Native>) -> gtk::Widget {
    let b = &native.bank;
    let curve = {
        let irs = native.irs.clone().unwrap();
        picture(150, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (l, r, top, bottom) = (12.0, w - 12.0, 24.0, h - 10.0);
            for f in [100.0, 1000.0, 10000.0] {
                canvas::rect(s, time::freq_x(f, l, r), top, 1.0, bottom - top, &alpha(&fg, 0.15));
            }
            label(s, c, time::freq_x(100.0, l, r) + 2.0, bottom - 12.0, "100", 0.5);
            label(s, c, time::freq_x(1000.0, l, r) + 2.0, bottom - 12.0, "1k", 0.5);
            label(s, c, time::freq_x(10000.0, l, r) + 2.0, bottom - 12.0, "10k", 0.5);
            let Some(ir) = irs.current() else {
                label(s, c, 8.0, 6.0, "No response loaded", 0.6);
                return;
            };
            label(s, c, 8.0, 6.0, "Frequency response", 0.6);
            // 36 dB from the loudest point down.
            let peak = ir.response.iter().fold(f32::MIN, |m, v| m.max(*v));
            let y = |db: f32| top + (bottom - top) * ((peak - db) / 36.0).clamp(0.0, 1.0);
            let points: Vec<(f32, f32)> = (0..RESPONSE_POINTS).map(|i| (time::freq_x(response_hz(i), l, r), y(ir.response[i]))).collect();
            let accent = canvas::accent(c);
            fill_under(s, &points, bottom, &alpha(&accent, 0.2));
            stroke(s, &points, 1.5, &accent);
        })
    };
    let levels = section("Response", Some(&curve), b, &[0, 1]);
    let content = stack(&[levels.upcast_ref()]);
    let describe = |n: &Native| {
        files::empty_or_missing::<Ir>(n, &IRS).unwrap_or_else(|| {
            let ir = n.irs.as_ref().and_then(|c| c.current()).unwrap();
            let layout = if ir.channels == 2 { "Stereo" } else { "Mono" };
            let mut detail = format!("{layout}, {:.2} seconds", ir.seconds);
            if (ir.rate as f64 - n.sample_rate).abs() > 0.5 {
                detail.push_str(&format!(", resampled from {:.1} kHz", ir.rate as f64 / 1000.0));
            }
            (ir.desc.name(), detail)
        })
    };
    files::chooser::<Ir>(ctx, native, &IRS, describe, &[], &content);
    content.upcast()
}
