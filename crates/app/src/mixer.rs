use crate::canvas::{Canvas, rect, rgba, track_color};
use crate::gestures::{self, KnobSpec, Paint, fmt_db, paint_button};
use crate::plugins;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, glib};
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

const FADER_H: i32 = 190;

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    Single,
    Tracks,
    All,
}

const AUDIO: u8 = 1;
const INST: u8 = 2;
const AUX: u8 = 4;
const OUTPUT: u8 = 8;

struct View {
    filter: Cell<Filter>,
    types: Cell<u8>,
    wide: Cell<bool>,
}

/// A mixer: used docked under the tracks area and in its own window. Both are views of the
/// same project, so they stay in step.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let view = Rc::new(View { filter: Cell::new(Filter::Tracks), types: Cell::new(0xff), wide: Cell::new(false) });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.add_css_class("mixer");
    row.set_margin_start(4);
    row.set_margin_end(4);
    row.set_margin_bottom(4);
    let scroller = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&row)
        .build();

    let refill: Rc<dyn Fn()> = {
        let (ctx, row, view) = (ctx.clone(), row.clone(), view.clone());
        Rc::new(move || fill(&ctx, &row, &view))
    };

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("toolbar");
    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    filters.add_css_class("linked");
    let mut group: Option<gtk::ToggleButton> = None;
    for (label, f) in [("Single", Filter::Single), ("Tracks", Filter::Tracks), ("All", Filter::All)] {
        let b = gtk::ToggleButton::with_label(label);
        if let Some(g) = &group {
            b.set_group(Some(g));
        } else {
            group = Some(b.clone());
        }
        b.set_active(f == Filter::Tracks);
        let (view, refill) = (view.clone(), refill.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                view.filter.set(f);
                refill();
            }
        });
        filters.append(&b);
    }
    bar.append(&filters);
    let types = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    types.add_css_class("linked");
    for (label, bit) in [("Audio", AUDIO), ("Inst", INST), ("Aux", AUX), ("Output", OUTPUT)] {
        let b = gtk::ToggleButton::with_label(label);
        b.set_active(true);
        let (view, refill) = (view.clone(), refill.clone());
        b.connect_toggled(move |b| {
            let t = view.types.get();
            view.types.set(if b.is_active() { t | bit } else { t & !bit });
            refill();
        });
        types.append(&b);
    }
    bar.append(&types);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    let wide = gtk::ToggleButton::with_label("Wide");
    {
        let (view, refill) = (view.clone(), refill.clone());
        wide.connect_toggled(move |b| {
            view.wide.set(b.is_active());
            refill();
        });
    }
    bar.append(&wide);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&bar);
    root.append(&scroller);
    refill();
    {
        let (ctx2, row, view) = (ctx.clone(), row.clone(), view.clone());
        let weak = root.downgrade();
        ctx.on(move |c| {
            // Views of a closed mixer window stop listening once their widgets are gone.
            if weak.upgrade().is_none() {
                return;
            }
            match c {
                Change::Project | Change::Catalog => refill(),
                Change::Selection if view.filter.get() == Filter::Single => refill(),
                Change::Selection => gestures::sync_selection_classes(&ctx2, &row, "strip:"),
                _ => {}
            }
        });
    }
    root.upcast()
}

