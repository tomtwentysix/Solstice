use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::grooves;
use crate::studio::{Change, Ctx};
use crate::timeline::{grid_ticks, snap_options};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_engine::Command;
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const ROW: f32 = 12.0;
const HIT_ROW: f32 = 22.0;
const KEY_W: i32 = 56;
const EDGE: f64 = 5.0;
const LANE_H: i32 = 72;

/// Controllers offered in the controller lane, as (number, name).
const CONTROLLERS: [(u8, &str); 7] = [
    (1, "Modulation"),
    (PITCH_BEND, "Pitch Bend"),
    (64, "Sustain"),
    (7, "Volume"),
    (10, "Pan"),
    (11, "Expression"),
    (2, "Breath"),
];

/// The rows of the piano roll: one per key, or in the Hits view one per drum sound, then a
/// last row for notes of any other pitch.
#[derive(Clone, Debug, PartialEq)]
enum Lanes {
    Keys,
    Hits(Vec<DrumRow>),
}

impl Lanes {
    fn row_h(&self) -> f32 {
        match self {
            Lanes::Keys => ROW,
            Lanes::Hits(_) => HIT_ROW,
        }
    }

    fn count(&self) -> usize {
        match self {
            Lanes::Keys => 128,
            Lanes::Hits(rows) => rows.len() + 1,
        }
    }

    fn height(&self) -> f32 {
        self.count() as f32 * self.row_h()
    }

    fn index(&self, pitch: u8) -> usize {
        match self {
            Lanes::Keys => 127 - pitch.min(127) as usize,
            Lanes::Hits(rows) => rows.iter().position(|r| r.pitch == pitch).unwrap_or(rows.len()),
        }
    }

    /// The pitch of row `index`; the Hits view's last row has none.
    fn pitch(&self, index: usize) -> Option<u8> {
        match self {
            Lanes::Keys => (index < 128).then(|| 127 - index as u8),
            Lanes::Hits(rows) => rows.get(index).map(|r| r.pitch),
        }
    }

    fn y_of(&self, pitch: u8) -> f32 {
        self.index(pitch) as f32 * self.row_h()
    }

    fn index_at(&self, y: f64) -> usize {
        (y / self.row_h() as f64).floor().clamp(0.0, (self.count() - 1) as f64) as usize
    }

    fn pitch_at(&self, y: f64) -> Option<u8> {
        self.pitch(self.index_at(y))
    }

    /// The pitch of a note moved `rows` rows down. In the Hits view a moved note lands on a
    /// drum row, never on the last one.
    fn moved(&self, pitch: u8, rows: i64) -> u8 {
        match self {
            Lanes::Keys => (pitch as i64 - rows).clamp(0, 127) as u8,
            Lanes::Hits(r) if rows == 0 || r.is_empty() => pitch,
            Lanes::Hits(r) => r[(self.index(pitch) as i64 + rows).clamp(0, r.len() as i64 - 1) as usize].pitch,
        }
    }

    fn is_hits(&self) -> bool {
        matches!(self, Lanes::Hits(_))
    }
}

/// Region `id`'s choice of Hits view (true) or keys, when it has made one.
type HitsPref = Rc<Cell<Option<(Id, bool)>>>;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Move { grab: Note, copy: bool },
    Resize { grab: Note },
    Marquee { x: f64, y: f64, add: bool },
}

struct Roll {
    /// Pixels per tick and the first visible tick.
    ppt: Cell<f64>,
    t0: Cell<f64>,
    snap: Cell<u64>,
    selected: RefCell<Vec<Note>>,
    drag: Cell<Drag>,
    offset: Cell<(f64, f64)>,
    hadj: gtk::Adjustment,
    clipboard: RefCell<Vec<Note>>,
    controller: Cell<u8>,
    strength: Cell<f32>,
    swing: Cell<f32>,
    /// Quantize to this groove instead of the grid.
    groove: RefCell<Option<Groove>>,
    groove_velocity: Cell<f32>,
    /// Velocity drag in progress: the notes being changed and the change so far.
    velocity_edit: RefCell<Option<(Vec<Note>, i32)>>,
    /// Controller points being drawn, not yet committed.
    draft: RefCell<Vec<Controller>>,
    /// Step Input: played notes go in at the playhead, which then moves on by a grid step.
    step_input: Cell<bool>,
    step_held: Cell<u32>,
    /// Set while the toolbar is updated from the selection, so it doesn't edit the notes back.
    syncing: Cell<bool>,
    /// The first click of a double-click added this note; the second leaves it be.
    just_added: Cell<Option<Note>>,
    lanes: RefCell<Lanes>,
}

/// The editor pane: a piano roll for MIDI regions and a waveform view for audio regions.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let stack = gtk::Stack::new();
    // Region shortcuts in the window leave keys alone while focus is in here.
    stack.set_widget_name("editor-pane");
    let empty = adw::StatusPage::builder()
        .icon_name("document-edit-symbolic")
        .title("No Region Selected")
        .description("Select a region in the tracks area to edit it")
        .build();
    empty.add_css_class("compact");
    stack.add_named(&empty, Some("empty"));
    // Drum Kit regions open in the step sequencer, others in the piano roll; buttons in each
    // MIDI view switch between it, the other one and the Event List.
    let prefer_steps: Rc<Cell<Option<&'static str>>> = Rc::default();
    let auto: Rc<Cell<Option<(Id, bool)>>> = Rc::default();
    let hits: HitsPref = Rc::default();
    let to_takes = gtk::Button::with_label("Takes");
    let update: Rc<dyn Fn()> = {
        let (ctx, stack, prefer, to_takes, auto) = (ctx.clone(), stack.clone(), prefer_steps.clone(), to_takes.clone(), auto.clone());
        Rc::new(move || {
            let (page, midi_folder) = {
                let s = ctx.s();
                match s.selected_region.and_then(|id| Some((id, s.project.find_region(id)?))) {
                    Some((id, (ti, r @ Region { content: Content::Midi(m), .. }))) => {
                        let fits = remembered(&auto, id, || {
                            crate::drum_editor::is_drum_track(&s.project.tracks[ti]) && crate::drum_editor::fits_steps(&s.project.time(), r)
                        });
                        (midi_page(prefer.get(), m.is_folder(), m.pattern.is_some() || fits), m.is_folder())
                    }
                    Some((_, (_, Region { content: Content::Audio(a), .. }))) if a.is_folder() => ("takes", false),
                    Some(_) => ("audio", false),
                    None => ("empty", false),
                }
            };
            to_takes.set_visible(midi_folder);
            stack.set_visible_child_name(page);
        })
    };
    let switch = |page: &'static str| {
        let (prefer, update) = (prefer_steps.clone(), update.clone());
        Rc::new(move || {
            prefer.set(Some(page));
            update();
        }) as Rc<dyn Fn()>
    };
    let roll = piano_roll(ctx, hits.clone());
    if let Some(bar) = roll.first_child().and_downcast::<gtk::Box>() {
        for (label, page) in [("Step Sequencer", "steps"), ("Event List", "events")] {
            let b = gtk::Button::with_label(label);
            let go = switch(page);
            b.connect_clicked(move |_| go());
            bar.append(&b);
        }
        to_takes.set_tooltip_text(Some("Show the takes of this take folder"));
        let go = switch("takes");
        to_takes.connect_clicked(move |_| go());
        bar.append(&to_takes);
    }
    stack.add_named(&roll, Some("midi"));
    let to_roll: Rc<dyn Fn(bool)> = {
        let (ctx, hits, go) = (ctx.clone(), hits.clone(), switch("midi"));
        Rc::new(move |h| {
            if let Some(id) = ctx.s().selected_region {
                hits.set(Some((id, h)));
            }
            go();
        })
    };
    stack.add_named(&crate::drum_editor::build(ctx, to_roll), Some("steps"));
    stack.add_named(&crate::event_list::build(ctx, switch("midi")), Some("events"));
    stack.add_named(&crate::audio_editor::build(ctx), Some("audio"));
    stack.add_named(&crate::takes_editor::build(ctx, switch("midi")), Some("takes"));
    update();
    {
        let (ctx2, prefer, auto) = (ctx.clone(), prefer_steps.clone(), auto.clone());
        let last = Cell::new(None::<Id>);
        ctx.on(move |c| {
            if matches!(c, Change::LiveNote) {
                return;
            }
            // A different region goes back to the automatic choice, made afresh.
            let cur = ctx2.s().selected_region;
            if cur != last.get() {
                prefer.set(None);
                auto.set(None);
                last.set(cur);
            }
            update();
        });
    }
    stack.upcast()
}

