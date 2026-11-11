use crate::canvas::{self, Canvas, rect, rgba, rounded, text};
use crate::studio::Ctx;
use gtk::prelude::*;
use gtk::{gdk, graphene, gsk, glib};
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

fn alt(g: &impl IsA<gtk::EventController>) -> bool {
    g.current_event_state().contains(gdk::ModifierType::ALT_MASK)
}

fn shift(g: &impl IsA<gtk::EventController>) -> bool {
    g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK)
}

fn ctrl(g: &impl IsA<gtk::EventController>) -> bool {
    g.current_event_state().contains(gdk::ModifierType::CONTROL_MASK)
}

/// Finds the widget under a point given in `from`'s coordinates whose name starts with `prefix`,
/// walking up from the picked widget. Used to follow a drag across sibling controls.
fn named_under(from: &impl IsA<gtk::Widget>, x: f64, y: f64, prefix: &str) -> Option<gtk::Widget> {
    let root = from.root()?;
    let p = from.compute_point(&root, &graphene::Point::new(x as f32, y as f32))?;
    let mut w = root.pick(p.x() as f64, p.y() as f64, gtk::PickFlags::DEFAULT);
    while let Some(cur) = w {
        if cur.widget_name().starts_with(prefix) {
            return Some(cur);
        }
        w = cur.parent();
    }
    None
}

fn id_key(id: Option<Id>) -> String {
    id.map_or("m".into(), |i| i.to_string())
}

fn parse_id(s: &str) -> Option<Option<Id>> {
    if s == "m" { Some(None) } else { s.parse().ok().map(Some) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paint {
    Mute,
    Solo,
    Arm,
    Monitor,
}

impl Paint {
    fn key(self) -> &'static str {
        match self {
            Paint::Mute => "mute",
            Paint::Solo => "solo",
            Paint::Arm => "arm",
            Paint::Monitor => "monitor",
        }
    }

    fn letter(self) -> &'static str {
        match self {
            Paint::Mute => "M",
            Paint::Solo => "S",
            Paint::Arm => "R",
            Paint::Monitor => "I",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Paint::Mute => "Mute",
            Paint::Solo => "Solo",
            Paint::Arm => "Record Enable",
            Paint::Monitor => "Input Monitoring",
        }
    }

    pub fn get(self, p: &Project, id: Option<Id>) -> bool {
        match self {
            Paint::Mute => p.channel(id).is_some_and(|c| c.mute),
            Paint::Solo => p.channel(id).is_some_and(|c| c.solo),
            Paint::Arm => id.and_then(|i| p.track(i)).is_some_and(|t| t.armed),
            Paint::Monitor => id.and_then(|i| p.track(i)).is_some_and(|t| t.monitor),
        }
    }

    fn set(self, p: &mut Project, id: Option<Id>, v: bool) {
        match self {
            Paint::Mute => {
                if let Some(c) = p.channel_mut(id) {
                    c.mute = v;
                }
            }
            Paint::Solo => {
                if let Some(c) = p.channel_mut(id).filter(|_| id.is_some()) {
                    c.solo = v;
                }
            }
            Paint::Arm => {
                if let Some(t) = id.and_then(|i| p.track_mut(i)) {
                    t.armed = v;
                }
            }
            Paint::Monitor => {
                if let Some(t) = id.and_then(|i| p.track_mut(i)).filter(|t| t.kind == TrackKind::Audio) {
                    t.monitor = v;
                }
            }
        }
    }

    /// Mute only touches the engine's atomics; the others change the graph.
    fn needs_rebuild(self) -> bool {
        self != Paint::Mute
    }
}

