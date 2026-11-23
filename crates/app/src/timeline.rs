use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

pub const TRACK_H: f32 = 64.0;

/// A row in the tracks area: a track, or the header of a Track Stack above its tracks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Row {
    Stack(Id),
    Track(usize),
}

/// The rows shown, top to bottom. Tracks in a collapsed stack have none.
pub fn rows(p: &Project) -> Vec<Row> {
    let mut out = Vec::new();
    let mut headed = Vec::new();
    for (i, t) in p.tracks.iter().enumerate() {
        if let Some(stack) = t.stack.and_then(|id| p.stack(id)) {
            if !headed.contains(&stack.id) {
                headed.push(stack.id);
                out.push(Row::Stack(stack.id));
            }
            if stack.collapsed {
                continue;
            }
        }
        out.push(Row::Track(i));
    }
    out
}

fn track_row(rows: &[Row], ti: usize) -> Option<usize> {
    rows.iter().position(|r| *r == Row::Track(ti))
}

fn track_y(rows: &[Row], ti: usize) -> Option<f32> {
    track_row(rows, ti).map(|r| r as f32 * TRACK_H)
}

fn row_at(rows: &[Row], y: f64) -> Option<Row> {
    if y < 0.0 { None } else { rows.get((y / TRACK_H as f64) as usize).copied() }
}

fn track_at(rows: &[Row], y: f64) -> Option<usize> {
    match row_at(rows, y) {
        Some(Row::Track(i)) => Some(i),
        _ => None,
    }
}
pub const HEADER_W: i32 = 300;
const RULER_H: f32 = 30.0;
/// The bar number row along the top of the ruler, which is also where the loop is set.
const CYCLE_H: f64 = 16.0;
const EDGE: f64 = 6.0;

/// Zoom, scroll and snap shared by the tracks area and the editors.
pub struct View {
    /// Samples per pixel.
    pub spp: Cell<f64>,
    /// Leftmost visible sample.
    pub x0: Cell<f64>,
    pub snap: Cell<u64>,
    pub hadj: gtk::Adjustment,
    pub tool: Cell<Tool>,
    /// Automation view (A): lanes drawn over the tracks, edited instead of regions.
    pub show_auto: Cell<bool>,
    /// The tracks area, whose left edge is where `x` counts from.
    lanes: glib::WeakRef<Canvas>,
}

/// The tool set for the tracks area, picked from the toolbar or with T.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pointer,
    Pencil,
    Scissors,
    Glue,
    Mute,
    Eraser,
}

impl Tool {
    pub const ALL: [Tool; 6] = [Tool::Pointer, Tool::Pencil, Tool::Scissors, Tool::Glue, Tool::Mute, Tool::Eraser];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pointer => "Pointer",
            Tool::Pencil => "Pencil",
            Tool::Scissors => "Scissors",
            Tool::Glue => "Glue",
            Tool::Mute => "Mute",
            Tool::Eraser => "Eraser",
        }
    }

    /// The letter that picks the tool while the tool menu is open.
    pub fn key(self) -> char {
        match self {
            Tool::Pointer => 't',
            Tool::Pencil => 'p',
            Tool::Scissors => 'i',
            Tool::Glue => 'g',
            Tool::Mute => 'm',
            Tool::Eraser => 'e',
        }
    }

    fn cursor(self) -> &'static str {
        match self {
            Tool::Pointer => "default",
            Tool::Pencil => "crosshair",
            Tool::Scissors => "cell",
            Tool::Glue => "copy",
            Tool::Mute => "pointer",
            Tool::Eraser => "not-allowed",
        }
    }
}

impl View {
    pub fn x(&self, pos: u64) -> f32 {
        ((pos as f64 - self.x0.get()) / self.spp.get()) as f32
    }
    pub fn pos(&self, x: f64) -> u64 {
        (self.x0.get() + x * self.spp.get()).max(0.0) as u64
    }
    /// Where the tracks area's left edge falls in `w`, which a strip drawn above or below the
    /// tracks adds to `x` (and takes from pointer positions) to stay in line with them, however
    /// wide the columns beside each turn out.
    pub fn shift(&self, w: &impl IsA<gtk::Widget>) -> f32 {
        self.lanes
            .upgrade()
            .and_then(|l| l.compute_point(w, &gtk::graphene::Point::new(0.0, 0.0)))
            .map_or(0.0, |p| p.x())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    /// Moves every selected region by the grabbed one's offset; `copy` leaves the originals.
    Move { region: Id, start: u64, copy: bool },
    TrimStart { region: Id },
    TrimEnd { region: Id },
    Loop { region: Id },
    FadeIn { region: Id },
    FadeOut { region: Id },
    Marquee { x: f64, y: f64, add: bool },
    /// A cycle range from `from` to the pointer; also how an edge of the cycle is dragged, which
    /// keeps its mode (`skip: None`). A new range sets whether it repeats or is skipped.
    Cycle { from: u64, skip: Option<bool> },
    /// The whole loop, grabbed at `grab`.
    CycleMove { grab: u64 },
    /// A punch range from `from` to the pointer, which is also how an edge of it is dragged. Released
    /// without moving, it locates the playhead instead, as the ruler does with punch off.
    Punch { from: u64 },
    /// The whole punch range, grabbed at `grab`.
    PunchMove { grab: u64 },
    /// An automation point on track row `row`, being dragged.
    AutoPoint { row: usize, index: usize },
}

/// A region's geometry after an in-progress drag.
#[derive(Clone, Copy)]
struct Shape {
    start: u64,
    length: u64,
    row: usize,
    loops: u32,
    fade_in: u64,
    fade_out: u64,
}

pub struct Tracks {
    pub widget: gtk::Widget,
    /// The Tools menu button; T pops it open.
    pub tools: gtk::MenuButton,
    pub view: Rc<View>,
    /// Global tracks (signature, tempo), shown with G.
    pub global: gtk::Revealer,
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
    let snap_index = (ctx.s().settings.default_snap as usize).min(snap_options().len() - 1);
    let view = Rc::new(View {
        spp: Cell::new(sr / 40.0),
        x0: Cell::new(0.0),
        snap: Cell::new(snap_options()[snap_index].1),
        hadj,
        tool: Cell::new(Tool::Pointer),
        show_auto: Cell::new(false),
        lanes: glib::WeakRef::new(),
    });
    let drag = Rc::new(Cell::new(Drag::None));
    let offset = Rc::new(Cell::new((0.0f64, 0.0f64)));

    let timeline = {
        let (ctx, view, drag, offset) = (ctx.clone(), view.clone(), drag.clone(), offset.clone());
        Canvas::new(move |c, s, w, h| draw_lanes(&ctx, &view, drag.get(), offset.get(), c, s, w, h))
    };
    view.lanes.set(Some(&timeline));
    timeline.set_hexpand(true);
    timeline.set_focusable(true);

    let loop_hover = Rc::new(Cell::new(false));
    let ruler = {
        let (ctx, view, drag, offset, hover) = (ctx.clone(), view.clone(), drag.clone(), offset.clone(), loop_hover.clone());
        Canvas::new(move |c, s, w, h| draw_ruler(&ctx, &view, drag.get(), offset.get(), hover.get(), c, s, w, h))
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

    // Fixed widths rather than a SizeGroup: a horizontal group also hands each member's height
    // to the others when measuring, which asks the tall header column to fit in the toolbar.
    for w in [corner.upcast_ref::<gtk::Widget>(), headers.upcast_ref(), hbar_pad.upcast_ref()] {
        w.set_size_request(HEADER_W, -1);
    }

    let (global_row, global_lanes) = crate::global::build(ctx, &view);
    let global_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    global_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    global_box.append(&global_row);
    let global = gtk::Revealer::builder().child(&global_box).reveal_child(false).build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&ruler_row);
    root.append(&global);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&overlay);
    root.append(&hbar_row);

    {
        let (view, t, r, g) = (view.clone(), timeline.clone(), ruler.clone(), global_lanes.clone());
        view.hadj.clone().connect_value_changed(move |a| {
            view.x0.set(a.value());
            t.queue_draw();
            r.queue_draw();
            g.queue_draw();
        });
    }

    let redraw = {
        let (t, r) = (timeline.clone(), ruler.clone());
        move || {
            t.queue_draw();
            r.queue_draw();
        }
    };

