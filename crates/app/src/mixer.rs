use crate::canvas::{self, Canvas, rect, rgba, rounded, text, track_color};
use crate::dnd::{self, Source};
use crate::gestures::{self, CAP_H, KnobSpec, Paint, db_to_pos, fmt_db, paint_button};
use crate::plugins;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

// Every strip is built from the same fixed-height rows so slots, knobs and faders line up across
// the mixer and with the label column.
const SLOT: i32 = 26;
const EQ_ROW: i32 = 34;
const ICON_ROW: i32 = 28;
const PAN_ROW: i32 = 50;
const DB_ROW: i32 = 26;
const FADER_H: i32 = 210;
const BUTTON_ROW: i32 = 26;
const NAME_ROW: i32 = 26;

#[derive(Clone, Copy)]
pub struct Layout {
    pub wide: bool,
    pub fx_rows: usize,
    pub send_rows: usize,
    /// MIDI FX slot rows; none when no instrument track is shown.
    pub midi_rows: usize,
}

impl Layout {
    /// Enough slot rows for the busiest strip shown, plus the empty slot below it.
    pub fn for_channels(p: &Project, ids: &[Option<Id>], wide: bool) -> Layout {
        let chans: Vec<&Channel> = ids.iter().filter_map(|id| p.channel(*id)).collect();
        let fx = chans.iter().map(|c| c.inserts.len()).max().unwrap_or(0) + 1;
        let sends = chans.iter().map(|c| c.sends.len()).max().unwrap_or(0) + 1;
        let midi = ids
            .iter()
            .filter_map(|id| id.and_then(|i| p.track(i)))
            .filter(|t| t.kind == TrackKind::Instrument)
            .map(|t| t.midi_fx.len() + 1)
            .max()
            .unwrap_or(0);
        Layout { wide, fx_rows: fx.max(2), send_rows: sends.max(2), midi_rows: midi }
    }
}

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

/// What a project's mixers show. Its docked mixer and mixer window share one, so they match.
struct View {
    filter: Cell<Filter>,
    types: Cell<u8>,
    wide: Cell<bool>,
    labels: Cell<bool>,
}

thread_local! {
    static VIEWS: std::cell::RefCell<std::collections::HashMap<usize, Rc<View>>> = Default::default();
}

fn row(h: i32) -> gtk::Box {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    r.set_size_request(-1, h);
    r.set_vexpand(false);
    r
}

/// A mixer: used docked under the tracks area and in its own window, both showing the same.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let view = VIEWS.with(|v| {
        v.borrow_mut()
            .entry(ctx.key())
            .or_insert_with(|| {
                Rc::new(View { filter: Cell::new(Filter::Tracks), types: Cell::new(0xff), wide: Cell::new(false), labels: Cell::new(true) })
            })
            .clone()
    });
    let strips = gtk::Box::new(gtk::Orientation::Horizontal, 1);
    strips.add_css_class("mixer");
    let scroller = gtk::ScrolledWindow::builder()
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&strips)
        .build();

    let refill: Rc<dyn Fn()> = {
        let (ctx, strips, view) = (ctx.clone(), strips.clone(), view.clone());
        Rc::new(move || fill(&ctx, &strips, &view))
    };

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("mixer-view");
    let (bar, sync_bar) = toolbar(ctx, &root, &view);
    root.append(&bar);
    root.append(&scroller);
    refill();
    {
        let (ctx2, strips, view) = (ctx.clone(), strips.clone(), view.clone());
        let weak = root.downgrade();
        ctx.on(move |c| {
            // A closed mixer window's view stops reacting once its widgets are gone.
            if weak.upgrade().is_none() {
                return;
            }
            match c {
                Change::Project | Change::Catalog => refill(),
                Change::Mixer => {
                    sync_bar();
                    refill();
                }
                Change::Selection if view.filter.get() == Filter::Single => refill(),
                Change::Selection => gestures::sync_selection_classes(&ctx2, &strips, "strip:"),
                _ => {}
            }
        });
    }
    root.upcast()
}