/// An M/S/R/I button. Press and drag across others of the same kind to paint the new state
/// onto each, as in Logic. Alt-click applies to every track; Ctrl-click Solo sets solo-safe.
pub fn paint_button(ctx: &Ctx, kind: Paint, id: Option<Id>) -> gtk::Label {
    let (on, safe) = {
        let p = &ctx.s().project;
        (kind.get(p, id), p.channel(id).is_some_and(|c| c.solo_safe))
    };
    let b = gtk::Label::new(Some(kind.letter()));
    b.set_widget_name(&format!("paint:{}:{}", kind.key(), id_key(id)));
    b.add_css_class("track-toggle");
    b.add_css_class(kind.key());
    b.set_tooltip_text(Some(kind.label()));
    b.set_valign(gtk::Align::Center);
    if on {
        b.add_css_class("on");
    }
    if kind == Paint::Solo && safe {
        b.add_css_class("safe");
    }

    let value = Rc::new(Cell::new(None::<bool>));
    let visited: Rc<RefCell<HashSet<Option<Id>>>> = Rc::default();
    let g = gtk::GestureDrag::new();
    {
        let (ctx, value, visited, b2) = (ctx.clone(), value.clone(), visited.clone(), b.clone());
        g.connect_drag_begin(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            value.set(None);
            if kind == Paint::Solo && ctrl(g) && id.is_some() {
                ctx.edit("Solo Safe", |p| {
                    let c = p.channel_mut(id).unwrap();
                    c.solo_safe = !c.solo_safe;
                });
                return;
            }
            let new = !kind.get(&ctx.s().project, id);
            ctx.begin_gesture();
            if alt(g) && matches!(kind, Paint::Mute | Paint::Solo) {
                ctx.live(kind.needs_rebuild(), |p| {
                    let ids: Vec<Id> = p.tracks.iter().map(|t| t.id).collect();
                    for t in ids {
                        kind.set(p, Some(t), new);
                    }
                });
                ctx.end_gesture(kind.label());
                return;
            }
            let gang = ctx.s().gang(id);
            ctx.live(kind.needs_rebuild(), |p| {
                for t in &gang {
                    kind.set(p, *t, new);
                }
            });
            visited.borrow_mut().clear();
            visited.borrow_mut().extend(gang);
            set_on(&b2, new);
            value.set(Some(new));
        });
    }
    {
        let (ctx, value, visited, b2) = (ctx.clone(), value.clone(), visited.clone(), b.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some(new) = value.get() else { return };
            let Some((sx, sy)) = g.start_point() else { return };
            let prefix = format!("paint:{}:", kind.key());
            let Some(w) = named_under(&b2, sx + dx, sy + dy, &prefix) else { return };
            let Some(target) = parse_id(&w.widget_name()[prefix.len()..]) else { return };
            if !visited.borrow_mut().insert(target) {
                return;
            }
            ctx.live(kind.needs_rebuild(), |p| kind.set(p, target, new));
            if let Ok(l) = w.downcast::<gtk::Label>() {
                set_on(&l, new);
            }
        });
    }
    {
        let ctx = ctx.clone();
        g.connect_drag_end(move |_, _, _| {
            if value.take().is_some() {
                ctx.end_gesture(kind.label());
            }
        });
    }
    b.add_controller(g);
    b
}

fn set_on(l: &gtk::Label, on: bool) {
    if on {
        l.add_css_class("on");
    } else {
        l.remove_css_class("on");
    }
}

pub fn db_to_pos(db: f32) -> f32 {
    if db <= -96.0 { 0.0 } else { (db_to_gain(db) / 2.0).powf(0.25).clamp(0.0, 1.0) }
}

pub fn pos_to_db(pos: f32) -> f32 {
    if pos <= 0.002 { -96.0 } else { (20.0 * (2.0 * pos.powi(4)).log10()).min(6.0) }
}

pub fn fmt_db(v: f32) -> String {
    if v <= -95.0 { "-∞".into() } else { format!("{v:+.1}") }
}

pub const CAP_H: f32 = 14.0;

