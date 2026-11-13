//! The plug-in presets menu: previous and next buttons around a menu of factory presets,
//! the user's own presets, Reset and Save as Preset. User presets are files in
//! `<data dir>/presets/<plug-in>/`: plug-in state for plug-ins, JSON for the Channel EQ.

use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub enum Pick {
    Default,
    Factory(usize),
    User(PathBuf),
}

pub fn dir(plugin: &str) -> PathBuf {
    let base = match crate::settings::dev_root() {
        Some(d) => d.join("data"),
        None => glib::user_data_dir().join("solstice"),
    };
    base.join("presets").join(plugin.replace([':', '/'], "-"))
}

fn user_presets(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == ext)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned())
}

/// The preset controls for a plug-in window's header bar. `load` applies a pick; `save`
/// returns the bytes to write for the current settings.
/// `initial` is the factory preset the current settings match, if any.
pub fn selector(
    factory: Vec<String>,
    initial: Option<usize>,
    dir: PathBuf,
    ext: &'static str,
    load: Rc<dyn Fn(&Pick)>,
    save: Rc<dyn Fn() -> Vec<u8>>,
) -> gtk::Widget {
    let current: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(initial));
    let users: Rc<RefCell<Vec<PathBuf>>> = Rc::new(RefCell::new(user_presets(&dir, ext)));
    let label = gtk::Label::new(Some(initial.and_then(|i| factory.get(i)).map_or("Presets", |s| s.as_str())));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(18);
    label.set_width_chars(12);
    let menu = gio::Menu::new();
    let button = gtk::MenuButton::builder().child(&label).menu_model(&menu).tooltip_text("Presets").build();

    let name_of = {
        let (factory, users) = (factory.clone(), users.clone());
        move |i: usize| factory.get(i).cloned().unwrap_or_else(|| users.borrow().get(i - factory.len()).map(|p| stem(p)).unwrap_or_default())
    };
    let pick = {
        let (factory, users, current, label, load) = (factory.clone(), users.clone(), current.clone(), label.clone(), load.clone());
        let name_of = name_of.clone();
        Rc::new(move |i: Option<usize>| {
            let total = factory.len() + users.borrow().len();
            match i {
                None => {
                    load(&Pick::Default);
                    label.set_text("Default");
                }
                Some(i) if i < factory.len() => load(&Pick::Factory(i)),
                Some(i) if i < total => load(&Pick::User(users.borrow()[i - factory.len()].clone())),
                Some(_) => return,
            }
            if let Some(i) = i {
                label.set_text(&name_of(i));
            }
            current.set(i);
        })
    };

    let fill = {
        let (factory, users, menu, dir) = (factory.clone(), users.clone(), menu.clone(), dir.clone());
        move || {
            *users.borrow_mut() = user_presets(&dir, ext);
            menu.remove_all();
            let top = gio::Menu::new();
            top.append(Some("Reset to Default"), Some("preset.pick(-1)"));
            menu.append_section(None, &top);
            let add = |names: Vec<String>, offset: usize, title: Option<&str>| {
                if names.is_empty() {
                    return;
                }
                let section = gio::Menu::new();
                for (i, n) in names.iter().enumerate() {
                    let item = gio::MenuItem::new(Some(n), None);
                    item.set_action_and_target_value(Some("preset.pick"), Some(&((offset + i) as i32).to_variant()));
                    section.append_item(&item);
                }
                menu.append_section(title, &section);
            };
            add(factory.clone(), 0, Some("Factory"));
            add(users.borrow().iter().map(|p| stem(p)).collect(), factory.len(), Some("My Presets"));
            let bottom = gio::Menu::new();
            bottom.append(Some("Save as Preset…"), Some("preset.save"));
            menu.append_section(None, &bottom);
        }
    };
    fill();
    {
        let fill = fill.clone();
        button.connect_active_notify(move |b| {
            if b.is_active() {
                fill();
            }
        });
    }

    let group = gio::SimpleActionGroup::new();
    let a = gio::SimpleAction::new("pick", Some(glib::VariantTy::INT32));
    {
        let pick = pick.clone();
        a.connect_activate(move |_, v| {
            let i = v.and_then(|v| v.get::<i32>()).unwrap_or(-1);
            pick(usize::try_from(i).ok());
        });
    }
    group.add_action(&a);
    let a = gio::SimpleAction::new("save", None);
    {
        let (dir, label, current, users, factory, button) = (dir.clone(), label.clone(), current.clone(), users.clone(), factory.len(), button.clone());
        let fill = fill.clone();
        a.connect_activate(move |_, _| {
            let entry = adw::EntryRow::builder().title("Name").build();
            let list = gtk::ListBox::new();
            list.add_css_class("boxed-list");
            list.append(&entry);
            let dialog = adw::AlertDialog::builder().heading("Save Preset").extra_child(&list).close_response("cancel").default_response("save").build();
            dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
            dialog.set_response_enabled("save", false);
            {
                let d = dialog.clone();
                entry.connect_changed(move |e| d.set_response_enabled("save", !e.text().trim().is_empty()));
            }
            let (dir, label, current, users, save, fill) = (dir.clone(), label.clone(), current.clone(), users.clone(), save.clone(), fill.clone());
            dialog.connect_response(Some("save"), move |_, _| {
                let name = entry.text().trim().replace('/', "-");
                let path = dir.join(format!("{name}.{ext}"));
                if std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, save())).is_err() {
                    return;
                }
                fill();
                label.set_text(&name);
                current.set(users.borrow().iter().position(|p| *p == path).map(|i| factory + i));
            });
            dialog.present(Some(&button));
        });
    }
    group.add_action(&a);
    button.insert_action_group("preset", Some(&group));

    let step = |delta: isize| {
        let (pick, current, factory, users) = (pick.clone(), current.clone(), factory.len(), users.clone());
        move |_: &gtk::Button| {
            let total = (factory + users.borrow().len()) as isize;
            if total == 0 {
                return;
            }
            let next = match current.get() {
                Some(i) => (i as isize + delta).rem_euclid(total),
                None if delta > 0 => 0,
                None => total - 1,
            };
            pick(Some(next as usize));
        }
    };
    let prev = gtk::Button::from_icon_name("go-previous-symbolic");
    prev.set_tooltip_text(Some("Previous Preset"));
    prev.connect_clicked(step(-1));
    let next = gtk::Button::from_icon_name("go-next-symbolic");
    next.set_tooltip_text(Some("Next Preset"));
    next.connect_clicked(step(1));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("linked");
    row.append(&prev);
    row.append(&button);
    row.append(&next);
    row.upcast()
}

