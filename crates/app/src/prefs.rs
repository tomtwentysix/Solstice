use crate::catalog;
use crate::yabridge;
use crate::settings::{MidiCycle, Settings, Startup};
use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::{gio, glib};

const BUFFERS: [u32; 7] = [32, 64, 128, 256, 512, 1024, 2048];
const RATES: [u32; 6] = [44100, 48000, 88200, 96000, 176400, 192000];
const AUTOSAVE: [u32; 5] = [0, 1, 2, 5, 10];
const SNAPS: [&str; 5] = ["Bar", "Beat", "1/8", "1/16", "Off"];

/// Changes a setting, saves it, and optionally rebuilds the engine graph.
pub(crate) fn set(ctx: &Ctx, rebuild: bool, f: impl FnOnce(&mut Settings)) {
    let mut s = ctx.m();
    f(&mut s.settings);
    s.settings.save();
    if rebuild {
        s.rebuild();
    }
}

pub(crate) fn info_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(glib::markup_escape_text(subtitle)).build();
    row.add_css_class("property");
    row
}

fn switch(title: &str, subtitle: &str, active: bool, f: impl Fn(bool) + 'static) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.connect_active_notify(move |r| f(r.is_active()));
    row
}

fn combo(title: &str, subtitle: &str, items: &[&str], selected: u32, f: impl Fn(u32) + 'static) -> adw::ComboRow {
    let row = adw::ComboRow::builder().title(title).model(&gtk::StringList::new(items)).selected(selected).build();
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row.connect_selected_notify(move |r| f(r.selected()));
    row
}

pub fn show(ctx: &Ctx, parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.set_search_enabled(true);
    dialog.add(&general_page(ctx));
    dialog.add(&audio_page(ctx));
    dialog.add(&midi_page(ctx));
    dialog.add(&metronome_page(ctx));
    dialog.add(&editing_page(ctx));
    dialog.add(&plugins_page(ctx, &dialog));
    crate::dialogs::clear_way(&dialog, parent);
    dialog.present(Some(parent));
}

fn general_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("General").icon_name("preferences-system-symbolic").build();
    let st = ctx.s().settings.clone();

    let startup = adw::PreferencesGroup::builder().title("Startup").build();
    let c = ctx.clone();
    startup.add(&combo(
        "When Solstice Starts",
        "",
        &["Show the Project Chooser", "Open the Last Project", "Start an Empty Project"],
        st.startup as u32,
        move |i| {
            set(&c, false, |s| {
                s.startup = [Startup::Chooser, Startup::LastProject, Startup::EmptyProject][i as usize];
            })
        },
    ));
    page.add(&startup);

    let saving = adw::PreferencesGroup::builder()
        .title("Saving")
        .description("Autosave keeps a recovery copy inside the project; your saved project is only replaced when you save")
        .build();
    let names: Vec<String> =
        AUTOSAVE.iter().map(|m| if *m == 0 { "Off".into() } else if *m == 1 { "Every Minute".into() } else { format!("Every {m} Minutes") }).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let current = AUTOSAVE.iter().position(|m| *m == st.autosave_minutes).unwrap_or(2) as u32;
    let c = ctx.clone();
    saving.add(&combo("Autosave", "", &refs, current, move |i| set(&c, false, |s| s.autosave_minutes = AUTOSAVE[i as usize])));
    let undo = adw::SpinRow::with_range(20.0, 1000.0, 10.0);
    undo.set_title("Undo Steps");
    undo.set_value(st.undo_levels as f64);
    let c = ctx.clone();
    undo.connect_value_notify(move |r| {
        let n = r.value() as usize;
        set(&c, false, |s| s.undo_levels = n);
        c.m().history.set_limit(n);
    });
    saving.add(&undo);
    page.add(&saving);
    page
}