fn region(ctx: &Ctx) -> Option<(Region, u8)> {
    let s = ctx.s();
    let (ti, r) = s.selected_region.and_then(|id| s.project.find_region(id))?;
    Some((r.clone(), s.project.tracks[ti].color))
}

/// The page a MIDI region opens on: the one the user picked, else its takes for a take
/// folder, else the step sequencer when `steps`, else the piano roll.
fn midi_page(prefer: Option<&'static str>, folder: bool, steps: bool) -> &'static str {
    match prefer {
        Some("takes") if !folder => {}
        Some(p) => return p,
        None => {}
    }
    if folder {
        "takes"
    } else if steps {
        "steps"
    } else {
        "midi"
    }
}

/// `f()` for region `id`, worked out once and kept while `id` stays selected, so an edit
/// does not move the region to another page under the user.
fn remembered(cache: &Cell<Option<(Id, bool)>>, id: Id, f: impl FnOnce() -> bool) -> bool {
    match cache.get() {
        Some((r, v)) if r == id => v,
        _ => {
            let v = f();
            cache.set(Some((id, v)));
            v
        }
    }
}

pub(crate) fn midi(ctx: &Ctx) -> Option<(Region, MidiContent, u8)> {
    let (r, color) = region(ctx)?;
    let Content::Midi(m) = &r.content else { return None };
    let m = m.clone();
    Some((r, m, color))
}

fn lanes_for(ctx: &Ctx, pref: &HitsPref) -> Lanes {
    let s = ctx.s();
    let Some((id, (ti, Region { content: Content::Midi(m), .. }))) = s.selected_region.and_then(|id| Some((id, s.project.find_region(id)?))) else {
        return Lanes::Keys;
    };
    let t = &s.project.tracks[ti];
    let hits = match pref.get() {
        Some((r, h)) if r == id => h,
        _ => crate::drum_editor::is_drum_track(t),
    };
    if hits { Lanes::Hits(crate::drum_editor::hit_rows(t, &m.notes)) } else { Lanes::Keys }
}

/// A hit in the Hits view: a diamond centred on (x, y), `d` across.
fn diamond(s: &gtk::Snapshot, x: f32, y: f32, d: f32, c: &gdk::RGBA) {
    let side = d / std::f32::consts::SQRT_2;
    s.save();
    s.translate(&gtk::graphene::Point::new(x, y));
    s.rotate(45.0);
    rect(s, -side / 2.0, -side / 2.0, side, side, c);
    s.restore();
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

pub(crate) fn edit_midi(ctx: &Ctx, label: &str, f: impl FnOnce(&mut MidiContent)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
            f(m);
            m.notes.sort_by_key(|n| (n.start, n.pitch));
            m.controllers.sort_by_key(|c| c.tick);
        }
    });
}

fn edit_notes(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Vec<Note>)) {
    edit_midi(ctx, label, |m| f(&mut m.notes));
}

/// Runs a note function on the selection (or on every note when nothing is selected) and keeps
/// the selection on the results.
fn apply(ctx: &Ctx, roll: &Roll, label: &str, f: impl FnOnce(&[Note]) -> Vec<Note>) {
    let Some((_, m, _)) = midi(ctx) else { return };
    let old = {
        let sel = roll.selected.borrow();
        if sel.is_empty() { m.notes.clone() } else { sel.clone() }
    };
    let new = f(&old);
    edit_notes(ctx, label, |notes| {
        midi_tools::replace(notes, &old, &new);
    });
    *roll.selected.borrow_mut() = new;
}

fn grid_of(roll: &Roll, time: &TimeMap) -> u32 {
    grid_ticks(roll.snap.get(), time).max(PPQ as u64 / 16) as u32
}

