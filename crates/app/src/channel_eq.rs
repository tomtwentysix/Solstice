use crate::canvas::{self, Canvas, rect, rgba, rounded, text};
use crate::gestures::redraw_on_change;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, graphene, gsk, glib};
use solstice_engine::eq::{band_response_db, design, response_db};
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::Ordering::Relaxed;

const F_MIN: f64 = 20.0;
const F_MAX: f64 = 20000.0;
const RANGE_DB: f64 = 24.0;
const PAD: f32 = 18.0;

const COLORS: [u32; EQ_BANDS] = [0xe01b24, 0xff7800, 0xf6d32d, 0x33d17a, 0x2ec27e, 0x3584e4, 0x9141ac, 0xdc8add];
const NAMES: [&str; EQ_BANDS] = ["Low Cut", "Low Shelf", "Bell 1", "Bell 2", "Bell 3", "Bell 4", "High Shelf", "High Cut"];

fn eq_of(p: &Project, id: Option<Id>) -> ChannelEq {
    p.channel(id).map(|c| c.eq.clone()).unwrap_or_default()
}

/// A cheap number that changes whenever anything about the EQ does, for redraw checks.
fn fingerprint(eq: &ChannelEq) -> f32 {
    let mut h = if eq.enabled { 1.0 } else { 0.0 };
    for (i, b) in eq.bands.iter().enumerate() {
        let k = (i + 2) as f32;
        h += k * (b.freq.ln() + b.gain_db * 0.37 + b.q * 1.3 + b.slope as f32 * 0.01 + if b.on { 0.5 } else { 0.0 });
    }
    h
}

fn x_of(f: f64, w: f32) -> f32 {
    ((f / F_MIN).log10() / (F_MAX / F_MIN).log10()) as f32 * w
}

fn f_of(x: f32, w: f32) -> f64 {
    F_MIN * (F_MAX / F_MIN).powf((x / w).clamp(0.0, 1.0) as f64)
}

fn y_of(db: f64, h: f32) -> f32 {
    h / 2.0 - (db / RANGE_DB) as f32 * (h / 2.0 - PAD)
}

fn curve(eq: &ChannelEq, fs: f64, w: f32, h: f32, band: Option<usize>) -> gsk::Path {
    curve_in(eq, fs, w, band, |db| y_of(db, h))
}

fn curve_in(eq: &ChannelEq, fs: f64, w: f32, band: Option<usize>, y_of: impl Fn(f64) -> f32) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    for (i, (x, y)) in curve_points(eq, fs, w, band, y_of).into_iter().enumerate() {
        if i == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
    }
    b.to_path()
}

fn curve_points(eq: &ChannelEq, fs: f64, w: f32, band: Option<usize>, y_of: impl Fn(f64) -> f32) -> Vec<(f32, f32)> {
    let c = design(eq, fs);
    let steps = (w as usize / 2).max(32);
    (0..=steps)
        .map(|i| {
            let x = w * i as f32 / steps as f32;
            let f = f_of(x, w);
            let db = match band {
                Some(k) => band_response_db(k, &eq.bands[k], f, fs),
                None => response_db(&c, f, fs),
            };
            (x, y_of(db.clamp(-RANGE_DB * 2.0, RANGE_DB * 2.0)))
        })
        .collect()
}