pub fn interface_row(ctx: &Ctx, changed: impl Fn() + 'static) -> adw::ComboRow {
    let (mut choices, selected) = {
        let s = ctx.s();
        (s.io.as_ref().map(|io| io.backend.devices()).unwrap_or_default(), s.settings.interface.clone())
    };
    if let Some(d) = selected.as_ref().filter(|d| !choices.contains(d)) {
        choices.push(d.clone());
    }
    let mut names = vec!["Every Interface".to_string()];
    names.extend(choices.iter().cloned());
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let chosen = selected.as_ref().and_then(|d| choices.iter().position(|x| x == d)).map_or(0, |i| i as u32 + 1);
    let c = ctx.clone();
    combo(
        "Interface",
        "Solstice connects only to this interface, leaving other devices on their own clock",
        &refs,
        chosen,
        move |i| {
            let device = (i as usize).checked_sub(1).map(|i| choices[i].clone());
            c.m().set_interface(device);
            changed();
            c.emit(Change::Mixer);
            c.emit(Change::Message);
        },
    )
}

pub fn buffer_row(ctx: &Ctx, changed: impl Fn(Option<u32>) + 'static) -> adw::ComboRow {
    let (rate, current, saved) = {
        let s = ctx.s();
        (s.sample_rate(), s.io.as_ref().map(|io| io.backend.buffer_size()), s.settings.buffer_size)
    };
    let names: Vec<String> = BUFFERS.iter().map(|b| format!("{b} samples ({:.1} ms)", *b as f64 / rate as f64 * 1000.0)).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let cur = current.or(saved).unwrap_or(128);
    let c = ctx.clone();
    combo(
        "Buffer Size",
        "Smaller buffers lower monitoring latency and raise CPU load",
        &refs,
        BUFFERS.iter().position(|b| *b == cur).unwrap_or(2) as u32,
        move |i| {
            let frames = BUFFERS[i as usize];
            let result = c.s().io.as_ref().map(|io| io.backend.set_buffer_size(frames).map(|_| io.backend.round_trip()));
            match result {
                Some(Err(e)) => c.message(&format!("The audio server refused a {frames}-frame buffer: {e}")),
                Some(Ok(rt)) => {
                    set(&c, false, |s| s.buffer_size = Some(frames));
                    c.m().apply_record_offset();
                    changed(Some(rt));
                }
                None => set(&c, false, |s| s.buffer_size = Some(frames)),
            }
        },
    )
}

