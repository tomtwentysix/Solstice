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

// The rows from the pan control down have fixed heights so they line up across the mixer; the
// slots above differ from strip to strip and scroll when they do not fit.
const SLOT: i32 = 26;
const EQ_ROW: i32 = 34;
const ICON_ROW: i32 = 28;
const PAN_ROW: i32 = 50;
const DB_ROW: i32 = 26;
const FADER_H: i32 = 210;
const BUTTON_ROW: i32 = 26;
const NAME_ROW: i32 = 26;
const PICTURE_SLOT: i32 = 40;

#[derive(Clone, Copy)]
pub struct Layout {
    pub wide: bool,
    /// Labels over the slots; a typed strip's section labels show regardless.
    pub labels: bool,
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
    {
        let a = gio::SimpleAction::new("new-cue-mix", None);
        let (c, root) = (ctx.clone(), root.clone());
        a.connect_activate(move |_, _| new_cue_mix(&c, &root));
        group.add_action(&a);
    }
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
    let cue = gio::Menu::new();
    cue.append(Some("New Cue Mix…"), Some("mixer.new-cue-mix"));
    options.append_section(None, &cue);
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
    let pictures = gtk::ToggleButton::new();
    pictures.set_icon_name("image-x-generic-symbolic");
    pictures.set_tooltip_text(Some("Pictures on All Strips"));
    pictures.set_active(ctx.s().settings.strip_pictures);
    {
        let c = ctx.clone();
        pictures.connect_toggled(move |b| {
            let mut s = c.m();
            if s.settings.strip_pictures != b.is_active() {
                s.settings.strip_pictures = b.is_active();
                s.settings.save();
                drop(s);
                c.emit(Change::Mixer);
            }
        });
    }
    bar.append(&pictures);
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
            let on = c.s().settings.strip_pictures;
            pictures.set_active(on);
            labels.set_state(&view.labels.get().to_variant());
            low.set_state(&c.s().settings.low_latency.to_variant());
        })
    };
    (bar.upcast(), sync)
}

