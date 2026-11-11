mod browser;
mod canvas;
mod catalog;
mod controlbar;
mod editor;
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
mod window;

use adw::prelude::*;
use settings::Settings;
use studio::{Change, Ctx, Studio};
use solstice_model::Bundle;

const CSS: &str = include_str!("style.css");

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(window::APP_ID).build();
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
        let recent = settings.recent.clone().filter(|p| Bundle::is_bundle(p));
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
        prefs::rescan_plugins(&ctx, false, None);
    });
    app.run()
}