    rebuild_headers(ctx, &headers, &timeline, false);
    {
        let (ctx2, headers, timeline, view) = (ctx.clone(), headers.clone(), timeline.clone(), view.clone());
        let redraw = redraw.clone();
        ctx.on(move |c| {
            match c {
                Change::Project | Change::View => {
                    rebuild_headers(&ctx2, &headers, &timeline, view.show_auto.get());
                    update_scroll(&ctx2, &view, timeline.width());
                }
                Change::Selection => update_header_selection(&ctx2, &headers),
                _ => {}
            }
            redraw();
        });
    }

    install_lane_input(ctx, &view, &timeline, drag.clone(), offset.clone(), open_editor);
    install_ruler_input(ctx, &view, &ruler, &timeline, drag, offset, loop_hover);
    install_zoom(&view, &timeline, &timeline, &ruler);
    install_zoom(&view, &ruler, &timeline, &ruler);
    install_drop(ctx, &view, &timeline);
    install_tilt_scroll(&view, &root);

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

    let tools = tools_button(&view, &timeline);
    corner.append(&tools);
    // The toolbar can need more than HEADER_W with larger fonts; the columns below it follow, or
    // the ruler would sit to the right of the lanes it measures.
    {
        let cols: Vec<gtk::Widget> =
            [headers.upcast_ref::<gtk::Widget>().clone(), hbar_pad.upcast_ref::<gtk::Widget>().clone()].into_iter().chain(global_row.first_child()).collect();
        corner.connect_map(move |c| {
            let w = c.measure(gtk::Orientation::Horizontal, -1).0.max(HEADER_W);
            for col in &cols {
                col.set_size_request(w, -1);
            }
        });
    }
    Tracks { widget: root.upcast(), tools, view, global }
}

fn update_scroll(ctx: &Ctx, view: &View, width: i32) {
    let s = ctx.s();
    let len = s.project.length().max(s.project.time().bar_len() * 32);
    let page = width.max(1) as f64 * view.spp.get();
    view.hadj.configure(view.x0.get(), 0.0, len as f64 + page, page * 0.1, page * 0.9, page);
}

/// The tool menu: click a tool, or press its letter while the menu is open.
fn tools_button(view: &Rc<View>, timeline: &Canvas) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().label(Tool::Pointer.label()).tooltip_text("Tool (T)").build();
    b.add_css_class("flat");
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let choose: Rc<dyn Fn(Tool)> = {
        let (view, b, pop, timeline) = (view.clone(), b.clone(), pop.clone(), timeline.clone());
        Rc::new(move |t: Tool| {
            view.tool.set(t);
            b.set_label(t.label());
            pop.popdown();
            timeline.set_cursor_from_name(Some(t.cursor()));
        })
    };
    for t in Tool::ALL {
        let row = gtk::Button::new();
        let bx = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let name = gtk::Label::new(Some(t.label()));
        name.set_hexpand(true);
        name.set_xalign(0.0);
        let key = gtk::Label::new(Some(&t.key().to_uppercase().to_string()));
        key.add_css_class("dim-label");
        bx.append(&name);
        bx.append(&key);
        row.set_child(Some(&bx));
        row.add_css_class("flat");
        let choose = choose.clone();
        row.connect_clicked(move |_| choose(t));
        list.append(&row);
    }
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(c) = key.to_unicode() else { return glib::Propagation::Proceed };
        match Tool::ALL.into_iter().find(|t| t.key() == c.to_ascii_lowercase()) {
            Some(t) => {
                choose(t);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    });
    pop.add_controller(keys);
    pop.set_child(Some(&list));
    b.set_popover(Some(&pop));
    b
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
    menu.append(Some("_VCA Fader"), Some("win.add-vca"));
    add.set_menu_model(Some(&menu));
    bar.append(&add);

    let names: Vec<&str> = snap_options().iter().map(|o| o.0).collect();
    let snap = gtk::DropDown::from_strings(&names);
    snap.set_selected(snap_options().iter().position(|o| o.1 == view.snap.get()).unwrap_or(1) as u32);
    snap.set_tooltip_text(Some("Snap"));
    {
        let view = view.clone();
        snap.connect_selected_notify(move |d| view.snap.set(snap_options()[d.selected() as usize].1));
    }
    bar.append(&snap);

    let global = gtk::ToggleButton::builder().icon_name("view-list-symbolic").tooltip_text("Global Tracks (G)").build();
    global.add_css_class("flat");
    global.set_action_name(Some("win.show-global"));
    bar.append(&global);

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

/// Sideways scrolling (wheel tilt buttons, touchpad) anywhere in the tracks area, including
/// over the headers, which have no horizontal scroller of their own.
fn install_tilt_scroll(view: &Rc<View>, root: &gtk::Box) {
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    let view = view.clone();
    sc.connect_scroll(move |_, dx, _| {
        if dx == 0.0 {
            return glib::Propagation::Proceed;
        }
        view.hadj.set_value(view.hadj.value() + dx * 40.0 * view.spp.get());
        glib::Propagation::Stop
    });
    root.add_controller(sc);
}

fn rebuild_headers(ctx: &Ctx, headers: &gtk::Box, timeline: &Canvas, show_auto: bool) {
    while let Some(c) = headers.first_child() {
        headers.remove(&c);
    }
    let (tracks, layout) = {
        let p = &ctx.s().project;
        (p.tracks.clone(), rows(p))
    };
    for row in &layout {
        match *row {
            Row::Track(i) => headers.append(&header(ctx, &tracks[i], show_auto)),
            Row::Stack(id) => headers.append(&stack_header(ctx, id)),
        }
    }
    timeline.set_size_request(-1, (layout.len() as f32 * TRACK_H).max(TRACK_H * 4.0) as i32);
    update_header_selection(ctx, headers);
}

/// A Track Stack's header: disclosure triangle, name, and for a summing stack its aux's
/// mute, solo and volume.
fn stack_header(ctx: &Ctx, id: Id) -> gtk::Widget {
    let Some(stack) = ctx.s().project.stack(id).cloned() else { return gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast() };
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("track-header");
    row.add_css_class("stack-header");
    row.set_size_request(-1, TRACK_H as i32);
    let fold = gtk::Button::from_icon_name(if stack.collapsed { "pan-end-symbolic" } else { "pan-down-symbolic" });
    fold.add_css_class("flat");
    fold.set_valign(gtk::Align::Center);
    fold.set_tooltip_text(Some(if stack.collapsed { "Show the stack's tracks" } else { "Hide the stack's tracks" }));
    {
        let ctx = ctx.clone();
        fold.connect_clicked(move |_| {
            ctx.tweak(|p| {
                if let Some(s) = p.stacks.iter_mut().find(|s| s.id == id) {
                    s.collapsed = !s.collapsed;
                }
            })
        });
    }
    row.append(&fold);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
    col.set_valign(gtk::Align::Center);
    col.set_hexpand(true);
    let name = gtk::Label::new(Some(&stack.name));
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("track-name");
    let count = ctx.s().project.tracks.iter().filter(|t| t.stack == Some(id)).count();
    let kind = if stack.bus.is_some() { "Summing stack" } else { "Folder stack" };
    name.set_tooltip_text(Some(&format!("{kind} of {count} tracks. Right-click for options")));
    col.append(&name);
    if let Some(bus) = stack.bus {
        use crate::gestures::{Paint, paint_button};
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        buttons.append(&paint_button(ctx, Paint::Mute, Some(bus)));
        buttons.append(&paint_button(ctx, Paint::Solo, Some(bus)));
        col.append(&buttons);
        row.append(&col);
        let mix = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        mix.set_valign(gtk::Align::Center);
        mix.append(&crate::gestures::mini_fader(ctx, Some(bus)));
        mix.append(&crate::gestures::mini_pan(ctx, Some(bus)));
        row.append(&mix);
    } else {
        let info = gtk::Label::new(Some(&format!("{count} tracks")));
        info.add_css_class("dim-label");
        info.add_css_class("caption");
        info.set_xalign(0.0);
        col.append(&info);
        row.append(&col);
    }
    let menu = gtk::GestureClick::new();
    menu.set_button(gdk::BUTTON_SECONDARY);
    {
        let (ctx, row2) = (ctx.clone(), row.clone());
        menu.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            crate::context_menu::stack_menu(&ctx, row2.upcast_ref(), x, y, id);
        });
    }
    row.add_controller(menu);
    row.upcast()
}

