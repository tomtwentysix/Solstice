use crate::settings::cache_dir;
use serde::{Deserialize, Serialize};
use solstice_vst3::ClassInfo;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
struct Entry {
    path: PathBuf,
    mtime: u64,
    classes: Vec<ClassInfo>,
    #[serde(default)]
    error: Option<String>,
}

fn cache_file() -> PathBuf {
    cache_dir().join("plugins.json")
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn load_entries() -> Vec<Entry> {
    std::fs::read(cache_file()).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default()
}

pub fn cached() -> Vec<ClassInfo> {
    flatten(load_entries())
}

fn flatten(entries: Vec<Entry>) -> Vec<ClassInfo> {
    let mut v: Vec<ClassInfo> = entries.into_iter().flat_map(|e| e.classes).collect();
    v.sort_by_key(|a| a.name.to_lowercase());
    v
}

fn scanner() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("solstice-scan")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("solstice-scan"))
}

/// Scans one module in a child process so a crashing plug-in can't take the app down.
fn scan_one(path: &Path) -> Result<Vec<ClassInfo>, String> {
    let mut child = Command::new(scanner())
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run the scanner: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out while loading".into());
            }
        }
    }
    let (mut out, mut err) = (String::new(), String::new());
    if let Some(mut s) = child.stdout.take() {
        let _ = std::io::Read::read_to_string(&mut s, &mut out);
    }
    if let Some(mut s) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut s, &mut err);
    }
    match serde_json::from_str::<Vec<ClassInfo>>(out.trim()) {
        Ok(v) => Ok(v),
        Err(_) if !err.trim().is_empty() => Err(err.trim().to_string()),
        Err(_) => Err("crashed while loading".into()),
    }
}

/// Modules that failed their last scan, with the reason.
pub fn failures() -> Vec<(PathBuf, String)> {
    load_entries().into_iter().filter_map(|e| e.error.map(|err| (e.path, err))).collect()
}

/// Rescans modules that are new or changed since the cache was written. Blocking.
pub fn scan(paths: &[PathBuf], force: bool) -> Vec<ClassInfo> {
    let old = if force { Vec::new() } else { load_entries() };
    let mut modules = solstice_vst3::find_modules(paths);
    let clap_dirs: Vec<PathBuf> = paths.iter().cloned().chain(solstice_clap::default_paths()).collect();
    modules.extend(solstice_clap::find_modules(&clap_dirs));
    let mut entries: Vec<Entry> = modules
        .into_iter()
        .map(|path| {
            let m = mtime(&path);
            match old.iter().find(|e| e.path == path && e.mtime == m) {
                Some(e) => Entry { path, mtime: m, classes: e.classes.clone(), error: e.error.clone() },
                None => match scan_one(&path) {
                    Ok(classes) => Entry { classes, path, mtime: m, error: None },
                    Err(e) => Entry { classes: Vec::new(), path, mtime: m, error: Some(e) },
                },
            }
        })
        .collect();
    // LV2 bundles are only metadata until a plug-in is opened, so they're read here rather than
    // in the scanner, and all at once.
    for c in solstice_lv2::scan_all() {
        match entries.iter_mut().find(|e| e.path == c.path) {
            Some(e) => e.classes.push(c),
            None => entries.push(Entry { mtime: mtime(&c.path), path: c.path.clone(), classes: vec![c], error: None }),
        }
    }
    let _ = std::fs::create_dir_all(cache_dir());
    if let Ok(json) = serde_json::to_vec(&entries) {
        let _ = std::fs::write(cache_file(), json);
    }
    flatten(entries)
}