fn visible(p: &Project, view: &View, selected: Option<Id>) -> (Vec<Id>, Vec<Id>, bool) {
    let types = view.types.get();
    let kind_ok = |t: &Track| match t.kind {
        TrackKind::Audio => types & AUDIO != 0,
        TrackKind::Instrument => types & INST != 0,
    };
    let fed: Vec<Id> = p
        .tracks
        .iter()
        .map(|t| &t.channel)
        .chain(p.buses.iter().map(|b| &b.channel))
        .flat_map(|c| {
            let mut v: Vec<Id> = c.sends.iter().map(|s| s.bus).collect();
            if let Output::Bus(b) = c.output {
                v.push(b);
            }
            v
        })
        .collect();
    let (tracks, buses): (Vec<Id>, Vec<Id>) = match view.filter.get() {
        Filter::Single => {
            let Some(t) = selected.and_then(|id| p.track(id)) else { return (vec![], vec![], true) };
            let mut path = Vec::new();
            let mut frontier: Vec<&Channel> = vec![&t.channel];
            while let Some(c) = frontier.pop() {
                let targets = c.sends.iter().map(|s| s.bus).chain(match c.output {
                    Output::Bus(b) => Some(b),
                    Output::Master => None,
                });
                for b in targets {
                    if !path.contains(&b)
                        && let Some(bus) = p.buses.iter().find(|x| x.id == b)
                    {
                        path.push(b);
                        frontier.push(&bus.channel);
                    }
                }
            }
            (vec![t.id], path)
        }
        Filter::Tracks => (
            p.tracks.iter().filter(|t| kind_ok(t)).map(|t| t.id).collect(),
            p.buses.iter().filter(|b| fed.contains(&b.id)).map(|b| b.id).collect(),
        ),
        Filter::All => (p.tracks.iter().filter(|t| kind_ok(t)).map(|t| t.id).collect(), p.buses.iter().map(|b| b.id).collect()),
    };
    let buses = if types & AUX != 0 { buses } else { Vec::new() };
    (tracks, buses, types & OUTPUT != 0)
}

