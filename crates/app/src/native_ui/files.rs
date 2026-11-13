use super::*;
use crate::library::{self, Library};
use crate::studio::Ctx;
use adw::prelude::*;
use solstice_dsp::slot::Loaded;
use std::cell::RefCell;
use std::path::PathBuf;

/// The file an effect plays and the library to choose it from: a heading, its name and a
/// detail line from `describe`, folder switches, Import… and any `extra` buttons, then the list.
/// Dropping files or zips on `content` imports them. The header follows loads finishing, undo
/// and redo.
pub(super) fn chooser<T: Loaded>(
    ctx: &Ctx,
    native: &Arc<Native>,
    lib: &'static Library,
    describe: impl Fn(&Native) -> (String, String) + 'static,
    extra: &[gtk::Widget],
    content: &gtk::Box,
) {
    let slot = T::slot(native).expect("the effect plays files").clone();
    let folder = Rc::new(Cell::new(slot.desc().map_or(0, |d| lib.folder_of(&d.source))));
    let what = format!("{}s", lib.noun);
    let ext = lib.ext();

    let top = gtk::Box::new(gtk::Orientation::Vertical, 8);
    top.add_css_class("plugin-section");
    let mut heading_text = lib.noun.to_string();
    heading_text[..1].make_ascii_uppercase();
    let heading = gtk::Label::new(Some(&heading_text));
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
    import.set_tooltip_text(Some(&format!("Add .{ext} files, or zips of them, to this library")));
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&folders);
    bar.append(&spacer);
    bar.append(&import);
    for w in extra {
        bar.append(w);
    }

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder().child(&list).hscrollbar_policy(gtk::PolicyType::Never).min_content_height(240).build();

    top.append(&heading);
    top.append(&name);
    top.append(&detail);
    top.append(&bar);
    top.append(&scroller);
    content.prepend(&top);

    let paths: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let fill: Rc<dyn Fn()> = {
        let (list, folder, slot, paths, what) = (list.clone(), folder.clone(), slot.clone(), paths.clone(), what.clone());
        Rc::new(move || {
            list.remove_all();
            let found = lib.list(&lib.folder(folder.get()));
            let playing = slot.desc().map(|d| d.source);
            if found.is_empty() {
                let place = if lib.folders.is_empty() { "the library" } else { "this folder" };
                let row = adw::ActionRow::builder()
                    .title(format!("No {what} in {place}"))
                    .subtitle(format!("Import {what}, or drop .{ext} files or zips of them here"))
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
                library::use_file::<T>(&ctx, &native, lib, p.clone());
            }
        });
    }

    let mut group: Option<gtk::ToggleButton> = None;
    for (i, f) in lib.folders.iter().enumerate() {
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
            let files: Vec<PathBuf> = files.into_iter().filter(|p| lib.is_importable(p)).collect();
            if files.is_empty() {
                ctx.message(&format!("Only .{ext} files and zips of them can be imported"));
                return;
            }
            let fill = fill.clone();
            library::import_and_use::<T>(&ctx, &native, lib, files, lib.folder(folder.get()), move || fill());
        })
    };
    {
        let import_files = import_files.clone();
        let mut title = what.clone();
        title[..1].make_ascii_uppercase();
        import.connect_clicked(move |b| {
            let import_files = import_files.clone();
            let (lower, upper) = (format!("*.{ext}"), format!("*.{}", ext.to_uppercase()));
            crate::sample_editor::pick_files(b, &title, &[&lower, &upper, "*.zip", "*.ZIP"], true, move |files| import_files(files));
        });
    }

    let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    drop.connect_drop(move |_, v, _, _| {
        let Ok(list) = v.get::<gdk::FileList>() else { return false };
        let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
        if !files.iter().any(|p| lib.is_importable(p)) {
            return false;
        }
        import_files(files);
        true
    });
    content.add_controller(drop);

    let seen: RefCell<Option<(String, String)>> = RefCell::default();
    let native = native.clone();
    top.add_tick_callback(move |_, _| {
        let now = describe(&native);
        if seen.borrow().as_ref() != Some(&now) {
            name.set_text(&now.0);
            detail.set_text(&now.1);
            *seen.borrow_mut() = Some(now);
            fill();
        }
        glib::ControlFlow::Continue
    });
}

/// The name and detail for a slot that plays nothing, or whose file could not be loaded.
pub(super) fn empty_or_missing<T: Loaded>(native: &Native, lib: &Library) -> Option<(String, String)> {
    let slot = T::slot(native)?;
    if let Some((desc, why)) = slot.missing() {
        return Some((desc.name(), format!("Could not be loaded: {why}")));
    }
    slot.current().is_none().then(|| (format!("No {}", lib.noun), format!("Choose one below, import one, or drop .{} files or zips here", lib.ext())))
}
