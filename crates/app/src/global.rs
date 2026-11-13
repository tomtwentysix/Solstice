//! Global tracks above the tracks area (G): arrangement, markers, signature, key and tempo.

use crate::canvas::{self, Canvas, rect, rounded, text};
use crate::studio::{Change, Ctx};
use crate::timeline::{HEADER_W, View};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    Arrangement,
    Marker,
    Signature,
    Key,
    Tempo,
}

impl Lane {
    pub const ALL: [Lane; 5] = [Lane::Arrangement, Lane::Marker, Lane::Signature, Lane::Key, Lane::Tempo];

    pub fn height(self) -> f32 {
        match self {
            Lane::Tempo => 56.0,
            _ => 22.0,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Lane::Arrangement => "Arrangement",
            Lane::Marker => "Marker",
            Lane::Signature => "Signature",
            Lane::Key => "Key",
            Lane::Tempo => "Tempo",
        }
    }
}

const SECTION_NAMES: [&str; 8] = ["Intro", "Verse", "Pre-Chorus", "Chorus", "Bridge", "Solo", "Breakdown", "Outro"];

pub const KEYS: [&str; 24] = [
    "C major", "C♯ major", "D major", "E♭ major", "E major", "F major", "F♯ major", "G major", "A♭ major", "A major", "B♭ major",
    "B major", "C minor", "C♯ minor", "D minor", "E♭ minor", "E minor", "F minor", "F♯ minor", "G minor", "G♯ minor", "A minor",
    "B♭ minor", "B minor",
];

fn lane_at(y: f64) -> Option<(Lane, f32)> {
    let mut top = 0.0;
    for l in Lane::ALL {
        if y < (top + l.height()) as f64 {
            return Some((l, top));
        }
        top += l.height();
    }
    None
}

fn top_of(lane: Lane) -> f32 {
    Lane::ALL.iter().take_while(|l| **l != lane).map(|l| l.height()).sum()
}

/// The tempo lane shows this range, widened to fit every tempo in the project.
fn tempo_range(p: &Project) -> (f64, f64) {
    let all = std::iter::once(p.tempo).chain(p.tempo_changes.iter().map(|c| c.tempo));
    let (lo, hi) = all.fold((f64::MAX, f64::MIN), |(a, b), t| (a.min(t), b.max(t)));
    ((lo - 20.0).max(20.0).floor(), (hi + 20.0).min(400.0).ceil())
}

fn tempo_y(t: f64, range: (f64, f64)) -> f32 {
    let h = Lane::Tempo.height() - 10.0;
    top_of(Lane::Tempo) + 5.0 + h * (1.0 - ((t - range.0) / (range.1 - range.0)) as f32)
}

fn y_tempo(y: f64, range: (f64, f64)) -> f64 {
    let h = (Lane::Tempo.height() - 10.0) as f64;
    let n = 1.0 - (y - top_of(Lane::Tempo) as f64 - 5.0) / h;
    (range.0 + n.clamp(0.0, 1.0) * (range.1 - range.0)).round()
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    None,
    /// A tempo change (or the opening tempo for `None`): where it started and its tempo then.
    Tempo { index: Option<usize>, tick: u64, tempo: f64, range: (f64, f64) },
}

/// The global tracks row: lane names beside lanes that scroll with the tracks area.
pub fn build(ctx: &Ctx, view: &Rc<View>) -> (gtk::Widget, Canvas) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
    names.set_size_request(HEADER_W, -1);
    names.add_css_class("track-headers");
    for l in Lane::ALL {
        let label = gtk::Label::new(Some(l.title()));
        label.set_xalign(1.0);
        label.set_margin_end(10);
        label.set_size_request(-1, l.height() as i32);
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        names.append(&label);
    }
    let drag = Rc::new(Cell::new(Drag::None));
    let lanes = {
        let (ctx, view, drag) = (ctx.clone(), view.clone(), drag.clone());
        Canvas::new(move |c, s, w, h| draw(&ctx, &view, drag.get(), c, s, w, h))
    };
    lanes.set_hexpand(true);
    lanes.set_size_request(-1, Lane::ALL.iter().map(|l| l.height()).sum::<f32>() as i32);
    row.append(&names);
    row.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    row.append(&lanes);
    install_input(ctx, view, &lanes, drag);
    {
        let lanes = lanes.clone();
        ctx.on(move |c| {
            if !matches!(c, Change::LiveNote) {
                lanes.queue_draw();
            }
        });
    }
    (row.upcast(), lanes)
}

