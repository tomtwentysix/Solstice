use crate::canvas::{self, Canvas, rect, rgba, text};
use crate::master_meter::{BANDS, HISTORY_LEN, HISTORY_STEP, band_freq};
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, glib, gsk};
use solstice_model::LoudnessTarget;
use std::cell::Cell;
use std::collections::HashMap;

const HISTORY_TOP: f32 = 0.0;
const HISTORY_BOTTOM: f32 = -42.0;
const SPECTRUM_FLOOR: f32 = -90.0;

thread_local! {
    static WINDOWS: std::cell::RefCell<HashMap<usize, adw::Window>> = Default::default();
}

fn target(ctx: &Ctx) -> LoudnessTarget {
    ctx.s().project.master_target
}

fn fmt_lufs(v: f64) -> String {
    if v.is_finite() && v > -70.0 { format!("{v:.1}") } else { "-∞".into() }
}

/// Opens the Loudness window, or brings the existing one forward.
pub fn open_window(ctx: &Ctx, parent: &impl IsA<gtk::Window>) {
    let key = ctx.key();
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&key).cloned()) {
        w.present();
        return;
    }
    ctx.m().loudness_open = true;
    let win = adw::Window::builder()
        .title(format!("{} — Loudness", ctx.s().project.name))
        .default_width(720)
        .default_height(640)
        .transient_for(parent)
        .build();
    // The Reference switch is the project window's action.
    if let Some(main) = parent.dynamic_cast_ref::<adw::ApplicationWindow>() {
        win.insert_action_group("win", Some(main));
    }

    let header = adw::HeaderBar::new();
    let names: Vec<&str> = LoudnessTarget::ALL.iter().map(|t| t.name()).collect();
    let targets = gtk::DropDown::from_strings(&names);
    targets.set_tooltip_text(Some("Loudness Target"));
    let index_of = |t: LoudnessTarget| LoudnessTarget::ALL.iter().position(|x| *x == t).unwrap_or(0) as u32;
    targets.set_selected(index_of(target(ctx)));
    {
        let ctx = ctx.clone();
        targets.connect_selected_notify(move |d| {
            let Some(&t) = LoudnessTarget::ALL.get(d.selected() as usize) else { return };
            if target(&ctx) != t {
                ctx.edit("Loudness Target", |p| p.master_target = t);
            }
        });
    }
    header.pack_start(&targets);
    let reset = gtk::Button::with_label("Reset");
    {
        let ctx = ctx.clone();
        reset.connect_clicked(move |_| {
            if let Some(m) = ctx.m().master_meter.as_mut() {
                m.reset();
            }
        });
    }
    header.pack_end(&reset);
    let on_play = gtk::CheckButton::with_label("Reset on Play");
    on_play.set_active(ctx.s().settings.loudness_reset_on_play);
    {
        let ctx = ctx.clone();
        on_play.connect_toggled(move |b| {
            let mut s = ctx.m();
            s.settings.loudness_reset_on_play = b.is_active();
            s.settings.save();
        });
    }
    header.pack_end(&on_play);

    let interrupted = gtk::Label::new(Some("Measurement interrupted"));
    interrupted.add_css_class("error");
    interrupted.set_tooltip_text(Some("Some audio was not measured while the display was busy. Reset to measure again."));
    interrupted.set_visible(false);

    let readouts = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    readouts.set_homogeneous(true);
    let value = |caption: &str, unit: &str| {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let cap = gtk::Label::new(Some(caption));
        cap.add_css_class("dim-label");
        cap.add_css_class("caption");
        let v = gtk::Label::new(Some("-∞"));
        v.add_css_class("numeric");
        v.add_css_class("loudness-value");
        let u = gtk::Label::new(Some(unit));
        u.add_css_class("dim-label");
        u.add_css_class("caption");
        cell.append(&cap);
        cell.append(&v);
        cell.append(&u);
        readouts.append(&cell);
        v
    };
    let momentary = value("Momentary", "LUFS");
    let short_term = value("Short-term", "LUFS");
    let integrated = value("Integrated", "LUFS");
    let range = value("Loudness Range", "LU");
    let true_peak = value("True Peak Max", "dBTP");

    let reference_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let ab = gtk::ToggleButton::with_label("Reference");
    ab.set_tooltip_text(Some("Play the Reference Tracks Instead of the Mix (O)"));
    ab.set_action_name(Some("win.reference"));
    reference_row.append(&ab);
    let matching = gtk::CheckButton::with_label("Match Levels");
    matching.set_tooltip_text(Some("Play the reference tracks at the mix's loudness"));
    matching.set_active(ctx.s().settings.reference_match);
    {
        let ctx = ctx.clone();
        matching.connect_toggled(move |b| {
            let mut s = ctx.m();
            s.settings.reference_match = b.is_active();
            s.settings.save();
        });
    }
    reference_row.append(&matching);
    let comparison = gtk::Label::new(None);
    comparison.add_css_class("numeric");
    comparison.set_hexpand(true);
    comparison.set_xalign(1.0);
    reference_row.append(&comparison);

    let history = history_graph(ctx);
    let correlation = correlation_meter(ctx);
    let spectrum = spectrum_graph(ctx);
    let caption = |t: &str| {
        let l = gtk::Label::new(Some(t));
        l.add_css_class("dim-label");
        l.add_css_class("caption");
        l.set_halign(gtk::Align::Start);
        l
    };

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&interrupted);
    content.append(&readouts);
    content.append(&reference_row);
    content.append(&caption("Short-term Loudness"));
    content.append(&history);
    content.append(&caption("Correlation"));
    content.append(&correlation);
    content.append(&caption("Spectrum"));
    content.append(&spectrum);

    {
        let ctx = ctx.clone();
        let frame = Cell::new(0u32);
        content.add_tick_callback(move |_, _| {
            frame.set(frame.get().wrapping_add(1));
            let ceiling = target(&ctx).levels().map(|(_, c)| c as f64);
            let mut s = ctx.m();
            let Some(m) = s.master_meter.as_mut() else { return glib::ControlFlow::Continue };
            if frame.get() % 2 == 0 {
                m.update_spectrum();
            }
            let l = &m.loudness;
            let set = |label: &gtk::Label, t: String| {
                if label.text() != t {
                    label.set_text(&t);
                }
            };
            set(&momentary, fmt_lufs(l.momentary()));
            set(&short_term, fmt_lufs(l.short_term()));
            set(&integrated, fmt_lufs(l.integrated()));
            set(&range, if l.integrated().is_finite() { format!("{:.1}", l.range()) } else { "—".into() });
            let tp = l.true_peak();
            set(&true_peak, if tp.is_finite() && tp > -96.0 { format!("{tp:.1}") } else { "-∞".into() });
            if ceiling.is_some_and(|c| tp > c) {
                true_peak.add_css_class("over");
            } else {
                true_peak.remove_css_class("over");
            }
            interrupted.set_visible(m.interrupted);
            let mix = crate::reference::mix_loudness(&m.loudness);
            reference_row.set_visible(s.has_reference());
            set(&comparison, reference_text(&s, mix));
            drop(s);
            history.queue_draw();
            correlation.queue_draw();
            spectrum.queue_draw();
            glib::ControlFlow::Continue
        });
    }

    {
        let (ctx2, targets) = (ctx.clone(), targets.clone());
        let weak = win.downgrade();
        ctx.on(move |c| {
            if c == Change::Project && weak.upgrade().is_some() {
                let i = index_of(target(&ctx2));
                if targets.selected() != i {
                    targets.set_selected(i);
                }
            }
        });
    }

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&content));
    win.set_content(Some(&tv));
    let ctx = ctx.clone();
    win.connect_close_request(move |_| {
        ctx.m().loudness_open = false;
        WINDOWS.with(|w| w.borrow_mut().remove(&key));
        glib::Propagation::Proceed
    });
    WINDOWS.with(|w| w.borrow_mut().insert(key, win.clone()));
    win.present();
}