fn fill(ctx: &Ctx, row: &gtk::Box, view: &View) {
    while let Some(c) = row.first_child() {
        row.remove(&c);
    }
    let (tracks, buses, output) = {
        let s = ctx.s();
        visible(&s.project, view, s.selected_track)
    };
    let wide = view.wide.get();
    for id in tracks.into_iter().chain(buses) {
        row.append(&strip(ctx, Some(id), wide));
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    row.append(&spacer);
    if output {
        row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        row.append(&strip(ctx, None, wide));
    }
}

struct Info {
    name: String,
    color: Option<u8>,
    kind: Option<TrackKind>,
    channel: Channel,
    instrument: Option<PluginSlot>,
    selected: bool,
    number: Option<usize>,
    is_bus: bool,
}

fn info(ctx: &Ctx, id: Option<Id>) -> Option<Info> {
    let s = ctx.s();
    let p = &s.project;
    let selected = id.is_some_and(|i| s.selection.contains(&i));
    match id {
        None => Some(Info {
            name: "Stereo Out".into(),
            color: None,
            kind: None,
            channel: p.master.clone(),
            instrument: None,
            selected,
            number: None,
            is_bus: false,
        }),
        Some(id) => {
            if let Some((n, t)) = p.tracks.iter().enumerate().find(|(_, t)| t.id == id) {
                Some(Info {
                    name: t.name.clone(),
                    color: Some(t.color),
                    kind: Some(t.kind),
                    channel: t.channel.clone(),
                    instrument: t.instrument.clone(),
                    selected,
                    number: Some(n + 1),
                    is_bus: false,
                })
            } else {
                p.buses.iter().find(|b| b.id == id).map(|b| Info {
                    name: b.name.clone(),
                    color: None,
                    kind: None,
                    channel: b.channel.clone(),
                    instrument: None,
                    selected,
                    number: None,
                    is_bus: true,
                })
            }
        }
    }
}

/// A Logic-style channel strip for a track, aux bus or (with `None`) the stereo output.
pub fn strip(ctx: &Ctx, id: Option<Id>, wide: bool) -> gtk::Widget {
    let Some(i) = info(ctx, id) else { return gtk::Box::new(gtk::Orientation::Vertical, 0).upcast() };
    let col = gtk::Box::new(gtk::Orientation::Vertical, 3);
    col.add_css_class("channel-strip");
    col.set_widget_name(&format!("strip:{}", id.map_or("m".into(), |i| i.to_string())));
    if i.selected {
        col.add_css_class("selected");
    }
    col.set_size_request(if wide { 132 } else { 92 }, -1);
    let is_track = i.kind.is_some();

    if i.kind == Some(TrackKind::Instrument) {
        col.append(&section("Instrument"));
        col.append(&instrument_slot(ctx, id.unwrap(), i.instrument.as_ref()));
    }

    col.append(&section("Audio FX"));
    for (k, slot) in i.channel.inserts.iter().enumerate() {
        col.append(&insert_row(ctx, id, k, slot));
    }
    col.append(&empty_slot(ctx, id, i.channel.inserts.len()));

    if id.is_some() {
        col.append(&section("Sends"));
        for (k, send) in i.channel.sends.iter().enumerate() {
            col.append(&send_row(ctx, id.unwrap(), k, send, wide));
        }
        col.append(&add_send_button(ctx, id.unwrap()));
        col.append(&section("Output"));
        col.append(&output_button(ctx, id.unwrap(), i.channel.output));
    }

    let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    col.append(&spacer);

    let pan = gestures::pan_knob(ctx, id);
    pan.set_halign(gtk::Align::Center);
    col.append(&pan);

    let db = gtk::Label::new(Some(&fmt_db(i.channel.volume_db)));
    db.add_css_class("numeric");
    db.add_css_class("caption");
    db.set_tooltip_text(Some("Double-click to type a level"));
    {
        let ctx = ctx.clone();
        db.add_tick_callback(move |l, _| {
            let v = ctx.s().project.channel(id).map_or(0.0, |c| c.volume_db);
            let t = fmt_db(v);
            if l.text() != t {
                l.set_text(&t);
            }
            glib::ControlFlow::Continue
        });
    }
    let dbl = gtk::GestureClick::new();
    {
        let (ctx, db2) = (ctx.clone(), db.clone());
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                type_level(&ctx, &db2, id);
            }
        });
    }
    db.add_controller(dbl);
    col.append(&db);

    let fader_row = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    fader_row.set_halign(gtk::Align::Center);
    fader_row.append(&gestures::fader(ctx, id, FADER_H));
    fader_row.append(&meter(ctx, id));
    col.append(&fader_row);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    buttons.set_halign(gtk::Align::Center);
    buttons.append(&paint_button(ctx, Paint::Mute, id));
    if id.is_some() {
        buttons.append(&paint_button(ctx, Paint::Solo, id));
    }
    if is_track {
        buttons.append(&paint_button(ctx, Paint::Arm, id));
        if i.kind == Some(TrackKind::Audio) {
            buttons.append(&paint_button(ctx, Paint::Monitor, id));
        }
    }
    col.append(&buttons);

    if let Some(c) = i.color {
        let color = track_color(c);
        let bar = Canvas::new(move |_, s, w, h| rect(s, 0.0, 0.0, w, h, &color));
        bar.set_size_request(-1, 3);
        col.append(&bar);
    }
    let name = gtk::Label::new(Some(&i.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("strip-name");
    if i.is_bus {
        name.add_css_class("aux");
    }
    if let Some(id) = id {
        gestures::select_name(ctx, &name, id);
    }
    col.append(&name);
    if let Some(n) = i.number {
        let num = gtk::Label::new(Some(&n.to_string()));
        num.add_css_class("caption");
        num.add_css_class("dim-label");
        col.append(&num);
    }
    col.upcast()
}

fn type_level(ctx: &Ctx, anchor: &gtk::Label, id: Option<Id>) {
    let pop = gtk::Popover::new();
    pop.set_parent(anchor);
    let spin = gtk::SpinButton::with_range(-96.0, 6.0, 0.1);
    spin.set_digits(1);
    spin.set_value(ctx.s().project.channel(id).map_or(0.0, |c| c.volume_db) as f64);
    pop.set_child(Some(&spin));
    let (ctx, p2) = (ctx.clone(), pop.clone());
    spin.connect_activate(move |s| {
        s.update();
        let v = s.value() as f32;
        let gang = ctx.s().gang(id);
        ctx.begin_gesture();
        ctx.live(false, |p| {
            for t in &gang {
                if let Some(c) = p.channel_mut(*t) {
                    c.volume_db = v;
                }
            }
        });
        ctx.end_gesture("Volume");
        p2.popdown();
    });
    pop.connect_closed(|p| p.unparent());
    pop.popup();
}

fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.add_css_class("caption-heading");
    l.add_css_class("dim-label");
    l.set_xalign(0.0);
    l
}