/// Asks for a name and an interface output pair past the main one, then adds the cue mix.
fn new_cue_mix(ctx: &Ctx, anchor: &gtk::Box) {
    let ports = ctx.s().io.as_ref().map(|io| {
        let names = io.backend.playback_ports();
        (2..io.backend.outputs.saturating_sub(1))
            .step_by(2)
            .map(|first| {
                let short = |k: usize| names.get(k).map(|n| n.rsplit(':').next().unwrap_or(n).to_string());
                let label = match (short(first), short(first + 1)) {
                    (Some(a), Some(b)) => format!("Output {} ({a}, {b})", hardware_name(first as u16, true)),
                    _ => format!("Output {}", hardware_name(first as u16, true)),
                };
                (first as u16, label)
            })
            .collect::<Vec<_>>()
    });
    let refuse = |body: &str| {
        let d = adw::AlertDialog::builder().heading("New Cue Mix").body(body).close_response("ok").build();
        d.add_responses(&[("ok", "_OK")]);
        d.present(Some(anchor));
    };
    let pairs = match ports {
        None => return refuse("Cue mixes play to an interface output, so they need the audio server running."),
        Some(p) if p.is_empty() => return refuse("Cue mixes need an interface output pair besides the main one, and none was found."),
        Some(p) => p,
    };
    let count = ctx.s().project.buses.iter().filter(|b| b.cue).count();
    let entry = adw::EntryRow::builder().title("Name").text(format!("Cue {}", count + 1)).build();
    let labels: Vec<&str> = pairs.iter().map(|(_, l)| l.as_str()).collect();
    let output = adw::ComboRow::builder().title("Output").model(&gtk::StringList::new(&labels)).build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.append(&entry);
    list.append(&output);
    let dialog = adw::AlertDialog::builder()
        .heading("New Cue Mix")
        .body("A headphone mix on its own output. Every track sends to it before its fader, starting at the current mix.")
        .extra_child(&list)
        .close_response("cancel")
        .default_response("create")
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("create", "C_reate")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    let ctx = ctx.clone();
    dialog.connect_response(Some("create"), move |_, _| {
        let Some(&(first, _)) = pairs.get(output.selected() as usize) else { return };
        let name = entry.text().trim().to_string();
        let name = if name.is_empty() { format!("Cue {}", count + 1) } else { name };
        ctx.edit("New Cue Mix", |p| {
            p.add_cue_mix(name, first);
        });
    });
    dialog.present(Some(anchor));
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
                        && let Some(bus) = p.buses.iter().find(|x| x.id == b && !x.cue)
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
    let layout = Layout { wide: view.wide.get(), labels: view.labels.get() };
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
    is_cue: bool,
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
        is_cue: false,
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
                p.buses.iter().find(|b| b.id == id).map(|b| Info { is_bus: true, is_vca: b.vca, is_cue: b.cue, ..base(b.name.clone(), b.channel.clone()) })
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

    // A VCA strip only has its fader, mute and name.
    let vca = i.is_vca;
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let label = |text: &str| {
        if layout.labels {
            body.append(&section_label(text));
        }
    };
    if !vca {
        label("Setting");
        let r = row(SLOT);
        let setting = setting_button(ctx, id, i.channel.kind);
        setting.set_hexpand(true);
        r.append(&setting);
        body.append(&r);
        label("EQ");
        let r = row(EQ_ROW);
        r.append(&crate::channel_eq::thumbnail(ctx, id));
        body.append(&r);

        // The stereo output has no input, only the polarity switch.
        if id.is_some() {
            label("Input");
        }
        let r = row(SLOT);
        let input = input_row(ctx, id, &i);
        input.set_hexpand(true);
        r.append(&input);
        r.append(&invert_button(ctx, id, i.channel.invert));
        body.append(&r);
    }

    let frozen = id.and_then(|t| ctx.s().project.track(t).map(|t| t.freeze.is_some())).unwrap_or(false);
    if let (Some(track), Some(TrackKind::Instrument)) = (id, i.kind) {
        label("MIDI FX");
        let midi = gtk::Box::new(gtk::Orientation::Vertical, 0);
        midi.set_sensitive(!frozen);
        for slot in &i.midi_fx {
            midi.append(&midi_fx_slot(ctx, track, slot));
        }
        midi.append(&empty_midi_fx_slot(ctx, track));
        body.append(&midi);
    }

    if !vca {
        let typed = i.channel.kind.is_some();
        if !typed {
            label("Audio FX");
        }
        let pictures = (typed || ctx.s().settings.strip_pictures).then(|| strip_colour(&i));
        let fx = gtk::Box::new(gtk::Orientation::Vertical, 0);
        fx.set_sensitive(!frozen);
        for r in insert_rows(&i.channel) {
            fx.append(&match r {
                InsertRow::Label(section, at) => section_drop_label(ctx, id, section, at),
                InsertRow::Insert(k) => insert_slot(ctx, id, k, &i.channel.inserts[k], pictures),
                InsertRow::Add(section, at) => {
                    let unused = section.is_some_and(|s| !i.channel.inserts.iter().any(|x| x.section == Some(s)));
                    empty_slot(ctx, id, at, section, pictures.filter(|_| unused))
                }
            });
        }
        body.append(&fx);
    }

    if let Some(owner) = id.filter(|_| !vca && !i.reference) {
        label("Sends");
        let sends = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (k, send) in i.channel.sends.iter().enumerate() {
            sends.append(&send_slot(ctx, owner, k, send));
        }
        sends.append(&add_send_slot(ctx, owner));
        body.append(&sends);
    }

    if i.reference {
        label("Output");
        let r = row(SLOT);
        let out = gtk::Label::new(Some("Reference"));
        out.add_css_class("strip-slot");
        out.add_css_class("dim-label");
        out.set_tooltip_text(Some("Plays to the main outputs while Reference is on"));
        out.set_hexpand(true);
        r.append(&out);
        body.append(&r);
    } else if let Some(owner) = id.filter(|_| !vca) {
        label("Output");
        let r = row(SLOT);
        let out = output_button(ctx, owner, i.channel.output);
        out.set_hexpand(true);
        r.append(&out);
        body.append(&r);
    }
    if !vca {
        label("Automation");
        let r = row(SLOT);
        let mode = crate::automation::mode_dropdown(ctx, id, i.channel.auto_mode);
        mode.add_css_class("strip-slot");
        mode.set_hexpand(true);
        r.append(&mode);
        body.append(&r);
    }
    // Below this the whole mixer scrolls instead, so a short docked mixer still shows the
    // setting, EQ, input and first inserts rather than squeezing them to a sliver.
    let top = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .min_content_height(SLOT * 7)
        .vexpand(true)
        .child(&body)
        .build();
    col.append(&top);

    let r = row(ICON_ROW);
    r.set_halign(gtk::Align::Center);
    r.append(&if i.reference {
        reference_badge().upcast()
    } else if i.is_cue {
        cue_badge().upcast()
    } else {
        type_badge(&i)
    });
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
    } else if !vca && !i.is_cue {
        r.append(&paint_button(ctx, Paint::Solo, id));
    }
    col.append(&r);

    col.append(&name_plate(ctx, id, &i));
    col.upcast()
}

fn section_label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.add_css_class("strip-section");
    l
}

