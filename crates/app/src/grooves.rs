//! Groove templates for the piano roll's Quantize: the factory ones, then the user's own,
//! saved as JSON in `<data dir>/grooves/`.

use gtk::glib;
use solstice_model::{Groove, groove};
use std::path::PathBuf;

fn dir() -> PathBuf {
    match crate::settings::dev_root() {
        Some(d) => d.join("data/grooves"),
        None => glib::user_data_dir().join("solstice/grooves"),
    }
}

fn file(name: &str) -> PathBuf {
    dir().join(format!("{}.json", name.replace('/', "-")))
}

fn user() -> Vec<Groove> {
    let mut v: Vec<Groove> = std::fs::read_dir(dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok())
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a: &Groove, b| a.name.cmp(&b.name));
    v
}

pub fn all() -> Vec<Groove> {
    let mut v = groove::factory();
    v.extend(user());
    v
}

/// Whether `name` is taken by a factory groove or would overwrite a saved one.
pub fn exists(name: &str) -> bool {
    groove::factory().iter().any(|g| g.name == name) || file(name).exists()
}

pub fn save(g: &Groove) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let json = serde_json::to_vec_pretty(g).map_err(std::io::Error::other)?;
    std::fs::write(file(&g.name), json)
}

pub fn delete(name: &str) -> std::io::Result<()> {
    std::fs::remove_file(file(name))
}
