use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

pub const TRACK_H: f32 = 64.0;
pub const HEADER_W: i32 = 230;
const RULER_H: f32 = 30.0;
const EDGE: f64 = 6.0;

/// Zoom, scroll and snap shared by the tracks area and the editors.
pub struct View {
    /// Samples per pixel.
    pub spp: Cell<f64>,
    /// Leftmost visible sample.
    pub x0: Cell<f64>,
    pub snap: Cell<u64>,
    pub hadj: gtk::Adjustment,
}

impl View {
    pub fn x(&self, pos: u64) -> f32 {
        ((pos as f64 - self.x0.get()) / self.spp.get()) as f32
    }
    pub fn pos(&self, x: f64) -> u64 {
        (self.x0.get() + x * self.spp.get()).max(0.0) as u64
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Move { region: Id, start: u64, track: usize },
    TrimStart { region: Id },
    TrimEnd { region: Id },
    Cycle { from: u64 },
}

pub struct Tracks {
    pub widget: gtk::Widget,
}

pub fn snap_options() -> Vec<(&'static str, u64)> {
    vec![("Bar", 0), ("Beat", 1), ("1/8", PPQ as u64 / 2), ("1/16", PPQ as u64 / 4), ("Off", u64::MAX)]
}

/// Grid size in ticks for the current snap setting; 0 means no snapping.
pub fn grid_ticks(snap: u64, time: &TimeMap) -> u64 {
    match snap {
        0 => time.ticks_per_bar(),
        1 => time.ticks_per_beat(),
        u64::MAX => 0,
        t => t,
    }
}

pub fn build(ctx: &Ctx, open_editor: Rc<dyn Fn()>) -> Tracks {
    let hadj = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0);
    let sr = ctx.s().project.sample_rate as f64;
    let view = Rc::new(View { spp: Cell::new(sr / 40.0), x0: Cell::new(0.0), snap: Cell::new(1), hadj });
    let drag = Rc::new(Cell::new(Drag::None));
    let offset = Rc::new(Cell::new((0.0f64, 0.0f64)));

    let timeline = {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        Canvas::new(move |c, s, w, h| draw_lanes(&ctx, &view, drag.get(), offset.get(), c, s, w, h))
    };
    timeline.set_hexpand(true);
    timeline.set_focusable(true);

    let ruler = {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        Canvas::new(move |c, s, w, h| draw_ruler(&ctx, &view, drag.get(), offset.get(), c, s, w, h))
    };
    ruler.set_size_request(-1, RULER_H as i32);
    ruler.set_hexpand(true);

    let headers = gtk::Box::new(gtk::Orientation::Vertical, 0);
    headers.set_size_request(HEADER_W, -1);
    headers.add_css_class("track-headers");