fn audio_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Audio").icon_name("audio-speakers-symbolic").build();
    let (rate, round_trip, inputs, outputs, error, st) = {
        let s = ctx.s();
        let io = s.io.as_ref();
        (
            s.sample_rate(),
            io.map(|io| io.backend.round_trip()),
            io.map(|io| io.backend.capture_ports()).unwrap_or_default(),
            io.map(|io| io.backend.playback_ports()).unwrap_or_default(),
            s.io_error.clone(),
            s.settings.clone(),
        )
    };

    let device = adw::PreferencesGroup::builder().title("Device").build();
    device.add(&info_row("Audio Server", &error.unwrap_or_else(|| "Connected through the JACK API (PipeWire or JACK)".into())));
    let list = |ports: &[String]| if ports.is_empty() { "None found".to_string() } else { ports.join(", ") };
    let inputs_row = info_row(&format!("Inputs ({})", inputs.len()), &list(&inputs));
    let outputs_row = info_row(&format!("Outputs ({})", outputs.len()), &list(&outputs));
    let (ins, outs) = (inputs_row.clone(), outputs_row.clone());
    let c = ctx.clone();
    device.add(&interface_row(ctx, move || {
        let (i, o) = {
            let s = c.s();
            let io = s.io.as_ref();
            (io.map(|io| io.backend.capture_ports()).unwrap_or_default(), io.map(|io| io.backend.playback_ports()).unwrap_or_default())
        };
        ins.set_title(&format!("Inputs ({})", i.len()));
        ins.set_subtitle(&glib::markup_escape_text(&list(&i)));
        outs.set_title(&format!("Outputs ({})", o.len()));
        outs.set_subtitle(&glib::markup_escape_text(&list(&o)));
    }));
    let mut names = vec!["Audio Server Default".to_string()];
    names.extend(RATES.iter().map(|r| format!("{} kHz", *r as f64 / 1000.0)));
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let chosen = st.sample_rate.and_then(|r| RATES.iter().position(|x| *x == r)).map_or(0, |i| i as u32 + 1);
    let c = ctx.clone();
    let sample_rate = combo("Sample Rate", &format!("Running at {rate} Hz"), &refs, chosen, move |i| {
        let rate = (i as usize).checked_sub(1).map(|i| RATES[i]);
        let result = c.m().set_server_rate(rate);
        match result {
            Ok(note) => {
                if let Some(note) = note {
                    c.message(&note);
                }
                c.emit(Change::Project);
                c.emit(Change::Message);
            }
            Err(e) => c.message(&format!("Could not change the sample rate: {e}")),
        }
    });
    let c = ctx.clone();
    let row = sample_rate.clone();
    // Show the rate actually running, which a project with audio can hold at another value.
    sample_rate.connect_selected_notify(move |_| row.set_subtitle(&format!("Running at {} Hz", c.s().sample_rate())));
    device.add(&sample_rate);
    let latency_row = info_row("Round-Trip Latency", "");
    let offset_row = info_row("Recording Delay Compensation", "");
    let samples = move |f: u32| format!("{f} samples ({:.1} ms)", f as f64 / rate as f64 * 1000.0);
    let show_offset = {
        let (row, c) = (offset_row.clone(), ctx.clone());
        move || {
            row.set_subtitle(&match c.s().record_offset() {
                Some(f) => samples(f),
                None => "Audio engine not running".into(),
            })
        }
    };
    let show_latency = {
        let (row, show_offset) = (latency_row.clone(), show_offset.clone());
        move |frames: Option<u32>| {
            row.set_subtitle(&match frames {
                Some(f) => samples(f),
                None => "Audio engine not running".into(),
            });
            show_offset();
        }
    };
    show_latency(round_trip);
    let buffer = buffer_row(ctx, show_latency);
    device.add(&buffer);
    device.add(&latency_row);
    device.add(&inputs_row);
    device.add(&outputs_row);
    page.add(&device);

    let monitoring = adw::PreferencesGroup::builder().title("Monitoring").build();
    let c = ctx.clone();
    monitoring.add(&switch(
        "Auto Input Monitoring",
        "Armed tracks play their regions until recording starts",
        st.auto_input,
        move |v| set(&c, true, |s| s.auto_input = v),
    ));
    let c = ctx.clone();
    monitoring.add(&switch(
        "Low Latency Mode",
        "Monitored tracks bypass plug-ins over the latency limit and skip delay compensation",
        st.low_latency,
        move |v| {
            set(&c, true, |s| s.low_latency = v);
            c.emit(crate::studio::Change::Mixer);
        },
    ));
    let limit = adw::SpinRow::with_range(0.0, 50.0, 0.5);
    limit.set_title("Latency Limit (ms)");
    limit.set_digits(1);
    limit.set_value(st.low_latency_ms as f64);
    let c = ctx.clone();
    limit.connect_value_notify(move |r| {
        let v = r.value() as f32;
        set(&c, true, |s| s.low_latency_ms = v);
    });
    monitoring.add(&limit);
    page.add(&monitoring);

    let processing = adw::PreferencesGroup::builder().title("Processing").build();
    const THREADS: [(u32, &str); 5] = [(0, "Automatic"), (1, "1"), (2, "2"), (4, "4"), (8, "8")];
    let threads = adw::ComboRow::builder()
        .title("Processing Threads")
        .subtitle("Tracks are shared between this many threads. Takes effect when Solstice next starts.")
        .model(&gtk::StringList::new(&THREADS.map(|t| t.1)))
        .selected(THREADS.iter().position(|t| t.0 == st.processing_threads).unwrap_or(0) as u32)
        .build();
    let c = ctx.clone();
    threads.connect_selected_notify(move |r| {
        let n = THREADS[r.selected() as usize].0;
        set(&c, false, |s| s.processing_threads = n);
    });
    processing.add(&threads);
    let running = ctx.s().io.as_ref().map(|io| io.handle.shared.workers.load(std::sync::atomic::Ordering::Relaxed));
    let status = match (running, st.workers()) {
        (None, _) => "Audio is not running".to_string(),
        (_, 0) => "Only the audio thread processes tracks".to_string(),
        (Some(0), _) => "The audio server didn't allow real-time worker threads, so the audio thread does all the work".to_string(),
        (Some(n), _) => format!("The audio thread and {n} real-time worker threads"),
    };
    processing.add(&info_row("In Use", &status));
    page.add(&processing);

    let recording = adw::PreferencesGroup::builder().title("Recording").build();
    recording.add(&info_row("File Format", "32-bit float WAV at the project sample rate"));
    let folder = info_row("Location", &ctx.s().bundle.audio_dir().display().to_string());
    let open = gtk::Button::from_icon_name("folder-open-symbolic");
    open.add_css_class("flat");
    open.set_valign(gtk::Align::Center);
    open.set_tooltip_text(Some("Open Folder"));
    let c = ctx.clone();
    open.connect_clicked(move |_| {
        let uri = gio::File::for_path(c.s().bundle.audio_dir()).uri();
        let _ = gio::AppInfo::launch_default_for_uri(&uri, gio::AppLaunchContext::NONE);
    });
    folder.add_suffix(&open);
    recording.add(&folder);
    let adjust = adw::SpinRow::with_range(-MAX_ADJUST as f64, MAX_ADJUST as f64, 1.0);
    adjust.set_title("Adjust (samples)");
    adjust.set_subtitle("Added to the latency the audio server reports");
    adjust.set_value(st.record_adjust as f64);
    let c = ctx.clone();
    adjust.connect_value_notify(move |r| {
        let v = r.value() as i32;
        set(&c, false, |s| s.record_adjust = v);
        c.m().apply_record_offset();
        show_offset();
    });
    let measure = gtk::Button::with_label("Measure…");
    measure.set_valign(gtk::Align::Center);
    measure.set_sensitive(!inputs.is_empty() && !outputs.is_empty());
    let (c, spin) = (ctx.clone(), adjust.clone());
    measure.connect_clicked(move |b| {
        let spin = spin.clone();
        measure_dialog(&c, b, &inputs, &outputs, move |v| spin.set_value(v as f64));
    });
    offset_row.add_suffix(&measure);
    recording.add(&offset_row);
    recording.add(&adjust);
    page.add(&recording);
    page
}

