//! Interfaces for Solstice's own plug-ins: knobs, switches and choices grouped by purpose, with
//! a picture of what each plug-in is doing.

mod distortion;
mod dynamics;
mod instruments;
mod midi;
mod time;

use crate::canvas::{self, Canvas, rounded, text};
use gtk::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use solstice_dsp::{Bank, Native, ParamDef};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// The interface for a built-in plug-in and the window size that suits it.
pub fn page(n: &Arc<Native>) -> (gtk::Widget, (i32, i32)) {
    let b = &n.bank;
    let (content, size) = match n.spec.id {
        "solstice:compressor" => (dynamics::compressor(b), (640, 520)),
        "solstice:limiter" => (dynamics::limiter(b), (560, 460)),
        "solstice:gate" => (dynamics::gate(b), (640, 520)),
        "solstice:de-esser" => (dynamics::de_esser(b), (640, 520)),
        "solstice:delay" => (time::delay(b), (720, 540)),
        "solstice:reverb" => (time::reverb(b), (640, 520)),
        "solstice:saturation" => (distortion::saturation(b), (640, 560)),
        "solstice:synth" => (instruments::synth(b), (900, 760)),
        "solstice:sampler" | "solstice:drums" => (instruments::sampler(b), (620, 560)),
        "solstice:arpeggiator" => (midi::arpeggiator(b), (680, 500)),
        "solstice:chord-trigger" => (midi::chord_trigger(b), (620, 440)),
        "solstice:note-repeater" => (midi::note_repeater(b), (640, 480)),
        "solstice:scale-quantize" => (midi::scale_quantize(b), (680, 420)),
        "solstice:transposer" => (midi::transposer(b), (600, 400)),
        "solstice:velocity" => (midi::velocity(b), (600, 480)),
        _ => return (gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(), (560, 520)),
    };
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let scroller = gtk::ScrolledWindow::builder().child(&content).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    (scroller.upcast(), size)
}

fn def(bank: &Bank, id: u32) -> ParamDef {
    *bank.defs.iter().find(|d| d.id == id).expect("parameter")
}

fn alpha(c: &gdk::RGBA, a: f32) -> gdk::RGBA {
    gdk::RGBA::new(c.red(), c.green(), c.blue(), c.alpha() * a)
}

