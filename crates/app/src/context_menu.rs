//! Right-click menus for regions, track headers and empty lane space.

use crate::canvas::track_color;
use crate::studio::Ctx;
use adw::prelude::*;
use gtk::gdk;
use solstice_model::*;
use std::rc::Rc;

pub(crate) struct Menu {
    pub(crate) pop: gtk::Popover,
    pub(crate) list: gtk::Box,
}

impl Menu {
    pub(crate) fn at(anchor: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Menu {
        let pop = gtk::Popover::new();
        pop.set_parent(anchor);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| p.unparent());
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        pop.set_child(Some(&list));
        Menu { pop, list }
    }

    pub(crate) fn item(&self, label: &str, enabled: bool, f: impl Fn() + 'static) {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_sensitive(enabled);
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let pop = self.pop.clone();
        b.connect_clicked(move |_| {
            pop.popdown();
            f();
        });
        self.list.append(&b);
    }

    /// Runs one of the window's actions.
    fn action(&self, label: &str, anchor: &gtk::Widget, action: &'static str) {
        let a = anchor.clone();
        self.item(label, true, move || {
            let _ = WidgetExt::activate_action(&a, action, None);
        });
    }

    /// A row of palette colours that runs `f` with the chosen index.
    fn swatches(&self, tooltip: &str, f: impl Fn(u8) + 'static) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row.set_margin_top(4);
        row.set_margin_bottom(4);
        row.set_margin_start(6);
        let f = Rc::new(f);
        for c in 0..TRACK_COLORS as u8 {
            let b = gtk::Button::new();
            b.add_css_class("flat");
            b.set_tooltip_text(Some(tooltip));
            let colour = track_color(c);
            let dot = crate::canvas::Canvas::new(move |_, s, w, h| crate::canvas::rounded(s, 0.0, 0.0, w, h, w / 2.0, &colour));
            dot.set_size_request(16, 16);
            b.set_child(Some(&dot));
            let (f, pop) = (f.clone(), self.pop.clone());
            b.connect_clicked(move |_| {
                pop.popdown();
                f(c);
            });
            row.append(&b);
        }
        self.list.append(&row);
    }

    fn heading(&self, text: &str) {
        let l = gtk::Label::new(Some(text));
        l.set_xalign(0.0);
        l.set_margin_start(10);
        l.add_css_class("dim-label");
        l.add_css_class("caption");
        self.list.append(&l);
    }