/// The mix's loudness that level matching follows, beside each reference track's own and
/// the gain matching gives it.
fn reference_text(s: &crate::studio::Studio, mix: Option<f32>) -> String {
    let lufs = |v: Option<f32>| v.map_or("—".into(), |v| format!("{v:.1} LUFS"));
    let mut parts = vec![format!("Mix {}", lufs(mix))];
    for t in s.project.tracks.iter().filter(|t| t.reference) {
        let gain = s.strips.get(&Some(t.id)).map_or(0.0, |st| st.match_db.get());
        let measured = lufs(t.reference_lufs);
        parts.push(if gain.abs() >= 0.05 { format!("{} {measured} ({gain:+.1} dB)", t.name) } else { format!("{} {measured}", t.name) });
    }
    parts.join("   ")
}

fn dim(c: &Canvas, a: f32) -> gdk::RGBA {
    let fg = canvas::fg(c);
    gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a)
}

fn history_graph(ctx: &Ctx) -> Canvas {
    let ctx = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let y_of = |v: f32| h * ((HISTORY_TOP - v) / (HISTORY_TOP - HISTORY_BOTTOM)).clamp(0.0, 1.0);
        rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.3));
        for v in (HISTORY_BOTTOM as i32..=HISTORY_TOP as i32).step_by(6) {
            let y = y_of(v as f32);
            rect(s, 0.0, y, w, 1.0, &dim(c, 0.08));
            text(s, c, 4.0, y + 1.0, &v.to_string(), &dim(c, 0.45), Some(6.5));
        }
        let minutes = (HISTORY_LEN as f64 * HISTORY_STEP / 60.0) as usize;
        for m in 1..minutes {
            let x = w - w * m as f32 / minutes as f32;
            rect(s, x, 0.0, 1.0, h, &dim(c, 0.06));
            text(s, c, x + 3.0, h - 14.0, &format!("−{m} min"), &dim(c, 0.4), Some(6.5));
        }
        if let Some((lufs, _)) = ctx.s().project.master_target.levels() {
            let y = y_of(lufs);
            rect(s, 0.0, y - 0.5, w, 1.5, &canvas::accent(c));
            text(s, c, w - 70.0, y - 14.0, &format!("Target {lufs}"), &canvas::accent(c), Some(7.0));
        }
        let st = ctx.s();
        let Some(m) = st.master_meter.as_ref() else { return };
        let step = w / (HISTORY_LEN - 1) as f32;
        let n = m.history.len();
        let b = gsk::PathBuilder::new();
        let mut drawing = false;
        for (i, &v) in m.history.iter().enumerate() {
            if !(v.is_finite() && v > -70.0) {
                drawing = false;
                continue;
            }
            let (x, y) = (w - (n - 1 - i) as f32 * step, y_of(v));
            if drawing {
                b.line_to(x, y);
            } else {
                b.move_to(x, y);
                drawing = true;
            }
        }
        s.append_stroke(&b.to_path(), &gsk::Stroke::new(1.5), &canvas::fg(c));
    });
    c.set_hexpand(true);
    c.set_vexpand(true);
    c.set_size_request(320, 140);
    c
}

