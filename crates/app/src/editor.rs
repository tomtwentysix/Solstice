use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::studio::{Change, Ctx};
use crate::timeline::{grid_ticks, snap_options};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_engine::Command;
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

const ROW: f32 = 12.0;
const KEY_W: i32 = 56;
const EDGE: f64 = 5.0;

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    None,
    Move(Note),
    Resize(Note),
}

struct Roll {
    /// Pixels per tick and the first visible tick.
    ppt: Cell<f64>,
    t0: Cell<f64>,
    snap: Cell<u64>,
    selected: Cell<Option<Note>>,
    drag: Cell<Drag>,
    offset: Cell<(f64, f64)>,
    hadj: gtk::Adjustment,
}

/// The editor pane: a piano roll for MIDI regions and a waveform view for audio regions.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let stack = gtk::Stack::new();
    let empty = adw::StatusPage::builder()
        .icon_name("document-edit-symbolic")
        .title("No Region Selected")
        .description("Select a region in the tracks area to edit it")
        .build();
    empty.add_css_class("compact");
    stack.add_named(&empty, Some("empty"));
    stack.add_named(&piano_roll(ctx), Some("midi"));
    stack.add_named(&audio_editor(ctx), Some("audio"));
    let update = {
        let (ctx, stack) = (ctx.clone(), stack.clone());
        move || {
            let s = ctx.s();
            let page = match s.selected_region.and_then(|id| s.project.find_region(id)) {
                Some((_, r)) if matches!(r.content, Content::Midi(_)) => "midi",
                Some(_) => "audio",
                None => "empty",
            };
            stack.set_visible_child_name(page);
        }
    };
    update();
    ctx.on(move |_| update());
    stack.upcast()
}

fn region(ctx: &Ctx) -> Option<(Region, u8)> {
    let s = ctx.s();
    let (ti, r) = s.selected_region.and_then(|id| s.project.find_region(id))?;
    Some((r.clone(), s.project.tracks[ti].color))
}

fn note_y(pitch: u8) -> f32 {
    (127 - pitch as i32) as f32 * ROW
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

fn edit_notes(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Vec<Note>)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
            f(&mut m.notes);
            m.notes.sort_by_key(|n| (n.start, n.pitch));
        }
    });
}