    pub(crate) fn separator(&self) {
        self.list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    pub(crate) fn show(&self) {
        self.pop.popup();
    }
}

/// A one-line text field in a popover, for renaming.
pub(crate) fn ask_name(anchor: &impl IsA<gtk::Widget>, x: f64, y: f64, current: &str, done: impl Fn(String) + 'static) {
    let m = Menu::at(anchor, x, y);
    let entry = gtk::Entry::builder().text(current).width_chars(24).build();
    m.list.append(&entry);
    let (pop, current) = (m.pop.clone(), current.to_string());
    entry.connect_activate(move |e| {
        let name = e.text().trim().to_string();
        pop.popdown();
        if !name.is_empty() && name != current {
            done(name);
        }
    });
    m.show();
    entry.grab_focus();
}

pub fn track_menu(ctx: &Ctx, anchor: &gtk::Widget, x: f64, y: f64, id: Id) {
    ctx.select_track(Some(id));
    let (name, index, count) = {
        let p = &ctx.s().project;
        (p.track(id).map(|t| t.name.clone()).unwrap_or_default(), p.tracks.iter().position(|t| t.id == id).unwrap_or(0), p.tracks.len())
    };
    let m = Menu::at(anchor, x, y);
    {
        let (ctx, anchor) = (ctx.clone(), anchor.clone());
        m.item("Rename…", true, move || {
            let ctx = ctx.clone();
            ask_name(&anchor, x, y, &name, move |n| {
                ctx.edit("Rename Track", |p| {
                    if let Some(t) = p.track_mut(id) {
                        t.name = n.clone();
                    }
                })
            });
        });
    }
    {
        let ctx = ctx.clone();
        m.swatches("Track Colour", move |c| {
            ctx.edit("Track Colour", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.color = c;
                }
            })
        });
    }
    m.separator();
    {
        let ctx = ctx.clone();
        m.item("Duplicate Track", true, move || {
            // The copy starts from each plug-in's current settings, not the last saved ones.
            ctx.m().capture_plugin_states();
            let mut new = None;
            ctx.edit("Duplicate Track", |p| new = p.duplicate_track(id));
            ctx.select_track(new);
        });
    }
    for (label, by) in [("Move Up", -1isize), ("Move Down", 1)] {
        let to = index as isize + by;
        let ctx = ctx.clone();
        m.item(label, to >= 0 && (to as usize) < count, move || ctx.edit("Move Track", |p| p.move_track(id, by)));
    }
    m.separator();
    // Stacks gather the selected tracks, or just this one.
    let chosen: Vec<Id> = {
        let s = ctx.s();
        let sel: Vec<Id> = s.selection.iter().copied().filter(|i| s.project.track(*i).is_some()).collect();
        if sel.contains(&id) { sel } else { vec![id] }
    };
    for (label, summing) in [("Create Folder Stack", false), ("Create Summing Stack", true)] {
        let (ctx, chosen) = (ctx.clone(), chosen.clone());
        m.item(label, true, move || ctx.edit(label, |p| {
            p.create_stack(&chosen, summing);
        }));
    }
    let (freezable, frozen) = ctx.s().project.track(id).map_or((false, false), |t| (crate::render::freezable(t), t.freeze.is_some()));
    if freezable {
        let (ctx, chosen, anchor) = (ctx.clone(), chosen.clone(), anchor.clone());
        m.item(if frozen { "Unfreeze Track" } else { "Freeze Track" }, true, move || {
            if frozen {
                ctx.unfreeze(&chosen);
            } else if ctx.freeze(&chosen) {
                crate::render::progress_dialog(&ctx, &anchor);
            }
        });
    }
    let reference = ctx.s().project.track(id).filter(|t| t.kind == TrackKind::Audio).map(|t| t.reference);
    if let Some(on) = reference {
        let ctx = ctx.clone();
        m.item(if on { "✓ Use as Reference Track" } else { "Use as Reference Track" }, true, move || {
            ctx.edit("Use as Reference Track", |p| {
                if let Some(t) = p.track_mut(id) {
                    t.reference = !on;
                    t.reference_lufs = None;
                }
            })
        });
    }
    let in_stack = ctx.s().project.track(id).and_then(|t| t.stack);
    if let Some(stack) = in_stack {
        let ctx = ctx.clone();
        m.item("Remove from Stack", true, move || {
            ctx.edit("Remove from Stack", |p| {
                let bus = p.stack(stack).and_then(|s| s.bus);
                if let Some(t) = p.track_mut(id) {
                    t.stack = None;
                    if bus.is_some_and(|b| t.channel.output == Output::Bus(b)) {
                        t.channel.output = Output::Master;
                    }
                }
                p.tidy_stacks();
            })
        });
    }
    m.separator();
    alternatives_section(ctx, &m, anchor, x, y, id);
    m.separator();
    m.action("New Audio Track", anchor, "win.add-audio-track");
    m.action("New Instrument Track", anchor, "win.add-instrument-track");
    m.separator();
    m.action("Delete Track", anchor, "win.delete-track");
    m.show();
}

fn alternatives_section(ctx: &Ctx, m: &Menu, anchor: &gtk::Widget, x: f64, y: f64, id: Id) {
    let Some(names) = ctx.s().project.track(id).map(|t| t.alternative_names()) else { return };
    m.heading("Alternatives");
    for (name, index) in &names {
        let ctx = ctx.clone();
        let index = *index;
        let label = if index.is_none() { format!("✓ {name}") } else { name.clone() };
        m.item(&label, true, move || {
            if let Some(i) = index {
                ctx.edit("Switch Alternative", |p| {
                    p.switch_alternative(id, i);
                });
            }
        });
    }
    for (label, duplicate) in [("New Alternative", false), ("Duplicate Alternative", true)] {
        let ctx = ctx.clone();
        m.item(label, true, move || {
            ctx.edit(label, |p| {
                p.new_alternative(id, duplicate);
            })
        });
    }
    {
        let (ctx, anchor) = (ctx.clone(), anchor.clone());
        let current = names.iter().find(|n| n.1.is_none()).map(|n| n.0.clone()).unwrap_or_default();
        let others: Vec<String> = names.iter().filter(|n| n.1.is_some()).map(|n| n.0.clone()).collect();
        m.item("Rename Alternative…", true, move || {
            let (ctx, others) = (ctx.clone(), others.clone());
            ask_name(&anchor, x, y, &current, move |n| {
                if others.contains(&n) {
                    ctx.message(&format!("There is already an alternative called {n}"));
                    return;
                }
                ctx.edit("Rename Alternative", |p| {
                    p.rename_alternative(id, &n);
                })
            });
        });
    }
    let ctx = ctx.clone();
    m.item("Delete Alternative", names.len() > 1, move || {
        ctx.edit("Delete Alternative", |p| {
            p.delete_alternative(id);
        })
    });
}

