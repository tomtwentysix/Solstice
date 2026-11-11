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
        }
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
