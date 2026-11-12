use serde::{Deserialize, Serialize};
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
    /// What MIDI recording over the cycle makes. Logic keeps this per project.
    pub midi_cycle: MidiCycle,
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
            default_snap: 1,
            disabled_plugins: Vec::new(),
            processing_threads: 0,
            loop_folders: Vec::new(),
            count_in_bars: 0,
            midi_cycle: MidiCycle::TakeFolder,
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
    std::env::var_os("SOLSTICE_DEV").filter(|v| !v.is_empty()).map(PathBuf::from)
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
