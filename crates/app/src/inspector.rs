use crate::mixer;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use solstice_model::*;

/// Logic's inspector: region parameters on top, then the selected track's strip beside the
/// strip it outputs to.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 8);
    col.set_margin_start(8);
    col.set_margin_end(8);
    col.set_margin_top(8);
    col.set_margin_bottom(8);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&col).build();
    fill(ctx, &col);
    let (ctx2, col2) = (ctx.clone(), col.clone());
    ctx.on(move |c| {
        if matches!(c, Change::Project | Change::Selection | Change::Catalog) {
            fill(&ctx2, &col2);
        }
    });
    scroller.upcast()
}

fn fill(ctx: &Ctx, col: &gtk::Box) {
    while let Some(c) = col.first_child() {
        col.remove(&c);
    }
    col.append(&region_group(ctx));

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
    if let Some(t) = track {
        strips.append(&mixer::strip(ctx, Some(t), false));
    }
    strips.append(&mixer::strip(ctx, out, false));
    col.append(&strips);
}

fn region_group(ctx: &Ctx) -> gtk::Widget {
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
        let ctx = ctx.clone();
        name.connect_apply(move |e| {
            let text = e.text().to_string();
            ctx.edit("Rename Region", |p| p.region_mut(id).unwrap().name = text);
        });
    }
    group.add(&name);

    let mute = adw::SwitchRow::builder().title("Mute").active(r.muted).build();
    {
        let ctx = ctx.clone();
        mute.connect_active_notify(move |s| {
            let on = s.is_active();
            ctx.edit("Mute Region", |p| p.region_mut(id).unwrap().muted = on);
        });
    }
    group.add(&mute);

    if let Content::Audio(a) = &r.content {
        let sr = ctx.s().project.sample_rate as f64;
        let gain = adw::SpinRow::with_range(-48.0, 24.0, 0.5);
        gain.set_title("Gain (dB)");
        gain.set_value(a.gain_db as f64);
        {
            let ctx = ctx.clone();
            gain.connect_value_notify(move |s| {
                let v = s.value() as f32;
                ctx.edit("Region Gain", |p| {
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
            let ctx = ctx.clone();
            row.connect_value_notify(move |s| {
                let frames = (s.value() / 1000.0 * sr) as u64;
                ctx.edit("Region Fade", |p| {
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
        let ctx = ctx.clone();
        transpose.connect_value_notify(move |s| {
            let by = s.value() as i32;
            if by == 0 {
                return;
            }
            s.set_value(0.0);
            ctx.edit("Transpose", |p| {
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
