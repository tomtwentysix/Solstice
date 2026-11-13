use crate::studio::{Change, Ctx};
use crate::variants;
use adw::prelude::*;
use gtk::glib;
use crate::categories;
use crate::settings::PluginGroup;
use solstice_model::{Id, PluginSlot, Project, TrackKind};
use solstice_vst3::ClassInfo;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

pub fn new_slot(p: &mut Project, info: &ClassInfo) -> PluginSlot {
    PluginSlot {
        id: p.alloc_id(),
        uid: info.uid.clone(),
        path: info.path.clone(),
        name: info.name.clone(),
        bypass: false,
        state: Vec::new(),
        sidechain: None,
        section: None,
    }
}

const MONO_TO_STEREO: &str = "Mono → Stereo";

/// The groups a family is listed under; by type a plug-in with several categories appears
/// under each.
fn groups(f: &Family, by: PluginGroup, user: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    if f.to_stereo {
        return vec![MONO_TO_STEREO.into()];
    }
    match by {
        PluginGroup::Type => categories::of(&f.representative, user),
        PluginGroup::Developer => {
            let v = f.representative.vendor.trim();
            vec![if v.is_empty() { "Unknown Developer".into() } else { v.to_string() }]
        }
        PluginGroup::Name => {
            let first = f.name.chars().next().filter(|c| c.is_alphabetic());
            vec![first.map_or("#".into(), |c| c.to_uppercase().collect())]
        }
    }
}

/// One listed plug-in family: its variants, the class to load for this slot, and whether loading
/// it turns a mono strip stereo.
struct Family {
    name: String,
    summary: String,
    pick: ClassInfo,
    representative: ClassInfo,
    to_stereo: bool,
    key: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Effect,
    Instrument,
    MidiFx,
}

pub fn kind(c: &ClassInfo) -> Kind {
    if solstice_dsp::is_midi_fx(&c.subcategories) {
        Kind::MidiFx
    } else if c.is_instrument() {
        Kind::Instrument
    } else {
        Kind::Effect
    }
}

fn families(catalog: &[ClassInfo], only: Option<Kind>, mono_slot: bool) -> Vec<Family> {
    let mut seen: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut out = Vec::new();
    for c in catalog.iter().filter(|c| only.is_none_or(|k| kind(c) == k)) {
        let v = variants::parse(c);
        let id = (c.path.clone(), v.family.clone());
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        let members = variants::members(catalog, c);
        let fit = variants::fit(&members, mono_slot);
        if fit == variants::Fit::No {
            continue;
        }
        let Some(pick) = variants::choose(&members, mono_slot, None) else { continue };
        let mut chans: Vec<variants::Channels> = members.iter().filter_map(|(_, v)| v.channels).collect();
        chans.sort();
        chans.dedup();
        let mut bands: Vec<u32> = members.iter().filter_map(|(_, v)| v.bands).collect();
        bands.sort();
        bands.dedup();
        let mut summary = vec![c.vendor.clone()];
        if !chans.is_empty() {
            summary.push(chans.iter().map(|c| c.label()).collect::<Vec<_>>().join(", "));
        }
        if !bands.is_empty() {
            summary.push(format!("{} bands", bands.iter().map(|b| b.to_string()).collect::<Vec<_>>().join("/")));
        }
        let name = if v.family.is_empty() { c.name.clone() } else { v.family.clone() };
        out.push(Family {
            key: format!("{name} {}", c.vendor).to_lowercase(),
            name,
            summary: summary.join(" · "),
            pick: pick.clone(),
            representative: c.clone(),
            to_stereo: fit == variants::Fit::MonoToStereo,
        });
    }
    out
}

fn family_row(ctx: &Ctx, f: &Family, on_pick: &Rc<dyn Fn(&ClassInfo)>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&f.name)).activatable(true).build();
    row.set_subtitle(&glib::markup_escape_text(&f.summary));
    // Drops resolve the family to the variant that suits the target slot.
    let payload = format!("lib:{}", f.representative.uid);
    let src = gtk::DragSource::new();
    src.set_actions(gtk::gdk::DragAction::COPY);
    src.connect_prepare(move |_, _, _| Some(gtk::gdk::ContentProvider::for_value(&payload.to_value())));
    row.add_controller(src);
    let (info, pick) = (f.pick.clone(), on_pick.clone());
    row.connect_activated(move |_| pick(&info));
    let menu = gtk::GestureClick::builder().button(3).build();
    let (ctx, rep, r) = (ctx.clone(), f.representative.clone(), row.clone());
    menu.connect_pressed(move |_, _, x, y| category_menu(&ctx, &r, x, y, &rep));
    row.add_controller(menu);
    row
}

