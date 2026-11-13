//! The audio file editor: the region's whole file with an overview, a ruler in minutes and
//! seconds or samples, an amplitude scale and Anchor and Region rows. Selections are in
//! file frames. Functions that change samples write a new file; the original is kept.

use crate::canvas::{self, Canvas, rect, rgba, text};
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_engine::audio::write_wav;
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

const SCALE_W: f32 = 44.0;
const OVERVIEW_H: f32 = 30.0;
const RULER_H: f32 = 22.0;
const ROW_H: f32 = 18.0;
const HANDLE: f64 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// Selecting from this file frame.
    Select(u64),
    RegionStart,
    RegionEnd,
    /// Sliding the audio under the region; the frame grabbed.
    Slip(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Units {
    Samples,
    Time,
}

struct View {
    /// File frames per pixel and the frame at the left edge of the waveform.
    spp: Cell<f64>,
    x0: Cell<f64>,
    vzoom: Cell<f64>,
    /// Selected frames of the file.
    sel: Cell<Option<(u64, u64)>>,
    drag: Cell<Drag>,
    at: Cell<u64>,
    fitted: Cell<Option<Id>>,
    fit_pending: Cell<bool>,
    units: Cell<Units>,
    looping: Cell<bool>,
    /// The timeline span being previewed with Play.
    preview: Cell<Option<(u64, u64)>>,
}

impl View {
    fn new() -> View {
        View {
            spp: Cell::new(100.0),
            x0: Cell::new(0.0),
            vzoom: Cell::new(1.0),
            sel: Cell::new(None),
            drag: Cell::new(Drag::None),
            at: Cell::new(0),
            fitted: Cell::new(None),
            fit_pending: Cell::new(false),
            units: Cell::new(Units::Time),
            looping: Cell::new(false),
            preview: Cell::new(None),
        }
    }

    fn x(&self, frame: u64) -> f32 {
        SCALE_W + ((frame as f64 - self.x0.get()) / self.spp.get()) as f32
    }

    fn frame(&self, x: f64) -> u64 {
        (self.x0.get() + (x - SCALE_W as f64) * self.spp.get()).max(0.0) as u64
    }
}

/// What the editor works on: the selected region and facts about its file.
struct Target {
    region: Region,
    audio: AudioContent,
    frames: u64,
    channels: usize,
    rate: u32,
}

impl Target {
    /// The file frames the region plays.
    fn span(&self) -> (u64, u64) {
        (self.audio.offset, self.audio.offset + self.region.length)
    }

    /// Where a file frame plays on the timeline, if the region covers it.
    fn timeline(&self, frame: u64) -> Option<u64> {
        let (a, b) = self.span();
        (frame >= a && frame < b).then(|| self.region.start + frame - a)
    }
}

fn target(ctx: &Ctx) -> Option<Target> {
    let (region, audio) = {
        let s = ctx.s();
        let (_, r) = s.selected_region.and_then(|id| s.project.find_region(id))?;
        let Content::Audio(a) = &r.content else { return None };
        (r.clone(), a.clone())
    };
    let data = ctx.m().audio(&audio.file);
    let (frames, channels, rate) = data.map_or((audio.offset + region.length, 1, 48000), |d| (d.frames, d.channels, d.sample_rate));
    Some(Target { region, audio, frames, channels, rate })
}

fn edit_region(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Region)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        if let Some(r) = p.region_mut(id) {
            f(r);
        }
    });
}

fn fmt_pos(units: Units, frames: u64, rate: u32) -> String {
    match units {
        Units::Samples => frames.to_string(),
        Units::Time => {
            let s = frames as f64 / rate.max(1) as f64;
            format!("{}:{:06.3}", (s / 60.0) as u64, s % 60.0)
        }
    }
}

