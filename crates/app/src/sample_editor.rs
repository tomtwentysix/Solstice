use crate::samples::{add_file, load_zone, note_from_name};
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_dsp::Native;
use solstice_dsp::sampler::{PADS, Zone, ZoneDesc, pad_zone, parse_sfz};
use solstice_engine::Command;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// A view's rebuild function. Widgets it creates hold it weakly, so it can rebuild them
/// without a reference cycle.
struct Refresh(Box<dyn Fn()>);

impl Refresh {
    fn run(&self) {
        (self.0)()
    }
}

fn run_weak(me: &std::rc::Weak<Refresh>) {
    if let Some(f) = me.upgrade() {
        f.run();
    }
}

pub(crate) fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 1)
}

/// Runs a change to the zones as one undo step.
fn zone_edit(ctx: &Ctx, native: &Native, label: &str, f: impl FnOnce(&mut Vec<Zone>)) {
    let Some(map) = &native.samples else { return };
    let sid = ctx.s().plugins.iter().find(|(_, l)| l.inst.native().is_some_and(|n| std::ptr::eq(n.as_ref(), native))).map(|(id, _)| *id);
    let before = solstice_dsp::sampler::state(&native.bank, &map.descs());
    let mut zones: Vec<Zone> = map.zones().as_ref().clone();
    f(&mut zones);
    map.set(zones);
    let key = native as *const Native as usize;
    SEEN.with(|s| {
        if let Some(seen) = s.borrow_mut().get_mut(&key) {
            *seen = map.descs();
        }
    });
    match sid {
        Some(sid) => ctx.m().plugin_state_edit(sid, before, label),
        None => ctx.m().history.mark_dirty(),
    }
    ctx.emit(Change::Project);
}

/// Changes one zone's settings, reusing its loaded audio.
fn update(ctx: &Ctx, native: &Native, index: usize, f: impl FnOnce(&mut ZoneDesc)) {
    zone_edit(ctx, native, "Edit Zone", |zones| {
        if let Some(z) = zones.get_mut(index) {
            f(&mut z.desc);
        }
    });
}

fn remove(ctx: &Ctx, native: &Native, index: usize) {
    zone_edit(ctx, native, "Remove Zone", |zones| {
        if index < zones.len() {
            zones.remove(index);
        }
    });
}

/// Converts files into the project in the background, then adds zones made by `make` (given
/// each file's position in the batch and its project-relative path).
pub(crate) fn add_files(ctx: &Ctx, native: &Arc<Native>, files: Vec<PathBuf>, make: Rc<dyn Fn(usize, &PathBuf, PathBuf) -> ZoneDesc>, done: Rc<dyn Fn()>) {
    let (dir, sr) = {
        let s = ctx.s();
        (s.bundle.audio_dir(), s.sample_rate())
    };
    let (ctx, native) = (ctx.clone(), native.clone());
    glib::spawn_future_local(async move {
        let dir2 = dir.clone();
        let originals = files.clone();
        let converted = gio::spawn_blocking(move || files.iter().map(|f| add_file(f, &dir2, sr)).collect::<Vec<_>>()).await.unwrap_or_default();
        let Some(map) = &native.samples else { return };
        let mut zones: Vec<Zone> = map.zones().as_ref().clone();
        let mut failed = Vec::new();
        for (i, (orig, res)) in originals.iter().zip(converted).enumerate() {
            match res {
                Ok(rel) => {
                    let desc = make(i, orig, rel);
                    if let Some(z) = load_zone(&desc, &dir) {
                        // A pad holds one sample: loading a new one replaces it.
                        if desc.one_shot {
                            zones.retain(|old| old.desc.lo != desc.lo || !old.desc.one_shot);
                        }
                        zones.push(z);
                    }
                }
                Err(e) => failed.push(format!("{}: {e}", orig.display())),
            }
        }
        map.set(zones);
        ctx.m().history.mark_dirty();
        if !failed.is_empty() {
            ctx.message(&format!("Could not load {}", failed.join(", ")));
        }
        done();
    });
}

