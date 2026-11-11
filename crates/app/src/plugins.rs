use crate::studio::{Change, Ctx};
use crate::variants;
use adw::prelude::*;
use gtk::glib;
use solstice_model::{Id, PluginSlot, Project, TrackKind};
use solstice_vst3::ClassInfo;
use std::rc::Rc;

pub fn new_slot(p: &mut Project, info: &ClassInfo) -> PluginSlot {
    PluginSlot {
        id: p.alloc_id(),
        uid: info.uid.clone(),
        path: info.path.clone(),
        name: info.name.clone(),
        bypass: false,
        state: Vec::new(),
    }
}

/// The group a plug-in is listed under: its first specific VST3 subcategory, skipping the
/// generic kind and channel-format tags ("Fx|Dynamics|Stereo" files under "Dynamics").
pub fn category(info: &ClassInfo) -> String {
    const GENERIC: [&str; 6] = ["Fx", "Instrument", "Stereo", "Mono", "Surround", "Ambisonics"];
    info.subcategories
        .split('|')
        .map(str::trim)
        .find(|c| !c.is_empty() && !GENERIC.contains(c))
        .map(str::to_string)
        .unwrap_or_else(|| if info.is_instrument() { "Instrument".into() } else { "Other".into() })
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

fn families(catalog: &[ClassInfo], instruments: Option<bool>, mono_slot: bool) -> Vec<Family> {
    let mut seen: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut out = Vec::new();
    for c in catalog.iter().filter(|c| instruments.is_none_or(|i| c.is_instrument() == i)) {
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

fn family_row(f: &Family, on_pick: &Rc<dyn Fn(&ClassInfo)>) -> adw::ActionRow {
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
    row
}

/// A searchable list of installed plug-ins, one entry per family, grouped into collapsible
/// categories. Only families that suit the slot's channel format are offered; on a mono slot,
/// stereo-only ones are grouped under "Mono → Stereo" as in Logic.
pub fn list(ctx: &Ctx, instruments: Option<bool>, mono_slot: bool, on_pick: Rc<dyn Fn(&ClassInfo)>) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search Plug-ins"));
    col.append(&search);
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

    let fams = families(&ctx.s().available_plugins(), instruments, mono_slot);
    let mut groups: std::collections::BTreeMap<String, Vec<&Family>> = Default::default();
    for f in &fams {
        let group = if f.to_stereo { "Mono → Stereo".to_string() } else { category(&f.representative) };
        groups.entry(group).or_default().push(f);
    }
    if groups.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("application-x-addon-symbolic")
            .title("No Plug-ins")
            .description("Install VST3 plug-ins in ~/.vst3 or add a folder in Preferences")
            .build();
        empty.add_css_class("compact");
        listbox.set_placeholder(Some(&empty));
    }

    let mut sections: Vec<(adw::ExpanderRow, Vec<(adw::ActionRow, String)>)> = Vec::new();
    // The Mono → Stereo group goes last, after the regular categories.
    let mut ordered: Vec<(&String, &Vec<&Family>)> = groups.iter().collect();
    ordered.sort_by_key(|(name, _)| (name.starts_with("Mono →"), name.to_lowercase()));
    for (name, items) in ordered {
        let exp = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(name))
            .subtitle(items.len().to_string())
            .build();
        let rows = items
            .iter()
            .map(|f| {
                let row = family_row(f, &on_pick);
                exp.add_row(&row);
                (row, f.key.clone())
            })
            .collect();
        listbox.append(&exp);
        sections.push((exp, rows));
    }
    if let [(exp, _)] = sections.as_slice() {
        exp.set_expanded(true);
    }
    search.connect_search_changed(move |s| {
        let q = s.text().to_lowercase();
        for (exp, rows) in &sections {
            let mut any = false;
            for (row, key) in rows {
                let hit = q.is_empty() || key.contains(&q);
                row.set_visible(hit);
                any |= hit;
            }
            exp.set_visible(any);
            exp.set_expanded(!q.is_empty() && any || sections.len() == 1);
        }
    });
    col
}

pub fn picker_button(
    ctx: &Ctx,
    instruments: bool,
    mono_slot: bool,
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
    pop.connect_show(move |p| p.set_child(Some(&list(&ctx, Some(instruments), mono_slot, on_pick.clone()))));
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

/// The Library pane: picking a plug-in loads it onto the selected track, like Logic's library.
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
            holder.set_child(Some(&list(&ctx, None, mono, pick)));
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
    } else {
        ctx.edit("Add Plug-in", |p| {
            let slot = new_slot(p, &info);
            p.channel_mut(sel).unwrap().inserts.push(slot);
        });
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum SlotPos {
    Insert { owner: Option<Id>, index: usize },
    Instrument(Id),
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
    }
    for b in &p.buses {
        if let Some(i) = b.channel.inserts.iter().position(|s| s.id == sid) {
            return Some((SlotPos::Insert { owner: Some(b.id), index: i }, b.channel.inserts[i].clone()));
        }
    }
    None
}