/// Meter with peak hold and a clip light; click clears the clip light, Alt-click clears all.
fn meter(ctx: &Ctx, id: Option<Id>) -> Canvas {
    let ctx2 = ctx.clone();
    let m = Canvas::new(move |_, s, w, h| {
        let m = ctx2.s().meters.get(&id).copied().unwrap_or_default();
        let top = 6.0;
        let mh = h - top;
        rect(s, 0.0, 0.0, w, 4.0, &if m.clip { rgba(0xe01b24, 1.0) } else { rgba(0x000000, 0.35) });
        let bw = (w - 1.0) / 2.0;
        rect(s, 0.0, top, w, mh, &rgba(0x000000, 0.35));
        let y_of = |lv: f32| {
            let pos = gestures::db_to_pos(20.0 * lv.max(1e-6).log10());
            top + (1.0 - pos) * mh
        };
        for k in 0..2 {
            let y = y_of(m.level[k]);
            let x = k as f32 * (bw + 1.0);
            let db = 20.0 * m.level[k].max(1e-6).log10();
            let col = if db > 0.0 {
                rgba(0xe01b24, 1.0)
            } else if db > -6.0 {
                rgba(0xf6d32d, 1.0)
            } else {
                rgba(0x33d17a, 1.0)
            };
            rect(s, x, y, bw, top + mh - y, &col);
            if m.hold[k] > 1e-4 {
                rect(s, x, y_of(m.hold[k]), bw, 1.5, &rgba(0xffffff, 0.85));
            }
        }
        rect(s, 0.0, y_of(1.0), w, 1.0, &rgba(0xffffff, 0.4));
    });
    m.set_size_request(10, FADER_H);
    m.set_tooltip_text(Some("Click to clear the clip light"));
    m.add_tick_callback(|m, _| {
        m.queue_draw();
        glib::ControlFlow::Continue
    });
    let click = gtk::GestureClick::new();
    {
        let ctx = ctx.clone();
        click.connect_pressed(move |g, _, _, _| {
            let all = g.current_event_state().contains(gdk::ModifierType::ALT_MASK);
            let mut s = ctx.m();
            for (k, m) in s.meters.iter_mut() {
                if all || *k == id {
                    m.clip = false;
                    m.hold = [0.0; 2];
                }
            }
        });
    }
    m.add_controller(click);
    m
}

// Plug-in drag and drop payloads: "slot:<owner>:<slot id>", "inst:<track>:<slot id>" or
// "lib:<class uid>", where <owner> is a channel id or "m" for the stereo output.
fn owner_key(id: Option<Id>) -> String {
    id.map_or("m".into(), |i| i.to_string())
}

enum Source {
    Insert { owner: Option<Id>, slot: Id },
    Instrument { track: Id, slot: Id },
    Library(String),
}

fn parse_payload(s: &str) -> Option<Source> {
    let mut parts = s.splitn(3, ':');
    let kind = parts.next()?;
    match kind {
        "lib" => Some(Source::Library(parts.next()?.to_string())),
        "slot" | "inst" => {
            let owner = parts.next()?;
            let slot: Id = parts.next()?.parse().ok()?;
            let owner = if owner == "m" { None } else { Some(owner.parse().ok()?) };
            if kind == "slot" {
                Some(Source::Insert { owner, slot })
            } else {
                Some(Source::Instrument { track: owner?, slot })
            }
        }
        _ => None,
    }
}

fn drag_source(w: &impl IsA<gtk::Widget>, payload: String) {
    let src = gtk::DragSource::new();
    src.set_actions(gdk::DragAction::MOVE | gdk::DragAction::COPY);
    src.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&payload.to_value())));
    w.add_controller(src);
}

