//! Comping a take folder, as in Logic: each take on its own lane, the parts that play lit up.
//! Drag across a take to play it there; click a take's name area to play all of it.

use crate::canvas::{self, Canvas, rect, rgba, text};
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

const LANE_H: f32 = 44.0;
const NAME_W: f32 = 90.0;

fn folder(ctx: &Ctx) -> Option<(Id, u64, AudioContent)> {
    let s = ctx.s();
    let id = s.selected_region?;
    let (_, r) = s.project.find_region(id)?;
    match &r.content {
        Content::Audio(a) if a.is_folder() => Some((id, r.length, a.clone())),
        _ => None,
    }
}

pub fn build(ctx: &Ctx) -> gtk::Widget {
    let swipe: Rc<Cell<Option<(usize, f64, f64)>>> = Rc::default();
    let canvas = {
        let (ctx, swipe) = (ctx.clone(), swipe.clone());
        Canvas::new(move |c, s, w, h| draw(&ctx, swipe.get(), c, s, w, h))
    };
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);

    let frame_at = |w: f64, length: u64, x: f64| ((x - NAME_W as f64).max(0.0) / (w - NAME_W as f64).max(1.0) * length as f64) as u64;
    let g = gtk::GestureDrag::new();
    {
        let swipe = swipe.clone();
        g.connect_drag_begin(move |_, x, y| swipe.set(Some(((y / LANE_H as f64) as usize, x, x))));
    }
    {
        let (swipe, canvas) = (swipe.clone(), canvas.clone());
        g.connect_drag_update(move |g, dx, _| {
            if let (Some((lane, x0, _)), Some((sx, _))) = (swipe.get(), g.start_point()) {
                swipe.set(Some((lane, x0, sx + dx)));
                canvas.queue_draw();
            }
        });
    }
    {
        let (ctx, swipe, canvas) = (ctx.clone(), swipe.clone(), canvas.clone());
        g.connect_drag_end(move |_, _, _| {
            let Some((lane, a, b)) = swipe.take() else { return };
            let Some((id, length, content)) = folder(&ctx) else { return };
            if lane >= content.takes.len() {
                return;
            }
            let w = canvas.width() as f64;
            let (from, to) = if a < NAME_W as f64 || (b - a).abs() < 3.0 {
                (0, length)
            } else {
                (frame_at(w, length, a.min(b)), frame_at(w, length, a.max(b)))
            };
            ctx.edit("Comp", |p| {
                if let Some(Region { content: Content::Audio(a), .. }) = p.region_mut(id) {
                    a.set_comp(from, to, lane, length);
                }
            });
        });
    }
    canvas.add_controller(g);
    let fit = {
        let (ctx, canvas) = (ctx.clone(), canvas.clone());
        move || {
            let lanes = folder(&ctx).map_or(0, |f| f.2.takes.len());
            canvas.set_size_request(-1, (LANE_H * lanes as f32) as i32);
            canvas.queue_draw();
        }
    };
    fit();
    ctx.on(move |c| {
        if !matches!(c, Change::LiveNote) {
            fit();
        }
    });

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    let title = gtk::Label::new(Some("Take Folder"));
    title.add_css_class("heading");
    bar.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    let flatten = gtk::Button::with_label("Flatten");
    flatten.set_tooltip_text(Some("Keep only the comp, dropping the unused takes"));
    {
        let ctx = ctx.clone();
        flatten.connect_clicked(move |_| flatten_folder(&ctx));
    }
    bar.append(&flatten);

    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&bar);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let scroller = gtk::ScrolledWindow::builder().child(&canvas).vexpand(true).build();
    col.append(&scroller);
    col.upcast()
}