fn update_header_selection(ctx: &Ctx, headers: &gtk::Box) {
    crate::gestures::sync_selection_classes(ctx, headers, "header:");
}

fn header(ctx: &Ctx, t: &Track, show_auto: bool) -> gtk::Widget {
    use crate::gestures::{Paint, paint_button};
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_widget_name(&format!("header:{}", t.id));
    row.add_css_class("track-header");
    row.set_size_request(-1, TRACK_H as i32);
    if t.stack.is_some() {
        row.set_margin_start(16);
    }

    let color = track_color(t.color);
    let strip = Canvas::new(move |_, s, w, h| rect(s, 0.0, 0.0, w, h, &color));
    strip.set_size_request(5, -1);
    row.append(&strip);

    let icon = gtk::Image::from_icon_name(match (t.kind, t.channel.kind) {
        (TrackKind::Audio, Some(StripKind::GuitarDi)) => "solstice-guitar-symbolic",
        (TrackKind::Audio, Some(StripKind::BassDi)) => "solstice-bass-symbolic",
        (TrackKind::Audio, None) => "audio-input-microphone-symbolic",
        (TrackKind::Instrument, _) => "audio-x-generic-symbolic",
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
    if t.alternatives.is_empty() && !t.reference {
        col.append(&name);
    } else {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        line.append(&name);
        if !t.alternatives.is_empty() {
            let alt = gtk::Label::new(Some(t.alternative_name()));
            alt.add_css_class("dim-label");
            alt.add_css_class("caption");
            alt.set_tooltip_text(Some("Active alternative"));
            line.append(&alt);
        }
        if t.reference {
            line.append(&crate::mixer::reference_badge());
        }
        col.append(&line);
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    let id = Some(t.id);
    buttons.append(&paint_button(ctx, Paint::Mute, id));
    buttons.append(&paint_button(ctx, Paint::Solo, id));
    buttons.append(&paint_button(ctx, Paint::Arm, id));
    if t.kind == TrackKind::Audio {
        buttons.append(&paint_button(ctx, Paint::Monitor, id));
    }
    if crate::render::freezable(t) {
        buttons.append(&freeze_button(ctx, t));
    }
    if t.freeze.is_some() {
        row.add_css_class("frozen");
    }
    col.append(&buttons);
    row.append(&col);
    if show_auto {
        row.append(&crate::automation::header_controls(ctx, t.id));
    } else {
        let mix = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        mix.set_valign(gtk::Align::Center);
        mix.append(&crate::gestures::mini_fader(ctx, id));
        mix.append(&crate::gestures::mini_pan(ctx, id));
        row.append(&mix);
    }

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
    row.add_controller(click);
    let menu = gtk::GestureClick::new();
    menu.set_button(gdk::BUTTON_SECONDARY);
    {
        let (ctx, tid, row2) = (ctx.clone(), t.id, row.clone());
        menu.connect_pressed(move |g, _, x, y| {
            g.set_state(gtk::EventSequenceState::Claimed);
            crate::context_menu::track_menu(&ctx, row2.upcast_ref(), x, y, tid);
        });
    }
    row.add_controller(menu);
    row.upcast()
}

fn freeze_button(ctx: &Ctx, t: &Track) -> gtk::Button {
    let b = gtk::Button::from_icon_name("solstice-freeze-symbolic");
    b.add_css_class("flat");
    b.add_css_class("freeze-button");
    b.set_valign(gtk::Align::Center);
    let frozen = t.freeze.is_some();
    if frozen {
        b.add_css_class("on");
    }
    b.set_tooltip_text(Some(if frozen { "Unfreeze" } else { "Freeze" }));
    let (ctx, id) = (ctx.clone(), t.id);
    b.connect_clicked(move |b| crate::render::toggle_freeze(&ctx, &[id], b));
    b
}

/// Region bounds after applying an in-progress drag.
fn fades(r: &Region) -> (u64, u64) {
    match &r.content {
        Content::Audio(a) => (a.fade_in, a.fade_out),
        Content::Midi(_) => (0, 0),
    }
}

/// Where a region would sit if the current drag ended now.
#[allow(clippy::too_many_arguments)]
fn shape(r: &Region, ti: usize, drag: Drag, off: (f64, f64), view: &View, time: &TimeMap, rows: &[Row], sel: &[Id]) -> Shape {
    let grid = grid_ticks(view.snap.get(), time);
    let dx = off.0 * view.spp.get();
    let snap = |pos: f64| time.snap(pos.max(0.0) as u64, grid);
    let (fade_in, fade_out) = fades(r);
    let mut out = Shape { start: r.start, length: r.length, row: ti, loops: r.loops, fade_in, fade_out };
    match drag {
        Drag::Move { region, start, .. } if region == r.id || (sel.contains(&r.id) && sel.contains(&region)) => {
            let delta = snap(start as f64 + dx) as i64 - start as i64;
            let by = (off.1 / TRACK_H as f64).round() as i64;
            out.start = (r.start as i64 + delta).max(0) as u64;
            // The nearest track row to where the pointer is, skipping stack headers.
            if let Some(from) = track_row(rows, ti) {
                let want = (from as i64 + by).clamp(0, rows.len() as i64 - 1) as usize;
                let near = (0..rows.len()).filter_map(|k| match rows[k] {
                    Row::Track(t) => Some((k.abs_diff(want), t)),
                    Row::Stack(_) => None,
                });
                if let Some((_, t)) = near.min_by_key(|(d, _)| *d) {
                    out.row = t;
                }
            }
        }
        Drag::TrimStart { region } if region == r.id => {
            // Audio can't be revealed from before its file begins.
            let floor = match &r.content {
                Content::Audio(a) => r.start.saturating_sub(a.offset),
                _ => 0,
            };
            let s = snap(r.start as f64 + dx).min(r.end().saturating_sub(64)).max(floor);
            out.start = s;
            out.length = r.end() - s;
        }
        Drag::TrimEnd { region } if region == r.id => {
            let e = snap(r.end() as f64 + dx).max(r.start + 64);
            out.length = e - r.start;
        }
        Drag::Loop { region } if region == r.id => {
            let end = r.span_end() as f64 + dx;
            out.loops = (((end - r.start as f64) / r.length as f64).round() as i64 - 1).clamp(0, 999) as u32;
        }
        Drag::FadeIn { region } if region == r.id => {
            out.fade_in = (fade_in as f64 + dx).clamp(0.0, r.length as f64) as u64;
        }
        Drag::FadeOut { region } if region == r.id => {
            out.fade_out = (fade_out as f64 - dx).clamp(0.0, r.length as f64) as u64;
        }
        _ => {}
    }
    out
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
        .flat_map(|r| match &r.content {
            Content::Audio(a) => std::iter::once(a.file.clone()).chain(a.takes.iter().map(|t| t.file.clone())).collect(),
            _ => Vec::new(),
        })
        .collect();
    let peaks: std::collections::HashMap<String, Rc<crate::peaks::Peaks>> =
        files.into_iter().filter_map(|f| st.peaks_for(&f).map(|p| (f, p))).collect();
    let p = &st.project;
    let time = p.time();
    let fg = canvas::fg(c);
    let accent = canvas::accent(c);

    let layout = rows(p);
    for (k, row) in layout.iter().enumerate() {
        let y = k as f32 * TRACK_H;
        match *row {
            Row::Track(i) => {
                if st.selected_track == Some(p.tracks[i].id) {
                    rect(s, 0.0, y, w, TRACK_H, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.08));
                }
            }
            // A stack's row sums up its tracks' regions.
            Row::Stack(id) => {
                rect(s, 0.0, y, w, TRACK_H, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.05));
                let members: Vec<&Track> = p.tracks.iter().filter(|t| t.stack == Some(id)).collect();
                let lane = (TRACK_H - 8.0) / members.len().max(1) as f32;
                for (m, t) in members.iter().enumerate() {
                    for r in &t.regions {
                        let col = track_color(r.color.unwrap_or(t.color));
                        let (x0, x1) = (view.x(r.start), view.x(r.span_end()));
                        rect(s, x0, y + 4.0 + m as f32 * lane, (x1 - x0).max(1.0), (lane - 1.0).max(1.0), &gdk::RGBA::new(col.red(), col.green(), col.blue(), 0.6));
                    }
                }
            }
        }
        rect(s, 0.0, y + TRACK_H - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08));
    }
    draw_grid(view, &time, s, w, h, &fg);
    if p.cycle_on || matches!(drag, Drag::Cycle { .. } | Drag::CycleMove { .. }) {
        let cycle = dragged_cycle(&time, view, p.cycle, drag, off.0);
        let (a, b) = (view.x(cycle.start), view.x(cycle.end));
        let col = if cycle_skips(p, drag) { rgba(0x000000, 0.25) } else { rgba(0xf6d32d, 0.06) };
        rect(s, a, 0.0, b - a, h, &col);
    }
    if p.punch_on {
        let punch = dragged_punch(&time, view, p.punch, drag, off.0);
        let (a, b) = (view.x(punch.start), view.x(punch.end));
        rect(s, a, 0.0, b - a, h, &rgba(0xe01b24, 0.06));
    }

    let sel = st.selected_regions.clone();
    let copying = matches!(drag, Drag::Move { copy: true, .. });
    let mut draws = Vec::new();
    for (ti, t) in p.tracks.iter().enumerate() {
        for r in &t.regions {
            let sh = shape(r, ti, drag, off, view, &time, &layout, &sel);
            let moved = sh.start != r.start || sh.row != ti;
            if copying && moved {
                // Alt-drag copies: the original stays and a copy follows the pointer.
                let still = Shape { start: r.start, row: ti, ..sh };
                draws.push((r.clone(), r.color.unwrap_or(t.color), still, false));
            }
            draws.push((r.clone(), r.color.unwrap_or(t.color), sh, copying && moved));
        }
    }
    for (r, color, sh, ghost) in draws {
        let full = (sh.length * (sh.loops as u64 + 1)) as f64 / view.spp.get();
        let x = view.x(sh.start);
        if x + (full as f32) < 0.0 || x > w {
            continue;
        }
        let rw = (sh.length as f64 / view.spp.get()) as f32;
        let Some(top) = track_y(&layout, sh.row) else { continue };
        let y = top + 2.0;
        let rh = TRACK_H - 4.0;
        let selected = sel.contains(&r.id) && !ghost;
        let base = track_color(color);
        let alpha = if ghost { 0.4 } else if r.muted { 0.25 } else if selected { 0.95 } else { 0.7 };
        let fill = gdk::RGBA::new(base.red(), base.green(), base.blue(), alpha);
        for k in 1..=sh.loops {
            // Loop repeats: dimmer, with a notch where each repeat begins.
            let lx = x + rw * k as f32;
            rounded(s, lx, y, rw, rh, 4.0, &gdk::RGBA::new(base.red(), base.green(), base.blue(), alpha * 0.45));
            rect(s, lx, y + 3.0, 1.0, rh - 6.0, &rgba(0x000000, 0.4));
        }
        rounded(s, x, y, rw, rh, 4.0, &fill);
        rounded(s, x, y, rw, 15.0, 4.0, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.25));
        if selected {
            rect(s, x, y + rh - 2.0, rw, 2.0, &rgba(0xffffff, 0.9));
        }
        if r.muted {
            text(s, c, x + rw - 12.0, y, "●", &rgba(0xffffff, 0.7), Some(7.0));
        }
        let ink = rgba(0x000000, 0.75);
        s.push_clip(&gtk::graphene::Rect::new(x, y, rw.max(0.0), rh));
        text(s, c, x + 4.0, y, &r.name, &rgba(0xffffff, 0.95), Some(8.0));
        let body_y = y + 15.0;
        let body_h = rh - 17.0;
        match &r.content {
            Content::Audio(a) => {
                let gain = db_to_gain(a.gain_db);
                for piece in a.pieces(sh.length) {
                    let Some(peaks) = peaks.get(piece.file) else { continue };
                    let px = x + (piece.start as f64 / view.spp.get()) as f32;
                    let pw = ((piece.end - piece.start) as f64 / view.spp.get()) as f32;
                    let x0 = px.max(0.0).max(x);
                    let x1 = (px + pw).min(w).min(x + rw);
                    let cols = (x1 - x0).max(0.0) as usize;
                    let frame = piece.offset as f64 + (x0 - px) as f64 * view.spp.get();
                    let data: Vec<(f32, f32)> =
                        peaks.columns(frame, view.spp.get(), cols).into_iter().map(|(lo, hi)| (lo * gain, hi * gain)).collect();
                    canvas::waveform(s, x0, body_y, body_h, &data, &ink);
                    if piece.start > 0 {
                        rect(s, px, body_y, 1.0, body_h, &rgba(0xffffff, 0.35));
                    }
                }
                if a.is_folder() {
                    let badge = format!("{} takes", a.takes.len());
                    text(s, c, x + rw - 52.0, y, &badge, &rgba(0xffffff, 0.8), Some(7.5));
                }
                // Fades: shade the faded part and draw the ramp.
                let shade = rgba(0x000000, 0.28);
                let (fi, fo) = ((sh.fade_in as f64 / view.spp.get()) as f32, (sh.fade_out as f64 / view.spp.get()) as f32);
                for (fx, fw, rising) in [(x, fi, true), (x + rw - fo, fo, false)] {
                    if fw < 1.0 {
                        continue;
                    }
                    let b = gtk::gsk::PathBuilder::new();
                    if rising {
                        b.move_to(fx, body_y);
                        b.line_to(fx + fw, body_y);
                        b.line_to(fx, body_y + body_h);
                    } else {
                        b.move_to(fx, body_y);
                        b.line_to(fx + fw, body_y);
                        b.line_to(fx + fw, body_y + body_h);
                    }
                    b.close();
                    s.append_fill(&b.to_path(), gtk::gsk::FillRule::Winding, &shade);
                }
            }
            Content::Midi(m) => {
                if !m.notes.is_empty() {
                    let lo = m.notes.iter().map(|n| n.pitch).min().unwrap() as f32;
                    let hi = m.notes.iter().map(|n| n.pitch).max().unwrap() as f32;
                    let span = (hi - lo).max(12.0);
                    for n in &m.notes {
                        let nx = view.x(time.at(sh.start, n.start as u64));
                        let nw = (view.x(time.at(sh.start, (n.start + n.length) as u64)) - nx).max(1.0);
                        let ny = body_y + body_h - 3.0 - (n.pitch as f32 - lo) / span * (body_h - 4.0);
                        rect(s, nx, ny, nw, 2.0, &ink);
                    }
                }
                if m.is_folder() {
                    let badge = format!("{} takes", m.takes.len());
                    text(s, c, x + rw - 52.0, y, &badge, &rgba(0xffffff, 0.8), Some(7.5));
                }
            }
        }
        s.pop();
    }
    if view.show_auto.get() {
        let dragging = match drag {
            Drag::AutoPoint { row, index } => auto_dragged(st, view, row, index, off),
            _ => None,
        };
        crate::automation::draw(st, c, s, w, TRACK_H, &|pos| view.x(pos), &|ti| track_y(&layout, ti), dragging);
    }
    if let Drag::Marquee { x, y, .. } = drag {
        let (x1, y1) = (x + off.0, y + off.1);
        let (rx, ry) = (x.min(x1) as f32, y.min(y1) as f32);
        let (rw, rh) = ((x - x1).abs() as f32, (y - y1).abs() as f32);
        rect(s, rx, ry, rw, rh, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.15));
        for (bx, by, bw, bh) in [(rx, ry, rw, 1.0), (rx, ry + rh, rw, 1.0), (rx, ry, 1.0, rh), (rx + rw, ry, 1.0, rh)] {
            rect(s, bx, by, bw, bh, &accent);
        }
    }

    if let Some(io) = &st.io {
        let live = io.recorder.live.lock().unwrap().clone();
        let pos = io.handle.shared.position.load(std::sync::atomic::Ordering::Relaxed);
        for (track, take) in live {
            let Some(top) = p.tracks.iter().position(|t| t.id == track).and_then(|ti| track_y(&layout, ti)) else { continue };
            let x = view.x(take.start);
            let end = (take.start + take.frames).max(pos.min(take.start + take.frames + 48000));
            let rw = view.x(end) - x;
            let y = top + 2.0;
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
    let (from, to) = (view.pos(0.0), view.pos(w as f64));
    let bar_px = time.bar_len() as f64 / view.spp.get();
    let step = if bar_px < 12.0 { (12.0 / bar_px).ceil() as u64 } else { 1 };
    for b in time.bars(from, to).iter().filter(|b| b.bar % step == 0) {
        let x = view.x(b.sample);
        rect(s, x, 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
        if bar_px > 60.0 {
            let beat = PPQ as u64 * 4 / b.den as u64;
            for k in 1..b.num as u64 {
                let bx = view.x(time.ticks_to_samples(b.tick + k * beat));
                let a = if b.starts_group(k) { 0.08 } else { 0.05 };
                rect(s, bx, 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_ruler(ctx: &Ctx, view: &View, drag: Drag, off: (f64, f64), hover: bool, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let st = ctx.s();
    let p = &st.project;
    let time = p.time();
    let fg = canvas::fg(c);
    let fga = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    let dragging = matches!(drag, Drag::Cycle { .. } | Drag::CycleMove { .. });
    let top = CYCLE_H as f32;
    // The loop shares the bar number row, as in Logic, so the row that is clicked anyway holds it.
    rect(s, 0.0, 0.0, w, top, &fga(if hover || dragging { 0.12 } else { 0.06 }));
    rect(s, 0.0, top, w, 1.0, &fga(0.15));
    rect(s, 0.0, h - 1.0, w, 1.0, &fga(0.15));
    // Everything placed in time is drawn from the tracks area's left edge, not the ruler's own.
    let sh = view.shift(c);
    s.save();
    s.translate(&gtk::graphene::Point::new(sh, 0.0));
    let cycle = dragged_cycle(&time, view, p.cycle, drag, off.0);
    let lit = p.cycle_on || dragging;
    let skip = cycle_skips(p, drag);
    let (a, b) = (view.x(cycle.start), view.x(cycle.end));
    if !cycle.is_empty() {
        let col = match (lit, skip) {
            (true, false) => rgba(0xf6d32d, 0.9),
            (true, true) => rgba(0xf6d32d, 0.45),
            _ => fga(if hover { 0.3 } else { 0.18 }),
        };
        rounded(s, a, 0.0, b - a, top, 2.0, &col);
        if skip {
            stripes(s, a.max(-sh), b.min(w - sh), top, &rgba(0x000000, if lit { 0.35 } else { 0.2 }));
        }
        if hover || dragging {
            for x in [a + 2.0, b - 4.0] {
                rect(s, x, 4.0, 2.0, top - 8.0, &rgba(0x000000, 0.4));
            }
        }
    }
    if p.punch_on {
        let punch = dragged_punch(&time, view, p.punch, drag, off.0);
        let (a, b) = (view.x(punch.start), view.x(punch.end));
        rounded(s, a, top + 3.0, b - a, h - top - 6.0, 2.0, &rgba(0xe01b24, 0.75));
        for x in [a + 2.0, b - 4.0] {
            rect(s, x, top + 5.0, 2.0, h - top - 10.0, &rgba(0x000000, 0.4));
        }
    }

    let bar_px = time.bar_len() as f64 / view.spp.get();
    let step = (48.0 / bar_px).ceil().max(1.0) as u64;
    for line in time.bars(view.pos(-sh as f64), view.pos((w - sh) as f64)) {
        let x = view.x(line.sample);
        let beats = line.num.max(1) as u64;
        if step == 1 && bar_px / beats as f64 >= 8.0 {
            let beat_ticks = PPQ as u64 * 4 / line.den.max(1) as u64;
            for i in 1..beats {
                rect(s, view.x(time.ticks_to_samples(line.tick + i * beat_ticks)), h - 4.0, 1.0, 4.0, &fga(0.3));
            }
        }
        if line.bar % step != 0 {
            continue;
        }
        rect(s, x, top + 1.0, 1.0, h - top - 1.0, &fga(0.5));
        let on_loop = lit && !cycle.is_empty() && x + 3.0 >= a && x + 3.0 < b;
        let col = if on_loop { rgba(0x1e1e1e, 0.9) } else { fga(0.8) };
        text(s, c, x + 3.0, 2.0, &(line.bar + 1).to_string(), &col, Some(8.0));
    }
    let x = view.x(ctx.position());
    rect(s, x - 4.0, h - 6.0, 9.0, 6.0, &fg);
    s.restore();
}

/// Where an automation point being dragged would land: (row, index, position, value).
fn auto_dragged(st: &crate::studio::Studio, view: &View, row: usize, index: usize, off: (f64, f64)) -> Option<(usize, usize, u64, f32)> {
    let t = st.project.tracks.get(row)?;
    let target = crate::automation::shown(st, t.id);
    let p = *t.channel.lane(target)?.points.get(index)?;
    let pos = (p.pos as f64 + off.0 * view.spp.get()).max(0.0) as u64;
    let n = crate::automation::norm(target, p.value) - (off.1 / (TRACK_H as f64 - 12.0)) as f32;
    Some((row, index, pos, crate::automation::value(target, n)))
}

/// Automation view clicks: on a point, grab it (Alt deletes it); elsewhere add a point there.
fn auto_begin(ctx: &Ctx, view: &View, x: f64, y: f64, alt: bool) -> Drag {
    let (row, top, track, target, hit) = {
        let s = ctx.s();
        let layout = rows(&s.project);
        let Some(row) = track_at(&layout, y) else { return Drag::None };
        let top = track_y(&layout, row).unwrap_or(0.0);
        let t = &s.project.tracks[row];
        let target = crate::automation::shown(&s, t.id);
        (row, top, t.id, target, crate::automation::nearest_point(&s, row, top, x, y, TRACK_H, &|pos| view.x(pos)))
    };
    match hit {
        Some(i) if alt => {
            ctx.edit("Delete Automation Point", |p| {
                let lane = p.channel_mut(Some(track)).unwrap().lane_mut(target);
                if i < lane.points.len() {
                    lane.points.remove(i);
                }
            });
            Drag::None
        }
        Some(i) => Drag::AutoPoint { row, index: i },
        None if alt => Drag::None,
        None => {
            let pos = view.pos(x);
            let n = 1.0 - ((y - top as f64 - 6.0) / (TRACK_H as f64 - 12.0)) as f32;
            let value = crate::automation::value(target, n);
            let mut index = 0;
            ctx.edit("Add Automation Point", |p| {
                let lane = p.channel_mut(Some(track)).unwrap().lane_mut(target);
                lane.write(pos, pos, &[AutoPoint { pos, value }]);
                index = lane.points.iter().position(|p| p.pos == pos).unwrap_or(0);
            });
            Drag::AutoPoint { row, index }
        }
    }
}

fn hit(ctx: &Ctx, view: &View, x: f64, y: f64) -> (Option<usize>, Option<(Id, Drag)>) {
    let s = ctx.s();
    let layout = rows(&s.project);
    let Some(row) = track_at(&layout, y) else { return (None, None) };
    let t = &s.project.tracks[row];
    let top = track_y(&layout, row).unwrap_or(0.0) as f64;
    let pos = view.pos(x);
    let spp = view.spp.get();
    let edge = (EDGE * spp) as u64;
    let fade_zone = (16.0 * spp) as u64;
    let inside_y = y - top - 2.0;
    // The loop handle is only the top-right corner, so the rest of the end edge resizes.
    let corner = inside_y < 10.0;
    for r in t.regions.iter().rev() {
        if pos + edge < r.start || pos >= r.span_end() + edge {
            continue;
        }
        let sizable = r.length > edge * 3;
        let near_end = pos + edge >= r.end() && pos < r.end() + edge;
        let near_span_end = r.loops > 0 && pos + edge >= r.span_end();
        let audio = matches!(r.content, Content::Audio(_));
        let d = if sizable && pos < r.start + edge {
            Drag::TrimStart { region: r.id }
        } else if near_span_end || (sizable && near_end && corner) {
            Drag::Loop { region: r.id }
        } else if sizable && near_end {
            Drag::TrimEnd { region: r.id }
        } else if audio && inside_y < 15.0 && pos < r.start + fade_zone {
            Drag::FadeIn { region: r.id }
        } else if audio && inside_y < 15.0 && pos < r.end() && pos + fade_zone > r.end() {
            Drag::FadeOut { region: r.id }
        } else {
            Drag::Move { region: r.id, start: r.start, copy: false }
        };
        return (Some(row), Some((r.id, d)));
    }
    (Some(row), None)
}

/// The regions a tool acts on: the selection when the clicked region is part of it, otherwise
/// just the clicked one.
fn targets(ctx: &Ctx, clicked: Id) -> Vec<Id> {
    let sel = ctx.selected_region_ids();
    if sel.contains(&clicked) { sel } else { vec![clicked] }
}

fn create_midi_region(ctx: &Ctx, view: &View, row: usize, x: f64) -> Option<Id> {
    let (kind, tid, time) = {
        let s = ctx.s();
        let t = s.project.tracks.get(row)?;
        (t.kind, t.id, s.project.time())
    };
    if kind != TrackKind::Instrument {
        return None;
    }
    let start = time.snap(view.pos(x), time.ticks_per_bar()).min(view.pos(x)) / time.bar_len() * time.bar_len();
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
            loops: 0,
            color: None,
            content: Content::Midi(MidiContent::default()),
        });
    });
    ctx.select_region(Some(new_id));
    Some(new_id)
}

/// Applies a click with a tool other than the Pointer. Returns true if the click was used.
fn tool_click(ctx: &Ctx, view: &View, tool: Tool, row: Option<usize>, region: Option<Id>, x: f64) -> bool {
    let time = ctx.s().project.time();
    let at = time.snap(view.pos(x), grid_ticks(view.snap.get(), &time));
    match (tool, region) {
        (Tool::Pencil, None) => row.and_then(|r| create_midi_region(ctx, view, r, x)).is_some(),
        (Tool::Scissors, Some(id)) => {
            let ids = targets(ctx, id);
            ctx.edit("Split Region", |p| {
                for id in &ids {
                    p.split_region(*id, at);
                }
            });
            true
        }
        (Tool::Glue, Some(id)) => {
            let mut ids = ctx.selected_region_ids();
            if !ids.contains(&id) {
                ids.push(id);
            }
            let check = ctx.s().project.clone().join_regions(&ids);
            match check {
                Ok(_) => {
                    let mut kept = 0;
                    ctx.edit("Join Regions", |p| kept = p.join_regions(&ids).unwrap_or(0));
                    ctx.select_region(Some(kept));
                }
                Err(e) => ctx.message(e),
            }
            true
        }
        (Tool::Mute, Some(id)) => {
            let ids = targets(ctx, id);
            ctx.edit("Mute Region", |p| {
                for id in &ids {
                    if let Some(r) = p.region_mut(*id) {
                        r.muted = !r.muted;
                    }
                }
            });
            true
        }
        (Tool::Eraser, Some(id)) => {
            let ids = targets(ctx, id);
            ctx.edit("Delete Region", |p| {
                for id in &ids {
                    p.remove_region(*id);
                }
            });
            true
        }
        _ => false,
    }
}

/// Selects regions that touch the marquee rectangle (in lane coordinates).
fn marquee_select(ctx: &Ctx, view: &View, a: (f64, f64), b: (f64, f64), add: bool) {
    let (t0, t1) = (view.pos(a.0.min(b.0)), view.pos(a.0.max(b.0)));
    let (r0, r1) = ((a.1.min(b.1).max(0.0) / TRACK_H as f64) as usize, (a.1.max(b.1).max(0.0) / TRACK_H as f64) as usize);
    let mut ids: Vec<Id> = if add { ctx.selected_region_ids() } else { Vec::new() };
    {
        let s = ctx.s();
        let layout = rows(&s.project);
        let inside: Vec<usize> = layout.iter().enumerate().filter(|(k, _)| *k >= r0 && *k <= r1).filter_map(|(_, r)| match r {
            Row::Track(t) => Some(*t),
            Row::Stack(_) => None,
        }).collect();
        for t in inside.iter().map(|i| &s.project.tracks[*i]) {
            for r in t.regions.iter().filter(|r| r.start < t1 && r.span_end() > t0) {
                if !ids.contains(&r.id) {
                    ids.push(r.id);
                }
            }
        }
    }
    ctx.select_regions(ids);
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
            drag.set(Drag::None);
            let mods = g.current_event_state();
            let (shift, alt) = (mods.contains(gdk::ModifierType::SHIFT_MASK), mods.contains(gdk::ModifierType::ALT_MASK));
            if view.show_auto.get() {
                drag.set(auto_begin(&ctx, &view, x, y, alt));
                return;
            }
            let (row, region) = hit(&ctx, &view, x, y);
            let tool = view.tool.get();
            if tool != Tool::Pointer && tool_click(&ctx, &view, tool, row, region.map(|r| r.0), x) {
                return;
            }
            let track = row.and_then(|r| ctx.s().project.tracks.get(r).map(|t| t.id));
            match region {
                Some((id, _)) if shift => ctx.toggle_region(id),
                Some((id, d)) => {
                    ctx.select_region(Some(id));
                    drag.set(match d {
                        Drag::Move { region, start, .. } => Drag::Move { region, start, copy: alt },
                        other => other,
                    });
                }
                None => {
                    if !shift {
                        ctx.select_regions(Vec::new());
                    }
                    ctx.select_track(track);
                    drag.set(Drag::Marquee { x, y, add: shift });
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
        let (ctx, view, drag, offset, canvas) = (ctx.clone(), view.clone(), drag.clone(), offset.clone(), canvas.clone());
        g.connect_drag_end(move |g, dx, dy| {
            let d = drag.replace(Drag::None);
            offset.set((0.0, 0.0));
            canvas.queue_draw();
            let clicked = dx.abs() < 2.0 && dy.abs() < 2.0;
            if clicked {
                match d {
                    // A click on empty lane space moves the playhead there.
                    Drag::Marquee { x, .. } => {
                        let time = ctx.s().project.time();
                        ctx.locate(time.snap(view.pos(x), grid_ticks(view.snap.get(), &time)));
                    }
                    // Clicking one region of a multiple selection, without dragging, picks it alone.
                    Drag::Move { region, .. } => {
                        if g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) {
                            return;
                        }
                        ctx.select_regions(vec![region]);
                    }
                    _ => {}
                }
                ctx.emit(Change::Selection);
                return;
            }
            match d {
                Drag::AutoPoint { row, index } => {
                    let moved = auto_dragged(&ctx.s(), &view, row, index, (dx, dy));
                    let track = ctx.s().project.tracks.get(row).map(|t| t.id);
                    if let (Some((_, i, pos, value)), Some(track)) = (moved, track) {
                        let target = crate::automation::shown(&ctx.s(), track);
                        ctx.edit("Move Automation Point", |p| {
                            let lane = p.channel_mut(Some(track)).unwrap().lane_mut(target);
                            if i < lane.points.len() {
                                lane.points[i] = AutoPoint { pos, value };
                                lane.points.sort_by_key(|p| p.pos);
                            }
                        });
                    }
                }
                Drag::Marquee { x, y, add } => marquee_select(&ctx, &view, (x, y), (x + dx, y + dy), add),
                Drag::Move { region, copy, .. } => finish_move(&ctx, &view, d, region, copy, (dx, dy)),
                Drag::TrimStart { region } | Drag::TrimEnd { region } => finish_trim(&ctx, &view, d, region, (dx, dy)),
                Drag::Loop { region } | Drag::FadeIn { region } | Drag::FadeOut { region } => {
                    let sh = {
                        let s = ctx.s();
                        let time = s.project.time();
                        let Some((ti, r)) = s.project.find_region(region) else { return };
                        shape(r, ti, d, (dx, dy), &view, &time, &rows(&s.project), &[])
                    };
                    let label = if matches!(d, Drag::Loop { .. }) { "Loop Region" } else { "Region Fade" };
                    ctx.edit(label, |p| {
                        let Some(r) = p.region_mut(region) else { return };
                        r.loops = sh.loops;
                        if let Content::Audio(a) = &mut r.content {
                            a.fade_in = sh.fade_in;
                            a.fade_out = sh.fade_out;
                        }
                    });
                }
                _ => {}
            }
        });
    }
    canvas.add_controller(g);

    let right = gtk::GestureClick::new();
    right.set_button(gdk::BUTTON_SECONDARY);
    {
        let (ctx, view, canvas2, open_editor) = (ctx.clone(), view.clone(), canvas.clone(), open_editor.clone());
        right.connect_pressed(move |g, _, x, y| {
            if view.show_auto.get() {
                return;
            }
            g.set_state(gtk::EventSequenceState::Claimed);
            let (row, region) = hit(&ctx, &view, x, y);
            let anchor: gtk::Widget = canvas2.clone().upcast();
            match region {
                Some((id, _)) => crate::context_menu::region_menu(&ctx, &anchor, x, y, id, open_editor.clone()),
                None => {
                    let track = row.and_then(|r| ctx.s().project.tracks.get(r).map(|t| (t.id, t.kind)));
                    let create: Option<Rc<dyn Fn()>> = match (row, track) {
                        (Some(row), Some((_, TrackKind::Instrument))) => {
                            let (ctx, view, open_editor) = (ctx.clone(), view.clone(), open_editor.clone());
                            Some(Rc::new(move || {
                                if create_midi_region(&ctx, &view, row, x).is_some() {
                                    open_editor();
                                }
                            }))
                        }
                        _ => None,
                    };
                    crate::context_menu::lane_menu(&ctx, &anchor, x, y, track.map(|t| t.0), create);
                }
            }
        });
    }
    canvas.add_controller(right);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, view) = (ctx.clone(), view.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n != 2 || view.tool.get() != Tool::Pointer || view.show_auto.get() {
                return;
            }
            let (row, region) = hit(&ctx, &view, x, y);
            if region.is_some() {
                open_editor();
            } else if let Some(row) = row
                && create_midi_region(&ctx, &view, row, x).is_some()
            {
                open_editor();
            }
        });
    }
    canvas.add_controller(dbl);

    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, view, canvas) = (ctx.clone(), view.clone(), canvas.clone());
        motion.connect_motion(move |_, x, y| {
            let tool = view.tool.get();
            let cursor = if tool != Tool::Pointer {
                tool.cursor()
            } else {
                match hit(&ctx, &view, x, y).1 {
                    Some((_, Drag::TrimStart { .. } | Drag::TrimEnd { .. })) => "col-resize",
                    Some((_, Drag::Loop { .. })) => "e-resize",
                    Some((_, Drag::FadeIn { .. } | Drag::FadeOut { .. })) => "ew-resize",
                    _ => "default",
                }
            };
            canvas.set_cursor_from_name(Some(cursor));
        });
    }
    canvas.add_controller(motion);
}

