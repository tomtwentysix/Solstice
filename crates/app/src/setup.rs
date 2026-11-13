//! The first-run setup: realtime priority, the audio interface, save folders, the plug-ins
//! Solstice can find, and yabridge for Windows plug-ins.

use crate::elevate::{self, Outcome};
use crate::plugin_search;
use crate::prefs::{self, info_row, set};
use crate::settings::Settings;
use crate::studio::Ctx;
use crate::system::{self, RtLimit, Verdict};
use crate::yabridge;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub const SETUP_VERSION: u32 = 1;

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum Page {
    Realtime,
    Interface,
    Folders,
    Plugins,
    Windows,
}

const TITLES: [&str; 5] = ["Realtime Priority", "Audio Interface", "Folders", "Plug-ins", "Windows Plug-ins"];

#[derive(Default)]
struct Shared {
    /// Windows plug-in folders the Plug-ins page found; `None` until it has searched.
    windows_dirs: RefCell<Option<Vec<PathBuf>>>,
    searched: Cell<bool>,
    detected: Cell<bool>,
}

#[allow(dead_code)]
pub fn present(ctx: &Ctx, win: &adw::ApplicationWindow, start: Page, closed: impl FnOnce() + 'static) {
    let dialog = adw::Dialog::builder().title("Set Up Solstice").content_width(640).content_height(620).build();
    let shared = Rc::new(Shared::default());

    let (plugins, plugins_body) = page();
    let (windows, windows_body) = page();
    let pages: [gtk::Widget; 5] =
        [realtime_page(), interface_page(ctx), folders_page(ctx, &dialog), plugins.upcast(), windows.upcast()];
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    for p in &pages {
        stack.add_child(p);
    }

    let title = adw::WindowTitle::new("", "");
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let back = gtk::Button::with_label("Back");
    let next = gtk::Button::with_label("Next");
    next.add_css_class("suggested-action");
    let done = gtk::Button::with_label("Done");
    done.add_css_class("suggested-action");

    let current = Rc::new(Cell::new(0usize));
    let show: Rc<dyn Fn(usize)> = {
        let (ctx, shared, current) = (ctx.clone(), shared.clone(), current.clone());
        let (stack, title, back, next, done) = (stack.clone(), title.clone(), back.clone(), next.clone(), done.clone());
        Rc::new(move |i| {
            current.set(i);
            stack.set_visible_child(&pages[i]);
            title.set_title(TITLES[i]);
            title.set_subtitle(&format!("Step {} of {}", i + 1, TITLES.len()));
            back.set_sensitive(i > 0);
            next.set_visible(i + 1 < TITLES.len());
            done.set_visible(i + 1 == TITLES.len());
            if i == Page::Plugins as usize && !shared.searched.get() {
                search(&ctx, &shared, &plugins_body);
            }
            if i == Page::Windows as usize && !shared.detected.get() {
                detect(&ctx, &shared, &windows_body);
            }
        })
    };
    {
        let (show, current) = (show.clone(), current.clone());
        back.connect_clicked(move |_| show(current.get().saturating_sub(1)));
    }
    {
        let (show, current) = (show.clone(), current.clone());
        next.connect_clicked(move |_| show(current.get() + 1));
    }
    {
        let (ctx, dialog) = (ctx.clone(), dialog.clone());
        done.connect_clicked(move |_| {
            set(&ctx, false, |s| s.setup_version = SETUP_VERSION);
            dialog.close();
        });
    }
    let closed = Cell::new(Some(closed));
    dialog.connect_closed(move |_| {
        if let Some(f) = closed.take() {
            f();
        }
    });

    let bottom = gtk::CenterBox::new();
    bottom.set_margin_top(6);
    bottom.set_margin_bottom(6);
    bottom.set_margin_start(6);
    bottom.set_margin_end(6);
    bottom.set_start_widget(Some(&back));
    let end = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    end.append(&next);
    end.append(&done);
    bottom.set_end_widget(Some(&end));

    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&stack));
    tv.add_bottom_bar(&bottom);
    dialog.set_child(Some(&tv));
    show(start as usize);
    crate::dialogs::clear_way(&dialog, win);
    dialog.present(Some(win));
}

