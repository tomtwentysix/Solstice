//! Logic's Event List for a MIDI region: one row per note, with its values editable.

use crate::editor::{edit_midi, midi};
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use solstice_model::*;
use std::rc::Rc;

pub fn build(ctx: &Ctx, to_roll: Rc<dyn Fn()>) -> gtk::Widget {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(6);
    bar.set_margin_end(6);
    bar.set_margin_top(4);
    bar.set_margin_bottom(4);
    let title = gtk::Label::new(Some("Event List"));
    title.add_css_class("heading");
    bar.append(&title);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    let back = gtk::Button::with_label("Piano Roll");
    back.connect_clicked(move |_| to_roll());
    bar.append(&back);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("event-list");
    let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build();
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&bar);
    col.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    col.append(&header());
    col.append(&scroller);

    let refresh = {
        let (ctx, list) = (ctx.clone(), list.clone());
        move || fill(&ctx, &list)
    };
    refresh();
    ctx.on(move |c| {
        if matches!(c, Change::Project | Change::Selection) {
            refresh();
        }
    });
    col.upcast()
}

const WIDTHS: [i32; 4] = [130, 70, 80, 110];

fn header() -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_start(12);
    row.set_margin_top(4);
    row.set_margin_bottom(4);
    for (name, w) in ["Position", "Note", "Velocity", "Length"].iter().zip(WIDTHS) {
        let l = gtk::Label::new(Some(name));
        l.set_xalign(0.0);
        l.set_size_request(w, -1);
        l.add_css_class("dim-label");
        l.add_css_class("caption-heading");
        row.append(&l);
    }
    row.upcast()
}

fn fill(ctx: &Ctx, list: &gtk::ListBox) {
    list.remove_all();
    let Some((r, m, _)) = midi(ctx) else { return };
    let time = ctx.s().project.time();
    for n in &m.notes {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.set_margin_start(6);
        let (bar, beat, div, tick) = time.position(time.at(r.start, n.start as u64));
        let pos = gtk::Label::new(Some(&format!("{bar} {beat} {div} {tick:>3}")));
        pos.add_css_class("numeric");
        let pitch = gtk::SpinButton::with_range(0.0, 127.0, 1.0);
        pitch.set_value(n.pitch as f64);
        pitch.connect_output(|s| {
            s.set_text(&crate::sample_editor::note_name(s.value() as u8));
            gtk::glib::Propagation::Stop
        });
        let vel = gtk::SpinButton::with_range(1.0, 127.0, 1.0);
        vel.set_value(n.velocity as f64);
        let beats = n.length as f64 / time.ticks_per_beat() as f64;
        let len = gtk::Label::new(Some(&format!("{beats:.2} beats")));
        len.add_css_class("numeric");
        for (w, width) in [pos.upcast_ref::<gtk::Widget>(), pitch.upcast_ref(), vel.upcast_ref(), len.upcast_ref()].into_iter().zip(WIDTHS) {
            w.set_size_request(width, -1);
            if let Some(l) = w.downcast_ref::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            row.append(w);
        }
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.add_css_class("flat");
        delete.set_tooltip_text(Some("Delete Note"));
        row.append(&delete);

        let note = *n;
        let set = {
            let ctx = ctx.clone();
            move |label: &'static str, f: Box<dyn Fn(&mut Note)>| {
                let ctx = ctx.clone();
                edit_midi(&ctx, label, move |m| {
                    if let Some(x) = m.notes.iter_mut().find(|x| **x == note) {
                        f(x);
                    }
                });
            }
        };
        {
            let set = set.clone();
            pitch.connect_value_changed(move |s| {
                let v = s.value() as u8;
                if v != note.pitch {
                    set("Change Pitch", Box::new(move |x| x.pitch = v));
                }
            });
        }
        vel.connect_value_changed(move |s| {
            let v = s.value() as u8;
            if v != note.velocity {
                set("Change Velocity", Box::new(move |x| x.velocity = v));
            }
        });
        {
            let ctx = ctx.clone();
            delete.connect_clicked(move |_| edit_midi(&ctx, "Delete Note", |m| m.notes.retain(|x| *x != note)));
        }
        list.append(&row);
    }
}