const MAX_ADJUST: i64 = 4800;

/// Plays a probe out of an output looped back to an input by cable, and hands `apply` the
/// adjustment that makes the compensation match the measured round trip.
fn measure_dialog(ctx: &Ctx, parent: &gtk::Button, inputs: &[String], outputs: &[String], apply: impl Fn(i32) + 'static) {
    let count = ctx.s().io.as_ref().map_or(2, |io| io.backend.outputs);
    let outs: Vec<&str> = outputs.iter().take(count).map(|s| s.as_str()).collect();
    let ins: Vec<&str> = inputs.iter().take(solstice_backend_jack::INPUTS).map(|s| s.as_str()).collect();
    let output = adw::ComboRow::builder().title("Output").model(&gtk::StringList::new(&outs)).build();
    let input = adw::ComboRow::builder().title("Input").model(&gtk::StringList::new(&ins)).build();
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.append(&output);
    list.append(&input);
    let dialog = adw::AlertDialog::builder()
        .heading("Measure Recording Delay")
        .body("Connect an output of your interface to an input with a cable and turn monitoring down. A short sweep plays from the output while the input is recorded.")
        .extra_child(&list)
        .close_response("cancel")
        .default_response("measure")
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("measure", "_Measure")]);
    dialog.set_response_appearance("measure", adw::ResponseAppearance::Suggested);
    crate::dialogs::clear_way(&dialog, parent);
    dialog.present(Some(parent));
    let (ctx, parent, apply) = (ctx.clone(), parent.clone(), std::rc::Rc::new(apply));
    dialog.connect_response(Some("measure"), move |_, _| {
        let cmd = solstice_engine::Command::Measure { output: output.selected() as u8, input: input.selected() as u8 };
        let Some(shared) = ctx.m().io.as_mut().map(|io| {
            io.handle.shared.measured.store(false, std::sync::atomic::Ordering::Relaxed);
            io.handle.send(cmd);
            io.handle.shared.clone()
        }) else {
            return;
        };
        parent.set_sensitive(false);
        let started = std::time::Instant::now();
        let (ctx, parent, apply) = (ctx.clone(), parent.clone(), apply.clone());
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let done = shared.measured.load(std::sync::atomic::Ordering::Acquire);
            if !done && started.elapsed().as_secs() < 5 {
                return glib::ControlFlow::Continue;
            }
            parent.set_sensitive(true);
            let rate = ctx.s().sample_rate();
            let reported = ctx.s().io.as_ref().map_or(0, |io| io.backend.round_trip()) as i64;
            let found = done
                .then(|| shared.measure.iter().map(|s| s.get()).collect::<Vec<f32>>())
                .and_then(|captured| solstice_engine::measure::find_delay(&captured, rate));
            let ms = |f: i64| f as f64 / rate as f64 * 1000.0;
            let (heading, body) = match found {
                Some(lag) if (lag as i64 - reported).abs() > MAX_ADJUST => (
                    "Measurement Out of Range",
                    format!(
                        "The round trip is {lag} samples ({:.1} ms), {} samples from what the audio server reports, which is more than the adjustment allows. The compensation is unchanged.",
                        ms(lag as i64),
                        (lag as i64 - reported).abs()
                    ),
                ),
                Some(lag) => {
                    let diff = lag as i64 - reported;
                    apply(diff as i32);
                    let more = if diff >= 0 { "more" } else { "less" };
                    (
                        "Measurement Complete",
                        format!(
                            "The round trip is {lag} samples ({:.1} ms), {} samples {more} than the audio server reports. Recording delay compensation now matches it.",
                            ms(lag as i64),
                            diff.abs()
                        ),
                    )
                }
                None if done => (
                    "No Signal Found",
                    "The sweep did not come back on the chosen input. Check the cable and input level. The compensation is unchanged.".to_string(),
                ),
                None => ("Measurement Failed", "The audio engine did not finish the measurement. The compensation is unchanged.".to_string()),
            };
            let result = adw::AlertDialog::new(Some(heading), Some(&body));
            result.add_response("close", "_Close");
            crate::dialogs::clear_way(&result, &parent);
            result.present(Some(&parent));
            glib::ControlFlow::Break
        });
    });
}