/// Moves (or with Alt, copies) the selected regions by the dragged amount, keeping each on a
/// track of the right kind.
fn finish_move(ctx: &Ctx, view: &View, d: Drag, region: Id, copy: bool, off: (f64, f64)) {
    let (moves, sel) = {
        let s = ctx.s();
        let p = &s.project;
        let time = p.time();
        let sel = if s.selected_regions.contains(&region) { s.selected_regions.clone() } else { vec![region] };
        let moves: Vec<(Id, usize, Shape)> = sel
            .iter()
            .filter_map(|id| {
                let (ti, r) = p.find_region(*id)?;
                let mut sh = shape(r, ti, d, off, view, &time, &rows(p), &sel);
                if p.tracks[sh.row].kind != p.tracks[ti].kind {
                    sh.row = ti;
                }
                Some((*id, ti, sh))
            })
            .collect();
        (moves, sel)
    };
    let mut new = Vec::new();
    ctx.edit(if copy { "Copy Region" } else { "Move Region" }, |p| {
        for (id, _, sh) in &moves {
            if copy {
                let Some((_, r)) = p.find_region(*id) else { continue };
                let mut c = r.clone();
                c.id = p.alloc_id();
                c.start = sh.start;
                new.push(c.id);
                p.tracks[sh.row].regions.push(c);
            } else if let Some(mut r) = p.remove_region(*id) {
                r.start = sh.start;
                p.tracks[sh.row].regions.push(r);
            }
        }
    });
    ctx.select_regions(if copy { new } else { sel });
}

