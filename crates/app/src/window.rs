use crate::peaks::Peaks;
use crate::studio::{Change, Ctx};
use crate::{browser, controlbar, editor, inspector, mixer, plugins, prefs, timeline};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use solstice_model::*;
use std::rc::Rc;

pub const APP_ID: &str = "uk.co.solarflareit.Solstice";

fn panel(child: &impl IsA<gtk::Widget>, width: i32, transition: gtk::RevealerTransitionType, shown: bool) -> gtk::Revealer {
    child.set_size_request(width, -1);
    let r = gtk::Revealer::builder().transition_type(transition).reveal_child(shown).child(child).build();
    r.add_css_class("side-panel");
    r.set_hexpand(false);
    r
}

pub fn build(app: &adw::Application, ctx: &Ctx) -> adw::ApplicationWindow {
    let win = adw::ApplicationWindow::builder().application(app).default_width(1440).default_height(900).build();

    let bottom_stack = adw::ViewStack::new();
    let editor_page = editor::build(ctx);
    bottom_stack.add_titled_with_icon(&mixer::build(ctx), Some("mixer"), "Mixer", "multimedia-volume-control-symbolic");
    bottom_stack.add_titled_with_icon(&editor_page, Some("editor"), "Editor", "document-edit-symbolic");
    let switcher = adw::InlineViewSwitcher::builder().stack(&bottom_stack).halign(gtk::Align::Center).build();
    switcher.add_css_class("flat");
    let bottom = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let bottom_bar = gtk::CenterBox::new();
    bottom_bar.set_center_widget(Some(&switcher));
    bottom_bar.add_css_class("toolbar");
    bottom.append(&bottom_bar);
    bottom.append(&bottom_stack);
    bottom.set_visible(false);

    let open_editor: Rc<dyn Fn()> = {
        let (bottom, stack) = (bottom.clone(), bottom_stack.clone());
        Rc::new(move || {
            bottom.set_visible(true);
            stack.set_visible_child_name("editor");
        })
    };
    let tracks = timeline::build(ctx, open_editor.clone());

    let library = panel(&plugins::library(ctx), 260, gtk::RevealerTransitionType::SlideRight, false);
    let inspector = panel(&inspector::build(ctx), 230, gtk::RevealerTransitionType::SlideRight, true);
    let browser = panel(&browser::build(ctx), 280, gtk::RevealerTransitionType::SlideLeft, false);

    let main = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    main.append(&library);
    main.append(&inspector);
    main.append(&tracks.widget);
    main.append(&browser);

    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&main)
        .end_child(&bottom)
        .resize_end_child(false)
        .shrink_end_child(false)
        .position(520)
        .build();
    // Opening the bottom pane gives it room for the whole compact mixer, as Logic does.
    {
        let paned = paned.clone();
        bottom.connect_visible_notify(move |b| {
            if b.is_visible() && paned.height() > 0 {
                paned.set_position((paned.height() - 500).max(160));
            }
        });
    }

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&controlbar::build(ctx)));
    let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    views.add_css_class("linked");
    for (icon, tip, action) in [
        ("view-grid-symbolic", "Library (Y)", "win.show-library"),
        ("dialog-information-symbolic", "Inspector (I)", "win.show-inspector"),
        ("multimedia-volume-control-symbolic", "Mixer (X)", "win.show-mixer"),
        ("document-edit-symbolic", "Editors (E)", "win.show-editor"),
    ] {
        let b = gtk::ToggleButton::new();
        b.set_icon_name(icon);
        b.set_tooltip_text(Some(tip));
        b.set_action_name(Some(action));
        views.append(&b);
    }
    header.pack_start(&views);

    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some("_New…"), Some("win.new"));
    file.append(Some("_Open…"), Some("win.open"));
    file.append(Some("_Save"), Some("win.save"));
    file.append(Some("Save _As…"), Some("win.save-as"));
    file.append(Some("Save as _Template…"), Some("win.save-template"));
    file.append(Some("_Revert to Saved"), Some("win.revert"));
    file.append(Some("_Close Project"), Some("win.close"));
    file.append(Some("_Bounce Project…"), Some("win.bounce"));
    file.append(Some("_Export All Tracks as Audio Files…"), Some("win.export-tracks"));
    let imports = gio::Menu::new();
    imports.append(Some("Import _Logic Project…"), Some("win.import-logic"));
    imports.append(Some("Import Final Cut Pro XML or _MIDI File…"), Some("win.import-file"));
    file.append_submenu(Some("_Import"), &imports);
    let recent = gio::Menu::new();
    file.insert_submenu(2, Some("Open _Recent"), &recent);
    menu.append_section(None, &file);
    let history = gio::Menu::new();
    history.append(Some("_Undo"), Some("win.undo"));
    history.append(Some("_Redo"), Some("win.redo"));
    menu.append_section(None, &history);
    let clip = gio::Menu::new();
    clip.append(Some("Cu_t"), Some("win.cut"));
    clip.append(Some("_Copy"), Some("win.copy"));
    clip.append(Some("_Paste"), Some("win.paste"));
    clip.append(Some("Select _All"), Some("win.select-all-regions"));
    clip.append(Some("Repeat"), Some("win.repeat"));
    clip.append(Some("Join"), Some("win.join"));
    menu.append_section(None, &clip);
    let edit = gio::Menu::new();
    edit.append(Some("Split Region at Playhead"), Some("win.split-region"));
    edit.append(Some("Bounce Regions in Place"), Some("win.bounce-in-place"));
    edit.append(Some("Freeze or Unfreeze Track"), Some("win.freeze"));
    edit.append(Some("Delete Region"), Some("win.delete-region"));
    edit.append(Some("Delete Track"), Some("win.delete-track"));
    menu.append_section(None, &edit);
    let record = gio::Menu::new();
    record.append(Some("Capture as _Recording"), Some("win.capture-recording"));
    record.append(Some("Musical _Typing"), Some("win.musical-typing"));
    menu.append_section(None, &record);
    let app_section = gio::Menu::new();
    app_section.append(Some("Open _Mixer Window"), Some("win.mixer-window"));
    app_section.append(Some("Project _Settings…"), Some("win.project-settings"));
    app_section.append(Some("_Preferences"), Some("win.preferences"));
    app_section.append(Some("_Keyboard Shortcuts"), Some("win.show-help-overlay"));
    app_section.append(Some("_About Solstice"), Some("win.about"));
    app_section.append(Some("_Quit"), Some("win.quit"));
    menu.append_section(None, &app_section);
    let menu_button = gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).primary(true).build();
    header.pack_end(&menu_button);
    {
        let (ctx, recent) = (ctx.clone(), recent.clone());
        let fill = move || {
            recent.remove_all();
            for dir in ctx.s().settings.recent_list.iter().filter(|d| d.is_dir()) {
                let name = dir.file_stem().map_or(dir.display().to_string(), |s| s.to_string_lossy().into_owned());
                let item = gio::MenuItem::new(Some(&name), None);
                item.set_action_and_target_value(Some("win.open-recent"), Some(&dir.to_string_lossy().to_variant()));
                recent.append_item(&item);
            }
        };
        fill();
        menu_button.connect_active_notify(move |b| {
            if b.is_active() {
                fill();
            }
        });
    }
    let browse = gtk::ToggleButton::new();
    browse.set_icon_name("folder-music-symbolic");
    browse.set_tooltip_text(Some("Browser (F)"));
    browse.set_action_name(Some("win.show-browser"));
    header.pack_end(&browse);
    let undo = gtk::Button::from_icon_name("edit-undo-symbolic");
    undo.set_action_name(Some("win.undo"));
    undo.set_tooltip_text(Some("Undo"));
    let redo = gtk::Button::from_icon_name("edit-redo-symbolic");
    redo.set_action_name(Some("win.redo"));
    redo.set_tooltip_text(Some("Redo"));
    header.pack_end(&redo);
    header.pack_end(&undo);

    let banner = adw::Banner::builder().button_label("Retry").build();
    {
        let (ctx, b) = (ctx.clone(), banner.clone());
        banner.connect_button_clicked(move |_| {
            ctx.m().start_audio();
            update_banner(&ctx, &b);
            ctx.emit(Change::Engine);
        });
    }
    update_banner(ctx, &banner);
    {
        let (ctx2, b) = (ctx.clone(), banner.clone());
        ctx.on(move |c| {
            if c == Change::Engine {
                update_banner(&ctx2, &b);
            }
        });
    }

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&paned));
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.add_top_bar(&banner);
    tv.set_content(Some(&toasts));
    win.set_content(Some(&tv));

    // Nudging moves by the snap grid (a beat when snap is off).
    let snap_step: std::rc::Rc<dyn Fn() -> u64> = {
        let (ctx, view) = (ctx.clone(), tracks.view.clone());
        std::rc::Rc::new(move || {
            let time = ctx.s().project.time();
            let ticks = timeline::grid_ticks(view.snap.get(), &time);
            time.ticks_to_samples(if ticks == 0 { time.ticks_per_beat() } else { ticks })
        })
    };
    install_actions(&win, ctx, &library, &inspector, &browser, &bottom, &bottom_stack, &toasts, snap_step);
    {
        let a = gio::SimpleAction::new("open-recent", Some(glib::VariantTy::STRING));
        let (ctx, t, w) = (ctx.clone(), toasts.clone(), win.clone());
        a.connect_activate(move |_, v| {
            let Some(path) = v.and_then(|v| v.get::<String>()).map(std::path::PathBuf::from) else { return };
            if let Err(e) = crate::projects::open_project(&ctx, &w, &path) {
                t.add_toast(adw::Toast::new(&glib::markup_escape_text(&e)));
            }
        });
        win.add_action(&a);
    }
    {
        let a = gio::SimpleAction::new("show-help-overlay", None);
        let w = win.clone();
        a.connect_activate(move |_, _| shortcuts_dialog().present(Some(&w)));
        win.add_action(&a);
    }
    {
        let a = gio::SimpleAction::new_stateful("show-global", None, &false.to_variant());
        let global = tracks.global.clone();
        a.connect_activate(move |a, _| {
            let on = !global.reveals_child();
            global.set_reveal_child(on);
            a.set_state(&on.to_variant());
        });
        win.add_action(&a);
    }
    {
        let a = gio::SimpleAction::new("toggle-automation", None);
        let (view, ctx2) = (tracks.view.clone(), ctx.clone());
        a.connect_activate(move |_, _| {
            view.show_auto.set(!view.show_auto.get());
            ctx2.emit(Change::View);
        });
        win.add_action(&a);
    }
    {
        let a = gio::SimpleAction::new("tool-menu", None);
        let tools = tracks.tools.clone();
        a.connect_activate(move |_, _| tools.popup());
        win.add_action(&a);
    }
    install_import(&win, ctx, &toasts);
    {
        let (ctx2, toasts, w) = (ctx.clone(), toasts.clone(), win.clone());
        ctx.on(move |c| {
            if c == Change::Message
                && let Some(text) = ctx2.m().message.take()
            {
                // Shown over Preferences while it is open, where it would otherwise be hidden.
                let toast = adw::Toast::new(&glib::markup_escape_text(&text));
                match w.visible_dialog().and_downcast::<adw::PreferencesDialog>() {
                    Some(d) => d.add_toast(toast),
                    None => toasts.add_toast(toast),
                }
            }
        });
    }
    {
        let (browser, w) = (browser.clone(), win.clone());
        ctx.on(move |c| {
            if c == Change::Browse && !browser.reveals_child() {
                let _ = WidgetExt::activate_action(&w, "win.show-browser", None);
            }
        });
    }
    {
        // Undo and Redo name the step they act on, and grey out with nothing to do.
        let (w, ctx2, undo, redo) = (win.clone(), ctx.clone(), undo.clone(), redo.clone());
        let update = move || {
            let s = ctx2.s();
            for (name, label, button, verb) in [("undo", s.history.undo_label(), &undo, "Undo"), ("redo", s.history.redo_label(), &redo, "Redo")] {
                if let Some(a) = w.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                    a.set_enabled(label.is_some());
                }
                button.set_tooltip_text(Some(&label.map_or(verb.to_string(), |l| format!("{verb} {l}"))));
            }
        };
        update();
        ctx.on(move |c| {
            if c == Change::Project {
                update();
            }
        });
    }
    crate::musical_typing::attach(ctx, &win);
    install_keys(&win);
    install_poll(&win, ctx);
    install_autosave(ctx);
    update_title(&win, ctx);
    {
        let (win2, ctx2) = (win.clone(), ctx.clone());
        ctx.on(move |c| {
            if c == Change::Project {
                update_title(&win2, &ctx2);
            }
        });
    }
    {
        let ctx = ctx.clone();
        win.connect_close_request(move |w| confirm_close(&ctx, w));
    }
    win
}

