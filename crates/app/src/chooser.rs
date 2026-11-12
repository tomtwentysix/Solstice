//! Logic's Project Chooser: new projects from templates, the user's own templates and recent
//! projects, with the details a new project starts with.

use crate::studio::{Change, Ctx};
use crate::templates::{self, Details, TEMPLATES};
use adw::prelude::*;
use gtk::glib;
use std::path::PathBuf;
use std::cell::Cell;
use std::rc::Rc;

const SIGNATURES: [(u8, u8); 6] = [(4, 4), (3, 4), (6, 8), (5, 4), (7, 8), (12, 8)];

/// A grid of cards, one chosen at a time, double-click to go straight ahead. Returns the grid
/// and the chosen index.
fn cards(items: &[(&str, &str, &str)], on_select: impl Fn(usize) + 'static, on_activate: Rc<dyn Fn(usize)>) -> (gtk::Grid, Rc<Cell<usize>>) {
    let grid = gtk::Grid::builder().row_spacing(12).column_spacing(12).column_homogeneous(true).valign(gtk::Align::Start).build();
    grid.set_margin_top(18);
    grid.set_margin_bottom(18);
    grid.set_margin_start(18);
    grid.set_margin_end(18);
    let chosen = Rc::new(Cell::new(0));
    let on_select = Rc::new(on_select);
    let mut first: Option<gtk::ToggleButton> = None;
    for (i, (icon, name, description)) in items.iter().enumerate() {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
        col.set_margin_top(12);
        col.set_margin_bottom(12);
        let image = gtk::Image::from_icon_name(icon);
        image.set_pixel_size(40);
        col.append(&image);
        let title = gtk::Label::new(Some(name));
        title.add_css_class("heading");
        col.append(&title);
        let desc = gtk::Label::new(Some(description));
        desc.add_css_class("caption");
        desc.add_css_class("dim-label");
        desc.set_wrap(true);
        desc.set_justify(gtk::Justification::Center);
        desc.set_max_width_chars(22);
        col.append(&desc);
        let b = gtk::ToggleButton::builder().child(&col).active(i == 0).build();
        b.set_group(first.as_ref());
        {
            let (chosen, on_select) = (chosen.clone(), on_select.clone());
            b.connect_toggled(move |b| {
                if b.is_active() {
                    chosen.set(i);
                    on_select(i);
                }
            });
        }
        let twice = gtk::GestureClick::new();
        twice.set_propagation_phase(gtk::PropagationPhase::Capture);
        {
            let on_activate = on_activate.clone();
            twice.connect_released(move |_, n, _, _| {
                if n == 2 {
                    on_activate(i);
                }
            });
        }
        b.add_controller(twice);
        grid.attach(&b, (i % 3) as i32, (i / 3) as i32, 1, 1);
        first.get_or_insert(b);
    }
    (grid, chosen)
}

fn labelled(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("caption-heading");
    l.add_css_class("dim-label");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(w);
    b
}

fn project_name(dir: &std::path::Path) -> String {
    dir.file_stem().map_or_else(|| dir.display().to_string(), |s| s.to_string_lossy().into_owned())
}