fn piano_roll(ctx: &Ctx, hits_pref: HitsPref) -> gtk::Widget {
    let roll = Rc::new(Roll {
        ppt: Cell::new(0.12),
        t0: Cell::new(0.0),
        snap: Cell::new(PPQ as u64 / 4),
        selected: RefCell::default(),
        drag: Cell::new(Drag::None),
        offset: Cell::new((0.0, 0.0)),
        hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
        clipboard: RefCell::default(),
        controller: Cell::new(1),
        strength: Cell::new(1.0),
        swing: Cell::new(0.0),
        groove: RefCell::default(),
        groove_velocity: Cell::new(0.0),
        velocity_edit: RefCell::default(),
        draft: RefCell::default(),
        step_input: Cell::new(false),
        step_held: Cell::new(0),
        syncing: Cell::new(false),
        just_added: Cell::new(None),
        lanes: RefCell::new(Lanes::Keys),
    });

    let keys = {
        let roll = roll.clone();
        Canvas::new(move |c, s, w, _| match &*roll.lanes.borrow() {
            Lanes::Keys => {
                for pitch in 0..128u8 {
                    let y = Lanes::Keys.y_of(pitch);
                    let col = if is_black(pitch) { rgba(0x241f31, 1.0) } else { rgba(0xf6f5f4, 1.0) };
                    rect(s, 0.0, y, w, ROW - 1.0, &col);
                    if pitch % 12 == 0 {
                        text(s, c, 4.0, y - 1.0, &format!("C{}", pitch as i32 / 12 - 1), &rgba(0x000000, 0.8), Some(7.0));
                    }
                }
            }
            Lanes::Hits(rows) => {
                let fg = canvas::fg(c);
                let names = rows.iter().map(|r| r.name.as_str()).chain(["Other"]);
                for (i, name) in names.enumerate() {
                    let y = i as f32 * HIT_ROW;
                    rect(s, 0.0, y + HIT_ROW - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
                    s.push_clip(&gtk::graphene::Rect::new(0.0, y, w - 2.0, HIT_ROW));
                    text(s, c, 4.0, y + 4.0, name, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), if i < rows.len() { 0.85 } else { 0.5 }), Some(7.0));
                    s.pop();
                }
            }
        })
    };
    keys.set_size_request(KEY_W, Lanes::Keys.height() as i32);
    {
        let ctx = ctx.clone();
        let g = gtk::GestureClick::new();
        let held = Rc::new(Cell::new(None::<u8>));
        let h2 = held.clone();
        let ctx2 = ctx.clone();
        let roll = roll.clone();
        g.connect_pressed(move |_, _, _, y| {
            let Some(pitch) = roll.lanes.borrow().pitch_at(y) else { return };
            h2.set(Some(pitch));
            ctx2.send(Command::Midi([0x90, pitch, 100]));
        });
        g.connect_released(move |_, _, _, _| {
            if let Some(p) = held.take() {
                ctx.send(Command::Midi([0x80, p, 0]));
            }
        });
        keys.add_controller(g);
    }

    let grid = {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        Canvas::new(move |c, s, w, h| draw_roll(&ctx, &roll, c, s, w, h))
    };
    grid.set_hexpand(true);
    grid.set_size_request(-1, Lanes::Keys.height() as i32);
    grid.set_focusable(true);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&keys);
    body.append(&grid);
    let vscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&body).build();
    // Start around middle C.
    vscroll.vadjustment().set_value(Lanes::Keys.y_of(84) as f64);

    let velocity = {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        Canvas::new(move |c, s, w, h| draw_velocity(&ctx, &roll, c, s, w, h))
    };
    let controllers = {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        Canvas::new(move |c, s, w, h| draw_controllers(&ctx, &roll, c, s, w, h))
    };
    let lane = |title: &str, canvas: &Canvas| {
        let r = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let l = gtk::Label::new(Some(title));
        l.add_css_class("caption");
        l.add_css_class("dim-label");
        l.set_size_request(KEY_W, -1);
        l.set_wrap(true);
        canvas.set_hexpand(true);
        canvas.set_size_request(-1, LANE_H);
        r.append(&l);
        r.append(canvas);
        r
    };

    let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj));
    hbar.set_margin_start(KEY_W);
    let redraw_all: Rc<dyn Fn()> = {
        let (g, v, c) = (grid.clone(), velocity.clone(), controllers.clone());
        Rc::new(move || {
            g.queue_draw();
            v.queue_draw();
            c.queue_draw();
        })
    };
    {
        let (roll, redraw) = (roll.clone(), redraw_all.clone());
        roll.hadj.clone().connect_value_changed(move |a| {
            roll.t0.set(a.value());
            redraw();
        });
    }

    let (bar, title, vel, hits) = toolbar(ctx, &roll, &redraw_all);

    install_roll_input(ctx, &roll, &grid, &redraw_all);
    install_velocity_input(ctx, &roll, &velocity, &redraw_all);
    install_controller_input(ctx, &roll, &controllers, &redraw_all);

    let refresh = {
        let (ctx, roll, grid, title, vel, redraw) = (ctx.clone(), roll.clone(), grid.clone(), title.clone(), vel.clone(), redraw_all.clone());
        let (keys, vscroll, hits, pref) = (keys.clone(), vscroll.clone(), hits.clone(), hits_pref.clone());
        move || {
            let lanes = lanes_for(&ctx, &pref);
            if lanes != *roll.lanes.borrow() {
                let switched = lanes.is_hits() != roll.lanes.borrow().is_hits();
                keys.set_size_request(KEY_W, lanes.height() as i32);
                grid.set_size_request(-1, lanes.height() as i32);
                if switched {
                    let y = if lanes.is_hits() { 0.0 } else { Lanes::Keys.y_of(84) as f64 };
                    vscroll.vadjustment().set_value(y);
                }
                roll.syncing.set(true);
                hits.set_active(lanes.is_hits());
                roll.syncing.set(false);
                *roll.lanes.borrow_mut() = lanes;
                keys.queue_draw();
            }
            if let Some((r, _)) = region(&ctx) {
                title.set_text(&r.name);
                let time = ctx.s().project.time();
                let ticks = time.ticks_between(r.start, r.start + r.length) as f64;
                let page = grid.width().max(1) as f64 / roll.ppt.get();
                roll.hadj.configure(roll.t0.get(), 0.0, ticks.max(page) + time.ticks_per_bar() as f64, page / 10.0, page, page);
            }
            match roll.selected.borrow().first() {
                Some(n) => {
                    vel.set_sensitive(true);
                    roll.syncing.set(true);
                    vel.set_value(n.velocity as f64);
                    roll.syncing.set(false);
                }
                None => vel.set_sensitive(false),
            }
            redraw();
        }
    };
    let refresh = Rc::new(refresh);
    refresh();
    {
        let (ctx, roll, refresh) = (ctx.clone(), roll.clone(), refresh.clone());
        hits.connect_toggled(move |b| {
            if roll.syncing.get() {
                return;
            }
            if let Some(id) = ctx.s().selected_region {
                hits_pref.set(Some((id, b.is_active())));
            }
            refresh();
        });
    }
    {
        let (ctx2, roll) = (ctx.clone(), roll.clone());
        ctx.on(move |c| {
            if matches!(c, Change::LiveNote) && roll.step_input.get() {
                step_input(&ctx2, &roll);
            }
        });
    }
    {
        let refresh = refresh.clone();
        let roll = roll.clone();
        let last = Cell::new(None::<Id>);
        let ctx2 = ctx.clone();
        ctx.on(move |c| {
            if matches!(c, Change::LiveNote) {
                return;
            }
            let cur = ctx2.s().selected_region;
            if cur != last.get() {
                roll.selected.borrow_mut().clear();
                roll.t0.set(0.0);
            } else if let Some((_, m, _)) = midi(&ctx2) {
                // Undo can remove notes the selection points at.
                roll.selected.borrow_mut().retain(|n| m.notes.contains(n));
            }
            last.set(cur);
            refresh();
        });
    }
    {
        let (ctx, redraw) = (ctx.clone(), redraw_all.clone());
        grid.add_tick_callback(move |_, _| {
            if ctx.playing() {
                redraw();
            }
            glib::ControlFlow::Continue
        });
    }
    {
        let refresh = refresh.clone();
        grid.connect_map(move |_| refresh());
    }
    let notify = gtk::EventControllerLegacy::new();
    notify.connect_event(move |_, e| {
        if e.event_type() == gdk::EventType::ButtonRelease {
            refresh();
        }
        glib::Propagation::Proceed
    });
    grid.add_controller(notify);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Sideways scrolling anywhere in the piano roll, keyboard included.
    let tilt = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    {
        let (roll, redraw) = (roll.clone(), redraw_all.clone());
        tilt.connect_scroll(move |_, dx, _| {
            if dx == 0.0 {
                return glib::Propagation::Proceed;
            }
            roll.hadj.set_value(roll.hadj.value() + dx * 40.0 / roll.ppt.get());
            redraw();
            glib::Propagation::Stop
        });
    }
    root.add_controller(tilt);
    root.append(&bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&vscroll);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&lane("Velocity", &velocity));
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&lane("Control", &controllers));
    root.append(&hbar);
    root.upcast()
}