/// A Logic-style fader: grab it anywhere and it moves relative to the pointer, never jumping.
/// Shift for fine moves, Alt-click for 0 dB, scroll to nudge. Moving one selected strip moves
/// the whole selection by the same amount.
pub fn fader(ctx: &Ctx, id: Option<Id>, height: i32) -> Canvas {
    let get = move |p: &Project| p.channel(id).map_or(0.0, |c| c.volume_db);
    let ctx2 = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let db = get(&ctx2.s().project);
        let fg = canvas::fg(c);
        let dim = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.35);
        let travel = h - CAP_H;
        for mark in [6.0, 0.0, -6.0, -12.0, -24.0, -40.0] {
            let y = CAP_H / 2.0 + (1.0 - db_to_pos(mark)) * travel;
            rect(s, 0.0, y, if mark == 0.0 { w } else { 5.0 }, 1.0, &dim);
        }
        rounded(s, w / 2.0 - 2.0, CAP_H / 2.0, 4.0, travel, 2.0, &rgba(0x000000, 0.4));
        let y = (1.0 - db_to_pos(db)) * travel;
        rounded(s, w / 2.0 - 2.0, y + CAP_H / 2.0, 4.0, travel - y, 2.0, &canvas::accent(c));
        rounded(s, 2.0, y, w - 4.0, CAP_H, 3.0, &rgba(0xdeddda, 1.0));
        rect(s, 4.0, y + CAP_H / 2.0 - 0.5, w - 8.0, 1.0, &rgba(0x000000, 0.6));
    });
    c.set_size_request(26, height);
    c.set_tooltip_text(Some("Volume"));
    redraw_on_change(ctx, &c, move |p| get(p));

    let starts: Rc<RefCell<Vec<(Option<Id>, f32)>>> = Rc::default();
    let g = gtk::GestureDrag::new();
    {
        let (ctx, starts) = (ctx.clone(), starts.clone());
        g.connect_drag_begin(move |g, _, _| {
            ctx.begin_gesture();
            let gang = ctx.s().gang(id);
            if alt(g) {
                ctx.live(false, |p| {
                    for t in &gang {
                        if let Some(c) = p.channel_mut(*t) {
                            c.volume_db = 0.0;
                        }
                    }
                });
                starts.borrow_mut().clear();
                ctx.end_gesture("Volume");
                return;
            }
            let p = &ctx.s().project;
            *starts.borrow_mut() =
                gang.iter().map(|t| (*t, db_to_pos(p.channel(*t).map_or(0.0, |c| c.volume_db)))).collect();
        });
    }
    {
        let (ctx, starts, c2) = (ctx.clone(), starts.clone(), c.clone());
        g.connect_drag_update(move |g, _, dy| {
            if starts.borrow().is_empty() {
                return;
            }
            let travel = (c2.height() as f32 - CAP_H).max(1.0);
            let delta = -dy as f32 / travel * if shift(g) { 0.15 } else { 1.0 };
            let starts = starts.borrow().clone();
            ctx.live(false, |p| {
                for (t, pos) in starts {
                    if let Some(c) = p.channel_mut(t) {
                        c.volume_db = pos_to_db((pos + delta).clamp(0.0, 1.0));
                    }
                }
            });
        });
    }
    {
        let ctx = ctx.clone();
        g.connect_drag_end(move |_, _, _| {
            if !starts.take().is_empty() {
                ctx.end_gesture("Volume");
            }
        });
    }
    c.add_controller(g);
    scroll_nudge(ctx, &c, "Volume", move |p, t, dir| {
        if let Some(c) = p.channel_mut(t) {
            c.volume_db = (c.volume_db.max(-60.0) + dir * 0.5).clamp(-96.0, 6.0);
        }
    }, id);
    c
}

/// The small horizontal volume slider in a track header. Same behaviour as the fader.
pub fn mini_fader(ctx: &Ctx, id: Option<Id>) -> Canvas {
    let get = move |p: &Project| p.channel(id).map_or(0.0, |c| c.volume_db);
    let ctx2 = ctx.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let pos = db_to_pos(get(&ctx2.s().project));
        let fg = canvas::fg(c);
        let y = h / 2.0;
        rounded(s, 0.0, y - 2.0, w, 4.0, 2.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15));
        let x = 5.0 + pos * (w - 10.0);
        rounded(s, 0.0, y - 2.0, x, 4.0, 2.0, &canvas::accent(c));
        let unity = 5.0 + db_to_pos(0.0) * (w - 10.0);
        rect(s, unity, y - 5.0, 1.0, 10.0, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.4));
        rounded(s, x - 5.0, y - 6.0, 10.0, 12.0, 3.0, &rgba(0xdeddda, 1.0));
    });
    c.set_size_request(70, 18);
    c.set_valign(gtk::Align::Center);
    c.set_tooltip_text(Some("Volume"));
    redraw_on_change(ctx, &c, move |p| get(p));
    let starts: Rc<RefCell<Vec<(Option<Id>, f32)>>> = Rc::default();
    let g = gtk::GestureDrag::new();
    {
        let (ctx, starts) = (ctx.clone(), starts.clone());
        g.connect_drag_begin(move |g, _, _| {
            g.set_state(gtk::EventSequenceState::Claimed);
            ctx.begin_gesture();
            let gang = ctx.s().gang(id);
            if alt(g) {
                ctx.live(false, |p| {
                    for t in &gang {
                        if let Some(c) = p.channel_mut(*t) {
                            c.volume_db = 0.0;
                        }
                    }
                });
                starts.borrow_mut().clear();
                ctx.end_gesture("Volume");
                return;
            }
            let p = &ctx.s().project;
            *starts.borrow_mut() = gang.iter().map(|t| (*t, db_to_pos(p.channel(*t).map_or(0.0, |c| c.volume_db)))).collect();
        });
    }
    {
        let (ctx, starts, c2) = (ctx.clone(), starts.clone(), c.clone());
        g.connect_drag_update(move |g, dx, _| {
            if starts.borrow().is_empty() {
                return;
            }
            let delta = dx as f32 / (c2.width() as f32 - 10.0).max(1.0) * if shift(g) { 0.15 } else { 1.0 };
            let starts = starts.borrow().clone();
            ctx.live(false, |p| {
                for (t, pos) in starts {
                    if let Some(c) = p.channel_mut(t) {
                        c.volume_db = pos_to_db((pos + delta).clamp(0.0, 1.0));
                    }
                }
            });
        });
    }
    {
        let ctx = ctx.clone();
        g.connect_drag_end(move |_, _, _| {
            if !starts.take().is_empty() {
                ctx.end_gesture("Volume");
            }
        });
    }
    c.add_controller(g);
    c
}