fn midi_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("MIDI").icon_name("audio-x-generic-symbolic").build();
    let sources: Vec<(String, bool)> = {
        let s = ctx.s();
        s.io.as_ref()
            .map(|io| io.backend.midi_sources().into_iter().map(|p| (p.clone(), io.backend.midi_connected(&p))).collect())
            .unwrap_or_default()
    };
    let inputs = adw::PreferencesGroup::builder()
        .title("Inputs")
        .description("MIDI devices Solstice listens to. Notes go to the selected instrument track.")
        .build();
    if sources.is_empty() {
        inputs.add(&info_row("No MIDI Devices", "Connect a keyboard or controller, then reopen Preferences"));
    }
    for (port, on) in sources {
        // Port names look like "system:midi_capture_1" or "Client:Port"; show the device part.
        let title = port.rsplit_once(':').map_or(port.as_str(), |(a, b)| if a == "system" { b } else { a }).to_string();
        let c = ctx.clone();
        let p = port.clone();
        let row = switch(&title, &port, on, move |v| {
            if let Some(io) = &c.s().io {
                io.backend.set_midi_connected(&p, v);
            }
            set(&c, false, |s| {
                s.disabled_midi.retain(|x| *x != p);
                if !v {
                    s.disabled_midi.push(p.clone());
                }
            });
        });
        inputs.add(&row);
    }
    page.add(&inputs);

    let thru = adw::PreferencesGroup::new();
    let c = ctx.clone();
    thru.add(&switch(
        "MIDI Thru",
        "Play the selected instrument track from incoming MIDI. Off, MIDI is still recorded.",
        ctx.s().settings.midi_thru,
        move |v| set(&c, true, |s| s.midi_thru = v),
    ));
    page.add(&thru);
    page
}