/// Shows the chooser over a project window. On choosing, that window's project is replaced, once
/// any unsaved changes have been saved or discarded. With `close_if_cancelled`, dismissing the
/// chooser closes the window, as for File ▸ New.
pub fn present(ctx: &Ctx, win: &adw::ApplicationWindow, close_if_cancelled: bool) {
    let dialog = adw::Dialog::builder().title("Choose a Project").content_width(820).content_height(580).build();

    let sidebar = gtk::ListBox::new();
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_width_request(170);
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_hexpand(true);
    let add_page = |name: &str, title: &str, icon: &str, page: &gtk::Widget| {
        stack.add_named(page, Some(name));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.set_margin_top(6);
        row.set_margin_bottom(6);
        row.append(&gtk::Image::from_icon_name(icon));
        row.append(&gtk::Label::new(Some(title)));
        let r = gtk::ListBoxRow::new();
        r.set_child(Some(&row));
        r.set_widget_name(name);
        sidebar.append(&r);
    };

    let tempo = gtk::SpinButton::with_range(30.0, 300.0, 1.0);
    tempo.set_digits(1);
    tempo.set_value(TEMPLATES[0].tempo);
    let sig_names: Vec<String> = SIGNATURES.iter().map(|(n, d)| format!("{n}/{d}")).collect();
    let sig = gtk::DropDown::from_strings(&sig_names.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let mut keys = vec!["No Key"];
    keys.extend(crate::global::KEYS);
    let key = gtk::DropDown::from_strings(&keys);
    let audio = gtk::SpinButton::with_range(0.0, 32.0, 1.0);
    audio.set_value(1.0);
    let inst = gtk::SpinButton::with_range(0.0, 32.0, 1.0);
    inst.set_value(1.0);
    let user = templates::user_templates();
    let recent: Vec<PathBuf> = ctx.s().settings.recent_list.iter().filter(|d| solstice_model::Bundle::is_bundle(d)).cloned().collect();

    let chosen = Rc::new(Cell::new(false));
    if close_if_cancelled {
        let (chosen, win) = (chosen.clone(), win.clone());
        dialog.connect_closed(move |_| {
            if !chosen.get() {
                win.close();
            }
        });
    }

    // Replaces the project once unsaved changes are dealt with.
    let replace: Rc<dyn Fn(Box<dyn FnOnce(&Ctx) -> Result<(), String>>)> = {
        let (ctx, win, dialog, chosen) = (ctx.clone(), win.clone(), dialog.clone(), chosen.clone());
        Rc::new(move |f| {
            let (ctx2, win2, dialog, chosen) = (ctx.clone(), win.clone(), dialog.clone(), chosen.clone());
            crate::window::confirm_discard(&ctx, &win, move || {
                chosen.set(true);
                dialog.close();
                match f(&ctx2) {
                    Ok(()) => {
                        ctx2.emit(Change::Project);
                        ctx2.emit(Change::Selection);
                        crate::window::offer_recovery(&ctx2, &win2);
                    }
                    Err(e) => {
                        ctx2.m().message = Some(e);
                        ctx2.emit(Change::Message);
                    }
                }
            });
        })
    };
    let open_dir: Rc<dyn Fn(PathBuf)> = {
        let (replace, dialog) = (replace.clone(), dialog.clone());
        Rc::new(move |dir: PathBuf| {
            // Already open in another window: go there instead of opening it twice.
            if let Some(w) = crate::projects::window_for(&dir) {
                dialog.close();
                w.present();
                return;
            }
            replace(Box::new(move |ctx| ctx.m().open(&dir).map_err(|e| format!("Could not open: {e}"))))
        })
    };
    let new_from_template: Rc<dyn Fn(usize)> = {
        let (replace, tempo, sig, key, audio, inst) = (replace.clone(), tempo.clone(), sig.clone(), key.clone(), audio.clone(), inst.clone());
        Rc::new(move |i: usize| {
            let details = Details {
                tempo: tempo.value(),
                sig: SIGNATURES[sig.selected() as usize],
                key: (key.selected() > 0).then(|| crate::global::KEYS[key.selected() as usize - 1].to_string()),
                audio_tracks: audio.value() as u32,
                instrument_tracks: inst.value() as u32,
            };
            replace(Box::new(move |ctx| {
                let sr = ctx.s().sample_rate();
                let (p, b) = templates::build(&TEMPLATES[i], &details, sr);
                ctx.m().replace_project(p, b);
                Ok(())
            }))
        })
    };
    let new_from_user: Rc<dyn Fn(usize)> = {
        let (replace, user) = (replace.clone(), user.clone());
        Rc::new(move |i: usize| {
            let path = user[i].1.clone();
            replace(Box::new(move |ctx| {
                let (p, b) = templates::from_user(&path).map_err(|e| format!("Could not use the template: {e}"))?;
                ctx.m().replace_project(p, b);
                Ok(())
            }))
        })
    };

    // New Project: Solstice's templates and the details they start with.
    let counts = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    counts.append(&labelled("Audio Tracks", &audio));
    counts.append(&labelled("Instrument Tracks", &inst));
    let items: Vec<(&str, &str, &str)> = TEMPLATES.iter().map(|t| (t.icon, t.name, t.description)).collect();
    let (new_grid, new_chosen) = {
        let (tempo, counts) = (tempo.clone(), counts.clone());
        cards(
            &items,
            move |i| {
                tempo.set_value(TEMPLATES[i].tempo);
                counts.set_visible(i == 0);
            },
            new_from_template.clone(),
        )
    };
    let details = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    details.set_margin_start(18);
    details.set_margin_end(18);
    details.set_margin_top(12);
    details.set_margin_bottom(12);
    details.append(&labelled("Tempo", &tempo));
    details.append(&labelled("Time Signature", &sig));
    details.append(&labelled("Key", &key));
    details.append(&counts);
    let new_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    new_page.append(&gtk::ScrolledWindow::builder().child(&new_grid).vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).build());
    new_page.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    new_page.append(&details);
    add_page("new", "New Project", "document-new-symbolic", new_page.upcast_ref());

    // My Templates: projects saved with File ▸ Save as Template.
    let items: Vec<(&str, &str, &str)> = user.iter().map(|(name, _)| ("folder-templates-symbolic", name.as_str(), "Your template")).collect();
    let (user_grid, user_chosen) = cards(&items, |_| {}, new_from_user.clone());
    if !user.is_empty() {
        let scroller = gtk::ScrolledWindow::builder().child(&user_grid).hscrollbar_policy(gtk::PolicyType::Never).build();
        add_page("mine", "My Templates", "folder-templates-symbolic", scroller.upcast_ref());
    }

    // Recent projects.
    let recent_list = gtk::ListBox::new();
    recent_list.add_css_class("boxed-list");
    recent_list.set_valign(gtk::Align::Start);
    recent_list.set_margin_top(18);
    recent_list.set_margin_bottom(18);
    recent_list.set_margin_start(18);
    recent_list.set_margin_end(18);
    let home = glib::home_dir();
    for dir in &recent {
        let shown = dir.strip_prefix(&home).map_or_else(|_| dir.display().to_string(), |p| format!("~/{}", p.display()));
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&project_name(dir))).subtitle(glib::markup_escape_text(&shown)).activatable(true).build();
        if let Some(when) = std::fs::metadata(dir.join("project.json"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| glib::DateTime::from_unix_local(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64).ok())
            .and_then(|d| d.format("%-d %b %Y, %H:%M").ok())
        {
            let l = gtk::Label::new(Some(&when));
            l.add_css_class("dim-label");
            row.add_suffix(&l);
        }
        recent_list.append(&row);
    }
    if recent.is_empty() {
        let empty = adw::StatusPage::builder().icon_name("document-open-recent-symbolic").title("No Recent Projects").build();
        add_page("recent", "Recent", "document-open-recent-symbolic", empty.upcast_ref());
    } else {
        let scroller = gtk::ScrolledWindow::builder().child(&recent_list).hscrollbar_policy(gtk::PolicyType::Never).build();
        add_page("recent", "Recent", "document-open-recent-symbolic", scroller.upcast_ref());
    }

    let choose = gtk::Button::with_label("Choose");
    choose.add_css_class("suggested-action");
    let open = gtk::Button::with_label("Open…");
    open.set_tooltip_text(Some("Open an existing project"));
    let last = ctx.s().settings.recent.clone().filter(|p| solstice_model::Bundle::is_bundle(p));
    let reopen = gtk::Button::with_label(&match &last {
        Some(p) => format!("Reopen “{}”", project_name(p)),
        None => "Reopen Last Project".into(),
    });
    reopen.set_sensitive(last.is_some());

    {
        let (f, recent) = (open_dir.clone(), recent.clone());
        recent_list.connect_row_activated(move |_, r| f(recent[r.index() as usize].clone()));
    }
    {
        let (stack, recent_list, recent) = (stack.clone(), recent_list.clone(), recent.clone());
        let open_dir = open_dir.clone();
        choose.connect_clicked(move |_| match stack.visible_child_name().as_deref() {
            Some("new") => new_from_template(new_chosen.get()),
            Some("mine") => new_from_user(user_chosen.get()),
            Some("recent") => {
                if let Some(r) = recent_list.selected_row() {
                    open_dir(recent[r.index() as usize].clone());
                }
            }
            _ => {}
        });
    }
    {
        let open_dir = open_dir.clone();
        reopen.connect_clicked(move |_| {
            if let Some(p) = last.clone() {
                open_dir(p);
            }
        });
    }
    {
        let (win, dialog, chosen) = (win.clone(), dialog.clone(), chosen.clone());
        open.connect_clicked(move |_| {
            chosen.set(true);
            dialog.close();
            let _ = WidgetExt::activate_action(&win, "win.open", None);
        });
    }
    {
        let (stack, choose) = (stack.clone(), choose.clone());
        sidebar.connect_row_selected(move |_, r| {
            if let Some(r) = r {
                stack.set_visible_child_name(&r.widget_name());
                choose.set_label(if r.widget_name() == "recent" { "Open" } else { "Choose" });
            }
        });
    }
    sidebar.select_row(sidebar.row_at_index(0).as_ref());

    let split = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    split.append(&gtk::ScrolledWindow::builder().child(&sidebar).hscrollbar_policy(gtk::PolicyType::Never).build());
    split.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    split.append(&stack);

    let bottom = gtk::CenterBox::new();
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(6);
    bottom.set_margin_start(6);
    bottom.set_margin_end(6);
    let left = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    left.append(&open);
    left.append(&reopen);
    bottom.set_start_widget(Some(&left));
    bottom.set_end_widget(Some(&choose));

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&split));
    tv.add_bottom_bar(&bottom);
    dialog.set_child(Some(&tv));
    dialog.set_default_widget(Some(&choose));
    dialog.present(Some(win));
}