fn update_banner(ctx: &Ctx, banner: &adw::Banner) {
    match &ctx.s().io_error {
        Some(e) => {
            banner.set_title(&glib::markup_escape_text(e));
            banner.set_revealed(true);
        }
        None => banner.set_revealed(false),
    }
}

fn update_title(win: &adw::ApplicationWindow, ctx: &Ctx) {
    let s = ctx.s();
    let dirty = if s.history.is_dirty() { "• " } else { "" };
    win.set_title(Some(&format!("{dirty}{} — Solstice", s.project.name)));
}

#[allow(clippy::too_many_arguments)]
fn install_actions(
    win: &adw::ApplicationWindow,
    ctx: &Ctx,
    library: &gtk::Revealer,
    inspector: &gtk::Revealer,
    browser: &gtk::Revealer,
    bottom: &gtk::Box,
    stack: &adw::ViewStack,
    toasts: &adw::ToastOverlay,
    snap_step: std::rc::Rc<dyn Fn() -> u64>,
) {
    let simple = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        win.add_action(&a);
    };
    let c = ctx.clone();
    simple("play", Box::new(move || c.play_pause()));
    let c = ctx.clone();
    simple("stop", Box::new(move || c.stop()));
    let c = ctx.clone();
    simple("record", Box::new(move || c.record()));
    let c = ctx.clone();
    simple("rewind", Box::new(move || c.nudge_bars(-1)));
    let c = ctx.clone();
    simple("forward", Box::new(move || c.nudge_bars(1)));
    let c = ctx.clone();
    simple("goto-start", Box::new(move || c.locate(0)));
    let (c, w) = (ctx.clone(), win.clone());
    simple("musical-typing", Box::new(move || crate::musical_typing::toggle(&c, w.upcast_ref())));
    let c = ctx.clone();
    simple(
        "capture-recording",
        Box::new(move || {
            c.capture_recording();
        }),
    );
    let c = ctx.clone();
    simple("undo", Box::new(move || c.undo()));
    let c = ctx.clone();
    simple("redo", Box::new(move || c.redo()));

    let add_track = |kind: Option<TrackKind>| {
        let c = ctx.clone();
        Box::new(move || {
            let mut new = None;
            c.edit("New Track", |p| {
                new = Some(match kind {
                    Some(k) => p.add_track(k),
                    None => p.add_bus(),
                });
            });
            if kind.is_some() {
                c.select_track(new);
            }
        }) as Box<dyn Fn()>
    };
    simple("add-audio-track", add_track(Some(TrackKind::Audio)));
    simple("add-instrument-track", add_track(Some(TrackKind::Instrument)));
    simple("add-bus", add_track(None));
    let c = ctx.clone();
    simple(
        "add-vca",
        Box::new(move || {
            c.edit("New VCA", |p| {
                p.add_vca();
            })
        }),
    );
    let c = ctx.clone();
    simple(
        "delete-track",
        Box::new(move || {
            let Some(id) = c.s().selected_track else { return };
            c.edit("Delete Track", |p| {
                p.tracks.retain(|t| t.id != id);
                p.tidy_stacks();
            });
        }),
    );
    let c = ctx.clone();
    simple(
        "delete-region",
        Box::new(move || {
            let ids = c.selected_region_ids();
            if ids.is_empty() {
                return;
            }
            c.edit("Delete Region", |p| {
                for id in &ids {
                    p.remove_region(*id);
                }
            });
        }),
    );
    let c = ctx.clone();
    simple("copy", Box::new(move || c.copy_regions(false)));
    let c = ctx.clone();
    simple("cut", Box::new(move || c.copy_regions(true)));
    let c = ctx.clone();
    simple("paste", Box::new(move || c.paste_regions()));
    let c = ctx.clone();
    simple(
        "repeat",
        Box::new(move || {
            let ids = c.selected_region_ids();
            let mut new = Vec::new();
            c.edit("Repeat", |p| new = p.repeat_regions(&ids));
            if !new.is_empty() {
                c.select_regions(new);
            }
        }),
    );
    let c = ctx.clone();
    simple(
        "join",
        Box::new(move || {
            let ids = c.selected_region_ids();
            match c.s().project.clone().join_regions(&ids) {
                Ok(_) => c.edit("Join Regions", |p| {
                    let _ = p.join_regions(&ids);
                }),
                Err(e) => c.message(e),
            }
        }),
    );
    let c = ctx.clone();
    simple(
        "select-all-regions",
        Box::new(move || {
            let ids = c.s().project.tracks.iter().flat_map(|t| t.regions.iter().map(|r| r.id)).collect();
            c.select_regions(ids);
        }),
    );
    let c = ctx.clone();
    simple("deselect-regions", Box::new(move || c.select_regions(Vec::new())));
    for (name, dir) in [("nudge-left", -1i64), ("nudge-right", 1)] {
        let c = ctx.clone();
        let tracks_view = snap_step.clone();
        simple(
            name,
            Box::new(move || {
                let ids = c.selected_region_ids();
                if ids.is_empty() {
                    return;
                }
                let step = tracks_view() as i64 * dir;
                c.edit("Nudge", |p| p.nudge_regions(&ids, step));
            }),
        );
    }
    let c = ctx.clone();
    simple(
        "split-region",
        Box::new(move || {
            // Every selected region under the playhead, or the selected track's region there.
            let pos = c.position();
            let targets: Vec<Id> = {
                let s = c.s();
                let under = |id: &Id| s.project.find_region(*id).is_some_and(|(_, r)| r.start < pos && r.end() > pos);
                let mut ids: Vec<Id> = c.selected_region_ids().into_iter().filter(under).collect();
                if ids.is_empty()
                    && let Some(t) = s.selected_track.and_then(|id| s.project.track(id))
                {
                    ids.extend(t.regions.iter().filter(|r| r.start < pos && r.end() > pos).map(|r| r.id));
                }
                ids
            };
            if targets.is_empty() {
                return;
            }
            c.edit("Split Region", |p| {
                for id in &targets {
                    p.split_region(*id, pos);
                }
            });
        }),
    );

    for (name, get, set) in [
        ("cycle", (|p: &Project| p.cycle_on) as fn(&Project) -> bool, (|p: &mut Project, v| p.cycle_on = v) as fn(&mut Project, bool)),
        ("punch", |p: &Project| p.punch_on, |p: &mut Project, v| p.punch_on = v),
        ("metronome", |p: &Project| p.metronome, |p: &mut Project, v| p.metronome = v),
    ] {
        let a = gio::SimpleAction::new_stateful(name, None, &get(&ctx.s().project).to_variant());
        let c = ctx.clone();
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            c.tweak(|p| set(p, on));
        });
        let (c, a2) = (ctx.clone(), a.clone());
        ctx.on(move |ch| {
            if ch == Change::Project {
                a2.set_state(&get(&c.s().project).to_variant());
            }
        });
        win.add_action(&a);
    }

    let reveal = |name: &str, r: &gtk::Revealer| {
        let a = gio::SimpleAction::new_stateful(name, None, &r.reveals_child().to_variant());
        let r = r.clone();
        a.connect_activate(move |a, _| {
            let on = !r.reveals_child();
            r.set_reveal_child(on);
            a.set_state(&on.to_variant());
        });
        win.add_action(&a);
    };
    reveal("show-library", library);
    reveal("show-inspector", inspector);
    reveal("show-browser", browser);

    let mixer_a = gio::SimpleAction::new_stateful("show-mixer", None, &false.to_variant());
    let editor_a = gio::SimpleAction::new_stateful("show-editor", None, &false.to_variant());
    let sync = {
        let (b, s, m, e) = (bottom.clone(), stack.clone(), mixer_a.clone(), editor_a.clone());
        move || {
            let page = s.visible_child_name();
            let vis = b.is_visible();
            m.set_state(&(vis && page.as_deref() == Some("mixer")).to_variant());
            e.set_state(&(vis && page.as_deref() == Some("editor")).to_variant());
        }
    };
    for (a, page) in [(&mixer_a, "mixer"), (&editor_a, "editor")] {
        let (b, s, sync) = (bottom.clone(), stack.clone(), sync.clone());
        a.connect_activate(move |_, _| {
            if b.is_visible() && s.visible_child_name().as_deref() == Some(page) {
                b.set_visible(false);
            } else {
                b.set_visible(true);
                s.set_visible_child_name(page);
            }
            sync();
        });
        win.add_action(a);
    }
    {
        let sync2 = sync.clone();
        stack.connect_visible_child_name_notify(move |_| sync2());
        bottom.connect_visible_notify(move |_| sync());
    }

    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "save",
        Box::new(move || {
            // A project that was never saved needs a name and place first.
            if !Bundle::is_bundle(&c.s().bundle.dir) {
                save_as_dialog(&c, w.upcast_ref(), None);
                return;
            }
            let r = c.m().save();
            match r {
                Ok(()) => t.add_toast(adw::Toast::new("Project saved")),
                Err(e) => t.add_toast(adw::Toast::new(&format!("Could not save: {e}"))),
            }
            update_title(&w, &c);
        }),
    );
    let (c, w) = (ctx.clone(), win.clone());
    simple("save-as", Box::new(move || save_as_dialog(&c, w.upcast_ref(), None)));
    let w = win.clone();
    simple("close", Box::new(move || w.close()));
    let w = win.clone();
    simple(
        "quit",
        Box::new(move || {
            // Each project window asks about its own unsaved changes.
            if let Some(app) = w.application() {
                for win in app.windows() {
                    win.close();
                }
            }
        }),
    );
    let (c, w) = (ctx.clone(), win.clone());
    simple(
        "revert",
        Box::new(move || {
            let dir = c.s().bundle.dir.clone();
            if !Bundle::is_bundle(&dir) {
                c.m().message = Some("This project hasn't been saved yet".into());
                c.emit(Change::Message);
                return;
            }
            let dialog = adw::AlertDialog::builder()
                .heading("Revert to Saved?")
                .body("Changes since the project was last saved will be lost.")
                .close_response("cancel")
                .default_response("cancel")
                .build();
            dialog.add_responses(&[("cancel", "_Cancel"), ("revert", "_Revert")]);
            dialog.set_response_appearance("revert", adw::ResponseAppearance::Destructive);
            let c = c.clone();
            dialog.connect_response(Some("revert"), move |_, _| {
                if let Err(e) = c.m().open(&dir) {
                    c.m().message = Some(format!("Could not revert: {e}"));
                    c.emit(Change::Message);
                }
                c.emit(Change::Project);
                c.emit(Change::Selection);
            });
            dialog.present(Some(&w));
        }),
    );
    let (c, w) = (ctx.clone(), win.clone());
    simple("project-settings", Box::new(move || project_settings(&c, &w)));
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "open",
        Box::new(move || {
            let (c, t, w2) = (c.clone(), t.clone(), w.clone());
            let d = gtk::FileDialog::builder().title("Open Project").build();
            d.set_initial_folder(Some(&gio::File::for_path(crate::studio::projects_dir())));
            d.select_folder(Some(&w), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(path) = f.path() else { return };
                if !Bundle::is_bundle(&path) {
                    t.add_toast(adw::Toast::new("That folder is not a Solstice project"));
                    return;
                }
                if let Err(e) = crate::projects::open_project(&c, &w2, &path) {
                    t.add_toast(adw::Toast::new(&glib::markup_escape_text(&e)));
                }
            });
        }),
    );
    let c = ctx.clone();
    let w = win.clone();
    simple(
        "new",
        Box::new(move || {
            let Some(app) = w.application().and_downcast::<adw::Application>() else { return };
            let (project, bundle) = crate::studio::untitled(c.s().sample_rate());
            crate::projects::open_window(&app, project, bundle, true, true);
        }),
    );
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "save-template",
        Box::new(move || {
            let entry = adw::EntryRow::builder().title("Name").text(c.s().project.name.as_str()).build();
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.append(&entry);
            let dialog = adw::AlertDialog::builder()
                .heading("Save as Template")
                .body("New projects can start from this one in the Project Chooser, under My Templates.")
                .extra_child(&list)
                .close_response("cancel")
                .default_response("save")
                .build();
            dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
            let (c, t) = (c.clone(), t.clone());
            dialog.connect_response(Some("save"), move |_, _| {
                let name = entry.text().trim().to_string();
                if name.is_empty() {
                    return;
                }
                let res = c.m().save_template(&name);
                t.add_toast(adw::Toast::new(&match res {
                    Ok(()) => format!("Saved template “{}”", glib::markup_escape_text(&name)),
                    Err(e) => format!("Could not save the template: {e}"),
                }));
            });
            dialog.present(Some(&w));
        }),
    );
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "bounce",
        Box::new(move || {
            let (c, t, w2) = (c.clone(), t.clone(), w.clone());
            let d = gtk::FileDialog::builder().title("Bounce Project").initial_name(format!("{}.wav", c.s().project.name)).build();
            d.save(Some(&w), gio::Cancellable::NONE, move |r| {
                let Ok(f) = r else { return };
                let Some(path) = f.path() else { return };
                if c.bounce(path) {
                    crate::render::progress_dialog(&c, &w2);
                } else {
                    t.add_toast(adw::Toast::new("There is nothing to bounce"));
                }
            });
        }),
    );
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "export-tracks",
        Box::new(move || {
            let (c, t, w2) = (c.clone(), t.clone(), w.clone());
            let d = gtk::FileDialog::builder().title("Export All Tracks").build();
            d.select_folder(Some(&w), gio::Cancellable::NONE, move |r| {
                let Some(dir) = r.ok().and_then(|f| f.path()) else { return };
                if c.export_tracks(&dir) > 0 {
                    crate::render::progress_dialog(&c, &w2);
                } else {
                    t.add_toast(adw::Toast::new("There are no tracks with regions to export"));
                }
            });
        }),
    );
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "bounce-in-place",
        Box::new(move || {
            if c.bounce_in_place() {
                crate::render::progress_dialog(&c, &w);
            } else {
                t.add_toast(adw::Toast::new("Select the regions to bounce first"));
            }
        }),
    );
    let (c, w, t) = (ctx.clone(), win.clone(), toasts.clone());
    simple(
        "freeze",
        Box::new(move || {
            let ids: Vec<Id> = {
                let s = c.s();
                let sel = s.selection.iter().chain(&s.selected_track);
                sel.copied().filter(|id| s.project.track(*id).is_some_and(crate::render::freezable)).collect()
            };
            if ids.is_empty() {
                t.add_toast(adw::Toast::new("Select a track with an instrument or plug-ins to freeze"));
            } else {
                crate::render::toggle_freeze(&c, &ids, &w);
            }
        }),
    );
    let (c, w) = (ctx.clone(), win.clone());
    simple("preferences", Box::new(move || prefs::show(&c, &w)));
    let (c, w) = (ctx.clone(), win.clone());
    simple("mixer-window", Box::new(move || mixer::open_window(&c, &w)));
    let w = win.clone();
    simple(
        "about",
        Box::new(move || {
            let about = adw::AboutDialog::builder()
                .application_name("Solstice")
                .application_icon(APP_ID)
                .developer_name("SolarFlare Developments")
                .version(env!("CARGO_PKG_VERSION"))
                .copyright("© 2026 SolarFlare Developments")
                .license_type(gtk::License::Gpl30)
                .build();
            about.present(Some(&w));
        }),
    );

    let app = win.application().unwrap();
    for (action, accels) in [
        ("win.undo", &["<Control>z"][..]),
        ("win.redo", &["<Control><Shift>z"]),
        ("win.save", &["<Control>s"]),
        ("win.close", &["<Control>w"]),
        ("win.quit", &["<Control>q"]),
        ("win.project-settings", &["<Alt>p"]),
        ("win.save-as", &["<Control><Shift>s"]),
        ("win.open", &["<Control>o"]),
        ("win.new", &["<Control>n"]),
        ("win.bounce", &["<Control>b"]),
        ("win.split-region", &["<Control>t"]),
        ("win.delete-track", &["<Control>Delete"]),
        ("win.preferences", &["<Control>comma"]),
        ("win.mixer-window", &["<Control>2"]),
        ("win.musical-typing", &["<Control>k"]),
        ("win.capture-recording", &["<Shift>r"]),
        ("win.add-audio-track", &["<Control><Alt>a"]),
        ("win.add-instrument-track", &["<Control><Alt>s"]),
        ("win.add-bus", &["<Control><Alt>b"]),
    ] {
        app.set_accels_for_action(action, accels);
    }
}