pub fn build(ctx: &Ctx) -> gtk::Widget {
    let view = Rc::new(View::new());
    let canvas = |f: fn(&Ctx, &View, &Canvas, &gtk::Snapshot, f32, f32)| {
        let (ctx, view) = (ctx.clone(), view.clone());
        Canvas::new(move |c, s, w, h| f(&ctx, &view, c, s, w, h))
    };
    let overview = canvas(draw_overview);
    overview.set_size_request(-1, OVERVIEW_H as i32);
    let ruler = canvas(draw_ruler);
    ruler.set_size_request(-1, RULER_H as i32);
    let wave = canvas(draw_wave);
    wave.set_vexpand(true);
    wave.set_hexpand(true);
    wave.set_focusable(true);
    let rows = canvas(draw_rows);
    rows.set_size_request(-1, (ROW_H * 2.0) as i32);

    let redraw: Rc<dyn Fn()> = {
        let ws = [overview.clone(), ruler.clone(), wave.clone(), rows.clone()];
        Rc::new(move || ws.iter().for_each(|w| w.queue_draw()))
    };
    let info = (gtk::Label::new(None), gtk::Label::new(None));
    let bar = toolbar(ctx, &view, &wave, &redraw, &info);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&bar);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    col.append(&overview);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    col.append(&ruler);
    col.append(&wave);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    col.append(&rows);

    let refresh = {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        move || {
            let Some(t) = target(&ctx) else { return };
            if view.fitted.get() != Some(t.region.id) {
                view.fitted.set(Some(t.region.id));
                view.sel.set(None);
                view.vzoom.set(1.0);
                view.fit_pending.set(true);
            }
            redraw();
        }
    };
    {
        let refresh = refresh.clone();
        wave.connect_map(move |_| refresh());
    }
    ctx.on(move |c| {
        if matches!(c, Change::Project | Change::Selection | Change::Peaks) {
            refresh();
        }
    });
    {
        // The playhead, the info display and previews follow the transport.
        let (ctx, view, redraw, wave) = (ctx.clone(), view.clone(), redraw.clone(), wave.clone());
        let last = Cell::new(None);
        glib::timeout_add_local(std::time::Duration::from_millis(33), move || {
            if !wave.is_mapped() {
                return glib::ControlFlow::Continue;
            }
            if let Some((a, b)) = view.preview.get() {
                let pos = ctx.position();
                if !ctx.playing() {
                    view.preview.set(None);
                } else if pos >= b {
                    if view.looping.get() {
                        ctx.locate(a);
                    } else {
                        ctx.stop();
                        view.preview.set(None);
                    }
                }
            }
            // Idle and unchanged: nothing to draw.
            let now = (ctx.position(), ctx.playing(), view.sel.get(), view.units.get() as u8);
            if !now.1 && last.get() == Some(now) {
                return glib::ControlFlow::Continue;
            }
            last.set(Some(now));
            let rate = target(&ctx).map_or(48000, |t| t.rate);
            let (start, len) = view.sel.get().map_or((0, 0), |(a, b)| (a, b - a));
            info.0.set_text(&fmt_pos(view.units.get(), start, rate));
            info.1.set_text(&fmt_pos(view.units.get(), len, rate));
            redraw();
            glib::ControlFlow::Continue
        });
    }

    install_wave_input(ctx, &view, &wave, &redraw);
    install_rows_input(ctx, &view, &rows, &redraw);
    install_overview_input(ctx, &view, &overview, &redraw);
    for w in [&wave, &ruler, &rows, &overview] {
        install_scroll(&view, w, &redraw);
    }
    col.upcast()
}

fn fit(view: &View, from: u64, to: u64, width: f32) {
    let span = to.saturating_sub(from).max(1) as f64;
    let w = (width - SCALE_W).max(100.0) as f64;
    let margin = span * 0.04;
    view.spp.set(((span + 2.0 * margin) / w).max(0.02));
    view.x0.set((from as f64 - margin).max(0.0));
}

fn zoom_at(view: &View, factor: f64, x: f64) {
    let anchor = view.x0.get() + (x - SCALE_W as f64) * view.spp.get();
    let spp = (view.spp.get() * factor).clamp(0.02, 1e5);
    view.spp.set(spp);
    view.x0.set((anchor - (x - SCALE_W as f64) * spp).max(0.0));
}

