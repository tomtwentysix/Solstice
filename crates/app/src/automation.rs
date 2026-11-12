use crate::canvas::{self, rect, rgba, rounded, text};
use crate::gestures::{db_to_pos, pos_to_db};
use crate::studio::{Ctx, Studio};
use gtk::prelude::*;
use gtk::gdk;
use solstice_model::*;

/// What the lane shown on `track` controls.
pub fn shown(st: &Studio, track: Id) -> AutoTarget {
    st.auto_view.get(&track).copied().unwrap_or(AutoTarget::Volume)
}

/// 0..1 height of a value in its lane; volume and sends use the fader's curve.
pub fn norm(t: AutoTarget, v: f32) -> f32 {
    match t {
        AutoTarget::Volume | AutoTarget::Send(_) => db_to_pos(v),
        _ => {
            let (a, b) = t.range();
            ((v - a) / (b - a)).clamp(0.0, 1.0)
        }
    }
}

pub fn value(t: AutoTarget, n: f32) -> f32 {
    let n = n.clamp(0.0, 1.0);
    match t {
        AutoTarget::Volume | AutoTarget::Send(_) => pos_to_db(n).max(-60.0),
        AutoTarget::Mute => {
            if n > 0.5 { 1.0 } else { 0.0 }
        }
        _ => {
            let (a, b) = t.range();
            a + n * (b - a)
        }
    }
}

pub fn label(t: AutoTarget, v: f32) -> String {
    match t {
        AutoTarget::Volume | AutoTarget::Send(_) => crate::gestures::fmt_db(v),
        AutoTarget::Pan => crate::gestures::fmt_pan(v),
        AutoTarget::Mute => (if v > 0.5 { "Muted" } else { "On" }).into(),
        AutoTarget::Plugin { .. } => format!("{:.0}%", v * 100.0),
    }
}

/// The control's current value when it has no automation yet.
pub fn current(st: &Studio, track: Id, t: AutoTarget) -> f32 {
    let Some(ch) = st.project.channel(Some(track)) else { return 0.0 };
    match t {
        AutoTarget::Volume => ch.volume_db,
        AutoTarget::Pan => ch.pan,
        AutoTarget::Mute => ch.mute as u8 as f32,
        AutoTarget::Send(i) => ch.sends.get(i).map_or(0.0, |s| s.level_db),
        AutoTarget::Plugin { slot, param } => st.plugins.get(&slot).map_or(0.0, |l| l.inst.param(param) as f32),
    }
}

/// Every parameter a track can automate, with display names.
pub fn targets(st: &Studio, track: Id) -> Vec<(AutoTarget, String)> {
    let mut v = vec![
        (AutoTarget::Volume, "Volume".to_string()),
        (AutoTarget::Pan, "Pan".to_string()),
        (AutoTarget::Mute, "Mute".to_string()),
    ];
    let Some(t) = st.project.track(track) else { return v };
    for (i, s) in t.channel.sends.iter().enumerate() {
        let bus = st.project.buses.iter().find(|b| b.id == s.bus).map_or("Bus".into(), |b| b.name.clone());
        v.push((AutoTarget::Send(i), format!("Send to {bus}")));
    }
    let slots = t.instrument.iter().chain(t.channel.inserts.iter());
    for slot in slots {
        let Some(l) = st.plugins.get(&slot.id) else { continue };
        for p in l.inst.params().into_iter().filter(|p| !p.hidden && !p.read_only).take(64) {
            v.push((AutoTarget::Plugin { slot: slot.id, param: p.id }, format!("{}: {}", slot.name, p.title)));
        }
    }
    v
}

/// The y of value `n` (0..1) on a lane whose row starts at `top`.
pub fn lane_y(top: f32, n: f32, track_h: f32) -> f32 {
    top + 6.0 + (1.0 - n) * (track_h - 12.0)
}