/// Logic's single-key shortcuts. They run in the capture phase so focused buttons don't eat
/// Space, but step aside while a text field has focus.
/// Whether keyboard focus is inside the editor pane (piano roll), which handles its own keys.
fn focus_in_editor(w: &adw::ApplicationWindow) -> bool {
    let mut cur = gtk::prelude::GtkWindowExt::focus(w);
    while let Some(c) = cur {
        if c.widget_name() == "editor-pane" {
            return true;
        }
        cur = c.parent();
    }
    false
}

fn install_keys(win: &adw::ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = win.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        // Dialogs, popovers and text fields keep their own keys: Escape closes, Return confirms.
        let focus = gtk::prelude::GtkWindowExt::focus(&w);
        if w.visible_dialog().is_some()
            || focus.is_some_and(|f| f.is::<gtk::Text>() || f.is::<gtk::Editable>() || f.ancestor(gtk::Popover::static_type()).is_some())
        {
            return glib::Propagation::Proceed;
        }
        let key = key.to_lower();
        let in_editor = focus_in_editor(&w);
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        if ctrl && !mods.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::SHIFT_MASK) {
            // Region editing, as Logic's Cmd shortcuts. Text fields keep their own copy and paste.
            let action = match key {
                _ if in_editor => return glib::Propagation::Proceed,
                gdk::Key::c => "copy",
                gdk::Key::x => "cut",
                gdk::Key::v => "paste",
                gdk::Key::r => "repeat",
                gdk::Key::j => "join",
                gdk::Key::a => "select-all-regions",
                _ => return glib::Propagation::Proceed,
            };
            let _ = WidgetExt::activate_action(&w, &format!("win.{action}"), None);
            return glib::Propagation::Stop;
        }
        if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK) {
            return glib::Propagation::Proceed;
        }
        let region_key = matches!(key, gdk::Key::Delete | gdk::Key::BackSpace | gdk::Key::Left | gdk::Key::Right | gdk::Key::Escape);
        if in_editor && region_key {
            return glib::Propagation::Proceed;
        }
        let action = match key {
            gdk::Key::t => "tool-menu",
            gdk::Key::a => "toggle-automation",
            gdk::Key::g => "show-global",
            gdk::Key::Left => "nudge-left",
            gdk::Key::Right => "nudge-right",
            gdk::Key::Escape => "deselect-regions",
            gdk::Key::space => "play",
            gdk::Key::r => "record",
            gdk::Key::c => "cycle",
            gdk::Key::k => "metronome",
            gdk::Key::x => "show-mixer",
            gdk::Key::e | gdk::Key::p => "show-editor",
            gdk::Key::i => "show-inspector",
            gdk::Key::y => "show-library",
            gdk::Key::f => "show-browser",
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Home => "goto-start",
            gdk::Key::comma => "rewind",
            gdk::Key::period => "forward",
            gdk::Key::Delete | gdk::Key::BackSpace => "delete-region",
            _ => return glib::Propagation::Proceed,
        };
        let _ = WidgetExt::activate_action(&w, &format!("win.{action}"), None);
        glib::Propagation::Stop
    });
    win.add_controller(keys);
}

