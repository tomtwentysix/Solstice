use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::editor::{edit_midi, midi};
use crate::studio::Ctx;
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_dsp::sampler::PADS;
use solstice_engine::Command;
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

const ROW_H: f32 = 26.0;
const CELL_W: f32 = 24.0;
const NAMES_W: i32 = 120;
const STEPS: [(u32, &str); 3] = [(PPQ / 2, "1/8"), (PPQ / 4, "1/16"), (PPQ / 8, "1/32")];

struct Steps {
    len: Cell<u32>,
    swing: Cell<f32>,
    /// While painting: whether cells are being switched on or off, and which were done.
    paint: Cell<Option<bool>>,
    visited: RefCell<HashSet<(usize, usize)>>,
}

/// The note and name for each row: the Drum Kit's pads, or a run of notes for other instruments.
fn rows(ctx: &Ctx) -> Vec<(u8, String)> {
    let s = ctx.s();
    let drums = s
        .selected_region
        .and_then(|id| s.project.find_region(id))
        .and_then(|(ti, _)| s.project.tracks[ti].instrument.as_ref())
        .is_some_and(|i| i.uid == "solstice:drums");
    if drums {
        PADS.iter().map(|(n, name, _)| (*n, name.to_string())).collect()
    } else {
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        (0..16u8).map(|i| 72 - i).map(|n| (n, format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 1))).collect()
    }
}

pub fn is_drum_track(ctx: &Ctx) -> bool {
    let s = ctx.s();
    s.selected_region
        .and_then(|id| s.project.find_region(id))
        .and_then(|(ti, _)| s.project.tracks[ti].instrument.as_ref())
        .is_some_and(|i| i.uid == "solstice:drums")
}

/// Where step `c` starts, with swing delaying every second step.
fn step_start(st: &Steps, c: usize) -> u32 {
    let len = st.len.get();
    let swing = if c % 2 == 1 { (st.swing.get() * len as f32 / 2.0) as u32 } else { 0 };
    c as u32 * len + swing
}

fn cell_of(st: &Steps, n: &Note) -> usize {
    (n.start / st.len.get()) as usize
}

fn set_cell(ctx: &Ctx, st: &Steps, note: u8, c: usize, on: bool) {
    let len = st.len.get();
    let start = step_start(st, c);
    edit_midi_live(ctx, |m| {
        m.notes.retain(|n| !(n.pitch == note && n.start / len == c as u32));
        if on {
            m.notes.push(Note { start, length: len, pitch: note, velocity: 100 });
        }
    });
}

/// Changes the selected region's MIDI during a gesture (no undo step until it ends).
fn edit_midi_live(ctx: &Ctx, f: impl FnOnce(&mut MidiContent)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.live(true, |p| {
        if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
            f(m);
            m.notes.sort_by_key(|n| (n.start, n.pitch));
        }
    });
}

