//! The audio region editor, like Logic's Audio Track Editor: the region in place on the
//! timeline with zoom, a bar ruler, one lane per channel, selection, trimming, fades and the
//! usual audio functions. Edits that change samples write a new file; the original is kept.

use crate::canvas::{self, Canvas, rect, rgba, text};
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_engine::audio::write_wav;
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

const RULER_H: f32 = 22.0;
const HANDLE: f64 = 6.0;
const FADE_BAND: f64 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// Selecting from this timeline position.
    Select(u64),
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

struct View {
    /// Samples per pixel and the timeline position at the left edge.
    spp: Cell<f64>,
    x0: Cell<f64>,
    /// Waveform magnification.
    vzoom: Cell<f32>,
    /// Selected span on the timeline.
    sel: Cell<Option<(u64, u64)>>,
    drag: Cell<Drag>,
    /// Where a drag is now, as a timeline position.
    at: Cell<u64>,
    /// The region the view was last fitted to.
    fitted: Cell<Option<Id>>,
    fit_pending: Cell<bool>,
    hadj: gtk::Adjustment,
}

impl View {
    fn x(&self, pos: u64) -> f32 {
        ((pos as f64 - self.x0.get()) / self.spp.get()) as f32
    }

    fn pos(&self, x: f64) -> u64 {
        (self.x0.get() + x * self.spp.get()).max(0.0) as u64
    }
}

fn region(ctx: &Ctx) -> Option<(Region, u8)> {
    let s = ctx.s();
    let (ti, r) = s.selected_region.and_then(|id| s.project.find_region(id))?;
    matches!(r.content, Content::Audio(_)).then(|| (r.clone(), s.project.tracks[ti].color))
}

fn audio(r: &Region) -> &AudioContent {
    match &r.content {
        Content::Audio(a) => a,
        Content::Midi(_) => unreachable!("only audio regions reach the audio editor"),
    }
}

fn edit_audio(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Region)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        if let Some(r) = p.region_mut(id) {
            f(r);
        }
    });
}

pub fn build(ctx: &Ctx) -> gtk::Widget {
    let view = Rc::new(View {
        spp: Cell::new(100.0),
        x0: Cell::new(0.0),
        vzoom: Cell::new(1.0),
        sel: Cell::new(None),
        drag: Cell::new(Drag::None),
        at: Cell::new(0),
        fitted: Cell::new(None),
        fit_pending: Cell::new(false),
        hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
    });

    let ruler = {
        let (ctx, view) = (ctx.clone(), view.clone());
        Canvas::new(move |c, s, w, h| draw_ruler(&ctx, &view, c, s, w, h))
    };
    ruler.set_size_request(-1, RULER_H as i32);
    let wave = {
        let (ctx, view) = (ctx.clone(), view.clone());
        Canvas::new(move |c, s, w, h| draw_wave(&ctx, &view, c, s, w, h))
    };
    wave.set_vexpand(true);
    wave.set_hexpand(true);
    wave.set_focusable(true);
    let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&view.hadj));

    let redraw: Rc<dyn Fn()> = {
        let (ruler, wave) = (ruler.clone(), wave.clone());
        Rc::new(move || {
            ruler.queue_draw();
            wave.queue_draw();
        })
    };
    let (bar, title) = toolbar(ctx, &view, &wave, &redraw);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&bar);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    col.append(&ruler);
    col.append(&wave);
    col.append(&hbar);

    {
        let (view, redraw) = (view.clone(), redraw.clone());
        view.hadj.clone().connect_value_changed(move |a| {
            view.x0.set(a.value());
            redraw();
        });
    }
    // A newly selected region is fitted into view; edits to it keep the view where it is.
    let refresh = {
        let (ctx, view, wave, redraw) = (ctx.clone(), view.clone(), wave.clone(), redraw.clone());
        move || {
            let Some((r, _)) = region(&ctx) else { return };
            title.set_text(&format!("{} · {:.2} s", r.name, ctx.s().project.time().seconds(r.length)));
            if view.fitted.get() != Some(r.id) {
                view.fitted.set(Some(r.id));
                view.sel.set(None);
                view.vzoom.set(1.0);
                // Fitted on the next draw, once the canvas knows its width.
                view.fit_pending.set(true);
            }
            update_scroll(&ctx, &view, &r, wave.width());
            redraw();
        }
    };
    {
        let (wave, refresh) = (wave.clone(), refresh.clone());
        wave.connect_map(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        ctx.on(move |c| {
            if matches!(c, Change::Project | Change::Selection | Change::Peaks) {
                refresh();
            }
        });
    }
    {
        // The playhead moves without project changes, so redraw on a timer while visible.
        let redraw = redraw.clone();
        let w = wave.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            if w.is_mapped() {
                redraw();
            }
            glib::ControlFlow::Continue
        });
    }

    install_input(ctx, &view, &wave, &redraw);
    install_scroll(ctx, &view, &wave, &redraw);
    install_scroll(ctx, &view, &ruler, &redraw);
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        let click = gtk::GestureClick::new();
        click.connect_pressed(move |_, _, x, _| ctx.locate(view.pos(x)));
        ruler.add_controller(click);
    }
    col.upcast()
}

