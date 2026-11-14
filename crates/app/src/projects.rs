//! Open projects, one window each. Only the frontmost project has the audio
//! engine: it moves to whichever project window becomes active.

use crate::settings::Settings;
use crate::studio::{Change, Ctx, Studio};
use adw::prelude::*;
use gtk::glib;
use solstice_model::{Bundle, Project};
use std::cell::RefCell;
use std::path::Path;

thread_local! {
    static OPEN: RefCell<Vec<(Ctx, glib::WeakRef<adw::ApplicationWindow>)>> = RefCell::default();
}

/// Writes every open project's recovery copy, as a last act when Solstice is going down.
pub fn emergency_autosave() {
    for (ctx, _) in open() {
        // Fails if the panic came in the middle of an edit; that project keeps its last autosave.
        if let Ok(s) = ctx.studio.try_borrow()
            && s.history.is_dirty()
        {
            let _ = s.bundle.autosave(&s.project);
        }
    }
}

pub fn ctx_of(win: &adw::ApplicationWindow) -> Option<Ctx> {
    open().into_iter().find(|(_, w)| w == win).map(|(c, _)| c)
}

fn open() -> Vec<(Ctx, adw::ApplicationWindow)> {
    OPEN.with(|o| o.borrow().iter().filter_map(|(c, w)| w.upgrade().map(|w| (c.clone(), w))).collect())
}

/// The window showing the project in `dir`, if it is open.
pub fn window_for(dir: &Path) -> Option<adw::ApplicationWindow> {
    open().into_iter().find(|(c, _)| c.s().bundle.dir == dir).map(|(_, w)| w)
}

/// Gives the audio engine to `ctx`, taking it from any other open project.
fn hand_audio(ctx: &Ctx) {
    // A take in progress keeps the audio until it stops.
    if let Some((busy, _)) = open().into_iter().find(|(o, _)| o.key() != ctx.key() && o.s().is_recording()) {
        let name = busy.s().project.name.clone();
        ctx.message(&format!("Audio stays with “{name}” until its recording stops"));
        return;
    }
    for (other, _) in open() {
        if other.key() != ctx.key() {
            // Also cancels a reconnection the other project's render was going to make.
            let had = other.s().io.is_some();
            other.m().stop_audio();
            if had {
                other.emit(Change::Engine);
            }
        }
    }
    if ctx.s().io.is_none() {
        // Another window may have changed Preferences since this project last had the engine.
        let fresh = Settings::load();
        ctx.m().settings = fresh;
        ctx.m().start_audio();
        ctx.emit(Change::Engine);
    }
}

/// Moves audio along with the active window; call once at start-up.
pub fn follow_active_window(app: &adw::Application) {
    app.connect_active_window_notify(|app| {
        let Some(active) = app.active_window() else { return };
        if let Some((ctx, _)) = open().into_iter().find(|(_, w)| w.upcast_ref::<gtk::Window>() == &active) {
            hand_audio(&ctx);
        }
    });
}

/// Opens a project in a new window. With `chooser`, the Project Chooser is shown over it;
/// `close_if_cancelled` closes the window again when the chooser is dismissed.
pub fn open_window(app: &adw::Application, project: Project, bundle: Bundle, chooser: bool, close_if_cancelled: bool) -> adw::ApplicationWindow {
    let ctx = Ctx::new(Studio::new(project, bundle, Settings::load()));
    let catalog = open().first().map(|(c, _)| c.s().catalog.clone());
    match catalog {
        Some(c) => ctx.m().catalog = c,
        None => {
            ctx.m().catalog = crate::catalog::cached();
            ctx.m().catalog.splice(0..0, crate::host::builtin_catalog());
        }
    }
    let first = open().is_empty();
    if Bundle::is_bundle(&ctx.s().bundle.dir) {
        ctx.m().remember_recent();
    }
    let win = crate::window::build(app, &ctx);
    OPEN.with(|o| o.borrow_mut().push((ctx.clone(), win.downgrade())));
    {
        let ctx = ctx.clone();
        win.connect_destroy(move |w| {
            for t in gtk::Window::list_toplevels() {
                if let Ok(t) = t.downcast::<gtk::Window>()
                    && t.transient_for().as_ref() == Some(w.upcast_ref())
                {
                    t.close();
                }
            }
            // The render's graph holds plug-ins; it must be gone before they are closed.
            ctx.m().abort_render();
            ctx.m().stop_audio();
            ctx.m().plugins.clear();
            // Widgets' listeners hold the project; dropping them lets it go.
            ctx.clear_listeners();
            OPEN.with(|o| o.borrow_mut().retain(|(c, w)| c.key() != ctx.key() && w.upgrade().is_some()));
        });
    }
    hand_audio(&ctx);
    win.present();
    ctx.emit(Change::Project);
    let proceed = {
        let (ctx, win) = (ctx.clone(), win.downgrade());
        move || {
            let Some(win) = win.upgrade() else { return };
            if chooser {
                crate::chooser::present(&ctx, &win, close_if_cancelled);
            } else {
                crate::window::offer_recovery(&ctx, &win);
            }
        }
    };
    if first && ctx.s().settings.setup_version < crate::setup::SETUP_VERSION {
        crate::setup::present(&ctx, &win, crate::setup::Page::Realtime, proceed);
    } else {
        proceed();
    }
    if first {
        crate::prefs::rescan_plugins(&ctx, false, None);
    }
    win
}

/// Opens the project in `dir`: brings its window forward if it is already open, loads it into
/// `ctx`'s window if that only holds an untouched new project, and otherwise opens a new window.
pub fn open_project(ctx: &Ctx, win: &adw::ApplicationWindow, dir: &Path) -> Result<(), String> {
    if let Some(w) = window_for(dir) {
        w.present();
        return Ok(());
    }
    let unsaved = !Bundle::is_bundle(dir);
    let untouched = !ctx.s().history.is_dirty() && !Bundle::is_bundle(&ctx.s().bundle.dir);
    let target = if untouched {
        if ctx.s().is_recording() {
            return Err("Stop recording before opening another project".into());
        }
        ctx.m().open(dir).map_err(|e| format!("Could not open: {e}"))?;
        ctx.emit(Change::Project);
        ctx.emit(Change::Selection);
        crate::window::offer_recovery(ctx, win);
        ctx.clone()
    } else {
        let bundle = Bundle::new(dir);
        let (project, note) = bundle.load_noting().map_err(|e| format!("Could not open: {e}"))?;
        let app = win.application().and_downcast::<adw::Application>().ok_or("No application")?;
        let new = open_window(&app, project, bundle, false, false);
        let ctx = ctx_of(&new).ok_or("No window")?;
        if let Some(n) = note {
            ctx.message(&n);
        }
        ctx
    };
    // A bundle that was never saved opened from its autosave or recordings; saving writes it properly.
    if unsaved {
        target.m().history.mark_dirty();
        target.message("This project was never saved, so it was rebuilt from what was left in its folder. Save it to keep it.");
    }
    Ok(())
}
