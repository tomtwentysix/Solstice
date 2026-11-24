//! The Drum Editor: a row per drum sound, hits as diamonds, a grid per row.

use crate::canvas::{self, Canvas, rect, rgba, text, track_color};
use crate::context_menu::Menu;
use crate::drum_rows::*;
use crate::editor::{draw_beats, playhead};
use crate::grooves;
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const GRIDS: [(u32, &str); 8] =
    [(PPQ, "1/4"), (PPQ / 2, "1/8"), (PPQ / 3, "1/8T"), (PPQ / 4, "1/16"), (PPQ / 6, "1/16T"), (PPQ / 8, "1/32"), (PPQ / 12, "1/32T"), (PPQ / 16, "1/64")];
const LANE_H: i32 = 72;
const HIT_D: f32 = 13.0;
/// How far from a diamond's centre, in pixels, a press still takes the hit.
const REACH: f64 = 7.0;

fn grid_name(g: u32) -> String {
    GRIDS.iter().find(|x| x.0 == g).map_or_else(|| format!("{g} ticks"), |x| x.1.to_string())
}

/// The start a hit at `tick` snaps to on a `grid`; 0 leaves it where it is.
fn snap(tick: f64, grid: u32) -> u32 {
    let tick = tick.max(0.0);
    if grid == 0 { tick as u32 } else { (tick / grid as f64).floor() as u32 * grid }
}

fn snap_round(tick: f64, grid: u32) -> u32 {
    let tick = tick.max(0.0);
    if grid == 0 { tick.round() as u32 } else { (tick / grid as f64).round() as u32 * grid }
}

/// What the editor shows for the selected region.
struct View {
    region: Region,
    /// The region's length in ticks.
    ticks: u32,
    color: u8,
    notes: Vec<Note>,
    rows: Vec<DrumRow>,
    dq: DrumQuantize,
    pattern: bool,
}

impl View {
    /// The grid of `pitch`'s row, 0 when it is off the grid.
    fn grid(&self, pitch: u8) -> u32 {
        self.rows.iter().find(|r| r.pitch == pitch).and_then(|r| r.grid).unwrap_or(self.dq.grid)
    }

    /// The step painting and new hits use on `pitch`'s row: its grid, or the editor's when off.
    fn step(&self, pitch: u8) -> u32 {
        match self.grid(pitch) {
            0 => self.dq.grid.max(1),
            g => g,
        }
    }

    fn row_of(&self, pitch: u8) -> Option<usize> {
        self.rows.iter().position(|r| r.pitch == pitch)
    }

    fn used(&self) -> Vec<u8> {
        self.notes.iter().map(|n| n.pitch).collect()
    }
}

fn view(ctx: &Ctx) -> Option<View> {
    let s = ctx.s();
    let id = s.selected_region?;
    let (ti, r) = s.project.find_region(id)?;
    let Content::Midi(m) = &r.content else { return None };
    let t = &s.project.tracks[ti];
    let ticks = s.project.time().ticks_between(r.start, r.start + r.length) as u32;
    Some(View {
        region: r.clone(),
        ticks,
        color: t.color,
        notes: m.notes.clone(),
        rows: hit_rows(t, &m.notes),
        dq: t.drum_quantize,
        pattern: m.pattern.is_some(),
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Move { grab: Note, copy: bool },
    Marquee { x: f64, y: f64, add: bool },
    /// A paint or erase stroke along one row: where it was pressed and last reached, in ticks,
    /// and whether it has changed anything yet.
    Paint { pitch: u8, from: u32, last: u32, started: bool },
    Erase { pitch: u8, last: u32 },
}

struct Hits {
    /// Pixels per tick and the first visible tick.
    ppt: Cell<f64>,
    t0: Cell<f64>,
    hadj: gtk::Adjustment,
    selected: RefCell<Vec<Note>>,
    drag: Cell<Drag>,
    offset: Cell<(f64, f64)>,
    clipboard: RefCell<Vec<Note>>,
    insert_velocity: Cell<u8>,
    focus: Cell<usize>,
    header: Rc<Header>,
    groove: RefCell<Option<Groove>>,
    groove_velocity: Cell<f32>,
    /// Set while the toolbar is updated from the project, so it doesn't edit it back.
    syncing: Cell<bool>,
    /// The press of a double-click that added this hit; the second press leaves it be.
    just_added: Cell<Option<Note>>,
    velocity_drag: Cell<bool>,
}

impl Hits {
    fn x(&self, tick: f64) -> f32 {
        ((tick - self.t0.get()) * self.ppt.get()) as f32
    }

    fn tick(&self, x: f64) -> f64 {
        self.t0.get() + x / self.ppt.get()
    }
}

/// Edits the region's notes as one undo step, turning a step pattern into hits first.
fn edit_hits(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Vec<Note>)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| change(p, id, f));
}

/// `edit_hits` during a gesture; `end_gesture` makes the undo step.
fn edit_live(ctx: &Ctx, f: impl FnOnce(&mut Vec<Note>)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.live(true, |p| change(p, id, f));
}

fn change(p: &mut Project, id: Id, f: impl FnOnce(&mut Vec<Note>)) {
    release(p, id);
    if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
        f(&mut m.notes);
        m.notes.sort_by_key(|n| (n.start, n.pitch));
    }
}

fn edit_quantize(ctx: &Ctx, label: &str, f: impl FnOnce(&mut DrumQuantize)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        if let Some(ti) = p.find_region(id).map(|(ti, _)| ti) {
            f(&mut p.tracks[ti].drum_quantize);
        }
    });
}