/// A section's label, which takes a dropped plug-in as a plain insert ahead of the section.
fn section_drop_label(ctx: &Ctx, owner: Option<Id>, section: StripSection, at: usize) -> gtk::Widget {
    let l = section_label(section.label());
    dnd::drop_target(ctx, &l, move |ctx, v, copy| dnd::drop_insert(ctx, v, owner, at, true, None, copy));
    file_drop(ctx, &l, owner, section);
    l.upcast()
}

/// One row of a strip's inserts. A typed strip draws each section's label, its inserts and its
/// add slot, with the plain inserts that sit between sections drawn between them; the indices
/// are where a drop on the label or add slot goes.
#[derive(Debug, PartialEq)]
enum InsertRow {
    Label(StripSection, usize),
    Insert(usize),
    Add(Option<StripSection>, usize),
}

fn insert_rows(ch: &Channel) -> Vec<InsertRow> {
    let n = ch.inserts.len();
    let Some(kind) = ch.kind.filter(|_| ch.sections_in_order()) else {
        return (0..n).map(InsertRow::Insert).chain([InsertRow::Add(None, n)]).collect();
    };
    let mut rows = Vec::new();
    let mut k = 0;
    for &section in kind.sections() {
        let end = ch.section_insert_index(section);
        let start = ch.inserts.iter().position(|s| s.section == Some(section)).unwrap_or(end);
        rows.extend((k..start).map(InsertRow::Insert));
        rows.push(InsertRow::Label(section, start));
        rows.extend((start..end).map(InsertRow::Insert));
        rows.push(InsertRow::Add(Some(section), end));
        k = end;
    }
    rows.extend((k..n).map(InsertRow::Insert));
    rows
}

fn strip_colour(i: &Info) -> gdk::RGBA {
    match (i.color, i.is_bus) {
        (Some(c), _) => track_color(c),
        (None, true) => rgba(0x8f8f2a, 1.0),
        (None, false) => rgba(0x813d9c, 1.0),
    }
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

/// The strip types an audio track can have, with None first.
pub fn strip_kinds() -> impl Iterator<Item = Option<StripKind>> {
    std::iter::once(None).chain(StripKind::ALL.map(Some))
}

pub fn set_strip_kind(ctx: &Ctx, track: Id, kind: Option<StripKind>) {
    ctx.edit("Strip Type", |p| {
        if let Some(ch) = p.channel_mut(Some(track)) {
            ch.set_kind(kind);
        }
    });
}

fn setting_button(ctx: &Ctx, id: Option<Id>, kind: Option<StripKind>) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().label(kind.map_or("Setting", |k| k.label())).build();
    b.add_css_class("strip-slot");
    let ctx = ctx.clone();
    b.set_create_popup_func(move |mb| {
        let pop = gtk::Popover::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let has_clip = ctx.s().strip_clipboard.is_some();
        let audio = id.and_then(|i| ctx.s().project.track(i).map(|t| t.kind == TrackKind::Audio)).unwrap_or(false);
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
                        let (output, vca, kind) = (ch.output, ch.vca, ch.kind);
                        let automation = std::mem::take(&mut ch.automation);
                        *ch = Channel { output, vca, kind, automation, auto_mode: ch.auto_mode, ..Channel::default() };
                    }
                })
            }),
        );
        if let Some(track) = id.filter(|_| audio) {
            let current = ctx.s().project.channel(id).and_then(|c| c.kind);
            list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            list.append(&menu_heading("Strip Type"));
            for k in strip_kinds() {
                let name = k.map_or("None", |k| k.label());
                let label = if k == current { format!("✓ {name}") } else { name.to_string() };
                item(&label, true, Box::new(move |c| set_strip_kind(c, track, k)));
            }
        }
        list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let anchor = mb.clone();
        item("Save Channel Strip Setting…", true, Box::new(move |c| save_strip_setting(c, id, &anchor)));
        list.append(&menu_heading("Factory"));
        // Settings with a strip type are for audio tracks, as the type is.
        for f in FACTORY.iter().filter(|f| f.kind.is_none() || audio) {
            let name = f.name.clone();
            item(&f.name, true, Box::new(move |c| load_factory_strip(c, id, &name)));
        }
        let saved = strip_settings();
        if !saved.is_empty() {
            list.append(&menu_heading("Load"));
            for (name, path) in saved {
                item(&name, true, Box::new(move |c| load_strip_setting(c, id, &path)));
            }
        }
        pop.set_child(Some(&list));
        mb.set_popover(Some(&pop));
    });
    b
}