fn toolbar(ctx: &Ctx, roll: &Rc<Roll>, redraw: &Rc<dyn Fn()>) -> (gtk::Box, gtk::Label, gtk::SpinButton, gtk::ToggleButton) {
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

    let vel = gtk::SpinButton::with_range(1.0, 127.0, 1.0);
    vel.set_tooltip_text(Some("Velocity of the selected notes"));
    vel.set_sensitive(false);
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        vel.connect_value_changed(move |v| {
            let value = v.value() as u8;
            if roll.syncing.get() || roll.selected.borrow().iter().all(|n| n.velocity == value) {
                return;
            }
            apply(&ctx, &roll, "Change Velocity", |notes| notes.iter().map(|n| Note { velocity: value, ..*n }).collect());
        });
    }
    bar.append(&gtk::Label::new(Some("Velocity")));
    bar.append(&vel);

    let names: Vec<&str> = snap_options().iter().map(|o| o.0).collect();
    let snap = gtk::DropDown::from_strings(&names);
    snap.set_selected(3);
    snap.set_tooltip_text(Some("Grid"));
    {
        let roll = roll.clone();
        snap.connect_selected_notify(move |d| roll.snap.set(snap_options()[d.selected() as usize].1));
    }
    bar.append(&snap);

    // Quantise, with strength and swing settings in its menu.
    let q = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    q.add_css_class("linked");
    let quantize = gtk::Button::with_label("Quantize");
    quantize.set_tooltip_text(Some("Quantize the selected notes, or all notes (Q)"));
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        quantize.connect_clicked(move |_| quantize_selection(&ctx, &roll));
    }
    let settings = gtk::MenuButton::new();
    let pop = gtk::Popover::new();
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(8).build();
    let names_model = gtk::StringList::new(&[]);
    let pick = gtk::DropDown::builder().model(&names_model).build();
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.add_css_class("flat");
    delete.set_tooltip_text(Some("Delete Groove Template"));
    let groove_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    groove_row.append(&pick);
    groove_row.append(&delete);
    grid.attach(&gtk::Label::new(Some("Groove")), 0, 0, 1, 1);
    grid.attach(&groove_row, 1, 0, 1, 1);
    let mut spins = Vec::new();
    for (row, (label, cell, default)) in [("Strength %", 0, 100.0), ("Swing %", 1, 0.0), ("Velocity %", 2, 0.0)].into_iter().enumerate() {
        let spin = gtk::SpinButton::with_range(0.0, 100.0, 5.0);
        spin.set_value(default);
        let roll = roll.clone();
        spin.connect_value_changed(move |s| {
            let v = s.value() as f32 / 100.0;
            match cell {
                0 => roll.strength.set(v),
                1 => roll.swing.set(v),
                _ => roll.groove_velocity.set(v),
            }
        });
        let name = gtk::Label::new(Some(label));
        grid.attach(&name, 0, row as i32 + 1, 1, 1);
        grid.attach(&spin, 1, row as i32 + 1, 1, 1);
        spins.push((name, spin));
    }
    // Index 0 of the drop-down is the grid; the rest index `list`, factory grooves first.
    let list: Rc<RefCell<Vec<Groove>>> = Rc::default();
    let factory_len = groove::factory().len();
    let show = {
        let (delete, spins) = (delete.clone(), spins.clone());
        move |i: u32| {
            let on = i > 0;
            spins[1].0.set_sensitive(!on);
            spins[1].1.set_sensitive(!on);
            spins[2].0.set_visible(on);
            spins[2].1.set_visible(on);
            delete.set_visible(i as usize > factory_len);
        }
    };
    show(0);
    {
        let (roll, list, show) = (roll.clone(), list.clone(), show.clone());
        pick.connect_selected_notify(move |d| {
            let i = d.selected();
            *roll.groove.borrow_mut() = i.checked_sub(1).and_then(|i| list.borrow().get(i as usize).cloned());
            show(i);
        });
    }
    let refresh = {
        let (roll, list, names_model, pick) = (roll.clone(), list.clone(), names_model.clone(), pick.clone());
        Rc::new(move || {
            let chosen = roll.groove.borrow().clone();
            *list.borrow_mut() = grooves::all();
            let names: Vec<String> = std::iter::once("Grid".to_string()).chain(list.borrow().iter().map(|g| g.name.clone())).collect();
            let names: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
            names_model.splice(0, names_model.n_items(), &names);
            let at = chosen
                .and_then(|c| {
                    let list = list.borrow();
                    list.iter().position(|g| *g == c).or_else(|| list.iter().position(|g| g.name == c.name))
                })
                .map_or(0, |i| i as u32 + 1);
            pick.set_selected(at);
            pick.notify("selected");
        })
    };
    {
        let refresh = refresh.clone();
        pop.connect_show(move |_| refresh());
    }
    {
        let (ctx, list, pick, refresh) = (ctx.clone(), list.clone(), pick.clone(), refresh.clone());
        delete.connect_clicked(move |_| {
            let Some(g) = pick.selected().checked_sub(1).and_then(|i| list.borrow().get(i as usize).cloned()) else { return };
            match grooves::delete(&g.name) {
                Ok(()) => ctx.message(&format!("Deleted groove template “{}”", g.name)),
                Err(e) => ctx.message(&format!("Could not delete the groove template: {e}")),
            }
            refresh();
        });
    }
    pop.set_child(Some(&grid));
    settings.set_popover(Some(&pop));
    q.append(&quantize);
    q.append(&settings);
    bar.append(&q);

    let functions = gtk::MenuButton::builder().label("Functions").always_show_arrow(true).build();
    let fpop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    type NoteFn = Box<dyn Fn(&Ctx, &Roll)>;
    let items: Vec<(&str, NoteFn)> = vec![
        ("Legato", Box::new(|c, r| apply(c, r, "Legato", midi_tools::legato))),
        (
            "Fixed Length (Grid)",
            Box::new(|c, r| {
                let len = grid_of(r, &c.s().project.time());
                apply(c, r, "Fixed Length", |n| midi_tools::fixed_length(n, len));
            }),
        ),
        ("Transpose Up an Octave", Box::new(|c, r| apply(c, r, "Transpose", |n| midi_tools::transpose(n, 12)))),
        ("Transpose Down an Octave", Box::new(|c, r| apply(c, r, "Transpose", |n| midi_tools::transpose(n, -12)))),
        (
            "Clear Controller Data",
            Box::new(|c, r| {
                let num = r.controller.get();
                edit_midi(c, "Clear Controller Data", |m| m.controllers.retain(|x| x.number != num));
            }),
        ),
    ];
    for (label, f) in items {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        let (ctx, roll, pop) = (ctx.clone(), roll.clone(), fpop.clone());
        b.connect_clicked(move |_| {
            pop.popdown();
            f(&ctx, &roll);
        });
        list.append(&b);
    }
    fpop.set_child(Some(&list));
    functions.set_popover(Some(&fpop));
    bar.append(&functions);

    let names: Vec<&str> = CONTROLLERS.iter().map(|c| c.1).collect();
    let lane = gtk::DropDown::from_strings(&names);
    lane.set_tooltip_text(Some("Controller lane"));
    {
        let (roll, redraw) = (roll.clone(), redraw.clone());
        lane.connect_selected_notify(move |d| {
            roll.controller.set(CONTROLLERS[d.selected() as usize].0);
            redraw();
        });
    }
    bar.append(&lane);

    let step = gtk::ToggleButton::builder().icon_name("input-keyboard-symbolic").tooltip_text("Step Input").build();
    step.add_css_class("flat");
    {
        let roll = roll.clone();
        step.connect_toggled(move |b| {
            roll.step_input.set(b.is_active());
            roll.step_held.set(0);
        });
    }
    bar.append(&step);

    let hits = gtk::ToggleButton::with_label("Hits");
    hits.set_tooltip_text(Some("Show a row per drum sound instead of a keyboard"));
    bar.append(&hits);

    for (icon, f) in [("zoom-out-symbolic", 1.0 / 1.5), ("zoom-in-symbolic", 1.5)] {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        let (roll, redraw) = (roll.clone(), redraw.clone());
        b.connect_clicked(move |_| {
            roll.ppt.set((roll.ppt.get() * f).clamp(0.01, 2.0));
            redraw();
        });
        bar.append(&b);
    }
    (bar, title, vel, hits)
}