    let lanes = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    lanes.append(&headers);
    lanes.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    lanes.append(&timeline);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&lanes)
        .build();

    let playhead = {
        let (ctx, view, timeline) = (ctx.clone(), view.clone(), timeline.clone());
        Canvas::new(move |c, s, _w, h| {
            let origin = timeline.compute_point(c, &gtk::graphene::Point::new(0.0, 0.0)).map_or(0.0, |p| p.x());
            let x = view.x(ctx.position()) + origin;
            if x >= origin {
                let col = if ctx.recording() { rgba(0xe01b24, 1.0) } else { canvas::fg(c) };
                rect(s, x, 0.0, 1.0, h, &col);
            }
        })
    };
    playhead.set_can_target(false);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroller));
    overlay.add_overlay(&playhead);

    let ruler_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let corner = toolbar(ctx, &view, &timeline, &ruler);
    corner.set_hexpand(false);
    headers.set_hexpand(false);
    ruler_row.append(&corner);
    ruler_row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    ruler_row.append(&ruler);

    let hbar_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let hbar_pad = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&view.hadj));
    hbar.set_hexpand(true);
    hbar_row.append(&hbar_pad);
    hbar_row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    hbar_row.append(&hbar);

    // The header column, the toolbar above it and the scrollbar padding share one width so the
    // ruler, lanes and scrollbar line up whatever the headers' natural width turns out to be.
    let column = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    column.add_widget(&corner);
    column.add_widget(&headers);
    column.add_widget(&hbar_pad);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&ruler_row);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&overlay);
    root.append(&hbar_row);

    {
        let (view, t, r) = (view.clone(), timeline.clone(), ruler.clone());
        view.hadj.clone().connect_value_changed(move |a| {
            view.x0.set(a.value());
            t.queue_draw();
            r.queue_draw();
        });
    }

    let redraw = {
        let (t, r) = (timeline.clone(), ruler.clone());
        move || {
            t.queue_draw();
            r.queue_draw();
        }
    };

    rebuild_headers(ctx, &headers, &timeline);
    {
        let (ctx2, headers, timeline, view) = (ctx.clone(), headers.clone(), timeline.clone(), view.clone());
        let redraw = redraw.clone();
        ctx.on(move |c| {
            match c {
                Change::Project => {
                    rebuild_headers(&ctx2, &headers, &timeline);
                    update_scroll(&ctx2, &view, timeline.width());
                }
                Change::Selection => update_header_selection(&ctx2, &headers),
                _ => {}
            }
            redraw();
        });
    }

    install_lane_input(ctx, &view, &timeline, drag.clone(), offset.clone(), open_editor);
    install_ruler_input(ctx, &view, &ruler, &timeline, drag, offset);
    install_zoom(&view, &timeline, &timeline, &ruler);
    install_zoom(&view, &ruler, &timeline, &ruler);
    install_drop(ctx, &view, &timeline);

    {
        let (ctx, view) = (ctx.clone(), view.clone());
        let (t, r) = (timeline.clone(), ruler.clone());
        let frame = Cell::new(0u32);
        playhead.add_tick_callback(move |ph, _| {
            ph.queue_draw();
            let pos = ctx.position();
            let w = t.width() as f64;
            if ctx.playing() && w > 0.0 {
                let x = (pos as f64 - view.x0.get()) / view.spp.get();
                if x > w * 0.95 || x < 0.0 {
                    update_scroll(&ctx, &view, t.width());
                    view.hadj.set_value(pos as f64 - w * 0.05 * view.spp.get());
                }
            }
            frame.set(frame.get().wrapping_add(1));
            if ctx.recording() && frame.get() % 4 == 0 {
                t.queue_draw();
                r.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        timeline.connect_map(move |t| update_scroll(&ctx, &view, t.width()));
    }

    Tracks { widget: root.upcast() }
}

fn update_scroll(ctx: &Ctx, view: &View, width: i32) {
    let s = ctx.s();
    let len = s.project.length().max(s.project.time().bar_len() * 32);
    let page = width.max(1) as f64 * view.spp.get();
    view.hadj.configure(view.x0.get(), 0.0, len as f64 + page, page * 0.1, page * 0.9, page);
}

fn toolbar(ctx: &Ctx, view: &Rc<View>, timeline: &Canvas, ruler: &Canvas) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_valign(gtk::Align::Center);

    let add = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text("New Track").build();
    add.add_css_class("flat");
    let menu = gio::Menu::new();
    menu.append(Some("_Audio Track"), Some("win.add-audio-track"));
    menu.append(Some("_Software Instrument Track"), Some("win.add-instrument-track"));
    menu.append(Some("Aux _Bus"), Some("win.add-bus"));
    add.set_menu_model(Some(&menu));
    bar.append(&add);

    let names: Vec<&str> = snap_options().iter().map(|o| o.0).collect();
    let snap = gtk::DropDown::from_strings(&names);
    snap.set_selected(1);
    snap.set_tooltip_text(Some("Snap"));
    {
        let view = view.clone();
        snap.connect_selected_notify(move |d| view.snap.set(snap_options()[d.selected() as usize].1));
    }
    bar.append(&snap);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    for (icon, factor, tip) in [("zoom-out-symbolic", 1.5, "Zoom Out"), ("zoom-in-symbolic", 1.0 / 1.5, "Zoom In")] {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        b.set_tooltip_text(Some(tip));
        let (view, t, r, ctx) = (view.clone(), timeline.clone(), ruler.clone(), ctx.clone());
        b.connect_clicked(move |_| {
            zoom(&view, factor, 0.0);
            update_scroll(&ctx, &view, t.width());
            t.queue_draw();
            r.queue_draw();
        });
        bar.append(&b);
    }
    bar
}

fn zoom(view: &View, factor: f64, anchor_x: f64) {
    let anchor = view.x0.get() + anchor_x * view.spp.get();
    let spp = (view.spp.get() * factor).clamp(4.0, 48000.0);
    view.spp.set(spp);
    let x0 = (anchor - anchor_x * spp).max(0.0);
    view.x0.set(x0);
    view.hadj.set_value(x0);
}

fn install_zoom(view: &Rc<View>, w: &Canvas, timeline: &Canvas, ruler: &Canvas) {
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let (view, t, r) = (view.clone(), timeline.clone(), ruler.clone());
    let pointer = Rc::new(Cell::new(0.0));
    let motion = gtk::EventControllerMotion::new();
    {
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, _| pointer.set(x));
    }
    w.add_controller(motion);
    sc.connect_scroll(move |sc, dx, dy| {
        let mods = sc.current_event_state();
        if mods.contains(gdk::ModifierType::CONTROL_MASK) {
            zoom(&view, if dy > 0.0 { 1.2 } else { 1.0 / 1.2 }, pointer.get());
        } else if mods.contains(gdk::ModifierType::SHIFT_MASK) || dx != 0.0 {
            let d = if dx != 0.0 { dx } else { dy };
            view.hadj.set_value(view.hadj.value() + d * 40.0 * view.spp.get());
        } else {
            return glib::Propagation::Proceed;
        }
        t.queue_draw();
        r.queue_draw();
        glib::Propagation::Stop
    });
    w.add_controller(sc);
}