fn finish_trim(ctx: &Ctx, view: &View, d: Drag, id: Id, off: (f64, f64)) {
    let (sh, old_start) = {
        let s = ctx.s();
        let time = s.project.time();
        let Some((ti, r)) = s.project.find_region(id) else { return };
        (shape(r, ti, d, off, view, &time, &rows(&s.project), &[]), r.start)
    };
    ctx.edit("Resize Region", |p| {
        let time = p.time();
        let Some(r) = p.region_mut(id) else { return };
        let (start, length) = (sh.start, sh.length);
        match &mut r.content {
            Content::Audio(a) if matches!(d, Drag::TrimStart { .. }) => a.shift_start(start as i64 - old_start as i64),
            Content::Midi(m) if matches!(d, Drag::TrimStart { .. }) => {
                let shift = time.ticks_between(start.min(old_start), start.max(old_start)) as i64 * if start > old_start { 1 } else { -1 };
                m.shift_ticks(shift);
            }
            _ => {}
        }
        r.start = start;
        r.length = length;
    });
}

/// Whether the cycle shown is a skip range, counting a new range being dragged out.
fn cycle_skips(p: &Project, drag: Drag) -> bool {
    match drag {
        Drag::Cycle { skip: Some(k), .. } => k,
        _ => p.cycle_skip,
    }
}