/// The mixer's toolbar, and a function that sets its controls from the shared view.
fn toolbar(ctx: &Ctx, root: &gtk::Box, view: &Rc<View>) -> (gtk::Widget, Rc<dyn Fn()>) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("toolbar");
    bar.add_css_class("mixer-toolbar");

    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        group.add_action(&a);
    };
    let c = ctx.clone();
    add("undo", Box::new(move || c.undo()));
    let c = ctx.clone();
    add("redo", Box::new(move || c.redo()));
    let c = ctx.clone();
    add(
        "select-all",
        Box::new(move || {
            let ids: Vec<Id> = {
                let p = &c.s().project;
                p.tracks.iter().map(|t| t.id).chain(p.buses.iter().map(|b| b.id)).collect()
            };
            c.set_selection(ids);
        }),
    );
    let c = ctx.clone();
    add("deselect", Box::new(move || c.set_selection(Vec::new())));
    let c = ctx.clone();
    add(
        "clear-clips",
        Box::new(move || {
            for m in c.m().meters.values_mut() {
                m.clip = false;
                m.max = 0.0;
                m.hold = [0.0; 2];
            }
        }),
    );
    let low = gio::SimpleAction::new_stateful("low-latency", None, &ctx.s().settings.low_latency.to_variant());
    {
        let c = ctx.clone();
        low.connect_activate(move |a, _| {
            let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            let mut s = c.m();
            s.settings.low_latency = on;
            s.settings.save();
            s.rebuild();
            drop(s);
            c.emit(Change::Mixer);
        });
    }
    group.add_action(&low);
    let labels = gio::SimpleAction::new_stateful("labels", None, &view.labels.get().to_variant());
    {
        let (view, c) = (view.clone(), ctx.clone());
        labels.connect_activate(move |_, _| {
            view.labels.set(!view.labels.get());
            c.emit(Change::Mixer);
        });
    }
    group.add_action(&labels);
    root.insert_action_group("mixer", Some(&group));

    let menus = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    menus.add_css_class("linked");
    let edit = gio::Menu::new();
    edit.append(Some("Undo"), Some("mixer.undo"));
    edit.append(Some("Redo"), Some("mixer.redo"));
    let sel = gio::Menu::new();
    sel.append(Some("Select All Channel Strips"), Some("mixer.select-all"));
    sel.append(Some("Deselect All"), Some("mixer.deselect"));
    edit.append_section(None, &sel);
    let options = gio::Menu::new();
    options.append(Some("Clear All Clip Lights and Peaks"), Some("mixer.clear-clips"));
    options.append(Some("Low Latency Mode"), Some("mixer.low-latency"));
    let viewm = gio::Menu::new();
    viewm.append(Some("Strip Labels"), Some("mixer.labels"));
    for (label, model) in [("Edit", edit), ("Options", options), ("View", viewm)] {
        let b = gtk::MenuButton::builder().label(label).menu_model(&model).always_show_arrow(true).build();
        menus.append(&b);
    }
    bar.append(&menus);

    let filters = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    filters.add_css_class("linked");
    let mut first: Option<gtk::ToggleButton> = None;
    let mut filter_buttons = Vec::new();
    for (label, f) in [("Single", Filter::Single), ("Tracks", Filter::Tracks), ("All", Filter::All)] {
        let b = gtk::ToggleButton::with_label(label);
        match &first {
            Some(g) => b.set_group(Some(g)),
            None => first = Some(b.clone()),
        }
        b.set_active(f == view.filter.get());
        let (view, c) = (view.clone(), ctx.clone());
        b.connect_toggled(move |b| {
            if b.is_active() && view.filter.get() != f {
                view.filter.set(f);
                c.emit(Change::Mixer);
            }
        });
        filters.append(&b);
        filter_buttons.push((b, f));
    }
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    bar.append(&filters);

    let types = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    types.add_css_class("linked");
    let mut type_buttons = Vec::new();
    for (label, bit) in [("Audio", AUDIO), ("Inst", INST), ("Aux", AUX), ("Output", OUTPUT)] {
        let b = gtk::ToggleButton::with_label(label);
        b.set_active(view.types.get() & bit != 0);
        b.add_css_class("filter-toggle");
        let (view, c) = (view.clone(), ctx.clone());
        b.connect_toggled(move |b| {
            let t = view.types.get();
            let new = if b.is_active() { t | bit } else { t & !bit };
            if new != t {
                view.types.set(new);
                c.emit(Change::Mixer);
            }
        });
        types.append(&b);
        type_buttons.push((b, bit));
    }
    bar.append(&types);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);

    let widths = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    widths.add_css_class("linked");
    let narrow = gtk::ToggleButton::new();
    narrow.set_icon_name("view-dual-symbolic");
    narrow.set_tooltip_text(Some("Narrow Channel Strips"));
    narrow.set_active(!view.wide.get());
    let wide = gtk::ToggleButton::new();
    wide.set_icon_name("view-paged-symbolic");
    wide.set_tooltip_text(Some("Wide Channel Strips"));
    wide.set_group(Some(&narrow));
    wide.set_active(view.wide.get());
    {
        let (view, c) = (view.clone(), ctx.clone());
        wide.connect_toggled(move |b| {
            if view.wide.get() != b.is_active() {
                view.wide.set(b.is_active());
                c.emit(Change::Mixer);
            }
        });
    }
    widths.append(&narrow);
    widths.append(&wide);
    bar.append(&widths);

    let sync: Rc<dyn Fn()> = {
        let (view, c) = (view.clone(), ctx.clone());
        Rc::new(move || {
            for (b, f) in &filter_buttons {
                b.set_active(*f == view.filter.get());
            }
            for (b, bit) in &type_buttons {
                b.set_active(view.types.get() & bit != 0);
            }
            narrow.set_active(!view.wide.get());
            wide.set_active(view.wide.get());
            labels.set_state(&view.labels.get().to_variant());
            low.set_state(&c.s().settings.low_latency.to_variant());
        })
    };
    (bar.upcast(), sync)
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
            v.extend(c.vca);
            v
        })
        .collect();
    let (tracks, buses): (Vec<Id>, Vec<Id>) = match view.filter.get() {
        Filter::Single => {
            // A selected track or aux, followed by the buses it feeds.
            let (mut tracks, mut path, start) = match selected {
                Some(id) if p.track(id).is_some() => (vec![id], Vec::new(), &p.track(id).unwrap().channel),
                Some(id) if p.buses.iter().any(|b| b.id == id) => (Vec::new(), vec![id], &p.buses.iter().find(|b| b.id == id).unwrap().channel),
                _ => return (vec![], vec![], true),
            };
            let mut frontier: Vec<&Channel> = vec![start];
            while let Some(c) = frontier.pop() {
                let targets = c.sends.iter().map(|s| s.bus).chain(match c.output {
                    Output::Bus(b) => Some(b),
                    Output::Master | Output::Hardware { .. } => None,
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
            tracks.dedup();
            path.dedup();
            (tracks, path)
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

fn fill(ctx: &Ctx, strips: &gtk::Box, view: &View) {
    while let Some(c) = strips.first_child() {
        strips.remove(&c);
    }
    let (tracks, buses, output) = {
        let s = ctx.s();
        visible(&s.project, view, s.selected_track)
    };
    let mut ids: Vec<Option<Id>> = tracks.iter().chain(&buses).map(|i| Some(*i)).collect();
    if output {
        ids.push(None);
    }
    let layout = Layout::for_channels(&ctx.s().project, &ids, view.wide.get());
    if view.labels.get() {
        strips.append(&label_column(&layout));
    }
    // Equal expanding gaps on either side of the buses keep them halfway between the tracks
    // and the output, with a minimum gap when the mixer is too narrow to spare more.
    let gap = |min: i32| {
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        spacer.set_size_request(min, -1);
        spacer
    };
    let min = if tracks.is_empty() { 0 } else { 16 };
    for id in tracks {
        strips.append(&strip(ctx, Some(id), &layout));
    }
    if !buses.is_empty() {
        strips.append(&gap(min));
        for id in buses {
            strips.append(&strip(ctx, Some(id), &layout));
        }
    }
    strips.append(&gap(if output { 16 } else { 0 }));
    if output {
        strips.append(&strip(ctx, None, &layout));
    }
}

fn label_column(layout: &Layout) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.add_css_class("strip-labels");
    // The labels expand to right-align; the column itself must not take the mixer's spare width.
    col.set_hexpand(false);
    let label = |text: &str, h: i32| {
        let r = row(h);
        let l = gtk::Label::new(Some(text));
        l.set_xalign(1.0);
        l.set_valign(gtk::Align::Start);
        l.set_margin_top(4);
        l.set_hexpand(true);
        r.append(&l);
        r
    };
    col.append(&label("Setting", SLOT));
    col.append(&label("EQ", EQ_ROW));
    col.append(&label("Input", SLOT));
    if layout.midi_rows > 0 {
        col.append(&label("MIDI FX", SLOT * layout.midi_rows as i32));
    }
    col.append(&label("Audio FX", SLOT * layout.fx_rows as i32));
    col.append(&label("Sends", SLOT * layout.send_rows as i32));
    col.append(&label("Output", SLOT));
    col.append(&label("Automation", SLOT));
    col.append(&label("", ICON_ROW));
    col.append(&label("Pan", PAN_ROW));
    col.append(&label("dB", DB_ROW));
    col.set_size_request(64, -1);
    col.upcast()
}

#[derive(Clone)]
struct Info {
    name: String,
    color: Option<u8>,
    kind: Option<TrackKind>,
    channel: Channel,
    instrument: Option<PluginSlot>,
    midi_fx: Vec<PluginSlot>,
    selected: bool,
    is_bus: bool,
    is_vca: bool,
    input: u32,
    stereo: bool,
    dim: bool,
    reference: bool,
}

fn info(ctx: &Ctx, id: Option<Id>) -> Option<Info> {
    let s = ctx.s();
    let p = &s.project;
    let selected = id.is_some_and(|i| s.selection.contains(&i));
    let base = |name: String, channel: Channel| Info {
        name,
        color: None,
        kind: None,
        channel,
        instrument: None,
        midi_fx: Vec::new(),
        selected,
        is_bus: false,
        is_vca: false,
        input: 0,
        stereo: true,
        dim: p.dim,
        reference: false,
    };
    match id {
        None => Some(base("Stereo Out".into(), p.master.clone())),
        Some(id) => {
            if let Some(t) = p.track(id) {
                Some(Info {
                    color: Some(t.color),
                    kind: Some(t.kind),
                    instrument: t.instrument.clone(),
                    midi_fx: t.midi_fx.clone(),
                    input: t.input,
                    stereo: t.stereo,
                    reference: t.reference,
                    ..base(t.name.clone(), t.channel.clone())
                })
            } else {
                p.buses.iter().find(|b| b.id == id).map(|b| Info { is_bus: true, is_vca: b.vca, ..base(b.name.clone(), b.channel.clone()) })
            }
        }
    }
}

/// A channel strip for a track, aux bus or (with `None`) the stereo output.
pub fn strip(ctx: &Ctx, id: Option<Id>, layout: &Layout) -> gtk::Widget {
    let Some(i) = info(ctx, id) else { return gtk::Box::new(gtk::Orientation::Vertical, 0).upcast() };
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.add_css_class("channel-strip");
    col.set_widget_name(&format!("strip:{}", id.map_or("m".into(), |i| i.to_string())));
    if i.selected {
        col.add_css_class("selected");
    }
    col.set_hexpand(false);
    col.set_size_request(
        if layout.wide { 128 } else { 92 },
        -1,
    );

    // A VCA strip keeps every row so it lines up, but only its fader, mute and name do anything.
    let vca = i.is_vca;
    let r = row(SLOT);
    if !vca {
        let setting = setting_button(ctx, id);
        setting.set_hexpand(true);
        r.append(&setting);
    }
    col.append(&r);
    let r = row(EQ_ROW);
    if !vca {
        r.append(&crate::channel_eq::thumbnail(ctx, id));
    }
    col.append(&r);

    let r = row(SLOT);
    if !vca {
        let input = input_row(ctx, id, &i);
        input.set_hexpand(true);
        r.append(&input);
        r.append(&invert_button(ctx, id, i.channel.invert));
    }
    col.append(&r);

    let frozen = id.and_then(|t| ctx.s().project.track(t).map(|t| t.freeze.is_some())).unwrap_or(false);
    if layout.midi_rows > 0 {
        let midi = gtk::Box::new(gtk::Orientation::Vertical, 0);
        midi.set_size_request(-1, SLOT * layout.midi_rows as i32);
        midi.set_sensitive(!frozen);
        if let (Some(track), Some(TrackKind::Instrument)) = (id, i.kind) {
            for slot in &i.midi_fx {
                midi.append(&midi_fx_slot(ctx, track, slot));
            }
            midi.append(&empty_midi_fx_slot(ctx, track));
        }
        col.append(&midi);
    }

    let fx = gtk::Box::new(gtk::Orientation::Vertical, 0);
    fx.set_size_request(-1, SLOT * layout.fx_rows as i32);
    fx.set_sensitive(!frozen);
    if !vca {
        for (k, slot) in i.channel.inserts.iter().enumerate() {
            fx.append(&insert_slot(ctx, id, k, slot));
        }
        fx.append(&empty_slot(ctx, id, i.channel.inserts.len()));
    }
    col.append(&fx);

    let sends = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sends.set_size_request(-1, SLOT * layout.send_rows as i32);
    if let Some(owner) = id.filter(|_| !vca && !i.reference) {
        for (k, send) in i.channel.sends.iter().enumerate() {
            sends.append(&send_slot(ctx, owner, k, send));
        }
        sends.append(&add_send_slot(ctx, owner));
    }
    col.append(&sends);

    let r = row(SLOT);
    if i.reference {
        let out = gtk::Label::new(Some("Reference"));
        out.add_css_class("strip-slot");
        out.add_css_class("dim-label");
        out.set_tooltip_text(Some("Plays to the main outputs while Reference is on"));
        out.set_hexpand(true);
        r.append(&out);
    } else if let Some(owner) = id.filter(|_| !vca) {
        let out = output_button(ctx, owner, i.channel.output);
        out.set_hexpand(true);
        r.append(&out);
    }
    col.append(&r);
    let r = row(SLOT);
    if !vca {
        let mode = crate::automation::mode_dropdown(ctx, id, i.channel.auto_mode);
        mode.add_css_class("strip-slot");
        mode.set_hexpand(true);
        r.append(&mode);
    }
    col.append(&r);

    let r = row(ICON_ROW);
    r.set_halign(gtk::Align::Center);
    r.append(&if i.reference { reference_badge().upcast() } else { type_badge(&i) });
    col.append(&r);

    let r = row(PAN_ROW);
    if !vca {
        let pan = gestures::pan_knob(ctx, id);
        pan.set_hexpand(true);
        pan.set_halign(gtk::Align::Center);
        r.append(&pan);
    }
    col.append(&r);

    let r = row(DB_ROW);
    r.set_homogeneous(true);
    r.append(&volume_readout(ctx, id));
    r.append(&peak_readout(ctx, id));
    col.append(&r);

    let fh = FADER_H;
    let r = row(fh);
    r.set_halign(gtk::Align::Center);
    r.append(&gestures::fader(ctx, id, fh));
    r.append(&scale(fh));
    r.append(&meter(ctx, id, fh));
    col.append(&r);

    let r = row(BUTTON_ROW);
    r.set_halign(gtk::Align::Center);
    match i.kind {
        Some(TrackKind::Audio) => {
            r.append(&paint_button(ctx, Paint::Monitor, id));
            r.append(&paint_button(ctx, Paint::Arm, id));
        }
        Some(TrackKind::Instrument) => r.append(&paint_button(ctx, Paint::Arm, id)),
        None if id.is_none() => {
            let b = gtk::Button::with_label("Exp");
            b.add_css_class("strip-small");
            b.set_tooltip_text(Some("Export Mix"));
            b.set_action_name(Some("win.export-mix"));
            r.append(&b);
            r.append(&loudness_button(ctx));
        }
        None => {}
    }
    col.append(&r);

    let r = row(BUTTON_ROW);
    r.set_halign(gtk::Align::Center);
    r.append(&paint_button(ctx, Paint::Mute, id));
    if id.is_none() {
        r.append(&dim_button(ctx, i.dim));
    } else if !vca {
        r.append(&paint_button(ctx, Paint::Solo, id));
    }
    col.append(&r);

    col.append(&name_plate(ctx, id, &i));
    col.upcast()
}

fn slot_button(label: &str) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("strip-slot");
    b.set_hexpand(true);
    if let Some(l) = b.child().and_downcast::<gtk::Label>() {
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        l.set_max_width_chars(6);
    }
    b
}

fn setting_button(ctx: &Ctx, id: Option<Id>) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().label("Setting").build();
    b.add_css_class("strip-slot");
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = gtk::Popover::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let has_clip = ctx.s().strip_clipboard.is_some();
        let item = |label: &str, sensitive: bool, f: Box<dyn Fn(&Ctx)>| {
            let b = gtk::Button::with_label(label);
            b.add_css_class("flat");
            b.set_sensitive(sensitive);
            let (ctx, pop2) = (ctx.clone(), pop.clone());
            b.connect_clicked(move |_| {
                pop2.popdown();
                f(&ctx);
            });
            list.append(&b);
        };
        item("Copy Channel Strip", true, Box::new(move |c| c.m().copy_strip(id)));
        item("Paste Channel Strip", has_clip, Box::new(move |c| paste_strip(c, id)));
        item(
            "Reset Channel Strip",
            true,
            Box::new(move |c| {
                c.edit("Reset Channel Strip", |p| {
                    // Routing and automation stay; the reset clears the processing.
                    if let Some(ch) = p.channel_mut(id) {
                        let (output, vca) = (ch.output, ch.vca);
                        let automation = std::mem::take(&mut ch.automation);
                        *ch = Channel { output, vca, automation, auto_mode: ch.auto_mode, ..Channel::default() };
                    }
                })
            }),
        );
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let anchor = mb.clone();
        item("Save Channel Strip Setting…", true, Box::new(move |c| save_strip_setting(c, id, &anchor)));
        let saved = strip_settings();
        if !saved.is_empty() {
            let title = gtk::Label::new(Some("Load"));
            title.add_css_class("dim-label");
            title.add_css_class("caption-heading");
            title.set_xalign(0.0);
            title.set_margin_start(8);
            list.append(&title);
            for (name, path) in saved {
                item(&name, true, Box::new(move |c| load_strip_setting(c, id, &path)));
            }
        }
        pop.set_child(Some(&list));
        mb.set_popover(Some(&pop));
    });
    b
}