fn install_import(win: &adw::ApplicationWindow, ctx: &Ctx, toasts: &adw::ToastOverlay) {
    let notify: crate::logic_import::Notify = {
        let t = toasts.clone();
        std::rc::Rc::new(move |msg: String| {
            let toast = adw::Toast::new(&glib::markup_escape_text(&msg));
            toast.set_timeout(8);
            t.add_toast(toast);
        })
    };
    // A .logicx project (and a .fcpxmld export) is a folder, so it needs the folder picker.
    let a = gio::SimpleAction::new("import-logic", None);
    {
        let (ctx, w, notify) = (ctx.clone(), win.clone(), notify.clone());
        a.connect_activate(move |_, _| {
            let (ctx, notify) = (ctx.clone(), notify.clone());
            let d = gtk::FileDialog::builder().title("Import Logic Project (.logicx) or Final Cut Pro XML Bundle").build();
            d.select_folder(Some(&w), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                {
                    crate::logic_import::import_path(&ctx, path, notify.clone());
                }
            });
        });
    }
    win.add_action(&a);
    let a = gio::SimpleAction::new("import-file", None);
    {
        let (ctx, w) = (ctx.clone(), win.clone());
        a.connect_activate(move |_, _| {
            let (ctx, notify) = (ctx.clone(), notify.clone());
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Final Cut Pro XML and MIDI files"));
            for p in ["*.fcpxml", "*.xml", "*.mid", "*.midi", "*.smf"] {
                filter.add_pattern(p);
            }
            let filters = gio::ListStore::new::<gtk::FileFilter>();
            filters.append(&filter);
            let d = gtk::FileDialog::builder().title("Import Final Cut Pro XML or MIDI File").filters(&filters).build();
            d.open(Some(&w), gio::Cancellable::NONE, move |r| {
                if let Ok(f) = r
                    && let Some(path) = f.path()
                {
                    crate::logic_import::import_path(&ctx, path, notify.clone());
                }
            });
        });
    }
    win.add_action(&a);
}