/// A compact pan knob for track headers.
pub fn mini_pan(ctx: &Ctx, id: Option<Id>) -> Canvas {
    let k = knob(
        ctx,
        id,
        KnobSpec {
            min: -1.0,
            max: 1.0,
            default: 0.0,
            bipolar: true,
            label: "Pan",
            gang: true,
            rebuild: false,
            compact: true,
            get: Rc::new(|p, id| p.channel(id).map_or(0.0, |c| c.pan)),
            set: Rc::new(|p, id, v| {
                if let Some(c) = p.channel_mut(id) {
                    c.pan = v;
                }
            }),
            fmt: fmt_pan,
        },
    );
    k.set_valign(gtk::Align::Center);
    k
}

fn scroll_nudge(ctx: &Ctx, w: &Canvas, label: &'static str, f: impl Fn(&mut Project, Option<Id>, f32) + 'static, id: Option<Id>) {
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    let ctx = ctx.clone();
    sc.connect_scroll(move |_, _, dy| {
        let dir = if dy < 0.0 { 1.0 } else { -1.0 };
        let gang = ctx.s().gang(id);
        ctx.begin_gesture();
        ctx.live(false, |p| {
            for t in &gang {
                f(p, *t, dir);
            }
        });
        ctx.end_gesture(label);
        glib::Propagation::Stop
    });
    w.add_controller(sc);
}

/// Redraws `w` once per frame only when the value it shows has changed.
pub fn redraw_on_change(ctx: &Ctx, w: &impl IsA<gtk::Widget>, get: impl Fn(&Project) -> f32 + 'static) {
    let ctx = ctx.clone();
    let last = Cell::new(f32::NAN);
    w.add_tick_callback(move |w, _| {
        let v = get(&ctx.s().project);
        if v.to_bits() != last.get().to_bits() {
            last.set(v);
            w.queue_draw();
        }
        glib::ControlFlow::Continue
    });
}

pub struct KnobSpec {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub bipolar: bool,
    pub label: &'static str,
    pub gang: bool,
    /// The value lives in the engine graph rather than the strip atomics.
    pub rebuild: bool,
    /// A small knob with no value text, as beside each send slot.
    pub compact: bool,
    pub get: Rc<dyn Fn(&Project, Option<Id>) -> f32>,
    pub set: Rc<dyn Fn(&mut Project, Option<Id>, f32)>,
    pub fmt: fn(f32) -> String,
}