/// Resolves a payload into a slot for the target. Moves take the slot out of its source so the
/// running instance and its settings come along; copies and library drops get a new slot.
fn take_for_drop(ctx: &Ctx, p: &mut Project, src: &Source, copy: bool, states: &Option<Vec<u8>>) -> Option<PluginSlot> {
    match src {
        Source::Library(uid) => {
            let info = ctx.s().catalog.iter().find(|c| &c.uid == uid).cloned()?;
            Some(plugins::new_slot(p, &info))
        }
        Source::Insert { owner, slot } => {
            let ch = p.channel_mut(*owner)?;
            let i = ch.inserts.iter().position(|s| s.id == *slot)?;
            if copy {
                let mut s = ch.inserts[i].clone();
                s.id = p.alloc_id();
                s.state = states.clone().unwrap_or_default();
                Some(s)
            } else {
                Some(ch.inserts.remove(i))
            }
        }
        Source::Instrument { track, slot } => {
            let t = p.track_mut(*track)?;
            if t.instrument.as_ref().is_none_or(|s| s.id != *slot) {
                return None;
            }
            if copy {
                let mut s = t.instrument.clone().unwrap();
                s.id = p.alloc_id();
                s.state = states.clone().unwrap_or_default();
                Some(s)
            } else {
                t.instrument.take()
            }
        }
    }
}

fn source_state(ctx: &Ctx, src: &Source) -> Option<Vec<u8>> {
    let slot = match src {
        Source::Insert { slot, .. } | Source::Instrument { slot, .. } => *slot,
        Source::Library(_) => return None,
    };
    ctx.s().plugins.get(&slot).map(|l| l.inst.state())
}

fn is_instrument(ctx: &Ctx, src: &Source) -> bool {
    match src {
        Source::Library(uid) => ctx.s().catalog.iter().any(|c| &c.uid == uid && c.is_instrument()),
        Source::Instrument { .. } => true,
        Source::Insert { .. } => false,
    }
}

/// Drop onto insert position `index` of `owner`. Within one strip this reorders; from elsewhere
/// it replaces the plug-in at `index` (or appends when `index` is the empty slot).
fn drop_insert(ctx: &Ctx, value: &glib::Value, owner: Option<Id>, index: usize, copy: bool) -> bool {
    let Ok(payload) = value.get::<String>() else { return false };
    let Some(src) = parse_payload(&payload) else { return false };
    if is_instrument(ctx, &src) {
        return false;
    }
    let same_strip = matches!(src, Source::Insert { owner: o, .. } if o == owner);
    let states = if copy { source_state(ctx, &src) } else { None };
    let label = if copy { "Copy Plug-in" } else if matches!(src, Source::Library(_)) { "Add Plug-in" } else { "Move Plug-in" };
    let ctx2 = ctx.clone();
    ctx.edit(label, move |p| {
        let from = match &src {
            Source::Insert { owner: o, slot } if *o == owner => p.channel(owner).and_then(|c| c.inserts.iter().position(|s| s.id == *slot)),
            _ => None,
        };
        let Some(slot) = take_for_drop(&ctx2, p, &src, copy, &states) else { return };
        let Some(ch) = p.channel_mut(owner) else { return };
        let mut at = index.min(ch.inserts.len());
        if same_strip && !copy {
            if let Some(f) = from
                && f < at
            {
                at -= 1;
            }
            ch.inserts.insert(at.min(ch.inserts.len()), slot);
        } else if at < ch.inserts.len() {
            ch.inserts[at] = slot;
        } else {
            ch.inserts.push(slot);
        }
    });
    true
}

fn drop_target(ctx: &Ctx, w: &impl IsA<gtk::Widget>, on_drop: impl Fn(&Ctx, &glib::Value, bool) -> bool + 'static) {
    let t = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE | gdk::DragAction::COPY);
    let ctx = ctx.clone();
    let w2 = w.clone().upcast::<gtk::Widget>();
    t.connect_enter(move |_, _, _| {
        w2.add_css_class("drop-hover");
        gdk::DragAction::MOVE
    });
    let w3 = w.clone().upcast::<gtk::Widget>();
    t.connect_leave(move |_| w3.remove_css_class("drop-hover"));
    t.connect_drop(move |t, value, _, _| {
        // Alt held at drop copies, as in Logic.
        let copy = t.current_event_state().contains(gdk::ModifierType::ALT_MASK);
        on_drop(&ctx, value, copy)
    });
    w.add_controller(t);
}