pub fn build(ctx: &Ctx, to_roll: Rc<dyn Fn()>) -> gtk::Widget {
    let st = Rc::new(Steps {
        len: Cell::new(PPQ / 4),
        swing: Cell::new(0.0),
        paint: Cell::new(None),
        visited: RefCell::default(),
    });

    let names = {
        let ctx = ctx.clone();
        Canvas::new(move |c, s, w, _| {
            let fg = canvas::fg(c);
            for (i, (_, name)) in rows(&ctx).iter().enumerate() {
                let y = i as f32 * ROW_H;
                rect(s, 0.0, y + ROW_H - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08));
                text(s, c, 8.0, y + 4.0, name, &fg, Some(8.5));
            }
        })
    };
    names.set_size_request(NAMES_W, (16.0 * ROW_H) as i32);
    {
        let g = gtk::GestureClick::new();
        let ctx = ctx.clone();
        g.connect_pressed(move |_, _, _, y| {
            if let Some((note, _)) = rows(&ctx).get((y / ROW_H as f64) as usize) {
                ctx.send(Command::Midi([0x99, *note, 110]));
                ctx.send(Command::Midi([0x89, *note, 0]));
            }
        });
        names.add_controller(g);
    }

    let grid = {
        let (ctx, st) = (ctx.clone(), st.clone());
        Canvas::new(move |c, s, w, h| draw(&ctx, &st, c, s, w, h))
    };
    let resize = {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        move || {
            let steps = midi(&ctx).map_or(16, |(r, _, _)| {
                let time = ctx.s().project.time();
                (time.samples_to_ticks(r.length) / st.len.get() as u64).max(1) as i32
            });
            grid.set_size_request((steps as f32 * CELL_W) as i32, (16.0 * ROW_H) as i32);
            grid.queue_draw();
        }
    };
    install_input(ctx, &st, &grid);

    let hscroll = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .child(&grid)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&names);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&hscroll);
    let vscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&body).build();

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    let title = gtk::Label::new(Some("Step Sequencer"));
    title.add_css_class("heading");
    bar.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let step = gtk::DropDown::from_strings(&STEPS.map(|s| s.1));
    step.set_selected(1);
    step.set_tooltip_text(Some("Step length"));
    {
        let (st, resize) = (st.clone(), resize.clone());
        step.connect_selected_notify(move |d| {
            st.len.set(STEPS[d.selected() as usize].0);
            resize();
        });
    }
    bar.append(&step);

    bar.append(&gtk::Label::new(Some("Swing")));
    let swing = gtk::SpinButton::with_range(0.0, 100.0, 5.0);
    swing.set_tooltip_text(Some("Delays every second step; moves the notes on those steps"));
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        swing.connect_value_changed(move |s| {
            st.swing.set(s.value() as f32 / 100.0);
            let len = st.len.get();
            edit_midi(&ctx, "Swing", |m| {
                for n in &mut m.notes {
                    let c = (n.start / len) as usize;
                    n.start = step_start(&st, c);
                }
            });
            grid.queue_draw();
        });
    }
    bar.append(&swing);

    bar.append(&gtk::Label::new(Some("Bars")));
    let bars = gtk::SpinButton::with_range(1.0, 64.0, 1.0);
    bars.set_tooltip_text(Some("Pattern length"));
    {
        let (ctx, resize) = (ctx.clone(), resize.clone());
        bars.connect_value_changed(move |b| {
            let Some(id) = ctx.s().selected_region else { return };
            let n = b.value() as u64;
            let bar = ctx.s().project.time().bar_len();
            if ctx.s().project.find_region(id).is_some_and(|(_, r)| r.length != n * bar) {
                ctx.edit("Pattern Length", |p| {
                    if let Some(r) = p.region_mut(id) {
                        r.length = n * bar;
                    }
                });
            }
            resize();
        });
    }
    bar.append(&bars);

    let roll = gtk::Button::with_label("Piano Roll");
    roll.connect_clicked(move |_| to_roll());
    bar.append(&roll);

    {
        let (ctx2, grid, names, resize, bars) = (ctx.clone(), grid.clone(), names.clone(), resize.clone(), bars.clone());
        ctx.on(move |_| {
            if let Some((r, _, _)) = midi(&ctx2) {
                let bar = ctx2.s().project.time().bar_len().max(1);
                let n = (r.length as f64 / bar as f64).round().max(1.0);
                if (bars.value() - n).abs() > 0.5 {
                    bars.set_value(n);
                }
            }
            resize();
            names.queue_draw();
            grid.queue_draw();
        });
    }
    {
        let ctx = ctx.clone();
        grid.add_tick_callback(move |g, _| {
            if ctx.playing() {
                g.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }
    resize();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&vscroll);
    root.upcast()
}

fn draw(ctx: &Ctx, st: &Steps, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    let Some((r, m, color)) = midi(ctx) else { return };
    let rows = rows(ctx);
    let time = ctx.s().project.time();
    let len = st.len.get();
    let steps = (time.samples_to_ticks(r.length) / len as u64).max(1) as usize;
    let per_beat = (time.ticks_per_beat() / len as u64).max(1) as usize;
    let base = track_color(color);
    for col in 0..steps {
        let x = col as f32 * CELL_W;
        let shade = if (col / per_beat) % 2 == 0 { 0.06 } else { 0.03 };
        rect(s, x, 0.0, CELL_W, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), shade));
        if col % (per_beat * time.sig_num as usize) == 0 {
            rect(s, x, 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.3));
        }
    }
    for (ri, (note, _)) in rows.iter().enumerate() {
        let y = ri as f32 * ROW_H;
        rect(s, 0.0, y + ROW_H - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.08));
        for col in 0..steps {
            let x = col as f32 * CELL_W;
            rounded(s, x + 2.0, y + 3.0, CELL_W - 4.0, ROW_H - 6.0, 3.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.07));
        }
        for n in m.notes.iter().filter(|n| n.pitch == *note) {
            let col = cell_of(st, n);
            if col >= steps {
                continue;
            }
            let x = col as f32 * CELL_W;
            let v = n.velocity as f32 / 127.0;
            rounded(s, x + 2.0, y + 3.0, CELL_W - 4.0, ROW_H - 6.0, 3.0, &gdk::RGBA::new(base.red(), base.green(), base.blue(), 0.35 + 0.65 * v));
            // Velocity as a bar along the cell's foot.
            rect(s, x + 4.0, y + ROW_H - 6.0, (CELL_W - 8.0) * v, 2.0, &rgba(0xffffff, 0.8));
        }
    }
    if ctx.playing() {
        let pos = ctx.position();
        if pos >= r.start && pos < r.span_end() {
            let tick = time.samples_to_ticks((pos - r.start) % r.length.max(1));
            let x = (tick / len as u64) as f32 * CELL_W;
            rect(s, x, 0.0, CELL_W, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
        }
    }
}