fn page() -> (gtk::ScrolledWindow, gtk::Box) {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 24);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    body.set_margin_start(12);
    body.set_margin_end(12);
    let clamp = adw::Clamp::builder().maximum_size(600).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().child(&clamp).hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).build();
    (scroller, body)
}

fn clear(body: &gtk::Box) {
    while let Some(c) = body.first_child() {
        body.remove(&c);
    }
}

fn group(title: &str, description: &str) -> adw::PreferencesGroup {
    let g = adw::PreferencesGroup::builder().title(title).build();
    if !description.is_empty() {
        g.set_description(Some(&glib::markup_escape_text(description)));
    }
    g
}

fn busy(text: &str) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.set_margin_top(48);
    let spinner = adw::Spinner::new();
    spinner.set_size_request(32, 32);
    b.append(&spinner);
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    b.append(&l);
    b
}

fn status_row(title: &str, subtitle: &str, icon: Option<(&str, &str)>) -> adw::ActionRow {
    let row = info_row(title, subtitle);
    if let Some((name, class)) = icon {
        let image = gtk::Image::from_icon_name(name);
        image.add_css_class(class);
        row.add_prefix(&image);
    }
    row
}

fn tick_row(title: &str, subtitle: &str) -> adw::ActionRow {
    status_row(title, subtitle, Some(("object-select-symbolic", "success")))
}

fn warning_row(title: &str, subtitle: &str) -> adw::ActionRow {
    status_row(title, subtitle, Some(("dialog-warning-symbolic", "warning")))
}

fn command_row(command: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title("Run This in a Terminal")
        .subtitle(glib::markup_escape_text(command))
        .subtitle_selectable(true)
        .build();
    row.add_css_class("property");
    row.add_css_class("monospace");
    row
}

fn shown(p: &Path) -> String {
    match p.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn realtime_page() -> gtk::Widget {
    let (scroller, body) = page();
    let limit = system::realtime_limit();
    if system::realtime_ok(limit) {
        let g = group("Realtime Priority", "Solstice and the plug-ins it hosts can run their audio threads ahead of other programs.");
        let value = match limit {
            Some(RtLimit::Limit(n)) => format!("Allowed (priority {n})"),
            _ => "Allowed (unlimited)".into(),
        };
        g.add(&tick_row("Realtime Priority", &value));
        body.append(&g);
        return scroller.upcast();
    }

    let g = group(
        "Realtime Priority",
        "Your account is not allowed realtime priority, so audio threads wait behind other programs. At small buffer sizes plug-ins then finish late and the audio crackles. Bridged Windows plug-ins are affected most.",
    );
    g.add(&warning_row("Realtime Priority", if limit.is_some() { "Not allowed" } else { "Could not be read" }));
    body.append(&g);

    let Some((name, priority)) = system::rt_group_on_system() else {
        body.append(&group(
            "Fix",
            "No group on this system is granted realtime priority in /etc/security/limits.conf or /etc/security/limits.d. Your distribution's documentation on realtime audio or the audio group explains how to set one up.",
        ));
        return scroller.upcast();
    };
    let user = glib::user_name().to_string_lossy().into_owned();
    let fix = group(
        "Fix",
        &format!("This system grants realtime priority {priority} to members of the “{name}” group. Joining it takes effect after you log out and back in."),
    );
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&format!("Join the “{name}” Group"))).build();
    fix.add(&row);
    let command = command_row(&elevate::group_command(&name));
    command.set_visible(false);
    fix.add(&command);
    body.append(&fix);
    if std::fs::read_to_string("/etc/group").is_ok_and(|t| member(&t, &name, &user)) {
        row.set_subtitle("You are already in this group. Log out and back in to finish.");
        return scroller.upcast();
    }
    row.set_subtitle("Solstice asks through your desktop's authentication prompt");
    let button = gtk::Button::with_label("Fix");
    button.add_css_class("suggested-action");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);
    button.connect_clicked(move |b| {
        let Some(argv) = elevate::group_argv(&name, &user) else {
            row.set_subtitle("Solstice cannot run this for your account name. Run the command below instead.");
            b.set_visible(false);
            command.set_visible(true);
            return;
        };
        b.set_sensitive(false);
        row.set_subtitle("Waiting for authentication…");
        let (b, row, command) = (b.clone(), row.clone(), command.clone());
        elevate::run(argv, move |outcome| {
            match outcome {
                Outcome::Done => {
                    row.set_subtitle("Log out and back in to finish.");
                    b.set_visible(false);
                    return;
                }
                Outcome::Cancelled => row.set_subtitle("Cancelled. You can run the command below instead."),
                Outcome::Failed(e) => row.set_subtitle(&glib::markup_escape_text(&format!("The change failed: {}. You can run the command below instead.", e.trim_end_matches('.')))),
            }
            b.set_sensitive(true);
            command.set_visible(true);
        });
    });
    scroller.upcast()
}