/// A toolbar menu button; an empty name makes a separator.
fn menu(label: &str, items: Vec<(&'static str, Rc<dyn Fn()>)>) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().label(label).always_show_arrow(true).build();
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for (name, f) in items {
        if name.is_empty() {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            continue;
        }
        let item = gtk::Button::with_label(name);
        item.add_css_class("flat");
        if let Some(l) = item.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let pop = pop.clone();
        item.connect_clicked(move |_| {
            pop.popdown();
            f();
        });
        list.append(&item);
    }
    pop.set_child(Some(&list));
    b.set_popover(Some(&pop));
    b
}

fn toolbar(ctx: &Ctx, view: &Rc<View>, wave: &Canvas, redraw: &Rc<dyn Fn()>, info: &(gtk::Label, gtk::Label)) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    let act = |f: fn(&Ctx, &View)| -> Rc<dyn Fn()> {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        Rc::new(move || {
            f(&ctx, &view);
            redraw();
        })
    };
    let sep = || ("", Rc::new(|| {}) as Rc<dyn Fn()>);

    let save: Rc<dyn Fn()> = {
        let (ctx, view, wave) = (ctx.clone(), view.clone(), wave.clone());
        Rc::new(move || save_copy(&ctx, &view, &wave))
    };
    let file_info: Rc<dyn Fn()> = {
        let ctx = ctx.clone();
        Rc::new(move || {
            if let Some(t) = target(&ctx) {
                let secs = t.frames as f64 / t.rate as f64;
                let ch = if t.channels == 1 { "mono".to_string() } else { format!("{} channels", t.channels) };
                ctx.message(&format!("{}: {ch}, {} Hz, {secs:.2} s, 32-bit float", t.audio.file, t.rate));
            }
        })
    };
    bar.append(&menu("Audio File", vec![("Save a Copy As…", save), ("File Info", file_info)]));
    bar.append(&menu(
        "Edit",
        vec![
            ("Select All", act(|c, v| select(c, v, true))),
            ("Select Region", act(|c, v| select(c, v, false))),
            sep(),
            ("Trim Region to Selection", act(trim_to_selection)),
            ("Split Region at Selection", act(|c, v| split_at_selection(c, v, false))),
            ("Delete Selection from Region", act(|c, v| split_at_selection(c, v, true))),
        ],
    ));
    let gain: Rc<dyn Fn()> = {
        let (ctx, view, wave) = (ctx.clone(), view.clone(), wave.clone());
        Rc::new(move || change_gain(&ctx, &view, &wave))
    };
    bar.append(&menu(
        "Functions",
        vec![
            ("Normalize", act(|c, v| process(c, v.sel.get(), "Normalize", normalize))),
            ("Change Gain…", gain),
            sep(),
            ("Fade In", act(|c, v| process(c, v.sel.get(), "Fade In", |ch| fade(ch, true)))),
            ("Fade Out", act(|c, v| process(c, v.sel.get(), "Fade Out", |ch| fade(ch, false)))),
            ("Silence", act(|c, v| process(c, v.sel.get(), "Silence", |ch| ch.iter_mut().for_each(|x| x.fill(0.0))))),
            ("Invert", act(|c, v| process(c, v.sel.get(), "Invert", |ch| ch.iter_mut().flat_map(|x| x.iter_mut()).for_each(|s| *s = -*s)))),
            ("Reverse", act(|c, v| process(c, v.sel.get(), "Reverse", |ch| ch.iter_mut().for_each(|x| x.reverse())))),
            ("Remove DC Offset", act(|c, v| process(c, v.sel.get(), "Remove DC Offset", remove_dc))),
        ],
    ));
    let units = |u: Units| -> Rc<dyn Fn()> {
        let view = view.clone();
        Rc::new(move || view.units.set(u))
    };
    let zoom_to = |f: fn(&Ctx, &View) -> Option<(u64, u64)>| -> Rc<dyn Fn()> {
        let (ctx, view, wave, redraw) = (ctx.clone(), view.clone(), wave.clone(), redraw.clone());
        Rc::new(move || {
            if let Some((a, b)) = f(&ctx, &view) {
                fit(&view, a, b, wave.width() as f32);
                redraw();
            }
        })
    };
    bar.append(&menu(
        "View",
        vec![
            ("Ruler in Minutes and Seconds", units(Units::Time)),
            ("Ruler in Samples", units(Units::Samples)),
            sep(),
            ("Zoom to Region", zoom_to(|c, _| target(c).map(|t| t.span()))),
            ("Zoom to Selection", zoom_to(|_, v| v.sel.get())),
            ("Show Whole File", zoom_to(|c, _| target(c).map(|t| (0, t.frames)))),
        ],
    ));

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.set_tooltip_text(Some("Play Selection"));
    play.add_css_class("suggested-action");
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        play.connect_clicked(move |_| preview(&ctx, &view));
    }
    bar.append(&play);
    let lp = gtk::ToggleButton::builder().icon_name("media-playlist-repeat-symbolic").tooltip_text("Loop Playback").build();
    {
        let view = view.clone();
        lp.connect_toggled(move |b| view.looping.set(b.is_active()));
    }
    bar.append(&lp);

    // The info display: where the selection starts and how long it is.
    let display = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    display.add_css_class("card");
    display.set_margin_start(4);
    for (title, l) in [("Start", &info.0), ("Length", &info.1)] {
        let cap = gtk::Label::new(Some(title));
        cap.add_css_class("dim-label");
        cap.add_css_class("caption");
        cap.set_margin_start(6);
        l.add_css_class("numeric");
        l.set_width_chars(10);
        l.set_xalign(1.0);
        l.set_margin_end(6);
        display.append(&cap);
        display.append(l);
    }
    bar.append(&display);

    let slider = |icon: &str, tip: &str, range: (f64, f64), f: Box<dyn Fn(&View, f64, f32)>| {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        b.set_tooltip_text(Some(tip));
        b.append(&gtk::Image::from_icon_name(icon));
        let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, range.0, range.1, 0.01);
        s.set_draw_value(false);
        s.set_size_request(80, -1);
        let (view, wave, redraw) = (view.clone(), wave.clone(), redraw.clone());
        s.connect_value_changed(move |s| {
            f(&view, s.value(), wave.width() as f32);
            redraw();
        });
        b.append(&s);
        bar.append(&b);
    };
    // Both log scales: waveform height from 1x to 32x, and zoom from far out to sample level.
    slider("object-flip-vertical-symbolic", "Waveform Height", (0.0, 5.0), Box::new(|v, x, _| v.vzoom.set(2f64.powf(x))));
    slider(
        "object-flip-horizontal-symbolic",
        "Zoom",
        (0.0, 1.0),
        Box::new(|v, x, w| {
            let half = (w - SCALE_W) as f64 / 2.0;
            let centre = v.x0.get() + half * v.spp.get();
            v.spp.set(4000f64.powf(1.0 - x) * 0.05);
            v.x0.set((centre - half * v.spp.get()).max(0.0));
        }),
    );
    bar
}

