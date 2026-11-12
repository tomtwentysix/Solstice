mod audio_editor;
mod automation;
mod browser;
mod canvas;
mod channel_eq;
mod chooser;
mod catalog;
mod controlbar;
mod dnd;
mod drum_editor;
mod editor;
mod event_list;
mod fft;
mod gestures;
mod global;
mod host;
mod import;
mod inspector;
mod logic_import;
mod loops;
mod mixer;
mod musical_typing;
mod peaks;
mod plugins;
mod prefs;
mod presets;
mod sample_editor;
mod samples;
mod settings;
mod studio;
mod takes_editor;
mod templates;
mod timeline;
mod variants;
mod window;
mod yabridge;

use adw::prelude::*;
use settings::{Settings, Startup};
use studio::{Change, Ctx, Studio};
use solstice_model::Bundle;

const CSS: &str = include_str!("style.css");

fn main() -> gtk::glib::ExitCode {
    let mut flags = gtk::gio::ApplicationFlags::empty();
    if settings::dev_root().is_some() {
        flags |= gtk::gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(window::APP_ID).flags(flags).build();
    app.connect_startup(|_| {
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });
    app.connect_activate(|app| {
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        let settings = Settings::load();
        let startup = settings.startup;
        let recent = settings.recent.clone().filter(|p| startup == Startup::LastProject && Bundle::is_bundle(p));
        let studio = match recent.as_ref().and_then(|p| Bundle::new(p).load().ok().map(|pr| (pr, Bundle::new(p)))) {
            Some((project, bundle)) => Studio::new(project, bundle, settings),
            None => {
                let (project, bundle) = studio::untitled(48000);
                Studio::new(project, bundle, settings)
            }
        };
        let ctx = Ctx::new(studio);
        ctx.m().catalog = catalog::cached();
        ctx.m().catalog.splice(0..0, host::builtin_catalog());
        ctx.m().start_audio();
        let win = window::build(app, &ctx);
        win.present();
        ctx.emit(Change::Project);
        if startup == Startup::Chooser {
            chooser::present(&ctx, &win);
        } else {
            window::offer_recovery(&ctx, &win);
        }
        prefs::rescan_plugins(&ctx, false, None);
    });
    app.run()
}