/// Shows the region with a little room either side.
fn fit(view: &View, r: &Region, width: i32) {
    let w = width.max(200) as f64;
    let span = r.span_end() - r.start;
    let margin = span as f64 * 0.05;
    view.spp.set(((span as f64 + 2.0 * margin) / w).max(0.05));
    view.x0.set((r.start as f64 - margin).max(0.0));
}

fn update_scroll(ctx: &Ctx, view: &View, r: &Region, width: i32) {
    let page = width.max(1) as f64 * view.spp.get();
    let end = (r.span_end() as f64 + page).max(ctx.s().project.length() as f64);
    view.hadj.configure(view.x0.get(), 0.0, end, page * 0.1, page * 0.9, page);
}

fn toolbar(ctx: &Ctx, view: &Rc<View>, wave: &Canvas, redraw: &Rc<dyn Fn()>) -> (gtk::Box, gtk::Label) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    let title = gtk::Label::new(None);
    title.add_css_class("heading");
    bar.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let functions = gtk::MenuButton::builder().label("Functions").always_show_arrow(true).build();
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    type Action = Box<dyn Fn(&Ctx, &View)>;
    let items: Vec<(&str, Action)> = vec![
        ("Select All", Box::new(|c, v| select_all(c, v))),
        ("Normalize", Box::new(|c, _| normalize(c))),
        ("Reverse", Box::new(|c, v| rewrite(c, v, "Reverse", |ch| ch.reverse()))),
        ("Silence", Box::new(|c, v| rewrite(c, v, "Silence", |ch| ch.fill(0.0)))),
        ("Fade In to Selection", Box::new(|c, v| fade_to_selection(c, v, true))),
        ("Fade Out to Selection", Box::new(|c, v| fade_to_selection(c, v, false))),
        ("Trim Region to Selection", Box::new(|c, v| trim_to_selection(c, v))),
        ("Split at Selection", Box::new(|c, v| split_at_selection(c, v, false))),
        ("Delete Selection", Box::new(|c, v| split_at_selection(c, v, true))),
    ];
    for (label, f) in items {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let (ctx, view, pop, redraw) = (ctx.clone(), view.clone(), pop.clone(), redraw.clone());
        b.connect_clicked(move |_| {
            pop.popdown();
            f(&ctx, &view);
            redraw();
        });
        list.append(&b);
    }
    pop.set_child(Some(&list));
    functions.set_popover(Some(&pop));
    bar.append(&functions);

    let zoom = |icon: &str, tip: &str, f: Box<dyn Fn(&View, i32)>| {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        let (ctx, view, wave, redraw) = (ctx.clone(), view.clone(), wave.clone(), redraw.clone());
        b.connect_clicked(move |_| {
            f(&view, wave.width());
            if let Some((r, _)) = region(&ctx) {
                update_scroll(&ctx, &view, &r, wave.width());
            }
            redraw();
        });
        bar.append(&b);
    };
    zoom("zoom-out-symbolic", "Waveform Smaller", Box::new(|v, _| v.vzoom.set((v.vzoom.get() / 1.5).max(1.0))));
    zoom("zoom-in-symbolic", "Waveform Larger", Box::new(|v, _| v.vzoom.set((v.vzoom.get() * 1.5).min(64.0))));
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    zoom("list-remove-symbolic", "Zoom Out (Ctrl-scroll)", Box::new(|v, w| zoom_at(v, 1.5, w as f64 / 2.0)));
    zoom("list-add-symbolic", "Zoom In (Ctrl-scroll)", Box::new(|v, w| zoom_at(v, 1.0 / 1.5, w as f64 / 2.0)));
    {
        let (ctx2, view, wave, redraw) = (ctx.clone(), view.clone(), wave.clone(), redraw.clone());
        let b = gtk::Button::from_icon_name("zoom-fit-best-symbolic");
        b.add_css_class("flat");
        b.set_tooltip_text(Some("Fit Region"));
        b.connect_clicked(move |_| {
            if let Some((r, _)) = region(&ctx2) {
                fit(&view, &r, wave.width());
                update_scroll(&ctx2, &view, &r, wave.width());
                redraw();
            }
        });
        bar.append(&b);
    }
    (bar, title)
}