pub fn stack_menu(ctx: &Ctx, anchor: &gtk::Widget, x: f64, y: f64, id: Id) {
    let Some(stack) = ctx.s().project.stack(id).cloned() else { return };
    let m = Menu::at(anchor, x, y);
    {
        let (ctx, anchor, name) = (ctx.clone(), anchor.clone(), stack.name.clone());
        m.item("Rename…", true, move || {
            let ctx = ctx.clone();
            ask_name(&anchor, x, y, &name, move |n| {
                ctx.edit("Rename Stack", |p| {
                    let bus = p.stack(id).and_then(|s| s.bus);
                    if let Some(s) = p.stacks.iter_mut().find(|s| s.id == id) {
                        s.name = n.clone();
                    }
                    if let Some(b) = bus.and_then(|b| p.buses.iter_mut().find(|x| x.id == b)) {
                        b.name = n.clone();
                    }
                })
            });
        });
    }
    {
        let ctx = ctx.clone();
        m.item(if stack.collapsed { "Expand" } else { "Collapse" }, true, move || {
            ctx.tweak(|p| {
                if let Some(s) = p.stacks.iter_mut().find(|s| s.id == id) {
                    s.collapsed = !s.collapsed;
                }
            })
        });
    }
    {
        let ctx = ctx.clone();
        m.item("Select Tracks", true, move || {
            let ids: Vec<Id> = ctx.s().project.tracks.iter().filter(|t| t.stack == Some(id)).map(|t| t.id).collect();
            ctx.set_selection(ids);
        });
    }
    m.separator();
    {
        let ctx = ctx.clone();
        m.item(if stack.bus.is_some() { "Ungroup (Remove the Aux)" } else { "Ungroup" }, true, move || ctx.edit("Ungroup Stack", |p| p.ungroup_stack(id)));
    }
    m.show();
}