fn menu_heading(text: &str) -> gtk::Label {
    let title = gtk::Label::new(Some(text));
    title.add_css_class("dim-label");
    title.add_css_class("caption-heading");
    title.set_xalign(0.0);
    title.set_margin_start(8);
    title
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

/// A factory strip setting from `data/factory-strips.json`: built-in effects by ID with one of
/// their factory presets and, for those that play one, a file in their library, plus an
/// optional Channel EQ preset.
#[derive(serde::Deserialize)]
struct FactoryStrip {
    name: String,
    #[serde(default)]
    kind: Option<StripKind>,
    #[serde(default)]
    eq: Option<String>,
    inserts: Vec<FactoryInsert>,
}

#[derive(serde::Deserialize)]
struct FactoryInsert {
    plugin: String,
    preset: String,
    #[serde(default)]
    section: Option<StripSection>,
    /// A path inside the effect's library, such as `Guitar/Clean.nam`.
    #[serde(default)]
    file: Option<String>,
}

static FACTORY: std::sync::LazyLock<Vec<FactoryStrip>> =
    std::sync::LazyLock::new(|| serde_json::from_str(include_str!("../../../data/factory-strips.json")).unwrap_or_default());

/// `place` gives the saved file for a path in a library.
fn factory_setting(f: &FactoryStrip, place: impl Fn(&'static crate::library::Library, &str) -> solstice_dsp::slot::FileDesc) -> StripSetting {
    let catalog = crate::host::builtin_catalog();
    let inserts = f
        .inserts
        .iter()
        .filter_map(|i| {
            let info = catalog.iter().find(|c| c.uid == i.plugin)?;
            let mut state = solstice_dsp::presets::state(&i.plugin, &i.preset);
            if let (Some(file), Some(lib)) = (&i.file, crate::library::of_effect(&i.plugin)) {
                state = crate::library::state_with_file(&i.plugin, &state, &place(lib, file));
            }
            Some(PluginSlot {
                id: 0,
                uid: info.uid.clone(),
                path: info.path.clone(),
                name: info.name.clone(),
                bypass: false,
                state,
                sidechain: None,
                section: i.section,
            })
        })
        .collect();
    let eq = f.eq.as_deref().and_then(crate::channel_eq::preset).unwrap_or_default();
    StripSetting { kind: f.kind, inserts, eq, instrument: None, midi_fx: vec![] }
}

/// Library files the setting plays are copied into the project first, as when picked by hand.
fn load_factory_strip(ctx: &Ctx, id: Option<Id>, name: &str) {
    let Some(f) = FACTORY.iter().find(|f| f.name == name) else { return };
    let dir = ctx.s().bundle.audio_dir();
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let Ok(setting) = gio::spawn_blocking(move || factory_setting(f, |lib, file| lib.for_project(file, &dir))).await else { return };
        ctx.edit("Load Channel Strip Setting", |p| apply_strip_setting(p, id, &setting));
    });
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
        ch.eq = setting.eq.clone();
    }
    take_kind(p, id, setting.kind);
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
            ch.sends = sends;
            ch.volume_db = src.volume_db;
            ch.pan = src.pan;
            ch.eq = src.eq.clone();
            ch.invert = src.invert;
        }
        take_kind(p, id, src.kind);
    });
}

/// Gives a channel the strip type of a pasted or loaded strip. Only audio tracks have one, so
/// anywhere else the type and its sections are dropped.
fn take_kind(p: &mut Project, id: Option<Id>, kind: Option<StripKind>) {
    let audio = id.and_then(|i| p.track(i)).is_some_and(|t| t.kind == TrackKind::Audio);
    if let Some(ch) = p.channel_mut(id) {
        if audio {
            ch.kind = kind;
        } else {
            ch.set_kind(None);
        }
    }
}

pub fn input_name(input: u32, stereo: bool) -> String {
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

fn cue_badge() -> gtk::Label {
    let l = gtk::Label::new(Some("Cue"));
    l.add_css_class("cue-badge");
    l.set_valign(gtk::Align::Center);
    l.set_tooltip_text(Some("Cue Mix"));
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
    let color = strip_colour(i);
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
    let (bypassed, uid, index, count, channel) = {
        let mut p = ctx.s().project.clone();
        let bypassed = place.slot(&mut p, sid).is_some_and(|s| s.bypass);
        let uid = place.slot(&mut p, sid).map(|s| s.uid.clone()).unwrap_or_default();
        let (index, count) = place.list(&mut p).map_or((0, 1), |l| (l.iter().position(|s| s.id == sid).unwrap_or(0), l.len()));
        let channel = match place {
            Place::Insert(owner) => p.channel(owner).cloned(),
            _ => None,
        };
        (bypassed, uid, index, count, channel)
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
            // On a typed strip an insert only moves within the order of the sections.
            let keeps_order = |to: usize| {
                channel.clone().is_none_or(|mut ch| {
                    ch.inserts.swap(index, to);
                    ch.sections_in_order()
                })
            };
            item(
                label,
                to >= 0 && (to as usize) < count && keeps_order(to as usize),
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
    let (mono, section) = match place {
        Place::Insert(owner) => {
            let s = ctx.s();
            let ch = s.project.channel(owner);
            let index = ch.and_then(|c| c.inserts.iter().position(|x| x.id == sid)).unwrap_or(0);
            let section = ch.and_then(|c| c.inserts.get(index)).and_then(|x| x.section);
            (crate::variants::slot_is_mono(&s.project, &s.catalog, owner, index), section)
        }
        _ => (false, None),
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
                    new.section = old.section;
                    *old = new;
                }
            });
        })
    };
    pop.set_child(Some(&plugins::list(ctx, Some(kind), mono, section.and_then(|s| s.requires()), pick)));
    pop.connect_closed(|p| p.unparent());
    pop.popup();
}