/// Diagonal hatching from `a` to `b`, `h` high, as a skip range is marked.
fn stripes(s: &gtk::Snapshot, a: f32, b: f32, h: f32, col: &gdk::RGBA) {
    if b <= a {
        return;
    }
    s.push_clip(&gtk::graphene::Rect::new(a, 0.0, b - a, h));
    let mut x = a - h;
    while x < b {
        s.save();
        s.translate(&gtk::graphene::Point::new(x, 0.0));
        s.rotate(45.0);
        rect(s, 0.0, -1.0, h * 1.5, 2.0, col);
        s.restore();
        x += 6.0;
    }
    s.pop();
}

/// The loop range as an in-progress drag with the pointer at `x` would leave it.
fn dragged_cycle(time: &TimeMap, view: &View, cycle: Range, drag: Drag, x: f64) -> Range {
    let bar = time.ticks_per_bar();
    match drag {
        Drag::Cycle { from, .. } => Range::new(from, time.snap(view.pos(x), bar)),
        Drag::CycleMove { grab } => {
            let delta = time.snap(view.pos(x), bar) as i64 - time.snap(grab, bar) as i64;
            let start = time.snap((cycle.start as i64 + delta).max(0) as u64, bar);
            Range::new(start, time.snap(start + cycle.len(), bar))
        }
        _ => cycle,
    }
}