pub fn region_menu(ctx: &Ctx, anchor: &gtk::Widget, x: f64, y: f64, id: Id, open_editor: Rc<dyn Fn()>) {
    if !ctx.s().selected_regions.contains(&id) {
        ctx.select_region(Some(id));
    }
    let (name, muted, looped) = {
        let s = ctx.s();
        let r = s.project.find_region(id).map(|(_, r)| r.clone());
        let all_muted = s.selected_regions.iter().all(|r| s.project.find_region(*r).is_some_and(|(_, r)| r.muted));
        (r.as_ref().map(|r| r.name.clone()).unwrap_or_default(), all_muted, r.is_some_and(|r| r.loops > 0))
    };
    let m = Menu::at(anchor, x, y);
    m.item("Open in Editor", true, move || open_editor());
    {
        let (ctx, anchor) = (ctx.clone(), anchor.clone());
        m.item("Rename…", true, move || {
            let ctx = ctx.clone();
            ask_name(&anchor, x, y, &name, move |n| {
                ctx.edit("Rename Region", |p| {
                    if let Some(r) = p.region_mut(id) {
                        r.name = n.clone();
                    }
                })
            });
        });
    }
    let colour_regions = {
        let ctx = ctx.clone();
        move |color: Option<u8>| {
            let ids = ctx.s().selected_regions.clone();
            ctx.edit("Colour Regions", |p| {
                for r in &ids {
                    if let Some(r) = p.region_mut(*r) {
                        r.color = color;
                    }
                }
            });
        }
    };
    {
        let f = colour_regions.clone();
        m.swatches("Region Colour", move |c| f(Some(c)));
    }
    m.item("Track Colour", true, move || colour_regions(None));
    m.separator();
    m.action("Cut", anchor, "win.cut");
    m.action("Copy", anchor, "win.copy");
    m.action("Paste", anchor, "win.paste");
    m.action("Repeat", anchor, "win.repeat");
    m.separator();
    m.action("Split at Playhead", anchor, "win.split-region");
    m.action("Join", anchor, "win.join");
    {
        let ctx = ctx.clone();
        m.item(if muted { "Unmute" } else { "Mute" }, true, move || {
            let ids = ctx.s().selected_regions.clone();
            ctx.edit(if muted { "Unmute Regions" } else { "Mute Regions" }, |p| {
                for r in &ids {
                    if let Some(r) = p.region_mut(*r) {
                        r.muted = !muted;
                    }
                }
            });
        });
    }
    {
        let ctx = ctx.clone();
        m.item(if looped { "Stop Looping" } else { "Loop" }, true, move || {
            ctx.edit(if looped { "Stop Looping" } else { "Loop Region" }, |p| loop_region(p, id, !looped));
        });
    }
    m.action("Bounce in Place", anchor, "win.bounce-in-place");
    if let Some(groove) = groove_source(ctx, id) {
        let (ctx, anchor) = (ctx.clone(), anchor.clone());
        m.item("Make Groove Template…", true, move || {
            let (ctx, (notes, length)) = (ctx.clone(), groove.clone());
            ask_name(&anchor, x, y, "", move |n| {
                if crate::grooves::exists(&n) {
                    ctx.message(&format!("There is already a groove template called {n}"));
                    return;
                }
                let g = Groove::from_notes(&n, &notes, length);
                ctx.message(&match crate::grooves::save(&g) {
                    Ok(()) => format!("Saved groove template “{n}”"),
                    Err(e) => format!("Could not save the groove template: {e}"),
                });
            });
        });
    }
    let takes = crate::takes_editor::take_names(ctx, id);
    if !takes.is_empty() {
        m.separator();
        let c = ctx.clone();
        m.item("Flatten Take Folder", true, move || crate::takes_editor::flatten_folder(&c, id));
        for (i, name) in takes.iter().enumerate() {
            let c = ctx.clone();
            m.item(&format!("Play {name}"), true, move || crate::takes_editor::play_take(&c, id, i));
        }
    }
    m.separator();
    m.action("Delete", anchor, "win.delete-region");
    m.show();
}

/// A MIDI region's notes and its length rounded to whole bars, at least one, in the
/// signature at its start.
fn groove_source(ctx: &Ctx, id: Id) -> Option<(Vec<Note>, u32)> {
    let p = &ctx.s().project;
    let (_, r) = p.find_region(id)?;
    let Content::Midi(m) = &r.content else { return None };
    let time = p.time();
    let (num, den) = time.sig_at(r.start);
    let bar = PPQ as u64 * 4 / den.max(1) as u64 * num.max(1) as u64;
    let bars = ((time.ticks_between(r.start, r.end()) + bar / 2) / bar).max(1);
    Some((m.notes.clone(), (bars * bar) as u32))
}

/// Loop: repeats the region up to the next region on its track, or to the end of
/// the project.
fn loop_region(p: &mut Project, id: Id, on: bool) {
    let end = p.length();
    let Some((ti, r)) = p.find_region(id).map(|(ti, r)| (ti, r.clone())) else { return };
    let next = p.tracks[ti].regions.iter().filter(|o| o.id != id && o.start >= r.end()).map(|o| o.start).min();
    let until = next.unwrap_or(end.max(r.end() + r.length * 4));
    if let Some(r2) = p.region_mut(id) {
        r2.loops = if on { ((until.saturating_sub(r.end())) / r.length.max(1)).max(1) as u32 } else { 0 };
    }
}

/// Right-click on empty lane space.
pub fn lane_menu(ctx: &Ctx, anchor: &gtk::Widget, x: f64, y: f64, track: Option<Id>, create_region: Option<Rc<dyn Fn()>>) {
    if let Some(t) = track {
        ctx.select_track(Some(t));
    }
    let m = Menu::at(anchor, x, y);
    m.action("Paste", anchor, "win.paste");
    m.action("Select All", anchor, "win.select-all-regions");
    if let Some(f) = create_region {
        m.item("Create MIDI Region", true, move || f());
    }
    m.separator();
    m.action("New Audio Track", anchor, "win.add-audio-track");
    m.action("New Instrument Track", anchor, "win.add-instrument-track");
    m.show();
}