fn zoom_at(view: &View, factor: f64, x: f64) {
    let anchor = view.x0.get() + x * view.spp.get();
    let spp = (view.spp.get() * factor).clamp(0.05, 1e5);
    view.spp.set(spp);
    view.x0.set((anchor - x * spp).max(0.0));
}

fn draw_ruler(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let st = ctx.s();
    let time = st.project.time();
    let fg = canvas::fg(c);
    rect(s, 0.0, h - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15));
    let bar_px = time.bar_len() as f64 / view.spp.get();
    let step = (48.0 / bar_px).ceil().max(1.0) as u64;
    for b in time.bars(view.pos(0.0), view.pos(w as f64)) {
        if b.bar % step == 0 {
            let x = view.x(b.sample);
            rect(s, x, h - 10.0, 1.0, 10.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.5));
            text(s, c, x + 3.0, 2.0, &(b.bar + 1).to_string(), &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.8), Some(8.0));
        }
        // Beats once there is room for them.
        if bar_px > 120.0 {
            let beat = PPQ as u64 * 4 / b.den as u64;
            for k in 1..b.num as u64 {
                let x = view.x(time.ticks_to_samples(b.tick + k * beat));
                rect(s, x, h - 5.0, 1.0, 5.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.35));
            }
        }
    }
    if let Some((a, b)) = view.sel.get() {
        let (x0, x1) = (view.x(a), view.x(b));
        rect(s, x0, 0.0, x1 - x0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
    }
    let x = view.x(ctx.position());
    rect(s, x - 4.0, h - 6.0, 9.0, 6.0, &fg);
}

/// Region bounds and fades as a drag in progress would leave them.
fn shape(view: &View, r: &Region, a: &AudioContent, frames: u64) -> (u64, u64, u64, u64) {
    let (mut start, mut end, mut fi, mut fo) = (r.start, r.end(), a.fade_in, a.fade_out);
    let at = view.at.get();
    // Trimming can't reach before the file starts or after it ends.
    let earliest = r.start.saturating_sub(a.offset);
    let latest = r.start + frames.saturating_sub(a.offset);
    match view.drag.get() {
        Drag::TrimStart => start = at.clamp(earliest, end.saturating_sub(1)),
        Drag::TrimEnd => end = at.clamp(start + 1, latest.max(start + 1)),
        Drag::FadeIn => fi = at.saturating_sub(start).min(end - start),
        Drag::FadeOut => fo = end.saturating_sub(at).min(end - start),
        _ => {}
    }
    (start, end, fi, fo)
}