/// The punch range as an in-progress drag with the pointer at `x` would leave it. Punches land on
/// the editing grid rather than on bars, since a drop-in often starts mid-bar.
fn dragged_punch(time: &TimeMap, view: &View, punch: Range, drag: Drag, x: f64) -> Range {
    let grid = grid_ticks(view.snap.get(), time);
    match drag {
        Drag::Punch { from } => Range::new(from, time.snap(view.pos(x), grid)),
        Drag::PunchMove { grab } => {
            let delta = time.snap(view.pos(x), grid) as i64 - time.snap(grab, grid) as i64;
            let start = time.snap((punch.start as i64 + delta).max(0) as u64, grid);
            Range::new(start, start + punch.len())
        }
        _ => punch,
    }
}

/// What pressing the lower half of the ruler at `x` takes hold of while punch is on.
fn punch_grab(time: &TimeMap, view: &View, punch: Range, x: f64) -> (Drag, &'static str) {
    let (a, b) = (view.x(punch.start) as f64, view.x(punch.end) as f64);
    if (x - a).abs() <= EDGE {
        return (Drag::Punch { from: punch.end }, "col-resize");
    }
    if (x - b).abs() <= EDGE {
        return (Drag::Punch { from: punch.start }, "col-resize");
    }
    if x > a && x < b {
        return (Drag::PunchMove { grab: view.pos(x) }, "grab");
    }
    (Drag::Punch { from: time.snap(view.pos(x), grid_ticks(view.snap.get(), time)) }, "default")
}

/// What pressing the loop lane at `x` takes hold of, and the cursor that says so.
fn cycle_grab(time: &TimeMap, view: &View, cycle: Range, x: f64) -> (Drag, &'static str) {
    if !cycle.is_empty() {
        let (a, b) = (view.x(cycle.start) as f64, view.x(cycle.end) as f64);
        if (x - a).abs() <= EDGE {
            return (Drag::Cycle { from: cycle.end, skip: None }, "col-resize");
        }
        if (x - b).abs() <= EDGE {
            return (Drag::Cycle { from: cycle.start, skip: None }, "col-resize");
        }
        if x > a && x < b {
            return (Drag::CycleMove { grab: view.pos(x) }, "grab");
        }
    }
    (Drag::Cycle { from: time.snap(view.pos(x), time.ticks_per_bar()), skip: Some(false) }, "crosshair")
}