fn rebuild_headers(ctx: &Ctx, headers: &gtk::Box, timeline: &Canvas) {
    while let Some(c) = headers.first_child() {
        headers.remove(&c);
    }
    let tracks: Vec<Track> = ctx.s().project.tracks.clone();
    for t in &tracks {
        headers.append(&header(ctx, t));
    }
    timeline.set_size_request(-1, (tracks.len() as f32 * TRACK_H).max(TRACK_H * 4.0) as i32);
    update_header_selection(ctx, headers);
}

fn update_header_selection(ctx: &Ctx, headers: &gtk::Box) {
    crate::gestures::sync_selection_classes(ctx, headers, "header:");
}

fn header(ctx: &Ctx, t: &Track) -> gtk::Widget {
    use crate::gestures::{Paint, paint_button};
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_widget_name(&format!("header:{}", t.id));
    row.add_css_class("track-header");
    row.set_size_request(-1, TRACK_H as i32);

    let color = track_color(t.color);
    let strip = Canvas::new(move |_, s, w, h| rect(s, 0.0, 0.0, w, h, &color));
    strip.set_size_request(5, -1);
    row.append(&strip);

    let icon = gtk::Image::from_icon_name(match t.kind {
        TrackKind::Audio => "audio-input-microphone-symbolic",
        TrackKind::Instrument => "audio-x-generic-symbolic",
    });
    icon.add_css_class("dim-label");
    row.append(&icon);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
    col.set_valign(gtk::Align::Center);
    col.set_hexpand(true);
    let name = gtk::Label::new(Some(&t.name));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("track-name");
    name.set_tooltip_text(Some("Click to select, Shift-click to add, drag to select a range, double-click to rename"));
    crate::gestures::select_name(ctx, &name, t.id);
    col.append(&name);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let id = Some(t.id);
    buttons.append(&paint_button(ctx, Paint::Mute, id));
    buttons.append(&paint_button(ctx, Paint::Solo, id));
    buttons.append(&paint_button(ctx, Paint::Arm, id));
    if t.kind == TrackKind::Audio {
        buttons.append(&paint_button(ctx, Paint::Monitor, id));
    }
    col.append(&buttons);
    row.append(&col);

    // Clicking empty header space selects the track; Shift-click adds it to the selection.
    let click = gtk::GestureClick::new();
    {
        let (ctx, tid) = (ctx.clone(), t.id);
        click.connect_pressed(move |g, _, _, _| {
            if g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) {
                ctx.toggle_selected(tid);
            } else {
                ctx.select_track(Some(tid));
            }
        });
    }
    icon.add_controller(click);
    row.upcast()
}