/// The EQ row of a channel strip: a small curve. Click to switch the EQ on and open it.
pub fn thumbnail(ctx: &Ctx, id: Option<Id>) -> Canvas {
    let ctx2 = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let (eq, fs) = {
            let st = ctx2.s();
            (eq_of(&st.project, id), st.project.sample_rate as f64)
        };
        rounded(s, 0.0, 1.0, w, h - 2.0, 3.0, &rgba(0x000000, 0.35));
        if !eq.enabled {
            return;
        }
        let fg = canvas::fg(c);
        rect(s, 0.0, h / 2.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15));
        // The whole range, edge to edge: 20 Hz to 20 kHz across and ±24 dB top to bottom.
        let half = (h - 4.0) / 2.0;
        let y_of = |db: f64| h / 2.0 - (db / RANGE_DB) as f32 * half;
        let points = curve_points(&eq, fs, w, None, y_of);
        let line = gsk::PathBuilder::new();
        // The fill runs between the curve and the 0 dB line.
        let fill = gsk::PathBuilder::new();
        fill.move_to(0.0, h / 2.0);
        for (i, &(x, y)) in points.iter().enumerate() {
            if i == 0 { line.move_to(x, y) } else { line.line_to(x, y) }
            fill.line_to(x, y);
        }
        fill.line_to(w, h / 2.0);
        fill.close();
        let line = line.to_path();
        let accent = canvas::accent(c);
        s.push_clip(&graphene::Rect::new(0.0, 1.0, w, h - 2.0));
        s.append_fill(&fill.to_path(), gsk::FillRule::Winding, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.25));
        s.append_stroke(&line, &gsk::Stroke::new(1.5), &accent);
        s.pop();
    });
    c.set_hexpand(true);
    c.set_tooltip_text(Some("Channel EQ: click to open"));
    redraw_on_change(ctx, &c, move |p| fingerprint(&eq_of(p, id)));
    let click = gtk::GestureClick::new();
    let (ctx, c2) = (ctx.clone(), c.clone());
    click.connect_released(move |_, _, _, _| {
        if !eq_of(&ctx.s().project, id).enabled {
            ctx.edit("Channel EQ", |p| {
                if let Some(ch) = p.channel_mut(id) {
                    ch.eq.enabled = true;
                }
            });
        }
        open_editor(&ctx, id, &c2);
    });
    c.add_controller(click);
    c
}

thread_local! {
    static WINDOWS: RefCell<HashMap<(usize, Option<Id>), adw::Window>> = RefCell::default();
}

fn channel_name(p: &Project, id: Option<Id>) -> String {
    match id {
        None => "Stereo Out".into(),
        Some(id) => p
            .track(id)
            .map(|t| t.name.clone())
            .or_else(|| p.buses.iter().find(|b| b.id == id).map(|b| b.name.clone()))
            .unwrap_or_default(),
    }
}

/// Applies a change to one band during a gesture, switching the EQ (and the band) on.
fn set_band(ctx: &Ctx, id: Option<Id>, band: usize, f: impl FnOnce(&mut EqBand)) {
    ctx.live(true, |p| {
        if let Some(ch) = p.channel_mut(id) {
            ch.eq.enabled = true;
            f(&mut ch.eq.bands[band]);
        }
    });
}

fn edit_band(ctx: &Ctx, id: Option<Id>, band: usize, f: impl FnOnce(&mut EqBand)) {
    ctx.begin_gesture();
    set_band(ctx, id, band, f);
    ctx.end_gesture("Channel EQ");
}

/// The Channel EQ editor for one channel. Reopening brings the existing window forward.
pub fn open_editor(ctx: &Ctx, id: Option<Id>, anchor: &impl IsA<gtk::Widget>) {
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&(ctx.key(), id)).cloned()) {
        w.present();
        return;
    }
    let title = format!("{} — Channel EQ", channel_name(&ctx.s().project, id));
    let win = adw::Window::builder().title(&title).default_width(560).default_height(500).build();
    if let Some(parent) = anchor.root().and_downcast::<gtk::Window>() {
        win.set_transient_for(Some(&parent));
    }
    let selected = Rc::new(Cell::new(None::<usize>));
    let analyzer = Rc::new(Cell::new(true));
    let strip = ctx.m().strip(id);
    strip.scope_on.store(true, Relaxed);

    let header = adw::HeaderBar::new();
    let power = gtk::ToggleButton::new();
    power.set_icon_name("system-shutdown-symbolic");
    power.set_tooltip_text(Some("EQ On/Off"));
    power.set_active(eq_of(&ctx.s().project, id).enabled);
    {
        let ctx = ctx.clone();
        power.connect_toggled(move |b| {
            let on = b.is_active();
            if eq_of(&ctx.s().project, id).enabled != on {
                ctx.edit("Channel EQ", |p| {
                    if let Some(ch) = p.channel_mut(id) {
                        ch.eq.enabled = on;
                    }
                });
            }
        });
    }
    header.pack_start(&power);
    let an = gtk::ToggleButton::with_label("Analyzer");
    an.set_active(true);
    {
        let (analyzer, strip) = (analyzer.clone(), strip.clone());
        an.connect_toggled(move |b| {
            analyzer.set(b.is_active());
            strip.scope_on.store(b.is_active(), Relaxed);
        });
    }
    header.pack_start(&an);
    let reset = gtk::Button::with_label("Reset");
    {
        let ctx = ctx.clone();
        reset.connect_clicked(move |_| {
            ctx.edit("Reset Channel EQ", |p| {
                if let Some(ch) = p.channel_mut(id) {
                    ch.eq = ChannelEq { enabled: true, ..ChannelEq::default() };
                }
            })
        });
    }
    header.pack_end(&reset);
    header.pack_end(&preset_selector(ctx, id, &power));

    let graph = graph(ctx, id, &selected, &analyzer);
    let bands = band_controls(ctx, id, &selected);
    // The graph keeps a 16:10 shape however the window is sized.
    let frame = gtk::AspectFrame::new(0.5, 0.5, 1.6, false);
    frame.set_child(Some(&graph));
    frame.set_vexpand(true);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&frame);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&bands);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&content));
    win.set_content(Some(&tv));
    {
        let (ctx2, power) = (ctx.clone(), power.clone());
        let weak = win.downgrade();
        ctx.on(move |c| {
            if c == Change::Project && weak.upgrade().is_some() {
                let on = eq_of(&ctx2.s().project, id).enabled;
                if power.is_active() != on {
                    power.set_active(on);
                }
            }
        });
    }
    let key = (ctx.key(), id);
    win.connect_close_request(move |_| {
        strip.scope_on.store(false, Relaxed);
        WINDOWS.with(|w| w.borrow_mut().remove(&key));
        glib::Propagation::Proceed
    });
    WINDOWS.with(|w| w.borrow_mut().insert(key, win.clone()));
    win.present();
}