fn piano_roll(ctx: &Ctx) -> gtk::Widget {
    let roll = Rc::new(Roll {
        ppt: Cell::new(0.12),
        t0: Cell::new(0.0),
        snap: Cell::new(PPQ as u64 / 4),
        selected: Cell::new(None),
        drag: Cell::new(Drag::None),
        offset: Cell::new((0.0, 0.0)),
        hadj: gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0),
    });

    let keys = {
        Canvas::new(move |c, s, w, _| {
            for pitch in 0..128u8 {
                let y = note_y(pitch);
                let col = if is_black(pitch) { rgba(0x241f31, 1.0) } else { rgba(0xf6f5f4, 1.0) };
                rect(s, 0.0, y, w, ROW - 1.0, &col);
                if pitch % 12 == 0 {
                    text(s, c, 4.0, y - 1.0, &format!("C{}", pitch as i32 / 12 - 1), &rgba(0x000000, 0.8), Some(7.0));
                }
            }
        })
    };
    keys.set_size_request(KEY_W, (128.0 * ROW) as i32);
    {
        let ctx = ctx.clone();
        let g = gtk::GestureClick::new();
        let held = Rc::new(Cell::new(None::<u8>));
        let h2 = held.clone();
        let ctx2 = ctx.clone();
        g.connect_pressed(move |_, _, _, y| {
            let pitch = (127 - (y / ROW as f64) as i32).clamp(0, 127) as u8;
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
    grid.set_size_request(-1, (128.0 * ROW) as i32);
    grid.set_focusable(true);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&keys);
    body.append(&grid);
    let vscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&body).build();
    // Start around middle C.
    vscroll.vadjustment().set_value(note_y(84) as f64);

    let hbar = gtk::Scrollbar::new(gtk::Orientation::Horizontal, Some(&roll.hadj));
    hbar.set_margin_start(KEY_W);
    {
        let (roll, grid) = (roll.clone(), grid.clone());
        roll.hadj.clone().connect_value_changed(move |a| {
            roll.t0.set(a.value());
            grid.queue_draw();
        });
    }

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
    vel.set_tooltip_text(Some("Velocity"));
    vel.set_sensitive(false);
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
    let quantize = gtk::Button::with_label("Quantize");
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        quantize.connect_clicked(move |_| {
            let time = ctx.s().project.time();
            let g = grid_ticks(roll.snap.get(), &time).max(1) as u32;
            let sel = roll.selected.get();
            edit_notes(&ctx, "Quantize", |notes| {
                for n in notes.iter_mut().filter(|n| sel.is_none_or(|s| s == **n)) {
                    n.start = (n.start + g / 2) / g * g;
                }
            });
            roll.selected.set(None);
        });
    }
    bar.append(&quantize);
    for (icon, f) in [("zoom-out-symbolic", 1.0 / 1.5), ("zoom-in-symbolic", 1.5)] {
        let b = gtk::Button::from_icon_name(icon);
        b.add_css_class("flat");
        let (roll, grid) = (roll.clone(), grid.clone());
        b.connect_clicked(move |_| {
            roll.ppt.set((roll.ppt.get() * f).clamp(0.01, 2.0));
            grid.queue_draw();
        });
        bar.append(&b);
    }

    {
        let (roll, ctx2) = (roll.clone(), ctx.clone());
        vel.connect_value_changed(move |v| {
            let Some(sel) = roll.selected.get() else { return };
            let value = v.value() as u8;
            if sel.velocity == value {
                return;
            }
            let new = Note { velocity: value, ..sel };
            roll.selected.set(Some(new));
            edit_notes(&ctx2, "Change Velocity", |notes| {
                if let Some(n) = notes.iter_mut().find(|n| **n == sel) {
                    *n = new;
                }
            });
        });
    }

    install_roll_input(ctx, &roll, &grid);

    let refresh = {
        let (ctx, roll, grid, title, vel) = (ctx.clone(), roll.clone(), grid.clone(), title.clone(), vel.clone());
        move || {
            if let Some((r, _)) = region(&ctx) {
                title.set_text(&r.name);
                let time = ctx.s().project.time();
                let ticks = time.samples_to_ticks(r.length) as f64;
                let page = grid.width().max(1) as f64 / roll.ppt.get();
                roll.hadj.configure(roll.t0.get(), 0.0, ticks.max(page) + time.ticks_per_bar() as f64, page / 10.0, page, page);
            }
            match roll.selected.get() {
                Some(n) => {
                    vel.set_sensitive(true);
                    vel.set_value(n.velocity as f64);
                }
                None => vel.set_sensitive(false),
            }
            grid.queue_draw();
        }
    };
    refresh();
    {
        let refresh = refresh.clone();
        let roll = roll.clone();
        let last = Cell::new(None::<Id>);
        let ctx2 = ctx.clone();
        ctx.on(move |c| {
            let cur = ctx2.s().selected_region;
            if c == Change::Selection && cur != last.get() {
                roll.selected.set(None);
                roll.t0.set(0.0);
            }
            last.set(cur);
            refresh();
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
    {
        let refresh = refresh.clone();
        grid.connect_map(move |_| refresh());
    }
    let sel_refresh = refresh;
    let notify = gtk::EventControllerLegacy::new();
    notify.connect_event(move |_, e| {
        if e.event_type() == gdk::EventType::ButtonRelease {
            sel_refresh();
        }
        glib::Propagation::Proceed
    });
    grid.add_controller(notify);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&vscroll);
    root.append(&hbar);
    root.upcast()
}

fn tick_x(roll: &Roll, tick: f64) -> f32 {
    ((tick - roll.t0.get()) * roll.ppt.get()) as f32
}

fn dragged_note(n: &Note, roll: &Roll, time: &TimeMap) -> Note {
    let (dx, dy) = roll.offset.get();
    let grid = grid_ticks(roll.snap.get(), time).max(1) as i64;
    let dt = (dx / roll.ppt.get()) as i64;
    let snap = |t: i64| ((t as f64 / grid as f64).round() as i64 * grid).max(0);
    match roll.drag.get() {
        Drag::Move(d) if d == *n => {
            let pitch = (n.pitch as i64 - (dy / ROW as f64).round() as i64).clamp(0, 127) as u8;
            Note { start: snap(n.start as i64 + dt) as u32, pitch, ..*n }
        }
        Drag::Resize(d) if d == *n => {
            let end = snap(n.start as i64 + n.length as i64 + dt).max(n.start as i64 + grid);
            Note { length: (end - n.start as i64) as u32, ..*n }
        }
        _ => *n,
    }
}

fn draw_roll(ctx: &Ctx, roll: &Roll, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let fg = canvas::fg(c);
    for pitch in 0..128u8 {
        if is_black(pitch) {
            rect(s, 0.0, note_y(pitch), w, ROW, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.04));
        }
        if pitch % 12 == 0 {
            rect(s, 0.0, note_y(pitch) + ROW - 1.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
        }
    }
    let Some((r, color)) = region(ctx) else { return };
    let Content::Midi(m) = &r.content else { return };
    let time = ctx.s().project.time();
    let len_ticks = time.samples_to_ticks(r.length) as f64;
    let beat = time.ticks_per_beat() as f64;
    let mut t = (roll.t0.get() / beat).floor() * beat;
    while tick_x(roll, t) < w {
        let strong = (t as u64) % time.ticks_per_bar() == 0;
        let a = if strong { 0.2 } else { 0.07 };
        rect(s, tick_x(roll, t), 0.0, 1.0, h, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a));
        t += beat;
    }
    let end = tick_x(roll, len_ticks);
    rect(s, end, 0.0, w - end, h, &rgba(0x000000, 0.15));

    let base = track_color(color);
    let sel = roll.selected.get();
    for n in &m.notes {
        let d = dragged_note(n, roll, &time);
        let x = tick_x(roll, d.start as f64);
        let nw = (d.length as f64 * roll.ppt.get()) as f32;
        let a = 0.35 + 0.65 * d.velocity as f32 / 127.0;
        let col = gdk::RGBA::new(base.red(), base.green(), base.blue(), a);
        rounded(s, x, note_y(d.pitch) + 1.0, nw.max(2.0), ROW - 2.0, 2.0, &col);
        if sel == Some(*n) {
            rect(s, x, note_y(d.pitch) + 1.0, nw.max(2.0), 1.5, &rgba(0xffffff, 1.0));
            rect(s, x, note_y(d.pitch) + ROW - 2.5, nw.max(2.0), 1.5, &rgba(0xffffff, 1.0));
        }
    }
    if ctx.playing() {
        let pos = ctx.position();
        if pos >= r.start && pos < r.end() {
            let x = tick_x(roll, time.samples_to_ticks(pos - r.start) as f64);
            rect(s, x, 0.0, 1.0, h, &fg);
        }
    }
}

