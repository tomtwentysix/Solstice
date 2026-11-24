//! The drum rows the step sequencer and the Drum Editor share: which rows a track shows, its
//! drum map, and the row header drawn beside both grids.

use crate::canvas::{self, Canvas, accent, rect, rgba, rounded, text};
use crate::context_menu::{Menu, ask_name};
use crate::studio::Ctx;
use gtk::prelude::*;
use gtk::gdk;
use solstice_dsp::Native;
use solstice_dsp::sampler::PADS;
use solstice_engine::Command;
use solstice_model::*;
use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const ROW_H: f32 = 26.0;
pub(crate) const HEAD_W: f32 = 220.0;
const BADGE_X: f32 = 112.0;
const BADGE_W: f32 = 46.0;
const MUTE_X: f32 = 162.0;
const SOLO_X: f32 = 182.0;
const MENU_X: f32 = 204.0;
const BOX_W: f32 = 18.0;

pub(crate) fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 1)
}

pub(crate) fn drum_row(pitch: u8, name: String) -> DrumRow {
    DrumRow { pitch, name, hidden: false, mute: false, solo: false, grid: None }
}

/// Whether a track plays drums: a Drum Kit, or a drum map that has been set up.
pub(crate) fn is_drum_track(t: &Track) -> bool {
    is_drum_kit(t) || !t.drum_map.is_empty()
}

pub(crate) fn is_drum_kit(t: &Track) -> bool {
    t.instrument.as_ref().is_some_and(|i| i.uid == "solstice:drums")
}

/// The rows the Hits view shows for a track, in order, hidden ones left out.
pub(crate) fn hit_rows(t: &Track, notes: &[Note]) -> Vec<DrumRow> {
    let mut used: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
    used.sort_unstable();
    used.dedup();
    let mut rows = all_rows(&t.drum_map, &defaults(t), &used);
    rows.retain(|r| !r.hidden);
    rows
}

/// The pad a Drum Kit plays on `pitch`.
pub(crate) fn pad_of(pitch: u8) -> Option<usize> {
    PADS.iter().position(|p| p.0 == pitch)
}

/// The selected region's Drum Kit: its slot and instance.
pub(crate) fn kit(ctx: &Ctx) -> Option<(Id, Arc<Native>, bool)> {
    let s = ctx.s();
    let (ti, _) = s.selected_region.and_then(|id| s.project.find_region(id))?;
    let t = &s.project.tracks[ti];
    if !is_drum_kit(t) {
        return None;
    }
    let sid = t.instrument.as_ref()?.id;
    let native = s.plugins.get(&sid)?.inst.native()?.clone();
    Some((sid, native, t.freeze.is_some()))
}

pub(crate) fn load_sample(ctx: &Ctx, pitch: u8, files: Vec<PathBuf>) {
    let (Some(pad), Some((_, native, false))) = (pad_of(pitch), kit(ctx)) else { return };
    crate::sample_editor::load_pad(ctx, &native, pad, files);
}

pub(crate) fn show_pad(ctx: &Ctx, pitch: u8, anchor: &impl IsA<gtk::Widget>) {
    let (Some(pad), Some((sid, native, _))) = (pad_of(pitch), kit(ctx)) else { return };
    crate::sample_editor::select_pad(&native, pad);
    crate::plugins::open_editor(ctx, sid, anchor);
}

/// The rows a track shows before its drum map is edited: the Drum Kit's pads, or a run of
/// notes for other instruments.
pub(crate) fn defaults(t: &Track) -> Vec<DrumRow> {
    if is_drum_kit(t) {
        PADS.iter().map(|(n, name, _)| drum_row(*n, name.to_string())).collect()
    } else {
        (0..16u8).map(|i| 72 - i).map(|n| drum_row(n, note_name(n))).collect()
    }
}

/// Every row, hidden ones included: the drum map (or the defaults while it is empty), then any
/// pitch in `used` it lacks. Unnamed rows take their default name.
pub(crate) fn all_rows(map: &[DrumRow], defaults: &[DrumRow], used: &[u8]) -> Vec<DrumRow> {
    let mut rows = if map.is_empty() { defaults.to_vec() } else { map.to_vec() };
    for r in rows.iter_mut().filter(|r| r.name.is_empty()) {
        r.name = defaults.iter().find(|d| d.pitch == r.pitch).map_or_else(|| note_name(r.pitch), |d| d.name.clone());
    }
    let mut extra: Vec<u8> = used.iter().copied().filter(|p| !rows.iter().any(|r| r.pitch == *p)).collect();
    extra.sort_unstable();
    extra.dedup();
    rows.extend(extra.into_iter().map(|p| drum_row(p, note_name(p))));
    rows
}

