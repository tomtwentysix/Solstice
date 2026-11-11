use crate::catalog;
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gio, glib};

const BUFFERS: [u32; 7] = [32, 64, 128, 256, 512, 1024, 2048];

pub fn show(ctx: &Ctx, parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");

    let audio = adw::PreferencesPage::builder().title("Audio").icon_name("audio-speakers-symbolic").build();
    let device = adw::PreferencesGroup::builder().title("Device").build();
    let (rate, current, round_trip, settings) = {
        let s = ctx.s();
        let io = s.io.as_ref();
        (
            s.sample_rate(),
            io.map(|io| io.backend.buffer_size()),
            io.map(|io| io.backend.round_trip()),
            s.settings.clone(),
        )
    };
    let names: Vec<String> = BUFFERS.iter().map(|b| format!("{b} samples ({:.1} ms)", *b as f64 / rate as f64 * 1000.0)).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let buffer = adw::ComboRow::builder()
        .title("Buffer Size")
        .subtitle("Smaller buffers lower monitoring latency and raise CPU load")
        .model(&gtk::StringList::new(&refs))
        .build();
    let cur = current.or(settings.buffer_size).unwrap_or(128);
    buffer.set_selected(BUFFERS.iter().position(|b| *b == cur).unwrap_or(2) as u32);
    let latency_row = adw::ActionRow::builder().title("Round-Trip Latency").build();
    let show_latency = {
        let row = latency_row.clone();
        move |frames: Option<u32>| {
            row.set_subtitle(&match frames {
                Some(f) => format!("{f} samples ({:.1} ms) at {rate} Hz", f as f64 / rate as f64 * 1000.0),
                None => "Audio engine not running".into(),
            });
        }
    };
    show_latency(round_trip);
    {
        let ctx = ctx.clone();
        buffer.connect_selected_notify(move |r| {
            let frames = BUFFERS[r.selected() as usize];
            let mut s = ctx.m();
            s.settings.buffer_size = Some(frames);
            s.settings.save();
            if let Some(io) = &s.io {
                if let Err(e) = io.backend.set_buffer_size(frames) {
                    eprintln!("solstice: buffer size: {e}");
                }
                let rt = io.backend.round_trip();
                drop(s);
                show_latency(Some(rt));
            }
        });
    }
    device.add(&buffer);
    device.add(&latency_row);
    audio.add(&device);

    let monitoring = adw::PreferencesGroup::builder().title("Monitoring").build();
    let auto = adw::SwitchRow::builder()
        .title("Auto Input Monitoring")
        .subtitle("Armed tracks play their regions until recording starts")
        .active(settings.auto_input)
        .build();
    let low = adw::SwitchRow::builder()
        .title("Low Latency Mode")
        .subtitle("Bypass plug-ins on monitored tracks that add more latency than the limit")
        .active(settings.low_latency)
        .build();
    let limit = adw::SpinRow::with_range(0.0, 50.0, 0.5);
    limit.set_title("Latency Limit (ms)");
    limit.set_value(settings.low_latency_ms as f64);
    for (row, apply) in [
        (auto.upcast_ref::<gtk::Widget>().clone(), 0),
        (low.upcast_ref::<gtk::Widget>().clone(), 1),
    ] {
        let ctx = ctx.clone();
        row.downcast::<adw::SwitchRow>().unwrap().connect_active_notify(move |r| {
            let mut s = ctx.m();
            if apply == 0 {
                s.settings.auto_input = r.is_active();
            } else {
                s.settings.low_latency = r.is_active();
            }
            s.settings.save();
            s.rebuild();
        });
    }
    {
        let ctx = ctx.clone();
        limit.connect_value_notify(move |r| {
            let mut s = ctx.m();
            s.settings.low_latency_ms = r.value() as f32;
            s.settings.save();
            s.rebuild();
        });
    }
    monitoring.add(&auto);
    monitoring.add(&low);
    monitoring.add(&limit);
    audio.add(&monitoring);
    dialog.add(&audio);

    let plugins = adw::PreferencesPage::builder().title("Plug-ins").icon_name("application-x-addon-symbolic").build();
    let folders = adw::PreferencesGroup::builder()
        .title("VST3 Folders")
        .description("~/.vst3 and the system VST3 folders are always searched")
        .build();
    for p in &settings.vst3_paths {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.display().to_string())).build();
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.add_css_class("flat");
        remove.set_valign(gtk::Align::Center);
        let (ctx, p, row2, group) = (ctx.clone(), p.clone(), row.clone(), folders.clone());
        remove.connect_clicked(move |_| {
            let mut s = ctx.m();
            s.settings.vst3_paths.retain(|x| *x != p);
            s.settings.save();
            group.remove(&row2);
        });
        row.add_suffix(&remove);
        folders.add(&row);
    }
    let add = adw::ButtonRow::builder().title("Add Folder…").start_icon_name("list-add-symbolic").build();
    {
        let (ctx, d) = (ctx.clone(), dialog.clone());
        add.connect_activated(move |_| {
            let ctx = ctx.clone();
            let chooser = gtk::FileDialog::builder().title("Add VST3 Folder").build();
            let win = d.root().and_downcast::<gtk::Window>();
            chooser.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                {
                    let mut s = ctx.m();
                    if !s.settings.vst3_paths.contains(&path) {
                        s.settings.vst3_paths.push(path);
                        s.settings.save();
                    }
                }
            });
        });
    }
    folders.add(&add);
    plugins.add(&folders);
    let scan_group = adw::PreferencesGroup::new();
    let count = ctx.s().catalog.len();
    let rescan = adw::ButtonRow::builder().title("Rescan Plug-ins").start_icon_name("view-refresh-symbolic").build();
    rescan.set_tooltip_text(Some(&format!("{count} plug-ins found")));
    {
        let ctx = ctx.clone();
        rescan.connect_activated(move |r| {
            r.set_sensitive(false);
            r.set_title("Scanning…");
            rescan_plugins(&ctx, true, Some(r.clone()));
        });
    }
    scan_group.add(&rescan);
    plugins.add(&scan_group);
    dialog.add(&plugins);

    dialog.present(Some(parent));
}

pub fn rescan_plugins(ctx: &Ctx, force: bool, row: Option<adw::ButtonRow>) {
    let paths = ctx.s().settings.search_paths();
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let found = gio::spawn_blocking(move || catalog::scan(&paths, force)).await.unwrap_or_default();
        let n = found.len();
        ctx.m().catalog = found;
        ctx.emit(Change::Catalog);
        if let Some(r) = row {
            r.set_title(&format!("Rescan Plug-ins ({n} found)"));
            r.set_sensitive(true);
        }
    });
}