// /etc/group lines are `name:password:gid:member,member`.
fn member(etc_group: &str, group: &str, user: &str) -> bool {
    etc_group.lines().any(|l| {
        let f: Vec<&str> = l.split(':').collect();
        f.len() == 4 && f[0] == group && f[3].split(',').any(|m| m.trim() == user)
    })
}

fn interface_page(ctx: &Ctx) -> gtk::Widget {
    let (scroller, body) = page();
    let g = group(
        "Audio Interface",
        "Choose the interface you record and monitor through. Solstice connects only to it, so other sound cards keep their own clock and cannot hold back a small buffer.",
    );
    if ctx.s().io.is_none() {
        g.set_description(Some("Audio engine not running. Once the audio server is running, these settings are in Preferences ▸ Audio."));
    } else {
        g.add(&prefs::interface_row(ctx, || {}));
        g.add(&prefs::buffer_row(ctx, |_| {}));
    }
    body.append(&g);
    scroller.upcast()
}

fn folders_page(ctx: &Ctx, dialog: &adw::Dialog) -> gtk::Widget {
    let (scroller, body) = page();
    let g = group(
        "Folders",
        "New projects are saved in the projects folder, and mixes and stems are offered to be exported to the exports folder. Captures, impulse responses, presets and templates stay in Solstice's own library.",
    );
    let projects = info_row("Projects", "");
    let exports = info_row("Exports", "");
    let refresh: Rc<dyn Fn()> = {
        let (ctx, projects, exports) = (ctx.clone(), projects.clone(), exports.clone());
        Rc::new(move || {
            let (p, e) = {
                let s = ctx.s();
                (s.settings.projects_dir(), s.settings.exports_dir())
            };
            projects.set_subtitle(&glib::markup_escape_text(&shown(&p)));
            exports.set_subtitle(&glib::markup_escape_text(&shown(&e)));
        })
    };
    refresh();
    folder_button(ctx, dialog, &projects, "Choose Projects Folder", Settings::projects_dir, |s, p| s.projects_folder = Some(p), refresh.clone());
    folder_button(ctx, dialog, &exports, "Choose Exports Folder", Settings::exports_dir, |s, p| s.exports_folder = Some(p), refresh);
    g.add(&projects);
    g.add(&exports);
    body.append(&g);
    scroller.upcast()
}

fn folder_button(
    ctx: &Ctx,
    dialog: &adw::Dialog,
    row: &adw::ActionRow,
    title: &'static str,
    current: fn(&Settings) -> PathBuf,
    store: fn(&mut Settings, PathBuf),
    refresh: Rc<dyn Fn()>,
) {
    let button = gtk::Button::from_icon_name("folder-open-symbolic");
    button.add_css_class("flat");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some("Choose Folder…"));
    let (ctx, dialog) = (ctx.clone(), dialog.clone());
    button.connect_clicked(move |_| {
        let chooser = gtk::FileDialog::builder().title(title).build();
        let dir = current(&ctx.s().settings);
        if dir.is_dir() {
            chooser.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let (ctx, refresh) = (ctx.clone(), refresh.clone());
        let win = dialog.root().and_downcast::<gtk::Window>();
        chooser.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
            if let Ok(f) = r
                && let Some(path) = f.path()
            {
                set(&ctx, false, |s| store(s, path));
                refresh();
            }
        });
    });
    row.add_suffix(&button);
}

#[derive(Default)]
struct Found {
    linux: Vec<PathBuf>,
    /// Prefix, plug-in folder, and how many plug-in modules it holds.
    windows: Vec<(PathBuf, PathBuf, usize)>,
    bridged: Vec<PathBuf>,
}