fn draw_wave(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    let Some((r, color)) = region(ctx) else { return };
    if view.fit_pending.replace(false) {
        fit(view, &r, w as i32);
        let (ctx, view_x0, view_spp) = (ctx.clone(), view.x0.get(), view.spp.get());
        let (hadj, page) = (view.hadj.clone(), w as f64 * view_spp);
        // The scrollbar can't change during drawing; it follows just after.
        glib::idle_add_local_once(move || {
            let end = (r.span_end() as f64 + page).max(ctx.s().project.length() as f64);
            hadj.configure(view_x0, 0.0, end, page * 0.1, page * 0.9, page);
        });
        return c.queue_draw();
    }
    let a = audio(&r);
    let time = ctx.s().project.time();
    let base = canvas::track_color(color);
    let dim = |alpha: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), alpha);

    for b in time.bars(view.pos(0.0), view.pos(w as f64)) {
        rect(s, view.x(b.sample), 0.0, 1.0, h, &dim(0.07));
    }
    let frames = ctx.m().audio(&a.file).map_or(r.length + a.offset, |d| d.frames);
    let (start, end, fade_in, fade_out) = shape(view, &r, a, frames);
    let (x0, x1) = (view.x(start), view.x(end));
    rect(s, x0, 0.0, x1 - x0, h, &gdk::RGBA::new(base.red(), base.green(), base.blue(), 0.14));

    // While trimming, show the audio that would come back into the region, dimmed.
    let shown = (view.x(r.start.saturating_sub(a.offset)).max(0.0), view.x(r.start + frames.saturating_sub(a.offset)).min(w));
    let peaks = ctx.m().peaks_for(&a.file);
    let pieces = a.pieces(r.length);
    let channels = peaks.as_ref().map_or(1, |p| p.channel_count());
    let lane_h = h / channels as f32;
    let gain = db_to_gain(a.gain_db) * view.vzoom.get();
    for ch in 0..channels {
        let y = ch as f32 * lane_h;
        rect(s, 0.0, y + lane_h / 2.0, w, 1.0, &dim(0.18));
        if ch > 0 {
            rect(s, 0.0, y, w, 1.0, &dim(0.12));
        }
        let Some(p) = peaks.as_ref() else { continue };
        if a.is_folder() {
            for piece in &pieces {
                let Some(pk) = ctx.m().peaks_for(piece.file) else { continue };
                let (px0, px1) = (view.x(r.start + piece.start).max(0.0), view.x(r.start + piece.end).min(w));
                let frame = piece.offset as f64 + (view.pos(px0 as f64) as f64 - (r.start + piece.start) as f64);
                let cols = (px1 - px0).max(0.0) as usize;
                let data: Vec<(f32, f32)> =
                    pk.channel_columns(ch, frame, view.spp.get(), cols).into_iter().map(|(lo, hi)| (lo * gain, hi * gain)).collect();
                canvas::waveform(s, px0, y + 4.0, lane_h - 8.0, &data, &base);
            }
            continue;
        }
        let (sx0, sx1) = shown;
        let frame = a.offset as f64 + (view.x0.get() + sx0 as f64 * view.spp.get() - r.start as f64);
        let cols = (sx1 - sx0).max(0.0) as usize;
        let data: Vec<(f32, f32)> = p
            .channel_columns(ch, frame, view.spp.get(), cols)
            .into_iter()
            .enumerate()
            .map(|(i, (lo, hi))| {
                // Fades shape what's drawn the way they shape what's heard.
                let pos = view.pos(sx0 as f64 + i as f64);
                let inside = pos >= start && pos < end;
                let mut g = if inside { gain } else { gain * 0.35 };
                if inside && fade_in > 0 && pos < start + fade_in {
                    g *= (pos - start) as f32 / fade_in as f32;
                }
                if inside && fade_out > 0 && pos + fade_out > end {
                    g *= (end - pos) as f32 / fade_out as f32;
                }
                (lo * g, hi * g)
            })
            .collect();
        canvas::waveform(s, sx0, y + 4.0, lane_h - 8.0, &data, &base);
    }

    // Fade ramps and their handles at the region's top corners.
    let ink = dim(0.7);
    for (fx, fw, rising) in [(x0, view.x(start + fade_in) - x0, true), (view.x(end - fade_out), x1 - view.x(end - fade_out), false)] {
        if fw >= 1.0 {
            let b = gtk::gsk::PathBuilder::new();
            if rising {
                b.move_to(fx, h);
                b.line_to(fx + fw, 0.0);
            } else {
                b.move_to(fx, 0.0);
                b.line_to(fx + fw, h);
            }
            s.append_stroke(&b.to_path(), &gtk::gsk::Stroke::new(1.0), &ink);
        }
    }
    rect(s, view.x(start + fade_in) - 3.0, 0.0, 7.0, 7.0, &ink);
    rect(s, view.x(end - fade_out) - 3.0, 0.0, 7.0, 7.0, &ink);
    // Region edges.
    rect(s, x0, 0.0, 2.0, h, &base);
    rect(s, x1 - 2.0, 0.0, 2.0, h, &base);

    let sel = match view.drag.get() {
        Drag::Select(from) => Some((from.min(view.at.get()), from.max(view.at.get()))),
        _ => view.sel.get(),
    };
    if let Some((a0, b0)) = sel {
        let (sx0, sx1) = (view.x(a0), view.x(b0));
        rect(s, sx0, 0.0, (sx1 - sx0).max(1.0), h, &rgba(0xffffff, 0.12));
        text(s, c, sx0 + 4.0, 2.0, &format!("{:.3} s", time.seconds(b0 - a0)), &fg, Some(8.0));
    }
    let px = view.x(ctx.position());
    if px >= 0.0 && px <= w {
        rect(s, px, 0.0, 1.0, h, &fg);
    }
}