/// Writes the crash-recovery copy on the interval chosen in Preferences.
fn install_autosave(ctx: &Ctx) {
    let ctx = ctx.clone();
    let last = std::cell::Cell::new(std::time::Instant::now());
    glib::timeout_add_seconds_local(15, move || {
        let minutes = ctx.s().settings.autosave_minutes;
        if minutes > 0 && last.get().elapsed().as_secs() >= minutes as u64 * 60 {
            last.set(std::time::Instant::now());
            ctx.m().autosave();
        }
        glib::ControlFlow::Continue
    });
}

/// Offers the autosaved copy when it is newer than the saved project, as after a crash.
pub fn offer_recovery(ctx: &Ctx, win: &impl IsA<gtk::Widget>) {
    let Some(recovered) = ctx.s().bundle.recoverable() else { return };
    let dialog = adw::AlertDialog::builder()
        .heading("Recover Unsaved Changes?")
        .body(format!(
            "“{}” has changes from an autosave that were never saved, probably because Solstice closed unexpectedly.",
            ctx.s().project.name
        ))
        .close_response("discard")
        .default_response("recover")
        .build();
    dialog.add_responses(&[("discard", "_Discard"), ("recover", "_Recover")]);
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("recover", adw::ResponseAppearance::Suggested);
    let ctx = ctx.clone();
    dialog.choose(Some(win), gio::Cancellable::NONE, move |r| {
        if r == "recover" {
            {
                let mut s = ctx.m();
                let s = &mut *s;
                s.history.push("Recover Autosave", &s.project);
                s.project = recovered;
                s.sync();
            }
            ctx.emit(Change::Project);
        } else {
            ctx.s().bundle.clear_autosave();
        }
    });
}