fn correlation_meter(ctx: &Ctx) -> Canvas {
    let ctx = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.3));
        let mid = w / 2.0;
        for (v, label) in [(-1.0, "−1"), (0.0, "0"), (1.0, "+1")] {
            let x = mid + v * (mid - 1.0);
            rect(s, x, 0.0, 1.0, h, &dim(c, 0.25));
            let tx = (x + 3.0).min(w - 14.0);
            text(s, c, tx, 1.0, label, &dim(c, 0.45), Some(6.5));
        }
        let Some(v) = ctx.s().master_meter.as_ref().map(|m| m.correlation()) else { return };
        let x = mid + v * (mid - 1.0);
        let color = if v < 0.0 { rgba(0xe01b24, 0.85) } else { rgba(0x33d17a, 0.85) };
        let bar = h * 0.4;
        rect(s, x.min(mid), h - bar - 3.0, (x - mid).abs().max(2.0), bar, &color);
    });
    c.set_hexpand(true);
    c.set_size_request(320, 28);
    c
}

fn spectrum_graph(ctx: &Ctx) -> Canvas {
    let ctx = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let (lo, hi) = (20f32.ln(), 20000f32.ln());
        let x_of = |f: f32| w * (f.ln() - lo) / (hi - lo);
        let y_of = |db: f32| h * (db / SPECTRUM_FLOOR).clamp(0.0, 1.0);
        rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.3));
        for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            let x = x_of(f);
            rect(s, x, 0.0, 1.0, h, &dim(c, 0.08));
            let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
            text(s, c, x + 3.0, h - 14.0, &label, &dim(c, 0.5), Some(7.0));
        }
        for db in (SPECTRUM_FLOOR as i32 + 18..=0).step_by(18) {
            let y = y_of(db as f32);
            rect(s, 0.0, y, w, 1.0, &dim(c, 0.07));
            text(s, c, 4.0, y + 1.0, &db.to_string(), &dim(c, 0.45), Some(6.5));
        }
        let st = ctx.s();
        let Some(m) = st.master_meter.as_ref() else { return };
        let fill = gsk::PathBuilder::new();
        fill.move_to(0.0, h);
        let peak = gsk::PathBuilder::new();
        for k in 0..BANDS {
            let x = x_of(band_freq(k)).min(w);
            fill.line_to(x, y_of(m.bands[k]));
            if k == 0 {
                peak.move_to(x, y_of(m.peaks[k]));
            } else {
                peak.line_to(x, y_of(m.peaks[k]));
            }
        }
        fill.line_to(w, h);
        fill.close();
        let accent = canvas::accent(c);
        s.append_fill(&fill.to_path(), gsk::FillRule::Winding, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.45));
        s.append_stroke(&peak.to_path(), &gsk::Stroke::new(1.0), &dim(c, 0.7));
    });
    c.set_hexpand(true);
    c.set_vexpand(true);
    c.set_size_request(320, 160);
    c
}