/// A rotary control. Drag up or down (Shift for fine), scroll, or double-click to reset.
fn knob(bank: &Arc<Bank>, id: u32) -> gtk::Widget {
    let d = def(bank, id);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 2);
    col.set_halign(gtk::Align::Center);
    let name = gtk::Label::new(Some(d.name));
    name.add_css_class("caption");
    name.add_css_class("dim-label");
    let dial = {
        let bank = bank.clone();
        Canvas::new(move |c, s, w, h| {
            let n = bank.norm(id);
            let r = w.min(h) / 2.0 - 4.0;
            let (cx, cy) = (w / 2.0, h / 2.0);
            let a0 = std::f32::consts::PI * 0.75;
            let sweep = std::f32::consts::PI * 1.5;
            let arc = |from: f32, to: f32| {
                let b = gsk::PathBuilder::new();
                for k in 0..=32 {
                    let a = from + (to - from) * k as f32 / 32.0;
                    let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                    if k == 0 { b.move_to(x, y) } else { b.line_to(x, y) }
                }
                b.to_path()
            };
            let fg = canvas::fg(c);
            let body = gsk::PathBuilder::new();
            body.add_circle(&graphene::Point::new(cx, cy), r - 6.0);
            s.append_fill(&body.to_path(), gsk::FillRule::Winding, &alpha(&fg, 0.08));
            let stroke = gsk::Stroke::new(4.0);
            stroke.set_line_cap(gsk::LineCap::Round);
            s.append_stroke(&arc(a0, a0 + sweep), &stroke, &alpha(&fg, 0.15));
            // Ranges that straddle zero fill from the middle, as pan and transpose do.
            let origin = if d.min < 0.0 && d.max > 0.0 { d.to_norm(0.0) } else { 0.0 };
            let (f0, f1) = if n < origin { (n, origin) } else { (origin, n) };
            if f1 > f0 + 0.001 {
                s.append_stroke(&arc(a0 + sweep * f0, a0 + sweep * f1), &stroke, &canvas::accent(c));
            }
            let a = a0 + sweep * n;
            let tick = gsk::PathBuilder::new();
            tick.move_to(cx + (r - 14.0).max(2.0) * a.cos(), cy + (r - 14.0).max(2.0) * a.sin());
            tick.line_to(cx + (r - 6.0) * a.cos(), cy + (r - 6.0) * a.sin());
            let ts = gsk::Stroke::new(2.0);
            ts.set_line_cap(gsk::LineCap::Round);
            s.append_stroke(&tick.to_path(), &ts, &fg);
        })
    };
    dial.set_size_request(54, 54);
    dial.set_tooltip_text(Some(d.name));
    let value = gtk::Label::new(Some(&bank.text(id, bank.get(id))));
    value.add_css_class("caption");
    value.add_css_class("numeric");
    value.set_width_chars(8);

    let start = Rc::new(Cell::new(0f32));
    let drag = gtk::GestureDrag::new();
    {
        let (bank, start) = (bank.clone(), start.clone());
        drag.connect_drag_begin(move |_, _, _| start.set(bank.norm(id)));
    }
    {
        let bank = bank.clone();
        drag.connect_drag_update(move |g, _, dy| {
            let fine = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
            let travel = if fine { 1500.0 } else { 180.0 };
            bank.set_norm(id, (start.get() - dy as f32 / travel).clamp(0.0, 1.0));
        });
    }
    dial.add_controller(drag);
    let reset = gtk::GestureClick::new();
    {
        let bank = bank.clone();
        reset.connect_pressed(move |g, n, _, _| {
            if n == 2 || g.current_event_state().contains(gdk::ModifierType::ALT_MASK) {
                bank.set(id, d.default);
            }
        });
    }
    dial.add_controller(reset);
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let bank = bank.clone();
        scroll.connect_scroll(move |_, _, dy| {
            let step = if d.steps > 0 { 1.0 / d.steps as f32 } else { 0.01 };
            bank.set_norm(id, (bank.norm(id) - dy as f32 * step).clamp(0.0, 1.0));
            glib::Propagation::Stop
        });
    }
    dial.add_controller(scroll);

    let last = Cell::new(f32::NAN);
    {
        let (bank, value) = (bank.clone(), value.clone());
        dial.add_tick_callback(move |c, _| {
            let v = bank.get(id);
            if v.to_bits() != last.get().to_bits() {
                last.set(v);
                value.set_text(&bank.text(id, v));
                c.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }
    col.append(&name);
    col.append(&dial);
    col.append(&value);
    col.upcast()
}

/// A stepped parameter with named values: linked buttons when they fit, a menu otherwise.
fn choice(bank: &Arc<Bank>, id: u32) -> gtk::Widget {
    let d = def(bank, id);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 4);
    col.set_valign(gtk::Align::Center);
    let name = gtk::Label::new(Some(d.name));
    name.add_css_class("caption");
    name.add_css_class("dim-label");
    col.append(&name);
    let index = {
        let bank = bank.clone();
        move || (bank.get(id) - d.min).round().max(0.0) as u32
    };
    let set = {
        let bank = bank.clone();
        move |i: u32| bank.set(id, d.min + i as f32)
    };
    if d.choices.len() <= 5 || d.choices.iter().all(|c| c.chars().count() <= 3) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.add_css_class("linked");
        let mut buttons: Vec<gtk::ToggleButton> = Vec::new();
        for (i, c) in d.choices.iter().enumerate() {
            let b = gtk::ToggleButton::with_label(c);
            b.set_group(buttons.first());
            b.set_active(index() == i as u32);
            let set = set.clone();
            b.connect_toggled(move |b| {
                if b.is_active() {
                    set(i as u32);
                }
            });
            row.append(&b);
            buttons.push(b);
        }
        row.add_tick_callback(move |_, _| {
            if let Some(b) = buttons.get(index() as usize)
                && !b.is_active()
            {
                b.set_active(true);
            }
            glib::ControlFlow::Continue
        });
        col.append(&row);
    } else {
        let dd = gtk::DropDown::from_strings(d.choices);
        dd.set_selected(index());
        dd.connect_selected_notify(move |dd| set(dd.selected()));
        dd.add_tick_callback(move |dd, _| {
            if dd.selected() != index() {
                dd.set_selected(index());
            }
            glib::ControlFlow::Continue
        });
        col.append(&dd);
    }
    col.upcast()
}

/// An on/off parameter.
fn switch(bank: &Arc<Bank>, id: u32) -> gtk::Widget {
    let d = def(bank, id);
    let b = gtk::ToggleButton::with_label(d.name);
    b.set_valign(gtk::Align::Center);
    b.set_active(bank.get(id) > 0.5);
    {
        let bank = bank.clone();
        b.connect_toggled(move |b| bank.set(id, if b.is_active() { 1.0 } else { 0.0 }));
    }
    let bank = bank.clone();
    b.add_tick_callback(move |b, _| {
        let on = bank.get(id) > 0.5;
        if b.is_active() != on {
            b.set_active(on);
        }
        glib::ControlFlow::Continue
    });
    b.upcast()
}