/// Runs a hit function on the selection, or on every hit when nothing is selected, and keeps
/// the selection on the results.
fn apply(ctx: &Ctx, h: &Hits, label: &str, f: impl FnOnce(&View, &[Note]) -> Vec<Note>) {
    let Some(v) = view(ctx) else { return };
    let old = {
        let sel = h.selected.borrow();
        if sel.is_empty() { v.notes.clone() } else { sel.clone() }
    };
    let new = f(&v, &old);
    edit_hits(ctx, label, |notes| {
        midi_tools::replace(notes, &old, &new);
    });
    *h.selected.borrow_mut() = new;
}

fn quantize(ctx: &Ctx, h: &Hits, iterative: bool) {
    let groove = h.groove.borrow().clone();
    let velocity = h.groove_velocity.get();
    let label = if iterative { "Iterative Quantize" } else { "Quantize" };
    apply(ctx, h, label, |v, n| {
        let strength = if iterative { v.dq.iterative } else { v.dq.strength };
        match &groove {
            Some(g) => midi_tools::groove_quantize(n, g, strength, velocity),
            None => midi_tools::quantize_rows(n, |p| v.grid(p), strength, v.dq.swing),
        }
    });
}

fn humanise(ctx: &Ctx, h: &Hits) {
    let tempo = {
        let s = ctx.s();
        let Some((_, r)) = s.selected_region.and_then(|id| s.project.find_region(id)) else { return };
        s.project.time().tempo_at(r.start)
    };
    apply(ctx, h, "Humanise", |v, n| {
        let ticks = (v.dq.humanise_ms as f64 / 1000.0 * tempo / 60.0 * PPQ as f64).round() as u32;
        midi_tools::humanise(n, ticks, v.dq.humanise_velocity, glib::random_int())
    });
}

/// The hit whose diamond is under (x, y).
fn hit_at(h: &Hits, v: &View, x: f64, y: f64) -> Option<Note> {
    let row = v.rows.get((y / ROW_H as f64).floor().max(0.0) as usize)?;
    v.notes.iter().rev().find(|n| n.pitch == row.pitch && (h.x(n.start as f64) as f64 - x).abs() <= REACH).copied()
}

/// Where a hit would be if the current move ended now.
fn dragged(h: &Hits, v: &View, n: &Note) -> Note {
    let Drag::Move { grab, .. } = h.drag.get() else { return *n };
    let sel = h.selected.borrow();
    if !(grab == *n || (sel.contains(&grab) && sel.contains(n))) {
        return *n;
    }
    let (dx, dy) = h.offset.get();
    let rows = (dy / ROW_H as f64).round() as i64;
    let pitch = match v.row_of(n.pitch) {
        Some(i) if !v.rows.is_empty() => v.rows[(i as i64 + rows).clamp(0, v.rows.len() as i64 - 1) as usize].pitch,
        _ => n.pitch,
    };
    let start = snap_round(n.start as f64 + dx / h.ppt.get(), v.grid(pitch));
    Note { start, pitch, ..*n }
}