/// An insert slot: click opens the plug-in, Alt-click bypasses, right-click for the slot menu,
/// drag to move, Alt-drag to copy. Given the strip's colour it is drawn tall with a picture.
fn insert_slot(ctx: &Ctx, owner: Option<Id>, index: usize, slot: &PluginSlot, pictures: Option<gdk::RGBA>) -> gtk::Widget {
    let sid = slot.id;
    let b = match pictures {
        Some(colour) => pictured_button(ctx, slot, colour),
        None => slot_button(&slot.name),
    };
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
    let section = slot.section;
    dnd::drop_target(ctx, &b, move |ctx, v, copy| dnd::drop_insert(ctx, v, owner, index, false, section, copy));
    b.upcast()
}

/// A tall insert: the plug-in's name over a large faint picture of what it is, with the
/// capture, impulse response or preset it is set to below the name.
fn pictured_button(ctx: &Ctx, slot: &PluginSlot, colour: gdk::RGBA) -> gtk::Button {
    let (picture, detail) = {
        let s = ctx.s();
        let cats = s.catalog.iter().find(|c| c.uid == slot.uid).map(|c| crate::categories::of(c, &s.settings.plugin_categories)).unwrap_or_default();
        let detail = s.plugins.get(&slot.id).and_then(|l| {
            let file = l.inst.native().and_then(|n| match (&n.captures, &n.irs) {
                (Some(c), _) => c.desc(),
                (_, Some(i)) => i.desc(),
                _ => None,
            });
            file.map(|f| f.name()).or_else(|| crate::presets::factory_name(&l.inst).map(str::to_string))
        });
        (crate::categories::picture(&cats, slot.section), detail)
    };
    let bg = picture_background(&picture, colour, 0.3);
    let lines = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lines.set_valign(gtk::Align::Center);
    lines.set_margin_start(6);
    lines.set_margin_end(4);
    let name = gtk::Label::new(Some(&slot.name));
    name.add_css_class("slot-name");
    lines.append(&name);
    if let Some(d) = detail {
        let l = gtk::Label::new(Some(&d));
        l.add_css_class("slot-detail");
        lines.append(&l);
    }
    let mut child = lines.first_child();
    while let Some(w) = child {
        if let Some(l) = w.downcast_ref::<gtk::Label>() {
            l.set_xalign(0.0);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.set_max_width_chars(6);
        }
        child = w.next_sibling();
    }
    let o = gtk::Overlay::new();
    o.set_child(Some(&bg));
    o.add_overlay(&lines);
    let b = gtk::Button::new();
    b.set_child(Some(&o));
    b.add_css_class("strip-slot");
    b.add_css_class("pictured");
    b.set_hexpand(true);
    b
}