/// The right control for a parameter.
fn control(bank: &Arc<Bank>, id: u32) -> gtk::Widget {
    let d = def(bank, id);
    if !d.choices.is_empty() {
        choice(bank, id)
    } else if d.steps == 1 && d.unit.is_empty() {
        switch(bank, id)
    } else {
        knob(bank, id)
    }
}

/// A titled group of controls, with an optional picture above them.
fn section(title: &str, picture: Option<&Canvas>, bank: &Arc<Bank>, ids: &[u32]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("plugin-section");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("heading");
    t.set_xalign(0.0);
    b.append(&t);
    if let Some(p) = picture {
        b.append(p);
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_halign(gtk::Align::Center);
    for id in ids {
        row.append(&control(bank, *id));
    }
    b.append(&row);
    b
}

/// Sections side by side, sharing the width.
fn columns(parts: &[gtk::Box]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_homogeneous(true);
    for p in parts {
        row.append(p);
    }
    row
}

fn stack(parts: &[&gtk::Widget]) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for p in parts {
        col.append(*p);
    }
    col
}

/// A live picture, redrawn every frame while shown.
fn picture(height: i32, draw: impl Fn(&Canvas, &gtk::Snapshot, f32, f32) + 'static) -> Canvas {
    let c = Canvas::new(move |c, s, w, h| {
        rounded(s, 0.0, 0.0, w, h, 8.0, &alpha(&canvas::fg(c), 0.05));
        draw(c, s, w, h);
    });
    c.set_size_request(-1, height);
    c.set_hexpand(true);
    c.add_tick_callback(|c, _| {
        c.queue_draw();
        glib::ControlFlow::Continue
    });
    c
}

fn line(points: &[(f32, f32)]) -> gsk::Path {
    let b = gsk::PathBuilder::new();
    for (i, (x, y)) in points.iter().enumerate() {
        if i == 0 { b.move_to(*x, *y) } else { b.line_to(*x, *y) }
    }
    b.to_path()
}

fn stroke(s: &gtk::Snapshot, points: &[(f32, f32)], width: f32, c: &gdk::RGBA) {
    if points.len() > 1 {
        let st = gsk::Stroke::new(width);
        st.set_line_join(gsk::LineJoin::Round);
        s.append_stroke(&line(points), &st, c);
    }
}

/// Fills between a curve and the horizontal line at `base`.
fn fill_under(s: &gtk::Snapshot, points: &[(f32, f32)], base: f32, c: &gdk::RGBA) {
    if points.len() < 2 {
        return;
    }
    let b = gsk::PathBuilder::new();
    b.move_to(points[0].0, base);
    for (x, y) in points {
        b.line_to(*x, *y);
    }
    b.line_to(points[points.len() - 1].0, base);
    b.close();
    s.append_fill(&b.to_path(), gsk::FillRule::Winding, c);
}

fn label(s: &gtk::Snapshot, c: &Canvas, x: f32, y: f32, t: &str, a: f32) {
    text(s, c, x, y, t, &alpha(&canvas::fg(c), a), Some(8.0));
}

/// Right-aligned at `x`.
fn label_right(s: &gtk::Snapshot, c: &Canvas, x: f32, y: f32, t: &str, a: f32) {
    let layout = c.create_pango_layout(Some(t));
    let mut fd = gtk::pango::FontDescription::new();
    fd.set_size(8 * gtk::pango::SCALE);
    layout.set_font_description(Some(&fd));
    label(s, c, x - layout.pixel_size().0 as f32, y, t, a);
}

/// A vertical meter for gain reduction or gate gain, 0 dB at the top.
fn reduction_meter(title: &'static str, bank: &Arc<Bank>, range_db: f32) -> Canvas {
    let bank = bank.clone();
    let c = picture(-1, move |c, s, w, h| {
        let (top, bottom) = (22.0, h - 8.0);
        let db = bank.meter().clamp(-range_db, 0.0);
        let y = top + (bottom - top) * (-db / range_db);
        canvas::rect(s, w / 2.0 - 6.0, top, 12.0, bottom - top, &alpha(&canvas::fg(c), 0.08));
        canvas::rect(s, w / 2.0 - 6.0, top, 12.0, y - top, &canvas::rgba(0xe5a50a, 0.9));
        label(s, c, 6.0, 4.0, title, 0.6);
        label_right(s, c, w - 4.0, bottom - 12.0, &format!("{db:.1}"), 0.7);
    });
    c.set_hexpand(false);
    c.set_size_request(46, -1);
    c
}