/// Region bounds after applying an in-progress drag.
fn dragged(r: &Region, ti: usize, drag: Drag, off: (f64, f64), view: &View, time: &TimeMap, tracks: usize) -> (u64, u64, usize) {
    let grid = grid_ticks(view.snap.get(), time);
    let dx = off.0 * view.spp.get();
    let shift = |pos: u64| time.snap((pos as f64 + dx).max(0.0) as u64, grid);
    match drag {
        Drag::Move { region, start, track } if region == r.id => {
            let rows = (off.1 / TRACK_H as f64).round() as i64;
            let ti = (track as i64 + rows).clamp(0, tracks as i64 - 1) as usize;
            (shift(start), r.length, ti)
        }
        Drag::TrimStart { region } if region == r.id => {
            let s = shift(r.start).min(r.end().saturating_sub(64));
            (s, r.end() - s, ti)
        }
        Drag::TrimEnd { region } if region == r.id => {
            let e = shift(r.end()).max(r.start + 64);
            (r.start, e - r.start, ti)
        }
        _ => (r.start, r.length, ti),
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_lanes(ctx: &Ctx, view: &View, drag: Drag, off: (f64, f64), c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let mut st = ctx.m();
    let st = &mut *st;
    let files: Vec<String> = st
        .project
        .tracks
        .iter()
        .flat_map(|t| t.regions.iter())
        .filter_map(|r| match &r.content {
            Content::Audio(a) => Some(a.file.clone()),
            _ => None,
        })
        .collect();
    let peaks: std::collections::HashMap<String, Rc<crate::peaks::Peaks>> =
        files.into_iter().filter_map(|f| st.peaks_for(&f).map(|p| (f, p))).collect();
    let p = &st.project;
    let time = p.time();
    let fg = canvas::fg(c);
    let accent = canvas::accent(c);

    for (i, t) in p.tracks.iter().enumerate() {
        let y = i as f32 * TRACK_H;
        if st.selected_track == Some(t.id) {
            rect(s, 0.0, y, w, TRACK_H, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.08));
        }
        rect(s, 0.0, y + TRACK_H - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08));
    }
    draw_grid(view, &time, s, w, h, &fg);
    if p.cycle_on {
        let (a, b) = (view.x(p.cycle.start), view.x(p.cycle.end));
        rect(s, a, 0.0, b - a, h, &rgba(0xf6d32d, 0.06));
    }

    let ntracks = p.tracks.len();
    let mut draws = Vec::new();
    for (ti, t) in p.tracks.iter().enumerate() {
        for r in &t.regions {
            let (start, length, row) = dragged(r, ti, drag, off, view, &time, ntracks);
            let x = view.x(start);
            let rw = (length as f64 / view.spp.get()) as f32;
            if x + rw < 0.0 || x > w {
                continue;
            }
            draws.push((r.clone(), t.color, row, x, rw, start));
        }
    }
    for (r, color, row, x, rw, _) in draws {
        let y = row as f32 * TRACK_H + 2.0;
        let rh = TRACK_H - 4.0;
        let selected = st.selected_region == Some(r.id);
        let base = track_color(color);
        let alpha = if r.muted { 0.25 } else if selected { 0.95 } else { 0.7 };
        rounded(s, x, y, rw, rh, 4.0, &gdk::RGBA::new(base.red(), base.green(), base.blue(), alpha));
        rounded(s, x, y, rw, 15.0, 4.0, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25));
        if selected {
            rect(s, x, y + rh - 2.0, rw, 2.0, &rgba(0xffffff, 0.9));
        }
        let ink = rgba(0x000000, 0.75);
        s.push_clip(&gtk::graphene::Rect::new(x, y, rw.max(0.0), rh));
        text(s, c, x + 4.0, y, &r.name, &rgba(0xffffff, 0.95), Some(8.0));
        let body_y = y + 15.0;
        let body_h = rh - 17.0;
        match &r.content {
            Content::Audio(a) => {
                if let Some(peaks) = peaks.get(&a.file) {
                    let x0 = x.max(0.0);
                    let x1 = (x + rw).min(w);
                    let cols = (x1 - x0).max(0.0) as usize;
                    let frame = a.offset as f64 + (x0 - x) as f64 * view.spp.get();
                    let gain = db_to_gain(a.gain_db);
                    let data: Vec<(f32, f32)> =
                        peaks.columns(frame, view.spp.get(), cols).into_iter().map(|(lo, hi)| (lo * gain, hi * gain)).collect();
                    canvas::waveform(s, x0, body_y, body_h, &data, &ink);
                }
            }
            Content::Midi(m) => {
                if !m.notes.is_empty() {
                    let lo = m.notes.iter().map(|n| n.pitch).min().unwrap() as f32;
                    let hi = m.notes.iter().map(|n| n.pitch).max().unwrap() as f32;
                    let span = (hi - lo).max(12.0);
                    for n in &m.notes {
                        let nx = x + (time.ticks_to_samples(n.start as u64) as f64 / view.spp.get()) as f32;
                        let nw = ((time.ticks_to_samples(n.length as u64) as f64 / view.spp.get()) as f32).max(1.0);
                        let ny = body_y + body_h - 3.0 - (n.pitch as f32 - lo) / span * (body_h - 4.0);
                        rect(s, nx, ny, nw, 2.0, &ink);
                    }
                }
            }
        }
        s.pop();
    }

    if let Some(io) = &st.io {
        let live = io.recorder.live.lock().unwrap().clone();
        let pos = io.handle.shared.position.load(std::sync::atomic::Ordering::Relaxed);
        for (track, take) in live {
            let Some(row) = p.tracks.iter().position(|t| t.id == track) else { continue };
            let x = view.x(take.start);
            let end = (take.start + take.frames).max(pos.min(take.start + take.frames + 48000));
            let rw = view.x(end) - x;
            let y = row as f32 * TRACK_H + 2.0;
            rounded(s, x, y, rw, TRACK_H - 4.0, 4.0, &rgba(0xe01b24, 0.6));
            let per_px = view.spp.get() / solstice_engine::recorder::LIVE_PEAK_BLOCK as f64;
            let cols: Vec<(f32, f32)> = (0..rw.max(0.0) as usize)
                .map(|c| {
                    let a = (c as f64 * per_px) as usize;
                    let b = (((c + 1) as f64 * per_px) as usize).max(a + 1);
                    take.peaks.get(a..b.min(take.peaks.len())).unwrap_or(&[]).iter().fold((0f32, 0f32), |m, p| (m.0.min(p.0), m.1.max(p.1)))
                })
                .collect();
            canvas::waveform(s, x, y + 15.0, TRACK_H - 21.0, &cols, &rgba(0x000000, 0.7));
        }
    }
}