/// A saved channel strip: its strip type, its plug-ins with their settings and sections and its
/// Channel EQ, plus the instrument and MIDI FX of an instrument track.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct StripSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<StripKind>,
    inserts: Vec<PluginSlot>,
    eq: ChannelEq,
    #[serde(default)]
    instrument: Option<PluginSlot>,
    #[serde(default)]
    midi_fx: Vec<PluginSlot>,
}

fn strip_settings_dir() -> std::path::PathBuf {
    match crate::settings::dev_root() {
        Some(d) => d.join("data/channel-strips"),
        None => glib::user_data_dir().join("solstice/channel-strips"),
    }
}

fn strip_settings() -> Vec<(String, std::path::PathBuf)> {
    let mut v: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(strip_settings_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .map(|p| (p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()), p))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn save_strip_setting(ctx: &Ctx, id: Option<Id>, anchor: &gtk::MenuButton) {
    let entry = adw::EntryRow::builder().title("Name").build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.append(&entry);
    let dialog = adw::AlertDialog::builder()
        .heading("Save Channel Strip Setting")
        .body("Its plug-ins, their settings and the Channel EQ, for loading onto any strip.")
        .extra_child(&list)
        .close_response("cancel")
        .default_response("save")
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    let ctx = ctx.clone();
    dialog.connect_response(Some("save"), move |_, _| {
        let name = entry.text().trim().replace('/', "-");
        if name.is_empty() {
            return;
        }
        ctx.m().capture_plugin_states();
        let setting = {
            let p = &ctx.s().project;
            let Some(ch) = p.channel(id) else { return };
            let track = id.and_then(|i| p.track(i));
            StripSetting {
                kind: ch.kind,
                inserts: ch.inserts.clone(),
                eq: ch.eq.clone(),
                instrument: track.and_then(|t| t.instrument.clone()),
                midi_fx: track.map(|t| t.midi_fx.clone()).unwrap_or_default(),
            }
        };
        let dir = strip_settings_dir();
        let saved = std::fs::create_dir_all(&dir).and_then(|_| {
            let json = serde_json::to_vec_pretty(&setting).map_err(std::io::Error::other)?;
            std::fs::write(dir.join(format!("{name}.json")), json)
        });
        ctx.message(&match saved {
            Ok(()) => format!("Saved channel strip setting “{name}”"),
            Err(e) => format!("Could not save the setting: {e}"),
        });
    });
    crate::dialogs::clear_way(&dialog, anchor);
    dialog.present(Some(anchor));
}

fn load_strip_setting(ctx: &Ctx, id: Option<Id>, path: &std::path::Path) {
    let Some(setting) = std::fs::read(path).ok().and_then(|d| serde_json::from_slice::<StripSetting>(&d).ok()) else {
        ctx.message("Could not read that channel strip setting");
        return;
    };
    ctx.edit("Load Channel Strip Setting", |p| apply_strip_setting(p, id, &setting));
}

fn apply_strip_setting(p: &mut Project, id: Option<Id>, setting: &StripSetting) {
    let fresh = |p: &mut Project, s: &PluginSlot| PluginSlot { id: p.alloc_id(), sidechain: None, ..s.clone() };
    let inserts: Vec<PluginSlot> = setting.inserts.iter().map(|s| fresh(p, s)).collect();
    let instrument = setting.instrument.as_ref().map(|s| fresh(p, s));
    let midi_fx: Vec<PluginSlot> = setting.midi_fx.iter().map(|s| fresh(p, s)).collect();
    if let Some(ch) = p.channel_mut(id) {
        ch.inserts = inserts;
        ch.kind = setting.kind;
        ch.eq = setting.eq.clone();
    }
    // Instruments only go onto instrument tracks.
    if let Some(t) = id.and_then(|i| p.track_mut(i)).filter(|t| t.kind == TrackKind::Instrument)
        && instrument.is_some()
    {
        t.instrument = instrument;
        t.midi_fx = midi_fx;
    }
}

fn paste_strip(ctx: &Ctx, id: Option<Id>) {
    let Some(src) = ctx.s().strip_clipboard.clone() else { return };
    ctx.edit("Paste Channel Strip", |p| {
        let inserts: Vec<PluginSlot> = src.inserts.iter().map(|s| PluginSlot { id: p.alloc_id(), ..s.clone() }).collect();
        let sends: Vec<Send> = src
            .sends
            .iter()
            .filter(|s| p.buses.iter().any(|b| b.id == s.bus) && id.is_some_and(|me| me != s.bus && !p.would_loop(me, s.bus)))
            .cloned()
            .collect();
        if let Some(ch) = p.channel_mut(id) {
            ch.inserts = inserts;
            ch.kind = src.kind;
            ch.sends = sends;
            ch.volume_db = src.volume_db;
            ch.pan = src.pan;
            ch.eq = src.eq.clone();
            ch.invert = src.invert;
        }
    });
}

fn input_name(input: u32, stereo: bool) -> String {
    if stereo { format!("In {}-{}", input + 1, input + 2) } else { format!("In {}", input + 1) }
}

fn input_row(ctx: &Ctx, id: Option<Id>, i: &Info) -> gtk::Widget {
    match (id, i.kind) {
        (Some(track), Some(TrackKind::Audio)) => {
            let r = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            r.add_css_class("linked");
            r.set_hexpand(true);
            let format = gtk::Button::with_label(if i.stereo { "◎" } else { "○" });
            format.add_css_class("strip-slot");
            format.add_css_class("format-button");
            format.set_tooltip_text(Some(if i.stereo { "Stereo input" } else { "Mono input" }));
            {
                let ctx = ctx.clone();
                format.connect_clicked(move |_| {
                    ctx.edit("Input Format", |p| {
                        let t = p.track_mut(track).unwrap();
                        t.stereo = !t.stereo;
                        if t.stereo {
                            t.input -= t.input % 2;
                        }
                    })
                });
            }
            let menu = gtk::MenuButton::builder().label(input_name(i.input, i.stereo)).build();
            menu.add_css_class("strip-slot");
            menu.set_hexpand(true);
            menu.set_tooltip_text(Some("Input"));
            let (ctx2, stereo) = (ctx.clone(), i.stereo);
            menu.set_create_popup_func(move |mb| {
                let pop = gtk::Popover::new();
                let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
                let step = if stereo { 2 } else { 1 };
                for input in (0..solstice_backend_jack::INPUTS as u32).step_by(step) {
                    let b = gtk::Button::with_label(&input_name(input, stereo));
                    b.add_css_class("flat");
                    let (ctx, pop2) = (ctx2.clone(), pop.clone());
                    b.connect_clicked(move |_| {
                        pop2.popdown();
                        ctx.edit("Change Input", |p| p.track_mut(track).unwrap().input = input);
                    });
                    list.append(&b);
                }
                let sc = gtk::ScrolledWindow::builder().child(&list).max_content_height(320).propagate_natural_height(true).build();
                pop.set_child(Some(&sc));
                mb.set_popover(Some(&pop));
            });
            r.append(&format);
            r.append(&menu);
            r.upcast()
        }
        (Some(track), Some(TrackKind::Instrument)) => instrument_slot(ctx, track, i.instrument.as_ref()),
        (Some(_), None) if i.is_bus => {
            let l = slot_button(&i.name);
            l.set_sensitive(false);
            l.set_tooltip_text(Some("Aux input"));
            l.upcast()
        }
        _ => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
    }
}

/// Toggles polarity on this strip, or on every selected strip when it is one of them.
fn invert_button(ctx: &Ctx, id: Option<Id>, on: bool) -> gtk::Button {
    let b = gtk::Button::with_label("Ø");
    b.add_css_class("strip-slot");
    b.add_css_class("format-button");
    b.add_css_class("invert");
    if on {
        b.add_css_class("on");
    }
    b.set_tooltip_text(Some("Invert Polarity"));
    let ctx = ctx.clone();
    b.connect_clicked(move |_| {
        let gang = ctx.s().gang(id);
        ctx.edit("Invert Polarity", |p| {
            for g in gang {
                let vca = g.is_some_and(|i| p.buses.iter().any(|b| b.id == i && b.vca));
                if let Some(c) = p.channel_mut(g).filter(|_| !vca) {
                    c.invert = !on;
                }
            }
        });
    });
    b
}

fn type_badge(i: &Info) -> gtk::Widget {
    let (color, icon) = match i.kind {
        Some(TrackKind::Instrument) => (0x26a269, "audio-x-generic-symbolic"),
        Some(TrackKind::Audio) => (0x1c71d8, "audio-input-microphone-symbolic"),
        None if i.is_vca => (0x9141ac, "view-sort-descending-symbolic"),
        None => (0x1c71d8, "audio-speakers-symbolic"),
    };
    let bg = Canvas::new(move |_, s, w, h| rounded(s, 0.0, 0.0, w, h, 4.0, &rgba(color, 1.0)));
    bg.set_size_request(30, 18);
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(12);
    img.add_css_class("badge-icon");
    let o = gtk::Overlay::new();
    o.set_child(Some(&bg));
    o.add_overlay(&img);
    o.set_valign(gtk::Align::Center);
    o.upcast()
}

pub fn reference_badge() -> gtk::Label {
    let l = gtk::Label::new(Some("REF"));
    l.add_css_class("reference-badge");
    l.set_valign(gtk::Align::Center);
    l.set_tooltip_text(Some("Reference Track"));
    l
}

fn readout(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("numeric");
    l.add_css_class("db-readout");
    l
}

fn volume_readout(ctx: &Ctx, id: Option<Id>) -> gtk::Label {
    let l = readout(&fmt_db(ctx.s().project.channel(id).map_or(0.0, |c| c.volume_db)));
    l.set_tooltip_text(Some("Volume. Double-click to type a level."));
    {
        let ctx = ctx.clone();
        l.add_tick_callback(move |l, _| {
            let t = fmt_db(ctx.s().project.channel(id).map_or(0.0, |c| c.volume_db));
            if l.text() != t {
                l.set_text(&t);
            }
            glib::ControlFlow::Continue
        });
    }
    let dbl = gtk::GestureClick::new();
    {
        let (ctx, l2) = (ctx.clone(), l.clone());
        dbl.connect_pressed(move |_, n, _, _| {
            if n == 2 {
                type_level(&ctx, &l2, id);
            }
        });
    }
    l.add_controller(dbl);
    l
}

fn peak_readout(ctx: &Ctx, id: Option<Id>) -> gtk::Label {
    let l = readout("");
    l.set_tooltip_text(Some("Peak level. Click to reset."));
    {
        let ctx = ctx.clone();
        l.add_tick_callback(move |l, _| {
            let max = ctx.s().meters.get(&id).map_or(0.0, |m| m.max);
            let t = if max <= 1e-5 { String::new() } else { format!("{:.1}", 20.0 * max.log10()) };
            if l.text() != t {
                l.set_text(&t);
                if max > 1.0 {
                    l.add_css_class("over");
                } else {
                    l.remove_css_class("over");
                }
            }
            glib::ControlFlow::Continue
        });
    }
    let click = gtk::GestureClick::new();
    {
        let ctx = ctx.clone();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(m) = ctx.m().meters.get_mut(&id) {
                m.max = 0.0;
                m.clip = false;
            }
        });
    }
    l.add_controller(click);
    l
}