fn quantize_selection(ctx: &Ctx, roll: &Roll) {
    let strength = roll.strength.get();
    match roll.groove.borrow().clone() {
        Some(g) => {
            let velocity = roll.groove_velocity.get();
            apply(ctx, roll, "Quantize", |n| midi_tools::groove_quantize(n, &g, strength, velocity));
        }
        None => {
            let (grid, swing) = (grid_of(roll, &ctx.s().project.time()), roll.swing.get());
            apply(ctx, roll, "Quantize", |n| midi_tools::quantize(n, grid, strength, swing));
        }
    }
}

/// Step Input: each note-on goes in at the playhead; once every key is up, the playhead moves
/// on a grid step, so keys held together make a chord.
fn step_input(ctx: &Ctx, roll: &Roll) {
    let events = ctx.s().live_in.clone();
    let Some((r, _, _)) = midi(ctx) else { return };
    let time = ctx.s().project.time();
    let len = grid_of(roll, &time);
    let pos = ctx.position();
    if pos < r.start {
        ctx.message("Move the playhead into the region to step-enter notes");
        return;
    }
    for d in events {
        let on = d[0] & 0xf0 == 0x90 && d[2] > 0;
        if on {
            let start = time.ticks_between(r.start, pos) as u32;
            let note = Note::new(start, len, d[1], d[2]);
            let end = time.at(r.start, (start + len) as u64);
            let id = r.id;
            ctx.edit("Step Input", |p| {
                if let Some(reg) = p.region_mut(id) {
                    reg.length = reg.length.max(end - reg.start);
                    if let Content::Midi(m) = &mut reg.content {
                        m.notes.retain(|n| !(n.start == start && n.pitch == note.pitch));
                        m.notes.push(note);
                        m.notes.sort_by_key(|n| (n.start, n.pitch));
                    }
                }
            });
            roll.step_held.set(roll.step_held.get() + 1);
        } else if roll.step_held.get() > 0 {
            roll.step_held.set(roll.step_held.get() - 1);
            if roll.step_held.get() == 0 {
                let next = time.at(r.start, time.ticks_between(r.start, pos) + len as u64);
                ctx.locate(next);
            }
        }
    }
}

fn tick_x(roll: &Roll, tick: f64) -> f32 {
    ((tick - roll.t0.get()) * roll.ppt.get()) as f32
}

/// Where a note would be if the current drag ended now.
fn dragged_note(n: &Note, roll: &Roll, time: &TimeMap) -> Note {
    let (dx, dy) = roll.offset.get();
    let grid = grid_ticks(roll.snap.get(), time).max(1) as i64;
    let dt = (dx / roll.ppt.get()) as i64;
    let snap = |t: i64| ((t as f64 / grid as f64).round() as i64 * grid).max(0);
    let sel = roll.selected.borrow();
    let moving = |grab: &Note| grab == n || (sel.contains(grab) && sel.contains(n));
    match roll.drag.get() {
        Drag::Move { grab, .. } if moving(&grab) => {
            let shift = snap(grab.start as i64 + dt) - grab.start as i64;
            let lanes = roll.lanes.borrow();
            let pitch = lanes.moved(n.pitch, (dy / lanes.row_h() as f64).round() as i64);
            Note { start: (n.start as i64 + shift).max(0) as u32, pitch, ..*n }
        }
        Drag::Resize { grab } if moving(&grab) => {
            let change = snap(grab.start as i64 + grab.length as i64 + dt) - (grab.start as i64 + grab.length as i64);
            Note { length: (n.length as i64 + change).max(grid) as u32, ..*n }
        }
        _ => *n,
    }
}

fn playhead(ctx: &Ctx, roll: &Roll, r: &Region, time: &TimeMap, s: &gtk::Snapshot, h: f32, fg: &gdk::RGBA) {
    if ctx.playing() {
        let pos = ctx.position();
        if pos >= r.start && pos < r.span_end() {
            let within = (pos - r.start) % r.length.max(1);
            rect(s, tick_x(roll, time.ticks_between(r.start, r.start + within) as f64), 0.0, 1.0, h, fg);
        }
    }
}

fn draw_roll(ctx: &Ctx, roll: &Roll, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    let shade = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    let lanes = roll.lanes.borrow().clone();
    let rh = lanes.row_h();
    match &lanes {
        Lanes::Keys => {
            for pitch in 0..128u8 {
                if is_black(pitch) {
                    rect(s, 0.0, lanes.y_of(pitch), w, ROW, &shade(0.04));
                }
                if pitch % 12 == 0 {
                    rect(s, 0.0, lanes.y_of(pitch) + ROW - 1.0, w, 1.0, &shade(0.12));
                }
            }
        }
        Lanes::Hits(_) => {
            for i in 0..lanes.count() {
                let y = i as f32 * rh;
                if i % 2 == 1 {
                    rect(s, 0.0, y, w, rh, &shade(0.03));
                }
                rect(s, 0.0, y + rh - 1.0, w, 1.0, &shade(0.08));
            }
        }
    }
    let Some((r, m, color)) = midi(ctx) else { return };
    let time = ctx.s().project.time();
    draw_beats(roll, &time, &r, s, w, h, &fg);

    let base = track_color(color);
    let sel = roll.selected.borrow().clone();
    let copying = matches!(roll.drag.get(), Drag::Move { copy: true, .. });
    for n in &m.notes {
        let d = dragged_note(n, roll, &time);
        let mut shapes = vec![(d, sel.contains(n))];
        if copying && d != *n {
            shapes.insert(0, (*n, false));
        }
        for (note, selected) in shapes {
            let x = tick_x(roll, note.start as f64);
            let y = lanes.y_of(note.pitch);
            let nw = (note.length as f64 * roll.ppt.get()) as f32;
            let a = 0.35 + 0.65 * note.velocity as f32 / 127.0;
            let col = gdk::RGBA::new(base.red(), base.green(), base.blue(), a);
            if lanes.is_hits() {
                let d = rh - 6.0;
                rect(s, x, y + rh / 2.0 - 1.0, nw, 2.0, &gdk::RGBA::new(base.red(), base.green(), base.blue(), 0.3));
                if selected {
                    diamond(s, x, y + rh / 2.0, d + 4.0, &rgba(0xffffff, 1.0));
                }
                diamond(s, x, y + rh / 2.0, d, &col);
                continue;
            }
            rounded(s, x, y + 1.0, nw.max(2.0), ROW - 2.0, 2.0, &col);
            if selected {
                rect(s, x, y + 1.0, nw.max(2.0), 1.5, &rgba(0xffffff, 1.0));
                rect(s, x, y + ROW - 2.5, nw.max(2.0), 1.5, &rgba(0xffffff, 1.0));
            }
        }
    }
    if let Drag::Marquee { x, y, .. } = roll.drag.get() {
        let (dx, dy) = roll.offset.get();
        let accent = canvas::accent(c);
        let (rx, ry) = (x.min(x + dx) as f32, y.min(y + dy) as f32);
        rect(s, rx, ry, dx.abs() as f32, dy.abs() as f32, &gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.15));
    }
    playhead(ctx, roll, &r, &time, s, h, &fg);
}