fn install_poll(win: &adw::ApplicationWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    let frame = std::cell::Cell::new(0u32);
    win.add_tick_callback(move |_, _| {
        ctx.poll();
        ctx.m().update_levels();
        frame.set(frame.get().wrapping_add(1));
        if frame.get() % 15 == 0 && ctx.playing() {
            let pos = ctx.position();
            ctx.m().prefetch(pos);
        }
        let requests = ctx.m().take_peak_requests();
        for (file, data) in requests {
            let ctx = ctx.clone();
            glib::spawn_future_local(async move {
                if let Ok(p) = gio::spawn_blocking(move || Peaks::build(&data)).await {
                    ctx.m().add_peaks(file, p);
                    ctx.emit(Change::Peaks);
                }
            });
        }
        glib::ControlFlow::Continue
    });
}

/// Asks where to save the project, saves it there, then runs `then`.
pub fn save_as_dialog(ctx: &Ctx, parent: &gtk::Window, then: Option<Box<dyn FnOnce()>>) {
    if ctx.refuse_while_rendering() {
        return;
    }
    let name = format!("{}.solstice", ctx.s().project.name);
    let d = gtk::FileDialog::builder().title("Save Project").initial_name(name).build();
    d.set_initial_folder(Some(&gio::File::for_path(crate::studio::projects_dir())));
    let c = ctx.clone();
    d.save(Some(parent), gio::Cancellable::NONE, move |r| {
        let Ok(f) = r else { return };
        let Some(mut path) = f.path() else { return };
        if path.extension().is_none_or(|e| e != "solstice") {
            path.set_extension("solstice");
        }
        let res = c.m().save_as(&path);
        c.m().message = Some(match &res {
            Ok(()) => "Project saved".into(),
            Err(e) => format!("Could not save: {e}"),
        });
        c.emit(Change::Message);
        c.emit(Change::Project);
        if res.is_ok()
            && let Some(then) = then
        {
            then();
        }
    });
}