fn draw_overview(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let Some(t) = target(ctx) else { return };
    let fg = canvas::fg(c);
    let dim = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.25));
    let ow = (w - SCALE_W).max(1.0);
    let fpp = t.frames as f64 / ow as f64;
    if let Some(p) = ctx.m().peaks_for(&t.audio.file) {
        canvas::waveform(s, SCALE_W, 3.0, h - 6.0, &p.columns(0.0, fpp, ow as usize), &dim(0.55));
    }
    let ox = |f: u64| SCALE_W + (f as f64 / fpp) as f32;
    let (a, b) = t.span();
    rect(s, ox(a), h - 3.0, ox(b) - ox(a), 3.0, &rgba(0x3584e4, 1.0));
    // The part shown below, boxed.
    let (x0, x1) = (ox(view.frame(SCALE_W as f64)), ox(view.frame(w as f64).min(t.frames)));
    rect(s, x0, 0.0, (x1 - x0).max(2.0), h, &dim(0.12));
    rect(s, x0, 0.0, 1.0, h, &dim(0.7));
    rect(s, x1, 0.0, 1.0, h, &dim(0.7));
}

/// Ruler tick spacing: a round number of frames at least `min_frames` apart.
fn tick_step(min_frames: f64, rate: u32, units: Units) -> u64 {
    let mut steps: Vec<f64> = match units {
        Units::Samples => [1.0, 2.0, 5.0].iter().flat_map(|m| (0..9).map(move |e| m * 10f64.powi(e))).collect(),
        Units::Time => [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0]
            .iter()
            .map(|s| s * rate as f64)
            .collect(),
    };
    steps.sort_by(|a, b| a.total_cmp(b));
    steps.into_iter().find(|s| *s >= min_frames).unwrap_or(1e9).max(1.0) as u64
}

fn draw_ruler(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let Some(t) = target(ctx) else { return };
    let fg = canvas::fg(c);
    let dim = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    rect(s, 0.0, h - 1.0, w, 1.0, &dim(0.15));
    if let Some((a, b)) = view.sel.get() {
        let (x0, x1) = (view.x(a).max(SCALE_W), view.x(b));
        rect(s, x0, 0.0, (x1 - x0).max(0.0), h, &dim(0.12));
    }
    let step = tick_step(90.0 * view.spp.get(), t.rate, view.units.get());
    let mut f = (view.x0.get() as u64 / step) * step;
    while view.x(f) < w {
        let x = view.x(f);
        if x >= SCALE_W {
            rect(s, x, h - 8.0, 1.0, 8.0, &dim(0.5));
            text(s, c, x + 3.0, 2.0, &fmt_pos(view.units.get(), f, t.rate), &dim(0.75), Some(7.5));
        }
        let minor = view.x(f + step / 2);
        if minor >= SCALE_W {
            rect(s, minor, h - 4.0, 1.0, 4.0, &dim(0.3));
        }
        f += step;
    }
    if let Some(frame) = playhead_frame(ctx, &t) {
        let x = view.x(frame);
        if x >= SCALE_W {
            rect(s, x - 4.0, h - 6.0, 9.0, 6.0, &fg);
        }
    }
}

fn playhead_frame(ctx: &Ctx, t: &Target) -> Option<u64> {
    let pos = ctx.position();
    (pos >= t.region.start && pos < t.region.end()).then(|| t.audio.offset + pos - t.region.start)
}