/// A tall slot's background: the strip colour with a large faint picture anchored to the right
/// edge, running off it and the top.
fn picture_background(picture: &str, colour: gdk::RGBA, tint_alpha: f32) -> Canvas {
    let icon = gdk::Display::default().map(|d| {
        gtk::IconTheme::for_display(&d).lookup_icon(picture, &[], 96, 2, gtk::TextDirection::None, gtk::IconLookupFlags::empty())
    });
    let tint = gdk::RGBA::new(colour.red(), colour.green(), colour.blue(), tint_alpha);
    let bg = Canvas::new(move |c, s, w, h| {
        let clip = gtk::gsk::RoundedRect::from_rect(gtk::graphene::Rect::new(0.0, 0.0, w, h), 5.0);
        s.push_rounded_clip(&clip);
        rect(s, 0.0, 0.0, w, h, &tint);
        if let Some(icon) = &icon {
            s.push_opacity(0.16);
            s.save();
            s.translate(&gtk::graphene::Point::new(w - 82.0, -28.0));
            icon.snapshot_symbolic(s, 96.0, 96.0, &[canvas::fg(c)]);
            s.restore();
            s.pop();
        }
        s.pop();
    });
    bg.set_size_request(-1, PICTURE_SLOT);
    bg
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

/// The empty slot below the last insert, or below a section's: click to pick, or drop a plug-in
/// on it. Amp and Cabinet sections only offer plug-ins of their category.
/// With `pictured`, a section with nothing in it draws this slot as tall as an insert, with the
/// section's picture.
fn empty_slot(ctx: &Ctx, owner: Option<Id>, index: usize, section: Option<StripSection>, pictured: Option<gdk::RGBA>) -> gtk::Widget {
    let mono = {
        let s = ctx.s();
        crate::variants::slot_is_mono(&s.project, &s.catalog, owner, index)
    };
    let tip = match (section, mono) {
        (Some(s), true) => format!("Add to {} (mono)", s.label()),
        (Some(s), false) => format!("Add to {} (stereo)", s.label()),
        (None, true) => "Add Audio Effect (mono)".into(),
        (None, false) => "Add Audio Effect (stereo)".into(),
    };
    let require = section.and_then(|s| s.requires());
    let b = plugins::picker_button(ctx, plugins::Kind::Effect, mono, require, &tip, {
        let ctx = ctx.clone();
        move |info| {
            let info = info.clone();
            ctx.edit("Add Plug-in", |p| {
                add_insert(p, owner, section, &info);
            });
        }
    });
    b.add_css_class("strip-slot");
    b.add_css_class("empty");
    b.set_hexpand(true);
    if let (Some(s), Some(colour)) = (section, pictured) {
        let label = gtk::Label::new(Some(&format!("+ {}", s.label())));
        label.add_css_class("slot-detail");
        label.set_xalign(0.0);
        label.set_margin_start(6);
        let o = gtk::Overlay::new();
        o.set_child(Some(&picture_background(&crate::categories::section_picture(s), colour, 0.12)));
        o.add_overlay(&label);
        b.set_child(Some(&o));
        b.add_css_class("pictured");
    }
    dnd::drop_target(ctx, &b, move |ctx, v, copy| dnd::drop_insert(ctx, v, owner, index, true, section, copy));
    if let Some(section) = section {
        file_drop(ctx, &b, owner, section);
    }
    b.upcast()
}

/// Adds a plug-in at the end of a section, or of the strip, and returns its slot's ID.
fn add_insert(p: &mut Project, owner: Option<Id>, section: Option<StripSection>, info: &solstice_vst3::ClassInfo) -> Option<Id> {
    let mut slot = plugins::new_slot(p, info);
    slot.section = section;
    let id = slot.id;
    let ch = p.channel_mut(owner)?;
    let at = section.map_or(ch.inserts.len(), |s| ch.section_insert_index(s));
    ch.inserts.insert(at, slot);
    Some(id)
}

/// Amp and Cabinet sections take dropped captures and impulse responses, or zips of them: they
/// go into the library and play in a new Neural Amp Modeler or IR loader at the section's end.
fn file_drop(ctx: &Ctx, w: &impl IsA<gtk::Widget>, owner: Option<Id>, section: StripSection) {
    use crate::library::{self, CAPTURES, IRS};
    let (uid, lib) = match section {
        StripSection::Amp => ("solstice:nam", &CAPTURES),
        StripSection::Cabinet => ("solstice:ir", &IRS),
        StripSection::Pedals | StripSection::Effects => return,
    };
    let t = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let w2 = w.clone().upcast::<gtk::Widget>();
    t.connect_enter(move |_, _, _| {
        w2.add_css_class("drop-hover");
        gdk::DragAction::COPY
    });
    let w3 = w.clone().upcast::<gtk::Widget>();
    t.connect_leave(move |_| w3.remove_css_class("drop-hover"));
    let ctx = ctx.clone();
    t.connect_drop(move |_, v, _, _| {
        let Ok(list) = v.get::<gdk::FileList>() else { return false };
        let files: Vec<std::path::PathBuf> = list.files().iter().filter_map(|f| f.path()).filter(|p| lib.is_importable(p)).collect();
        let Some(info) = crate::host::builtin_catalog().into_iter().find(|c| c.uid == uid) else { return false };
        if files.is_empty() {
            return false;
        }
        let kind = ctx.s().project.channel(owner).and_then(|c| c.kind);
        let folder = lib.folder(lib.first_folder(kind));
        let add = move |p: &mut Project| add_insert(p, owner, Some(section), &info);
        match section {
            StripSection::Amp => library::import_and_add::<solstice_dsp::nam::Capture>(&ctx, lib, files, folder, add),
            _ => library::import_and_add::<solstice_dsp::ir::Ir>(&ctx, lib, files, folder, add),
        }
        true
    });
    w.add_controller(t);
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
    let b = plugins::picker_button(ctx, plugins::Kind::MidiFx, false, None, "Add MIDI Effect", move |info| {
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
            let b = plugins::picker_button(ctx, plugins::Kind::Instrument, false, None, "Choose Instrument", move |info| {
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
        if mode != SendMode::PostPan {
            let pan = gestures::knob(
                &ctx2,
                Some(owner),
                KnobSpec {
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                    bipolar: true,
                    label: "Send Pan",
                    gang: false,
                    rebuild: true,
                    compact: false,
                    get: Rc::new(move |p, id| p.channel(id).and_then(|c| c.sends.get(k)).and_then(|s| s.pan).unwrap_or(0.0)),
                    set: Rc::new(move |p, id, v| {
                        if let Some(s) = p.channel_mut(id).and_then(|c| c.sends.get_mut(k)) {
                            s.pan = Some(v);
                        }
                    }),
                    fmt: gestures::fmt_pan,
                },
            );
            pan.set_halign(gtk::Align::Center);
            list.append(&pan);
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
        p.buses
            .iter()
            .filter(|b| b.id != owner && !b.vca && !(include_master && b.cue))
            .map(|b| (b.id, b.name.clone(), p.would_loop(owner, b.id)))
            .collect()
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
                p.channel_mut(Some(owner)).unwrap().sends.push(Send { bus, level_db: 0.0, mode: SendMode::PostPan, pan: None });
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
    fn only_audio_tracks_take_a_pasted_or_loaded_type() {
        let mut p = Project::new("T", 48000);
        let keys = p.add_track(TrackKind::Instrument);
        let bus = p.add_bus();
        let amp = slot(&mut p, "Amp", Some(StripSection::Amp));
        let setting = StripSetting { kind: Some(StripKind::GuitarDi), inserts: vec![amp], eq: ChannelEq::default(), instrument: None, midi_fx: vec![] };
        for id in [Some(keys), Some(bus), None] {
            apply_strip_setting(&mut p, id, &setting);
            let ch = p.channel(id).unwrap();
            assert_eq!(ch.kind, None);
            assert_eq!(ch.inserts.iter().map(|s| (s.name.as_str(), s.section)).collect::<Vec<_>>(), [("Amp", None)]);
        }
    }

    #[test]
    fn adding_to_a_section_goes_after_its_last_insert() {
        use StripSection::*;
        let mut p = Project::new("T", 48000);
        let t = p.add_track(TrackKind::Audio);
        let inserts = vec![slot(&mut p, "Drive", Some(Pedals)), slot(&mut p, "Amp", Some(Amp)), slot(&mut p, "Delay", Some(Effects))];
        let ch = p.channel_mut(Some(t)).unwrap();
        ch.kind = Some(StripKind::GuitarDi);
        ch.inserts = inserts;
        let catalog = crate::host::builtin_catalog();
        let nam = catalog.iter().find(|c| c.uid == "solstice:nam").unwrap();
        let ir = catalog.iter().find(|c| c.uid == "solstice:ir").unwrap();
        let a = add_insert(&mut p, Some(t), Some(Amp), nam).unwrap();
        let c = add_insert(&mut p, Some(t), Some(Cabinet), ir).unwrap();
        let ch = p.channel(Some(t)).unwrap();
        let ids: Vec<Id> = ch.inserts.iter().map(|s| s.id).collect();
        assert_eq!(ids[2..4], [a, c]);
        assert_eq!(ch.inserts[2].section, Some(Amp));
        assert_eq!(ch.inserts[3].section, Some(Cabinet));
        assert!(ch.sections_in_order());
    }

    #[test]
    fn typed_strips_draw_their_sections_in_order() {
        use InsertRow::*;
        use StripSection::*;
        let mut p = Project::new("T", 48000);
        let mut ch = Channel { kind: Some(StripKind::GuitarDi), ..Channel::default() };
        for (name, section) in [("Drive", Some(Pedals)), ("Amp", Some(Amp)), ("EQ", None), ("Delay", Some(Effects))] {
            ch.inserts.push(slot(&mut p, name, section));
        }
        assert_eq!(
            insert_rows(&ch),
            [
                Label(Pedals, 0),
                Insert(0),
                Add(Some(Pedals), 1),
                Label(Amp, 1),
                Insert(1),
                Add(Some(Amp), 2),
                Insert(2),
                Label(Cabinet, 3),
                Add(Some(Cabinet), 3),
                Label(Effects, 3),
                Insert(3),
                Add(Some(Effects), 4),
            ]
        );
        // Every insert is drawn once, in order.
        let drawn: Vec<usize> = insert_rows(&ch).iter().filter_map(|r| if let Insert(k) = r { Some(*k) } else { None }).collect();
        assert_eq!(drawn, [0, 1, 2, 3]);

        ch.set_kind(None);
        assert_eq!(insert_rows(&ch), [Insert(0), Insert(1), Insert(2), Insert(3), Add(None, 4)]);
        let empty = Channel { kind: Some(StripKind::BassDi), ..Channel::default() };
        assert_eq!(insert_rows(&empty).iter().filter(|r| matches!(r, Add(Some(_), 0))).count(), 4);
    }

    /// Points at the library without copying anything.
    fn in_library(lib: &'static crate::library::Library, file: &str) -> solstice_dsp::slot::FileDesc {
        let src = lib.root().join(file);
        solstice_dsp::slot::FileDesc { file: src.clone(), source: src }
    }

    #[test]
    fn factory_strip_settings_use_real_effects_and_presets() {
        let names: Vec<&str> = FACTORY.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["Guitar Clean", "Guitar Crunch", "Guitar High Gain", "Bass", "Lead Vocal", "Kick", "Snare", "Overheads", "Drum Bus", "Guitar Bus", "Mix Bus", "Reverb Send"]
        );
        for f in FACTORY.iter() {
            if let Some(eq) = &f.eq {
                assert!(crate::channel_eq::preset(eq).is_some(), "{}: no Channel EQ preset {eq}", f.name);
            }
            for i in &f.inserts {
                assert!(solstice_dsp::presets::find(&i.plugin, &i.preset).is_some(), "{}: no {} preset {}", f.name, i.plugin, i.preset);
                let lib = crate::library::of_effect(&i.plugin);
                assert_eq!(lib.is_some(), i.file.is_some(), "{}: {} needs a file, and only it", f.name, i.plugin);
                if let (Some(lib), Some(file)) = (lib, &i.file) {
                    let starter = crate::library::STARTER.iter().any(|s| s.lib.ext() == lib.ext() && s.path == file);
                    assert!(starter, "{}: {file} is not starter content", f.name);
                }
            }
            let s = factory_setting(f, in_library);
            assert_eq!(s.inserts.len(), f.inserts.len(), "{}", f.name);
            assert!(s.inserts.iter().all(|i| !i.state.is_empty()));
            assert_eq!(s.eq.enabled, f.eq.is_some());
            let ch = Channel { kind: s.kind, inserts: s.inserts, ..Channel::default() };
            assert!(ch.sections_in_order(), "{}", f.name);
        }
    }

    #[test]
    fn loading_a_guitar_strip_setting_fills_the_sections() {
        use StripSection::*;
        let mut p = Project::new("T", 48000);
        let t = p.add_track(TrackKind::Audio);
        let f = FACTORY.iter().find(|f| f.name == "Guitar High Gain").unwrap();
        apply_strip_setting(&mut p, Some(t), &factory_setting(f, in_library));
        let ch = p.channel(Some(t)).unwrap();
        assert_eq!(ch.kind, Some(StripKind::GuitarDi));
        let got: Vec<(&str, Option<StripSection>)> = ch.inserts.iter().map(|s| (s.uid.as_str(), s.section)).collect();
        assert_eq!(got, [("solstice:gate", Some(Pedals)), ("solstice:nam", Some(Amp)), ("solstice:ir", Some(Cabinet))]);
        assert!(ch.sections_in_order());
        let bank = solstice_dsp::Bank::new(&solstice_dsp::nam::NAM);
        let capture = solstice_dsp::slot::set_state::<solstice_dsp::nam::Capture>(&bank, &ch.inserts[1].state).unwrap();
        assert_eq!(capture.source, crate::library::CAPTURES.root().join("Guitar/High Gain.nam"));
        let bank = solstice_dsp::Bank::new(&solstice_dsp::ir::IR);
        let ir = solstice_dsp::slot::set_state::<solstice_dsp::ir::Ir>(&bank, &ch.inserts[2].state).unwrap();
        assert_eq!(ir.source, crate::library::IRS.root().join("4x12 Cabinet.wav"));

        // On a bus the strip type does not apply.
        let b = p.add_bus();
        apply_strip_setting(&mut p, Some(b), &factory_setting(f, in_library));
        assert_eq!(p.channel(Some(b)).unwrap().kind, None);
    }

    #[test]
    fn loading_a_factory_strip_replaces_the_inserts() {
        let mut p = Project::new("T", 48000);
        let t = p.add_track(TrackKind::Audio);
        let old = slot(&mut p, "Old", None);
        let ch = p.channel_mut(Some(t)).unwrap();
        ch.inserts.push(old);
        ch.kind = Some(StripKind::GuitarDi);
        let kick = FACTORY.iter().find(|f| f.name == "Kick").unwrap();
        apply_strip_setting(&mut p, Some(t), &factory_setting(kick, in_library));
        let ch = p.channel(Some(t)).unwrap();
        assert_eq!(ch.kind, None);
        let uids: Vec<&str> = ch.inserts.iter().map(|s| s.uid.as_str()).collect();
        assert_eq!(uids, ["solstice:gate", "solstice:compressor"]);
        assert!(ch.inserts.iter().all(|s| s.id != 0));
        assert_eq!(ch.eq, crate::channel_eq::preset("Kick").unwrap());
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