/// Logic's Project Settings: tempo, time signature and key at the start of the song.
fn project_settings(ctx: &Ctx, win: &adw::ApplicationWindow) {
    let (tempo, num, den, key, rate, name) = {
        let p = &ctx.s().project;
        let key = p.key_changes.iter().find(|k| k.bar == 0).map(|k| k.key.clone());
        (p.tempo, p.sig_num, p.sig_den, key, ctx.s().sample_rate(), p.name.clone())
    };
    let dialog = adw::PreferencesDialog::builder().title("Project Settings").build();
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&name)).description(format!("Sample rate {} Hz, set by the audio server", rate)).build();
    let t = adw::SpinRow::with_range(30.0, 300.0, 0.1);
    t.set_title("Tempo");
    t.set_digits(1);
    t.set_value(tempo);
    group.add(&t);
    let n = adw::SpinRow::with_range(1.0, 32.0, 1.0);
    n.set_title("Beats per Bar");
    n.set_value(num as f64);
    group.add(&n);
    const DENS: [u8; 4] = [2, 4, 8, 16];
    let d = adw::ComboRow::builder().title("Beat Value").model(&gtk::StringList::new(&["2", "4", "8", "16"])).build();
    d.set_selected(DENS.iter().position(|x| *x == den).unwrap_or(1) as u32);
    group.add(&d);
    let mut keys = vec!["No Key"];
    keys.extend(crate::global::KEYS);
    let k = adw::ComboRow::builder().title("Key").model(&gtk::StringList::new(&keys)).build();
    k.set_selected(key.as_ref().and_then(|key| crate::global::KEYS.iter().position(|x| x == key)).map_or(0, |i| i as u32 + 1));
    group.add(&k);
    page.add(&group);
    dialog.add(&page);
    let ctx = ctx.clone();
    dialog.connect_closed(move |_| {
        let (tempo, num, den) = (t.value(), n.value() as u8, DENS[d.selected() as usize]);
        let new_key = (k.selected() > 0).then(|| crate::global::KEYS[k.selected() as usize - 1].to_string());
        let changed = {
            let p = &ctx.s().project;
            (p.tempo - tempo).abs() > 1e-6 || p.sig_num != num || p.sig_den != den || new_key != key
        };
        if changed {
            ctx.edit("Project Settings", |p| {
                let old = p.time();
                (p.tempo, p.sig_num, p.sig_den) = (tempo, num, den);
                p.retime(&old);
                p.key_changes.retain(|k| k.bar != 0);
                if let Some(key) = &new_key {
                    p.key_changes.insert(0, KeyChange { bar: 0, key: key.clone() });
                }
            });
        }
    });
    dialog.present(Some(win));
}