pub fn build(ctx: &Ctx, go: Rc<dyn Fn(&'static str)>) -> gtk::Widget {
    let h = Rc::new(Hits {
        ppt: Cell::new(0.12),
        t0: Cell::new(0.0),
        hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
        selected: RefCell::default(),
        drag: Cell::new(Drag::None),
        offset: Cell::new((0.0, 0.0)),
        clipboard: RefCell::default(),
        insert_velocity: Cell::new(100),
        focus: Cell::new(0),
        header: Rc::new(Header::default()),
        groove: RefCell::default(),
        groove_velocity: Cell::new(0.0),
        syncing: Cell::new(false),
        just_added: Cell::new(None),
        velocity_drag: Cell::new(false),
    });

    let grid = {
        let (ctx, h) = (ctx.clone(), h.clone());
        Canvas::new(move |c, s, w, height| draw(&ctx, &h, c, s, w, height))
    };
    grid.set_hexpand(true);
    grid.set_focusable(true);
    grid.set_focus_on_click(true);
    let source: Rc<dyn RowSource> = Rc::new(HitRows { h: h.clone() });
    let header = {
        let (ctx, source, h, grid) = (ctx.clone(), source.clone(), h.clone(), grid.clone());
        Canvas::new(move |c, s, w, _| draw_header(&ctx, &*source, &h.header, &grid, c, s, w))
    };
    header.set_size_request(HEAD_W as i32, -1);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&header);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&grid);
    let vscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&body).build();

    let velocity = {
        let (ctx, h) = (ctx.clone(), h.clone());
        Canvas::new(move |c, s, w, height| draw_velocity(&ctx, &h, c, s, w, height))
    };
    velocity.set_hexpand(true);
    velocity.set_size_request(-1, LANE_H);
    let lane_label = gtk::Label::new(Some("Velocity"));
    lane_label.add_css_class("caption");
    lane_label.add_css_class("dim-label");
    lane_label.set_size_request(HEAD_W as i32, -1);
    lane_label.set_wrap(true);
    let lane = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    lane.append(&lane_label);
    lane.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    lane.append(&velocity);

    let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&h.hadj));
    hbar.set_margin_start(HEAD_W as i32 + 1);
    let redraw: Rc<dyn Fn()> = {
        let (g, v, hd) = (grid.clone(), velocity.clone(), header.clone());
        Rc::new(move || {
            g.queue_draw();
            v.queue_draw();
            hd.queue_draw();
        })
    };
    {
        let (h, redraw) = (h.clone(), redraw.clone());
        h.hadj.clone().connect_value_changed(move |a| {
            h.t0.set(a.value());
            redraw();
        });
    }

    let (bar, title, vel, grid_pick) = toolbar(ctx, &h, &go, &redraw);
    let banner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    banner.set_margin_start(6);
    banner.set_margin_end(6);
    banner.set_margin_top(4);
    banner.set_margin_bottom(4);
    let banner_text = gtk::Label::new(Some("This region is a step pattern. Editing it here turns it into hits."));
    banner_text.set_hexpand(true);
    banner_text.set_xalign(0.0);
    banner_text.set_wrap(true);
    banner.append(&banner_text);
    let to_steps = gtk::Button::with_label("Open in Step Sequencer");
    {
        let go = go.clone();
        to_steps.connect_clicked(move |_| go("steps"));
    }
    banner.append(&to_steps);
    banner.set_visible(false);

    install_grid_input(ctx, &h, &grid, &redraw);
    install_velocity_input(ctx, &h, &velocity, &redraw);
    install_header_input(ctx, source, h.header.clone(), &header, &grid);

    let refresh: Rc<dyn Fn()> = {
        let (ctx, h, grid, header, title, vel, grid_pick, banner, lane_label, redraw) =
            (ctx.clone(), h.clone(), grid.clone(), header.clone(), title.clone(), vel.clone(), grid_pick.clone(), banner.clone(), lane_label.clone(), redraw.clone());
        Rc::new(move || {
            let Some(v) = view(&ctx) else { return };
            let rows_h = (v.rows.len() as f32 * ROW_H) as i32;
            grid.set_size_request(-1, rows_h);
            header.set_size_request(HEAD_W as i32, rows_h);
            title.set_text(&v.region.name);
            banner.set_visible(v.pattern);
            let time = ctx.s().project.time();
            let page = grid.width().max(1) as f64 / h.ppt.get();
            h.hadj.configure(h.t0.get(), 0.0, (v.ticks as f64).max(page) + time.ticks_per_bar() as f64, page / 10.0, page, page);
            if h.focus.get() >= v.rows.len() {
                h.focus.set(v.rows.len().saturating_sub(1));
            }
            let name = v.rows.get(h.focus.get()).map_or(String::new(), |r| format!("Velocity · {}", r.name));
            lane_label.set_text(&name);
            h.syncing.set(true);
            grid_pick.set_selected(GRIDS.iter().position(|g| g.0 == v.dq.grid).unwrap_or(3) as u32);
            vel.set_value(h.selected.borrow().first().map_or(h.insert_velocity.get(), |n| n.velocity) as f64);
            h.syncing.set(false);
            redraw();
        })
    };
    {
        let (ctx2, h, refresh) = (ctx.clone(), h.clone(), refresh.clone());
        let last = Cell::new(None::<Id>);
        ctx.on(move |c| {
            if matches!(c, Change::LiveNote) {
                return;
            }
            let cur = ctx2.s().selected_region;
            if cur != last.get() {
                h.selected.borrow_mut().clear();
                h.t0.set(0.0);
                h.focus.set(0);
                last.set(cur);
            } else if let Some(v) = view(&ctx2) {
                // Undo, and edits that change velocity, leave the selection on hits that moved on.
                let keys: Vec<(u32, u8)> = h.selected.borrow().iter().map(|n| (n.start, n.pitch)).collect();
                *h.selected.borrow_mut() = v.notes.iter().filter(|n| keys.contains(&(n.start, n.pitch))).copied().collect();
            }
            refresh();
        });
    }
    {
        let refresh = refresh.clone();
        grid.connect_map(move |_| refresh());
    }
    {
        let refresh = refresh.clone();
        let notify = gtk::EventControllerLegacy::new();
        notify.connect_event(move |_, e| {
            if e.event_type() == gdk::EventType::ButtonRelease {
                refresh();
            }
            glib::Propagation::Proceed
        });
        grid.add_controller(notify);
    }
    {
        let (ctx, redraw) = (ctx.clone(), redraw.clone());
        grid.add_tick_callback(move |_, _| {
            if ctx.playing() {
                redraw();
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let redraw = redraw.clone();
        grid.connect_has_focus_notify(move |_| redraw());
    }

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tilt = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    {
        let (h, redraw) = (h.clone(), redraw.clone());
        tilt.connect_scroll(move |_, dx, _| {
            if dx == 0.0 {
                return glib::Propagation::Proceed;
            }
            h.hadj.set_value(h.hadj.value() + dx * 40.0 / h.ppt.get());
            redraw();
            glib::Propagation::Stop
        });
    }
    root.add_controller(tilt);
    root.append(&bar);
    root.append(&banner);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&vscroll);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&lane);
    root.append(&hbar);
    refresh();
    root.upcast()
}

fn spin_row(grid: &gtk::Grid, row: i32, label: &str, lo: f64, hi: f64, step: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(lo, hi, step);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(&spin, 1, row, 1, 1);
    spin
}

fn popover_grid() -> gtk::Grid {
    gtk::Grid::builder().row_spacing(6).column_spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build()
}

fn toolbar(ctx: &Ctx, h: &Rc<Hits>, go: &Rc<dyn Fn(&'static str)>, redraw: &Rc<dyn Fn()>) -> (gtk::Box, gtk::Label, gtk::SpinButton, gtk::DropDown) {
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

    let names: Vec<&str> = GRIDS.iter().map(|g| g.1).collect();
    let grid_pick = gtk::DropDown::from_strings(&names);
    grid_pick.set_tooltip_text(Some("Grid for rows that follow the editor"));
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        grid_pick.connect_selected_notify(move |d| {
            if h.syncing.get() {
                return;
            }
            let g = GRIDS[d.selected() as usize].0;
            edit_quantize(&ctx, "Set Grid", |q| q.grid = g);
        });
    }
    bar.append(&gtk::Label::new(Some("Grid")));
    bar.append(&grid_pick);

    bar.append(&quantize_controls(ctx, h));
    bar.append(&humanise_controls(ctx, h));

    let vel = gtk::SpinButton::with_range(1.0, 127.0, 1.0);
    vel.set_tooltip_text(Some("Velocity of the selected hits, or of new hits when none are selected"));
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        vel.connect_value_changed(move |s| {
            if h.syncing.get() {
                return;
            }
            let value = s.value() as u8;
            if h.selected.borrow().is_empty() {
                h.insert_velocity.set(value);
            } else if h.selected.borrow().iter().any(|n| n.velocity != value) {
                apply(&ctx, &h, "Change Velocity", |_, n| n.iter().map(|n| Note { velocity: value, ..*n }).collect());
            }
        });
    }
    bar.append(&gtk::Label::new(Some("Velocity")));
    bar.append(&vel);

    for (icon, f) in [("zoom-out-symbolic", 1.0 / 1.5), ("zoom-in-symbolic", 1.5)] {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        let (h, redraw) = (h.clone(), redraw.clone());
        b.connect_clicked(move |_| {
            h.ppt.set((h.ppt.get() * f).clamp(0.01, 2.0));
            redraw();
        });
        bar.append(&b);
    }
    for (label, page) in [("Step Sequencer", "steps"), ("Piano Roll", "midi"), ("Event List", "events")] {
        let b = gtk::Button::with_label(label);
        let go = go.clone();
        b.connect_clicked(move |_| go(page));
        bar.append(&b);
    }
    (bar, title, vel, grid_pick)
}