fn install_ruler_input(
    ctx: &Ctx,
    view: &Rc<View>,
    ruler: &Canvas,
    timeline: &Canvas,
    drag: Rc<Cell<Drag>>,
    offset: Rc<Cell<(f64, f64)>>,
    hover: Rc<Cell<bool>>,
) {
    // Pointer positions count from the tracks area's left edge, as everything drawn here does.
    let lane_x = {
        let (view, ruler) = (view.clone(), ruler.clone());
        move |x: f64| x - view.shift(&ruler) as f64
    };
    let motion = gtk::EventControllerMotion::new();
    {
        let (ctx, view, ruler, hover, drag, lane_x) = (ctx.clone(), view.clone(), ruler.clone(), hover.clone(), drag.clone(), lane_x.clone());
        motion.connect_motion(move |m, x, y| {
            let over = y < CYCLE_H;
            if hover.replace(over) != over {
                ruler.queue_draw();
            }
            let skip = m.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            let cursor = match drag.get() {
                Drag::CycleMove { .. } | Drag::PunchMove { .. } => "grabbing",
                _ if over && skip => "crosshair",
                _ if over => {
                    let s = ctx.s();
                    cycle_grab(&s.project.time(), &view, s.project.cycle, lane_x(x)).1
                }
                _ if ctx.s().project.punch_on => {
                    let s = ctx.s();
                    punch_grab(&s.project.time(), &view, s.project.punch, lane_x(x)).1
                }
                _ => "default",
            };
            ruler.set_cursor_from_name(Some(cursor));
        });
    }
    {
        let (ruler, hover) = (ruler.clone(), hover.clone());
        motion.connect_leave(move |_| {
            if hover.replace(false) {
                ruler.queue_draw();
            }
        });
    }
    ruler.add_controller(motion);
    ruler.set_has_tooltip(true);
    {
        let ctx = ctx.clone();
        ruler.connect_query_tooltip(move |_, _, y, _, tip| {
            let text = if (y as f64) < CYCLE_H {
                "Cycle: drag to set the range, drag it to move it or its edges to resize it, and click to turn it on or off. \
                 Ctrl-drag sets a range that playback skips over instead."
            } else if ctx.s().project.punch_on {
                "Punch: drag it to move it or its edges to resize it, or drag anywhere along here to set a new range. \
                 Click to move the playhead."
            } else {
                return false;
            };
            tip.set_text(Some(text));
            true
        });
    }

    let g = gtk::GestureDrag::new();
    {
        let (ctx, view, drag, offset, ruler, lane_x) = (ctx.clone(), view.clone(), drag.clone(), offset.clone(), ruler.clone(), lane_x.clone());
        g.connect_drag_begin(move |g, x, y| {
            let time = ctx.s().project.time();
            let x = lane_x(x);
            if y < CYCLE_H {
                let d = if g.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    Drag::Cycle { from: time.snap(view.pos(x), time.ticks_per_bar()), skip: Some(true) }
                } else {
                    cycle_grab(&time, &view, ctx.s().project.cycle, x).0
                };
                if matches!(d, Drag::CycleMove { .. }) {
                    ruler.set_cursor_from_name(Some("grabbing"));
                }
                drag.set(d);
                offset.set((x, 0.0));
            } else if ctx.s().project.punch_on {
                let d = punch_grab(&time, &view, ctx.s().project.punch, x).0;
                if matches!(d, Drag::PunchMove { .. }) {
                    ruler.set_cursor_from_name(Some("grabbing"));
                }
                drag.set(d);
                offset.set((x, 0.0));
            } else {
                let grid = grid_ticks(view.snap.get(), &time);
                ctx.locate(time.snap(view.pos(x), grid));
            }
        });
    }
    {
        let (ruler, timeline, drag, offset, lane_x) = (ruler.clone(), timeline.clone(), drag.clone(), offset.clone(), lane_x.clone());
        g.connect_drag_update(move |g, dx, _| {
            if matches!(drag.get(), Drag::Cycle { .. } | Drag::CycleMove { .. } | Drag::Punch { .. } | Drag::PunchMove { .. }) {
                let (sx, _) = g.start_point().unwrap_or((0.0, 0.0));
                offset.set((lane_x(sx + dx), 0.0));
                ruler.queue_draw();
                timeline.queue_draw();
            }
        });
    }
    {
        let (ctx, view, drag, ruler) = (ctx.clone(), view.clone(), drag.clone(), ruler.clone());
        g.connect_drag_end(move |g, dx, _| {
            let d = drag.replace(Drag::None);
            if matches!(d, Drag::Punch { .. } | Drag::PunchMove { .. }) {
                end_punch(&ctx, &view, g, d, dx, &lane_x);
                ruler.set_cursor_from_name(Some(if matches!(d, Drag::PunchMove { .. }) { "grab" } else { "default" }));
                return;
            }
            if !matches!(d, Drag::Cycle { .. } | Drag::CycleMove { .. }) {
                return;
            }
            if matches!(d, Drag::CycleMove { .. }) {
                ruler.set_cursor_from_name(Some("grab"));
            }
            if dx.abs() < 3.0 {
                ctx.tweak(|p| p.cycle_on = !p.cycle_on);
                return;
            }
            let (sx, _) = g.start_point().unwrap_or((0.0, 0.0));
            let r = {
                let s = ctx.s();
                dragged_cycle(&s.project.time(), &view, s.project.cycle, d, lane_x(sx + dx))
            };
            if !r.is_empty() {
                ctx.tweak(|p| {
                    p.cycle = r;
                    p.cycle_on = true;
                    if let Drag::Cycle { skip: Some(k), .. } = d {
                        p.cycle_skip = k;
                    }
                });
            }
        });
    }
    ruler.add_controller(g);
}

/// Finishes a press on the punch strip: a click locates the playhead there, a drag sets the range.
fn end_punch(ctx: &Ctx, view: &View, g: &gtk::GestureDrag, d: Drag, dx: f64, lane_x: &impl Fn(f64) -> f64) {
    let (sx, _) = g.start_point().unwrap_or((0.0, 0.0));
    let time = ctx.s().project.time();
    if dx.abs() < 3.0 {
        ctx.locate(time.snap(view.pos(lane_x(sx)), grid_ticks(view.snap.get(), &time)));
        return;
    }
    let r = dragged_punch(&time, view, ctx.s().project.punch, d, lane_x(sx + dx));
    if !r.is_empty() {
        ctx.tweak(|p| p.punch = r);
    }
}

fn install_drop(ctx: &Ctx, view: &Rc<View>, canvas: &Canvas) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let (ctx, view) = (ctx.clone(), view.clone());
    target.connect_drop(move |_, value, x, y| {
        let Ok(list) = value.get::<gdk::FileList>() else { return false };
        let paths: Vec<_> = list.files().iter().filter_map(|f| f.path()).collect();
        // Another application's project or exports bring their own arrangement.
        let (projects, paths): (Vec<_>, Vec<_>) =
            paths.into_iter().partition(|p| !crate::import::is_audio(p) && crate::project_import::is_importable(p));
        for path in projects {
            let c = ctx.clone();
            let notify: crate::project_import::Notify = Rc::new(move |msg| {
                c.m().message = Some(msg);
                c.emit(Change::Message);
            });
            crate::project_import::import_path(&ctx, path, notify);
        }
        if paths.is_empty() {
            return true;
        }
        // Onto the track under the pointer, a stack's first track, or new tracks below.
        let row = {
            let p = &ctx.s().project;
            match row_at(&rows(p), y) {
                Some(Row::Track(i)) => i,
                Some(Row::Stack(id)) => p.tracks.iter().position(|t| t.stack == Some(id)).unwrap_or(p.tracks.len()),
                None => p.tracks.len(),
            }
        };
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
                    loops: 0,
                    color: None,
                    content: Content::Audio(AudioContent::new(file)),
                });
                row += 1;
            }
        });
    });
}
