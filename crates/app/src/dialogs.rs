use adw::prelude::*;
use gtk::glib;

/// Hides the app's other windows (plug-ins, Channel EQ, the mixer window, Musical Typing) while
/// `dialog` is open over `parent`'s window, and shows them again when it closes. A dialog draws
/// inside its window, and transient or XWayland plug-in windows always stack above that, so
/// moving them out of the way is the only reliable way to keep the dialog in view.
pub fn clear_way(dialog: &impl IsA<adw::Dialog>, parent: &impl IsA<gtk::Widget>) {
    let Some(root) = parent.root().and_downcast::<gtk::Window>() else { return };
    let mut keep = vec![root.clone()];
    while let Some(up) = keep.last().and_then(|w| w.transient_for()) {
        keep.push(up);
    }
    let hidden: Vec<glib::WeakRef<gtk::Window>> = gtk::Window::list_toplevels()
        .into_iter()
        .filter_map(|w| w.downcast::<gtk::Window>().ok())
        .filter(|w| w.is_visible() && !keep.contains(w) && !w.is::<adw::ApplicationWindow>())
        .map(|w| {
            w.set_visible(false);
            w.downgrade()
        })
        .collect();
    root.present();
    if hidden.is_empty() {
        return;
    }
    dialog.as_ref().connect_closed(move |_| {
        for w in hidden.iter().filter_map(|w| w.upgrade()) {
            w.set_visible(true);
        }
    });
}