/// Opens the Loudness window; while it measures, shows the short-term loudness.
fn loudness_button(ctx: &Ctx) -> gtk::Button {
    let b = gtk::Button::with_label("LUFS");
    b.add_css_class("strip-small");
    b.add_css_class("numeric");
    b.set_tooltip_text(Some("Loudness (Shift+L)"));
    b.set_action_name(Some("win.show-loudness"));
    let ctx = ctx.clone();
    b.add_tick_callback(move |b, _| {
        let st = ctx.s().master_meter.as_ref().map(|m| m.loudness.short_term());
        let t = match st {
            Some(v) if v.is_finite() && v > -70.0 => format!("{v:.1}"),
            Some(_) => "-∞".into(),
            None => "LUFS".into(),
        };
        if b.label().is_none_or(|l| l != t) {
            b.set_label(&t);
        }
        glib::ControlFlow::Continue
    });
    b
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

fn dim_button(ctx: &Ctx, on: bool) -> gtk::Label {
    let b = gtk::Label::new(Some("D"));
    b.add_css_class("track-toggle");
    b.add_css_class("dim");
    b.set_tooltip_text(Some("Dim (−20 dB)"));
    if on {
        b.add_css_class("on");
    }
    let click = gtk::GestureClick::new();
    let ctx = ctx.clone();
    click.connect_pressed(move |_, _, _, _| ctx.tweak(|p| p.dim = !p.dim));
    b.add_controller(click);
    b
}

fn name_plate(ctx: &Ctx, id: Option<Id>, i: &Info) -> gtk::Widget {
    let color = match (i.color, i.is_bus) {
        (Some(c), _) => track_color(c),
        (None, true) => rgba(0x8f8f2a, 1.0),
        (None, false) => rgba(0x813d9c, 1.0),
    };
    let light = 0.299 * color.red() + 0.587 * color.green() + 0.114 * color.blue() > 0.6;
    let bg = Canvas::new(move |_, s, w, h| rect(s, 0.0, 0.0, w, h, &color));
    let name = gtk::Label::new(Some(&i.name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.add_css_class("strip-name");
    name.add_css_class(if light { "on-light" } else { "on-dark" });
    if let Some(id) = id {
        gestures::select_name(ctx, &name, id);
    }
    let o = gtk::Overlay::new();
    o.set_child(Some(&bg));
    o.add_overlay(&name);
    o.set_size_request(-1, NAME_ROW);
    o.upcast()
}

/// The dB scale between fader and meter, using the fader's own curve so marks line up.
fn scale(height: i32) -> Canvas {
    let c = Canvas::new(|c, s, w, h| {
        let fg = canvas::fg(c);
        let dim = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.55);
        let travel = h - CAP_H;
        let mut last = -100.0f32;
        for mark in [6.0f32, 3.0, 0.0, -3.0, -6.0, -9.0, -12.0, -15.0, -18.0, -24.0, -30.0, -40.0, -50.0, -60.0] {
            let y = CAP_H / 2.0 + (1.0 - db_to_pos(mark)) * travel;
            if y - last < 9.0 {
                continue;
            }
            last = y;
            let label = format!("{}", mark.abs() as i32);
            let layout = c.create_pango_layout(Some(&label));
            let tw = layout.pixel_size().0 as f32 * 0.65;
            text(s, c, w - tw - 1.0, y - 5.0, &label, &dim, Some(5.5));
        }
    });
    c.set_size_request(15, height);
    c
}

/// Meter with peak hold and a clip light; click clears the clip light, Alt-click clears all.
fn meter(ctx: &Ctx, id: Option<Id>, height: i32) -> Canvas {
    let ctx2 = ctx.clone();
    let m = Canvas::new(move |_, s, w, h| {
        let m = ctx2.s().meters.get(&id).copied().unwrap_or_default();
        let top = CAP_H / 2.0;
        let travel = h - CAP_H;
        rect(s, 0.0, 0.0, w, top - 2.0, &if m.clip { rgba(0xe01b24, 1.0) } else { rgba(0x000000, 0.35) });
        let bw = (w - 1.0) / 2.0;
        rect(s, 0.0, top, w, travel, &rgba(0x000000, 0.4));
        let y_of = |lv: f32| top + (1.0 - db_to_pos(20.0 * lv.max(1e-6).log10())) * travel;
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
            rect(s, x, y, bw, top + travel - y, &col);
            if m.hold[k] > 1e-4 {
                rect(s, x, y_of(m.hold[k]), bw, 1.5, &rgba(0xffffff, 0.85));
            }
        }
    });
    m.set_size_request(9, height);
    m.set_tooltip_text(Some("Click to clear the clip light, Alt-click to clear all"));
    m.add_tick_callback(|m, _| {
        m.queue_draw();
        glib::ControlFlow::Continue
    });
    let click = gtk::GestureClick::new();
    {
        let ctx = ctx.clone();
        click.connect_pressed(move |g, _, _, _| {
            let all = g.current_event_state().contains(gdk::ModifierType::ALT_MASK);
            for (k, m) in ctx.m().meters.iter_mut() {
                if all || *k == id {
                    m.clip = false;
                    m.hold = [0.0; 2];
                    m.max = 0.0;
                }
            }
        });
    }
    m.add_controller(click);
    m
}

/// Where a plug-in slot sits on a strip.
#[derive(Clone, Copy)]
enum Place {
    Insert(Option<Id>),
    MidiFx(Id),
    Instrument(Id),
}

impl Place {
    /// The slot list it belongs to, for the kinds kept in order.
    fn list(self, p: &mut Project) -> Option<&mut Vec<PluginSlot>> {
        match self {
            Place::Insert(owner) => p.channel_mut(owner).map(|c| &mut c.inserts),
            Place::MidiFx(t) => p.track_mut(t).map(|t| &mut t.midi_fx),
            Place::Instrument(_) => None,
        }
    }

    fn slot(self, p: &mut Project, sid: Id) -> Option<&mut PluginSlot> {
        match self {
            Place::Instrument(t) => p.track_mut(t).and_then(|t| t.instrument.as_mut()).filter(|s| s.id == sid),
            _ => self.list(p).and_then(|l| l.iter_mut().find(|s| s.id == sid)),
        }
    }
}

thread_local! {
    /// Copy Settings: the plug-in's class and its state.
    static SETTINGS: std::cell::RefCell<Option<(String, Vec<u8>)>> = const { std::cell::RefCell::new(None) };
}

/// The plug-in slot menu.
fn slot_menu(ctx: &Ctx, place: Place, sid: Id, anchor: &gtk::Widget) {
    let pop = gtk::Popover::new();
    pop.set_parent(anchor);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let item = |label: &str, enabled: bool, f: Box<dyn Fn(&Ctx)>| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_sensitive(enabled);
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let (ctx, pop2) = (ctx.clone(), pop.clone());
        b.connect_clicked(move |_| {
            pop2.popdown();
            f(&ctx);
        });
        list.append(&b);
    };
    let (bypassed, uid, index, count) = {
        let mut p = ctx.s().project.clone();
        let bypassed = place.slot(&mut p, sid).is_some_and(|s| s.bypass);
        let uid = place.slot(&mut p, sid).map(|s| s.uid.clone()).unwrap_or_default();
        let (index, count) = place.list(&mut p).map_or((0, 1), |l| (l.iter().position(|s| s.id == sid).unwrap_or(0), l.len()));
        (bypassed, uid, index, count)
    };
    let a = anchor.clone();
    item("Open", true, Box::new(move |c| plugins::open_editor(c, sid, &a)));
    if !matches!(place, Place::Instrument(_)) {
        item(
            if bypassed { "Turn On" } else { "Bypass" },
            true,
            Box::new(move |c| {
                c.edit(if bypassed { "Turn On Plug-in" } else { "Bypass Plug-in" }, |p| {
                    if let Some(s) = place.slot(p, sid) {
                        s.bypass = !bypassed;
                    }
                })
            }),
        );
    }
    let a = anchor.clone();
    item("Replace…", true, Box::new(move |c| replace_menu(c, place, sid, &a)));
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let u = uid.clone();
    item(
        "Copy Settings",
        true,
        Box::new(move |c| {
            let state = c.s().plugins.get(&sid).map(|l| l.inst.state());
            SETTINGS.with(|s| *s.borrow_mut() = state.map(|st| (u.clone(), st)));
        }),
    );
    let can_paste = SETTINGS.with(|s| s.borrow().as_ref().is_some_and(|(u, _)| *u == uid));
    item(
        "Paste Settings",
        can_paste,
        Box::new(move |c| {
            let Some((_, state)) = SETTINGS.with(|s| s.borrow().clone()) else { return };
            let inst = c.s().plugins.get(&sid).map(|l| l.inst.clone());
            if let Some(inst) = inst {
                inst.set_state_in(&state, &c.s().bundle.audio_dir());
                c.m().history.mark_dirty();
                c.emit(Change::Project);
            }
        }),
    );
    if !matches!(place, Place::Instrument(_)) {
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        for (label, delta) in [("Move Up", -1isize), ("Move Down", 1)] {
            let to = index as isize + delta;
            item(
                label,
                to >= 0 && (to as usize) < count,
                Box::new(move |c| {
                    c.edit("Move Plug-in", |p| {
                        if let Some(l) = place.list(p) {
                            l.swap(index, to as usize);
                        }
                    })
                }),
            );
        }
    }
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    item(
        "Remove",
        true,
        Box::new(move |c| {
            c.edit("Remove Plug-in", |p| match place {
                Place::Instrument(t) => {
                    if let Some(t) = p.track_mut(t) {
                        t.instrument = None;
                    }
                }
                _ => {
                    if let Some(l) = place.list(p) {
                        l.retain(|s| s.id != sid);
                    }
                }
            })
        }),
    );
    pop.set_child(Some(&list));
    pop.connect_closed(|p| p.unparent());
    pop.popup();
}