fn insert_row(ctx: &Ctx, owner: Option<Id>, index: usize, slot: &PluginSlot) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("linked");
    row.add_css_class("insert-row");
    if slot.bypass {
        row.add_css_class("bypassed");
    }
    let sid = slot.id;
    let power = gtk::ToggleButton::new();
    power.set_icon_name("system-shutdown-symbolic");
    power.set_active(!slot.bypass);
    power.set_tooltip_text(Some("Bypass"));
    power.add_css_class("insert-power");
    {
        let ctx = ctx.clone();
        power.connect_toggled(move |b| {
            let bypass = !b.is_active();
            ctx.edit("Bypass Plug-in", |p| set_bypass(p, owner, sid, Some(bypass)));
        });
    }
    let errors = ctx.s().plugin_errors.get(&sid).cloned();
    let open = gtk::Button::with_label(&slot.name);
    open.set_hexpand(true);
    open.add_css_class("insert-slot");
    if let Some(e) = errors {
        open.add_css_class("error");
        open.set_tooltip_text(Some(&e));
    } else {
        open.set_tooltip_text(Some("Click to open, Alt-click to bypass, drag to move, Alt-drag to copy"));
    }
    if let Some(l) = open.child().and_downcast::<gtk::Label>() {
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(8);
    }
    {
        let ctx = ctx.clone();
        let alt_click = gtk::GestureClick::new();
        alt_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let ctx2 = ctx.clone();
        alt_click.connect_pressed(move |g, _, _, _| {
            if g.current_event_state().contains(gdk::ModifierType::ALT_MASK) {
                g.set_state(gtk::EventSequenceState::Claimed);
                ctx2.edit("Bypass Plug-in", |p| set_bypass(p, owner, sid, None));
            }
        });
        open.add_controller(alt_click);
        open.connect_clicked(move |b| plugins::open_editor(&ctx, sid, b));
    }
    drag_source(&open, format!("slot:{}:{sid}", owner_key(owner)));
    drop_target(ctx, &row, move |ctx, v, copy| drop_insert(ctx, v, owner, index, copy));

    let remove = gtk::Button::from_icon_name("window-close-symbolic");
    remove.set_tooltip_text(Some("Remove"));
    {
        let ctx = ctx.clone();
        remove.connect_clicked(move |_| {
            ctx.edit("Remove Plug-in", |p| p.channel_mut(owner).unwrap().inserts.retain(|s| s.id != sid));
        });
    }
    row.append(&power);
    row.append(&open);
    row.append(&remove);
    row.upcast()
}

fn set_bypass(p: &mut Project, owner: Option<Id>, sid: Id, value: Option<bool>) {
    if let Some(s) = p.channel_mut(owner).and_then(|c| c.inserts.iter_mut().find(|s| s.id == sid)) {
        s.bypass = value.unwrap_or(!s.bypass);
    }
}

/// The always-present empty slot below the last insert: click to pick, or drop a plug-in on it.
fn empty_slot(ctx: &Ctx, owner: Option<Id>, index: usize) -> gtk::Widget {
    let b = plugins::picker_button(ctx, false, "Add Audio Effect", {
        let ctx = ctx.clone();
        move |info| {
            let info = info.clone();
            ctx.edit("Add Plug-in", |p| {
                let slot = plugins::new_slot(p, &info);
                p.channel_mut(owner).unwrap().inserts.push(slot);
            });
        }
    });
    b.add_css_class("empty-slot");
    b.set_hexpand(true);
    drop_target(ctx, &b, move |ctx, v, copy| drop_insert(ctx, v, owner, index, copy));
    b.upcast()
}