fn graph(ctx: &Ctx, id: Option<Id>, selected: &Rc<Cell<Option<usize>>>, analyzer: &Rc<Cell<bool>>) -> Canvas {
    let spectrum: Rc<RefCell<Vec<f32>>> = Rc::default();
    let (ctx2, sel, an, spec) = (ctx.clone(), selected.clone(), analyzer.clone(), spectrum.clone());
    let c = Canvas::new(move |c, s, w, h| {
        let (eq, fs) = {
            let st = ctx2.s();
            (eq_of(&st.project, id), st.project.sample_rate as f64)
        };
        let fg = canvas::fg(c);
        let dim = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
        rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.3));

        for f in [20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0] {
            let x = x_of(f, w);
            rect(s, x, 0.0, 1.0, h, &dim(0.08));
            let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
            text(s, c, x + 3.0, h - 14.0, &label, &dim(0.5), Some(7.0));
        }
        for db in [-24.0, -18.0, -12.0, -6.0, 0.0, 6.0, 12.0, 18.0, 24.0] {
            let y = y_of(db, h);
            rect(s, 0.0, y, w, 1.0, &dim(if db == 0.0 { 0.25 } else { 0.07 }));
            text(s, c, 4.0, y - 12.0, &format!("{db:+}"), &dim(0.45), Some(6.5));
        }

        if an.get() {
            let sp = spec.borrow();
            if !sp.is_empty() {
                let bins = sp.len() as f64;
                let b = gsk::PathBuilder::new();
                b.move_to(0.0, h);
                for px in 0..w as usize {
                    let (f0, f1) = (f_of(px as f32, w), f_of(px as f32 + 1.0, w));
                    let (k0, k1) = ((f0 / (fs / 2.0) * bins) as usize, ((f1 / (fs / 2.0) * bins) as usize).max((f0 / (fs / 2.0) * bins) as usize + 1));
                    let db = sp[k0.min(sp.len() - 1)..k1.min(sp.len())].iter().fold(-120f32, |m, v| m.max(*v));
                    // The analyser uses its own scale: 0 dBFS at the top, -90 at the bottom.
                    let y = h * (-db / 90.0).clamp(0.0, 1.0);
                    b.line_to(px as f32, y);
                }
                b.line_to(w, h);
                b.close();
                s.append_fill(&b.to_path(), gsk::FillRule::Winding, &dim(0.12));
            }
        }

        if !eq.enabled {
            text(s, c, w / 2.0 - 40.0, h / 2.0 - 20.0, "EQ Off", &dim(0.5), Some(12.0));
        }
        for (k, band) in eq.bands.iter().enumerate() {
            if band.on && (sel.get() == Some(k) || EqKind::of(k) != EqKind::Bell || band.gain_db != 0.0) {
                let col = rgba(COLORS[k], if sel.get() == Some(k) { 0.7 } else { 0.3 });
                s.append_stroke(&curve(&eq, fs, w, h, Some(k)), &gsk::Stroke::new(1.0), &col);
            }
        }
        let total = curve(&eq, fs, w, h, None);
        let col = if eq.enabled { canvas::accent(c) } else { dim(0.4) };
        s.append_stroke(&total, &gsk::Stroke::new(2.5), &col);

        for (k, band) in eq.bands.iter().enumerate() {
            let x = x_of(band.freq as f64, w);
            let y = if EqKind::of(k).is_cut() { y_of(0.0, h) } else { y_of(band.gain_db as f64, h) };
            let r = if sel.get() == Some(k) { 7.0 } else { 5.5 };
            let alpha = if band.on { 1.0 } else { 0.35 };
            let dot = gsk::PathBuilder::new();
            dot.add_circle(&graphene::Point::new(x, y), r);
            s.append_fill(&dot.to_path(), gsk::FillRule::Winding, &rgba(COLORS[k], alpha));
            if sel.get() == Some(k) {
                s.append_stroke(&dot.to_path(), &gsk::Stroke::new(1.5), &rgba(0xffffff, 0.9));
            }
        }
        if let Some(k) = sel.get() {
            let b = &eq.bands[k];
            let detail = if EqKind::of(k).is_cut() {
                format!("{} · {} Hz · {} dB/oct", NAMES[k], fmt_freq(b.freq), b.slope)
            } else {
                format!("{} · {} Hz · {:+.1} dB · Q {:.2}", NAMES[k], fmt_freq(b.freq), b.gain_db, b.q)
            };
            text(s, c, w - 260.0, 6.0, &detail, &fg, Some(8.0));
        }
    });
    c.set_vexpand(true);
    c.set_hexpand(true);
    c.set_size_request(320, 200);

    // Redraw on edits; when the analyser runs, refresh the spectrum about 30 times a second.
    redraw_on_change(ctx, &c, move |p| fingerprint(&eq_of(p, id)));
    {
        let (ctx, an, spec) = (ctx.clone(), analyzer.clone(), spectrum.clone());
        let frame = Cell::new(0u32);
        c.add_tick_callback(move |c, _| {
            frame.set(frame.get().wrapping_add(1));
            if an.get() && frame.get().is_multiple_of(2) {
                let samples = ctx.m().strip(id).scope();
                let fresh = crate::fft::spectrum_db(&samples);
                let mut sp = spec.borrow_mut();
                if sp.len() != fresh.len() {
                    *sp = fresh;
                } else {
                    for (old, new) in sp.iter_mut().zip(fresh) {
                        *old = new.max(*old - 1.5);
                    }
                }
                c.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    let nearest = move |ctx: &Ctx, x: f64, y: f64, w: f32, h: f32| -> Option<usize> {
        let eq = eq_of(&ctx.s().project, id);
        let mut best = None;
        let mut best_d = f32::MAX;
        for (k, b) in eq.bands.iter().enumerate() {
            let nx = x_of(b.freq as f64, w);
            let ny = if EqKind::of(k).is_cut() { y_of(0.0, h) } else { y_of(b.gain_db as f64, h) };
            let d = ((nx - x as f32).powi(2) + (ny - y as f32).powi(2)).sqrt();
            if d < best_d {
                best_d = d;
                best = Some(k);
            }
        }
        best.filter(|_| best_d < 40.0)
    };

    let start = Rc::new(Cell::new(None::<Grab>));
    // Ctrl locks a drag to the direction it first moves in: true for horizontal.
    let axis = Rc::new(Cell::new(None::<bool>));
    let g = gtk::GestureDrag::new();
    {
        let (ctx, sel, start, c2, axis) = (ctx.clone(), selected.clone(), start.clone(), c.clone(), axis.clone());
        g.connect_drag_begin(move |g, x, y| {
            axis.set(None);
            let (w, h) = (c2.width() as f32, c2.height() as f32);
            let Some(k) = nearest(&ctx, x, y, w, h) else {
                sel.set(None);
                start.set(None);
                c2.queue_draw();
                return;
            };
            sel.set(Some(k));
            let mods = g.current_event_state();
            // Alt-click resets the band; Ctrl+Alt is the Q drag, handled as it moves.
            if mods.contains(gdk::ModifierType::ALT_MASK) && !mods.contains(gdk::ModifierType::CONTROL_MASK) {
                let def = ChannelEq::default().bands[k];
                edit_band(&ctx, id, k, |b| *b = EqBand { on: true, ..def });
                start.set(None);
                return;
            }
            let b = eq_of(&ctx.s().project, id).bands[k];
            ctx.begin_gesture();
            set_band(&ctx, id, k, |b| b.on = true);
            let slope = EQ_SLOPES.iter().position(|s| *s == b.slope).unwrap_or(3);
            start.set(Some(Grab { band: k, freq: b.freq, gain: b.gain_db, q: b.q, slope }));
            c2.queue_draw();
        });
    }
    {
        let (ctx, start, c2, axis) = (ctx.clone(), start.clone(), c.clone(), axis.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some(grab) = start.get() else { return };
            let k = grab.band;
            let (w, h) = (c2.width() as f32, c2.height() as f32);
            let mods = g.current_event_state();
            let (ctrl, alt) = (mods.contains(gdk::ModifierType::CONTROL_MASK), mods.contains(gdk::ModifierType::ALT_MASK));
            let fine = if mods.contains(gdk::ModifierType::SHIFT_MASK) { 0.15 } else { 1.0 };
            // Ctrl-drag sticks to one direction; Ctrl+Alt turns vertical movement into Q.
            let q_drag = ctrl && alt;
            let (mut dx, mut dy) = (dx, dy);
            if ctrl && !alt {
                if axis.get().is_none() && dx.abs().max(dy.abs()) > 4.0 {
                    axis.set(Some(dx.abs() >= dy.abs()));
                }
                match axis.get() {
                    Some(true) => dy = 0.0,
                    Some(false) => dx = 0.0,
                    None => return,
                }
            }
            let decades = (F_MAX / F_MIN).log10() as f32;
            let freq = (grab.freq * 10f32.powf(dx as f32 / w * decades * fine)).clamp(F_MIN as f32, F_MAX as f32);
            let gain = (grab.gain - dy as f32 / (h / 2.0 - PAD) * RANGE_DB as f32 * fine).clamp(-RANGE_DB as f32, RANGE_DB as f32);
            // Up narrows the band (higher Q); about 100 px per doubling.
            let q = (grab.q * 2f32.powf(-dy as f32 / 100.0 * fine)).clamp(0.1, 100.0);
            let slope = (grab.slope as i32 - (dy / 30.0) as i32).clamp(0, EQ_SLOPES.len() as i32 - 1) as usize;
            set_band(&ctx, id, k, |b| {
                b.freq = freq;
                if q_drag {
                    if EqKind::of(k).is_cut() {
                        b.slope = EQ_SLOPES[slope];
                    } else {
                        b.q = (q * 100.0).round() / 100.0;
                    }
                } else if !EqKind::of(k).is_cut() {
                    b.gain_db = (gain * 10.0).round() / 10.0;
                }
            });
        });
    }
    {
        let (ctx, start) = (ctx.clone(), start.clone());
        g.connect_drag_end(move |_, _, _| {
            if start.take().is_some() {
                ctx.end_gesture("Channel EQ");
            }
        });
    }
    c.add_controller(g);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, c2) = (ctx.clone(), c.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n == 2
                && let Some(k) = nearest(&ctx, x, y, c2.width() as f32, c2.height() as f32)
            {
                edit_band(&ctx, id, k, |b| b.on = !b.on);
            }
        });
    }
    c.add_controller(dbl);

    // Scroll changes Q on bells and shelves, or steps the slope on the cuts.
    let pointer = Rc::new(Cell::new((0.0, 0.0)));
    let motion = gtk::EventControllerMotion::new();
    {
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, y| pointer.set((x, y)));
    }
    c.add_controller(motion);
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (ctx, sel, c2) = (ctx.clone(), selected.clone(), c.clone());
        sc.connect_scroll(move |_, _, dy| {
            let (x, y) = pointer.get();
            let Some(k) = nearest(&ctx, x, y, c2.width() as f32, c2.height() as f32).or(sel.get()) else {
                return glib::Propagation::Proceed;
            };
            sel.set(Some(k));
            let up = dy < 0.0;
            edit_band(&ctx, id, k, |b| {
                if EqKind::of(k).is_cut() {
                    let i = EQ_SLOPES.iter().position(|s| *s == b.slope).unwrap_or(3) as i32;
                    b.slope = EQ_SLOPES[(i + if up { 1 } else { -1 }).clamp(0, EQ_SLOPES.len() as i32 - 1) as usize];
                } else {
                    b.q = (b.q * if up { 1.12 } else { 1.0 / 1.12 }).clamp(0.1, 100.0);
                }
            });
            glib::Propagation::Stop
        });
    }
    c.add_controller(sc);
    c
}