/// Picks another plug-in for a slot, keeping its place and bypass state.
fn replace_menu(ctx: &Ctx, place: Place, sid: Id, anchor: &gtk::Widget) {
    let pop = gtk::Popover::new();
    pop.set_parent(anchor);
    pop.set_size_request(320, -1);
    let kind = match place {
        Place::Insert(_) => plugins::Kind::Effect,
        Place::MidiFx(_) => plugins::Kind::MidiFx,
        Place::Instrument(_) => plugins::Kind::Instrument,
    };
    let mono = match place {
        Place::Insert(owner) => {
            let s = ctx.s();
            let index = s.project.channel(owner).and_then(|c| c.inserts.iter().position(|x| x.id == sid)).unwrap_or(0);
            crate::variants::slot_is_mono(&s.project, &s.catalog, owner, index)
        }
        _ => false,
    };
    let pick: Rc<dyn Fn(&solstice_vst3::ClassInfo)> = {
        let (ctx, pop) = (ctx.clone(), pop.clone());
        Rc::new(move |info| {
            pop.popdown();
            let info = info.clone();
            ctx.edit("Replace Plug-in", |p| {
                let mut new = plugins::new_slot(p, &info);
                if let Some(old) = place.slot(p, sid) {
                    new.bypass = old.bypass;
                    *old = new;
                }
            });
        })
    };
    pop.set_child(Some(&plugins::list(ctx, Some(kind), mono, pick)));
    pop.connect_closed(|p| p.unparent());
    pop.popup();
}