fn find() -> Found {
    let home = glib::home_dir();
    let wineprefix = std::env::var_os("WINEPREFIX").filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut windows = Vec::new();
    for prefix in plugin_search::wine_prefixes(&home, wineprefix.as_deref()) {
        for dir in plugin_search::windows_plugin_dirs(&prefix) {
            let d = std::slice::from_ref(&dir);
            let n = solstice_vst3::find_modules(d).len() + solstice_clap::find_modules(d).len();
            windows.push((prefix.clone(), dir, n));
        }
    }
    Found {
        linux: plugin_search::linux_plugin_dirs(&plugin_search::extra_roots(&home)),
        windows,
        bridged: yabridge::folders(),
    }
}

fn search(ctx: &Ctx, shared: &Rc<Shared>, body: &gtk::Box) {
    shared.searched.set(true);
    clear(body);
    body.append(&busy("Searching for plug-ins…"));
    let (ctx, shared, body) = (ctx.clone(), shared.clone(), body.clone());
    glib::spawn_future_local(async move {
        prefs::scan_catalog(&ctx, false).await;
        let found = gio::spawn_blocking(find).await.unwrap_or_default();
        *shared.windows_dirs.borrow_mut() = Some(found.windows.iter().map(|w| w.1.clone()).collect());
        clear(&body);

        let linux = group("Linux Plug-ins", "Plug-ins Solstice can use now");
        let rows = ["VST3", "CLAP", "LV2"].map(|f| {
            let r = info_row(f, "");
            linux.add(&r);
            r
        });
        let bridged = info_row("Bridged Windows Plug-ins", "");
        let counts = {
            let (ctx, bridged) = (ctx.clone(), bridged.clone());
            move || {
                let home = glib::home_dir();
                let bridges = [home.join(".vst3/yabridge"), home.join(".clap/yabridge")];
                let mut n = [0usize; 4];
                for c in ctx.s().catalog.iter().filter(|c| !c.path.as_os_str().is_empty()) {
                    let i = if bridges.iter().any(|b| c.path.starts_with(b)) {
                        3
                    } else if c.uid.starts_with(solstice_clap::PREFIX) {
                        1
                    } else if c.uid.starts_with(solstice_lv2::PREFIX) {
                        2
                    } else {
                        0
                    };
                    n[i] += 1;
                }
                let text = |n: usize| match n {
                    0 => "None found".to_string(),
                    1 => "1 plug-in".to_string(),
                    n => format!("{n} plug-ins"),
                };
                for (r, n) in rows.iter().zip(n) {
                    r.set_subtitle(&text(n));
                }
                bridged.set_subtitle(&text(n[3]));
                bridged.set_visible(n[3] > 0);
            }
        };
        counts();
        body.append(&linux);

        let added = ctx.s().settings.vst3_paths.clone();
        let extra: Vec<PathBuf> = found.linux.into_iter().filter(|d| !added.contains(d)).collect();
        if !extra.is_empty() {
            let g = group(
                "Other Linux Plug-in Folders",
                "These folders hold VST3 or CLAP plug-ins outside the standard folders. Add a folder to use its plug-ins.",
            );
            let counts = Rc::new(counts);
            for dir in extra {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&shown(&dir))).build();
                let add = gtk::Button::with_label("Add");
                add.set_valign(gtk::Align::Center);
                let (ctx, counts) = (ctx.clone(), counts.clone());
                add.connect_clicked(move |b| {
                    b.set_sensitive(false);
                    b.set_label("Added");
                    set(&ctx, false, |s| {
                        if !s.vst3_paths.contains(&dir) {
                            s.vst3_paths.push(dir.clone());
                        }
                    });
                    let (ctx, counts) = (ctx.clone(), counts.clone());
                    glib::spawn_future_local(async move {
                        prefs::scan_catalog(&ctx, false).await;
                        counts();
                    });
                });
                row.add_suffix(&add);
                g.add(&row);
            }
            body.append(&g);
        }

        let windows = group(
            "Windows Plug-ins",
            "Windows VST3 and CLAP plug-ins installed in Wine prefixes. Solstice uses them through yabridge, which the next page sets up.",
        );
        if found.windows.is_empty() {
            windows.add(&info_row("None Found", "No Wine prefix holds Windows VST3 or CLAP plug-ins"));
        }
        for (prefix, dir, n) in found.windows {
            let format = dir.file_name().map_or_else(String::new, |f| f.to_string_lossy().into_owned());
            let plugins = if n == 1 { "1 plug-in".to_string() } else { format!("{n} plug-ins") };
            let title = glib::markup_escape_text(&format!("{format} in {}", shown(&prefix)));
            let row = if found.bridged.contains(&dir) {
                tick_row(&title, &format!("{plugins}, bridged by yabridge"))
            } else {
                info_row(&title, &format!("{plugins}, not bridged yet"))
            };
            row.set_tooltip_text(Some(&dir.display().to_string()));
            windows.add(&row);
        }
        windows.add(&bridged);
        body.append(&windows);
    });
}