fn draw(ctx: &Ctx, view: &View, _drag: Drag, c: &Canvas, s: &gtk::Snapshot, w: f32, _h: f32) {
    let st = ctx.s();
    let p = &st.project;
    let time = p.time();
    let fg = canvas::fg(c);
    let faint = gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12);
    for l in Lane::ALL {
        rect(s, 0.0, top_of(l) + l.height() - 1.0, w, 1.0, &faint);
    }

    let ay = top_of(Lane::Arrangement);
    for (i, sec) in p.sections.iter().enumerate() {
        let (x0, x1) = (view.x(sec.start), view.x(sec.end()));
        if x1 < 0.0 || x0 > w {
            continue;
        }
        let col = canvas::track_color((i % 8) as u8);
        rounded(s, x0 + 1.0, ay + 2.0, (x1 - x0 - 2.0).max(2.0), Lane::Arrangement.height() - 4.0, 3.0, &col);
        text(s, c, x0 + 6.0, ay + 4.0, &sec.name, &canvas::rgba(0xffffff, 0.95), Some(8.0));
    }

    let my = top_of(Lane::Marker);
    for m in &p.markers {
        let x = view.x(m.pos);
        if x > w || x < -200.0 {
            continue;
        }
        rect(s, x, my + 2.0, 1.5, Lane::Marker.height() - 4.0, &fg);
        rounded(s, x, my + 3.0, 8.0, 8.0, 1.0, &canvas::accent(c));
        text(s, c, x + 11.0, my + 3.0, &m.name, &fg, Some(8.0));
    }

    let ky = top_of(Lane::Key);
    for k in &p.key_changes {
        let x = view.x(time.bar_start(k.bar));
        if x <= w {
            rect(s, x, ky + 3.0, 1.0, Lane::Key.height() - 6.0, &fg);
            text(s, c, x + 4.0, ky + 3.0, &k.key, &fg, Some(8.5));
        }
    }

    // Signature: the opening one and each change.
    let sy = top_of(Lane::Signature);
    let mut sigs = vec![(0u64, p.sig_num, p.sig_den)];
    sigs.extend(p.sig_changes.iter().map(|c| (c.bar, c.num, c.den)));
    for (bar, num, den) in sigs {
        let x = view.x(time.bar_start(bar));
        if x > w {
            continue;
        }
        rect(s, x, sy + 3.0, 1.0, Lane::Signature.height() - 6.0, &fg);
        text(s, c, x + 4.0, sy + 3.0, &format!("{num}/{den}"), &fg, Some(8.5));
    }

    // Tempo: a step line through each change, with a handle and value at each.
    let range = tempo_range(p);
    let accent = canvas::accent(c);
    let mut points = vec![(0u64, p.tempo)];
    let mut changes = p.tempo_changes.clone();
    changes.sort_by_key(|c| c.tick);
    points.extend(changes.iter().map(|c| (c.tick, c.tempo)));
    for (i, &(tick, tempo)) in points.iter().enumerate() {
        let x = view.x(time.ticks_to_samples(tick));
        let next_x = points.get(i + 1).map_or(w, |n| view.x(time.ticks_to_samples(n.0)));
        let y = tempo_y(tempo, range);
        if next_x < 0.0 || x > w {
            continue;
        }
        rect(s, x.max(0.0), y, (next_x - x.max(0.0)).max(1.0), 1.5, &accent);
        if let Some(n) = points.get(i + 1) {
            let ny = tempo_y(n.1, range);
            rect(s, next_x, y.min(ny), 1.5, (ny - y).abs() + 1.5, &accent);
        }
        rounded(s, x - 3.0, y - 3.0, 7.0, 7.0, 3.5, &accent);
        text(s, c, x + 5.0, y - 13.0, &format!("{tempo:.1}"), &fg, Some(7.5));
    }
}

/// Index into `tempo_changes` of the change whose handle is near `x`.
fn hit_tempo(p: &Project, view: &View, x: f64) -> Option<usize> {
    let time = p.time();
    p.tempo_changes.iter().position(|c| (view.x(time.ticks_to_samples(c.tick)) as f64 - x).abs() < 6.0)
}