/// An insert slot: click opens the plug-in, Alt-click bypasses, right-click for the slot menu,
/// drag to move, Alt-drag to copy.
fn insert_slot(ctx: &Ctx, owner: Option<Id>, index: usize, slot: &PluginSlot) -> gtk::Widget {
    let sid = slot.id;
    let b = slot_button(&slot.name);
    b.add_css_class("insert");
    if slot.bypass {
        b.add_css_class("bypassed");
    }
    match ctx.s().plugin_errors.get(&sid) {
        Some(e) => {
            b.add_css_class("error");
            b.set_tooltip_text(Some(e));
        }
        None => b.set_tooltip_text(Some(&format!(
            "{}\nClick to open, Alt-click to bypass, right-click for options, drag to move, Alt-drag to copy",
            slot.name
        ))),
    }
    let press = gtk::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let (ctx, b2) = (ctx.clone(), b.clone());
        press.connect_pressed(move |g, _, _, _| {
            let alt = g.current_event_state().contains(gdk::ModifierType::ALT_MASK);
            if g.current_button() == gdk::BUTTON_SECONDARY {
                g.set_state(gtk::EventSequenceState::Claimed);
                slot_menu(&ctx, Place::Insert(owner), sid, b2.upcast_ref());
            } else if alt {
                g.set_state(gtk::EventSequenceState::Claimed);
                ctx.edit("Bypass Plug-in", |p| set_bypass(p, owner, sid, None));
            }
        });
    }
    b.add_controller(press);
    {
        let ctx = ctx.clone();
        b.connect_clicked(move |b| plugins::open_editor(&ctx, sid, b));
    }
    dnd::drag_source(&b, format!("slot:{}:{sid}", dnd::owner_key(owner)));
    dnd::drop_target(ctx, &b, move |ctx, v, copy| dnd::drop_insert(ctx, v, owner, index, copy));
    b.upcast()
}

/// Right-click for the slot menu; Alt-click bypasses, except on instruments.
fn slot_gestures(ctx: &Ctx, b: &gtk::Button, place: Place, sid: Id) {
    let press = gtk::GestureClick::new();
    press.set_button(0);
    press.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (ctx, b2) = (ctx.clone(), b.clone());
    press.connect_pressed(move |g, _, _, _| {
        if g.current_button() == gdk::BUTTON_SECONDARY {
            g.set_state(gtk::EventSequenceState::Claimed);
            slot_menu(&ctx, place, sid, b2.upcast_ref());
        } else if g.current_event_state().contains(gdk::ModifierType::ALT_MASK) && !matches!(place, Place::Instrument(_)) {
            g.set_state(gtk::EventSequenceState::Claimed);
            ctx.edit("Bypass Plug-in", |p| {
                if let Some(s) = place.slot(p, sid) {
                    s.bypass = !s.bypass;
                }
            });
        }
    });
    b.add_controller(press);
}

fn set_bypass(p: &mut Project, owner: Option<Id>, sid: Id, value: Option<bool>) {
    if let Some(s) = p.channel_mut(owner).and_then(|c| c.inserts.iter_mut().find(|s| s.id == sid)) {
        s.bypass = value.unwrap_or(!s.bypass);
    }
}