/// A band being dragged, with its settings when the drag began.
#[derive(Clone, Copy)]
struct Grab {
    band: usize,
    freq: f32,
    gain: f32,
    q: f32,
    slope: usize,
}

fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 { format!("{:.2}k", f / 1000.0) } else { format!("{f:.0}") }
}

const SHORT: [&str; EQ_BANDS] = ["Cut", "Shelf", "1", "2", "3", "4", "Shelf", "Cut"];

/// A row of band switches (click to select, toggle to switch on and off) and the fields of
/// the selected band, kept in step with drags and undo.
fn band_controls(ctx: &Ctx, id: Option<Id>, selected: &Rc<Cell<Option<usize>>>) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_margin_start(8);
    col.set_margin_end(8);
    col.set_margin_top(8);
    col.set_margin_bottom(8);

    let toggles_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    toggles_row.set_homogeneous(true);
    let mut toggles = Vec::new();
    for k in 0..EQ_BANDS {
        let on = gtk::ToggleButton::with_label(SHORT[k]);
        on.add_css_class("eq-band-toggle");
        on.add_css_class(&format!("eq-band-{k}"));
        on.set_tooltip_text(Some(NAMES[k]));
        {
            let (ctx, sel) = (ctx.clone(), selected.clone());
            on.connect_toggled(move |b| {
                let v = b.is_active();
                // Syncing to the project also lands here; only a real change selects the band.
                if eq_of(&ctx.s().project, id).bands[k].on != v {
                    sel.set(Some(k));
                    edit_band(&ctx, id, k, |b| b.on = v);
                }
            });
        }
        toggles_row.append(&on);
        toggles.push(on);
    }
    col.append(&toggles_row);

    let fields = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(None);
    name.add_css_class("heading");
    name.set_width_chars(9);
    name.set_xalign(0.0);
    let freq = gtk::SpinButton::with_range(20.0, 20000.0, 1.0);
    freq.set_tooltip_text(Some("Frequency (Hz)"));
    let gain = gtk::SpinButton::with_range(-24.0, 24.0, 0.1);
    gain.set_digits(1);
    gain.set_tooltip_text(Some("Gain (dB)"));
    let q = gtk::SpinButton::with_range(0.1, 100.0, 0.01);
    q.set_digits(2);
    q.set_tooltip_text(Some("Q"));
    let slope_names: Vec<String> = EQ_SLOPES.iter().map(|s| format!("{s} dB/oct")).collect();
    let refs: Vec<&str> = slope_names.iter().map(|s| s.as_str()).collect();
    let slope = gtk::DropDown::from_strings(&refs);
    let caption = |t: &str| {
        let l = gtk::Label::new(Some(t));
        l.add_css_class("dim-label");
        l
    };
    let (hz, db, ql) = (caption("Hz"), caption("dB"), caption("Q"));
    fields.append(&name);
    for w in [freq.upcast_ref::<gtk::Widget>(), hz.upcast_ref(), gain.upcast_ref(), db.upcast_ref(), q.upcast_ref(), ql.upcast_ref(), slope.upcast_ref()] {
        fields.append(w);
    }
    let hint = gtk::Label::new(Some("Click a band to edit it. Ctrl-drag keeps to one direction; Ctrl+Alt-drag sets Q; Alt-click resets."));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    col.append(&fields);
    col.append(&hint);

    let current = |sel: &Cell<Option<usize>>| sel.get();
    {
        let (ctx, sel) = (ctx.clone(), selected.clone());
        freq.connect_value_changed(move |s| {
            let (Some(k), v) = (current(&sel), s.value() as f32) else { return };
            if (eq_of(&ctx.s().project, id).bands[k].freq - v).abs() > 0.5 {
                edit_band(&ctx, id, k, |b| b.freq = v);
            }
        });
    }
    {
        let (ctx, sel) = (ctx.clone(), selected.clone());
        gain.connect_value_changed(move |s| {
            let (Some(k), v) = (current(&sel), s.value() as f32) else { return };
            if (eq_of(&ctx.s().project, id).bands[k].gain_db - v).abs() > 0.05 {
                edit_band(&ctx, id, k, |b| b.gain_db = v);
            }
        });
    }
    {
        let (ctx, sel) = (ctx.clone(), selected.clone());
        q.connect_value_changed(move |s| {
            let (Some(k), v) = (current(&sel), s.value() as f32) else { return };
            if (eq_of(&ctx.s().project, id).bands[k].q - v).abs() > 0.005 {
                edit_band(&ctx, id, k, |b| b.q = v);
            }
        });
    }
    {
        let (ctx, sel) = (ctx.clone(), selected.clone());
        slope.connect_selected_notify(move |d| {
            let Some(k) = current(&sel) else { return };
            let v = EQ_SLOPES[d.selected() as usize];
            if eq_of(&ctx.s().project, id).bands[k].slope != v {
                edit_band(&ctx, id, k, |b| b.slope = v);
            }
        });
    }

    let sync = {
        let (ctx, sel) = (ctx.clone(), selected.clone());
        move || {
            let eq = eq_of(&ctx.s().project, id);
            for (t, b) in toggles.iter().zip(eq.bands.iter()) {
                if t.is_active() != b.on {
                    t.set_active(b.on);
                }
            }
            let Some(k) = sel.get() else {
                fields.set_visible(false);
                hint.set_visible(true);
                return;
            };
            fields.set_visible(true);
            hint.set_visible(false);
            let b = eq.bands[k];
            let cut = EqKind::of(k).is_cut();
            name.set_markup(&format!("<span foreground=\"#{:06x}\">{}</span>", COLORS[k], NAMES[k]));
            for w in [gain.upcast_ref::<gtk::Widget>(), db.upcast_ref(), q.upcast_ref(), ql.upcast_ref()] {
                w.set_visible(!cut);
            }
            slope.set_visible(cut);
            if (freq.value() as f32 - b.freq).abs() > 0.5 {
                freq.set_value(b.freq as f64);
            }
            if (gain.value() as f32 - b.gain_db).abs() > 0.05 {
                gain.set_value(b.gain_db as f64);
            }
            if (q.value() as f32 - b.q).abs() > 0.005 {
                q.set_value(b.q as f64);
            }
            let si = EQ_SLOPES.iter().position(|s| *s == b.slope).unwrap_or(3) as u32;
            if slope.selected() != si {
                slope.set_selected(si);
            }
        }
    };
    // Follows drags (which don't emit project changes until release), undo and selection.
    let last = Cell::new((f32::NAN, None::<usize>));
    let (ctx, sel) = (ctx.clone(), selected.clone());
    col.add_tick_callback(move |_, _| {
        let now = (fingerprint(&eq_of(&ctx.s().project, id)), sel.get());
        if now.0.to_bits() != last.get().0.to_bits() || now.1 != last.get().1 {
            last.set(now);
            sync();
        }
        glib::ControlFlow::Continue
    });
    col.upcast()
}