fn quantize_controls(ctx: &Ctx, h: &Rc<Hits>) -> gtk::Box {
    let q = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    q.add_css_class("linked");
    let button = gtk::Button::with_label("Quantize");
    button.set_tooltip_text(Some("Quantize the selected hits, or all hits, to their row grids (Q)"));
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        button.connect_clicked(move |_| quantize(&ctx, &h, false));
    }
    let menu = gtk::MenuButton::new();
    menu.set_tooltip_text(Some("Quantize settings"));
    let pop = gtk::Popover::new();
    let grid = popover_grid();
    let model = gtk::StringList::new(&[]);
    let groove_pick = gtk::DropDown::builder().model(&model).build();
    let l = gtk::Label::new(Some("Groove"));
    l.set_xalign(0.0);
    grid.attach(&l, 0, 0, 1, 1);
    grid.attach(&groove_pick, 1, 0, 1, 1);
    let strength = spin_row(&grid, 1, "Strength %", 0.0, 100.0, 5.0);
    let swing = spin_row(&grid, 2, "Swing %", 0.0, 100.0, 5.0);
    let iterative = spin_row(&grid, 3, "Iterative %", 5.0, 100.0, 5.0);
    let groove_vel = spin_row(&grid, 4, "Groove Velocity %", 0.0, 100.0, 5.0);
    let again = gtk::Button::with_label("Iterative Quantize");
    again.set_tooltip_text(Some("Move the hits part of the way to their grid (Shift+Q)"));
    grid.attach(&again, 0, 5, 2, 1);
    let auto = gtk::Switch::new();
    auto.set_halign(gtk::Align::End);
    auto.set_tooltip_text(Some("Quantize what is recorded on this track to its row grids"));
    let l = gtk::Label::new(Some("Auto Quantize"));
    l.set_xalign(0.0);
    grid.attach(&l, 0, 6, 1, 1);
    grid.attach(&auto, 1, 6, 1, 1);
    pop.set_child(Some(&grid));
    menu.set_popover(Some(&pop));

    let list: Rc<RefCell<Vec<Groove>>> = Rc::default();
    let sync = {
        let (ctx, h, list, model, groove_pick, strength, swing, iterative, groove_vel, auto) =
            (ctx.clone(), h.clone(), list.clone(), model.clone(), groove_pick.clone(), strength.clone(), swing.clone(), iterative.clone(), groove_vel.clone(), auto.clone());
        move || {
            let Some(v) = view(&ctx) else { return };
            h.syncing.set(true);
            *list.borrow_mut() = grooves::all();
            let names: Vec<String> = std::iter::once("Grid".to_string()).chain(list.borrow().iter().map(|g| g.name.clone())).collect();
            let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            model.splice(0, model.n_items(), &names);
            let chosen = h.groove.borrow().clone();
            let at = chosen.and_then(|c| list.borrow().iter().position(|g| g.name == c.name)).map_or(0, |i| i as u32 + 1);
            groove_pick.set_selected(at);
            strength.set_value(v.dq.strength as f64 * 100.0);
            swing.set_value(v.dq.swing as f64 * 100.0);
            iterative.set_value(v.dq.iterative as f64 * 100.0);
            groove_vel.set_value(h.groove_velocity.get() as f64 * 100.0);
            groove_vel.set_sensitive(at > 0);
            auto.set_active(v.dq.auto);
            h.syncing.set(false);
        }
    };
    pop.connect_show(move |_| sync());
    {
        let (h, list, groove_vel) = (h.clone(), list.clone(), groove_vel.clone());
        groove_pick.connect_selected_notify(move |d| {
            if h.syncing.get() {
                return;
            }
            let i = d.selected();
            *h.groove.borrow_mut() = i.checked_sub(1).and_then(|i| list.borrow().get(i as usize).cloned());
            groove_vel.set_sensitive(i > 0);
        });
    }
    type Setter = fn(&mut DrumQuantize, f32);
    let settings: [(&gtk::SpinButton, Setter); 3] =
        [(&strength, |q, v| q.strength = v), (&swing, |q, v| q.swing = v), (&iterative, |q, v| q.iterative = v)];
    for (spin, set) in settings {
        let (ctx, h) = (ctx.clone(), h.clone());
        spin.connect_value_changed(move |s| {
            if !h.syncing.get() {
                let v = s.value() as f32 / 100.0;
                edit_quantize(&ctx, "Change Quantize Settings", |q| set(q, v));
            }
        });
    }
    {
        let h = h.clone();
        groove_vel.connect_value_changed(move |s| h.groove_velocity.set(s.value() as f32 / 100.0));
    }
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        auto.connect_active_notify(move |s| {
            if !h.syncing.get() {
                let on = s.is_active();
                edit_quantize(&ctx, if on { "Turn On Auto Quantize" } else { "Turn Off Auto Quantize" }, |q| q.auto = on);
            }
        });
    }
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        again.connect_clicked(move |_| quantize(&ctx, &h, true));
    }
    q.append(&button);
    q.append(&menu);
    q
}