fn draw_beats(roll: &Roll, time: &TimeMap, r: &Region, s: &gtk::Snapshot, w: f32, h: f32, fg: &gdk::RGBA) {
    let len_ticks = time.ticks_between(r.start, r.start + r.length) as f64;
    // Ticks in the roll count from the region start; bar lines come from the project.
    let t0 = time.sample_to_tick_f(r.start as f64);
    let view_end = roll.t0.get() + w as f64 / roll.ppt.get();
    let to = time.at(r.start, view_end.max(0.0) as u64);
    let from = time.at(r.start, roll.t0.get().max(0.0) as u64);
    for b in time.bars(from, to) {
        let beat = PPQ as u64 * 4 / b.den as u64;
        for k in 0..b.num as u64 {
            let t = (b.tick + k * beat) as f64 - t0;
            let a = if k == 0 {
                0.2
            } else if b.starts_group(k) {
                0.1
            } else {
                0.07
            };
            rect(s, tick_x(roll, t), 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a));
        }
    }
    let end = tick_x(roll, len_ticks);
    rect(s, end, 0.0, w - end, h, &rgba(0x000000, 0.15));
}

fn draw_velocity(ctx: &Ctx, roll: &Roll, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.15));
    let Some((r, m, color)) = midi(ctx) else { return };
    let time = ctx.s().project.time();
    draw_beats(roll, &time, &r, s, w, h, &fg);
    let base = track_color(color);
    let accent = canvas::accent(c);
    let sel = roll.selected.borrow();
    let edit = roll.velocity_edit.borrow();
    for n in &m.notes {
        let mut v = n.velocity as i32;
        if let Some((notes, delta)) = edit.as_ref()
            && notes.contains(n)
        {
            v = (v + delta).clamp(1, 127);
        }
        let x = tick_x(roll, n.start as f64);
        let bh = (v as f32 / 127.0) * (h - 4.0);
        let col = if sel.contains(n) { accent } else { base };
        rect(s, x, h - bh, 2.0, bh, &col);
        rounded(s, x - 2.0, h - bh - 2.0, 6.0, 4.0, 2.0, &col);
    }
}

fn controller_y(number: u8, value: u16, h: f32) -> f32 {
    let max = if number == PITCH_BEND { 16383.0 } else { 127.0 };
    h - 2.0 - (value as f32 / max) * (h - 4.0)
}

fn draw_controllers(ctx: &Ctx, roll: &Roll, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    rect(s, 0.0, 0.0, w, h, &rgba(0x000000, 0.15));
    let Some((r, m, color)) = midi(ctx) else { return };
    let time = ctx.s().project.time();
    draw_beats(roll, &time, &r, s, w, h, &fg);
    let number = roll.controller.get();
    if number == PITCH_BEND {
        rect(s, 0.0, h / 2.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.2));
    }
    let base = track_color(color);
    let draft = roll.draft.borrow();
    let (lo, hi) = (draft.iter().map(|d| d.tick).min(), draft.iter().map(|d| d.tick).max());
    let mut points: Vec<Controller> = m
        .controllers
        .iter()
        .filter(|x| x.number == number)
        .filter(|x| !matches!((lo, hi), (Some(a), Some(b)) if x.tick >= a && x.tick <= b))
        .copied()
        .chain(draft.iter().copied())
        .collect();
    points.sort_by_key(|p| p.tick);
    // Controllers hold their value until the next point, so draw steps.
    for (i, p) in points.iter().enumerate() {
        let x = tick_x(roll, p.tick as f64);
        let next = points.get(i + 1).map_or(w, |n| tick_x(roll, n.tick as f64));
        let y = controller_y(number, p.value, h);
        rect(s, x, y, (next - x).max(1.0), 1.5, &base);
        rounded(s, x - 2.5, y - 2.5, 5.0, 5.0, 2.5, &base);
    }
    if points.is_empty() {
        text(s, c, 8.0, h / 2.0 - 8.0, "Drag to draw controller data", &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.4), Some(8.0));
    }
}

fn find_note(ctx: &Ctx, roll: &Roll, x: f64, y: f64) -> Option<(Note, bool)> {
    let (_, m, _) = midi(ctx)?;
    let lanes = roll.lanes.borrow();
    let row = lanes.index_at(y);
    let tick = roll.t0.get() + x / roll.ppt.get();
    let edge = EDGE / roll.ppt.get();
    if lanes.is_hits() {
        // A hit is its diamond; hits are not resized.
        let half = (HIT_ROW as f64 / 2.0) / roll.ppt.get();
        return m.notes.iter().rev().find(|n| lanes.index(n.pitch) == row && (tick - n.start as f64).abs() <= half).map(|n| (*n, false));
    }
    m.notes.iter().rev().find(|n| lanes.index(n.pitch) == row && tick >= n.start as f64 && tick < (n.start + n.length) as f64 + edge).map(|n| {
        let near_end = tick > (n.start + n.length) as f64 - edge && n.length as f64 > edge * 2.0;
        (*n, near_end)
    })
}

fn preview(ctx: &Ctx, n: &Note) {
    ctx.send(Command::Midi([0x90, n.pitch, n.velocity]));
    ctx.send(Command::Midi([0x80, n.pitch, 0]));
}