fn find_note(ctx: &Ctx, roll: &Roll, x: f64, y: f64) -> Option<(Note, bool)> {
    let (r, _) = region(ctx)?;
    let Content::Midi(m) = &r.content else { return None };
    let pitch = (127 - (y / ROW as f64) as i32).clamp(0, 127) as u8;
    let tick = roll.t0.get() + x / roll.ppt.get();
    let edge = EDGE / roll.ppt.get();
    m.notes.iter().rev().find(|n| n.pitch == pitch && tick >= n.start as f64 && tick < (n.start + n.length) as f64 + edge).map(|n| {
        let near_end = tick > (n.start + n.length) as f64 - edge && n.length as f64 > edge * 2.0;
        (*n, near_end)
    })
}

fn install_roll_input(ctx: &Ctx, roll: &Rc<Roll>, grid: &Canvas) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        g.connect_drag_begin(move |g, x, y| {
            if let Some(w) = g.widget() {
                w.grab_focus();
            }
            roll.offset.set((0.0, 0.0));
            match find_note(&ctx, &roll, x, y) {
                Some((n, edge)) => {
                    roll.selected.set(Some(n));
                    roll.drag.set(if edge { Drag::Resize(n) } else { Drag::Move(n) });
                    ctx.send(Command::Midi([0x90, n.pitch, n.velocity]));
                    ctx.send(Command::Midi([0x80, n.pitch, 0]));
                }
                None => {
                    roll.selected.set(None);
                    roll.drag.set(Drag::None);
                }
            }
        });
    }
    {
        let (roll, grid) = (roll.clone(), grid.clone());
        g.connect_drag_update(move |_, dx, dy| {
            roll.offset.set((dx, dy));
            grid.queue_draw();
        });
    }
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        g.connect_drag_end(move |g, dx, dy| {
            let time = ctx.s().project.time();
            let moved = dx.abs() > 2.0 || dy.abs() > 2.0;
            let d = roll.drag.get();
            match d {
                Drag::Move(n) | Drag::Resize(n) if moved => {
                    let new = dragged_note(&n, &roll, &time);
                    roll.drag.set(Drag::None);
                    roll.offset.set((0.0, 0.0));
                    roll.selected.set(Some(new));
                    edit_notes(&ctx, if matches!(d, Drag::Move(_)) { "Move Note" } else { "Resize Note" }, |notes| {
                        if let Some(x) = notes.iter_mut().find(|x| **x == n) {
                            *x = new;
                        }
                    });
                }
                Drag::None if !moved => {
                    let Some((sx, sy)) = g.start_point() else { return };
                    let grid = grid_ticks(roll.snap.get(), &time).max(PPQ as u64 / 4);
                    let tick = (roll.t0.get() + sx / roll.ppt.get()).max(0.0) as u64;
                    let Some((r, _)) = region(&ctx) else { return };
                    if tick >= time.samples_to_ticks(r.length) {
                        return;
                    }
                    let pitch = (127 - (sy / ROW as f64) as i32).clamp(0, 127) as u8;
                    let n = Note { start: (tick / grid * grid) as u32, length: grid as u32, pitch, velocity: 100 };
                    roll.selected.set(Some(n));
                    ctx.send(Command::Midi([0x90, pitch, 100]));
                    ctx.send(Command::Midi([0x80, pitch, 0]));
                    edit_notes(&ctx, "Add Note", |notes| notes.push(n));
                }
                _ => {
                    roll.drag.set(Drag::None);
                    roll.offset.set((0.0, 0.0));
                }
            }
        });
    }
    grid.add_controller(g);

    let dbl = gtk::GestureClick::new();
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        dbl.connect_pressed(move |_, n, x, y| {
            if n == 2
                && let Some((note, _)) = find_note(&ctx, &roll, x, y)
            {
                roll.selected.set(None);
                edit_notes(&ctx, "Delete Note", |notes| notes.retain(|n| *n != note));
            }
        });
    }
    grid.add_controller(dbl);

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, roll) = (ctx.clone(), roll.clone());
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(key, gdk::Key::Delete | gdk::Key::BackSpace)
                && let Some(sel) = roll.selected.take()
            {
                edit_notes(&ctx, "Delete Note", |notes| notes.retain(|n| *n != sel));
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
    }
    grid.add_controller(keys);

    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    {
        let (roll, grid2) = (roll.clone(), grid.clone());
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
            grid2.queue_draw();
            glib::Propagation::Stop
        });
    }
    grid.add_controller(sc);
}