fn humanise_controls(ctx: &Ctx, h: &Rc<Hits>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    b.add_css_class("linked");
    let button = gtk::Button::with_label("Humanise");
    button.set_tooltip_text(Some("Move the selected hits, or all hits, slightly off the grid and vary their velocity"));
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        button.connect_clicked(move |_| humanise(&ctx, &h));
    }
    let menu = gtk::MenuButton::new();
    menu.set_tooltip_text(Some("Humanise settings"));
    let pop = gtk::Popover::new();
    let grid = popover_grid();
    let timing = spin_row(&grid, 0, "Timing (ms)", 0.0, 50.0, 1.0);
    let velocity = spin_row(&grid, 1, "Velocity %", 0.0, 50.0, 1.0);
    pop.set_child(Some(&grid));
    menu.set_popover(Some(&pop));
    {
        let (ctx, h, timing, velocity) = (ctx.clone(), h.clone(), timing.clone(), velocity.clone());
        pop.connect_show(move |_| {
            let Some(v) = view(&ctx) else { return };
            h.syncing.set(true);
            timing.set_value(v.dq.humanise_ms as f64);
            velocity.set_value(v.dq.humanise_velocity as f64 * 100.0);
            h.syncing.set(false);
        });
    }
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        timing.connect_value_changed(move |s| {
            if !h.syncing.get() {
                let v = s.value() as f32;
                edit_quantize(&ctx, "Change Humanise Settings", |q| q.humanise_ms = v);
            }
        });
    }
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        velocity.connect_value_changed(move |s| {
            if !h.syncing.get() {
                let v = s.value() as f32 / 100.0;
                edit_quantize(&ctx, "Change Humanise Settings", |q| q.humanise_velocity = v);
            }
        });
    }
    b.append(&button);
    b.append(&menu);
    b
}

/// A hit: a diamond centred on (x, y), `d` across.
fn diamond(s: &gtk::Snapshot, x: f32, y: f32, d: f32, c: &gdk::RGBA) {
    let side = d / std::f32::consts::SQRT_2;
    s.save();
    s.translate(&gtk::graphene::Point::new(x, y));
    s.rotate(45.0);
    rect(s, -side / 2.0, -side / 2.0, side, side, c);
    s.restore();
}

fn draw(ctx: &Ctx, h: &Hits, c: &Canvas, s: &gtk::Snapshot, w: f32, height: f32) {
    let fg = canvas::fg(c);
    let shade = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    let Some(v) = view(ctx) else { return };
    let time = ctx.s().project.time();
    let focus = c.has_focus().then(|| h.focus.get());
    let (first, last) = (h.tick(0.0).max(0.0), h.tick(w as f64));
    for (i, row) in v.rows.iter().enumerate() {
        let y = i as f32 * ROW_H;
        if focus == Some(i) {
            rect(s, 0.0, y, w, ROW_H, &shade(0.05));
        } else if i % 2 == 1 {
            rect(s, 0.0, y, w, ROW_H, &shade(0.025));
        }
        rect(s, 0.0, y + ROW_H - 1.0, w, 1.0, &shade(0.08));
        let g = v.step(row.pitch) as f64;
        if g * h.ppt.get() >= 4.0 {
            let mut t = (first / g).floor() * g;
            while t <= last.min(v.ticks as f64) {
                rect(s, h.x(t), y + 3.0, 1.0, ROW_H - 6.0, &shade(if v.grid(row.pitch) == 0 { 0.03 } else { 0.06 }));
                t += g;
            }
        }
    }
    draw_beats((h.t0.get(), h.ppt.get()), &time, &v.region, s, w, height, &fg);

    let base = track_color(v.color);
    let sel = h.selected.borrow().clone();
    let copying = matches!(h.drag.get(), Drag::Move { copy: true, .. });
    for n in &v.notes {
        let d = dragged(h, &v, n);
        let mut shapes = vec![(d, sel.contains(n))];
        if copying && d != *n {
            shapes.insert(0, (*n, false));
        }
        for (note, selected) in shapes {
            let Some(i) = v.row_of(note.pitch) else { continue };
            let (x, y) = (h.x(note.start as f64), i as f32 * ROW_H + ROW_H / 2.0);
            if selected {
                diamond(s, x, y, HIT_D + 4.0, &rgba(0xffffff, 1.0));
            }
            let a = 0.25 + 0.75 * note.velocity as f32 / 127.0;
            diamond(s, x, y, HIT_D, &gdk::RGBA::new(base.red(), base.green(), base.blue(), a));
        }
    }
    if let Drag::Marquee { x, y, .. } = h.drag.get() {
        let (dx, dy) = h.offset.get();
        let accent = canvas::accent(c);
        rect(s, x.min(x + dx) as f32, y.min(y + dy) as f32, dx.abs() as f32, dy.abs() as f32, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.15));
    }
    playhead(ctx, |t| h.x(t), &v.region, &time, s, height, &fg);
}