fn install_input(ctx: &Ctx, view: &Rc<View>, lanes: &Canvas, drag: Rc<Cell<Drag>>) {
    let g = gtk::GestureDrag::new();
    {
        let (ctx, view, drag, lanes) = (ctx.clone(), view.clone(), drag.clone(), lanes.clone());
        g.connect_drag_begin(move |g, x, y| {
            let Some((lane, _)) = lane_at(y) else { return };
            let alt = g.current_event_state().contains(gdk::ModifierType::ALT_MASK);
            let right = g.current_button() == gdk::BUTTON_SECONDARY;
            // Only the tempo lane uses the right button (to delete); the others take left clicks.
            if g.current_button() == gdk::BUTTON_MIDDLE || (right && lane != Lane::Tempo) {
                return;
            }
            match lane {
                Lane::Signature => signature_popover(&ctx, &view, &lanes, x),
                Lane::Arrangement => arrangement_click(&ctx, &view, &lanes, x),
                Lane::Marker => marker_click(&ctx, &view, &lanes, x),
                Lane::Key => key_popover(&ctx, &view, &lanes, x),
                Lane::Tempo => {
                    let (time, range, hit, opening) = {
                        let s = ctx.s();
                        let p = &s.project;
                        (p.time(), tempo_range(p), hit_tempo(p, &view, x), p.tempo)
                    };
                    if let Some(i) = hit
                        && (alt || right)
                    {
                        ctx.edit("Delete Tempo Change", |p| {
                            let old = p.time();
                            p.tempo_changes.remove(i);
                            p.retime(&old);
                        });
                        return;
                    }
                    let first_change = ctx.s().project.tempo_changes.iter().map(|c| c.tick).min();
                    let index = match hit {
                        Some(i) => Some(i),
                        // Before the first change the line is the opening tempo.
                        None if first_change.is_none_or(|t| view.pos(x) < time.ticks_to_samples(t))
                            && (tempo_y(opening, range) as f64 - y).abs() < 8.0 =>
                        {
                            None
                        }
                        None => {
                            // Anywhere else adds a change on the nearest beat.
                            let tick = time.samples_to_ticks(time.snap(view.pos(x), time.ticks_per_beat()));
                            if tick == 0 {
                                return;
                            }
                            let tempo = y_tempo(y, range);
                            let mut i = 0;
                            ctx.edit("Add Tempo Change", |p| {
                                let old = p.time();
                                p.tempo_changes.retain(|c| c.tick != tick);
                                p.tempo_changes.push(TempoChange { tick, tempo });
                                p.tempo_changes.sort_by_key(|c| c.tick);
                                i = p.tempo_changes.iter().position(|c| c.tick == tick).unwrap();
                                p.retime(&old);
                            });
                            Some(i)
                        }
                    };
                    let (tick, tempo) = {
                        let s = ctx.s();
                        match index {
                            Some(i) => (s.project.tempo_changes[i].tick, s.project.tempo_changes[i].tempo),
                            None => (0, s.project.tempo),
                        }
                    };
                    ctx.begin_gesture();
                    drag.set(Drag::Tempo { index, tick, tempo, range });
                }
            }
        });
    }
    {
        let (ctx, view, drag) = (ctx.clone(), view.clone(), drag.clone());
        g.connect_drag_update(move |_, dx, dy| {
            let Drag::Tempo { index, tick, tempo, range } = drag.get() else { return };
            let per_px = (range.1 - range.0) / (Lane::Tempo.height() as f64 - 10.0);
            let new_tempo = (tempo - dy * per_px).clamp(20.0, 400.0).round();
            let new_tick = index.map(|_| {
                let time = ctx.s().project.time();
                let at = time.ticks_to_samples(tick) as f64 + dx * view.spp.get();
                time.samples_to_ticks(time.snap(at.max(0.0) as u64, time.ticks_per_beat())).max(1)
            });
            ctx.live(true, |p| {
                let old = p.time();
                match index {
                    Some(i) if i < p.tempo_changes.len() => {
                        p.tempo_changes[i].tempo = new_tempo;
                        p.tempo_changes[i].tick = new_tick.unwrap_or(tick);
                    }
                    Some(_) => return,
                    None => p.tempo = new_tempo,
                }
                p.retime(&old);
            });
        });
    }
    {
        let (ctx, drag) = (ctx.clone(), drag.clone());
        g.connect_drag_end(move |_, _, _| {
            if let Drag::Tempo { index, .. } = drag.replace(Drag::None) {
                ctx.live(true, |p| {
                    // Two changes dragged onto one beat: keep the one just moved.
                    if let Some(moved) = index.and_then(|i| p.tempo_changes.get(i).cloned()) {
                        p.tempo_changes.retain(|c| c.tick != moved.tick);
                        p.tempo_changes.push(moved);
                    }
                    p.tempo_changes.sort_by_key(|c| c.tick);
                });
                ctx.end_gesture("Change Tempo");
            }
        });
    }
    g.set_button(0);
    lanes.add_controller(g);
}