fn draw_grid(view: &View, time: &TimeMap, s: &gtk::Snapshot, w: f32, h: f32, fg: &gdk::RGBA) {
    let bar = time.bar_len() as f64;
    let beat = time.beat_len() as f64;
    let bar_px = bar / view.spp.get();
    let step = if bar_px < 12.0 { (12.0 / bar_px).ceil() as u64 } else { 1 };
    let first = (view.x0.get() / bar) as u64;
    let mut b = first - first % step;
    loop {
        let x = view.x((b as f64 * bar) as u64);
        if x > w {
            break;
        }
        rect(s, x, 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
        if bar_px > 60.0 {
            for k in 1..time.sig_num as u64 {
                let bx = view.x((b as f64 * bar + k as f64 * beat) as u64);
                rect(s, bx, 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.05));
            }
        }
        b += step;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_ruler(ctx: &Ctx, view: &View, drag: Drag, off: (f64, f64), c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let st = ctx.s();
    let p = &st.project;
    let time = p.time();
    let fg = canvas::fg(c);
    let mut cycle = p.cycle;
    if let Drag::Cycle { from } = drag {
        let to = time.snap(view.pos(off.0), time.ticks_per_bar());
        cycle = Range::new(from, to);
    }
    let (a, b) = (view.x(cycle.start), view.x(cycle.end));
    let col = if p.cycle_on || matches!(drag, Drag::Cycle { .. }) { rgba(0xf6d32d, 0.85) } else { rgba(0x888888, 0.35) };
    rounded(s, a, 2.0, b - a, 10.0, 3.0, &col);
    if p.punch_on {
        let (a, b) = (view.x(p.punch.start), view.x(p.punch.end));
        rounded(s, a, h - 8.0, b - a, 6.0, 2.0, &rgba(0xe01b24, 0.7));
    }

    let bar = time.bar_len() as f64;
    let bar_px = bar / view.spp.get();
    let step = (48.0 / bar_px).ceil().max(1.0) as u64;
    let first = (view.x0.get() / bar) as u64;
    let mut n = first - first % step;
    loop {
        let x = view.x((n as f64 * bar) as u64);
        if x > w {
            break;
        }
        rect(s, x, h - 12.0, 1.0, 12.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.5));
        text(s, c, x + 3.0, h - 17.0, &(n + 1).to_string(), &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.8), Some(8.0));
        n += step;
    }
    rect(s, 0.0, h - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15));
    let x = view.x(ctx.position());
    rect(s, x - 4.0, h - 6.0, 9.0, 6.0, &fg);
}