fn draw_velocity(ctx: &Ctx, h: &Hits, c: &Canvas, s: &gtk::Snapshot, w: f32, height: f32) {
    let fg = canvas::fg(c);
    rect(s, 0.0, 0.0, w, height, &rgba(0x000000, 0.15));
    let Some(v) = view(ctx) else { return };
    let time = ctx.s().project.time();
    draw_beats((h.t0.get(), h.ppt.get()), &time, &v.region, s, w, height, &fg);
    let Some(row) = v.rows.get(h.focus.get()) else { return };
    let (base, accent) = (track_color(v.color), canvas::accent(c));
    let sel = h.selected.borrow();
    for n in v.notes.iter().filter(|n| n.pitch == row.pitch) {
        let x = h.x(n.start as f64);
        let bh = (n.velocity as f32 / 127.0) * (height - 4.0);
        let col = if sel.contains(n) { accent } else { base };
        rect(s, x - 1.0, height - bh, 3.0, bh, &col);
        diamond(s, x + 0.5, height - bh, 7.0, &col);
    }
    if v.notes.iter().all(|n| n.pitch != row.pitch) {
        text(s, c, 8.0, 6.0, "No hits on this row", &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.5), Some(8.0));
    }
}

fn install_velocity_input(ctx: &Ctx, h: &Rc<Hits>, lane: &Canvas, redraw: &Rc<dyn Fn()>) {
    let set = {
        let (ctx, h, lane) = (ctx.clone(), h.clone(), lane.clone());
        move |x: f64, y: f64| {
            let Some(v) = view(&ctx) else { return };
            let Some(pitch) = v.rows.get(h.focus.get()).map(|r| r.pitch) else { return };
            let height = lane.height().max(8) as f64;
            let value = ((height - y) / (height - 4.0) * 127.0).round().clamp(1.0, 127.0) as u8;
            let near = |n: &Note| n.pitch == pitch && (h.x(n.start as f64) as f64 - x).abs() <= 5.0 && n.velocity != value;
            if !v.notes.iter().any(near) {
                return;
            }
            edit_live(&ctx, |notes| {
                for n in notes.iter_mut().filter(|n| near(n)) {
                    n.velocity = value;
                }
            });
        }
    };
    let g = gtk::GestureDrag::new();
    {
        let (ctx, h, set, redraw) = (ctx.clone(), h.clone(), set.clone(), redraw.clone());
        g.connect_drag_begin(move |_, x, y| {
            ctx.begin_gesture();
            h.velocity_drag.set(true);
            set(x, y);
            redraw();
        });
    }
    {
        let (set, redraw) = (set.clone(), redraw.clone());
        g.connect_drag_update(move |g, dx, dy| {
            if let Some((x, y)) = g.start_point() {
                set(x + dx, y + dy);
            }
            redraw();
        });
    }
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        g.connect_drag_end(move |_, _, _| {
            if h.velocity_drag.replace(false) {
                ctx.end_gesture("Change Velocity");
            }
        });
    }
    lane.add_controller(g);
}

