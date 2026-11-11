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
    let c = design(eq, fs);
    let b = gsk::PathBuilder::new();
    let steps = (w as usize / 2).max(32);
    for i in 0..=steps {
        let x = w * i as f32 / steps as f32;
        let f = f_of(x, w);
        let db = match band {
            Some(k) => band_response_db(k, &eq.bands[k], f, fs),
            None => response_db(&c, f, fs),
        };
        let y = y_of(db.clamp(-RANGE_DB * 2.0, RANGE_DB * 2.0), h);
        if i == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
    }
    b.to_path()
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
        s.push_clip(&graphene::Rect::new(0.0, 1.0, w, h - 2.0));
        s.append_stroke(&curve(&eq, fs, w, h, None), &gsk::Stroke::new(1.5), &canvas::accent(c));
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
    static WINDOWS: RefCell<HashMap<Option<Id>, adw::Window>> = RefCell::default();
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

/// Logic-style Channel EQ editor for one channel. Reopening brings the existing window forward.
pub fn open_editor(ctx: &Ctx, id: Option<Id>, anchor: &impl IsA<gtk::Widget>) {
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&id).cloned()) {
        w.present();
        return;
    }
    let title = format!("{} — Channel EQ", channel_name(&ctx.s().project, id));
    let win = adw::Window::builder().title(&title).default_width(900).default_height(520).build();
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

    let graph = graph(ctx, id, &selected, &analyzer);
    let bands = band_controls(ctx, id, &selected);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&graph);
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
    win.connect_close_request(move |_| {
        strip.scope_on.store(false, Relaxed);
        WINDOWS.with(|w| w.borrow_mut().remove(&id));
        glib::Propagation::Proceed
    });
    WINDOWS.with(|w| w.borrow_mut().insert(id, win.clone()));
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
    c.set_size_request(640, 300);

    // Redraw on edits; when the analyser runs, refresh the spectrum about 30 times a second.
    redraw_on_change(ctx, &c, move |p| fingerprint(&eq_of(p, id)));
    {
        let (ctx, an, spec) = (ctx.clone(), analyzer.clone(), spectrum.clone());
        let frame = Cell::new(0u32);
        c.add_tick_callback(move |c, _| {
            frame.set(frame.get().wrapping_add(1));
            if an.get() && frame.get() % 2 == 0 {
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

    let start = Rc::new(Cell::new(None::<(usize, f32, f32)>));
    let g = gtk::GestureDrag::new();
    {
        let (ctx, sel, start, c2) = (ctx.clone(), selected.clone(), start.clone(), c.clone());
        g.connect_drag_begin(move |g, x, y| {
            let (w, h) = (c2.width() as f32, c2.height() as f32);
            let Some(k) = nearest(&ctx, x, y, w, h) else {
                sel.set(None);
                start.set(None);
                c2.queue_draw();
                return;
            };
            sel.set(Some(k));
            let alt = g.current_event_state().contains(gdk::ModifierType::ALT_MASK);
            if alt {
                let def = ChannelEq::default().bands[k];
                edit_band(&ctx, id, k, |b| *b = EqBand { on: true, ..def });
                start.set(None);
                return;
            }
            let b = eq_of(&ctx.s().project, id).bands[k];
            ctx.begin_gesture();
            set_band(&ctx, id, k, |b| b.on = true);
            start.set(Some((k, b.freq, b.gain_db)));
            c2.queue_draw();
        });
    }
    {
        let (ctx, start, c2) = (ctx.clone(), start.clone(), c.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some((k, f0, g0)) = start.get() else { return };
            let (w, h) = (c2.width() as f32, c2.height() as f32);
            let fine = if g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) { 0.15 } else { 1.0 };
            let decades = (F_MAX / F_MIN).log10() as f32;
            let freq = (f0 * 10f32.powf(dx as f32 / w * decades * fine)).clamp(F_MIN as f32, F_MAX as f32);
            let gain = (g0 - dy as f32 / (h / 2.0 - PAD) * RANGE_DB as f32 * fine).clamp(-RANGE_DB as f32, RANGE_DB as f32);
            set_band(&ctx, id, k, |b| {
                b.freq = freq;
                if !EqKind::of(k).is_cut() {
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

fn fmt_freq(f: f32) -> String {
    if f >= 1000.0 { format!("{:.2}k", f / 1000.0) } else { format!("{f:.0}") }
}

/// One column of fields per band, kept in step with drags and undo.
fn band_controls(ctx: &Ctx, id: Option<Id>, selected: &Rc<Cell<Option<usize>>>) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_homogeneous(true);
    row.set_margin_start(8);
    row.set_margin_end(8);
    row.set_margin_top(8);
    row.set_margin_bottom(8);
    let mut syncs: Vec<Box<dyn Fn(&EqBand)>> = Vec::new();
    for k in 0..EQ_BANDS {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let kind = EqKind::of(k);
        let on = gtk::ToggleButton::with_label(NAMES[k]);
        on.add_css_class("eq-band-toggle");
        on.add_css_class(&format!("eq-band-{k}"));
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

        {
            let ctx = ctx.clone();
            on.connect_toggled(move |b| {
                let v = b.is_active();
                if eq_of(&ctx.s().project, id).bands[k].on != v {
                    edit_band(&ctx, id, k, |b| b.on = v);
                }
            });
        }
        {
            let ctx = ctx.clone();
            freq.connect_value_changed(move |s| {
                let v = s.value() as f32;
                if (eq_of(&ctx.s().project, id).bands[k].freq - v).abs() > 0.5 {
                    edit_band(&ctx, id, k, |b| b.freq = v);
                }
            });
        }
        {
            let ctx = ctx.clone();
            gain.connect_value_changed(move |s| {
                let v = s.value() as f32;
                if (eq_of(&ctx.s().project, id).bands[k].gain_db - v).abs() > 0.05 {
                    edit_band(&ctx, id, k, |b| b.gain_db = v);
                }
            });
        }
        {
            let ctx = ctx.clone();
            q.connect_value_changed(move |s| {
                let v = s.value() as f32;
                if (eq_of(&ctx.s().project, id).bands[k].q - v).abs() > 0.005 {
                    edit_band(&ctx, id, k, |b| b.q = v);
                }
            });
        }
        {
            let ctx = ctx.clone();
            slope.connect_selected_notify(move |d| {
                let v = EQ_SLOPES[d.selected() as usize];
                if eq_of(&ctx.s().project, id).bands[k].slope != v {
                    edit_band(&ctx, id, k, |b| b.slope = v);
                }
            });
        }
        for w in [freq.upcast_ref::<gtk::Widget>(), gain.upcast_ref(), q.upcast_ref(), slope.upcast_ref()] {
            let focus = gtk::EventControllerFocus::new();
            let sel = selected.clone();
            focus.connect_enter(move |_| sel.set(Some(k)));
            w.add_controller(focus);
        }

        col.append(&on);
        col.append(&freq);
        if kind.is_cut() {
            col.append(&slope);
        } else {
            col.append(&gain);
            col.append(&q);
        }
        row.append(&col);

        let (on, freq, gain, q, slope) = (on.clone(), freq.clone(), gain.clone(), q.clone(), slope.clone());
        syncs.push(Box::new(move |b: &EqBand| {
            if on.is_active() != b.on {
                on.set_active(b.on);
            }
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
        }));
    }
    let syncs = Rc::new(syncs);
    let sync = {
        let (ctx, syncs) = (ctx.clone(), syncs.clone());
        move || {
            let eq = eq_of(&ctx.s().project, id);
            for (f, b) in syncs.iter().zip(eq.bands.iter()) {
                f(b);
            }
        }
    };
    sync();
    // Follows drags (which don't emit project changes until release) as well as undo.
    let last = Cell::new(f32::NAN);
    let ctx = ctx.clone();
    row.add_tick_callback(move |_, _| {
        let fp = fingerprint(&eq_of(&ctx.s().project, id));
        if fp.to_bits() != last.get().to_bits() {
            last.set(fp);
            sync();
        }
        glib::ControlFlow::Continue
    });
    row.upcast()
}
