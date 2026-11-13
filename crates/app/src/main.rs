mod audio_editor;
mod automation;
mod browser;
mod canvas;
mod channel_eq;
mod chooser;
mod context_menu;
mod dialogs;
mod catalog;
mod controlbar;
mod dnd;
mod embed;
mod drum_editor;
mod drum_patterns;
mod editor;
mod event_list;
mod export;
mod fft;
mod gestures;
mod global;
mod grooves;
mod host;
mod import;
mod inspector;
mod loops;
mod loudness;
mod master_meter;
mod mixer;
mod musical_typing;
mod native_ui;
mod peaks;
mod plugins;
mod prefs;
mod project_import;
mod reference;
mod projects;
mod render;
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
use solstice_model::Bundle;

const CSS: &str = include_str!("style.css");

/// Solstice's styles, for the main display and the X11 one plug-in editors open on.
pub fn add_css(display: &gtk::gdk::Display) {
    let css = gtk::CssProvider::new();
    css.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

fn main() -> gtk::glib::ExitCode {
    gtk::gio::resources_register_include!("solstice.gresource").expect("bundled resources");
    let mut flags = gtk::gio::ApplicationFlags::empty();
    if settings::dev_root().is_some() {
        flags |= gtk::gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(window::APP_ID).flags(flags).build();
    app.connect_startup(|app| {
        projects::follow_active_window(app);
        let display = gtk::gdk::Display::default().unwrap();
        add_css(&display);
        gtk::IconTheme::for_display(&display).add_resource_path("/uk/co/solarflareit/Solstice/icons");
    });
    app.connect_activate(|app| {
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        let settings = Settings::load();
        let startup = settings.startup;
        let recent = settings.recent.clone().filter(|p| startup == Startup::LastProject && Bundle::is_bundle(p));
        let (project, bundle) = match recent.as_ref().and_then(|p| Bundle::new(p).load().ok().map(|pr| (pr, Bundle::new(p)))) {
            Some(opened) => opened,
            None => studio::untitled(48000),
        };
        projects::open_window(app, project, bundle, startup == Startup::Chooser, false);
    });
    app.run()
}