fn install_grid_input(ctx: &Ctx, h: &Rc<Hits>, grid: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    g.set_button(0);
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        g.connect_drag_begin(move |g, x, y| {
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
            h.offset.set((0.0, 0.0));
            h.drag.set(Drag::None);
            let Some(v) = view(&ctx) else { return };
            let Some((i, row)) = v.rows.get((y / ROW_H as f64).floor().max(0.0) as usize).map(|r| ((y / ROW_H as f64) as usize, r.clone())) else { return };
            let mods = g.current_event_state();
            let (shift, alt, ctrl) =
                (mods.contains(gdk::ModifierType::SHIFT_MASK), mods.contains(gdk::ModifierType::ALT_MASK), mods.contains(gdk::ModifierType::CONTROL_MASK));
            let tick = h.tick(x).max(0.0);
            h.focus.set(i);
            if g.current_button() == gdk::BUTTON_SECONDARY {
                ctx.begin_gesture();
                h.drag.set(Drag::Erase { pitch: row.pitch, last: tick as u32 });
                erase(&ctx, &h, row.pitch, tick as u32, tick as u32);
                return;
            }
            match hit_at(&h, &v, x, y) {
                Some(n) if shift => {
                    let mut sel = h.selected.borrow_mut();
                    match sel.iter().position(|x| *x == n) {
                        Some(i) => {
                            sel.remove(i);
                        }
                        None => sel.push(n),
                    }
                }
                Some(n) => {
                    if !h.selected.borrow().contains(&n) {
                        *h.selected.borrow_mut() = vec![n];
                    }
                    h.drag.set(Drag::Move { grab: n, copy: alt });
                    audition(&ctx, n.pitch, n.velocity);
                }
                None if ctrl => {
                    if !shift {
                        h.selected.borrow_mut().clear();
                    }
                    h.drag.set(Drag::Marquee { x, y, add: shift });
                }
                None => {
                    // A click that only clears a selection adds nothing; dragging on paints.
                    let clearing = !std::mem::take(&mut *h.selected.borrow_mut()).is_empty();
                    let from = tick as u32;
                    h.drag.set(Drag::Paint { pitch: row.pitch, from, last: from, started: !clearing });
                    if !clearing && from < v.ticks {
                        ctx.begin_gesture();
                        let n = if v.grid(row.pitch) == 0 {
                            Note::new(from, v.step(row.pitch), row.pitch, h.insert_velocity.get())
                        } else {
                            let at = snap(tick, v.grid(row.pitch));
                            Note::new(at, v.step(row.pitch), row.pitch, h.insert_velocity.get())
                        };
                        if !v.notes.iter().any(|x| x.pitch == n.pitch && x.start == n.start) {
                            h.just_added.set(Some(n));
                            audition(&ctx, n.pitch, n.velocity);
                            edit_live(&ctx, |notes| notes.push(n));
                        }
                    }
                }
            }
        });
    }
    {
        let (ctx, h, redraw) = (ctx.clone(), h.clone(), redraw.clone());
        g.connect_drag_update(move |g, dx, dy| {
            h.offset.set((dx, dy));
            let Some((x, _)) = g.start_point() else { return };
            let tick = h.tick(x + dx).max(0.0) as u32;
            match h.drag.get() {
                Drag::Paint { pitch, from, last, started } => {
                    let moved = dx.abs() > 2.0;
                    if !started && !moved {
                        return;
                    }
                    if !started {
                        ctx.begin_gesture();
                    }
                    let Some(v) = view(&ctx) else { return };
                    let (a, b) = if started { (last, tick) } else { (from, tick) };
                    let new = midi_tools::paint(&v.notes, pitch, a, b, v.step(pitch), v.step(pitch), h.insert_velocity.get(), v.ticks);
                    if !new.is_empty() {
                        edit_live(&ctx, |notes| notes.extend(new));
                    }
                    h.drag.set(Drag::Paint { pitch, from, last: tick, started: true });
                }
                Drag::Erase { pitch, last } => {
                    erase(&ctx, &h, pitch, last, tick);
                    h.drag.set(Drag::Erase { pitch, last: tick });
                }
                _ => {}
            }
            redraw();
        });
    }
    {
        let (ctx, h, redraw) = (ctx.clone(), h.clone(), redraw.clone());
        g.connect_drag_end(move |g, dx, dy| {
            let moved = dx.abs() > 2.0 || dy.abs() > 2.0;
            let d = h.drag.get();
            let Some(v) = view(&ctx) else { return };
            match d {
                Drag::Paint { started: true, .. } => ctx.end_gesture("Add Hits"),
                Drag::Erase { .. } => ctx.end_gesture("Erase Hits"),
                Drag::Move { grab, copy } if moved => {
                    let old = h.selected.borrow().clone();
                    let old = if old.contains(&grab) { old } else { vec![grab] };
                    let new: Vec<Note> = old.iter().map(|n| dragged(&h, &v, n)).collect();
                    edit_hits(&ctx, if copy { "Copy Hits" } else { "Move Hits" }, |notes| {
                        if copy {
                            notes.extend(new.iter().copied());
                        } else {
                            midi_tools::replace(notes, &old, &new);
                        }
                    });
                    *h.selected.borrow_mut() = new;
                    if let Some(i) = h.selected.borrow().first().and_then(|n| v.row_of(n.pitch)) {
                        h.focus.set(i);
                    }
                }
                Drag::Move { grab, .. } if !g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) => {
                    *h.selected.borrow_mut() = vec![grab];
                }
                Drag::Marquee { x, y, add } if moved => {
                    let (t0, t1) = (h.tick(x.min(x + dx)), h.tick(x.max(x + dx)));
                    let row = |y: f64| (y / ROW_H as f64).floor().max(0.0) as usize;
                    let rows = row(y.min(y + dy))..=row(y.max(y + dy));
                    let mut sel = if add { h.selected.borrow().clone() } else { Vec::new() };
                    for n in v.notes.iter().filter(|n| (t0..=t1).contains(&(n.start as f64)) && v.row_of(n.pitch).is_some_and(|i| rows.contains(&i))) {
                        if !sel.contains(n) {
                            sel.push(*n);
                        }
                    }
                    *h.selected.borrow_mut() = sel;
                }
                _ => {}
            }
            h.drag.set(Drag::None);
            h.offset.set((0.0, 0.0));
            redraw();
        });
    }
    grid.add_controller(g);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, h) = (ctx.clone(), h.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n == 1 {
                return;
            }
            let Some(v) = view(&ctx) else { return };
            if let Some(note) = hit_at(&h, &v, x, y)
                && h.just_added.take() != Some(note)
            {
                h.selected.borrow_mut().retain(|n| *n != note);
                edit_hits(&ctx, "Delete Hit", |notes| notes.retain(|n| *n != note));
            }
        });
    }
    {
        let h = h.clone();
        dbl.connect_released(move |_, n, _, _| {
            if n >= 2 {
                h.just_added.set(None);
            }
        });
    }
    grid.add_controller(dbl);

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, h, redraw) = (ctx.clone(), h.clone(), redraw.clone());
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let has_sel = !h.selected.borrow().is_empty();
            let handled = match key {
                gdk::Key::Delete | gdk::Key::BackSpace => {
                    let sel = std::mem::take(&mut *h.selected.borrow_mut());
                    if !sel.is_empty() {
                        edit_hits(&ctx, "Delete Hits", |notes| notes.retain(|n| !sel.contains(n)));
                    }
                    true
                }
                gdk::Key::Up | gdk::Key::Down if has_sel => {
                    let by: i64 = if key == gdk::Key::Up { -1 } else { 1 };
                    apply(&ctx, &h, "Move Hits", |v, n| {
                        n.iter()
                            .map(|n| match v.row_of(n.pitch) {
                                Some(i) => Note { pitch: v.rows[(i as i64 + by).clamp(0, v.rows.len() as i64 - 1) as usize].pitch, ..*n },
                                None => *n,
                            })
                            .collect()
                    });
                    true
                }
                gdk::Key::Left | gdk::Key::Right if has_sel => {
                    let right = key == gdk::Key::Right;
                    apply(&ctx, &h, "Nudge Hits", |v, n| {
                        let min = n.iter().map(|n| (n.start, v.step(n.pitch))).filter(|(s, g)| s < g).count();
                        if !right && min > 0 {
                            return n.to_vec();
                        }
                        n.iter()
                            .map(|n| {
                                let g = v.step(n.pitch);
                                Note { start: if right { n.start + g } else { n.start - g }, ..*n }
                            })
                            .collect()
                    });
                    true
                }
                gdk::Key::q if !ctrl => {
                    quantize(&ctx, &h, false);
                    true
                }
                gdk::Key::Q if !ctrl => {
                    quantize(&ctx, &h, true);
                    true
                }
                gdk::Key::Escape => {
                    h.selected.borrow_mut().clear();
                    true
                }
                gdk::Key::a if ctrl => {
                    if let Some(v) = view(&ctx) {
                        *h.selected.borrow_mut() = v.notes.iter().filter(|n| v.row_of(n.pitch).is_some()).copied().collect();
                    }
                    true
                }
                gdk::Key::c | gdk::Key::x if ctrl => {
                    let sel = h.selected.borrow().clone();
                    *h.clipboard.borrow_mut() = sel.clone();
                    if key == gdk::Key::x && !sel.is_empty() {
                        h.selected.borrow_mut().clear();
                        edit_hits(&ctx, "Cut Hits", |notes| notes.retain(|n| !sel.contains(n)));
                    }
                    true
                }
                gdk::Key::v if ctrl => {
                    paste(&ctx, &h);
                    true
                }
                _ => false,
            };
            if handled {
                redraw();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    grid.add_controller(keys);
}