fn install_roll_input(ctx: &Ctx, roll: &Rc<Roll>, grid: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    let had_selection = Rc::new(Cell::new(false));
    {
        let (ctx, roll, had) = (ctx.clone(), roll.clone(), had_selection.clone());
        g.connect_drag_begin(move |g, x, y| {
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
            roll.offset.set((0.0, 0.0));
            let mods = g.current_event_state();
            let (shift, alt) = (mods.contains(gdk::ModifierType::SHIFT_MASK), mods.contains(gdk::ModifierType::ALT_MASK));
            had.set(!roll.selected.borrow().is_empty());
            match find_note(&ctx, &roll, x, y) {
                Some((n, _)) if shift => {
                    let mut sel = roll.selected.borrow_mut();
                    match sel.iter().position(|x| *x == n) {
                        Some(i) => {
                            sel.remove(i);
                        }
                        None => sel.push(n),
                    }
                    roll.drag.set(Drag::None);
                }
                Some((n, edge)) => {
                    if !roll.selected.borrow().contains(&n) {
                        *roll.selected.borrow_mut() = vec![n];
                    }
                    roll.drag.set(if edge { Drag::Resize { grab: n } } else { Drag::Move { grab: n, copy: alt } });
                    preview(&ctx, &n);
                }
                None => {
                    if !shift {
                        roll.selected.borrow_mut().clear();
                    }
                    roll.drag.set(Drag::Marquee { x, y, add: shift });
                }
            }
        });
    }
    {
        let (roll, redraw) = (roll.clone(), redraw.clone());
        g.connect_drag_update(move |_, dx, dy| {
            roll.offset.set((dx, dy));
            redraw();
        });
    }
    {
        let (ctx, roll, redraw) = (ctx.clone(), roll.clone(), redraw.clone());
        g.connect_drag_end(move |g, dx, dy| {
            let time = ctx.s().project.time();
            let moved = dx.abs() > 2.0 || dy.abs() > 2.0;
            let d = roll.drag.replace(Drag::None);
            match d {
                Drag::Move { grab, copy } if moved => {
                    let old = roll.selected.borrow().clone();
                    let old = if old.contains(&grab) { old } else { vec![grab] };
                    let new: Vec<Note> = old.iter().map(|n| dragged_note_with(n, &roll, &time, d)).collect();
                    roll.offset.set((0.0, 0.0));
                    edit_notes(&ctx, if copy { "Copy Notes" } else { "Move Notes" }, |notes| {
                        if copy {
                            notes.extend(new.iter().copied());
                        } else {
                            midi_tools::replace(notes, &old, &new);
                        }
                    });
                    *roll.selected.borrow_mut() = new;
                }
                Drag::Resize { grab } if moved => {
                    let old = roll.selected.borrow().clone();
                    let old = if old.contains(&grab) { old } else { vec![grab] };
                    let new: Vec<Note> = old.iter().map(|n| dragged_note_with(n, &roll, &time, d)).collect();
                    roll.offset.set((0.0, 0.0));
                    edit_notes(&ctx, "Resize Notes", |notes| {
                        midi_tools::replace(notes, &old, &new);
                    });
                    *roll.selected.borrow_mut() = new;
                }
                Drag::Move { grab, .. } if !g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK) => {
                    *roll.selected.borrow_mut() = vec![grab];
                }
                Drag::Marquee { x, y, add } if moved => {
                    let (t0, t1) = (roll.t0.get() + x.min(x + dx) / roll.ppt.get(), roll.t0.get() + x.max(x + dx) / roll.ppt.get());
                    let lanes = roll.lanes.borrow().clone();
                    let rows = lanes.index_at(y.min(y + dy))..=lanes.index_at(y.max(y + dy));
                    if let Some((_, m, _)) = midi(&ctx) {
                        let mut sel = if add { roll.selected.borrow().clone() } else { Vec::new() };
                        for n in m.notes.iter().filter(|n| {
                            (n.start as f64) < t1 && ((n.start + n.length) as f64) > t0 && rows.contains(&lanes.index(n.pitch))
                        }) {
                            if !sel.contains(n) {
                                sel.push(*n);
                            }
                        }
                        *roll.selected.borrow_mut() = sel;
                    }
                }
                // A click on empty space adds a note, unless it only clears a selection.
                Drag::Marquee { x, y, .. } if !had_selection.get() => {
                    let grid = grid_of(&roll, &time);
                    let tick = (roll.t0.get() + x / roll.ppt.get()).max(0.0) as u32;
                    let Some((r, _)) = region(&ctx) else { return };
                    if tick as u64 >= time.ticks_between(r.start, r.start + r.length) {
                        return;
                    }
                    let Some(pitch) = roll.lanes.borrow().pitch_at(y) else { return };
                    let n = Note::new(tick / grid * grid, grid, pitch, 100);
                    *roll.selected.borrow_mut() = vec![n];
                    roll.just_added.set(Some(n));
                    preview(&ctx, &n);
                    edit_notes(&ctx, "Add Note", |notes| notes.push(n));
                }
                _ => {}
            }
            roll.offset.set((0.0, 0.0));
            redraw();
        });
    }
    grid.add_controller(g);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n == 1 {
                roll.just_added.set(None);
            }
            if n == 2
                && let Some((note, _)) = find_note(&ctx, &roll, x, y)
                && roll.just_added.get() != Some(note)
            {
                roll.selected.borrow_mut().retain(|n| *n != note);
                edit_notes(&ctx, "Delete Note", |notes| notes.retain(|n| *n != note));
            }
        });
    }
    grid.add_controller(dbl);

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, roll, redraw) = (ctx.clone(), roll.clone(), redraw.clone());
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            let time = ctx.s().project.time();
            let grid = grid_of(&roll, &time) as i64;
            let handled = match key {
                gdk::Key::Delete | gdk::Key::BackSpace => {
                    let sel = std::mem::take(&mut *roll.selected.borrow_mut());
                    if !sel.is_empty() {
                        edit_notes(&ctx, "Delete Notes", |notes| notes.retain(|n| !sel.contains(n)));
                    }
                    true
                }
                gdk::Key::Up | gdk::Key::Down if !roll.selected.borrow().is_empty() => {
                    let lanes = roll.lanes.borrow().clone();
                    if lanes.is_hits() {
                        let rows = if key == gdk::Key::Up { -1 } else { 1 };
                        apply(&ctx, &roll, "Move Hits", |n| n.iter().map(|n| Note { pitch: lanes.moved(n.pitch, rows), ..*n }).collect());
                    } else {
                        let step = if shift { 12 } else { 1 } * if key == gdk::Key::Up { 1 } else { -1 };
                        apply(&ctx, &roll, "Transpose", |n| midi_tools::transpose(n, step));
                    }
                    true
                }
                gdk::Key::Left | gdk::Key::Right if !roll.selected.borrow().is_empty() => {
                    let step = if key == gdk::Key::Right { grid } else { -grid };
                    apply(&ctx, &roll, "Nudge Notes", |n| midi_tools::shift(n, step));
                    true
                }
                gdk::Key::q if !ctrl => {
                    quantize_selection(&ctx, &roll);
                    true
                }
                gdk::Key::Escape => {
                    roll.selected.borrow_mut().clear();
                    true
                }
                gdk::Key::a if ctrl => {
                    if let Some((_, m, _)) = midi(&ctx) {
                        *roll.selected.borrow_mut() = m.notes;
                    }
                    true
                }
                gdk::Key::c | gdk::Key::x if ctrl => {
                    let sel = roll.selected.borrow().clone();
                    *roll.clipboard.borrow_mut() = sel.clone();
                    if key == gdk::Key::x && !sel.is_empty() {
                        roll.selected.borrow_mut().clear();
                        edit_notes(&ctx, "Cut Notes", |notes| notes.retain(|n| !sel.contains(n)));
                    }
                    true
                }
                gdk::Key::v if ctrl => {
                    // Pastes at the playhead when it's inside the region, else straight after the
                    // copied notes, on the grid.
                    let clip = roll.clipboard.borrow().clone();
                    let Some((r, _)) = region(&ctx) else { return glib::Propagation::Stop };
                    if let Some(first) = clip.iter().map(|n| n.start).min() {
                        let pos = ctx.position();
                        let end = clip.iter().map(|n| n.start + n.length).max().unwrap_or(first);
                        let grid = grid_of(&roll, &time).max(1);
                        let at = if pos >= r.start && pos < r.end() { time.ticks_between(r.start, pos) as u32 } else { end.div_ceil(grid) * grid };
                        let new: Vec<Note> = clip.iter().map(|n| Note { start: n.start - first + at, ..*n }).collect();
                        let added = new.clone();
                        edit_notes(&ctx, "Paste Notes", |notes| notes.extend(added));
                        *roll.selected.borrow_mut() = new;
                    }
                    true
                }
                gdk::Key::d if ctrl => {
                    // Duplicate: a copy straight after the selection.
                    let sel = roll.selected.borrow().clone();
                    if let (Some(a), Some(b)) = (sel.iter().map(|n| n.start).min(), sel.iter().map(|n| n.start + n.length).max()) {
                        let span = (b - a).div_ceil(grid as u32).max(1) * grid as u32;
                        let new: Vec<Note> = sel.iter().map(|n| Note { start: n.start + span, ..*n }).collect();
                        let added = new.clone();
                        edit_notes(&ctx, "Duplicate Notes", |notes| notes.extend(added));
                        *roll.selected.borrow_mut() = new;
                    }
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

    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let (roll, redraw) = (roll.clone(), redraw.clone());
        sc.connect_scroll(move |sc, dx, dy| {
            let mods = sc.current_event_state();
            if mods.contains(gdk::ModifierType::CONTROL_MASK) {
                roll.ppt.set((roll.ppt.get() * if dy > 0.0 { 1.0 / 1.2 } else { 1.2 }).clamp(0.01, 2.0));
            } else if mods.contains(gdk::ModifierType::SHIFT_MASK) || dx != 0.0 {
                let d = if dx != 0.0 { dx } else { dy };
                roll.hadj.set_value(roll.hadj.value() + d * 40.0 / roll.ppt.get());
            } else {
                return glib::Propagation::Proceed;
            }
            redraw();
            glib::Propagation::Stop
        });
    }
    grid.add_controller(sc);
}