fn hit(ctx: &Ctx, view: &View, x: f64, y: f64) -> (Option<usize>, Option<(Id, Drag)>) {
    let s = ctx.s();
    let row = (y / TRACK_H as f64) as usize;
    let Some(t) = s.project.tracks.get(row) else { return (None, None) };
    let pos = view.pos(x);
    let edge = (EDGE * view.spp.get()) as u64;
    for r in t.regions.iter().rev() {
        if pos + edge >= r.start && pos < r.end() + edge {
            let d = if pos < r.start + edge && r.length > edge * 3 {
                Drag::TrimStart { region: r.id }
            } else if pos + edge >= r.end() && r.length > edge * 3 {
                Drag::TrimEnd { region: r.id }
            } else {
                Drag::Move { region: r.id, start: r.start, track: row }
            };
            return (Some(row), Some((r.id, d)));
        }
    }
    (Some(row), None)
}

fn install_lane_input(ctx: &Ctx, view: &Rc<View>, canvas: &Canvas, drag: Rc<Cell<Drag>>, offset: Rc<Cell<(f64, f64)>>, open_editor: Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    g.set_button(gdk::BUTTON_PRIMARY);
    {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        g.connect_drag_begin(move |g, x, y| {
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
            offset.set((0.0, 0.0));
            let (row, region) = hit(&ctx, &view, x, y);
            let track = row.and_then(|r| ctx.s().project.tracks.get(r).map(|t| t.id));
            match region {
                Some((id, d)) => {
                    ctx.select_region(Some(id));
                    drag.set(d);
                }
                None => {
                    ctx.select_region(None);
                    ctx.select_track(track);
                    drag.set(Drag::None);
                }
            }
        });
    }
    {
        let (canvas, drag, offset) = (canvas.clone(), drag.clone(), offset.clone());
        g.connect_drag_update(move |_, dx, dy| {
            if drag.get() != Drag::None {
                offset.set((dx, dy));
                canvas.queue_draw();
            }
        });
    }
    {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        g.connect_drag_end(move |_, dx, dy| {
            let d = drag.replace(Drag::None);
            offset.set((0.0, 0.0));
            if dx.abs() < 2.0 && dy.abs() < 2.0 {
                ctx.emit(Change::Selection);
                return;
            }
            let id = match d {
                Drag::Move { region, .. } | Drag::TrimStart { region } | Drag::TrimEnd { region } => region,
                _ => return,
            };
            let (label, result) = {
                let s = ctx.s();
                let time = s.project.time();
                let Some((ti, r)) = s.project.find_region(id) else { return };
                let res = dragged(r, ti, d, (dx, dy), &view, &time, s.project.tracks.len());
                let same_kind = s.project.tracks[res.2].kind == s.project.tracks[ti].kind;
                let res = if same_kind { res } else { (res.0, res.1, ti) };
                (if matches!(d, Drag::Move { .. }) { "Move Region" } else { "Resize Region" }, (res, r.start))
            };
            let ((start, length, to_track), old_start) = result;
            ctx.edit(label, |p| {
                let mut r = p.remove_region(id).unwrap();
                if let (Drag::TrimStart { .. }, Content::Audio(a)) = (d, &mut r.content) {
                    a.offset = (a.offset as i64 + start as i64 - old_start as i64).max(0) as u64;
                }
                if let (Drag::TrimStart { .. }, Content::Midi(m)) = (d, &mut r.content) {
                    let shift = p.time().samples_to_ticks(start.abs_diff(old_start)) as i64 * if start > old_start { 1 } else { -1 };
                    m.notes.retain(|n| n.start as i64 - shift >= 0);
                    for n in &mut m.notes {
                        n.start = (n.start as i64 - shift) as u32;
                    }
                }
                r.start = start;
                r.length = length;
                p.tracks[to_track].regions.push(r);
            });
        });
    }
    canvas.add_controller(g);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n != 2 {
                return;
            }
            let (row, region) = hit(&ctx, &view, x, y);
            if region.is_some() {
                open_editor();
                return;
            }
            let Some(row) = row else { return };
            let (kind, tid, time) = {
                let s = ctx.s();
                let t = &s.project.tracks[row];
                (t.kind, t.id, s.project.time())
            };
            if kind == TrackKind::Instrument {
                let start = time.snap(view.pos(x).saturating_sub(time.bar_len() / 2), time.ticks_per_bar());
                let mut new_id = 0;
                ctx.edit("Create MIDI Region", |p| {
                    new_id = p.alloc_id();
                    let t = p.track_mut(tid).unwrap();
                    let name = t.name.clone();
                    t.regions.push(Region {
                        id: new_id,
                        name,
                        start,
                        length: time.bar_len(),
                        muted: false,
                        content: Content::Midi(MidiContent::default()),
                    });
                });
                ctx.select_region(Some(new_id));
                open_editor();
            }
        });
    }
    canvas.add_controller(dbl);

    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, view, canvas) = (ctx.clone(), view.clone(), canvas.clone());
        motion.connect_motion(move |_, x, y| {
            let cursor = match hit(&ctx, &view, x, y).1 {
                Some((_, Drag::TrimStart { .. } | Drag::TrimEnd { .. })) => "col-resize",
                _ => "default",
            };
            canvas.set_cursor_from_name(Some(cursor));
        });
    }
    canvas.add_controller(motion);
}