fn draw_wave(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let Some(t) = target(ctx) else { return };
    if view.fit_pending.replace(false) {
        let (a, b) = t.span();
        fit(view, a, b, w);
        return c.queue_draw();
    }
    let fg = canvas::fg(c);
    let dim = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    rect(s, SCALE_W, 0.0, w - SCALE_W, h, &rgba(0x000000, 0.22));

    let sel = match view.drag.get() {
        Drag::Select(from) => Some((from.min(view.at.get()), from.max(view.at.get()))),
        _ => view.sel.get(),
    };
    if let Some((a, b)) = sel {
        let x0 = view.x(a).max(SCALE_W);
        rect(s, x0, 0.0, (view.x(b) - x0).max(0.0), h, &dim(0.13));
    }
    // The region's edges, so it's clear which audio plays.
    let (ra, rb) = t.span();
    for f in [ra, rb] {
        let x = view.x(f);
        if x >= SCALE_W {
            rect(s, x, 0.0, 1.0, h, &rgba(0x3584e4, 0.7));
        }
    }

    let peaks = ctx.m().peaks_for(&t.audio.file);
    let channels = peaks.as_ref().map_or(t.channels, |p| p.channel_count()).max(1);
    let lane = h / channels as f32;
    let vz = view.vzoom.get() as f32;
    let cols = (w - SCALE_W).max(0.0) as usize;
    let grey = dim(0.5);
    let bright = rgba(0xffffff, 0.95);
    for ch in 0..channels {
        let y = ch as f32 * lane;
        if ch > 0 {
            rect(s, 0.0, y, w, 1.0, &dim(0.2));
        }
        rect(s, SCALE_W, y + lane / 2.0, w - SCALE_W, 1.0, &dim(0.2));
        // The amplitude scale, in percent.
        let half = lane / 2.0 - 4.0;
        for pct in [100i32, 50, 0, -50, -100] {
            if half < 30.0 && pct.abs() == 50 {
                continue;
            }
            let ly = y + lane / 2.0 - pct as f32 / 100.0 * half;
            rect(s, SCALE_W - 5.0, ly, 5.0, 1.0, &dim(0.4));
            text(s, c, 2.0, ly - 6.0, &format!("{}", (pct as f32 / vz).round()), &dim(0.55), Some(6.5));
        }
        let Some(p) = peaks.as_ref() else { continue };
        let data: Vec<(f32, f32)> =
            p.channel_columns(ch, view.x0.get(), view.spp.get(), cols).into_iter().map(|(lo, hi)| (lo * vz, hi * vz)).collect();
        canvas::waveform(s, SCALE_W, y + 4.0, lane - 8.0, &data, &grey);
        // The selected part again, bright.
        if let Some((a, b)) = sel {
            let (x0, x1) = (view.x(a).max(SCALE_W), view.x(b).min(w));
            let i0 = ((x0 - SCALE_W).max(0.0) as usize).min(data.len());
            let i1 = ((x1 - SCALE_W).max(0.0) as usize).min(data.len());
            if i1 > i0 {
                canvas::waveform(s, SCALE_W + i0 as f32, y + 4.0, lane - 8.0, &data[i0..i1], &bright);
            }
        }
    }
    rect(s, SCALE_W - 1.0, 0.0, 1.0, h, &dim(0.2));
    if let Some(frame) = playhead_frame(ctx, &t) {
        let x = view.x(frame);
        if x >= SCALE_W && x <= w {
            rect(s, x, 0.0, 1.0, h, &fg);
        }
    }
}

/// The Anchor and Region rows under the waveform.
fn draw_rows(ctx: &Ctx, view: &View, c: &Canvas, s: &gtk::Snapshot, w: f32, _h: f32) {
    let Some(t) = target(ctx) else { return };
    let fg = canvas::fg(c);
    let dim = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    text(s, c, 4.0, 3.0, "Anchor", &dim(0.6), Some(7.0));
    text(s, c, 4.0, ROW_H + 3.0, "Region", &dim(0.6), Some(7.0));
    rect(s, SCALE_W, ROW_H, w - SCALE_W, 1.0, &dim(0.1));
    let (mut a, mut b) = t.span();
    let at = view.at.get();
    match view.drag.get() {
        Drag::RegionStart => a = at.min(b - 1),
        Drag::RegionEnd => b = at.max(a + 1).min(t.frames.max(a + 1)),
        Drag::Slip(grab) => {
            let len = b - a;
            a = (a as i64 + at as i64 - grab as i64).clamp(0, t.frames.saturating_sub(len) as i64) as u64;
            b = a + len;
        }
        _ => {}
    }
    // The anchor is the region's start: the point that sits at its timeline position.
    let ax = view.x(a);
    if ax >= SCALE_W {
        let tri = gtk::gsk::PathBuilder::new();
        tri.move_to(ax - 5.0, 3.0);
        tri.line_to(ax + 5.0, 3.0);
        tri.line_to(ax, ROW_H - 3.0);
        tri.close();
        s.append_fill(&tri.to_path(), gtk::gsk::FillRule::Winding, &fg);
    }
    let (x0, x1) = (view.x(a).max(SCALE_W), view.x(b).min(w));
    if x1 > x0 {
        rect(s, x0, ROW_H + 2.0, x1 - x0, ROW_H - 4.0, &rgba(0x3584e4, 1.0));
        text(s, c, x0 + 4.0, ROW_H + 3.0, &t.region.name, &rgba(0xffffff, 0.95), Some(7.5));
    }
}