fn install_input(ctx: &Ctx, view: &Rc<View>, wave: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, view, wave) = (ctx.clone(), view.clone(), wave.clone());
        g.connect_drag_begin(move |_, x, y| {
            wave.grab_focus();
            let Some((r, _)) = region(&ctx) else { return };
            let a = audio(&r);
            let near = |pos: u64| (view.x(pos) as f64 - x).abs() <= HANDLE;
            let pos = view.pos(x);
            let drag = if y < FADE_BAND && near(r.start + a.fade_in) {
                Drag::FadeIn
            } else if y < FADE_BAND && near(r.end() - a.fade_out) {
                Drag::FadeOut
            } else if near(r.start) {
                Drag::TrimStart
            } else if near(r.end()) {
                Drag::TrimEnd
            } else {
                Drag::Select(pos)
            };
            view.drag.set(drag);
            view.at.set(pos);
        });
    }
    {
        let (view, redraw) = (view.clone(), redraw.clone());
        g.connect_drag_update(move |g, dx, _| {
            if let Some((sx, _)) = g.start_point() {
                view.at.set(view.pos(sx + dx));
                redraw();
            }
        });
    }
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        g.connect_drag_end(move |_, dx, _| {
            let drag = view.drag.replace(Drag::None);
            let Some((r, _)) = region(&ctx) else { return };
            let a = audio(&r).clone();
            let frames = ctx.m().audio(&a.file).map_or(r.length + a.offset, |d| d.frames);
            match drag {
                Drag::Select(from) if dx.abs() < 2.0 => {
                    view.sel.set(None);
                    ctx.locate(from);
                }
                Drag::Select(from) => {
                    let at = view.at.get();
                    view.sel.set(Some((from.min(at).max(r.start), from.max(at).min(r.end()))).filter(|(a, b)| b > a));
                }
                Drag::TrimStart | Drag::TrimEnd | Drag::FadeIn | Drag::FadeOut => {
                    view.drag.set(drag);
                    let (start, end, fi, fo) = shape(&view, &r, &a, frames);
                    view.drag.set(Drag::None);
                    let label = if matches!(drag, Drag::FadeIn | Drag::FadeOut) { "Change Fade" } else { "Resize Region" };
                    edit_audio(&ctx, label, |reg| {
                        let delta = start as i64 - reg.start as i64;
                        reg.start = start;
                        reg.length = end - start;
                        if let Content::Audio(c) = &mut reg.content {
                            if delta != 0 {
                                c.shift_start(delta);
                            }
                            c.fade_in = fi.min(reg.length);
                            c.fade_out = fo.min(reg.length);
                        }
                    });
                }
                Drag::None => {}
            }
            redraw();
        });
    }
    wave.add_controller(g);

    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, view, wave) = (ctx.clone(), view.clone(), wave.clone());
        motion.connect_motion(move |_, x, y| {
            let Some((r, _)) = region(&ctx) else { return };
            let a = audio(&r);
            let near = |pos: u64| (view.x(pos) as f64 - x).abs() <= HANDLE;
            let cursor = if y < FADE_BAND && (near(r.start + a.fade_in) || near(r.end() - a.fade_out)) {
                "pointer"
            } else if near(r.start) || near(r.end()) {
                "col-resize"
            } else {
                "text"
            };
            wave.set_cursor_from_name(Some(cursor));
        });
    }
    wave.add_controller(motion);

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Delete | gdk::Key::BackSpace if view.sel.get().is_some() => split_at_selection(&ctx, &view, true),
                gdk::Key::Escape => view.sel.set(None),
                gdk::Key::a if ctrl => select_all(&ctx, &view),
                _ => return glib::Propagation::Proceed,
            }
            redraw();
            glib::Propagation::Stop
        });
    }
    wave.add_controller(keys);
}