fn install_ruler_input(ctx: &Ctx, view: &Rc<View>, ruler: &Canvas, timeline: &Canvas, drag: Rc<Cell<Drag>>, offset: Rc<Cell<(f64, f64)>>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        g.connect_drag_begin(move |_, x, y| {
            let time = ctx.s().project.time();
            if y < 14.0 {
                drag.set(Drag::Cycle { from: time.snap(view.pos(x), time.ticks_per_bar()) });
                offset.set((x, 0.0));
            } else {
                let grid = grid_ticks(view.snap.get(), &time);
                ctx.locate(time.snap(view.pos(x), grid));
            }
        });
    }
    {
        let (ruler, timeline, drag, offset) = (ruler.clone(), timeline.clone(), drag.clone(), offset.clone());
        g.connect_drag_update(move |g, dx, _| {
            if matches!(drag.get(), Drag::Cycle { .. }) {
                let (sx, _) = g.start_point().unwrap_or((0.0, 0.0));
                offset.set((sx + dx, 0.0));
                ruler.queue_draw();
                timeline.queue_draw();
            }
        });
    }
    {
        let (ctx, view, drag) = (ctx.clone(), view.clone(), drag.clone());
        g.connect_drag_end(move |g, dx, _| {
            let Drag::Cycle { from } = drag.replace(Drag::None) else { return };
            let time = ctx.s().project.time();
            let (sx, _) = g.start_point().unwrap_or((0.0, 0.0));
            if dx.abs() < 3.0 {
                ctx.tweak(|p| p.cycle_on = !p.cycle_on);
                return;
            }
            let to = time.snap(view.pos(sx + dx), time.ticks_per_bar());
            let r = Range::new(from, to);
            if !r.is_empty() {
                ctx.tweak(|p| {
                    p.cycle = r;
                    p.cycle_on = true;
                });
            }
        });
    }
    ruler.add_controller(g);
}