pub(crate) fn pick_files(anchor: &impl IsA<gtk::Widget>, title: &str, patterns: &[&str], multiple: bool, then: impl Fn(Vec<PathBuf>) + 'static) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(title));
    for p in patterns {
        filter.add_pattern(p);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let d = gtk::FileDialog::builder().title(title).filters(&filters).build();
    let win = anchor.root().and_downcast::<gtk::Window>();
    if multiple {
        d.open_multiple(win.as_ref(), gio::Cancellable::NONE, move |r| {
            if let Ok(list) = r {
                let paths = (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
                then(paths);
            }
        });
    } else {
        d.open(win.as_ref(), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r
                && let Some(p) = f.path()
            {
                then(vec![p]);
            }
        });
    }
}

pub(crate) const AUDIO: [&str; 8] = ["*.wav", "*.flac", "*.aif", "*.aiff", "*.mp3", "*.ogg", "*.m4a", "*.caf"];

/// Loads one audio file onto a Drum Kit pad, replacing what the pad held.
pub(crate) fn load_pad(ctx: &Ctx, native: &Arc<Native>, pad: usize, files: Vec<PathBuf>) {
    if pad >= PADS.len() {
        return;
    }
    let files: Vec<PathBuf> = files.into_iter().take(1).collect();
    let c = ctx.clone();
    add_files(ctx, native, files, Rc::new(move |_, _, rel| pad_zone(pad, rel)), Rc::new(move || c.emit(Change::Project)));
}

/// Shows pad `pad` on the Drum Kit's Pads page, now if it is open, else when it opens.
pub(crate) fn select_pad(native: &Arc<Native>, pad: usize) {
    let key = Arc::as_ptr(native) as usize;
    SELECTED.with(|p| p.borrow_mut().insert(key, pad));
    for rebuild in REBUILD.with(|r| r.borrow().get(&key).cloned()).unwrap_or_default() {
        rebuild();
    }
}

/// Each pad's name: the drum map's name for its note on the track playing this kit, else
/// the pad's own.
fn pad_names(ctx: &Ctx, native: &Native) -> Vec<String> {
    let s = ctx.s();
    let sid = s.plugins.iter().find(|(_, l)| l.inst.native().is_some_and(|n| std::ptr::eq(n.as_ref(), native))).map(|(id, _)| *id);
    let track = sid.and_then(|sid| s.project.tracks.iter().find(|t| t.instrument.as_ref().is_some_and(|i| i.id == sid)));
    PADS.iter()
        .map(|(note, name, _)| {
            track
                .and_then(|t| t.drum_map.iter().find(|r| r.pitch == *note && !r.name.is_empty()))
                .map_or_else(|| name.to_string(), |r| r.name.clone())
        })
        .collect()
}

/// The Samples page for the Sampler, or the Pads page for the Drum Kit.
pub fn page(ctx: &Ctx, native: &Arc<Native>) -> Option<gtk::Widget> {
    let map = native.samples.clone()?;
    let key = Arc::as_ptr(native) as usize;
    let names = Rc::new(std::cell::RefCell::new(pad_names(ctx, native)));
    let build = {
        let (ctx, native, names) = (ctx.clone(), native.clone(), names.clone());
        move || {
            if native.is_drum_kit() {
                let pad = SELECTED.with(|p| p.borrow().get(&key).copied()).unwrap_or(0);
                pads(&ctx, &native, names.borrow().clone(), pad)
            } else {
                zones(&ctx, &native)
            }
        }
    };
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.append(&build());
    SEEN.with(|s| s.borrow_mut().insert(key, map.descs()));
    let build = Rc::new(build);
    let rebuild = {
        let (weak, build) = (holder.downgrade(), build.clone());
        move || {
            let Some(holder) = weak.upgrade() else { return };
            while let Some(child) = holder.first_child() {
                holder.remove(&child);
            }
            holder.append(&build());
            if let Some(stack) = holder.ancestor(adw::ViewStack::static_type()).and_downcast::<adw::ViewStack>() {
                stack.set_visible_child_name("samples");
            }
        }
    };
    let rebuild: Rc<dyn Fn()> = Rc::new(rebuild);
    REBUILD.with(|r| r.borrow_mut().entry(key).or_default().push(rebuild.clone()));
    holder.connect_destroy(move |_| forget_page(key, &rebuild));
    // Rebuilt when undo or redo changes the zones, or the drum map renames a pad; edits made
    // here are already showing.
    let (weak, ctx2, native) = (holder.downgrade(), ctx.clone(), native.clone());
    ctx.on(move |c| {
        let Some(holder) = weak.upgrade() else { return };
        if c != Change::Project {
            return;
        }
        let now = map.descs();
        let named = if native.is_drum_kit() { pad_names(&ctx2, &native) } else { Vec::new() };
        if SEEN.with(|s| s.borrow().get(&key) == Some(&now)) && (named.is_empty() || *names.borrow() == named) {
            return;
        }
        SEEN.with(|s| s.borrow_mut().insert(key, now));
        *names.borrow_mut() = named;
        while let Some(child) = holder.first_child() {
            holder.remove(&child);
        }
        holder.append(&build());
    });
    Some(holder.upcast())
}

thread_local! {
    /// The zones each open sample page is showing, by instrument.
    static SEEN: std::cell::RefCell<std::collections::HashMap<usize, Vec<ZoneDesc>>> = Default::default();
    /// The pad each Pads page shows, and how to rebuild each open one, by instrument.
    static SELECTED: std::cell::RefCell<std::collections::HashMap<usize, usize>> = Default::default();
    static REBUILD: std::cell::RefCell<std::collections::HashMap<usize, Vec<Rc<dyn Fn()>>>> = Default::default();
}

/// Drops a closed page's entry. When it was the instrument's last page, the rest goes too:
/// the key is an address, which a later instrument can be given.
fn forget_page(key: usize, rebuild: &Rc<dyn Fn()>) {
    let last = REBUILD.with(|r| {
        let mut r = r.borrow_mut();
        let pages = r.entry(key).or_default();
        pages.retain(|p| !Rc::ptr_eq(p, rebuild));
        let last = pages.is_empty();
        if last {
            r.remove(&key);
        }
        last
    });
    if last {
        SELECTED.with(|s| s.borrow_mut().remove(&key));
        SEEN.with(|s| s.borrow_mut().remove(&key));
    }
}

fn zones(ctx: &Ctx, native: &Arc<Native>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().title("Zones").build();
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let add = gtk::Button::with_label("Add Samples…");
    let sfz = gtk::Button::with_label("Import SFZ…");
    buttons.append(&add);
    buttons.append(&sfz);
    group.set_header_suffix(Some(&buttons));
    page.add(&group);

    let rows: Rc<std::cell::RefCell<Vec<gtk::Widget>>> = Rc::default();
    let refresh: Rc<Refresh> = {
        let (ctx, native, group, rows) = (ctx.clone(), native.clone(), group.clone(), rows.clone());
        Rc::new_cyclic(|me: &std::rc::Weak<Refresh>| {
            let me = me.clone();
            Refresh(Box::new(move || {
                for r in rows.borrow_mut().drain(..) {
                    group.remove(&r);
                }
                let zones = native.samples.as_ref().map(|m| m.descs()).unwrap_or_default();
                if zones.is_empty() {
                    let empty = adw::ActionRow::builder().title("No samples").subtitle("Add samples or import an SFZ instrument").build();
                    group.add(&empty);
                    rows.borrow_mut().push(empty.upcast());
                }
                for (i, z) in zones.iter().enumerate() {
                    let row = zone_row(&ctx, &native, i, z, me.clone());
                    group.add(&row);
                    rows.borrow_mut().push(row.upcast());
                }
            }))
        })
    };
    refresh.run();

    {
        let (ctx, native, refresh) = (ctx.clone(), native.clone(), refresh.clone());
        add.connect_clicked(move |b| {
            let (ctx, native, refresh) = (ctx.clone(), native.clone(), refresh.clone());
            pick_files(b, "Audio Files", &AUDIO, true, move |files| {
                // Files named after a note go on that key; the rest fill keys upward from C3.
                let next = Rc::new(Cell::new(48u8));
                let make: Rc<dyn Fn(usize, &PathBuf, PathBuf) -> ZoneDesc> = Rc::new(move |_, orig, rel| {
                    let name = orig.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let root = note_from_name(&name).unwrap_or_else(|| {
                        let n = next.get();
                        next.set(n.saturating_add(1).min(127));
                        n
                    });
                    let mut z = ZoneDesc::new(rel, root);
                    z.name = orig.file_stem().map_or(z.name.clone(), |s| s.to_string_lossy().into_owned());
                    z
                });
                let r = refresh.clone();
                add_files(&ctx, &native, files, make, Rc::new(move || r.run()));
            });
        });
    }
    {
        let (ctx, native, refresh) = (ctx.clone(), native.clone(), refresh.clone());
        sfz.connect_clicked(move |b| {
            let (ctx, native, refresh) = (ctx.clone(), native.clone(), refresh.clone());
            pick_files(b, "SFZ Instruments", &["*.sfz"], false, move |files| {
                let Some(path) = files.first() else { return };
                let Ok(text) = std::fs::read_to_string(path) else {
                    ctx.message("Could not read the SFZ file");
                    return;
                };
                let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                let descs = parse_sfz(&text, &dir);
                if descs.is_empty() {
                    ctx.message("The SFZ file has no regions with samples");
                    return;
                }
                let sources: Vec<PathBuf> = descs.iter().map(|d| d.path.clone()).collect();
                let descs = Rc::new(descs);
                let make: Rc<dyn Fn(usize, &PathBuf, PathBuf) -> ZoneDesc> =
                    Rc::new(move |i, _, rel| ZoneDesc { path: rel, ..descs[i].clone() });
                let r = refresh.clone();
                add_files(&ctx, &native, sources, make, Rc::new(move || r.run()));
            });
        });
    }
    page.upcast()
}

fn zone_row(ctx: &Ctx, native: &Arc<Native>, i: usize, z: &ZoneDesc, refresh: std::rc::Weak<Refresh>) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(glib::markup_escape_text(&z.name))
        .subtitle(format!(
            "{}–{} · root {} · velocity {}–{}",
            note_name(z.lo),
            note_name(z.hi),
            note_name(z.root),
            z.vel_lo,
            z.vel_hi
        ))
        .build();
    let spin = |title: &str, min: f64, max: f64, step: f64, value: f64, f: fn(&mut ZoneDesc, f64)| {
        let r = adw::SpinRow::with_range(min, max, step);
        r.set_digits(if step < 1.0 { 1 } else { 0 });
        r.set_title(title);
        r.set_value(value);
        let (ctx, native) = (ctx.clone(), native.clone());
        r.connect_value_notify(move |r| {
            let v = r.value();
            update(&ctx, &native, i, |d| f(d, v));
        });
        r
    };
    row.add_row(&spin("Low Key", 0.0, 127.0, 1.0, z.lo as f64, |d, v| d.lo = v as u8));
    row.add_row(&spin("High Key", 0.0, 127.0, 1.0, z.hi as f64, |d, v| d.hi = v as u8));
    row.add_row(&spin("Root Key", 0.0, 127.0, 1.0, z.root as f64, |d, v| d.root = v as u8));
    row.add_row(&spin("Low Velocity", 1.0, 127.0, 1.0, z.vel_lo as f64, |d, v| d.vel_lo = v as u8));
    row.add_row(&spin("High Velocity", 1.0, 127.0, 1.0, z.vel_hi as f64, |d, v| d.vel_hi = v as u8));
    row.add_row(&spin("Tune (cents)", -2400.0, 2400.0, 1.0, z.tune as f64, |d, v| d.tune = v as f32));
    row.add_row(&spin("Level (dB)", -40.0, 12.0, 0.5, z.gain_db as f64, |d, v| d.gain_db = v as f32));
    row.add_row(&spin("Pan", -100.0, 100.0, 1.0, z.pan as f64 * 100.0, |d, v| d.pan = v as f32 / 100.0));
    let one = adw::SwitchRow::builder().title("One Shot").subtitle("Play to the end, ignoring note-off").active(z.one_shot).build();
    {
        let (ctx, native) = (ctx.clone(), native.clone());
        one.connect_active_notify(move |r| {
            let v = r.is_active();
            update(&ctx, &native, i, |d| d.one_shot = v);
        });
    }
    row.add_row(&one);
    let del = adw::ButtonRow::builder().title("Remove Zone").start_icon_name("user-trash-symbolic").build();
    del.add_css_class("destructive-action");
    {
        let (ctx, native) = (ctx.clone(), native.clone());
        del.connect_activated(move |_| {
            remove(&ctx, &native, i);
            run_weak(&refresh);
        });
    }
    row.add_row(&del);
    row
}