/// "2+2+3" for [2, 2, 3].
pub fn format_groups(groups: &[u8]) -> String {
    groups.iter().map(|g| g.to_string()).collect::<Vec<_>>().join("+")
}

/// Reads a grouping such as "2+2+3", which must add up to `num`; empty text is the default.
pub fn parse_groups(text: &str, num: u8) -> Option<Vec<u8>> {
    let text = text.trim();
    if text.is_empty() {
        return Some(Vec::new());
    }
    let groups = text.split('+').map(|g| g.trim().parse::<u8>().ok().filter(|g| *g > 0)).collect::<Option<Vec<u8>>>()?;
    (groups.iter().map(|&g| g as u32).sum::<u32>() == num as u32).then_some(groups)
}

pub fn grouping_entry(groups: &[u8]) -> gtk::Entry {
    let e = gtk::Entry::builder().text(format_groups(groups)).width_chars(10).build();
    e.set_tooltip_text(Some("Beats per click group, such as 2+2+3"));
    e
}

/// Shows the signature's default grouping as the placeholder and marks text that doesn't fit it.
pub fn check_grouping(e: &gtk::Entry, num: u8, den: u8) {
    e.set_placeholder_text(Some(&format_groups(&default_groups(num, den))));
    if parse_groups(&e.text(), num).is_some() {
        e.remove_css_class("error");
    } else {
        e.add_css_class("error");
    }
}

/// Sets or removes the signature from the bar nearest `x`.
fn signature_popover(ctx: &Ctx, view: &View, lanes: &Canvas, x: f64) {
    let (bar, num, den, groups, existing) = {
        let s = ctx.s();
        let p = &s.project;
        let time = p.time();
        let pos = view.pos(x);
        let bar = time.bar_at(pos + time.bar_len() / 2).min(time.bar_at(pos) + 1);
        let bar = if (view.x(time.bar_start(bar)) as f64 - x).abs() < 40.0 { bar } else { time.bar_at(pos) };
        let (num, den) = time.sig_at(time.bar_start(bar));
        let change = p.sig_changes.iter().find(|c| c.bar == bar);
        let groups = if bar == 0 { p.sig_groups.clone() } else { change.map(|c| c.groups.clone()).unwrap_or_default() };
        (bar, num, den, groups, change.is_some())
    };
    let pop = gtk::Popover::new();
    pop.set_parent(lanes);
    let xb = view.x(ctx.s().project.time().bar_start(bar));
    pop.set_pointing_to(Some(&gdk::Rectangle::new(xb as i32, 0, 1, Lane::Signature.height() as i32)));
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(6).build();
    grid.attach(&gtk::Label::new(Some(&format!("Bar {}", bar + 1))), 0, 0, 3, 1);
    let n = gtk::SpinButton::with_range(1.0, 32.0, 1.0);
    n.set_value(num as f64);
    let dens = [2u8, 4, 8, 16];
    let d = gtk::DropDown::from_strings(&["2", "4", "8", "16"]);
    d.set_selected(dens.iter().position(|x| *x == den).unwrap_or(1) as u32);
    grid.attach(&n, 0, 1, 1, 1);
    grid.attach(&gtk::Label::new(Some("/")), 1, 1, 1, 1);
    grid.attach(&d, 2, 1, 1, 1);
    let label = gtk::Label::builder().label("Grouping").xalign(0.0).build();
    grid.attach(&label, 0, 2, 3, 1);
    let g = grouping_entry(&groups);
    grid.attach(&g, 0, 3, 3, 1);
    let check = {
        let (n, d, g) = (n.clone(), d.clone(), g.clone());
        Rc::new(move || check_grouping(&g, n.value() as u8, dens[d.selected() as usize]))
    };
    check();
    let c = check.clone();
    n.connect_value_changed(move |_| c());
    let c = check.clone();
    d.connect_selected_notify(move |_| c());
    g.connect_changed(move |_| check());
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    remove.set_visible(existing);
    grid.attach(&remove, 0, 4, 3, 1);
    pop.set_child(Some(&grid));
    let removed = Rc::new(Cell::new(false));
    {
        let (pop, removed) = (pop.clone(), removed.clone());
        remove.connect_clicked(move |_| {
            removed.set(true);
            pop.popdown();
        });
    }
    let ctx = ctx.clone();
    pop.connect_closed(move |p| {
        let (num2, den2) = (n.value() as u8, dens[d.selected() as usize]);
        let groups2 = parse_groups(&g.text(), num2).unwrap_or_else(|| groups.clone());
        let remove = removed.get();
        if remove || num2 != num || den2 != den || groups2 != groups {
            ctx.edit(if remove { "Remove Signature Change" } else { "Change Signature" }, |p| {
                let old = p.time();
                p.sig_changes.retain(|c| c.bar != bar);
                if bar == 0 {
                    p.sig_num = num2;
                    p.sig_den = den2;
                    p.sig_groups = groups2.clone();
                } else if !remove {
                    p.sig_changes.push(SigChange { bar, num: num2, den: den2, groups: groups2.clone() });
                    p.sig_changes.sort_by_key(|c| c.bar);
                }
                p.retime(&old);
            });
        }
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

fn popover_at(lanes: &Canvas, x: f32, lane: Lane) -> gtk::Popover {
    let pop = gtk::Popover::new();
    pop.set_parent(lanes);
    pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, top_of(lane) as i32, 1, lane.height() as i32)));
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop
}