/// Right-click on a listed plug-in: tick the categories it is listed and accepted under.
fn category_menu(ctx: &Ctx, anchor: &adw::ActionRow, x: f64, y: f64, info: &ClassInfo) {
    let menu = crate::context_menu::Menu::at(anchor, x, y);
    let (uids, mut known, current) = {
        let s = ctx.s();
        let uids: Vec<String> = variants::members(&s.catalog, info).iter().map(|(c, _)| c.uid.clone()).collect();
        let mut known: Vec<String> = categories::GUITAR.iter().map(|c| c.to_string()).collect();
        for c in s.available_plugins().iter().filter(|c| kind(c) == kind(info)) {
            known.extend(categories::of(c, &s.settings.plugin_categories));
        }
        (uids, known, categories::of(info, &s.settings.plugin_categories))
    };
    known.extend(current.iter().cloned());
    known.sort_by_key(|c| c.to_lowercase());
    known.dedup();
    let chosen = Rc::new(RefCell::new(current));
    let changed = Rc::new(std::cell::Cell::new(false));
    menu.heading("Category");
    let store = {
        let (ctx, uids, changed) = (ctx.clone(), uids.clone(), changed.clone());
        move |cats: Option<Vec<String>>| {
            {
                let mut s = ctx.m();
                for uid in &uids {
                    match &cats {
                        Some(c) => s.settings.plugin_categories.insert(uid.clone(), c.clone()),
                        None => s.settings.plugin_categories.remove(uid),
                    };
                }
                s.settings.save();
            }
            changed.set(true);
        }
    };
    let store = Rc::new(store);
    for cat in known {
        let check = gtk::CheckButton::with_label(&cat);
        check.set_margin_start(6);
        check.set_margin_end(6);
        check.set_active(chosen.borrow().contains(&cat));
        let (chosen, store) = (chosen.clone(), store.clone());
        check.connect_toggled(move |b| {
            let mut c = chosen.borrow_mut();
            c.retain(|x| *x != cat);
            if b.is_active() {
                c.push(cat.clone());
            }
            store(Some(c.clone()));
        });
        menu.list.append(&check);
    }
    menu.separator();
    let custom = uids.iter().any(|u| ctx.s().settings.plugin_categories.contains_key(u));
    menu.item("Use Default Category", custom, move || store(None));
    // Lists rebuild on a catalog change, which would take this menu's row away while it is open.
    let ctx = ctx.clone();
    menu.pop.connect_closed(move |_| {
        if changed.get() {
            ctx.emit(Change::Catalog);
        }
    });
    menu.show();
}

type Sections = Vec<(adw::ExpanderRow, Vec<(adw::ActionRow, String)>)>;