fn install_drop(ctx: &Ctx, view: &Rc<View>, canvas: &Canvas) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let (ctx, view) = (ctx.clone(), view.clone());
    target.connect_drop(move |_, value, x, y| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<_> = list.files().iter().filter_map(|f| f.path()).collect();
        let row = (y / TRACK_H as f64) as usize;
        let time = ctx.s().project.time();
        let pos = time.snap(view.pos(x), grid_ticks(view.snap.get(), &time));
        import_files(&ctx, &paths, Some(row), pos);
        true
    });
    canvas.add_controller(target);
}

/// Imports audio files onto consecutive tracks from `row`, creating audio tracks as needed.
pub fn import_files(ctx: &Ctx, paths: &[std::path::PathBuf], row: Option<usize>, pos: u64) {
    let paths: Vec<_> = paths.iter().filter(|p| crate::import::is_audio(p)).cloned().collect();
    if paths.is_empty() {
        return;
    }
    let (dir, sr) = {
        let s = ctx.s();
        (s.bundle.audio_dir(), s.project.sample_rate)
    };
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let results = gio::spawn_blocking(move || {
            paths.iter().map(|p| crate::import::import(p, &dir, sr).map_err(|e| format!("{}: {e}", p.display()))).collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let ok: Vec<(String, u64)> = results.iter().filter_map(|r| r.as_ref().ok().cloned()).collect();
        for e in results.iter().filter_map(|r| r.as_ref().err()) {
            eprintln!("solstice: import failed: {e}");
        }
        if ok.is_empty() {
            return;
        }
        ctx.edit("Import Audio", |p| {
            let mut row = row.unwrap_or(p.tracks.len());
            for (file, frames) in ok {
                while row < p.tracks.len() && p.tracks[row].kind != TrackKind::Audio {
                    row += 1;
                }
                if row >= p.tracks.len() {
                    p.add_track(TrackKind::Audio);
                    row = p.tracks.len() - 1;
                }
                let id = p.alloc_id();
                let name = file.trim_end_matches(".wav").to_string();
                p.tracks[row].regions.push(Region {
                    id,
                    name,
                    start: pos,
                    length: frames,
                    muted: false,
                    content: Content::Audio(AudioContent { file, offset: 0, gain_db: 0.0, fade_in: 0, fade_out: 0 }),
                });
                row += 1;
            }
        });
    });
}
