use super::*;
use adw::prelude::*;
use crate::captures;
use crate::studio::Ctx;
use gtk::gio;
use std::cell::RefCell;
use std::path::PathBuf;

const TONE3000: &str = "https://www.tone3000.com/";

fn folder_of(native: &Native) -> usize {
    let bass = captures::library().join(captures::FOLDERS[1]);
    native.captures.as_ref().and_then(|c| c.desc()).is_some_and(|d| d.source.starts_with(&bass)) as usize
}

/// What the header says about the capture playing: its name and a detail line.
fn describe(native: &Native) -> (String, String) {
    let Some(c) = &native.captures else { return Default::default() };
    if let Some((desc, why)) = c.missing() {
        return (desc.name(), format!("Could not be loaded: {why}"));
    }
    match c.current() {
        Some(cap) => {
            let detail = match cap.rate {
                Some(r) if cap.latency > 0 => format!("Made at {:.1} kHz, resampled, adding {} samples of latency", r / 1000.0, cap.latency),
                Some(r) => format!("Made at {:.1} kHz", r / 1000.0),
                None => "Sample rate not stated".into(),
            };
            (cap.desc.name(), detail)
        }
        None => ("No capture".into(), "Choose one below, import one, or drop .nam files or zips here".into()),
    }
}

pub fn nam(ctx: &Ctx, native: &Arc<Native>) -> gtk::Widget {
    let b = &native.bank;
    let folder = Rc::new(Cell::new(folder_of(native)));

    let top = gtk::Box::new(gtk::Orientation::Vertical, 8);
    top.add_css_class("plugin-section");
    let heading = gtk::Label::new(Some("Capture"));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    let name = gtk::Label::new(None);
    name.add_css_class("title-3");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let detail = gtk::Label::new(None);
    detail.add_css_class("caption");
    detail.add_css_class("dim-label");
    detail.set_xalign(0.0);
    detail.set_wrap(true);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let folders = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    folders.add_css_class("linked");
    let import = gtk::Button::with_label("Import…");
    import.set_tooltip_text(Some("Add .nam captures, or zips of them, to this folder"));
    let web = gtk::Button::with_label("Open TONE3000");
    web.set_tooltip_text(Some("Find captures to download in your browser"));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&folders);
    bar.append(&spacer);
    bar.append(&import);
    bar.append(&web);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).min_content_height(240).build();

    top.append(&heading);
    top.append(&name);
    top.append(&detail);
    top.append(&bar);
    top.append(&scroller);

    let paths: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let fill: Rc<dyn Fn()> = {
        let (list, folder, native, paths) = (list.clone(), folder.clone(), native.clone(), paths.clone());
        Rc::new(move || {
            list.remove_all();
            let found = captures::list(&captures::library().join(captures::FOLDERS[folder.get()]));
            let playing = native.captures.as_ref().and_then(|c| c.desc()).map(|d| d.source);
            if found.is_empty() {
                let row = adw::ActionRow::builder()
                    .title("No captures in this folder")
                    .subtitle("Import captures, drop .nam files or zips here, or find more on TONE3000")
                    .build();
                list.append(&row);
            }
            for p in &found {
                let title = p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&title)).activatable(true).build();
                if playing.as_ref() == Some(p) {
                    row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
                }
                list.append(&row);
            }
            *paths.borrow_mut() = found;
        })
    };
    {
        let (ctx, native, paths) = (ctx.clone(), native.clone(), paths.clone());
        list.connect_row_activated(move |_, row| {
            if let Some(p) = paths.borrow().get(row.index() as usize) {
                captures::use_capture(&ctx, &native, p.clone());
            }
        });
    }

    let mut group: Option<gtk::ToggleButton> = None;
    for (i, f) in captures::FOLDERS.iter().enumerate() {
        let t = gtk::ToggleButton::with_label(f);
        t.set_group(group.as_ref());
        t.set_active(folder.get() == i);
        let (folder, fill) = (folder.clone(), fill.clone());
        t.connect_toggled(move |t| {
            if t.is_active() && folder.get() != i {
                folder.set(i);
                fill();
            }
        });
        folders.append(&t);
        group.get_or_insert(t);
    }

    let import_files: Rc<dyn Fn(Vec<PathBuf>)> = {
        let (ctx, native, folder, fill) = (ctx.clone(), native.clone(), folder.clone(), fill.clone());
        Rc::new(move |files: Vec<PathBuf>| {
            let files: Vec<PathBuf> = files.into_iter().filter(|p| captures::is_importable(p)).collect();
            if files.is_empty() {
                ctx.message("Only .nam captures and zips of them can be imported");
                return;
            }
            let fill = fill.clone();
            captures::import_and_use(&ctx, &native, files, captures::FOLDERS[folder.get()], move || fill());
        })
    };
    {
        let import_files = import_files.clone();
        import.connect_clicked(move |b| {
            let import_files = import_files.clone();
            crate::sample_editor::pick_files(b, "Captures", &["*.nam", "*.NAM", "*.zip", "*.ZIP"], true, move |files| import_files(files));
        });
    }
    web.connect_clicked(|b| {
        let win = b.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(TONE3000).launch(win.as_ref(), gio::Cancellable::NONE, |_| {});
    });

    let meter = {
        let b = b.clone();
        picture(34, move |c, s, w, h| {
            let fg = canvas::fg(c);
            let (lo, hi) = (-48.0, 6.0);
            let x = |db: f32| 8.0 + (w - 16.0) * ((db - lo) / (hi - lo)).clamp(0.0, 1.0);
            let level = b.meter();
            canvas::rect(s, x(lo), h - 14.0, x(hi) - x(lo), 6.0, &alpha(&fg, 0.08));
            let colour = if level > 0.0 { canvas::rgba(0xe01b24, 0.9) } else { canvas::accent(c) };
            canvas::rect(s, x(lo), h - 14.0, x(level) - x(lo), 6.0, &colour);
            canvas::rect(s, x(0.0), h - 16.0, 1.0, 10.0, &alpha(&fg, 0.4));
            label(s, c, 8.0, 3.0, "Into the capture", 0.6);
            let text = if level <= lo { "−∞".to_string() } else { format!("{level:.1} dB") };
            label_right(s, c, w - 8.0, 3.0, &text, 0.8);
        })
    };
    let levels = section("Levels", Some(&meter), b, &[0, 1]);

    let content = stack(&[top.upcast_ref(), levels.upcast_ref()]);
    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(move |_, v, _, _| {
        let Ok(list) = v.get::<gdk::FileList>() else { return false };
        let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if !files.iter().any(|p| captures::is_importable(p)) {
            return false;
        }
        import_files(files);
        true
    });
    content.add_controller(drop);

    // Follows loads finishing, undo and redo; refills the list when the capture changes.
    let seen: RefCell<Option<(String, String)>> = RefCell::default();
    let native = native.clone();
    content.add_tick_callback(move |_, _| {
        let now = describe(&native);
        if seen.borrow().as_ref() != Some(&now) {
            name.set_text(&now.0);
            detail.set_text(&now.1);
            *seen.borrow_mut() = Some(now);
            fill();
        }
        glib::ControlFlow::Continue
    });
    content.upcast()
}
