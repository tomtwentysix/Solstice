mod browser;
mod canvas;
mod channel_eq;
mod catalog;
mod controlbar;
mod dnd;
mod editor;
mod fft;
mod gestures;
mod import;
mod inspector;
mod mixer;
mod peaks;
mod plugins;
mod prefs;
mod settings;
mod studio;
mod timeline;
mod variants;
mod window;

use adw::prelude::*;
use settings::Settings;
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
        let recent = settings.recent.clone().filter(|p| !settings.start_new && Bundle::is_bundle(p));
        let studio = match recent.as_ref().and_then(|p| Bundle::new(p).load().ok().map(|pr| (pr, Bundle::new(p)))) {
            Some((project, bundle)) => Studio::new(project, bundle, settings),
            None => {
                let (project, bundle) = studio::untitled(48000);
                Studio::new(project, bundle, settings)
            }
        };
        let ctx = Ctx::new(studio);
        ctx.m().catalog = catalog::cached();
        ctx.m().start_audio();
        let win = window::build(app, &ctx);
        win.present();
        ctx.emit(Change::Project);
        window::offer_recovery(&ctx, &win);
        prefs::rescan_plugins(&ctx, false, None);
    });
    app.run()
}
