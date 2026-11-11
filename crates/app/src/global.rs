//! Logic's global tracks above the tracks area (G): signature and tempo.

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
    Signature,
    Tempo,
}

impl Lane {
    pub const ALL: [Lane; 2] = [Lane::Signature, Lane::Tempo];

    pub fn height(self) -> f32 {
        match self {
            Lane::Signature => 22.0,
            Lane::Tempo => 56.0,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Lane::Signature => "Signature",
            Lane::Tempo => "Tempo",
        }
    }
}

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

    // Signature: the opening one and each change, as Logic labels them.
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
            match lane {
                Lane::Signature => signature_popover(&ctx, &view, &lanes, x),
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
            if let Drag::Tempo { .. } = drag.replace(Drag::None) {
                ctx.live(true, |p| {
                    // Two changes dragged onto one beat: keep the one just moved.
                    p.tempo_changes.sort_by_key(|c| c.tick);
                    p.tempo_changes.dedup_by_key(|c| c.tick);
                });
                ctx.end_gesture("Change Tempo");
            }
        });
    }
    g.set_button(0);
    lanes.add_controller(g);
}

/// Sets or removes the signature from the bar nearest `x`.
fn signature_popover(ctx: &Ctx, view: &View, lanes: &Canvas, x: f64) {
    let (bar, num, den, existing) = {
        let s = ctx.s();
        let p = &s.project;
        let time = p.time();
        let pos = view.pos(x);
        let bar = time.bar_at(pos + time.bar_len() / 2).min(time.bar_at(pos) + 1);
        let bar = if (view.x(time.bar_start(bar)) as f64 - x).abs() < 40.0 { bar } else { time.bar_at(pos) };
        let (num, den) = time.sig_at(time.bar_start(bar));
        (bar, num, den, p.sig_changes.iter().any(|c| c.bar == bar))
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
    let remove = gtk::Button::with_label("Remove");
    remove.add_css_class("destructive-action");
    remove.set_visible(existing);
    grid.attach(&remove, 0, 2, 3, 1);
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
        let remove = removed.get();
        if remove || num2 != num || den2 != den {
            ctx.edit(if remove { "Remove Signature Change" } else { "Change Signature" }, |p| {
                let old = p.time();
                p.sig_changes.retain(|c| c.bar != bar);
                if bar == 0 {
                    p.sig_num = num2;
                    p.sig_den = den2;
                } else if !remove {
                    p.sig_changes.push(SigChange { bar, num: num2, den: den2 });
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