fn instrument_slot(ctx: &Ctx, track: Id, slot: Option<&PluginSlot>) -> gtk::Widget {
    let set = {
        let ctx = ctx.clone();
        move |info: &solstice_vst3::ClassInfo| {
            let info = info.clone();
            ctx.edit("Set Instrument", |p| {
                let slot = plugins::new_slot(p, &info);
                p.track_mut(track).unwrap().instrument = Some(slot);
            });
        }
    };
    let w: gtk::Widget = match slot {
        None => {
            let b = plugins::picker_button(ctx, true, "Choose Instrument", set);
            b.add_css_class("empty-slot");
            b.upcast()
        }
        Some(slot) => {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            row.add_css_class("linked");
            let open = gtk::Button::with_label(&slot.name);
            open.set_hexpand(true);
            open.add_css_class("insert-slot");
            if let Some(l) = open.child().and_downcast::<gtk::Label>() {
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_max_width_chars(8);
            }
            let sid = slot.id;
            {
                let ctx = ctx.clone();
                open.connect_clicked(move |b| plugins::open_editor(&ctx, sid, b));
            }
            drag_source(&open, format!("inst:{track}:{sid}"));
            let remove = gtk::Button::from_icon_name("window-close-symbolic");
            {
                let ctx = ctx.clone();
                remove.connect_clicked(move |_| ctx.edit("Remove Instrument", |p| p.track_mut(track).unwrap().instrument = None));
            }
            row.append(&open);
            row.append(&remove);
            row.upcast()
        }
    };
    drop_target(ctx, &w, move |ctx, value, copy| {
        let Ok(payload) = value.get::<String>() else { return false };
        let Some(src) = parse_payload(&payload) else { return false };
        if !is_instrument(ctx, &src) || matches!(src, Source::Instrument { track: t, .. } if t == track) {
            return false;
        }
        let states = if copy { source_state(ctx, &src) } else { None };
        let ctx2 = ctx.clone();
        ctx.edit(if copy { "Copy Instrument" } else { "Set Instrument" }, move |p| {
            if let Some(slot) = take_for_drop(&ctx2, p, &src, copy, &states) {
                p.track_mut(track).unwrap().instrument = Some(slot);
            }
        });
        true
    });
    w
}

fn send_mode_name(m: SendMode) -> &'static str {
    match m {
        SendMode::PreFader => "Pre",
        SendMode::PostFader => "Post",
        SendMode::PostPan => "Post Pan",
    }
}

fn send_row(ctx: &Ctx, owner: Id, k: usize, send: &Send, wide: bool) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let name = ctx.s().project.buses.iter().find(|b| b.id == send.bus).map_or("?".into(), |b| b.name.clone());
    let menu_button = gtk::MenuButton::builder().label(&name).tooltip_text(send_mode_name(send.mode)).build();
    menu_button.add_css_class("flat");
    menu_button.add_css_class("send-name");
    menu_button.set_hexpand(true);
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for mode in [SendMode::PreFader, SendMode::PostFader, SendMode::PostPan] {
        let b = gtk::CheckButton::with_label(send_mode_name(mode));
        b.set_active(mode == send.mode);
        let (ctx, pop2) = (ctx.clone(), pop.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                pop2.popdown();
                ctx.edit("Send Mode", |p| {
                    if let Some(s) = p.channel_mut(Some(owner)).and_then(|c| c.sends.get_mut(k)) {
                        s.mode = mode;
                    }
                });
            }
        });
        list.append(&b);
    }
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let remove = gtk::Button::with_label("Remove Send");
    remove.add_css_class("flat");
    {
        let (ctx, pop2) = (ctx.clone(), pop.clone());
        remove.connect_clicked(move |_| {
            pop2.popdown();
            ctx.edit("Remove Send", |p| {
                if let Some(c) = p.channel_mut(Some(owner))
                    && k < c.sends.len()
                {
                    c.sends.remove(k);
                }
            });
        });
    }
    list.append(&remove);
    pop.set_child(Some(&list));
    menu_button.set_popover(Some(&pop));
    if let Some(l) = menu_button.child().and_then(|b| b.first_child()).and_downcast::<gtk::Label>() {
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    }

    let level = gestures::knob(
        ctx,
        Some(owner),
        KnobSpec {
            min: -60.0,
            max: 6.0,
            default: 0.0,
            bipolar: false,
            label: "Send Level",
            gang: false,
            rebuild: true,
            get: Rc::new(move |p, id| p.channel(id).and_then(|c| c.sends.get(k)).map_or(0.0, |s| s.level_db)),
            set: Rc::new(move |p, id, v| {
                if let Some(s) = p.channel_mut(id).and_then(|c| c.sends.get_mut(k)) {
                    s.level_db = v;
                }
            }),
            fmt: |v| if v <= -59.9 { "-∞".into() } else { format!("{v:.0}") },
        },
    );
    if !wide {
        level.set_size_request(34, 40);
    }
    row.append(&menu_button);
    row.append(&level);
    row.upcast()
}

