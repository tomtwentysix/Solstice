use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The rate Solstice asks the audio server for; `None` leaves it to the server.
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// The interface Solstice wires to, as `device_of` names it; `None` wires every one.
    pub interface: Option<String>,
    pub auto_input: bool,
    pub low_latency: bool,
    pub low_latency_ms: f32,
    pub vst3_paths: Vec<PathBuf>,
    pub recent: Option<PathBuf>,
    pub recent_list: Vec<PathBuf>,
    pub startup: Startup,
    /// Minutes between autosaves; 0 turns autosave off.
    pub autosave_minutes: u32,
    pub undo_levels: usize,
    /// Physical MIDI sources Solstice should not listen to.
    pub disabled_midi: Vec<String>,
    /// Play the selected instrument track from incoming MIDI.
    pub midi_thru: bool,
    pub click_level_db: f32,
    pub click_accent: bool,
    /// Click only on the first beat of each group, so 6/8 clicks twice a bar.
    pub click_groups_only: bool,
    /// Index into the snap menu (Bar, Beat, 1/8, 1/16, Off).
    pub default_snap: u32,
    /// Plug-in class IDs hidden from the pickers.
    pub disabled_plugins: Vec<String>,
    /// Threads that process tracks, the audio thread included; 0 picks from the CPU count.
    pub processing_threads: u32,
    /// Folders the Loop Browser lists.
    pub loop_folders: Vec<PathBuf>,
    /// Bars of click before recording starts from a stop; 0 for none.
    pub count_in_bars: u32,
    /// Bars played before the record position when recording starts from a stop; 0 for none.
    pub preroll_bars: u32,
    /// What MIDI recording over the cycle makes, kept per project.
    pub midi_cycle: MidiCycle,
    /// Samples added to the interface's reported round trip when placing takes; may be negative.
    pub record_adjust: i32,
    /// Start the Loudness window's measurement afresh when playback starts from a stop.
    pub loudness_reset_on_play: bool,
    /// Play reference tracks at the mix's loudness.
    pub reference_match: bool,
    /// The Tuner's reference pitch for A4, in Hz.
    pub tuner_a4: f32,
    /// Silence the main outputs while the Tuner window is open.
    pub tuner_mute: bool,
    pub export: crate::render::ExportChoices,
    /// Categories the user gave plug-ins, by class ID, overriding the shipped table.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_categories: BTreeMap<String, Vec<String>>,
    pub plugin_group: PluginGroup,
    /// Tall inserts with pictures on untyped strips too, as typed strips always have.
    pub strip_pictures: bool,
    /// The starter set of captures and impulse responses last put into the libraries.
    pub starter_installed: u32,
    /// The first-run setup version the user last completed; 0 means never.
    pub setup_version: u32,
    pub projects_folder: Option<PathBuf>,
    /// `None` exports next to the projects.
    pub exports_folder: Option<PathBuf>,
}

/// How the plug-in list is grouped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginGroup {
    #[default]
    Type,
    Developer,
    Name,
}

impl PluginGroup {
    pub const ALL: [PluginGroup; 3] = [PluginGroup::Type, PluginGroup::Developer, PluginGroup::Name];

    pub fn label(self) -> &'static str {
        match self {
            PluginGroup::Type => "Type",
            PluginGroup::Developer => "Developer",
            PluginGroup::Name => "Name",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiCycle {
    /// A take folder with a take for each pass.
    #[default]
    TakeFolder,
    /// One region holding every pass.
    Merge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Startup {
    #[default]
    Chooser,
    LastProject,
    EmptyProject,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            sample_rate: None,
            buffer_size: None,
            interface: None,
            auto_input: true,
            low_latency: false,
            low_latency_ms: 5.0,
            vst3_paths: Vec::new(),
            recent: None,
            recent_list: Vec::new(),
            startup: Startup::Chooser,
            autosave_minutes: 2,
            undo_levels: 200,
            disabled_midi: Vec::new(),
            midi_thru: true,
            click_level_db: -6.0,
            click_accent: true,
            click_groups_only: false,
            default_snap: 1,
            disabled_plugins: Vec::new(),
            processing_threads: 0,
            loop_folders: Vec::new(),
            count_in_bars: 0,
            preroll_bars: 0,
            midi_cycle: MidiCycle::TakeFolder,
            record_adjust: 0,
            loudness_reset_on_play: true,
            reference_match: true,
            tuner_a4: 440.0,
            tuner_mute: false,
            export: Default::default(),
            plugin_categories: BTreeMap::new(),
            plugin_group: PluginGroup::Type,
            strip_pictures: false,
            starter_installed: 0,
            setup_version: 0,
            projects_folder: None,
            exports_folder: None,
        }
    }
}

impl Settings {
    /// Extra threads to start beside the audio thread.
    pub fn workers(&self) -> usize {
        let threads = match self.processing_threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()).min(8),
            n => n as usize,
        };
        threads.saturating_sub(1)
    }

    pub fn projects_dir(&self) -> PathBuf {
        if let Some(d) = &self.projects_folder {
            return d.clone();
        }
        if let Some(d) = dev_root() {
            return d.join("projects");
        }
        gtk::glib::user_special_dir(gtk::glib::UserDirectory::Music).unwrap_or_else(gtk::glib::home_dir).join("Solstice")
    }

    pub fn exports_dir(&self) -> PathBuf {
        self.exports_folder.clone().unwrap_or_else(|| self.projects_dir())
    }
}

