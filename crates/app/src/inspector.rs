use crate::mixer;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::glib;
use solstice_model::*;
use std::cell::Cell;
use std::rc::Rc;

/// Logic's inspector: region parameters on top, then the selected track's strip beside the
/// strip it outputs to.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
    col.set_margin_start(8);
    col.set_margin_end(8);
    col.set_margin_top(8);
    col.set_margin_bottom(8);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&col).build();
    let busy: Busy = Rc::default();
    fill(ctx, &col, &busy);
    let (ctx2, col2) = (ctx.clone(), col.clone());
    ctx.on(move |c| {
        // The inspector's own edits leave it alone, so the control being used survives.
        if matches!(c, Change::Project | Change::Selection | Change::Catalog) && !busy.get() {
            fill(&ctx2, &col2, &busy);
        }
    });
    wheel_scrolls_panel(&scroller);
    scroller.upcast()
}

type Busy = Rc<Cell<bool>>;

fn quiet_edit(ctx: &Ctx, busy: &Busy, label: &str, f: impl FnOnce(&mut Project)) {
    busy.set(true);
    ctx.edit(label, f);
    busy.set(false);
}

/// The wheel scrolls the panel when the pointer is over a number field, rather than changing
/// a value the user only meant to scroll past.
fn wheel_scrolls_panel(scroller: &gtk::ScrolledWindow) {
    let at = Rc::new(Cell::new((0.0, 0.0)));
    let motion = gtk::EventControllerMotion::new();
    {
        let at = at.clone();
        motion.connect_motion(move |_, x, y| at.set((x, y)));
    }
    scroller.add_controller(motion);
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sc = scroller.clone();
    wheel.connect_scroll(move |_, _, dy| {
        let (x, y) = at.get();
        let over_number = sc.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|w| w.is::<gtk::SpinButton>() || w.ancestor(gtk::SpinButton::static_type()).is_some());
        if !over_number {
            return glib::Propagation::Proceed;
        }
        let adj = sc.vadjustment();
        adj.set_value(adj.value() + dy * adj.step_increment().max(40.0));
        glib::Propagation::Stop
    });
    scroller.add_controller(wheel);
}

fn fill(ctx: &Ctx, col: &gtk::Box, busy: &Busy) {
    while let Some(c) = col.first_child() {
        col.remove(&c);
    }
    col.append(&region_group(ctx, busy));

    let (track, out) = {
        let s = ctx.s();
        let t = s.selected_track.and_then(|id| s.project.track(id));
        let out = t.map(|t| match t.channel.output {
            Output::Master => None,
            Output::Bus(b) => Some(b),
        });
        (t.map(|t| t.id), out.flatten())
    };
    let strips = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    strips.set_homogeneous(true);
    strips.add_css_class("inspector-strips");
    let ids: Vec<Option<Id>> = track.map(Some).into_iter().chain([out]).collect();
    let layout = mixer::Layout::for_channels(&ctx.s().project, &ids, false);
    for id in ids {
        strips.append(&mixer::strip(ctx, id, &layout));
    }
    col.append(&strips);
}

fn region_group(ctx: &Ctx, busy: &Busy) -> gtk::Widget {
    let group = adw::PreferencesGroup::new();
    let region = {
        let s = ctx.s();
        s.selected_region.and_then(|id| s.project.find_region(id)).map(|(_, r)| r.clone())
    };
    let Some(r) = region else {
        group.set_title("Region");
        group.set_description(Some("Nothing selected"));
        return group.upcast();
    };
    group.set_title(&glib_escape(&r.name));
    let id = r.id;

    let name = adw::EntryRow::builder().title("Name").text(&r.name).show_apply_button(true).build();
    {
        let (ctx, busy) = (ctx.clone(), busy.clone());
        name.connect_apply(move |e| {
            let text = e.text().to_string();
            quiet_edit(&ctx, &busy, "Rename Region", |p| p.region_mut(id).unwrap().name = text);
        });
    }
    group.add(&name);

    let mute = adw::SwitchRow::builder().title("Mute").active(r.muted).build();
    {
        let (ctx, busy) = (ctx.clone(), busy.clone());
        mute.connect_active_notify(move |s| {
            let on = s.is_active();
            quiet_edit(&ctx, &busy, "Mute Region", |p| p.region_mut(id).unwrap().muted = on);
        });
    }
    group.add(&mute);

    if let Content::Audio(a) = &r.content {
        let sr = ctx.s().project.sample_rate as f64;
        let gain = adw::SpinRow::with_range(-48.0, 24.0, 0.5);
        gain.set_title("Gain (dB)");
        gain.set_value(a.gain_db as f64);
        {
            let (ctx, busy) = (ctx.clone(), busy.clone());
            gain.connect_value_notify(move |s| {
                let v = s.value() as f32;
                quiet_edit(&ctx, &busy, "Region Gain", |p| {
                    if let Content::Audio(a) = &mut p.region_mut(id).unwrap().content {
                        a.gain_db = v;
                    }
                });
            });
        }
        group.add(&gain);
        for (title, fade_in) in [("Fade In (ms)", true), ("Fade Out (ms)", false)] {
            let row = adw::SpinRow::with_range(0.0, 10000.0, 5.0);
            row.set_title(title);
            let frames = if fade_in { a.fade_in } else { a.fade_out };
            row.set_value(frames as f64 / sr * 1000.0);
            let (ctx, busy) = (ctx.clone(), busy.clone());
            row.connect_value_notify(move |s| {
                let frames = (s.value() / 1000.0 * sr) as u64;
                quiet_edit(&ctx, &busy, "Region Fade", |p| {
                    let r = p.region_mut(id).unwrap();
                    let len = r.length;
                    if let Content::Audio(a) = &mut r.content {
                        if fade_in {
                            a.fade_in = frames.min(len);
                        } else {
                            a.fade_out = frames.min(len);
                        }
                    }
                });
            });
            group.add(&row);
        }
    }
    if let Content::Midi(m) = &r.content {
        let transpose = adw::SpinRow::with_range(-48.0, 48.0, 1.0);
        transpose.set_title("Transpose");
        transpose.set_subtitle(&format!("{} notes", m.notes.len()));
        let (ctx, busy) = (ctx.clone(), busy.clone());
        transpose.connect_value_notify(move |s| {
            let by = s.value() as i32;
            if by == 0 {
                return;
            }
            s.set_value(0.0);
            quiet_edit(&ctx, &busy, "Transpose", |p| {
                if let Content::Midi(m) = &mut p.region_mut(id).unwrap().content {
                    for n in &mut m.notes {
                        n.pitch = (n.pitch as i32 + by).clamp(0, 127) as u8;
                    }
                }
            });
        });
        group.add(&transpose);
    }
    group.upcast()
}

fn glib_escape(s: &str) -> String {
    gtk::glib::markup_escape_text(s).to_string()
}