/// A rotary control dragged vertically, with Shift for fine moves and Alt-click to reset.
pub fn knob(ctx: &Ctx, id: Option<Id>, spec: KnobSpec) -> Canvas {
    let spec = Rc::new(spec);
    let ctx2 = ctx.clone();
    let sp = spec.clone();
    let c = Canvas::new(move |c, s, w, h| {
        let v = (sp.get)(&ctx2.s().project, id);
        let frac = ((v - sp.min) / (sp.max - sp.min)).clamp(0.0, 1.0);
        let r = if sp.compact { w.min(h) / 2.0 - 2.0 } else { (w / 2.0 - 3.0).min(13.0) };
        let (cx, cy) = if sp.compact { (w / 2.0, h / 2.0) } else { (w / 2.0, r + 3.0) };
        let a0 = std::f32::consts::PI * 0.75;
        let sweep = std::f32::consts::PI * 1.5;
        let arc = |from: f32, to: f32| {
            let b = gsk::PathBuilder::new();
            let steps = 24;
            for k in 0..=steps {
                let a = from + (to - from) * k as f32 / steps as f32;
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                if k == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
            }
            b.to_path()
        };
        let stroke = gsk::Stroke::new(if sp.compact { 2.0 } else { 3.0 });
        let fg = canvas::fg(c);
        s.append_stroke(&arc(a0, a0 + sweep), &stroke, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.15));
        let origin = if sp.bipolar { 0.5 } else { 0.0 };
        let (f0, f1) = if frac < origin { (frac, origin) } else { (origin, frac) };
        if f1 > f0 {
            s.append_stroke(&arc(a0 + sweep * f0, a0 + sweep * f1), &stroke, &canvas::accent(c));
        }
        let a = a0 + sweep * frac;
        let inset = if sp.compact { 4.0 } else { 6.0 };
        let dot = gsk::PathBuilder::new();
        dot.add_circle(&graphene::Point::new(cx + (r - inset) * a.cos(), cy + (r - inset) * a.sin()), 1.5);
        s.append_fill(&dot.to_path(), gsk::FillRule::Winding, &fg);
        if sp.compact {
            return;
        }
        let label = (sp.fmt)(v);
        let layout = c.create_pango_layout(Some(&label));
        let tw = layout.pixel_size().0 as f32 * 0.8;
        text(s, c, (w - tw) / 2.0, cy + r - 2.0, &label, &gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.7), Some(7.0));
    });
    if spec.compact {
        c.set_size_request(22, 22);
    } else {
        c.set_size_request(40, 44);
    }
    c.set_tooltip_text(Some(spec.label));
    {
        let sp = spec.clone();
        redraw_on_change(ctx, &c, move |p| (sp.get)(p, id));
    }

    let starts: Rc<RefCell<Vec<(Option<Id>, f32)>>> = Rc::default();
    let g = gtk::GestureDrag::new();
    {
        let (ctx, starts, sp) = (ctx.clone(), starts.clone(), spec.clone());
        g.connect_drag_begin(move |g, _, _| {
            ctx.begin_gesture();
            let targets = if sp.gang { ctx.s().gang(id) } else { vec![id] };
            if alt(g) {
                ctx.live(sp.rebuild, |p| {
                    for t in &targets {
                        (sp.set)(p, *t, sp.default);
                    }
                });
                starts.borrow_mut().clear();
                ctx.end_gesture(sp.label);
                return;
            }
            let p = &ctx.s().project;
            *starts.borrow_mut() = targets.iter().map(|t| (*t, (sp.get)(p, *t))).collect();
        });
    }
    {
        let (ctx, starts, sp) = (ctx.clone(), starts.clone(), spec.clone());
        g.connect_drag_update(move |g, _, dy| {
            let delta = -dy as f32 / 150.0 * (sp.max - sp.min) * if shift(g) { 0.1 } else { 1.0 };
            let starts = starts.borrow().clone();
            ctx.live(sp.rebuild, |p| {
                for (t, v) in starts {
                    (sp.set)(p, t, (v + delta).clamp(sp.min, sp.max));
                }
            });
        });
    }
    {
        let (ctx, sp) = (ctx.clone(), spec.clone());
        g.connect_drag_end(move |_, _, _| {
            if !starts.take().is_empty() {
                ctx.end_gesture(sp.label);
            }
        });
    }
    c.add_controller(g);
    let dbl = gtk::GestureClick::new();
    {
        let (ctx, sp) = (ctx.clone(), spec.clone());
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                ctx.begin_gesture();
                ctx.live(sp.rebuild, |p| (sp.set)(p, id, sp.default));
                ctx.end_gesture(sp.label);
            }
        });
    }
    c.add_controller(dbl);
    let sp = spec.clone();
    let step = (spec.max - spec.min) / 100.0;
    scroll_nudge(ctx, &c, spec.label, move |p, t, dir| {
        let v = (sp.get)(p, t);
        (sp.set)(p, t, (v + dir * step).clamp(sp.min, sp.max));
    }, id);
    c
}

pub fn fmt_pan(v: f32) -> String {
    let n = (v * 64.0).round() as i32;
    match n {
        0 => "C".into(),
        n if n < 0 => format!("L{}", -n),
        n => format!("R{n}"),
    }
}