fn audio_editor(ctx: &Ctx) -> gtk::Widget {
    let ctx2 = ctx.clone();
    let wave = Canvas::new(move |c, s, w, h| {
        let Some((r, color)) = region(&ctx2) else { return };
        let Content::Audio(a) = &r.content else { return };
        let fg = canvas::fg(c);
        let base = track_color(color);
        rect(s, 0.0, 0.0, w, h, &gdk::RGBA::new(base.red(), base.green(), base.blue(), 0.15));
        rect(s, 0.0, h / 2.0, w, 1.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.2));
        let spp = r.length as f64 / w.max(1.0) as f64;
        let peaks = ctx2.m().peaks_for(&a.file);
        if let Some(p) = peaks {
            let gain = db_to_gain(a.gain_db);
            let cols: Vec<(f32, f32)> =
                p.columns(a.offset as f64, spp, w as usize).into_iter().map(|(lo, hi)| (lo * gain, hi * gain)).collect();
            canvas::waveform(s, 0.0, 8.0, h - 16.0, &cols, &base);
        }
        let fi = (a.fade_in as f64 / spp) as f32;
        let fo = (a.fade_out as f64 / spp) as f32;
        rect(s, 0.0, 0.0, fi, h, &rgba(0x000000, 0.2));
        rect(s, w - fo, 0.0, fo, h, &rgba(0x000000, 0.2));
        let time = ctx2.s().project.time();
        text(s, c, 8.0, 4.0, &format!("{} · {:.2} s", r.name, time.seconds(r.length)), &fg, Some(9.0));
        let pos = ctx2.position();
        if pos >= r.start && pos < r.end() {
            rect(s, ((pos - r.start) as f64 / spp) as f32, 0.0, 1.0, h, &fg);
        }
    });
    wave.set_vexpand(true);
    wave.set_hexpand(true);
    {
        let w2 = wave.clone();
        ctx.on(move |_| w2.queue_draw());
    }
    {
        let ctx = ctx.clone();
        wave.add_tick_callback(move |w, _| {
            if ctx.playing() {
                w.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }
    wave.upcast()
}