/// The empty slot below the last insert: click to pick, or drop a plug-in on it.
fn empty_slot(ctx: &Ctx, owner: Option<Id>, index: usize) -> gtk::Widget {
    let mono = {
        let s = ctx.s();
        crate::variants::slot_is_mono(&s.project, &s.catalog, owner, index)
    };
    let tip = if mono { "Add Audio Effect (mono)" } else { "Add Audio Effect (stereo)" };
    let b = plugins::picker_button(ctx, plugins::Kind::Effect, mono, tip, {
        let ctx = ctx.clone();
        move |info| {
            let info = info.clone();
            ctx.edit("Add Plug-in", |p| {
                let slot = plugins::new_slot(p, &info);
                p.channel_mut(owner).unwrap().inserts.push(slot);
            });
        }
    });
    b.add_css_class("strip-slot");
    b.add_css_class("empty");
    b.set_hexpand(true);
    dnd::drop_target(ctx, &b, move |ctx, v, copy| dnd::drop_insert(ctx, v, owner, index, copy));
    b.upcast()
}

fn midi_fx_slot(ctx: &Ctx, track: Id, slot: &PluginSlot) -> gtk::Widget {
    let b = slot_button(&slot.name);
    b.add_css_class("midi-fx");
    if slot.bypass {
        b.add_css_class("bypassed");
    }
    let sid = slot.id;
    b.set_tooltip_text(Some(&format!("{}\nClick to open, Alt-click to bypass, right-click for options", slot.name)));
    {
        let ctx = ctx.clone();
        b.connect_clicked(move |b| plugins::open_editor(&ctx, sid, b));
    }
    slot_gestures(ctx, &b, Place::MidiFx(track), sid);
    b.upcast()
}

fn empty_midi_fx_slot(ctx: &Ctx, track: Id) -> gtk::Widget {
    let ctx2 = ctx.clone();
    let b = plugins::picker_button(ctx, plugins::Kind::MidiFx, false, "Add MIDI Effect", move |info| {
        let info = info.clone();
        ctx2.edit("Add MIDI Effect", |p| {
            let slot = plugins::new_slot(p, &info);
            p.track_mut(track).unwrap().midi_fx.push(slot);
        });
    });
    b.add_css_class("strip-slot");
    b.add_css_class("empty");
    b.set_hexpand(true);
    b.upcast()
}

fn instrument_slot(ctx: &Ctx, track: Id, slot: Option<&PluginSlot>) -> gtk::Widget {
    let w: gtk::Widget = match slot {
        None => {
            let ctx2 = ctx.clone();
            let b = plugins::picker_button(ctx, plugins::Kind::Instrument, false, "Choose Instrument", move |info| {
                let info = info.clone();
                ctx2.edit("Set Instrument", |p| {
                    let slot = plugins::new_slot(p, &info);
                    p.track_mut(track).unwrap().instrument = Some(slot);
                });
            });
            b.add_css_class("strip-slot");
            b.add_css_class("empty");
            b.set_hexpand(true);
            b.upcast()
        }
        Some(slot) => {
            let b = slot_button(&slot.name);
            b.add_css_class("instrument");
            let sid = slot.id;
            b.set_tooltip_text(Some(&format!("{}\nClick to open, right-click for options, drag to move", slot.name)));
            {
                let ctx = ctx.clone();
                b.connect_clicked(move |b| plugins::open_editor(&ctx, sid, b));
            }
            slot_gestures(ctx, &b, Place::Instrument(track), sid);
            dnd::drag_source(&b, format!("inst:{track}:{sid}"));
            b.upcast()
        }
    };
    dnd::drop_target(ctx, &w, move |ctx, value, copy| {
        let Ok(payload) = value.get::<String>() else { return false };
        let Some(src) = dnd::parse_payload(&payload) else { return false };
        if !dnd::is_instrument(ctx, &src) || matches!(src, Source::Instrument { track: t, .. } if t == track) {
            return false;
        }
        let states = if copy { dnd::source_state(ctx, &src) } else { None };
        let lib = dnd::library_class(ctx, &src, false);
        ctx.edit(if copy { "Copy Instrument" } else { "Set Instrument" }, move |p| {
            if let Some(slot) = dnd::take_for_drop(p, &src, copy, &states, lib.as_ref()) {
                p.track_mut(track).unwrap().instrument = Some(slot);
            }
        });
        true
    });
    w
}

fn send_mode_name(m: SendMode) -> &'static str {
    match m {
        SendMode::PreFader => "Pre Fader",
        SendMode::PostFader => "Post Fader",
        SendMode::PostPan => "Post Pan",
    }
}

/// A send slot: bus name (menu for mode and removal) with its level knob beside it.
fn send_slot(ctx: &Ctx, owner: Id, k: usize, send: &Send) -> gtk::Widget {
    let r = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let name = ctx.s().project.buses.iter().find(|b| b.id == send.bus).map_or("?".into(), |b| b.name.clone());
    let b = gtk::MenuButton::builder().label(&name).tooltip_text(send_mode_name(send.mode)).build();
    b.add_css_class("strip-slot");
    b.add_css_class("send");
    if send.mode == SendMode::PreFader {
        b.add_css_class("pre");
    }
    b.set_hexpand(true);
    let mode = send.mode;
    let ctx2 = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = gtk::Popover::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let mut group: Option<gtk::CheckButton> = None;
        for m in [SendMode::PreFader, SendMode::PostFader, SendMode::PostPan] {
            let c = gtk::CheckButton::with_label(send_mode_name(m));
            match &group {
                Some(g) => c.set_group(Some(g)),
                None => group = Some(c.clone()),
            }
            c.set_active(m == mode);
            let (ctx, pop2) = (ctx2.clone(), pop.clone());
            c.connect_toggled(move |c| {
                if c.is_active() && m != mode {
                    pop2.popdown();
                    ctx.edit("Send Mode", |p| {
                        if let Some(s) = p.channel_mut(Some(owner)).and_then(|c| c.sends.get_mut(k)) {
                            s.mode = m;
                        }
                    });
                }
            });
            list.append(&c);
        }
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let remove = gtk::Button::with_label("Remove Send");
        remove.add_css_class("flat");
        let (ctx, pop2) = (ctx2.clone(), pop.clone());
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
        list.append(&remove);
        pop.set_child(Some(&list));
        mb.set_popover(Some(&pop));
    });
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
            compact: true,
            get: Rc::new(move |p, id| p.channel(id).and_then(|c| c.sends.get(k)).map_or(0.0, |s| s.level_db)),
            set: Rc::new(move |p, id, v| {
                if let Some(s) = p.channel_mut(id).and_then(|c| c.sends.get_mut(k)) {
                    s.level_db = v;
                }
            }),
            fmt: |v| if v <= -59.9 { "-∞".into() } else { format!("{v:.1} dB") },
        },
    );
    level.set_valign(gtk::Align::Center);
    r.append(&b);
    r.append(&level);
    r.upcast()
}