/// A searchable list of installed plug-ins, one entry per family, grouped into collapsible
/// sections by type, developer or name. Only families that suit the slot's channel format are
/// offered; on a mono slot, stereo-only ones are grouped under "Mono → Stereo". `require` limits
/// it to one category, for a strip's Amp and Cabinet sections.
pub fn list(ctx: &Ctx, only: Option<Kind>, mono_slot: bool, require: Option<&str>, on_pick: Rc<dyn Fn(&ClassInfo)>) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search Plug-ins"));
    search.set_hexpand(true);
    top.append(&search);
    let labels: Vec<String> = PluginGroup::ALL.iter().map(|g| format!("By {}", g.label())).collect();
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let group_by = gtk::DropDown::from_strings(&refs);
    group_by.set_tooltip_text(Some("Group By"));
    let by = ctx.s().settings.plugin_group;
    group_by.set_selected(PluginGroup::ALL.iter().position(|g| *g == by).unwrap_or(0) as u32);
    top.append(&group_by);
    col.append(&top);
    let listbox = gtk::ListBox::new();
    listbox.add_css_class("boxed-list");
    listbox.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&listbox)
        .vexpand(true)
        .min_content_height(260)
        .max_content_height(520)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    col.append(&scroller);

    let mut fams = families(&ctx.s().available_plugins(), only, mono_slot);
    if let Some(r) = require {
        let user = ctx.s().settings.plugin_categories.clone();
        fams.retain(|f| categories::of(&f.representative, &user).iter().any(|c| c == r));
    }
    let fams = Rc::new(fams);
    if fams.is_empty() {
        let (title, description) = match require {
            Some(r) => (format!("No {r} Plug-ins"), format!("Right-click a plug-in in the Library to give it the {r} category")),
            None => ("No Plug-ins".into(), "Install VST3 plug-ins in ~/.vst3 or add a folder in Preferences".into()),
        };
        let empty = adw::StatusPage::builder()
            .icon_name("application-x-addon-symbolic")
            .title(title)
            .description(description)
            .build();
        empty.add_css_class("compact");
        listbox.set_placeholder(Some(&empty));
    }
    let sections: Rc<RefCell<Sections>> = Rc::default();

    let filter = {
        let sections = sections.clone();
        move |q: &str| {
            let sections = sections.borrow();
            for (exp, rows) in sections.iter() {
                let mut any = false;
                for (row, key) in rows {
                    let hit = q.is_empty() || key.contains(q);
                    row.set_visible(hit);
                    any |= hit;
                }
                exp.set_visible(any);
                exp.set_expanded(!q.is_empty() && any || sections.len() == 1);
            }
        }
    };
    let fill = {
        let (ctx, listbox, sections) = (ctx.clone(), listbox.clone(), sections.clone());
        move |by: PluginGroup| {
            listbox.remove_all();
            let user = ctx.s().settings.plugin_categories.clone();
            let mut grouped: BTreeMap<String, Vec<&Family>> = BTreeMap::new();
            for f in fams.iter() {
                for g in groups(f, by, &user) {
                    grouped.entry(g).or_default().push(f);
                }
            }
            // The Mono → Stereo group goes last, after the regular ones.
            let mut ordered: Vec<(&String, &Vec<&Family>)> = grouped.iter().collect();
            ordered.sort_by_key(|(name, _)| (name.as_str() == MONO_TO_STEREO, name.to_lowercase()));
            let mut out: Sections = Vec::new();
            for (name, items) in ordered {
                let exp = adw::ExpanderRow::builder()
                    .title(glib::markup_escape_text(name))
                    .subtitle(items.len().to_string())
                    .build();
                let rows = items
                    .iter()
                    .map(|f| {
                        let row = family_row(&ctx, f, &on_pick);
                        exp.add_row(&row);
                        (row, f.key.clone())
                    })
                    .collect();
                listbox.append(&exp);
                out.push((exp, rows));
            }
            if let [(exp, _)] = out.as_slice() {
                exp.set_expanded(true);
            }
            *sections.borrow_mut() = out;
        }
    };
    fill(by);
    {
        let (ctx, search, filter) = (ctx.clone(), search.clone(), filter.clone());
        group_by.connect_selected_notify(move |d| {
            let by = PluginGroup::ALL[(d.selected() as usize).min(PluginGroup::ALL.len() - 1)];
            {
                let mut s = ctx.m();
                s.settings.plugin_group = by;
                s.settings.save();
            }
            fill(by);
            filter(&search.text().to_lowercase());
        });
    }
    search.connect_search_changed(move |s| filter(&s.text().to_lowercase()));
    col
}

pub fn picker_button(
    ctx: &Ctx,
    only: Kind,
    mono_slot: bool,
    require: Option<&'static str>,
    tip: &str,
    on_pick: impl Fn(&ClassInfo) + 'static,
) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text(tip).build();
    b.add_css_class("flat");
    let pop = gtk::Popover::new();
    pop.set_size_request(320, -1);
    let pop2 = pop.clone();
    let on_pick: Rc<dyn Fn(&ClassInfo)> = Rc::new(move |i| {
        pop2.popdown();
        on_pick(i);
    });
    let ctx = ctx.clone();
    pop.connect_show(move |p| p.set_child(Some(&list(&ctx, Some(only), mono_slot, require, on_pick.clone()))));
    b.set_popover(Some(&pop));
    b
}

/// The variant of `info`'s family that suits a slot, for drops from the Library.
pub fn resolve(ctx: &Ctx, info: &ClassInfo, mono_slot: bool) -> Option<ClassInfo> {
    let s = ctx.s();
    let members = variants::members(&s.catalog, info);
    if variants::fit(&members, mono_slot) == variants::Fit::No {
        return None;
    }
    variants::choose(&members, mono_slot, None).cloned()
}

/// Whether the next insert on the selected channel would see a mono signal.
fn selection_is_mono(ctx: &Ctx) -> bool {
    let s = ctx.s();
    let sel = s.selected_track;
    let next = s.project.channel(sel).map_or(0, |c| c.inserts.len());
    variants::slot_is_mono(&s.project, &s.catalog, sel, next)
}