pub fn pan_knob(ctx: &Ctx, id: Option<Id>) -> Canvas {
    knob(
        ctx,
        id,
        KnobSpec {
            min: -1.0,
            max: 1.0,
            default: 0.0,
            bipolar: true,
            label: "Pan",
            gang: true,
            rebuild: false,
            compact: false,
            get: Rc::new(|p, id| p.channel(id).map_or(0.0, |c| c.pan)),
            set: Rc::new(|p, id, v| {
                if let Some(c) = p.channel_mut(id) {
                    c.pan = v;
                }
            }),
            fmt: fmt_pan,
        },
    )
}

/// Channel order used for drag-selecting a range of names: tracks, then buses.
fn channel_order(p: &Project) -> Vec<Id> {
    p.tracks.iter().map(|t| t.id).chain(p.buses.iter().map(|b| b.id)).collect()
}

/// A strip or header name. Click selects, Shift-click adds or removes, dragging across names
/// selects the range, and double-click renames.
pub fn select_name(ctx: &Ctx, w: &impl IsA<gtk::Widget>, id: Id) {
    w.set_widget_name(&format!("name:{id}"));
    let g = gtk::GestureDrag::new();
    let anchor = Rc::new(Cell::new(None::<Id>));
    {
        let (ctx, anchor) = (ctx.clone(), anchor.clone());
        g.connect_drag_begin(move |g, _, _| {
            if shift(g) {
                ctx.toggle_selected(id);
                anchor.set(None);
            } else {
                anchor.set(Some(id));
                if !ctx.s().selection.contains(&id) || ctx.s().selection.len() > 1 {
                    ctx.select_track(Some(id));
                }
            }
        });
    }
    {
        let (ctx, anchor, w2) = (ctx.clone(), anchor.clone(), w.clone().upcast::<gtk::Widget>());
        g.connect_drag_update(move |g, dx, dy| {
            let Some(a) = anchor.get() else { return };
            let Some((sx, sy)) = g.start_point() else { return };
            if dx.abs() < 3.0 && dy.abs() < 3.0 {
                return;
            }
            let Some(over) = named_under(&w2, sx + dx, sy + dy, "name:") else { return };
            let Ok(b) = over.widget_name()["name:".len()..].parse::<Id>() else { return };
            let order = channel_order(&ctx.s().project);
            let (Some(i), Some(j)) = (order.iter().position(|x| *x == a), order.iter().position(|x| *x == b)) else { return };
            let (lo, hi) = (i.min(j), i.max(j));
            ctx.set_selection(order[lo..=hi].to_vec());
        });
    }
    w.add_controller(g);
    let dbl = gtk::GestureClick::new();
    {
        let (ctx, w2) = (ctx.clone(), w.clone().upcast::<gtk::Widget>());
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                rename_popover(&ctx, &w2, id);
            }
        });
    }
    w.add_controller(dbl);
}

fn rename_popover(ctx: &Ctx, anchor: &gtk::Widget, id: Id) {
    let current = {
        let p = &ctx.s().project;
        p.track(id).map(|t| t.name.clone()).or_else(|| p.buses.iter().find(|b| b.id == id).map(|b| b.name.clone()))
    };
    let Some(current) = current else { return };
    let pop = gtk::Popover::new();
    pop.set_parent(anchor);
    let entry = gtk::Entry::builder().text(&current).activates_default(true).build();
    pop.set_child(Some(&entry));
    let ctx = ctx.clone();
    let p2 = pop.clone();
    entry.connect_activate(move |e| {
        let name = e.text().to_string();
        if !name.is_empty() && name != current {
            ctx.edit("Rename", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.name = name.clone();
                } else if let Some(b) = p.buses.iter_mut().find(|b| b.id == id) {
                    b.name = name.clone();
                }
            });
        }
        p2.popdown();
    });
    pop.connect_closed(|p| p.unparent());
    pop.popup();
    entry.grab_focus();
}

/// Keeps the "selected" style on widgets named `<prefix><id>` in line with the selection,
/// without rebuilding them (rebuilding would cancel a drag in progress).
pub fn sync_selection_classes(ctx: &Ctx, container: &impl IsA<gtk::Widget>, prefix: &str) {
    let sel = ctx.s().selection.clone();
    let mut stack = vec![container.clone().upcast::<gtk::Widget>()];
    while let Some(w) = stack.pop() {
        let name = w.widget_name();
        if let Some(rest) = name.strip_prefix(prefix) {
            match rest.parse::<Id>() {
                Ok(id) if sel.contains(&id) => w.add_css_class("selected"),
                _ => w.remove_css_class("selected"),
            }
            continue;
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            stack.push(ch.clone());
            c = ch.next_sibling();
        }
    }
}