fn install_wave_input(ctx: &Ctx, view: &Rc<View>, wave: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, view, wave) = (ctx.clone(), view.clone(), wave.clone());
        g.connect_drag_begin(move |g, x, _| {
            wave.grab_focus();
            let Some(t) = target(&ctx) else { return };
            let f = view.frame(x).min(t.frames);
            // Shift extends the selection, keeping its far end.
            let from = match view.sel.get() {
                Some((a, b)) if g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) => {
                    if f.abs_diff(a) > f.abs_diff(b) { a } else { b }
                }
                _ => f,
            };
            view.drag.set(Drag::Select(from));
            view.at.set(f);
        });
    }
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        g.connect_drag_update(move |g, dx, _| {
            let (Some((sx, _)), Some(t)) = (g.start_point(), target(&ctx)) else { return };
            view.at.set(view.frame(sx + dx).min(t.frames));
            redraw();
        });
    }
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        g.connect_drag_end(move |_, dx, _| {
            let Drag::Select(from) = view.drag.replace(Drag::None) else { return };
            let at = view.at.get();
            if dx.abs() < 2.0 && from == at {
                view.sel.set(None);
                if let Some(pos) = target(&ctx).and_then(|t| t.timeline(at)) {
                    ctx.locate(pos);
                }
            } else {
                view.sel.set(Some((from.min(at), from.max(at))).filter(|(a, b)| b > a));
            }
            redraw();
        });
    }
    wave.add_controller(g);
    wave.set_cursor_from_name(Some("text"));

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            match key {
                gdk::Key::Delete | gdk::Key::BackSpace if view.sel.get().is_some() => split_at_selection(&ctx, &view, true),
                gdk::Key::Escape => view.sel.set(None),
                gdk::Key::a if ctrl => select(&ctx, &view, true),
                _ => return glib::Propagation::Proceed,
            }
            redraw();
            glib::Propagation::Stop
        });
    }
    wave.add_controller(keys);
}

/// The Region row: drag an end to trim, or the middle to slide the audio under the region.
fn install_rows_input(ctx: &Ctx, view: &Rc<View>, rows: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        g.connect_drag_begin(move |_, x, _| {
            let Some(t) = target(&ctx) else { return };
            let (a, b) = t.span();
            let near = |f: u64| (view.x(f) as f64 - x).abs() <= HANDLE;
            let f = view.frame(x);
            let drag = if near(a) {
                Drag::RegionStart
            } else if near(b) {
                Drag::RegionEnd
            } else if f > a && f < b {
                Drag::Slip(f)
            } else {
                Drag::None
            };
            view.drag.set(drag);
            view.at.set(f);
        });
    }
    {
        let (view, redraw) = (view.clone(), redraw.clone());
        g.connect_drag_update(move |g, dx, _| {
            if let Some((sx, _)) = g.start_point() {
                view.at.set(view.frame(sx + dx));
                redraw();
            }
        });
    }
    {
        let (ctx, view, redraw) = (ctx.clone(), view.clone(), redraw.clone());
        g.connect_drag_end(move |_, _, _| {
            let drag = view.drag.replace(Drag::None);
            let Some(t) = target(&ctx) else { return };
            let (a, b) = t.span();
            let at = view.at.get();
            match drag {
                Drag::RegionStart => {
                    // The region can't start before the timeline does.
                    let new_a = at.min(b - 1).max(a.saturating_sub(t.region.start));
                    edit_region(&ctx, "Resize Region", |r| {
                        let delta = new_a as i64 - a as i64;
                        r.start = (r.start as i64 + delta) as u64;
                        r.length = b - new_a;
                        if let Content::Audio(c) = &mut r.content {
                            c.shift_start(delta);
                        }
                    });
                }
                Drag::RegionEnd => {
                    let new_b = at.max(a + 1).min(t.frames.max(a + 1));
                    edit_region(&ctx, "Resize Region", |r| r.length = new_b - a);
                }
                Drag::Slip(grab) => {
                    let len = b - a;
                    let new_a = (a as i64 + at as i64 - grab as i64).clamp(0, t.frames.saturating_sub(len) as i64) as u64;
                    edit_region(&ctx, "Slip Region", |r| {
                        if let Content::Audio(c) = &mut r.content {
                            c.offset = new_a;
                        }
                    });
                }
                _ => {}
            }
            redraw();
        });
    }
    rows.add_controller(g);
    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, view, rows) = (ctx.clone(), view.clone(), rows.clone());
        motion.connect_motion(move |_, x, y| {
            let Some(t) = target(&ctx) else { return };
            let (a, b) = t.span();
            let near = |f: u64| (view.x(f) as f64 - x).abs() <= HANDLE;
            let cursor = if y < ROW_H as f64 {
                "default"
            } else if near(a) || near(b) {
                "col-resize"
            } else if x >= view.x(a) as f64 && x <= view.x(b) as f64 {
                "grab"
            } else {
                "default"
            };
            rows.set_cursor_from_name(Some(cursor));
        });
    }
    rows.add_controller(motion);
}