/// Moves the row on `pitch` to where the row on `target` is, shifting the rows between.
pub(crate) fn place_row(map: &mut Vec<DrumRow>, pitch: u8, target: u8) {
    let (Some(from), Some(to)) = (map.iter().position(|r| r.pitch == pitch), map.iter().position(|r| r.pitch == target)) else { return };
    let row = map.remove(from);
    map.insert(to, row);
}

/// Swaps the row on `pitch` with the next shown row above or below it.
pub(crate) fn move_row(map: &mut [DrumRow], pitch: u8, up: bool) {
    let Some(i) = map.iter().position(|r| r.pitch == pitch) else { return };
    let other = if up {
        map[..i].iter().rposition(|r| !r.hidden)
    } else {
        map[i + 1..].iter().position(|r| !r.hidden).map(|j| i + 1 + j)
    };
    if let Some(j) = other {
        map.swap(i, j);
    }
}

/// Edits the selected region's track's drum map, filling it in from what is shown first.
pub(crate) fn edit_map(ctx: &Ctx, label: &str, used: Vec<u8>, f: impl FnOnce(&mut Vec<DrumRow>)) {
    let Some(id) = ctx.s().selected_region else { return };
    ctx.edit(label, |p| {
        let Some(ti) = p.find_region(id).map(|(ti, _)| ti) else { return };
        let t = &mut p.tracks[ti];
        t.drum_map = all_rows(&t.drum_map, &defaults(t), &used);
        f(&mut t.drum_map);
    });
}

/// Turns region `id` from a step pattern into plain hits, filling in the track's drum map
/// from the rows it shows when it has none.
pub(crate) fn release(p: &mut Project, id: Id) {
    let Some((ti, Region { content: Content::Midi(m), .. })) = p.find_region(id) else { return };
    if m.pattern.is_none() {
        return;
    }
    let t = &p.tracks[ti];
    let used: Vec<u8> = m.notes.iter().map(|n| n.pitch).collect();
    let shown = all_rows(&t.drum_map, &defaults(t), &used);
    p.release_pattern(id, shown);
}

pub(crate) fn audition(ctx: &Ctx, pitch: u8, velocity: u8) {
    ctx.send(Command::Midi([0x99, pitch, velocity]));
    ctx.send(Command::Midi([0x89, pitch, 0]));
}


/// What a drum page tells the shared row header about its rows.
pub(crate) trait RowSource {
    /// The shown rows, or `None` with no MIDI region selected.
    fn rows(&self, ctx: &Ctx) -> Option<Vec<DrumRow>>;
    /// Pitches with hits, which `edit_map` keeps rows for.
    fn used(&self, ctx: &Ctx) -> Vec<u8>;
    /// The badge's text, and whether the row has its own setting.
    fn badge(&self, ctx: &Ctx, row: &DrumRow) -> (String, bool);
    fn badge_tip(&self) -> &'static str;
    fn badge_clicked(&self, ctx: &Ctx, anchor: &Canvas, x: f64, y: f64, row: &DrumRow);
    /// Adds the page's own items to the top of a row's menu.
    fn extend_menu(&self, ctx: &Ctx, m: &Menu, anchor: &Canvas, x: f64, y: f64, row: &DrumRow);
    fn focus_row(&self) -> usize;
    fn set_focus_row(&self, i: usize);
}

/// A row being dragged to a new place: from and to, as shown-row indices.
#[derive(Default)]
pub(crate) struct Header {
    pub moving: Cell<Option<(usize, usize)>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Name,
    Badge,
    Mute,
    Solo,
    Menu,
}

fn zone(x: f64) -> Zone {
    let x = x as f32;
    if x >= MENU_X {
        Zone::Menu
    } else if x >= SOLO_X {
        Zone::Solo
    } else if x >= MUTE_X {
        Zone::Mute
    } else if x >= BADGE_X {
        Zone::Badge
    } else {
        Zone::Name
    }
}