/// Draws each track's shown lane over its row: the curve, its points and, for a point being
/// dragged, where it will land.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    st: &Studio,
    c: &impl IsA<gtk::Widget>,
    s: &gtk::Snapshot,
    w: f32,
    track_h: f32,
    x_of: &dyn Fn(u64) -> f32,
    top_of: &dyn Fn(usize) -> Option<f32>,
    dragging: Option<(usize, usize, u64, f32)>,
) {
    let accent = canvas::accent(c);
    for (row, t) in st.project.tracks.iter().enumerate() {
        // Tracks folded away in a stack have no row.
        let Some(y0) = top_of(row) else { continue };
        rect(s, 0.0, y0, w, track_h, &rgba(0x000000, 0.28));
        let target = shown(st, t.id);
        let mut points: Vec<AutoPoint> = t.channel.lane(target).map(|l| l.points.clone()).unwrap_or_default();
        if let Some((r, i, pos, v)) = dragging
            && r == row
            && let Some(p) = points.get_mut(i)
        {
            *p = AutoPoint { pos, value: v };
            points.sort_by_key(|p| p.pos);
        }
        let name = targets(st, t.id).into_iter().find(|(x, _)| *x == target).map(|(_, n)| n).unwrap_or_default();
        text(s, c, 6.0, y0 + 2.0, &name, &gdk::RGBA::new(1.0, 1.0, 1.0, 0.6), Some(7.0));
        let col = if points.is_empty() { rgba(0xffffff, 0.3) } else { accent };
        if points.is_empty() {
            let y = lane_y(y0, norm(target, current(st, t.id, target)), track_h);
            rect(s, 0.0, y, w, 1.0, &col);
            continue;
        }
        let ys: Vec<(f32, f32)> = points.iter().map(|p| (x_of(p.pos), lane_y(y0, norm(target, p.value), track_h))).collect();
        let b = gtk::gsk::PathBuilder::new();
        b.move_to(0.0, ys[0].1);
        for (i, (x, y)) in ys.iter().enumerate() {
            if target == AutoTarget::Mute && i > 0 {
                b.line_to(*x, ys[i - 1].1);
            }
            b.line_to(*x, *y);
        }
        b.line_to(w, ys.last().unwrap().1);
        s.append_stroke(&b.to_path(), &gtk::gsk::Stroke::new(1.5), &col);
        for (x, y) in &ys {
            rounded(s, x - 3.0, y - 3.0, 6.0, 6.0, 3.0, &col);
        }
        // The value of a point being dragged, as Logic shows beside the pointer.
        if let Some((r, _, pos, v)) = dragging
            && r == row
        {
            let (x, y) = (x_of(pos), lane_y(y0, norm(target, v), track_h));
            text(s, c, x + 6.0, y - 14.0, &label(target, v), &gdk::RGBA::new(1.0, 1.0, 1.0, 0.95), Some(8.0));
        }
    }
}

/// The point nearest (x, y) on the lane under it, within a few pixels.
pub fn nearest_point(st: &Studio, row: usize, top: f32, x: f64, y: f64, track_h: f32, x_of: &dyn Fn(u64) -> f32) -> Option<usize> {
    let t = st.project.tracks.get(row)?;
    let target = shown(st, t.id);
    let lane = t.channel.lane(target)?;
    lane.points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (px, py) = (x_of(p.pos) as f64, lane_y(top, norm(target, p.value), track_h) as f64);
            (i, ((px - x).powi(2) + (py - y).powi(2)).sqrt())
        })
        .filter(|(_, d)| *d < 8.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// Header controls in automation view: which parameter the lane shows, and the mode.
pub fn header_controls(ctx: &Ctx, track: Id) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_valign(gtk::Align::Center);
    let (list, current, mode) = {
        let s = ctx.s();
        let list = targets(&s, track);
        let cur = shown(&s, track);
        let mode = s.project.track(track).map_or(AutoMode::Read, |t| t.channel.auto_mode);
        (list, cur, mode)
    };
    let names: Vec<&str> = list.iter().map(|(_, n)| n.as_str()).collect();
    let param = gtk::DropDown::from_strings(&names);
    param.add_css_class("auto-param");
    param.set_selected(list.iter().position(|(t, _)| *t == current).unwrap_or(0) as u32);
    {
        let ctx = ctx.clone();
        let targets: Vec<AutoTarget> = list.iter().map(|(t, _)| *t).collect();
        param.connect_selected_notify(move |d| {
            ctx.m().auto_view.insert(track, targets[d.selected() as usize]);
            ctx.emit(crate::studio::Change::View);
        });
    }
    col.append(&param);
    col.append(&mode_dropdown(ctx, Some(track), mode));
    col.upcast()
}

pub fn mode_dropdown(ctx: &Ctx, id: Option<Id>, mode: AutoMode) -> gtk::DropDown {
    let names: Vec<&str> = AutoMode::ALL.iter().map(|m| m.label()).collect();
    let dd = gtk::DropDown::from_strings(&names);
    dd.add_css_class("auto-mode");
    dd.set_tooltip_text(Some("Automation mode"));
    dd.set_selected(AutoMode::ALL.iter().position(|m| *m == mode).unwrap_or(1) as u32);
    let ctx = ctx.clone();
    dd.connect_selected_notify(move |d| {
        let m = AutoMode::ALL[d.selected() as usize];
        if ctx.s().project.channel(id).is_some_and(|c| c.auto_mode != m) {
            ctx.tweak(|p| {
                if let Some(c) = p.channel_mut(id) {
                    c.auto_mode = m;
                }
            });
        }
    });
    dd
}