/// Clicking or dragging in the overview brings that part of the file into view.
fn install_overview_input(ctx: &Ctx, view: &Rc<View>, overview: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    let centre = {
        let (ctx, view, overview, redraw) = (ctx.clone(), view.clone(), overview.clone(), redraw.clone());
        Rc::new(move |x: f64| {
            let Some(t) = target(&ctx) else { return };
            let ow = (overview.width() as f32 - SCALE_W).max(1.0) as f64;
            let f = (x - SCALE_W as f64).max(0.0) / ow * t.frames as f64;
            let visible = ow * view.spp.get();
            view.x0.set((f - visible / 2.0).max(0.0));
            redraw();
        })
    };
    {
        let centre = centre.clone();
        g.connect_drag_begin(move |_, x, _| centre(x));
    }
    g.connect_drag_update(move |g, dx, _| {
        if let Some((sx, _)) = g.start_point() {
            centre(sx + dx);
        }
    });
    overview.add_controller(g);
}

/// Ctrl-scroll zooms around the pointer and Alt-scroll changes the waveform height; plain
/// scrolling and the wheel's side buttons move along the file.
fn install_scroll(view: &Rc<View>, widget: &Canvas, redraw: &Rc<dyn Fn()>) {
    let pointer = Rc::new(Cell::new(0.0f64));
    let motion = gtk::EventControllerMotion::new();
    {
        let p = pointer.clone();
        motion.connect_motion(move |_, x, _| p.set(x));
    }
    widget.add_controller(motion);
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let (view, redraw) = (view.clone(), redraw.clone());
    sc.connect_scroll(move |c, dx, dy| {
        let mods = c.current_event_state();
        if mods.contains(gdk::ModifierType::CONTROL_MASK) {
            zoom_at(&view, if dy > 0.0 { 1.25 } else { 0.8 }, pointer.get());
        } else if mods.contains(gdk::ModifierType::ALT_MASK) {
            view.vzoom.set((view.vzoom.get() * if dy > 0.0 { 0.8 } else { 1.25 }).clamp(1.0, 32.0));
        } else {
            view.x0.set((view.x0.get() + (dx + dy) * 40.0 * view.spp.get()).max(0.0));
        }
        redraw();
        glib::Propagation::Stop
    });
    widget.add_controller(sc);
}

fn select(ctx: &Ctx, view: &View, whole_file: bool) {
    if let Some(t) = target(ctx) {
        view.sel.set(Some(if whole_file { (0, t.frames) } else { t.span() }));
    }
}

/// Plays the selection (or the region) through the transport from where it sits on the
/// timeline. Only the part the region covers can be heard.
fn preview(ctx: &Ctx, view: &View) {
    if view.preview.get().is_some() && ctx.playing() {
        ctx.stop();
        view.preview.set(None);
        return;
    }
    let Some(t) = target(ctx) else { return };
    let (ra, rb) = t.span();
    let (a, b) = view.sel.get().unwrap_or((ra, rb));
    let (a, b) = (a.max(ra), b.min(rb));
    if b <= a {
        ctx.message("Only the part of the file inside the region can be played");
        return;
    }
    let (start, end) = (t.region.start + a - ra, t.region.start + b - ra);
    ctx.locate(start);
    view.preview.set(Some((start, end)));
    if !ctx.playing() {
        ctx.play_pause();
    }
}

/// Runs `f` over the selected frames (or the region's) of every channel in a copy of the
/// file, which the region then plays instead.
fn process(ctx: &Ctx, sel: Option<(u64, u64)>, label: &str, f: impl Fn(&mut [&mut [f32]])) {
    let Some(t) = target(ctx) else { return };
    let Some(data) = ctx.m().audio(&t.audio.file) else { return };
    let (a, b) = sel.unwrap_or(t.span());
    let (from, to) = (a.min(t.frames) as usize, b.min(t.frames) as usize);
    if to <= from {
        return;
    }
    let mut channels: Vec<Vec<f32>> = (0..data.channels).map(|ch| (0..data.frames).map(|i| data.sample(ch, i)).collect()).collect();
    {
        let mut spans: Vec<&mut [f32]> = channels.iter_mut().map(|c| &mut c[from..to]).collect();
        f(&mut spans);
    }
    let dir = ctx.s().bundle.audio_dir();
    let stem = std::path::Path::new(&t.audio.file).file_stem().map_or("Audio".into(), |s| s.to_string_lossy().into_owned());
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
    edit_region(ctx, label, |r| {
        if let Content::Audio(c) = &mut r.content {
            c.file = name;
        }
    });
}