/// `SOLSTICE_DEV=<folder>` runs a separate instance whose settings, cache and projects live in
/// that folder, so test runs never touch (or hand off to) the everyday instance.
pub fn dev_root() -> Option<PathBuf> {
    let root = std::env::var_os("SOLSTICE_DEV").filter(|v| !v.is_empty()).map(PathBuf::from);
    // Tests get a throwaway root so they never write the real settings, recent list or user data.
    #[cfg(test)]
    let root = root.or_else(|| Some(std::env::temp_dir().join(format!("solstice-test-{}", std::process::id()))));
    root
}

fn path() -> PathBuf {
    match dev_root() {
        Some(d) => d.join("config/settings.json"),
        None => gtk::glib::user_config_dir().join("solstice/settings.json"),
    }
}

pub fn cache_dir() -> PathBuf {
    match dev_root() {
        Some(d) => d.join("cache"),
        None => gtk::glib::user_cache_dir().join("solstice"),
    }
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(path()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        #[cfg(test)]
        assert!(
            p.starts_with(std::env::temp_dir())
                || std::env::var_os("SOLSTICE_DEV").is_some_and(|d| !d.is_empty() && p.starts_with(d)),
            "a test tried to save settings to {}",
            p.display()
        );
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }

    /// Writes `starter_installed` into the settings file and nothing else, so a file this
    /// build could not load (a hand edit, or a newer build's values) is not replaced by defaults.
    /// A file that is not a JSON object is left alone and the set is recorded on a later launch.
    pub fn record_starter(set: u32) {
        record_starter_in(&path(), set);
    }

    pub fn search_paths(&self) -> Vec<PathBuf> {
        let mut v = solstice_vst3::default_paths();
        v.extend(self.vst3_paths.iter().cloned());
        v
    }
}

fn record_starter_in(p: &Path, set: u32) -> bool {
    let mut json = match std::fs::read(p) {
        Ok(d) => match serde_json::from_slice::<serde_json::Value>(&d) {
            Ok(serde_json::Value::Object(o)) => o,
            _ => return false,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(_) => return false,
    };
    json.insert("starter_installed".into(), set.into());
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    serde_json::to_vec_pretty(&json).is_ok_and(|d| std::fs::write(p, d).is_ok())
}

#[cfg(test)]
mod tests {
    #[test]
    fn recording_the_starter_set_keeps_the_rest_of_the_file() {
        let d = std::env::temp_dir().join(format!("solstice-test-{}/record-starter", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("settings.json");
        assert!(super::record_starter_in(&p, 1));
        assert_eq!(serde_json::from_slice::<super::Settings>(&std::fs::read(&p).unwrap()).unwrap().starter_installed, 1);

        let newer = r#"{"vst3_paths": ["/opt/vst3"], "startup": "SomethingNewer", "starter_installed": 0}"#;
        std::fs::write(&p, newer).unwrap();
        assert!(super::record_starter_in(&p, 2));
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["startup"], "SomethingNewer");
        assert_eq!(v["vst3_paths"][0], "/opt/vst3");
        assert_eq!(v["starter_installed"], 2);

        let broken = b"{\"vst3_paths\": [\"/opt/vst3\"],}";
        std::fs::write(&p, broken).unwrap();
        assert!(!super::record_starter_in(&p, 2));
        assert_eq!(std::fs::read(&p).unwrap(), broken);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tests_keep_settings_out_of_the_real_config() {
        assert!(super::path().starts_with(std::env::temp_dir()));
        assert!(super::cache_dir().starts_with(std::env::temp_dir()));
    }

    #[test]
    fn old_settings_keep_their_values() {
        let old = r#"{"buffer_size": 128, "interface": "Midas UMC22", "vst3_paths": ["/opt/vst3"], "starter_installed": 1}"#;
        let s: super::Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.buffer_size, Some(128));
        assert_eq!(s.interface.as_deref(), Some("Midas UMC22"));
        assert_eq!(s.setup_version, 0);
        assert_eq!(s.projects_folder, None);
        assert_eq!(s.exports_folder, None);
    }

    #[test]
    fn export_folder_follows_projects_folder() {
        let mut s = super::Settings::default();
        s.projects_folder = Some("/srv/audio/Projects".into());
        assert_eq!(s.projects_dir(), std::path::PathBuf::from("/srv/audio/Projects"));
        assert_eq!(s.exports_dir(), std::path::PathBuf::from("/srv/audio/Projects"));
        s.exports_folder = Some("/srv/audio/Mixes".into());
        assert_eq!(s.exports_dir(), std::path::PathBuf::from("/srv/audio/Mixes"));
    }
}