/// `dragged_note` for a drag that has already been taken out of `roll.drag`.
fn dragged_note_with(n: &Note, roll: &Roll, time: &TimeMap, d: Drag) -> Note {
    let saved = roll.drag.replace(d);
    let out = dragged_note(n, roll, time);
    roll.drag.set(saved);
    out
}

fn install_velocity_input(ctx: &Ctx, roll: &Rc<Roll>, lane: &Canvas, redraw: &Rc<dyn Fn()>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        g.connect_drag_begin(move |_, x, _| {
            let Some((_, m, _)) = midi(&ctx) else { return };
            let tick = roll.t0.get() + x / roll.ppt.get();
            let reach = 5.0 / roll.ppt.get();
            let Some(n) = m.notes.iter().min_by(|a, b| {
                (a.start as f64 - tick).abs().total_cmp(&(b.start as f64 - tick).abs())
            }) else {
                return;
            };
            if (n.start as f64 - tick).abs() > reach {
                return;
            }
            let sel = roll.selected.borrow().clone();
            let notes = if sel.contains(n) { sel } else { vec![*n] };
            *roll.velocity_edit.borrow_mut() = Some((notes, 0));
        });
    }
    {
        let (roll, redraw) = (roll.clone(), redraw.clone());
        g.connect_drag_update(move |_, _, dy| {
            if let Some((_, delta)) = roll.velocity_edit.borrow_mut().as_mut() {
                *delta = (-dy / (LANE_H as f64 - 4.0) * 127.0) as i32;
            }
            redraw();
        });
    }
    {
        let (ctx, roll, redraw) = (ctx.clone(), roll.clone(), redraw.clone());
        g.connect_drag_end(move |_, _, _| {
            let Some((old, delta)) = roll.velocity_edit.borrow_mut().take() else { return };
            if delta != 0 {
                let new: Vec<Note> =
                    old.iter().map(|n| Note { velocity: (n.velocity as i32 + delta).clamp(1, 127) as u8, ..*n }).collect();
                // The selection follows the notes before the edit, so the refresh keeps it.
                {
                    let mut sel = roll.selected.borrow_mut();
                    for (o, n) in old.iter().zip(&new) {
                        if let Some(s) = sel.iter_mut().find(|s| *s == o) {
                            *s = *n;
                        }
                    }
                }
                edit_notes(&ctx, "Change Velocity", |notes| {
                    midi_tools::replace(notes, &old, &new);
                });
            }
            redraw();
        });
    }
    lane.add_controller(g);
}

fn install_controller_input(ctx: &Ctx, roll: &Rc<Roll>, lane: &Canvas, redraw: &Rc<dyn Fn()>) {
    let point = |roll: &Roll, x: f64, y: f64, h: f64| -> Controller {
        let number = roll.controller.get();
        let max = if number == PITCH_BEND { 16383.0 } else { 127.0 };
        let step = PPQ as f64 / 8.0;
        let tick = ((roll.t0.get() + x / roll.ppt.get()).max(0.0) / step).round() * step;
        let value = ((h - 2.0 - y) / (h - 4.0) * max).round().clamp(0.0, max) as u16;
        Controller { tick: tick as u32, number, value }
    };
    let g = gtk::GestureDrag::new();
    {
        let (roll, redraw, lane2) = (roll.clone(), redraw.clone(), lane.clone());
        g.connect_drag_begin(move |_, x, y| {
            *roll.draft.borrow_mut() = vec![point(&roll, x, y, lane2.height() as f64)];
            redraw();
        });
    }
    {
        let (roll, redraw, lane2) = (roll.clone(), redraw.clone(), lane.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some((sx, sy)) = g.start_point() else { return };
            let p = point(&roll, sx + dx, sy + dy, lane2.height() as f64);
            let mut draft = roll.draft.borrow_mut();
            draft.retain(|d| d.tick != p.tick);
            draft.push(p);
            draft.sort_by_key(|d| d.tick);
            drop(draft);
            redraw();
        });
    }
    {
        let (ctx, roll, redraw) = (ctx.clone(), roll.clone(), redraw.clone());
        g.connect_drag_end(move |_, _, _| {
            let draft = std::mem::take(&mut *roll.draft.borrow_mut());
            let (Some(lo), Some(hi)) = (draft.iter().map(|d| d.tick).min(), draft.iter().map(|d| d.tick).max()) else { return };
            let number = roll.controller.get();
            edit_midi(&ctx, "Draw Controller Data", |m| {
                m.controllers.retain(|c| c.number != number || c.tick < lo || c.tick > hi);
                m.controllers.extend(draft);
            });
            redraw();
        });
    }
    lane.add_controller(g);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits() -> Lanes {
        let row = |pitch, name: &str| DrumRow { pitch, name: name.into(), hidden: false, mute: false, solo: false, grid: None };
        Lanes::Hits(vec![row(42, "Closed Hat"), row(38, "Snare"), row(36, "Kick")])
    }

    #[test]
    fn midi_regions_open_on_the_picked_page_else_by_content() {
        assert_eq!(midi_page(None, false, false), "midi");
        assert_eq!(midi_page(None, false, true), "steps");
        assert_eq!(midi_page(None, true, true), "takes");
        assert_eq!(midi_page(Some("events"), true, false), "events");
        assert_eq!(midi_page(Some("midi"), false, true), "midi");
        assert_eq!(midi_page(Some("takes"), false, true), "steps");
    }

    #[test]
    fn the_automatic_choice_is_made_once_per_region() {
        let cache = Cell::new(None);
        let calls = Cell::new(0);
        let fits = |v| {
            calls.set(calls.get() + 1);
            v
        };
        assert!(!remembered(&cache, 7, || fits(false)));
        // Edits that put every note on the grid leave the Hits view showing.
        assert!(!remembered(&cache, 7, || fits(true)));
        assert_eq!(calls.get(), 1);
        assert!(remembered(&cache, 8, || fits(true)));
        cache.set(None);
        assert!(!remembered(&cache, 8, || fits(false)));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn rows_map_to_pitches_and_back() {
        for lanes in [Lanes::Keys, hits()] {
            for i in 0..lanes.count() {
                let y = (i as f32 + 0.5) * lanes.row_h();
                assert_eq!(lanes.index_at(y as f64), i);
                if let Some(p) = lanes.pitch_at(y as f64) {
                    assert_eq!(lanes.y_of(p), i as f32 * lanes.row_h());
                }
            }
        }
        assert_eq!((Lanes::Keys.pitch_at(0.0), Lanes::Keys.y_of(0)), (Some(127), 127.0 * ROW));
        let h = hits();
        assert_eq!((h.count(), h.pitch_at(0.0), h.pitch_at(2.5 * HIT_ROW as f64)), (4, Some(42), Some(36)));
        // Pitches without a row share the last one, which takes no new notes.
        assert_eq!((h.index(60), h.index(61), h.pitch(3)), (3, 3, None));
        assert_eq!(h.index_at(-5.0), 0);
        assert_eq!(h.index_at(1000.0), 3);
    }

    #[test]
    fn moving_notes_between_rows() {
        assert_eq!(Lanes::Keys.moved(60, 2), 58);
        assert_eq!(Lanes::Keys.moved(1, 5), 0);
        let h = hits();
        assert_eq!((h.moved(42, 1), h.moved(42, 2), h.moved(42, 9), h.moved(36, -1)), (38, 36, 36, 38));
        // A note on the last row stays put until it moves, then lands on a drum row.
        assert_eq!((h.moved(60, 0), h.moved(60, -1), h.moved(60, 1)), (60, 36, 36));
    }
}