fn metronome_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Metronome").icon_name("alarm-symbolic").build();
    let st = ctx.s().settings.clone();
    let group = adw::PreferencesGroup::builder().title("Click").build();
    let level = adw::SpinRow::with_range(-40.0, 0.0, 1.0);
    level.set_title("Level (dB)");
    level.set_value(st.click_level_db as f64);
    let c = ctx.clone();
    level.connect_value_notify(move |r| {
        let v = r.value() as f32;
        set(&c, true, |s| s.click_level_db = v);
    });
    group.add(&level);
    let c = ctx.clone();
    group.add(&switch("Accent the First Beat", "A higher click on beat one of each bar", st.click_accent, move |v| {
        set(&c, true, |s| s.click_accent = v)
    }));
    let c = ctx.clone();
    group.add(&switch(
        "Click Only on Beat Groups",
        "Leaves out the beats inside each group, so 6/8 clicks twice per bar",
        st.click_groups_only,
        move |v| set(&c, true, |s| s.click_groups_only = v),
    ));
    page.add(&group);
    let rec = adw::PreferencesGroup::builder().title("Recording").build();
    let count = adw::SpinRow::with_range(0.0, 4.0, 1.0);
    count.set_title("Count-in (bars)");
    count.set_subtitle("The click plays this many bars before recording starts");
    count.set_value(st.count_in_bars as f64);
    let c = ctx.clone();
    count.connect_value_notify(move |r| {
        let v = r.value() as u32;
        set(&c, false, |s| s.count_in_bars = v);
    });
    rec.add(&count);
    let preroll = adw::SpinRow::with_range(0.0, 4.0, 1.0);
    preroll.set_title("Pre-roll (bars)");
    preroll.set_subtitle("Playback starts this many bars before the record position");
    preroll.set_value(st.preroll_bars as f64);
    let c = ctx.clone();
    preroll.connect_value_notify(move |r| {
        let v = r.value() as u32;
        set(&c, false, |s| s.preroll_bars = v);
    });
    rec.add(&preroll);
    let c = ctx.clone();
    rec.add(&combo(
        "MIDI Cycle Recording",
        "What each pass over the cycle makes",
        &["Create Take Folders", "Merge"],
        st.midi_cycle as u32,
        move |i| set(&c, false, |s| s.midi_cycle = if i == 1 { MidiCycle::Merge } else { MidiCycle::TakeFolder }),
    ));
    page.add(&rec);
    page
}

fn editing_page(ctx: &Ctx) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Editing").icon_name("document-edit-symbolic").build();
    let group = adw::PreferencesGroup::builder().title("Tracks Area").build();
    let c = ctx.clone();
    group.add(&combo(
        "Default Snap",
        "Used for new windows; the snap menu changes it for the current one",
        &SNAPS,
        ctx.s().settings.default_snap,
        move |i| set(&c, false, |s| s.default_snap = i),
    ));
    page.add(&group);
    page
}

/// A plug-in folder with a button to remove it; the plug-ins are scanned again either way.
fn add_folder_row(ctx: &Ctx, list: &gtk::ListBox, p: std::path::PathBuf) {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.display().to_string())).build();
    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_valign(gtk::Align::Center);
    remove.set_tooltip_text(Some("Remove Folder"));
    let (ctx, row2, list2) = (ctx.clone(), row.clone(), list.clone());
    remove.connect_clicked(move |_| {
        set(&ctx, false, |s| s.vst3_paths.retain(|x| *x != p));
        list2.remove(&row2);
        list2.set_visible(list2.first_child().is_some());
        rescan_plugins(&ctx, false, None);
    });
    row.add_suffix(&remove);
    list.append(&row);
}