/// Runs `then` once unsaved changes are saved or discarded, or straight away if there are none.
pub fn confirm_discard(ctx: &Ctx, parent: &impl IsA<gtk::Widget>, then: impl FnOnce() + 'static) {
    if !ctx.s().history.is_dirty() {
        then();
        return;
    }
    let dialog = adw::AlertDialog::builder()
        .heading("Save Changes?")
        .body(format!("Unsaved changes to “{}” will be lost.", ctx.s().project.name))
        .close_response("cancel")
        .default_response("save")
        .build();
    dialog.add_responses(&[("cancel", "_Cancel"), ("discard", "_Discard"), ("save", "_Save")]);
    dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    let ctx = ctx.clone();
    let parent_window = parent.root().and_downcast::<gtk::Window>();
    dialog.choose(Some(parent), gio::Cancellable::NONE, move |r| {
        match r.as_str() {
            "save" if !Bundle::is_bundle(&ctx.s().bundle.dir) => {
                if let Some(win) = parent_window {
                    save_as_dialog(&ctx, &win, Some(Box::new(then)));
                }
                return;
            }
            "save" => {
                if let Err(e) = ctx.m().save() {
                    ctx.m().message = Some(format!("Could not save: {e}"));
                    ctx.emit(Change::Message);
                    return;
                }
            }
            "discard" => ctx.m().history.mark_saved(),
            _ => return,
        }
        then();
    });
}

fn confirm_close(ctx: &Ctx, win: &adw::ApplicationWindow) -> glib::Propagation {
    if !ctx.s().history.is_dirty() {
        ctx.m().cleanup_unused_bundle();
        return glib::Propagation::Proceed;
    }
    let w = win.clone();
    confirm_discard(ctx, win, move || w.close());
    glib::Propagation::Stop
}

fn shortcuts_dialog() -> adw::ShortcutsDialog {
    let dialog = adw::ShortcutsDialog::new();
    let sections: [(&str, &[(&str, &str)]); 6] = [
        (
            "Transport",
            &[
                ("Play or Stop", "space"),
                ("Record", "r"),
                ("Cycle", "c"),
                ("Metronome", "k"),
                ("Go to Start", "Return"),
                ("Back a Bar", "comma"),
                ("Forward a Bar", "period"),
                ("Capture as Recording", "<Shift>r"),
            ],
        ),
        (
            "Views",
            &[
                ("Mixer", "x"),
                ("Editor", "e"),
                ("Inspector", "i"),
                ("Library", "y"),
                ("Browser", "f"),
                ("Global Tracks", "g"),
                ("Automation", "a"),
                ("Mixer Window", "<Control>2"),
                ("Musical Typing", "<Control>k"),
            ],
        ),
        (
            "Regions",
            &[
                ("Tools", "t"),
                ("Copy", "<Control>c"),
                ("Cut", "<Control>x"),
                ("Paste at Playhead", "<Control>v"),
                ("Repeat", "<Control>r"),
                ("Join", "<Control>j"),
                ("Select All", "<Control>a"),
                ("Split at Playhead", "<Control>t"),
                ("Nudge", "Left Right"),
                ("Delete", "Delete"),
            ],
        ),
        (
            "Piano Roll",
            &[
                ("Quantize", "q"),
                ("Transpose", "Up Down"),
                ("Transpose an Octave", "<Shift>Up <Shift>Down"),
                ("Move by the Grid", "Left Right"),
                ("Duplicate", "<Control>d"),
            ],
        ),
        (
            "Tracks",
            &[
                ("New Audio Track", "<Control><Alt>a"),
                ("New Instrument Track", "<Control><Alt>s"),
                ("New Aux Bus", "<Control><Alt>b"),
                ("Delete Track", "<Control>Delete"),
            ],
        ),
        (
            "Project",
            &[
                ("New", "<Control>n"),
                ("Open", "<Control>o"),
                ("Save", "<Control>s"),
                ("Save As", "<Control><Shift>s"),
                ("Bounce", "<Control>b"),
                ("Undo", "<Control>z"),
                ("Redo", "<Control><Shift>z"),
                ("Preferences", "<Control>comma"),
            ],
        ),
    ];
    for (title, items) in sections {
        let section = adw::ShortcutsSection::new(Some(title));
        for (name, accel) in items {
            section.add(adw::ShortcutsItem::new(name, accel));
        }
        dialog.add(section);
    }
    dialog
}