/// Creates "Bus N" and returns its id. Logic does this when you route to an unused bus.
fn new_bus(p: &mut Project) -> Id {
    p.add_bus()
}

fn bus_menu(ctx: &Ctx, owner: Id, include_master: bool, on_pick: impl Fn(&Ctx, Option<Id>) + 'static) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let on_pick = Rc::new(on_pick);
    let buses: Vec<(Id, String, bool)> = {
        let p = &ctx.s().project;
        p.buses.iter().filter(|b| b.id != owner).map(|b| (b.id, b.name.clone(), p.would_loop(owner, b.id))).collect()
    };
    let add = |label: &str, target: Option<Option<Id>>, sensitive: bool| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_sensitive(sensitive);
        if !sensitive {
            b.set_tooltip_text(Some("This routing would feed the bus back into itself"));
        }
        let (ctx, pop2, f) = (ctx.clone(), pop.clone(), on_pick.clone());
        b.connect_clicked(move |_| {
            pop2.popdown();
            let target = match target {
                Some(t) => t,
                None => {
                    let mut id = 0;
                    ctx.edit("New Aux", |p| id = new_bus(p));
                    Some(id)
                }
            };
            f(&ctx, target);
        });
        list.append(&b);
    };
    if include_master {
        add("Stereo Out", Some(None), true);
    }
    for (id, name, loops) in &buses {
        add(name, Some(Some(*id)), !loops);
    }
    add("New Bus", None, true);
    pop.set_child(Some(&list));
    pop
}

fn add_send_button(ctx: &Ctx, owner: Id) -> gtk::Widget {
    let b = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text("Add Send").build();
    b.add_css_class("flat");
    b.add_css_class("empty-slot");
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = bus_menu(&ctx, owner, false, move |ctx, target| {
            let Some(bus) = target else { return };
            ctx.edit("Add Send", |p| {
                p.channel_mut(Some(owner)).unwrap().sends.push(Send { bus, level_db: 0.0, mode: SendMode::PostPan });
            });
        });
        mb.set_popover(Some(&pop));
    });
    b.upcast()
}

fn output_button(ctx: &Ctx, owner: Id, current: Output) -> gtk::Widget {
    let name = match current {
        Output::Master => "Stereo Out".to_string(),
        Output::Bus(b) => ctx.s().project.buses.iter().find(|x| x.id == b).map_or("?".into(), |x| x.name.clone()),
    };
    let b = gtk::MenuButton::builder().label(&name).tooltip_text("Output").build();
    b.add_css_class("output-button");
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = bus_menu(&ctx, owner, true, move |ctx, target| {
            let out = target.map_or(Output::Master, Output::Bus);
            ctx.edit("Change Output", |p| {
                if let Some(c) = p.channel_mut(Some(owner)) {
                    c.output = out;
                }
            });
        });
        mb.set_popover(Some(&pop));
    });
    b.upcast()
}

/// Opens the mixer in its own window, or brings the existing one forward.
pub fn open_window(ctx: &Ctx, parent: &impl IsA<gtk::Window>) {
    thread_local! {
        static WINDOW: std::cell::RefCell<Option<adw::Window>> = const { std::cell::RefCell::new(None) };
    }
    if let Some(w) = WINDOW.with(|w| w.borrow().clone()) {
        w.present();
        return;
    }
    let win = adw::Window::builder().title("Mixer").default_width(1100).default_height(560).transient_for(parent).build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&build(ctx)));
    win.set_content(Some(&tv));
    win.connect_close_request(|_| {
        WINDOW.with(|w| w.borrow_mut().take());
        glib::Propagation::Proceed
    });
    WINDOW.with(|w| *w.borrow_mut() = Some(win.clone()));
    win.present();
}