/// A section under the click opens its menu; a click elsewhere adds an eight-bar section
/// there, shortened to fit before the next one.
fn arrangement_click(ctx: &Ctx, view: &View, lanes: &Canvas, x: f64) {
    let pos = view.pos(x);
    let hit = ctx.s().project.sections.iter().find(|s| s.start <= pos && pos < s.end()).cloned();
    let Some(sec) = hit else {
        ctx.edit("Add Arrangement Marker", |p| {
            let time = p.time();
            let bar = time.bar_at(pos);
            let start = time.bar_start(bar);
            let next = p.sections.iter().map(|s| s.start).filter(|s| *s > start).min().unwrap_or(u64::MAX);
            let end = time.bar_start(bar + 8).min(next);
            if end <= start {
                return;
            }
            let id = p.alloc_id();
            let name = SECTION_NAMES[p.sections.len() % SECTION_NAMES.len()].to_string();
            p.sections.push(Section { id, start, length: end - start, name });
            p.sections.sort_by_key(|s| s.start);
        });
        return;
    };
    let pop = popover_at(lanes, view.x(sec.start), Lane::Arrangement);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let name = gtk::Entry::builder().text(&sec.name).build();
    col.append(&name);
    let bars_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bars_row.append(&gtk::Label::new(Some("Bars")));
    let time = ctx.s().project.time();
    let bars = (time.bar_at(sec.end().saturating_sub(1)) + 1).saturating_sub(time.bar_at(sec.start));
    let spin = gtk::SpinButton::with_range(1.0, 256.0, 1.0);
    spin.set_value(bars as f64);
    bars_row.append(&spin);
    col.append(&bars_row);
    for (label, dup) in [("Duplicate", true), ("Delete with Contents", false)] {
        let b = gtk::Button::with_label(label);
        if !dup {
            b.add_css_class("destructive-action");
        }
        let (ctx, pop) = (ctx.clone(), pop.clone());
        b.connect_clicked(move |_| {
            pop.popdown();
            if dup {
                ctx.edit("Duplicate Section", |p| {
                    p.duplicate_section(sec.id);
                });
            } else {
                ctx.edit("Delete Section", |p| p.delete_section(sec.id));
            }
        });
        col.append(&b);
    }
    pop.set_child(Some(&col));
    let ctx = ctx.clone();
    pop.connect_closed(move |_| {
        let (new_name, new_bars) = (name.text().to_string(), spin.value() as u64);
        let Some(cur) = ctx.s().project.sections.iter().find(|s| s.id == sec.id).cloned() else { return };
        if new_name != cur.name || new_bars != bars {
            ctx.edit("Change Section", |p| {
                let time = p.time();
                let end = time.bar_start(time.bar_at(cur.start) + new_bars);
                let next = p.sections.iter().map(|s| s.start).filter(|s| *s > cur.start).min().unwrap_or(u64::MAX);
                if let Some(s) = p.sections.iter_mut().find(|s| s.id == sec.id) {
                    s.name = new_name;
                    s.length = end.min(next) - s.start;
                }
            });
        }
    });
    pop.popup();
}