pub(crate) fn draw_header(ctx: &Ctx, src: &dyn RowSource, h: &Header, grid: &Canvas, c: &Canvas, s: &gtk::Snapshot, w: f32) {
    let Some(rows) = src.rows(ctx) else { return };
    let fg = canvas::fg(c);
    let shade = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    let acc = accent(c);
    let focus = grid.has_focus().then(|| src.focus_row());
    for (i, row) in rows.iter().enumerate() {
        let y = i as f32 * ROW_H;
        if focus == Some(i) {
            rect(s, 0.0, y, w, ROW_H, &shade(0.06));
        }
        rect(s, 0.0, y + ROW_H - 1.0, w, 1.0, &shade(0.08));
        s.push_clip(&gtk::graphene::Rect::new(0.0, y, BADGE_X - 4.0, ROW_H));
        text(s, c, 8.0, y + 4.0, &row.name, &fg, Some(8.5));
        s.pop();
        let (badge, own) = src.badge(ctx, row);
        rounded(s, BADGE_X, y + 5.0, BADGE_W, ROW_H - 10.0, 4.0, &if own { acc } else { shade(0.08) });
        let badge_fg = if own { rgba(0xffffff, 1.0) } else { shade(0.6) };
        s.push_clip(&gtk::graphene::Rect::new(BADGE_X, y, BADGE_W, ROW_H));
        text(s, c, BADGE_X + 4.0, y + 6.0, &badge, &badge_fg, Some(7.0));
        s.pop();
        for (x, label, on, color) in [(MUTE_X, "M", row.mute, rgba(0xe5a50a, 1.0)), (SOLO_X, "S", row.solo, acc)] {
            rounded(s, x, y + 4.0, BOX_W, ROW_H - 8.0, 3.0, &if on { color } else { shade(0.1) });
            text(s, c, x + 5.0, y + 5.0, label, &if on { rgba(0xffffff, 1.0) } else { shade(0.7) }, Some(7.5));
        }
        text(s, c, MENU_X + 4.0, y + 3.0, "⋮", &shade(0.7), Some(9.0));
    }
    if let Some((from, to)) = h.moving.get()
        && from != to
    {
        let y = if to > from { to + 1 } else { to } as f32 * ROW_H;
        rect(s, 0.0, y - 1.0, w, 2.0, &acc);
    }
}

fn toggle(ctx: &Ctx, label: &str, used: Vec<u8>, pitch: u8, f: fn(&mut DrumRow)) {
    edit_map(ctx, label, used, |map| {
        if let Some(r) = map.iter_mut().find(|r| r.pitch == pitch) {
            f(r);
        }
    });
}

fn rename(ctx: &Ctx, anchor: &Canvas, x: f64, y: f64, row: &DrumRow, used: Vec<u8>) {
    let (ctx, pitch) = (ctx.clone(), row.pitch);
    ask_name(anchor, x, y, &row.name, move |name| {
        edit_map(&ctx, "Rename Drum Row", used.clone(), |map| {
            if let Some(r) = map.iter_mut().find(|r| r.pitch == pitch) {
                r.name = name;
            }
        });
    });
}