/// Bands switched on by each preset, as (band, frequency, gain, Q); the rest stay at their
/// defaults. Bands: 0 low cut, 1 low shelf, 2-5 bells, 6 high shelf, 7 high cut.
const PRESETS: &[(&str, &[(usize, f32, f32, f32)])] = &[
    ("Vocal Clarity", &[(0, 90.0, 0.0, 0.71), (2, 250.0, -2.0, 1.0), (4, 3000.0, 2.0, 0.9), (6, 10000.0, 3.0, 0.71)]),
    ("Podcast Voice", &[(0, 80.0, 0.0, 0.71), (2, 200.0, -3.0, 1.0), (4, 4000.0, 2.5, 1.0), (6, 12000.0, 1.5, 0.71)]),
    ("Backing Vocals", &[(0, 150.0, 0.0, 0.71), (3, 400.0, -2.5, 1.0), (6, 9000.0, 2.0, 0.71)]),
    ("Kick", &[(0, 30.0, 0.0, 0.71), (2, 60.0, 3.0, 1.2), (3, 350.0, -4.0, 1.4), (5, 4000.0, 3.0, 1.5)]),
    ("Snare", &[(0, 80.0, 0.0, 0.71), (2, 200.0, 2.0, 1.1), (3, 800.0, -2.0, 1.5), (5, 5000.0, 3.0, 1.1), (6, 10000.0, 1.0, 0.71)]),
    ("Overheads", &[(0, 200.0, 0.0, 0.71), (3, 400.0, -2.0, 1.1), (6, 10000.0, 2.0, 0.71)]),
    ("Bass", &[(0, 35.0, 0.0, 0.71), (2, 100.0, 2.0, 1.1), (3, 300.0, -3.0, 1.2), (4, 1000.0, 1.5, 1.1), (7, 8000.0, 0.0, 0.71)]),
    ("Acoustic Guitar", &[(0, 90.0, 0.0, 0.71), (2, 200.0, -3.0, 1.1), (5, 3000.0, 2.0, 1.1), (6, 12000.0, 2.0, 0.71)]),
    ("Electric Guitar", &[(0, 100.0, 0.0, 0.71), (3, 400.0, -2.0, 1.1), (5, 2500.0, 2.0, 1.1), (7, 9000.0, 0.0, 0.71)]),
    ("Bright Keys", &[(0, 50.0, 0.0, 0.71), (2, 300.0, -1.5, 1.1), (6, 8000.0, 2.0, 0.71)]),
    ("Thin Pad", &[(0, 200.0, 0.0, 0.71), (3, 500.0, -2.0, 1.1), (6, 8000.0, 1.0, 0.71)]),
    ("Master Polish", &[(0, 25.0, 0.0, 0.71), (1, 80.0, 1.0, 0.71), (2, 300.0, -1.0, 0.7), (6, 12000.0, 1.5, 0.71)]),
    ("Telephone", &[(0, 400.0, 0.0, 0.71), (4, 1500.0, 4.0, 0.7), (7, 3500.0, 0.0, 0.71)]),
    ("Low Cut at 100 Hz", &[(0, 100.0, 0.0, 0.71)]),
];