fn plugins_page(ctx: &Ctx, dialog: &adw::PreferencesDialog) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder().title("Plug-ins").icon_name("application-x-addon-symbolic").build();
    let settings = ctx.s().settings.clone();

    let folders = adw::PreferencesGroup::builder()
        .title("Plug-in Folders")
        .description("VST3 and CLAP plug-ins here are found too, alongside ~/.vst3, ~/.clap and the system plug-in folders. LV2 plug-ins are found in ~/.lv2 and the system LV2 folders.")
        .build();
    let folder_list = gtk::ListBox::new();
    folder_list.add_css_class("boxed-list");
    folder_list.set_selection_mode(gtk::SelectionMode::None);
    for p in &settings.vst3_paths {
        add_folder_row(ctx, &folder_list, p.clone());
    }
    folder_list.set_visible(!settings.vst3_paths.is_empty());
    folders.add(&folder_list);
    let add = adw::ButtonRow::builder().title("Add Folder…").start_icon_name("list-add-symbolic").build();
    {
        let (ctx, d, folder_list) = (ctx.clone(), dialog.clone(), folder_list.clone());
        add.connect_activated(move |_| {
            let (ctx, folder_list) = (ctx.clone(), folder_list.clone());
            let chooser = gtk::FileDialog::builder().title("Add Plug-in Folder").build();
            let win = d.root().and_downcast::<gtk::Window>();
            chooser.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                    && !ctx.s().settings.vst3_paths.contains(&path)
                {
                    set(&ctx, false, |s| s.vst3_paths.push(path.clone()));
                    add_folder_row(&ctx, &folder_list, path);
                    folder_list.set_visible(true);
                    rescan_plugins(&ctx, false, None);
                }
            });
        });
    }
    folders.add(&add);
    let rescan = adw::ButtonRow::builder().title("Rescan Plug-ins").start_icon_name("view-refresh-symbolic").build();
    {
        let ctx = ctx.clone();
        rescan.connect_activated(move |r| {
            r.set_sensitive(false);
            r.set_title("Scanning…");
            rescan_plugins(&ctx, true, Some(r.clone()));
        });
    }
    folders.add(&rescan);
    page.add(&folders);
    page.add(&windows_group(ctx, dialog));

    // Installed plug-ins by vendor, each switchable.
    let catalog = ctx.s().catalog.clone();
    let installed = adw::PreferencesGroup::builder()
        .title("Installed")
        .description(format!("{} plug-ins. Switched-off plug-ins are hidden from the plug-in menus.", catalog.len()))
        .build();
    let mut vendors: std::collections::BTreeMap<String, Vec<solstice_vst3::ClassInfo>> = Default::default();
    for c in catalog {
        vendors.entry(if c.vendor.is_empty() { "Unknown".into() } else { c.vendor.clone() }).or_default().push(c);
    }
    for (vendor, items) in vendors {
        let exp = adw::ExpanderRow::builder().title(glib::markup_escape_text(&vendor)).subtitle(items.len().to_string()).build();
        for info in items {
            let on = !settings.disabled_plugins.contains(&info.uid);
            let (ctx, uid) = (ctx.clone(), info.uid.clone());
            let row = switch(&glib::markup_escape_text(&info.name), &info.subcategories.replace('|', " · "), on, move |v| {
                set(&ctx, false, |s| {
                    s.disabled_plugins.retain(|x| *x != uid);
                    if !v {
                        s.disabled_plugins.push(uid.clone());
                    }
                });
                ctx.emit(Change::Catalog);
            });
            exp.add_row(&row);
        }
        installed.add(&exp);
    }
    page.add(&installed);

    let failures = catalog::failures();
    if !failures.is_empty() {
        let failed = adw::PreferencesGroup::builder()
            .title("Failed to Load")
            .description("These modules crashed or errored in the scanner and are not available")
            .build();
        for (path, reason) in failures {
            let name = path.file_name().map_or(path.display().to_string(), |n| n.to_string_lossy().into_owned());
            let row = info_row(&glib::markup_escape_text(&name), &reason);
            row.set_tooltip_text(Some(&path.display().to_string()));
            failed.add(&row);
        }
        page.add(&failed);
    }
    page
}