thread_local! {
    static WINDOWS: std::cell::RefCell<std::collections::HashMap<Id, adw::Window>> = Default::default();
}

/// Opens a plug-in: the host window (Logic's plug-in header: variant pickers, bypass and
/// generic controls) plus the plug-in's own interface, which opens as a separate window because
/// GTK 4 cannot embed a foreign X11 window.
pub fn open_editor(ctx: &Ctx, slot: Id, anchor: &impl IsA<gtk::Widget>) {
    let Some(inst) = ctx.s().plugins.get(&slot).map(|l| l.inst.clone()) else { return };
    if inst.has_editor() && !inst.editor_open() {
        inst.open_editor(&inst.info.name);
    }
    if let Some(w) = WINDOWS.with(|w| w.borrow().get(&slot).cloned()) {
        w.present();
        return;
    }
    let parent = anchor.root().and_downcast::<gtk::Window>();
    host_window(ctx, slot, &inst, parent.as_ref());
}

fn host_window(ctx: &Ctx, sid: Id, inst: &std::sync::Arc<solstice_vst3::Instance>, parent: Option<&gtk::Window>) {
    let Some((pos, slot)) = locate(&ctx.s().project, sid) else { return };
    let variant = variants::parse(&inst.info);
    let family = if variant.family.is_empty() { inst.info.name.clone() } else { variant.family.clone() };
    let win = adw::Window::builder().title(&family).default_width(560).default_height(520).build();
    win.set_transient_for(parent);
    let header = adw::HeaderBar::new();

    let (members, mono_slot): (Vec<(ClassInfo, variants::Variant)>, bool) = {
        let s = ctx.s();
        let mono = match pos {
            SlotPos::Insert { owner, index } => variants::slot_is_mono(&s.project, &s.catalog, owner, index),
            SlotPos::Instrument(_) => false,
        };
        let m = variants::members(&s.catalog, &inst.info).into_iter().map(|(c, v)| (c.clone(), v)).collect();
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
            if target.uid == slot.uid {
                return;
            }
            let mut new_id = 0;
            let bypass = slot.bypass;
            ctx.edit("Change Plug-in Variant", |p| {
                let mut s = new_slot(p, &target);
                s.bypass = bypass;
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
                }
            });
            win.close();
            let parent = win.transient_for();
            if let Some(inst) = ctx.s().plugins.get(&new_id).map(|l| l.inst.clone()) {
                if inst.has_editor() {
                    inst.open_editor(&inst.info.name);
                }
                host_window(&ctx, new_id, &inst, parent.as_ref());
            }
        })
    };

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
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
    header.pack_start(&controls);

    if let SlotPos::Insert { owner, .. } = pos {
        let bypass = gtk::ToggleButton::new();
        bypass.set_icon_name("system-shutdown-symbolic");
        bypass.set_tooltip_text(Some("On/Off"));
        bypass.set_active(!slot.bypass);
        let ctx = ctx.clone();
        bypass.connect_toggled(move |b| {
            let off = !b.is_active();
            ctx.edit("Bypass Plug-in", |p| {
                if let Some(s) = p.channel_mut(owner).and_then(|c| c.inserts.iter_mut().find(|s| s.id == sid)) {
                    s.bypass = off;
                }
            });
        });
        header.pack_end(&bypass);
    }
    if inst.has_editor() {
        let iface = gtk::Button::with_label("Interface");
        iface.set_tooltip_text(Some("Show the plug-in's own interface"));
        let inst = inst.clone();
        iface.connect_clicked(move |_| {
            if !inst.editor_open() {
                inst.open_editor(&inst.info.name);
            }
        });
        header.pack_end(&iface);
    }

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&controls_page(inst)));
    win.set_content(Some(&tv));
    {
        let inst = inst.clone();
        win.connect_close_request(move |_| {
            WINDOWS.with(|w| w.borrow_mut().remove(&sid));
            inst.close_editor();
            glib::Propagation::Proceed
        });
    }
    WINDOWS.with(|w| w.borrow_mut().insert(sid, win.clone()));
    win.present();
}

/// Logic's "Controls" view: a slider per parameter.
fn controls_page(inst: &std::sync::Arc<solstice_vst3::Instance>) -> gtk::Widget {
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
        scale.connect_value_changed(move |s| {
            i.set_param(id, s.value());
            v.set_text(&i.param_text(id, s.value()));
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
    fn categories() {
        assert_eq!(category(&info("Fx|Dynamics|Stereo")), "Dynamics");
        assert_eq!(category(&info("Fx|Reverb|Stereo")), "Reverb");
        assert_eq!(category(&info("Instrument|Instrument|Sampler|Instrument|Drum|Mono")), "Sampler");
        assert_eq!(category(&info("Fx")), "Other");
        assert_eq!(category(&info("Instrument")), "Instrument");
    }
}