pub fn preset(name: &str) -> Option<ChannelEq> {
    PRESETS.iter().find(|p| p.0 == name).map(|p| preset_eq(p.1))
}

fn preset_eq(bands: &[(usize, f32, f32, f32)]) -> ChannelEq {
    let mut eq = ChannelEq { enabled: true, ..ChannelEq::default() };
    for &(i, freq, gain_db, q) in bands {
        let b = &mut eq.bands[i];
        (b.on, b.freq, b.gain_db, b.q) = (true, freq, gain_db, q);
    }
    eq
}

fn preset_selector(ctx: &Ctx, id: Option<Id>, power: &gtk::ToggleButton) -> gtk::Widget {
    let names = PRESETS.iter().map(|p| p.0.to_string()).collect();
    let load = {
        let (ctx, power) = (ctx.clone(), power.clone());
        Rc::new(move |pick: &crate::presets::Pick| {
            let eq = match pick {
                crate::presets::Pick::Default => ChannelEq { enabled: true, ..ChannelEq::default() },
                crate::presets::Pick::Factory(i) => preset_eq(PRESETS[*i].1),
                crate::presets::Pick::User(path) => {
                    let Some(eq) = std::fs::read(path).ok().and_then(|d| serde_json::from_slice::<ChannelEq>(&d).ok()) else { return };
                    eq
                }
            };
            ctx.edit("Channel EQ Preset", |p| {
                if let Some(ch) = p.channel_mut(id) {
                    ch.eq = eq.clone();
                }
            });
            power.set_active(eq.enabled);
        })
    };
    let save = {
        let ctx = ctx.clone();
        Rc::new(move || serde_json::to_vec_pretty(&eq_of(&ctx.s().project, id)).unwrap_or_default())
    };
    let current = eq_of(&ctx.s().project, id);
    let initial = PRESETS.iter().position(|p| preset_eq(p.1).bands == current.bands);
    crate::presets::selector(names, initial, crate::presets::dir("channel-eq"), "json", load, save)
}