fn detect(ctx: &Ctx, shared: &Rc<Shared>, body: &gtk::Box) {
    shared.detected.set(true);
    clear(body);
    body.append(&busy("Checking Wine and yabridge…"));
    let (ctx, shared, body) = (ctx.clone(), shared.clone(), body.clone());
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(system::detect).await;
        clear(&body);
        let Ok((wine, verdict)) = result else {
            body.append(&group("Wine and yabridge", "Solstice could not check Wine and yabridge."));
            return;
        };
        let version = wine.map(|(a, b)| format!("Wine {a}.{b}"));
        let status = group("Wine and yabridge", "Windows plug-ins run in Wine, and yabridge bridges them into Solstice as Linux plug-ins.");
        status.add(&match &version {
            Some(v) => info_row("Wine", v),
            None => warning_row("Wine", "Not installed"),
        });
        let link = wine.map(system::yabridge_link);
        let (row, advice) = match &verdict {
            Verdict::NoWine => (warning_row("yabridge", "Needs Wine first"), None),
            Verdict::NoYabridge { branch: true } => (
                warning_row("yabridge", "Not installed"),
                Some("No distribution packages yabridge, so it is installed by hand. With Wine 9.22 and later, use a build of its new-wine10-embedding branch: in yabridge 5.1.1, the latest release, Windows plug-in windows ignore clicks in their pop-up menus and dialogs."),
            ),
            Verdict::NoYabridge { branch: false } => (
                warning_row("yabridge", "Not installed"),
                Some("No distribution packages yabridge, so it is installed by hand. The latest release works with this Wine."),
            ),
            Verdict::Broken => (
                warning_row("yabridge", "Pop-up windows will not take clicks with this Wine"),
                Some("yabridge 5.1.1 and earlier ignore clicks in Windows plug-in pop-up menus and dialogs with Wine 9.22 and later. Plug-ins still load and play. A build of the new-wine10-embedding branch fixes this."),
            ),
            Verdict::Unknown => (
                info_row("yabridge", "Built from a commit Solstice does not recognise"),
                Some("Solstice can't tell whether this build has the fix for pop-up windows in Wine 9.22 and later. If pop-up menus and dialogs in Windows plug-ins ignore clicks, use a build of the new-wine10-embedding branch."),
            ),
            Verdict::Unchecked => (
                info_row("yabridge", "Installed, but Solstice couldn't read which build"),
                Some("Without the build, Solstice can't tell whether yabridge works with this Wine. If pop-up menus and dialogs in Windows plug-ins ignore clicks, use the build the link below leads to."),
            ),
            Verdict::Working => (tick_row("yabridge", &format!("Works with {}", version.as_deref().unwrap_or("this Wine"))), None),
        };
        status.add(&row);
        body.append(&status);

        if verdict == Verdict::NoWine {
            body.append(&install_wine(&ctx, &shared, &body));
        }
        if let (Some(text), Some(link)) = (advice, link) {
            let g = group("Getting yabridge", text);
            let row = adw::ActionRow::builder().title("yabridge").subtitle(link).build();
            row.add_suffix(&gtk::LinkButton::with_label(link, "Open"));
            g.add(&row);
            body.append(&g);
        }
        if !matches!(verdict, Verdict::NoWine | Verdict::NoYabridge { .. }) {
            body.append(&set_up(&ctx, &shared));
        }

        let again = adw::ButtonRow::builder().title("Check Again").start_icon_name("view-refresh-symbolic").build();
        let (ctx, shared, body2) = (ctx.clone(), shared.clone(), body.clone());
        again.connect_activated(move |_| detect(&ctx, &shared, &body2));
        let g = adw::PreferencesGroup::new();
        g.add(&again);
        body.append(&g);
    });
}

