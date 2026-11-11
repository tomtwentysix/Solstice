use crate::studio::{Change, Ctx};
use adw::prelude::*;
use gtk::glib;
use solstice_model::{Id, PluginSlot, Project, TrackKind};
use solstice_vst3::ClassInfo;
use std::rc::Rc;

pub fn new_slot(p: &mut Project, info: &ClassInfo) -> PluginSlot {
    PluginSlot {
        id: p.alloc_id(),
        uid: info.uid.clone(),
        path: info.path.clone(),
        name: info.name.clone(),
        bypass: false,
        state: Vec::new(),
    }
}

/// A searchable list of installed plug-ins of one kind.
pub fn list(ctx: &Ctx, instruments: Option<bool>, on_pick: Rc<dyn Fn(&ClassInfo)>) -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search Plug-ins"));
    col.append(&search);
    let listbox = gtk::ListBox::new();
    listbox.add_css_class("navigation-sidebar");
    listbox.set_selection_mode(gtk::SelectionMode::None);
    let scroller = gtk::ScrolledWindow::builder().child(&listbox).vexpand(true).min_content_height(260).build();
    col.append(&scroller);

    let items: Vec<ClassInfo> = ctx
        .s()
        .catalog
        .iter()
        .filter(|c| instruments.is_none_or(|i| c.is_instrument() == i))
        .cloned()
        .collect();
    if items.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("application-x-addon-symbolic")
            .title("No Plug-ins")
            .description("Install VST3 plug-ins in ~/.vst3 or add a folder in Preferences")
            .build();
        empty.add_css_class("compact");
        listbox.set_placeholder(Some(&empty));
    }
    for info in &items {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&info.name)).activatable(true).build();
        let sub = info.subcategories.replace('|', " · ");
        row.set_subtitle(&glib::markup_escape_text(&format!("{} · {}", info.vendor, sub)));
        let payload = format!("lib:{}", info.uid);
        let src = gtk::DragSource::new();
        src.set_actions(gtk::gdk::DragAction::COPY);
        src.connect_prepare(move |_, _, _| Some(gtk::gdk::ContentProvider::for_value(&payload.to_value())));
        row.add_controller(src);
        let (info, pick) = (info.clone(), on_pick.clone());
        row.connect_activated(move |_| pick(&info));
        listbox.append(&row);
    }
    {
        let items = items.clone();
        let lb = listbox.clone();
        search.connect_search_changed(move |s| {
            let q = s.text().to_lowercase();
            let mut i = 0;
            while let Some(row) = lb.row_at_index(i) {
                let it = &items[i as usize];
                row.set_visible(q.is_empty() || it.name.to_lowercase().contains(&q) || it.vendor.to_lowercase().contains(&q));
                i += 1;
            }
        });
    }
    col
}

pub fn picker_button(ctx: &Ctx, instruments: bool, tip: &str, on_pick: impl Fn(&ClassInfo) + 'static) -> gtk::MenuButton {
    let b = gtk::MenuButton::builder().icon_name("list-add-symbolic").tooltip_text(tip).build();
    b.add_css_class("flat");
    let pop = gtk::Popover::new();
    pop.set_size_request(320, -1);
    let pop2 = pop.clone();
    let on_pick: Rc<dyn Fn(&ClassInfo)> = Rc::new(move |i| {
        pop2.popdown();
        on_pick(i);
    });
    let ctx = ctx.clone();
    pop.connect_show(move |p| p.set_child(Some(&list(&ctx, Some(instruments), on_pick.clone()))));
    b.set_popover(Some(&pop));
    b
}

/// The Library pane: picking a plug-in loads it onto the selected track, like Logic's library.
pub fn library(ctx: &Ctx) -> gtk::Widget {
    let holder = adw::Bin::new();
    holder.set_margin_start(8);
    holder.set_margin_end(8);
    holder.set_margin_top(8);
    let fill = {
        let ctx = ctx.clone();
        let holder = holder.clone();
        move || {
            let ctx2 = ctx.clone();
            let pick: Rc<dyn Fn(&ClassInfo)> = Rc::new(move |info| apply_to_selection(&ctx2, info));
            holder.set_child(Some(&list(&ctx, None, pick)));
        }
    };
    fill();
    ctx.on(move |c| {
        if c == Change::Catalog {
            fill();
        }
    });
    holder.upcast()
}

fn apply_to_selection(ctx: &Ctx, info: &ClassInfo) {
    let info = info.clone();
    let sel = ctx.s().selected_track;
    let kind = sel.and_then(|id| ctx.s().project.track(id).map(|t| t.kind));
    if info.is_instrument() {
        ctx.edit("Set Instrument", |p| {
            let id = match (sel, kind) {
                (Some(id), Some(TrackKind::Instrument)) => id,
                _ => p.add_track(TrackKind::Instrument),
            };
            let slot = new_slot(p, &info);
            let t = p.track_mut(id).unwrap();
            t.name = info.name.clone();
            t.instrument = Some(slot);
        });
    } else {
        ctx.edit("Add Plug-in", |p| {
            let slot = new_slot(p, &info);
            p.channel_mut(sel).unwrap().inserts.push(slot);
        });
    }
}

pub fn open_editor(ctx: &Ctx, slot: Id, anchor: &impl IsA<gtk::Widget>) {
    let Some(inst) = ctx.s().plugins.get(&slot).map(|l| l.inst.clone()) else { return };
    if inst.editor_open() {
        inst.close_editor();
        return;
    }
    if inst.has_editor() && inst.open_editor(&inst.info.name) {
        return;
    }
    generic_editor(&inst, anchor);
}

/// Sliders for every parameter, for plug-ins without an X11 editor.
fn generic_editor(inst: &std::sync::Arc<solstice_vst3::Instance>, anchor: &impl IsA<gtk::Widget>) {
    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::builder().title(glib::markup_escape_text(&inst.info.name)).build();
    for p in inst.params().into_iter().filter(|p| !p.hidden) {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.title)).build();
        let value = gtk::Label::new(Some(&inst.param_text(p.id, inst.param(p.id))));
        value.add_css_class("dim-label");
        value.add_css_class("numeric");
        value.set_width_chars(10);
        let step = if p.steps > 0 { 1.0 / p.steps as f64 } else { 0.001 };
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, step);
        scale.set_draw_value(false);
        scale.set_size_request(200, -1);
        scale.set_value(inst.param(p.id));
        scale.set_sensitive(!p.read_only);
        let (i, id, v) = (inst.clone(), p.id, value.clone());
        scale.connect_value_changed(move |s| {
            i.set_param(id, s.value());
            v.set_text(&i.param_text(id, s.value()));
        });
        row.add_suffix(&value);
        row.add_suffix(&scale);
        group.add(&row);
    }
    page.add(&group);
    let dialog = adw::Dialog::builder().title(&inst.info.name).content_width(520).content_height(480).build();
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::new());
    tv.set_content(Some(&page));
    dialog.set_child(Some(&tv));
    dialog.present(Some(anchor));
}
