//! Plug-in editors shown inside the plug-in window, under its header.
//!
//! Editors are X11 windows and GTK 4 can't hold a foreign window, so a plug-in window with an
//! editor is opened on the X server (XWayland on a Wayland desktop) and the editor is made a
//! child of it, positioned over a placeholder widget. The rest of Solstice stays on Wayland.

use crate::host::Host;
use gtk::prelude::*;
use gtk::{gdk, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    static DISPLAY: RefCell<Option<Option<gdk::Display>>> = const { RefCell::new(None) };
}

/// The X11 display plug-in windows open on, if there is one.
pub fn x11_display() -> Option<gdk::Display> {
    DISPLAY.with(|d| {
        d.borrow_mut()
            .get_or_insert_with(|| {
                let default = gdk::Display::default()?;
                if default.is::<gdk_x11::X11Display>() {
                    return Some(default);
                }
                let name = std::env::var("DISPLAY").ok().filter(|n| !n.is_empty())?;
                let display = gdk::DisplayManager::get().open_display(Some(&name)).filter(|d| d.is::<gdk_x11::X11Display>())?;
                crate::add_css(&display);
                Some(display)
            })
            .clone()
    })
}

/// Shows `inst`'s editor over `area`, which keeps the editor's size. The editor follows the
/// area while it is shown and is moved out of view while it isn't.
pub fn attach(win: &adw::Window, area: &gtk::Box, inst: &Host) {
    let opened = Rc::new(Cell::new(false));
    let last = Rc::new(Cell::new((i32::MIN, i32::MIN, 0, 0)));
    let announced = Rc::new(Cell::new(None));
    let place = {
        let (win, area, inst, opened, last, announced) =
            (win.clone(), area.clone(), inst.clone(), opened.clone(), last.clone(), announced.clone());
        move || {
            let Some(surface) = win.surface() else { return };
            let Some(xid) = surface.downcast_ref::<gdk_x11::X11Surface>().map(|s| s.xid()) else { return };
            let scale = surface.scale_factor();
            let (tx, ty) = win.surface_transform();
            let at = area.compute_point(&win, &graphene::Point::new(0.0, 0.0)).unwrap_or(graphene::Point::new(0.0, 0.0));
            let shown = area.is_mapped() && area.is_child_visible() && area.width() > 0;
            let (x, y) = if shown {
                (((at.x() as f64 + tx) * scale as f64) as i32, ((at.y() as f64 + ty) * scale as f64) as i32)
            } else {
                (-20000, 0)
            };
            if !opened.get() {
                if !shown {
                    return;
                }
                opened.set(inst.open_editor_in(xid, x, y));
                if !opened.get() {
                    return;
                }
            }
            let (w, h) = inst.editor_size();
            let state = (x, y, w, h);
            if last.get() != state {
                last.set(state);
                inst.move_editor(x, y);
                area.set_size_request((w as f64 / scale as f64).ceil() as i32, (h as f64 / scale as f64).ceil() as i32);
            }
            // Bridged editors take their mouse input from where Wine thinks they are on screen,
            // which only window-manager style notices keep right; moving the plug-in window
            // or anything above the editor would otherwise offset every click.
            let xw = if inst.bridged() { inst.editor_window() } else { 0 };
            if shown
                && xw != 0
                && let Some(g) = solstice_vst3::x11::geometry(xw)
                && announced.get() != Some(g)
            {
                announced.set(Some(g));
                solstice_vst3::x11::announce(xw, g);
            }
        }
    };
    let (w, h) = inst.editor_size();
    if w > 0 && h > 0 {
        area.set_size_request(w, h);
    } else {
        area.set_size_request(480, 320);
    }
    // On the window rather than the area, so it still runs to hide the editor when the
    // Controls page replaces it.
    win.add_tick_callback(move |_, _| {
        place();
        glib::ControlFlow::Continue
    });
}