fn install_wine(ctx: &Ctx, shared: &Rc<Shared>, body: &gtk::Box) -> adw::PreferencesGroup {
    let g = group(
        "Install Wine",
        "Solstice installs your distribution's Wine package through PackageKit, which asks through your desktop's authentication prompt.",
    );
    let row = adw::ActionRow::builder().title("Wine").subtitle("Not installed").build();
    let button = gtk::Button::with_label("Install Wine");
    button.add_css_class("suggested-action");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);
    g.add(&row);
    let os_release = std::fs::read_to_string("/etc/os-release").or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    let command = command_row(&elevate::wine_command(&os_release));
    command.set_visible(false);
    g.add(&command);
    let (ctx, shared, body) = (ctx.clone(), shared.clone(), body.clone());
    button.connect_clicked(move |b| {
        b.set_sensitive(false);
        row.set_subtitle("Installing…");
        let (ctx, shared, body, b, row, command) = (ctx.clone(), shared.clone(), body.clone(), b.clone(), row.clone(), command.clone());
        elevate::run(elevate::wine_argv(), move |outcome| {
            match outcome {
                Outcome::Done => return detect(&ctx, &shared, &body),
                Outcome::Cancelled => row.set_subtitle("Cancelled. You can install it with the command below instead."),
                Outcome::Failed(e) => row.set_subtitle(&glib::markup_escape_text(&format!("The install failed: {}. You can install it with the command below instead.", e.trim_end_matches('.')))),
            }
            b.set_sensitive(true);
            command.set_visible(true);
        });
    });
    g
}

fn set_up(ctx: &Ctx, shared: &Rc<Shared>) -> adw::PreferencesGroup {
    let g = group(
        "Set Up yabridge",
        "Adds the Windows plug-in folders found on the Plug-ins page to yabridge, bridges their plug-ins, and scans the plug-ins again.",
    );
    let row = adw::ActionRow::builder().title("Windows Plug-in Folders").build();
    let button = gtk::Button::with_label("Set Up");
    button.add_css_class("suggested-action");
    button.set_valign(gtk::Align::Center);
    row.add_suffix(&button);
    g.add(&row);
    let (ctx, shared) = (ctx.clone(), shared.clone());
    button.connect_clicked(move |b| {
        b.set_sensitive(false);
        row.set_subtitle("Setting up…");
        let dirs = shared.windows_dirs.borrow().clone();
        let (ctx, shared, b, row) = (ctx.clone(), shared.clone(), b.clone(), row.clone());
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || bridge(dirs)).await.unwrap_or_else(|_| Err("yabridgectl failed".into()));
            match result {
                Ok(summary) => {
                    prefs::scan_catalog(&ctx, false).await;
                    shared.searched.set(false);
                    row.set_subtitle(&glib::markup_escape_text(&summary));
                }
                Err(e) => row.set_subtitle(&glib::markup_escape_text(&e)),
            }
            b.set_sensitive(true);
        });
    });
    g
}

fn bridge(dirs: Option<Vec<PathBuf>>) -> Result<String, String> {
    let dirs = dirs.unwrap_or_else(|| find().windows.into_iter().map(|w| w.1).collect());
    let bridged = yabridge::folders();
    if dirs.is_empty() && bridged.is_empty() {
        return Err("No Windows plug-in folders were found in Wine prefixes".into());
    }
    let mut added = 0;
    for d in dirs.iter().filter(|d| !bridged.contains(d)) {
        yabridge::add(d)?;
        added += 1;
    }
    let summary = yabridge::sync()?;
    Ok(match added {
        0 => summary,
        1 => format!("Added 1 folder. {summary}"),
        n => format!("Added {n} folders. {summary}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_group_members() {
        let etc = "wheel:x:10:tom\npipewire:x:973:alice,tom\naudio:x:63:\n";
        assert!(member(etc, "pipewire", "tom"));
        assert!(!member(etc, "audio", "tom"));
        assert!(!member(etc, "pipewire", "to"));
        assert!(!member(etc, "realtime", "tom"));
    }
}