/// Adds a marker on the nearest bar, or opens the one clicked for renaming, deleting or
/// moving the playhead to it.
fn marker_click(ctx: &Ctx, view: &View, lanes: &Canvas, x: f64) {
    let hit = ctx.s().project.markers.iter().find(|m| x >= view.x(m.pos) as f64 - 3.0 && x <= view.x(m.pos) as f64 + 60.0).cloned();
    let Some(m) = hit else {
        ctx.edit("Create Marker", |p| {
            let time = p.time();
            let pos = time.snap(view.pos(x), time.ticks_per_bar());
            let id = p.alloc_id();
            let n = p.markers.len() + 1;
            p.markers.push(Marker { id, pos, name: format!("Marker {n}") });
            p.markers.sort_by_key(|m| m.pos);
        });
        return;
    };
    let pop = popover_at(lanes, view.x(m.pos), Lane::Marker);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let name = gtk::Entry::builder().text(&m.name).build();
    col.append(&name);
    let go = gtk::Button::with_label("Go to Marker");
    {
        let (ctx, pop, pos) = (ctx.clone(), pop.clone(), m.pos);
        go.connect_clicked(move |_| {
            pop.popdown();
            ctx.locate(pos);
        });
    }
    col.append(&go);
    let delete = gtk::Button::with_label("Delete Marker");
    delete.add_css_class("destructive-action");
    let deleted = Rc::new(Cell::new(false));
    {
        let (pop, deleted) = (pop.clone(), deleted.clone());
        delete.connect_clicked(move |_| {
            deleted.set(true);
            pop.popdown();
        });
    }
    col.append(&delete);
    pop.set_child(Some(&col));
    let ctx = ctx.clone();
    pop.connect_closed(move |_| {
        let text = name.text().to_string();
        if deleted.get() {
            ctx.edit("Delete Marker", |p| p.markers.retain(|x| x.id != m.id));
        } else if text != m.name {
            ctx.edit("Rename Marker", |p| {
                if let Some(x) = p.markers.iter_mut().find(|x| x.id == m.id) {
                    x.name = text;
                }
            });
        }
    });
    pop.popup();
}

fn key_popover(ctx: &Ctx, view: &View, lanes: &Canvas, x: f64) {
    let (bar, current) = {
        let s = ctx.s();
        let time = s.project.time();
        let bar = time.bar_at(view.pos(x));
        let cur = s.project.key_changes.iter().rev().find(|k| k.bar <= bar).map(|k| k.key.clone());
        (bar, cur)
    };
    let pop = popover_at(lanes, view.x(ctx.s().project.time().bar_start(bar)), Lane::Key);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.append(&gtk::Label::new(Some(&format!("Key from bar {}", bar + 1))));
    let dd = gtk::DropDown::from_strings(&KEYS);
    dd.set_selected(current.as_deref().and_then(|k| KEYS.iter().position(|x| *x == k)).unwrap_or(0) as u32);
    let opened = dd.selected();
    col.append(&dd);
    let existing = ctx.s().project.key_changes.iter().any(|k| k.bar == bar);
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    remove.set_visible(existing);
    let removed = Rc::new(Cell::new(false));
    {
        let (pop, removed) = (pop.clone(), removed.clone());
        remove.connect_clicked(move |_| {
            removed.set(true);
            pop.popdown();
        });
    }
    col.append(&remove);
    pop.set_child(Some(&col));
    let ctx = ctx.clone();
    pop.connect_closed(move |_| {
        let key = KEYS[dd.selected() as usize].to_string();
        if removed.get() {
            ctx.edit("Remove Key Change", |p| p.key_changes.retain(|k| k.bar != bar));
        } else if dd.selected() != opened {
            ctx.edit("Set Key", |p| {
                p.key_changes.retain(|k| k.bar != bar);
                p.key_changes.push(KeyChange { bar, key });
                p.key_changes.sort_by_key(|k| k.bar);
            });
        }
    });
    pop.popup();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groupings_parse_only_when_they_fill_the_bar() {
        assert_eq!(parse_groups("2+2+3", 7), Some(vec![2, 2, 3]));
        assert_eq!(parse_groups(" 3 + 3 ", 6), Some(vec![3, 3]));
        assert_eq!(parse_groups("", 7), Some(vec![]));
        assert_eq!(parse_groups("2+2", 7), None);
        assert_eq!(parse_groups("2+0+5", 7), None);
        assert_eq!(parse_groups("2,2,3", 7), None);
        assert_eq!(format_groups(&[2, 2, 3]), "2+2+3");
    }
}