fn pads(ctx: &Ctx, native: &Arc<Native>, names: Vec<String>, pad: usize) -> gtk::Widget {
    let selected = Rc::new(Cell::new(pad.min(PADS.len() - 1)));
    let col = gtk::Box::new(gtk::Orientation::Vertical, 12);
    col.set_margin_start(12);
    col.set_margin_end(12);
    col.set_margin_top(12);
    col.set_margin_bottom(12);
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(6).row_homogeneous(true).column_homogeneous(true).build();
    let detail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&grid);
    col.append(&detail);

    let index_of = |native: &Native, pad: usize| -> Option<usize> {
        native.samples.as_ref()?.descs().iter().position(|d| d.one_shot && d.lo == PADS[pad].0)
    };

    let refresh: Rc<Refresh> = {
        let (ctx, native, grid, detail, selected) = (ctx.clone(), native.clone(), grid.clone(), detail.clone(), selected.clone());
        Rc::new_cyclic(|me: &std::rc::Weak<Refresh>| {
            let me = me.clone();
            Refresh(Box::new(move || {
                while let Some(c) = grid.first_child() {
                    grid.remove(&c);
                }
                let descs = native.samples.as_ref().map(|m| m.descs()).unwrap_or_default();
                for (p, ((note, _, _), name)) in PADS.iter().zip(&names).enumerate() {
                    let loaded = descs.iter().find(|d| d.one_shot && d.lo == *note);
                    let b = gtk::Button::new();
                    let inner = gtk::Box::new(gtk::Orientation::Vertical, 2);
                    let title = gtk::Label::new(Some(name.as_str()));
                    title.add_css_class("heading");
                    let sub = gtk::Label::new(Some(&loaded.map_or("Empty".to_string(), |d| d.path.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()))));
                    sub.add_css_class("caption");
                    sub.add_css_class("dim-label");
                    sub.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    inner.append(&title);
                    inner.append(&sub);
                    b.set_child(Some(&inner));
                    b.add_css_class("drum-pad");
                    if p == selected.get() {
                        b.add_css_class("suggested-action");
                    }
                    b.set_size_request(110, 64);
                    {
                        let (ctx, selected, me, note, key) = (ctx.clone(), selected.clone(), me.clone(), *note, Arc::as_ptr(&native) as usize);
                        b.connect_clicked(move |_| {
                            selected.set(p);
                            SELECTED.with(|s| s.borrow_mut().insert(key, p));
                            ctx.send(Command::Midi([0x99, note, 110]));
                            ctx.send(Command::Midi([0x89, note, 0]));
                            run_weak(&me);
                        });
                    }
                    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                    {
                        let (ctx, native, me) = (ctx.clone(), native.clone(), me.clone());
                        drop.connect_drop(move |_, v, _, _| {
                            let Ok(list) = v.get::<gdk::FileList>() else { return false };
                            let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).take(1).collect();
                            let done: Rc<dyn Fn()> = Rc::new({
                                let me = me.clone();
                                move || {
                                    run_weak(&me);
                                }
                            });
                            add_files(&ctx, &native, files, Rc::new(move |_, _, rel| pad_zone(p, rel)), done);
                            true
                        });
                    }
                    b.add_controller(drop);
                    grid.attach(&b, (p % 4) as i32, 3 - (p / 4) as i32, 1, 1);
                }

                while let Some(c) = detail.first_child() {
                    detail.remove(&c);
                }
                let p = selected.get();
                let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&names[p])).build();
                let load = gtk::Button::with_label("Load Sample…");
                group.set_header_suffix(Some(&load));
                {
                    let (ctx, native, me) = (ctx.clone(), native.clone(), me.clone());
                    load.connect_clicked(move |b| {
                        let (ctx, native, me) = (ctx.clone(), native.clone(), me.clone());
                        pick_files(b, "Audio Files", &AUDIO, false, move |files| {
                            let done: Rc<dyn Fn()> = Rc::new({
                                let me = me.clone();
                                move || {
                                    run_weak(&me);
                                }
                            });
                            add_files(&ctx, &native, files, Rc::new(move |_, _, rel| pad_zone(p, rel)), done);
                        });
                    });
                }
                if let Some(i) = index_of(&native, p) {
                    let d = descs[i].clone();
                    let spin = |title: &str, min: f64, max: f64, step: f64, value: f64, f: fn(&mut ZoneDesc, f64)| {
                        let r = adw::SpinRow::with_range(min, max, step);
                        r.set_digits(if step < 1.0 { 1 } else { 0 });
                        r.set_title(title);
                        r.set_value(value);
                        let (ctx, native) = (ctx.clone(), native.clone());
                        r.connect_value_notify(move |r| {
                            let v = r.value();
                            update(&ctx, &native, i, |d| f(d, v));
                        });
                        r
                    };
                    group.add(&spin("Level (dB)", -40.0, 12.0, 0.5, d.gain_db as f64, |d, v| d.gain_db = v as f32));
                    group.add(&spin("Pan", -100.0, 100.0, 1.0, d.pan as f64 * 100.0, |d, v| d.pan = v as f32 / 100.0));
                    group.add(&spin("Tune (semitones)", -24.0, 24.0, 0.1, d.tune as f64 / 100.0, |d, v| d.tune = v as f32 * 100.0));
                    group.add(&spin("Choke Group", 0.0, 16.0, 1.0, d.choke as f64, |d, v| d.choke = v as u8));
                    let clear = adw::ButtonRow::builder().title("Clear Pad").start_icon_name("user-trash-symbolic").build();
                    let (ctx, native, me) = (ctx.clone(), native.clone(), me.clone());
                    clear.connect_activated(move |_| {
                        remove(&ctx, &native, i);
                        run_weak(&me);
                    });
                    group.add(&clear);
                } else {
                    group.set_description(Some("Empty pad. Load a sample, or drop an audio file on the pad."));
                }
                detail.append(&group);
            }))
        })
    };
    refresh.run();
    // The page owns its rebuild function for as long as it exists.
    col.connect_map(move |_| {
        let _ = &refresh;
    });
    col.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_last_page_of_a_kit_forgets_its_pad() {
        let key = 0x1000;
        let pages: Vec<Rc<dyn Fn()>> = vec![Rc::new(|| ()), Rc::new(|| ())];
        REBUILD.with(|r| r.borrow_mut().insert(key, pages.clone()));
        SELECTED.with(|s| s.borrow_mut().insert(key, 5));
        forget_page(key, &pages[0]);
        assert_eq!(REBUILD.with(|r| r.borrow()[&key].len()), 1);
        assert_eq!(SELECTED.with(|s| s.borrow().get(&key).copied()), Some(5));
        forget_page(key, &pages[1]);
        assert!(REBUILD.with(|r| !r.borrow().contains_key(&key)));
        assert_eq!(SELECTED.with(|s| s.borrow().get(&key).copied()), None);
    }
}
