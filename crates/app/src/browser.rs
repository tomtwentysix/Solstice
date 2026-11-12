use crate::import::is_audio;
use crate::studio::{Change, Ctx};
use crate::timeline::import_files;
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// The Browser panel: all files, or loops from the user's loop folders.
pub fn build(ctx: &Ctx) -> gtk::Widget {
    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(&files(ctx), Some("files"), "Files", "folder-symbolic");
    stack.add_titled_with_icon(&crate::loops::build(ctx), Some("loops"), "Loops", "media-playlist-repeat-symbolic");
    {
        // Imports that point at their files show the Files tab.
        let stack = stack.clone();
        ctx.on(move |c| {
            if c == Change::Browse {
                stack.set_visible_child_name("files");
            }
        });
    }
    let switcher = adw::InlineViewSwitcher::builder().stack(&stack).build();
    switcher.set_margin_start(8);
    switcher.set_margin_end(8);
    switcher.set_margin_top(8);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&switcher);
    col.append(&stack);
    stack.set_vexpand(true);
    col.upcast()
}

/// A file browser for audio, like Logic's All Files browser. Double-click imports at the
/// playhead; rows can be dragged onto the tracks area.
fn files(ctx: &Ctx) -> gtk::Widget {
    let start = glib::user_special_dir(glib::UserDirectory::Music).unwrap_or_else(glib::home_dir);
    let dir = Rc::new(RefCell::new(start));

    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    col.set_margin_start(8);
    col.set_margin_end(8);
    col.set_margin_top(8);
    let nav = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    up.set_tooltip_text(Some("Parent Folder"));
    up.add_css_class("flat");
    let home = gtk::Button::from_icon_name("user-home-symbolic");
    home.set_tooltip_text(Some("Home"));
    home.add_css_class("flat");
    let label = gtk::Label::new(None);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    label.set_hexpand(true);
    label.set_xalign(0.0);
    label.add_css_class("caption");
    nav.append(&up);
    nav.append(&home);
    nav.append(&label);
    col.append(&nav);

    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder().child(&list).vexpand(true).build();
    col.append(&scroller);

    let refresh: Rc<dyn Fn()> = {
        let (dir, list, label) = (dir.clone(), list.clone(), label.clone());
        Rc::new(move || {
            while let Some(c) = list.first_child() {
                list.remove(&c);
            }
            let d = dir.borrow().clone();
            label.set_text(&d.display().to_string());
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&d)
                .map(|rd| rd.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            entries.retain(|p| {
                !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) && (p.is_dir() || is_audio(p))
            });
            entries.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
            for p in entries {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&name)).activatable(true).build();
                let icon = if p.is_dir() { "folder-symbolic" } else { "audio-x-generic-symbolic" };
                row.add_prefix(&gtk::Image::from_icon_name(icon));
                if !p.is_dir() {
                    let src = gtk::DragSource::new();
                    src.set_actions(gdk::DragAction::COPY);
                    let file = gio::File::for_path(&p);
                    src.connect_prepare(move |_, _, _| {
                        Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&[file.clone()]).to_value()))
                    });
                    row.add_controller(src);
                }
                row.set_widget_name(&p.to_string_lossy());
                list.append(&row);
            }
        })
    };
    {
        let (dir, refresh, ctx) = (dir.clone(), refresh.clone(), ctx.clone());
        list.connect_row_activated(move |_, row| {
            let p = PathBuf::from(row.widget_name().as_str());
            if p.is_dir() {
                *dir.borrow_mut() = p;
                refresh();
            } else {
                let row = ctx.s().selected_track.and_then(|id| ctx.s().project.tracks.iter().position(|t| t.id == id));
                import_files(&ctx, &[p], row, ctx.position());
            }
        });
    }
    {
        let (dir, refresh) = (dir.clone(), refresh.clone());
        up.connect_clicked(move |_| {
            let parent = dir.borrow().parent().map(|p| p.to_path_buf());
            if let Some(p) = parent {
                *dir.borrow_mut() = p;
                refresh();
            }
        });
    }
    {
        let (dir, refresh) = (dir.clone(), refresh.clone());
        home.connect_clicked(move |_| {
            *dir.borrow_mut() = glib::home_dir();
            refresh();
        });
    }
    refresh();
    {
        let (ctx2, dir, refresh) = (ctx.clone(), dir.clone(), refresh.clone());
        ctx.on(move |c| {
            if c == Change::Browse
                && let Some(p) = ctx2.m().browse_to.take()
            {
                *dir.borrow_mut() = p;
                refresh();
            }
        });
    }
    col.upcast()
}