/// Windows VST3s bridged by yabridge from Wine prefixes into `~/.vst3/yabridge`.
fn windows_group(ctx: &Ctx, dialog: &adw::PreferencesDialog) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Windows Plug-ins").build();
    if yabridge::ctl().is_none() {
        group.set_description(Some("Install yabridge to use Windows VST3 plug-ins installed with Wine"));
        return group;
    }
    group.set_description(Some("Folders in Wine prefixes that hold Windows VST3 plug-ins, bridged by yabridge"));
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    group.add(&list);
    fill_windows_folders(&list, ctx);

    let add = adw::ButtonRow::builder().title("Add Folder…").start_icon_name("list-add-symbolic").build();
    {
        let (ctx, d, list) = (ctx.clone(), dialog.clone(), list.clone());
        add.connect_activated(move |_| {
            let (ctx, list) = (ctx.clone(), list.clone());
            let chooser = gtk::FileDialog::builder().title("Add Windows Plug-in Folder").build();
            if let Some(dir) = yabridge::default_vst3_folder() {
                chooser.set_initial_folder(Some(&gio::File::for_path(dir)));
            }
            let win = d.root().and_downcast::<gtk::Window>();
            chooser.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                {
                    if let Err(e) = yabridge::add(&path) {
                        ctx.message(&e);
                    }
                    fill_windows_folders(&list, &ctx);
                }
            });
        });
    }
    group.add(&add);

    let sync = adw::ButtonRow::builder().title("Bridge Plug-ins").start_icon_name("view-refresh-symbolic").build();
    {
        let ctx = ctx.clone();
        sync.connect_activated(move |r| {
            r.set_sensitive(false);
            r.set_title("Bridging…");
            let (ctx, r) = (ctx.clone(), r.clone());
            glib::spawn_future_local(async move {
                let result = gio::spawn_blocking(yabridge::sync).await.unwrap_or_else(|_| Err("yabridgectl failed".into()));
                r.set_title("Bridge Plug-ins");
                r.set_sensitive(true);
                match result {
                    Ok(summary) => {
                        ctx.message(&summary);
                        rescan_plugins(&ctx, false, None);
                    }
                    Err(e) => ctx.message(&e),
                }
            });
        });
    }
    group.add(&sync);
    group
}

fn fill_windows_folders(list: &gtk::ListBox, ctx: &Ctx) {
    list.remove_all();
    let folders = yabridge::folders();
    let suggested = yabridge::default_vst3_folder().filter(|d| !folders.contains(d));
    let rows = folders.into_iter().map(|f| (f, false)).chain(suggested.map(|d| (d, true)));
    for (folder, is_suggestion) in rows {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&folder.display().to_string())).build();
        let button = if is_suggestion {
            row.set_subtitle("Default Wine prefix");
            gtk::Button::with_label("Add")
        } else {
            let b = gtk::Button::from_icon_name("user-trash-symbolic");
            b.add_css_class("flat");
            b.set_tooltip_text(Some("Remove Folder"));
            b
        };
        button.set_valign(gtk::Align::Center);
        let (ctx, list2) = (ctx.clone(), list.clone());
        button.connect_clicked(move |_| {
            let r = if is_suggestion { yabridge::add(&folder) } else { yabridge::remove(&folder) };
            if let Err(e) = r {
                ctx.message(&e);
            }
            fill_windows_folders(&list2, &ctx);
        });
        row.add_suffix(&button);
        list.append(&row);
    }
}

/// Replaces the catalog. Returns how many plug-ins the scan found, leaving out the built-in ones.
pub async fn scan_catalog(ctx: &Ctx, force: bool) -> usize {
    let paths = ctx.s().settings.search_paths();
    let found = gio::spawn_blocking(move || catalog::scan(&paths, force)).await.unwrap_or_default();
    let n = found.len();
    let mut all = crate::host::builtin_catalog();
    all.extend(found);
    ctx.m().catalog = all;
    ctx.emit(Change::Catalog);
    n
}

pub fn rescan_plugins(ctx: &Ctx, force: bool, row: Option<adw::ButtonRow>) {
    let ctx = ctx.clone();
    glib::spawn_future_local(async move {
        let n = scan_catalog(&ctx, force).await;
        if let Some(r) = row {
            r.set_title(&format!("Rescan Plug-ins ({n} found)"));
            r.set_sensitive(true);
        }
    });
}