/// Ctrl-scroll zooms around the pointer, Alt-scroll scales the waveform, plain scrolling and
/// the wheel's side buttons move along the timeline.
fn install_scroll(ctx: &Ctx, view: &Rc<View>, widget: &Canvas, redraw: &Rc<dyn Fn()>) {
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let pointer = Rc::new(Cell::new(0.0f64));
    {
        let motion = gtk::EventControllerMotion::new();
        let p = pointer.clone();
        motion.connect_motion(move |_, x, _| p.set(x));
        widget.add_controller(motion);
    }
    let (ctx, view, redraw, w) = (ctx.clone(), view.clone(), redraw.clone(), widget.clone());
    sc.connect_scroll(move |c, dx, dy| {
        let mods = c.current_event_state();
        if mods.contains(gdk::ModifierType::CONTROL_MASK) {
            zoom_at(&view, if dy > 0.0 { 1.25 } else { 0.8 }, pointer.get());
        } else if mods.contains(gdk::ModifierType::ALT_MASK) {
            let z = view.vzoom.get() * if dy > 0.0 { 1.0 / 1.25 } else { 1.25 };
            view.vzoom.set(z.clamp(1.0, 64.0));
        } else {
            let step = (dx + dy) * 40.0 * view.spp.get();
            view.x0.set((view.x0.get() + step).max(0.0));
        }
        if let Some((r, _)) = region(&ctx) {
            update_scroll(&ctx, &view, &r, w.width());
        }
        redraw();
        glib::Propagation::Stop
    });
    widget.add_controller(sc);
}

fn select_all(ctx: &Ctx, view: &View) {
    if let Some((r, _)) = region(ctx) {
        view.sel.set(Some((r.start, r.end())));
    }
}

/// The selection, or the whole region when nothing is selected.
fn span(ctx: &Ctx, view: &View) -> Option<(Region, u64, u64)> {
    let (r, _) = region(ctx)?;
    let (a, b) = view.sel.get().unwrap_or((r.start, r.end()));
    let (a, b) = (a.max(r.start), b.min(r.end()));
    (b > a).then_some((r, a, b))
}