fn draw(ctx: &Ctx, swipe: Option<(usize, f64, f64)>, c: &Canvas, s: &gtk::Snapshot, w: f32, _h: f32) {
    let Some((_, length, a)) = folder(ctx) else { return };
    let fg = canvas::fg(c);
    let accent = canvas::accent(c);
    let span = (w - NAME_W).max(1.0);
    let spp = length as f64 / span as f64;
    let x_of = |frame: u64| NAME_W + (frame as f64 / spp) as f32;
    let pieces = a.pieces(length);
    let comp_take = |frame: u64| a.comp.iter().rev().find(|c| c.start <= frame).map_or(0, |c| c.take);
    for (i, take) in a.takes.iter().enumerate() {
        let y = i as f32 * LANE_H;
        rect(s, 0.0, y + LANE_H - 1.0, w, 1.0, &gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.12));
        let in_use = pieces.iter().any(|p| p.file == take.file);
        let name_col = if in_use { fg } else { gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.6) };
        text(s, c, 8.0, y + LANE_H / 2.0 - 7.0, &take.name, &name_col, Some(9.0));
        // Lit where this take plays.
        for sec in a.comp.iter().enumerate().filter(|(_, sec)| sec.take == i) {
            let end = a.comp.get(sec.0 + 1).map_or(length, |n| n.start);
            let (x0, x1) = (x_of(sec.1.start), x_of(end));
            rect(s, x0, y + 2.0, x1 - x0, LANE_H - 4.0, &gtk::gdk::RGBA::new(accent.red(), accent.green(), accent.blue(), 0.25));
        }
        if let Some(peaks) = ctx.m().peaks_for(&take.file) {
            let cols = peaks.columns(take.offset as f64, spp, span as usize);
            let ink = if in_use { fg } else { gtk::gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), 0.45) };
            let data: Vec<(f32, f32)> = cols
                .into_iter()
                .enumerate()
                .map(|(k, v)| if comp_take((k as f64 * spp) as u64) == i { v } else { (v.0 * 0.6, v.1 * 0.6) })
                .collect();
            canvas::waveform(s, NAME_W, y + 4.0, LANE_H - 8.0, &data, &ink);
        }
    }
    if let Some((lane, a0, b0)) = swipe
        && lane < a.takes.len()
        && a0 >= NAME_W as f64
    {
        let (x0, x1) = (a0.min(b0) as f32, a0.max(b0) as f32);
        rect(s, x0, lane as f32 * LANE_H, x1 - x0, LANE_H, &rgba(0xffffff, 0.15));
    }
    let pos = ctx.position();
    if let Some((_, r)) = ctx.s().selected_region.and_then(|id| ctx.s().project.find_region(id).map(|(t, r)| (t, r.clone())))
        && pos >= r.start
        && pos < r.end()
    {
        rect(s, x_of(pos - r.start), 0.0, 1.0, LANE_H * a.takes.len() as f32, &fg);
    }
}

/// Logic's Flatten: the folder becomes regions playing just the comp.
fn flatten_folder(ctx: &Ctx) {
    let Some((id, length, a)) = folder(ctx) else { return };
    ctx.edit("Flatten Take Folder", |p| {
        let Some(t) = p.tracks.iter().position(|t| t.regions.iter().any(|r| r.id == id)) else { return };
        let Some(i) = p.tracks[t].regions.iter().position(|r| r.id == id) else { return };
        let r = p.tracks[t].regions.remove(i);
        let pieces: Vec<(u64, u64, String, u64)> = a.pieces(length).iter().map(|p| (p.start, p.end, p.file.to_string(), p.offset)).collect();
        let n = pieces.len();
        for (k, (start, end, file, offset)) in pieces.into_iter().enumerate() {
            let rid = p.alloc_id();
            let content = AudioContent {
                file,
                offset,
                gain_db: a.gain_db,
                fade_in: if k == 0 { a.fade_in } else { 0 },
                fade_out: if k + 1 == n { a.fade_out } else { 0 },
                takes: Vec::new(),
                comp: Vec::new(),
            };
            p.tracks[t].regions.push(Region {
                id: rid,
                name: r.name.clone(),
                start: r.start + start,
                length: end - start,
                muted: r.muted,
                loops: 0,
                content: Content::Audio(content),
            });
        }
    });
}