pub(crate) fn install_header_input(ctx: &Ctx, src: Rc<dyn RowSource>, h: Rc<Header>, header: &Canvas, grid: &Canvas) {
    let row_at = {
        let (ctx, src) = (ctx.clone(), src.clone());
        move |y: f64| -> Option<(usize, DrumRow, usize)> {
            let rows = src.rows(&ctx)?;
            let i = (y / ROW_H as f64).floor();
            let row = rows.get(i as usize).filter(|_| i >= 0.0)?.clone();
            Some((i as usize, row, rows.len()))
        }
    };
    let click = gtk::GestureClick::new();
    click.set_button(0);
    {
        let (ctx, src, header, grid, row_at) = (ctx.clone(), src.clone(), header.clone(), grid.clone(), row_at.clone());
        click.connect_pressed(move |g, n, x, y| {
            let Some((i, row, _)) = row_at(y) else { return };
            // A press on the name may start dragging the row to a new place.
            if zone(x) != Zone::Name || g.current_button() == gdk::BUTTON_SECONDARY {
                g.set_state(gtk::EventSequenceState::Claimed);
            }
            src.set_focus_row(i);
            grid.grab_focus();
            let used = src.used(&ctx);
            if g.current_button() == gdk::BUTTON_SECONDARY {
                row_menu(&ctx, &*src, &header, x, y, &row);
                return;
            }
            match zone(x) {
                Zone::Name if n == 2 => rename(&ctx, &header, x, y, &row, used),
                Zone::Name => audition(&ctx, row.pitch, 110),
                Zone::Badge => src.badge_clicked(&ctx, &header, x, y, &row),
                Zone::Mute => toggle(&ctx, if row.mute { "Unmute Drum Row" } else { "Mute Drum Row" }, used, row.pitch, |r| r.mute = !r.mute),
                Zone::Solo => toggle(&ctx, if row.solo { "Unsolo Drum Row" } else { "Solo Drum Row" }, used, row.pitch, |r| r.solo = !r.solo),
                Zone::Menu => row_menu(&ctx, &*src, &header, x, y, &row),
            }
            header.queue_draw();
            grid.queue_draw();
        });
    }
    header.add_controller(click);

    let drag = gtk::GestureDrag::new();
    {
        let (h, row_at) = (h.clone(), row_at.clone());
        drag.connect_drag_begin(move |_, x, y| {
            h.moving.set(row_at(y).filter(|_| zone(x) == Zone::Name).map(|(i, _, _)| (i, i)));
        });
    }
    {
        let (ctx, src, h, header) = (ctx.clone(), src.clone(), h.clone(), header.clone());
        drag.connect_drag_update(move |g, _, dy| {
            let (Some((from, _)), Some((_, y))) = (h.moving.get(), g.start_point()) else { return };
            let rows = src.rows(&ctx).map_or(0, |r| r.len());
            let to = ((y + dy) / ROW_H as f64).floor().clamp(0.0, rows.saturating_sub(1) as f64) as usize;
            h.moving.set(Some((from, to)));
            header.queue_draw();
        });
    }
    {
        let (ctx, src, h, header) = (ctx.clone(), src.clone(), h.clone(), header.clone());
        drag.connect_drag_end(move |_, _, _| {
            let Some((from, to)) = h.moving.take() else { return };
            header.queue_draw();
            let Some(rows) = src.rows(&ctx) else { return };
            if from == to {
                return;
            }
            let (Some(a), Some(b)) = (rows.get(from), rows.get(to)) else { return };
            let (pitch, target) = (a.pitch, b.pitch);
            edit_map(&ctx, "Move Drum Row", src.used(&ctx), |map| place_row(map, pitch, target));
        });
    }
    header.add_controller(drag);

    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    {
        let (ctx, row_at) = (ctx.clone(), row_at.clone());
        drop.connect_drop(move |_, value, _, y| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            let Some((_, row, _)) = row_at(y) else { return false };
            if pad_of(row.pitch).is_none() || kit(&ctx).is_none_or(|k| k.2) {
                return false;
            }
            load_sample(&ctx, row.pitch, list.files().iter().filter_map(|f| f.path()).collect());
            true
        });
    }
    header.add_controller(drop);
    header.set_has_tooltip(true);
    header.connect_query_tooltip(move |_, x, y, _, tip| {
        if row_at(y as f64).is_none() {
            return false;
        }
        tip.set_text(Some(match zone(x as f64) {
            Zone::Name => "Click to hear this row, double-click to rename it, or drag it to a new place",
            Zone::Badge => src.badge_tip(),
            Zone::Mute => "Mute this row wherever it plays on the track",
            Zone::Solo => "Solo this row wherever it plays on the track",
            Zone::Menu => "Row actions",
        }));
        true
    });
}

fn row_menu(ctx: &Ctx, src: &dyn RowSource, header: &Canvas, x: f64, y: f64, row: &DrumRow) {
    let Some(rows) = src.rows(ctx) else { return };
    let m = Menu::at(header, x, y);
    let pitch = row.pitch;
    src.extend_menu(ctx, &m, header, x, y, row);
    m.separator();
    let used = src.used(ctx);
    {
        let (ctx, used, anchor, row) = (ctx.clone(), used.clone(), header.clone(), row.clone());
        m.item("Rename…", true, move || rename(&ctx, &anchor, x, y, &row, used.clone()));
    }
    let index = rows.iter().position(|r| r.pitch == pitch).unwrap_or(0);
    for (label, enabled, up) in [("Move Up", index > 0, true), ("Move Down", index + 1 < rows.len(), false)] {
        let (ctx, used) = (ctx.clone(), used.clone());
        m.item(label, enabled, move || edit_map(&ctx, label, used.clone(), |map| move_row(map, pitch, up)));
    }
    if pad_of(pitch).is_some()
        && let Some((_, _, frozen)) = kit(ctx)
    {
        m.separator();
        let (c, anchor) = (ctx.clone(), header.clone());
        m.item("Load Sample…", !frozen, move || {
            let c = c.clone();
            crate::sample_editor::pick_files(&anchor, "Audio Files", &crate::sample_editor::AUDIO, false, move |files| load_sample(&c, pitch, files));
        });
        let (c, anchor) = (ctx.clone(), header.clone());
        m.item("Show in Drum Kit", true, move || show_pad(&c, pitch, &anchor));
        m.separator();
    }
    {
        let ctx = ctx.clone();
        m.item("Hide Row", true, move || toggle(&ctx, "Hide Drum Row", used.clone(), pitch, |r| r.hidden = true));
    }
    m.show();
}
