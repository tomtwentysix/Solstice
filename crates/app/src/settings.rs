use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub buffer_size: Option<u32>,
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
    /// What MIDI recording over the cycle makes, kept per project.
    pub midi_cycle: MidiCycle,
    /// Samples added to the interface's reported round trip when placing takes; may be negative.
    pub record_adjust: i32,
    /// Start the Loudness window's measurement afresh when playback starts from a stop.
    pub loudness_reset_on_play: bool,
    /// Play reference tracks at the mix's loudness.
    pub reference_match: bool,
    pub export: crate::render::ExportChoices,
    /// Categories the user gave plug-ins, by class ID, overriding the shipped table.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_categories: BTreeMap<String, Vec<String>>,
    pub plugin_group: PluginGroup,
    /// Tall inserts with pictures on untyped strips too, as typed strips always have.
    pub strip_pictures: bool,
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
            buffer_size: None,
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
            midi_cycle: MidiCycle::TakeFolder,
            record_adjust: 0,
            loudness_reset_on_play: true,
            reference_match: true,
            export: Default::default(),
            plugin_categories: BTreeMap::new(),
            plugin_group: PluginGroup::Type,
            strip_pictures: false,
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

    pub fn search_paths(&self) -> Vec<PathBuf> {
        let mut v = solstice_vst3::default_paths();
        v.extend(self.vst3_paths.iter().cloned());
        v
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tests_keep_settings_out_of_the_real_config() {
        assert!(super::path().starts_with(std::env::temp_dir()));
        assert!(super::cache_dir().starts_with(std::env::temp_dir()));
    }
}