/// Sets the region gain so its loudest sample reaches 0 dBFS.
fn normalize(ctx: &Ctx) {
    let Some((r, _)) = region(ctx) else { return };
    let a = audio(&r);
    let mut peak = 0f32;
    for p in a.pieces(r.length) {
        let Some(d) = ctx.m().audio(p.file) else { continue };
        let end = (p.offset + p.end - p.start).min(d.frames);
        for f in p.offset..end {
            for ch in 0..d.channels {
                peak = peak.max(d.sample(ch, f).abs());
            }
        }
    }
    if peak <= 0.0 {
        ctx.message("The region is silent");
        return;
    }
    let gain = -20.0 * peak.log10();
    edit_audio(ctx, "Normalize", |reg| {
        if let Content::Audio(c) = &mut reg.content {
            c.gain_db = gain;
        }
    });
}

/// Applies `f` to every channel of the selected span in a copy of the region's file, which
/// the region then plays instead.
fn rewrite(ctx: &Ctx, view: &View, label: &str, f: impl Fn(&mut [f32])) {
    let Some((r, a0, b0)) = span(ctx, view) else { return };
    let a = audio(&r).clone();
    if a.is_folder() {
        ctx.message("Flatten the take folder first");
        return;
    }
    let Some(data) = ctx.m().audio(&a.file) else { return };
    let mut channels: Vec<Vec<f32>> =
        (0..data.channels).map(|ch| (0..data.frames).map(|i| data.sample(ch, i)).collect()).collect();
    let from = (a.offset + (a0 - r.start)) as usize;
    let to = ((a.offset + (b0 - r.start)) as usize).min(data.frames as usize);
    for ch in &mut channels {
        f(&mut ch[from..to]);
    }
    let dir = ctx.s().bundle.audio_dir();
    let stem = std::path::Path::new(&a.file).file_stem().map_or("Audio".into(), |s| s.to_string_lossy().into_owned());
    let mut n = 1;
    let mut name = format!("{stem} {label}.wav");
    while dir.join(&name).exists() {
        n += 1;
        name = format!("{stem} {label} {n}.wav");
    }
    if let Err(e) = write_wav(&dir.join(&name), data.sample_rate, &channels) {
        ctx.message(&format!("Could not write the edited audio: {e}"));
        return;
    }
    edit_audio(ctx, label, |reg| {
        if let Content::Audio(c) = &mut reg.content {
            c.file = name;
        }
    });
}

fn fade_to_selection(ctx: &Ctx, view: &View, fade_in: bool) {
    let Some((r, a0, b0)) = span(ctx, view) else { return };
    edit_audio(ctx, if fade_in { "Fade In" } else { "Fade Out" }, |reg| {
        if let Content::Audio(c) = &mut reg.content {
            if fade_in {
                c.fade_in = b0 - r.start;
            } else {
                c.fade_out = r.end() - a0;
            }
        }
    });
}

fn trim_to_selection(ctx: &Ctx, view: &View) {
    let Some((r, a0, b0)) = span(ctx, view) else { return };
    edit_audio(ctx, "Trim Region", |reg| {
        reg.start = a0;
        reg.length = b0 - a0;
        if let Content::Audio(c) = &mut reg.content {
            c.shift_start((a0 - r.start) as i64);
            c.fade_in = c.fade_in.min(reg.length);
            c.fade_out = c.fade_out.min(reg.length);
        }
    });
    view.sel.set(None);
}

/// Splits the region at the selection's edges, optionally removing the part in between.
fn split_at_selection(ctx: &Ctx, view: &View, delete: bool) {
    let Some((r, a0, b0)) = span(ctx, view) else { return };
    let mut keep = r.id;
    ctx.edit(if delete { "Delete Selection" } else { "Split Region" }, |p| {
        let middle = if a0 > r.start { p.split_region(r.id, a0) } else { Some(r.id) };
        if let Some(mid) = middle {
            if b0 < r.end() {
                p.split_region(mid, b0);
            }
            keep = mid;
            if delete {
                p.remove_region(mid);
                keep = r.id;
            }
        }
    });
    view.sel.set(None);
    if !delete {
        ctx.select_regions(vec![keep]);
    }
}