/// Buses to route to. `current` gets a tick; `None` inside it is Stereo Out.
fn bus_menu(ctx: &Ctx, owner: Id, include_master: bool, current: Option<Option<Id>>, on_pick: impl Fn(&Ctx, Option<Id>) + 'static) -> gtk::Popover {
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let on_pick = Rc::new(on_pick);
    let buses: Vec<(Id, String, bool)> = {
        let p = &ctx.s().project;
        p.buses.iter().filter(|b| b.id != owner && !b.vca).map(|b| (b.id, b.name.clone(), p.would_loop(owner, b.id))).collect()
    };
    let add = |label: &str, target: Option<Option<Id>>, sensitive: bool| {
        let label = if target.is_some() && target == current { format!("✓ {label}") } else { label.to_string() };
        let b = gtk::Button::with_label(&label);
        b.add_css_class("flat");
        b.set_sensitive(sensitive);
        if !sensitive {
            b.set_tooltip_text(Some("This routing would feed the bus back into itself"));
        }
        let (ctx, pop2, f) = (ctx.clone(), pop.clone(), on_pick.clone());
        b.connect_clicked(move |_| {
            pop2.popdown();
            match target {
                Some(t) => f(&ctx, t),
                None => {
                    // The aux is created as soon as you route to an unused bus; the routing
                    // joins the same undo step.
                    let mut id = 0;
                    ctx.edit("New Aux", |p| id = p.add_bus());
                    f(&ctx, Some(id));
                    ctx.m().history.merge_last();
                }
            }
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

fn add_send_slot(ctx: &Ctx, owner: Id) -> gtk::Widget {
    let b = gtk::MenuButton::builder().tooltip_text("Add Send").build();
    b.add_css_class("strip-slot");
    b.add_css_class("empty");
    b.set_hexpand(true);
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = bus_menu(&ctx, owner, false, None, move |ctx, target| {
            let Some(bus) = target else { return };
            ctx.edit("Add Send", |p| {
                p.channel_mut(Some(owner)).unwrap().sends.push(Send { bus, level_db: 0.0, mode: SendMode::PostPan });
            });
        });
        mb.set_popover(Some(&pop));
    });
    b.upcast()
}

/// "3" or "3–4": interface outputs counted from 1.
fn hardware_name(first: u16, stereo: bool) -> String {
    if stereo { format!("{}–{}", first + 1, first + 2) } else { format!("{}", first + 1) }
}

fn output_button(ctx: &Ctx, owner: Id, current: Output) -> gtk::MenuButton {
    let name = match current {
        Output::Master => "Stereo Out".to_string(),
        Output::Bus(b) => ctx.s().project.buses.iter().find(|x| x.id == b).map_or("?".into(), |x| x.name.clone()),
        Output::Hardware { first, stereo } => format!("Out {}", hardware_name(first, stereo)),
    };
    let b = gtk::MenuButton::builder().label(&name).tooltip_text("Output").build();
    b.add_css_class("strip-slot");
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let current = match ctx.s().project.channel(Some(owner)).map(|c| c.output) {
            Some(Output::Bus(b)) => Some(Some(b)),
            Some(Output::Hardware { .. }) => None,
            _ => Some(None),
        };
        let pop = bus_menu(&ctx, owner, true, current, move |ctx, target| {
            let out = target.map_or(Output::Master, Output::Bus);
            ctx.edit("Change Output", |p| {
                if let Some(c) = p.channel_mut(Some(owner)) {
                    c.output = out;
                }
            });
        });
        // Interface outputs past the main pair, singly and in pairs; none without a backend.
        let outputs = ctx.s().io.as_ref().map_or(0, |io| io.backend.outputs) as u16;
        let current_hw = match ctx.s().project.channel(Some(owner)).map(|c| c.output) {
            Some(Output::Hardware { first, stereo }) => Some((first, stereo)),
            _ => None,
        };
        if let Some(list) = pop.child().and_downcast::<gtk::Box>()
            && outputs > 2
        {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let heading = gtk::Label::new(Some("Hardware Outputs"));
            heading.add_css_class("dim-label");
            heading.add_css_class("caption-heading");
            heading.set_xalign(0.0);
            list.append(&heading);
            for first in 2..outputs {
                let pair = (first % 2 == 0 && first + 1 < outputs).then_some(true);
                for stereo in [false].into_iter().chain(pair) {
                    let name = format!("Output {}", hardware_name(first, stereo));
                    let label = if current_hw == Some((first, stereo)) { format!("✓ {name}") } else { name };
                    let b = gtk::Button::with_label(&label);
                    b.add_css_class("flat");
                    let (ctx, pop2) = (ctx.clone(), pop.clone());
                    b.connect_clicked(move |_| {
                        pop2.popdown();
                        ctx.edit("Change Output", |p| {
                            if let Some(c) = p.channel_mut(Some(owner)) {
                                c.output = Output::Hardware { first, stereo };
                            }
                        });
                    });
                    list.append(&b);
                }
            }
        }
        // The VCA this channel follows, chosen here from the strip.
        let (vcas, current_vca): (Vec<(Id, String)>, Option<Id>) = {
            let p = &ctx.s().project;
            let vcas = p.buses.iter().filter(|b| b.vca && b.id != owner).map(|b| (b.id, b.name.clone())).collect();
            (vcas, p.channel(Some(owner)).and_then(|c| c.vca))
        };
        if let Some(list) = pop.child().and_downcast::<gtk::Box>()
            && !vcas.is_empty()
        {
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            let none = [(None, "No VCA".to_string())];
            for (id, name) in none.into_iter().chain(vcas.into_iter().map(|(i, n)| (Some(i), n))) {
                let label = if id == current_vca { format!("✓ {name}") } else { name };
                let b = gtk::Button::with_label(&label);
                b.add_css_class("flat");
                let (ctx, pop2) = (ctx.clone(), pop.clone());
                b.connect_clicked(move |_| {
                    pop2.popdown();
                    ctx.edit("Assign VCA", |p| {
                        if let Some(c) = p.channel_mut(Some(owner)) {
                            c.vca = id;
                        }
                    });
                });
                list.append(&b);
            }
        }
        mb.set_popover(Some(&pop));
    });
    b
}

/// Opens the mixer in its own window, or brings the existing one forward.
pub fn open_window(ctx: &Ctx, parent: &impl IsA<gtk::Window>) {
    thread_local! {
        static WINDOWS: std::cell::RefCell<std::collections::HashMap<usize, adw::Window>> = Default::default();
    }
    let key = ctx.key();
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&key).cloned()) {
        w.present();
        return;
    }
    let win = adw::Window::builder().title(format!("{} — Mixer", ctx.s().project.name)).default_width(1200).default_height(760).transient_for(parent).build();
    // Strip buttons such as Export use the project window's actions.
    if let Some(main) = parent.dynamic_cast_ref::<adw::ApplicationWindow>() {
        win.insert_action_group("win", Some(main));
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&build(ctx)));
    win.set_content(Some(&tv));
    win.connect_close_request(move |_| {
        WINDOWS.with(|w| w.borrow_mut().remove(&key));
        glib::Propagation::Proceed
    });
    WINDOWS.with(|w| w.borrow_mut().insert(key, win.clone()));
    win.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(p: &mut Project, name: &str, section: Option<StripSection>) -> PluginSlot {
        PluginSlot { id: p.alloc_id(), uid: name.into(), path: Default::default(), name: name.into(), bypass: false, state: vec![7], sidechain: None, section }
    }

    #[test]
    fn strip_settings_carry_the_type_and_sections() {
        let mut p = Project::new("T", 48000);
        let from = p.add_track(TrackKind::Audio);
        let to = p.add_track(TrackKind::Audio);
        let inserts = vec![
            slot(&mut p, "Drive", Some(StripSection::Pedals)),
            slot(&mut p, "Amp", Some(StripSection::Amp)),
            slot(&mut p, "EQ", None),
            slot(&mut p, "Cab", Some(StripSection::Cabinet)),
        ];
        let ch = p.channel_mut(Some(from)).unwrap();
        ch.kind = Some(StripKind::GuitarDi);
        ch.inserts = inserts;
        let ch = p.channel(Some(from)).unwrap();
        let setting = StripSetting { kind: ch.kind, inserts: ch.inserts.clone(), eq: ch.eq.clone(), instrument: None, midi_fx: vec![] };
        let json = serde_json::to_string(&setting).unwrap();
        let back: StripSetting = serde_json::from_str(&json).unwrap();
        assert_eq!(back, setting);
        apply_strip_setting(&mut p, Some(to), &back);
        let (a, b) = (p.channel(Some(from)).unwrap(), p.channel(Some(to)).unwrap());
        assert_eq!(b.kind, Some(StripKind::GuitarDi));
        let sections = |c: &Channel| c.inserts.iter().map(|s| (s.name.clone(), s.section)).collect::<Vec<_>>();
        assert_eq!(sections(a), sections(b));
        assert!(a.inserts.iter().zip(&b.inserts).all(|(x, y)| x.id != y.id && x.state == y.state));

        // An untyped setting clears the type it replaces.
        let plain = StripSetting { kind: None, inserts: vec![], eq: ChannelEq::default(), instrument: None, midi_fx: vec![] };
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("kind"));
        apply_strip_setting(&mut p, Some(to), &plain);
        assert_eq!(p.channel(Some(to)).unwrap().kind, None);
    }

    #[test]
    fn strip_settings_from_before_strip_types_load() {
        let eq = serde_json::to_string(&ChannelEq::default()).unwrap();
        let json = format!(r#"{{"inserts":[{{"id":3,"uid":"x","path":"","name":"X","bypass":false,"state":""}}],"eq":{eq}}}"#);
        let s: StripSetting = serde_json::from_str(&json).unwrap();
        assert_eq!(s.kind, None);
        assert_eq!(s.inserts[0].section, None);
    }
}