fn normalize(ch: &mut [&mut [f32]]) {
    let peak = ch.iter().flat_map(|c| c.iter()).fold(0f32, |m, s| m.max(s.abs()));
    if peak > 0.0 {
        ch.iter_mut().flat_map(|c| c.iter_mut()).for_each(|s| *s /= peak);
    }
}

fn fade(ch: &mut [&mut [f32]], rising: bool) {
    for c in ch.iter_mut() {
        let n = c.len().max(1) as f32;
        for (i, s) in c.iter_mut().enumerate() {
            let g = i as f32 / n;
            *s *= if rising { g } else { 1.0 - g };
        }
    }
}

fn remove_dc(ch: &mut [&mut [f32]]) {
    for c in ch.iter_mut() {
        let mean = c.iter().sum::<f32>() / c.len().max(1) as f32;
        c.iter_mut().for_each(|s| *s -= mean);
    }
}

fn change_gain(ctx: &Ctx, view: &View, wave: &Canvas) {
    let dialog = adw::AlertDialog::new(Some("Change Gain"), Some("Applies to the selection, or to the region when nothing is selected"));
    let spin = gtk::SpinButton::with_range(-48.0, 48.0, 0.5);
    spin.set_digits(1);
    spin.set_value(-3.0);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::Center);
    row.append(&spin);
    row.append(&gtk::Label::new(Some("dB")));
    dialog.set_extra_child(Some(&row));
    dialog.add_responses(&[("cancel", "Cancel"), ("apply", "Change Gain")]);
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    let (ctx, sel) = (ctx.clone(), view.sel.get());
    dialog.connect_response(None, move |_, r| {
        if r == "apply" {
            let g = db_to_gain(spin.value() as f32);
            process(&ctx, sel, "Gain", |ch| ch.iter_mut().flat_map(|c| c.iter_mut()).for_each(|s| *s *= g));
        }
    });
    dialog.present(Some(wave));
}

fn save_copy(ctx: &Ctx, view: &View, wave: &Canvas) {
    let Some(t) = target(ctx) else { return };
    let (a, b) = view.sel.get().unwrap_or(t.span());
    let (from, to) = (a.min(t.frames), b.min(t.frames));
    let d = gtk::FileDialog::builder().title("Save a Copy As").initial_name(format!("{}.wav", t.region.name)).build();
    let win = wave.root().and_downcast::<gtk::Window>();
    let ctx = ctx.clone();
    d.save(win.as_ref(), gio::Cancellable::NONE, move |r| {
        let Some(path) = r.ok().and_then(|f| f.path()) else { return };
        let Some(data) = ctx.m().audio(&t.audio.file) else { return };
        let channels: Vec<Vec<f32>> = (0..data.channels).map(|ch| (from..to).map(|i| data.sample(ch, i)).collect()).collect();
        match write_wav(&path, data.sample_rate, &channels) {
            Ok(()) => ctx.message(&format!("Saved {}", path.display())),
            Err(e) => ctx.message(&format!("Could not save: {e}")),
        }
    });
}

/// The selection as a timeline span, limited to the region.
fn timeline_selection(ctx: &Ctx, view: &View) -> Option<(Region, u64, u64)> {
    let t = target(ctx)?;
    let (ra, rb) = t.span();
    let (a, b) = view.sel.get()?;
    let (a, b) = (a.max(ra), b.min(rb));
    (b > a).then(|| (t.region.clone(), t.region.start + a - ra, t.region.start + b - ra))
}

/// The region becomes exactly the selection, growing if the selection reaches outside it.
fn trim_to_selection(ctx: &Ctx, view: &View) {
    let Some(t) = target(ctx) else { return };
    let Some((a, b)) = view.sel.get() else { return };
    let (ra, _) = t.span();
    let a = a.max(ra.saturating_sub(t.region.start));
    let b = b.min(t.frames);
    if b <= a {
        return;
    }
    edit_region(ctx, "Trim Region", |r| {
        let delta = a as i64 - ra as i64;
        r.start = (r.start as i64 + delta) as u64;
        r.length = b - a;
        if let Content::Audio(c) = &mut r.content {
            c.shift_start(delta);
            c.fade_in = c.fade_in.min(r.length);
            c.fade_out = c.fade_out.min(r.length);
        }
    });
}

/// Splits the region at the selection's edges, optionally removing the part in between.
fn split_at_selection(ctx: &Ctx, view: &View, delete: bool) {
    let Some((r, a, b)) = timeline_selection(ctx, view) else {
        ctx.message("Select part of the region first");
        return;
    };
    let mut keep = r.id;
    ctx.edit(if delete { "Delete Selection" } else { "Split Region" }, |p| {
        let middle = if a > r.start { p.split_region(r.id, a) } else { Some(r.id) };
        if let Some(mid) = middle {
            if b < r.end() {
                p.split_region(mid, b);
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