/// The Library pane: picking a plug-in loads it onto the selected track.
/// It lists what suits the selected channel's next insert slot.
pub fn library(ctx: &Ctx) -> gtk::Widget {
    let holder = adw::Bin::new();
    holder.set_margin_start(8);
    holder.set_margin_end(8);
    holder.set_margin_top(8);
    let last = Rc::new(std::cell::Cell::new(None::<bool>));
    let fill = {
        let ctx = ctx.clone();
        let holder = holder.clone();
        move |force: bool| {
            let mono = selection_is_mono(&ctx);
            if !force && last.get() == Some(mono) {
                return;
            }
            last.set(Some(mono));
            let ctx2 = ctx.clone();
            let pick: Rc<dyn Fn(&ClassInfo)> = Rc::new(move |info| apply_to_selection(&ctx2, info));
            holder.set_child(Some(&list(&ctx, None, mono, None, pick)));
        }
    };
    fill(true);
    ctx.on(move |c| match c {
        Change::Catalog => fill(true),
        Change::Project | Change::Selection => fill(false),
        _ => {}
    });
    holder.upcast()
}

fn apply_to_selection(ctx: &Ctx, info: &ClassInfo) {
    let info = info.clone();
    let sel = ctx.s().selected_track;
    let kind = sel.and_then(|id| ctx.s().project.track(id).map(|t| t.kind));
    if info.is_instrument() {
        // Instrument tracks are stereo, so load the stereo variant whatever was listed.
        let info = resolve(ctx, &info, false).unwrap_or(info);
        ctx.edit("Set Instrument", |p| {
            let id = match (sel, kind) {
                (Some(id), Some(TrackKind::Instrument)) => id,
                _ => p.add_track(TrackKind::Instrument),
            };
            let slot = new_slot(p, &info);
            let t = p.track_mut(id).unwrap();
            t.name = variants::parse(&info).family;
            t.instrument = Some(slot);
        });
    } else if crate::plugins::kind(&info) == Kind::MidiFx {
        if kind != Some(TrackKind::Instrument) {
            ctx.m().message = Some("MIDI effects go on an instrument track: select one first".into());
            ctx.emit(Change::Message);
            return;
        }
        ctx.edit("Add MIDI Effect", |p| {
            let slot = new_slot(p, &info);
            if let Some(t) = sel.and_then(|id| p.track_mut(id)) {
                t.midi_fx.push(slot);
            }
        });
    } else {
        let (is_vca, name) = {
            let p = &ctx.s().project;
            let bus = sel.and_then(|id| p.buses.iter().find(|b| b.id == id));
            let name = sel.and_then(|id| p.track(id).map(|t| t.name.clone())).or(bus.map(|b| b.name.clone())).unwrap_or("Stereo Out".into());
            (bus.is_some_and(|b| b.vca), name)
        };
        if is_vca {
            ctx.m().message = Some("A VCA fader carries no audio, so it can't hold plug-ins".into());
            ctx.emit(Change::Message);
            return;
        }
        ctx.edit("Add Plug-in", |p| {
            let mut slot = new_slot(p, &info);
            if let Some(c) = p.channel_mut(sel) {
                // Added at the end of a typed strip, it goes in the last section.
                slot.section = c.kind.and_then(|k| k.sections().last().copied());
                c.inserts.push(slot);
            }
        });
        if sel.is_none() {
            ctx.m().message = Some(format!("Added {} to {name}", info.name));
            ctx.emit(Change::Message);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SlotPos {
    Insert { owner: Option<Id>, index: usize },
    Instrument(Id),
    MidiFx { track: Id, index: usize },
}

/// The channel a slot's plug-in sits on, for an instrument or MIDI effect its track's.
pub(crate) fn slot_channel(p: &Project, sid: Id) -> Option<Option<Id>> {
    locate(p, sid).map(|(pos, _)| match pos {
        SlotPos::Insert { owner, .. } => owner,
        SlotPos::Instrument(t) | SlotPos::MidiFx { track: t, .. } => Some(t),
    })
}

fn on_frozen_track(p: &Project, pos: SlotPos) -> bool {
    let track = match pos {
        SlotPos::Insert { owner, .. } => owner,
        SlotPos::Instrument(t) | SlotPos::MidiFx { track: t, .. } => Some(t),
    };
    track.and_then(|t| p.track(t)).is_some_and(|t| t.freeze.is_some())
}

fn locate(p: &Project, sid: Id) -> Option<(SlotPos, PluginSlot)> {
    if let Some(i) = p.master.inserts.iter().position(|s| s.id == sid) {
        return Some((SlotPos::Insert { owner: None, index: i }, p.master.inserts[i].clone()));
    }
    for t in &p.tracks {
        if let Some(i) = t.channel.inserts.iter().position(|s| s.id == sid) {
            return Some((SlotPos::Insert { owner: Some(t.id), index: i }, t.channel.inserts[i].clone()));
        }
        if let Some(s) = t.instrument.as_ref().filter(|s| s.id == sid) {
            return Some((SlotPos::Instrument(t.id), s.clone()));
        }
        if let Some(i) = t.midi_fx.iter().position(|s| s.id == sid) {
            return Some((SlotPos::MidiFx { track: t.id, index: i }, t.midi_fx[i].clone()));
        }
    }
    for b in &p.buses {
        if let Some(i) = b.channel.inserts.iter().position(|s| s.id == sid) {
            return Some((SlotPos::Insert { owner: Some(b.id), index: i }, b.channel.inserts[i].clone()));
        }
    }
    None
}

thread_local! {
    static WINDOWS: std::cell::RefCell<std::collections::HashMap<(usize, Id), adw::Window>> = Default::default();
}

/// Opens a plug-in: the host window (header with variant pickers, bypass and generic
/// controls) with the plug-in's own interface inside it, or beside it in a window of its own
/// when it can't be embedded.
pub fn open_editor(ctx: &Ctx, slot: Id, anchor: &impl IsA<gtk::Widget>) {
    let Some(inst) = ctx.s().plugins.get(&slot).map(|l| l.inst.clone()) else { return };
    if floats(&inst) && !inst.editor_open() {
        inst.open_editor();
    }
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&(ctx.key(), slot)).cloned()) {
        w.present();
        return;
    }
    let parent = anchor.root().and_downcast::<gtk::Window>();
    host_window(ctx, slot, &inst, parent.as_ref());
}

/// Whether the plug-in's interface needs a window of its own: it can't be embedded, or there
/// is no X server to embed it on.
fn floats(inst: &crate::host::Host) -> bool {
    inst.has_editor() && (!inst.can_embed() || crate::embed::x11_display().is_none())
}

fn host_window(ctx: &Ctx, sid: Id, inst: &crate::host::Host, parent: Option<&gtk::Window>) {
    let info = inst.info();
    let Some((pos, slot)) = locate(&ctx.s().project, sid) else { return };
    let variant = variants::parse(&info);
    let family = if variant.family.is_empty() { info.name.clone() } else { variant.family.clone() };
    let win = adw::Window::builder().title(&family).default_width(560).default_height(520).build();
    let embed = inst.can_embed().then(crate::embed::x11_display).flatten();
    match &embed {
        // Windows on another display can't be transient for the main window.
        Some(display) => win.set_display(display),
        None => win.set_transient_for(parent),
    }
    let header = adw::HeaderBar::new();

    let (members, mono_slot): (Vec<(ClassInfo, variants::Variant)>, bool) = {
        let s = ctx.s();
        let mono = match pos {
            SlotPos::Insert { owner, index } => variants::slot_is_mono(&s.project, &s.catalog, owner, index),
            SlotPos::Instrument(_) | SlotPos::MidiFx { .. } => false,
        };
        let m = variants::members(&s.catalog, &info).into_iter().map(|(c, v)| (c.clone(), v)).collect();
        (m, mono)
    };
    let mut chans: Vec<variants::Channels> = members
        .iter()
        .filter_map(|(_, v)| v.channels)
        .filter(|c| mono_slot || c.is_stereo())
        .collect();
    chans.sort();
    chans.dedup();
    let bands_for = |ch: Option<variants::Channels>| -> Vec<u32> {
        let mut b: Vec<u32> = members.iter().filter(|(_, v)| v.channels == ch).filter_map(|(_, v)| v.bands).collect();
        b.sort();
        b.dedup();
        b
    };
    let bands = bands_for(variant.channels);

    // Swaps the slot to another variant of this family, then reopens the window for it.
    let swap = {
        let (ctx, members, win) = (ctx.clone(), members.clone(), win.clone());
        Rc::new(move |ch: Option<variants::Channels>, b: Option<u32>| {
            let candidates: Vec<(&ClassInfo, variants::Variant)> =
                members.iter().filter(|(_, v)| v.channels == ch).map(|(c, v)| (c, v.clone())).collect();
            let target = candidates
                .iter()
                .find(|(_, v)| v.bands == b)
                .map(|(c, _)| *c)
                .or_else(|| variants::choose(&candidates, ch == Some(variants::Channels::Mono), b));
            let Some(target) = target.cloned() else { return };
            if target.uid == info.uid {
                return;
            }
            let Some((pos, slot)) = locate(&ctx.s().project, sid) else { return };
            // Siblings in a family share their parameters, so the settings carry across.
            let state = ctx.s().plugins.get(&sid).map(|l| l.inst.state()).unwrap_or_default();
            let mut new_id = 0;
            ctx.edit("Change Plug-in Variant", |p| {
                let mut s = new_slot(p, &target);
                s.bypass = slot.bypass;
                s.sidechain = slot.sidechain;
                s.section = slot.section;
                s.state = state.clone();
                new_id = s.id;
                match pos {
                    SlotPos::Insert { owner, index } => {
                        if let Some(ch) = p.channel_mut(owner)
                            && index < ch.inserts.len()
                        {
                            ch.inserts[index] = s;
                        }
                    }
                    SlotPos::Instrument(t) => {
                        if let Some(t) = p.track_mut(t) {
                            t.instrument = Some(s);
                        }
                    }
                    SlotPos::MidiFx { track, index } => {
                        if let Some(t) = p.track_mut(track)
                            && index < t.midi_fx.len()
                        {
                            t.midi_fx[index] = s;
                        }
                    }
                }
            });
            win.close();
            let parent = win.transient_for();
            if let Some(inst) = ctx.s().plugins.get(&new_id).map(|l| l.inst.clone()) {
                if floats(&inst) {
                    inst.open_editor();
                }
                host_window(&ctx, new_id, &inst, parent.as_ref());
            }
        })
    };

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    type Sync = std::cell::RefCell<Option<Box<dyn Fn(&PluginSlot)>>>;
    let sync_bypass: Rc<Sync> = Rc::default();
    let sync_sidechain: Rc<Sync> = Rc::default();
    {
        let ctx = ctx.clone();
        controls.append(&crate::presets::for_plugin(inst, move || ctx.s().bundle.audio_dir()));
    }
    if chans.len() > 1 {
        let names: Vec<&str> = chans.iter().map(|c| c.label()).collect();
        let dd = gtk::DropDown::from_strings(&names);
        dd.set_tooltip_text(Some("Channels"));
        if let Some(i) = variant.channels.and_then(|c| chans.iter().position(|x| *x == c)) {
            dd.set_selected(i as u32);
        }
        let (swap, chans, current_bands) = (swap.clone(), chans.clone(), variant.bands);
        dd.connect_selected_notify(move |d| swap(Some(chans[d.selected() as usize]), current_bands));
        controls.append(&dd);
    }
    if bands.len() > 1 {
        let names: Vec<String> = bands.iter().map(|b| format!("{b} bands")).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dd = gtk::DropDown::from_strings(&refs);
        dd.set_tooltip_text(Some("Bands"));
        if let Some(i) = variant.bands.and_then(|b| bands.iter().position(|x| *x == b)) {
            dd.set_selected(i as u32);
        }
        let (swap, bands, ch) = (swap.clone(), bands.clone(), variant.channels);
        dd.connect_selected_notify(move |d| swap(ch, Some(bands[d.selected() as usize])));
        controls.append(&dd);
    }
    if !matches!(pos, SlotPos::Instrument(_)) {
        let bypass = gtk::ToggleButton::new();
        bypass.set_icon_name("system-shutdown-symbolic");
        bypass.set_tooltip_text(Some("On/Off"));
        bypass.set_active(!slot.bypass);
        {
            let b = bypass.clone();
            sync_bypass.replace(Some(Box::new(move |slot: &PluginSlot| {
                if b.is_active() == slot.bypass {
                    b.set_active(!slot.bypass);
                }
            })));
        }
        let ctx = ctx.clone();
        bypass.connect_toggled(move |b| {
            let off = !b.is_active();
            let Some((pos, slot)) = locate(&ctx.s().project, sid) else { return };
            if slot.bypass == off {
                return;
            }
            ctx.edit("Bypass Plug-in", |p| {
                let slot = match pos {
                    SlotPos::Insert { owner, .. } => p.channel_mut(owner).and_then(|c| c.inserts.iter_mut().find(|s| s.id == sid)),
                    SlotPos::MidiFx { track, .. } => p.track_mut(track).and_then(|t| t.midi_fx.iter_mut().find(|s| s.id == sid)),
                    SlotPos::Instrument(_) => None,
                };
                if let Some(s) = slot {
                    s.bypass = off;
                }
            });
        });
        header.pack_start(&bypass);
    }
    header.pack_start(&controls);
    let header_controls = controls.clone();

    let keyable = inst.has_sidechain();
    if let (true, SlotPos::Insert { owner, .. }) = (keyable, pos) {
        let tracks: Vec<(Id, String)> =
            ctx.s().project.tracks.iter().filter(|t| Some(t.id) != owner && !t.reference).map(|t| (t.id, t.name.clone())).collect();
        let mut names = vec!["No Side Chain".to_string()];
        names.extend(tracks.iter().map(|t| t.1.clone()));
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let dd = gtk::DropDown::from_strings(&refs);
        let sc_widget = dd.clone();
        dd.set_tooltip_text(Some("Side Chain"));
        dd.set_selected(slot.sidechain.and_then(|id| tracks.iter().position(|t| t.0 == id)).map_or(0, |i| i as u32 + 1));
        {
            let (ctx, tracks) = (ctx.clone(), tracks.clone());
            dd.connect_selected_notify(move |d| {
                let key = (d.selected() as usize).checked_sub(1).map(|i| tracks[i].0);
                let Some((SlotPos::Insert { owner, .. }, slot)) = locate(&ctx.s().project, sid) else { return };
                if slot.sidechain == key {
                    return;
                }
                ctx.edit("Set Side Chain", |p| {
                    if let Some(s) = p.channel_mut(owner).and_then(|c| c.inserts.iter_mut().find(|s| s.id == sid)) {
                        s.sidechain = key;
                    }
                });
            });
        }
        sync_sidechain.replace(Some(Box::new(move |slot: &PluginSlot| {
            let want = slot.sidechain.and_then(|id| tracks.iter().position(|t| t.0 == id)).map_or(0, |i| i as u32 + 1);
            if dd.selected() != want {
                dd.set_selected(want);
            }
        })));
        controls.append(&sc_widget);
    }

    if inst.has_editor() && embed.is_none() {
        let iface = gtk::Button::with_label("Interface");
        iface.set_tooltip_text(Some("Show the plug-in's own interface"));
        let (inst, ctx) = (inst.clone(), ctx.clone());
        iface.connect_clicked(move |_| {
            if !inst.open_editor() {
                ctx.m().message = Some(format!("{}'s interface could not be opened", inst.info().name));
                ctx.emit(Change::Message);
            }
        });
        header.pack_end(&iface);
    }

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    let (controls, size) = match inst.native() {
        Some(n) => crate::native_ui::page(ctx, n),
        None => (controls_page(inst), (560, 520)),
    };
    win.set_default_size(size.0, size.1);
    let extra = inst.native().and_then(|n| crate::sample_editor::page(ctx, n));
    match extra {
        _ if embed.is_some() => {
            // The plug-in's own interface, or the Controls view of sliders.
            let stack = adw::ViewStack::builder().hhomogeneous(false).vhomogeneous(false).build();
            let area = gtk::Box::new(gtk::Orientation::Vertical, 0);
            area.set_halign(gtk::Align::Start);
            area.set_valign(gtk::Align::Start);
            stack.add_titled_with_icon(&area, Some("editor"), "Plug-in", "application-x-addon-symbolic");
            stack.add_titled_with_icon(&controls, Some("controls"), "Controls", "preferences-other-symbolic");
            let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
            header.set_title_widget(Some(&switcher));
            win.set_default_size(-1, -1);
            tv.set_content(Some(&stack));
            crate::embed::attach(&win, &area, inst);
        }
        Some(page) => {
            let stack = adw::ViewStack::new();
            let title = if inst.native().is_some_and(|n| n.is_drum_kit()) { "Pads" } else { "Samples" };
            stack.add_titled_with_icon(&page, Some("samples"), title, "audio-x-generic-symbolic");
            stack.add_titled_with_icon(&controls, Some("controls"), "Controls", "preferences-other-symbolic");
            let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
            header.set_title_widget(Some(&switcher));
            win.set_default_size(size.0.max(620), 640);
            tv.set_content(Some(&stack));
        }
        None => tv.set_content(Some(&controls)),
    }
    win.set_content(Some(&tv));
    let editable: Vec<gtk::Widget> = [Some(header_controls.upcast()), tv.content()].into_iter().flatten().collect();
    let set_frozen = move |frozen: bool| editable.iter().for_each(|w| w.set_sensitive(!frozen));
    set_frozen(on_frozen_track(&ctx.s().project, pos));
    let key = (ctx.key(), sid);
    {
        let inst = inst.clone();
        win.connect_close_request(move |_| {
            WINDOWS.with(|w| w.borrow_mut().remove(&key));
            inst.close_editor();
            glib::Propagation::Proceed
        });
    }
    {
        // Closes when the slot goes, or when undo replaces the instance this window drives.
        let (ctx2, inst, weak) = (ctx.clone(), inst.clone(), win.downgrade());
        ctx.on(move |c| {
            if c != Change::Project {
                return;
            }
            let Some(win) = weak.upgrade() else { return };
            let found = locate(&ctx2.s().project, sid);
            let same = ctx2.s().plugins.get(&sid).is_some_and(|l| l.inst.same(&inst));
            match found {
                Some((pos, slot)) if same => {
                    set_frozen(on_frozen_track(&ctx2.s().project, pos));
                    for sync in [&sync_bypass, &sync_sidechain] {
                        if let Some(f) = sync.borrow().as_ref() {
                            f(&slot);
                        }
                    }
                }
                _ => win.close(),
            }
        });
    }
    WINDOWS.with(|w| w.borrow_mut().insert(key, win.clone()));
    win.present();
}

/// The Controls view: a slider per parameter.
fn controls_page(inst: &crate::host::Host) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().title("Controls").build();
    for p in inst.params().into_iter().filter(|p| !p.hidden) {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.title)).build();
        let value = gtk::Label::new(Some(&inst.param_text(p.id, inst.param(p.id))));
        value.add_css_class("dim-label");
        value.add_css_class("numeric");
        value.set_width_chars(10);
        let step = if p.steps > 0 { 1.0 / p.steps as f64 } else { 0.001 };
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, step);
        scale.set_draw_value(false);
        scale.set_size_request(200, -1);
        scale.set_value(inst.param(p.id));
        scale.set_sensitive(!p.read_only);
        let (i, id, v) = (inst.clone(), p.id, value.clone());
        let handler = scale.connect_value_changed(move |s| {
            i.set_param(id, s.value());
            v.set_text(&i.param_text(id, s.value()));
        });
        // Follows changes made elsewhere: presets, automation and the plug-in's own interface.
        // They aren't sent back to the plug-in, where they would fight a drag a frame late.
        let (i, v) = (inst.clone(), value.clone());
        scale.add_tick_callback(move |s, _| {
            let now = i.param(id);
            if (now - s.value()).abs() > 1e-6 {
                s.block_signal(&handler);
                s.set_value(now);
                s.unblock_signal(&handler);
                v.set_text(&i.param_text(id, now));
            }
            glib::ControlFlow::Continue
        });
        row.add_suffix(&value);
        row.add_suffix(&scale);
        group.add(&row);
    }
    page.add(&group);
    page.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(sub: &str) -> ClassInfo {
        ClassInfo { uid: String::new(), name: String::new(), vendor: String::new(), subcategories: sub.into(), path: Default::default() }
    }

    #[test]
    fn slots_belong_to_their_channel() {
        let mut p = Project::new("T", 48000);
        let track = p.add_track(TrackKind::Instrument);
        let bus = p.add_bus();
        let [inst, fx, midi, on_bus, on_master] = [(); 5].map(|_| new_slot(&mut p, &info("Fx")));
        let ids = [inst.id, fx.id, midi.id, on_bus.id, on_master.id];
        let t = p.track_mut(track).unwrap();
        t.instrument = Some(inst);
        t.channel.inserts.push(fx);
        t.midi_fx.push(midi);
        p.channel_mut(Some(bus)).unwrap().inserts.push(on_bus);
        p.master.inserts.push(on_master);
        let channels: Vec<_> = ids.iter().map(|id| slot_channel(&p, *id)).collect();
        assert_eq!(channels, [Some(Some(track)), Some(Some(track)), Some(Some(track)), Some(Some(bus)), Some(None)]);
        assert_eq!(slot_channel(&p, 9999), None);
    }

    #[test]
    fn group_by_type_developer_and_name() {
        let family = |name: &str, vendor: &str, sub: &str, to_stereo: bool| {
            let c = ClassInfo { uid: "vst3:x".into(), name: name.into(), vendor: vendor.into(), subcategories: sub.into(), path: Default::default() };
            Family { name: name.into(), summary: String::new(), pick: c.clone(), representative: c, to_stereo, key: String::new() }
        };
        let none = BTreeMap::new();
        let eq = family("tilt EQ", "  ", "Fx|EQ|Filter", false);
        assert_eq!(groups(&eq, PluginGroup::Type, &none), ["EQ", "Filter"]);
        assert_eq!(groups(&eq, PluginGroup::Developer, &none), ["Unknown Developer"]);
        assert_eq!(groups(&eq, PluginGroup::Name, &none), ["T"]);
        let odd = family("4 Band", "Someone", "Fx|Dynamics", false);
        assert_eq!(groups(&odd, PluginGroup::Developer, &none), ["Someone"]);
        assert_eq!(groups(&odd, PluginGroup::Name, &none), ["#"]);
        let wide = family("Wide", "Someone", "Fx|Spatial", true);
        for by in PluginGroup::ALL {
            assert_eq!(groups(&wide, by, &none), [MONO_TO_STEREO]);
        }
        let user = BTreeMap::from([("vst3:x".to_string(), vec!["Pedal".to_string()])]);
        assert_eq!(groups(&eq, PluginGroup::Type, &user), ["Pedal"]);
    }
}