/// The selector for a loaded plug-in: factory presets for built-in ones, and the user's own
/// presets for any plug-in, saved as its state.
pub fn for_plugin(inst: &crate::host::Host, audio_dir: impl Fn() -> PathBuf + 'static) -> gtk::Widget {
    let uid = inst.info().uid;
    let factory = solstice_dsp::presets::factory(&uid);
    let names = factory.iter().map(|p| p.name.to_string()).collect();
    let load = {
        let inst = inst.clone();
        Rc::new(move |pick: &Pick| match pick {
            Pick::Default => match inst.native() {
                Some(n) => n.bank.defs.iter().for_each(|d| n.bank.set(d.id, d.default)),
                None => inst.params().iter().for_each(|p| inst.set_param(p.id, p.default_value)),
            },
            Pick::Factory(i) => {
                if let (Some(n), Some(p)) = (inst.native(), factory.get(*i)) {
                    solstice_dsp::presets::apply(&n.bank, p);
                }
            }
            Pick::User(path) => {
                if let Ok(data) = std::fs::read(path) {
                    inst.set_state_in(&data, &audio_dir());
                }
            }
        })
    };
    let save = {
        let inst = inst.clone();
        Rc::new(move || inst.state())
    };
    selector(names, matching(inst), dir(&uid), "preset", load, save)
}

/// The factory preset a built-in plug-in's settings match, if any.
fn matching(inst: &crate::host::Host) -> Option<usize> {
    let factory = solstice_dsp::presets::factory(&inst.info().uid);
    inst.native().and_then(|n| {
        factory.iter().position(|p| {
            n.bank.defs.iter().all(|d| {
                let want = p.values.iter().find(|(id, _)| *id == d.id).map_or(d.default, |(_, v)| *v);
                (n.bank.get(d.id) - want).abs() < 1e-3
            })
        })
    })
}

pub fn factory_name(inst: &crate::host::Host) -> Option<&'static str> {
    matching(inst).and_then(|i| solstice_dsp::presets::factory(&inst.info().uid).get(i)).map(|p| p.name)
}