fn erase(ctx: &Ctx, h: &Hits, pitch: u8, from: u32, to: u32) {
    let Some(v) = view(ctx) else { return };
    let gone = midi_tools::erase(&v.notes, pitch, from, to, (REACH / h.ppt.get()) as u32);
    if !gone.is_empty() {
        h.selected.borrow_mut().retain(|n| !gone.contains(n));
        edit_live(ctx, |notes| notes.retain(|n| !gone.contains(n)));
    }
}

/// Pastes at the playhead when it's inside the region, else straight after the copied hits,
/// on the editor grid.
fn paste(ctx: &Ctx, h: &Hits) {
    let clip = h.clipboard.borrow().clone();
    let (Some(v), Some(first)) = (view(ctx), clip.iter().map(|n| n.start).min()) else { return };
    let time = ctx.s().project.time();
    let pos = ctx.position();
    let g = v.dq.grid.max(1);
    let at = if pos >= v.region.start && pos < v.region.start + v.region.length {
        time.ticks_between(v.region.start, pos) as u32 / g * g
    } else {
        let end = clip.iter().map(|n| n.start).max().unwrap_or(first) + 1;
        end.div_ceil(g) * g
    };
    let new: Vec<Note> = clip.iter().map(|n| Note { start: n.start - first + at, ..*n }).filter(|n| n.start < v.ticks).collect();
    edit_hits(ctx, "Paste Hits", |notes| {
        notes.retain(|n| !new.iter().any(|x| x.start == n.start && x.pitch == n.pitch));
        notes.extend(new.iter().copied());
    });
    *h.selected.borrow_mut() = new;
}

struct HitRows {
    h: Rc<Hits>,
}

impl RowSource for HitRows {
    fn rows(&self, ctx: &Ctx) -> Option<Vec<DrumRow>> {
        view(ctx).map(|v| v.rows)
    }

    fn used(&self, ctx: &Ctx) -> Vec<u8> {
        view(ctx).map(|v| v.used()).unwrap_or_default()
    }

    fn badge(&self, _: &Ctx, row: &DrumRow) -> (String, bool) {
        match row.grid {
            None => ("Editor".into(), false),
            Some(0) => ("Off".into(), true),
            Some(g) => (grid_name(g), true),
        }
    }

    fn badge_tip(&self) -> &'static str {
        "The grid hits on this row snap to"
    }

    fn badge_clicked(&self, ctx: &Ctx, anchor: &Canvas, x: f64, y: f64, row: &DrumRow) {
        let m = Menu::at(anchor, x, y);
        let used = self.used(ctx);
        let choices = [(None, "Editor".to_string()), (Some(0), "Off".to_string())].into_iter().chain(GRIDS.iter().map(|g| (Some(g.0), g.1.to_string())));
        for (grid, label) in choices {
            let (ctx, used, pitch) = (ctx.clone(), used.clone(), row.pitch);
            let label = if grid == row.grid { format!("{label} ✓") } else { label };
            m.item(&label, true, move || {
                edit_map(&ctx, "Set Row Grid", used.clone(), |map| {
                    if let Some(r) = map.iter_mut().find(|r| r.pitch == pitch) {
                        r.grid = grid;
                    }
                });
            });
        }
        m.show();
    }

    fn extend_menu(&self, ctx: &Ctx, m: &Menu, _: &Canvas, _: f64, _: f64, row: &DrumRow) {
        let pitch = row.pitch;
        let has = view(ctx).is_some_and(|v| v.notes.iter().any(|n| n.pitch == pitch));
        {
            let (ctx, h) = (ctx.clone(), self.h.clone());
            m.item("Select Hits in Row", has, move || {
                if let Some(v) = view(&ctx) {
                    *h.selected.borrow_mut() = v.notes.iter().filter(|n| n.pitch == pitch).copied().collect();
                    ctx.emit(Change::Selection);
                }
            });
        }
        let (ctx, h) = (ctx.clone(), self.h.clone());
        m.item("Delete Hits in Row", has, move || {
            h.selected.borrow_mut().retain(|n| n.pitch != pitch);
            edit_hits(&ctx, "Delete Hits in Row", |notes| notes.retain(|n| n.pitch != pitch));
        });
    }

    fn focus_row(&self) -> usize {
        self.h.focus.get()
    }

    fn set_focus_row(&self, i: usize) {
        self.h.focus.set(i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_floors_to_the_row_grid_and_off_is_free() {
        assert_eq!(snap(250.0, 240), 240);
        assert_eq!(snap(479.9, 240), 240);
        assert_eq!(snap(250.0, 0), 250);
        assert_eq!(snap(-3.0, 240), 0);
        assert_eq!((snap_round(370.0, 240), snap_round(350.0, 240), snap_round(350.4, 0)), (480, 240, 350));
    }

    #[test]
    fn grids_are_named_by_note_value() {
        assert_eq!((grid_name(PPQ / 3), grid_name(PPQ / 4), grid_name(7)), ("1/8T".into(), "1/16".into(), "7 ticks".into()));
    }
}
