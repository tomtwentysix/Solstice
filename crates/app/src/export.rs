//! The Export dialog, for Export Mix and Export Stems.

use crate::render::{self, ExportChoices};
use crate::studio::Ctx;
use adw::prelude::*;
use gtk::gio;
use solstice_engine::export::{Dither, Format};
use solstice_model::{Id, LoudnessTarget};

const RANGES: [&str; 2] = ["Whole Project", "Cycle Range"];

fn combo(title: &str, items: &[&str], selected: usize) -> adw::ComboRow {
    adw::ComboRow::builder().title(title).model(&gtk::StringList::new(items)).selected(selected as u32).build()
}

pub fn present(ctx: &Ctx, win: &adw::ApplicationWindow, toasts: &adw::ToastOverlay, stems: bool) {
    let (choices, rate, has_cycle, target, list, name) = {
        let s = ctx.s();
        let p = &s.project;
        (s.settings.export, s.sample_rate(), render::has_cycle(p), p.master_target, render::stems(p), p.name.clone())
    };
    let title = if stems { "Export Stems" } else { "Export Mix" };
    let page = adw::PreferencesPage::new();

    let file = adw::PreferencesGroup::builder().title("File").description(format!("Written at {rate} Hz, without resampling")).build();
    let names: Vec<&str> = Format::ALL.iter().map(|f| f.name()).collect();
    let format = combo("Format", &names, Format::ALL.iter().position(|f| *f == choices.format).unwrap_or(1));
    let names: Vec<&str> = Dither::ALL.iter().map(|d| d.name()).collect();
    let dither = combo("Dither", &names, Dither::ALL.iter().position(|d| *d == choices.dither).unwrap_or(1));
    let integer = |row: &adw::ComboRow| Format::ALL[row.selected() as usize].bits().is_some();
    dither.set_sensitive(integer(&format));
    {
        let dither = dither.clone();
        format.connect_selected_notify(move |r| dither.set_sensitive(integer(r)));
    }
    file.add(&format);
    file.add(&dither);
    page.add(&file);

    let span = adw::PreferencesGroup::builder().title("Range").build();
    let range = combo("Range", &RANGES, usize::from(choices.cycle && has_cycle));
    range.set_sensitive(has_cycle);
    let tail = adw::SpinRow::with_range(0.0, 10.0, 0.5);
    tail.set_title("Tail");
    tail.set_subtitle("Seconds after the end for reverbs and delays to ring out");
    tail.set_digits(1);
    tail.set_value(choices.tail);
    span.add(&range);
    span.add(&tail);
    page.add(&span);

    let targets: Vec<LoudnessTarget> = LoudnessTarget::ALL.into_iter().filter(|t| t.levels().is_some()).collect();
    let normalise = adw::SwitchRow::builder().title("Normalise").subtitle("Gain only, without limiting").active(choices.normalise).build();
    let names: Vec<&str> = targets.iter().map(|t| t.name()).collect();
    let target_row = combo("Target", &names, targets.iter().position(|t| *t == target).unwrap_or(0));
    normalise.bind_property("active", &target_row, "sensitive").sync_create().build();
    if !stems {
        let loudness = adw::PreferencesGroup::builder().title("Loudness").build();
        loudness.add(&normalise);
        loudness.add(&target_row);
        page.add(&loudness);
    }

    let mut checks: Vec<(Id, gtk::CheckButton)> = Vec::new();
    if stems {
        let group = adw::PreferencesGroup::builder()
            .title("Tracks and Buses")
            .description("Each is exported on its own, at its level in the mix")
            .build();
        for s in &list {
            let check = gtk::CheckButton::builder().active(s.checked).valign(gtk::Align::Center).build();
            let row = adw::ActionRow::builder().title(gtk::glib::markup_escape_text(&s.name)).activatable_widget(&check).build();
            if s.bus {
                row.set_subtitle("Bus");
            }
            row.add_prefix(&check);
            group.add(&row);
            checks.push((s.id, check));
        }
        if list.is_empty() {
            group.add(&adw::ActionRow::builder().title("There are no tracks or buses to export").build());
        }
        page.add(&group);
    }

    let export = gtk::Button::builder().label("_Export…").use_underline(true).build();
    export.add_css_class("suggested-action");
    let cancel = gtk::Button::builder().label("_Cancel").use_underline(true).build();
    let header = adw::HeaderBar::builder().show_start_title_buttons(false).show_end_title_buttons(false).build();
    header.pack_start(&cancel);
    header.pack_end(&export);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder().title(title).content_width(480).content_height(if stems { 640 } else { 520 }).child(&view).build();
    {
        let d = dialog.clone();
        cancel.connect_clicked(move |_| {
            d.close();
        });
    }
    if stems {
        let update = {
            let (export, checks) = (export.clone(), checks.clone());
            move || export.set_sensitive(checks.iter().any(|(_, c)| c.is_active()))
        };
        update();
        for (_, c) in &checks {
            let update = update.clone();
            c.connect_toggled(move |_| update());
        }
    }

    dialog.present(Some(win));
    let (ctx, win, toasts, d) = (ctx.clone(), win.clone(), toasts.clone(), dialog.clone());
    export.connect_clicked(move |_| {
        let choices = ExportChoices {
            format: Format::ALL[format.selected() as usize],
            dither: Dither::ALL[dither.selected() as usize],
            normalise: normalise.is_active(),
            cycle: range.selected() == 1,
            tail: tail.value(),
        };
        {
            let mut s = ctx.m();
            s.settings.export = choices;
            s.settings.save();
        }
        let levels = targets.get(target_row.selected() as usize).and_then(|t| t.levels());
        let ids: Vec<Id> = checks.iter().filter(|(_, c)| c.is_active()).map(|(id, _)| *id).collect();
        d.close();
        let (ctx, toasts, w) = (ctx.clone(), toasts.clone(), win.clone());
        let nothing = move |t: &adw::ToastOverlay| t.add_toast(adw::Toast::new("There is nothing to export"));
        if stems {
            let chooser = gtk::FileDialog::builder().title("Export Stems").build();
            chooser.select_folder(Some(&win), gio::Cancellable::NONE, move |r| {
                let Some(dir) = r.ok().and_then(|f| f.path()) else { return };
                if ctx.export_stems(&dir, &ids, &choices) > 0 {
                    render::progress_dialog(&ctx, &w);
                } else {
                    nothing(&toasts);
                }
            });
        } else {
            let file = format!("{name}.{}", choices.format.extension());
            let chooser = gtk::FileDialog::builder().title("Export Mix").initial_name(file).build();
            chooser.save(Some(&win), gio::Cancellable::NONE, move |r| {
                let Some(path) = r.ok().and_then(|f| f.path()) else { return };
                let normalise = levels.filter(|_| choices.normalise);
                if ctx.export_mix(path, &choices, normalise) {
                    render::progress_dialog(&ctx, &w);
                } else {
                    nothing(&toasts);
                }
            });
        }
    });
}