fn cell_at(ctx: &Ctx, st: &Steps, x: f64, y: f64) -> Option<(u8, usize)> {
    let rows = rows(ctx);
    let (note, _) = rows.get((y / ROW_H as f64) as usize)?;
    if x < 0.0 {
        return None;
    }
    let (r, _, _) = midi(ctx)?;
    let time = ctx.s().project.time();
    let steps = (time.samples_to_ticks(r.length) / st.len.get() as u64) as usize;
    let col = (x / CELL_W as f64) as usize;
    (col < steps).then_some((*note, col))
}

fn is_on(ctx: &Ctx, st: &Steps, note: u8, col: usize) -> bool {
    midi(ctx).is_some_and(|(_, m, _)| m.notes.iter().any(|n| n.pitch == note && cell_of(st, n) == col))
}

fn install_input(ctx: &Ctx, st: &Rc<Steps>, grid: &Canvas) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        g.connect_drag_begin(move |_, x, y| {
            let Some((note, col)) = cell_at(&ctx, &st, x, y) else { return };
            let on = !is_on(&ctx, &st, note, col);
            st.paint.set(Some(on));
            st.visited.borrow_mut().clear();
            st.visited.borrow_mut().insert((note as usize, col));
            ctx.begin_gesture();
            set_cell(&ctx, &st, note, col, on);
            if on {
                ctx.send(Command::Midi([0x99, note, 100]));
                ctx.send(Command::Midi([0x89, note, 0]));
            }
            grid.queue_draw();
        });
    }
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some(on) = st.paint.get() else { return };
            let Some((sx, sy)) = g.start_point() else { return };
            let Some((note, col)) = cell_at(&ctx, &st, sx + dx, sy + dy) else { return };
            if st.visited.borrow_mut().insert((note as usize, col)) {
                set_cell(&ctx, &st, note, col, on);
                grid.queue_draw();
            }
        });
    }
    {
        let (ctx, st) = (ctx.clone(), st.clone());
        g.connect_drag_end(move |_, _, _| {
            if st.paint.take().is_some() {
                ctx.end_gesture("Edit Steps");
            }
        });
    }
    grid.add_controller(g);

    // Scroll over a step to change its velocity.
    let pointer = Rc::new(Cell::new((0.0, 0.0)));
    let motion = gtk::EventControllerMotion::new();
    {
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, y| pointer.set((x, y)));
    }
    grid.add_controller(motion);
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        sc.connect_scroll(move |_, _, dy| {
            let (x, y) = pointer.get();
            let Some((note, col)) = cell_at(&ctx, &st, x, y) else { return glib::Propagation::Proceed };
            if !is_on(&ctx, &st, note, col) {
                return glib::Propagation::Proceed;
            }
            let delta: i32 = if dy < 0.0 { 8 } else { -8 };
            edit_midi(&ctx, "Step Velocity", |m| {
                for n in m.notes.iter_mut().filter(|n| n.pitch == note && cell_of(&st, n) == col) {
                    n.velocity = (n.velocity as i32 + delta).clamp(1, 127) as u8;
                }
            });
            grid.queue_draw();
            glib::Propagation::Stop
        });
    }
    grid.add_controller(sc);
}
