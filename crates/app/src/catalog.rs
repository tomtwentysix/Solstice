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
    let mut cmd = Command::new(scanner());
    cmd.arg(path);
    let (out, err) = run_captured(cmd, TIMEOUT)?;
    match serde_json::from_str::<Vec<ClassInfo>>(out.trim()) {
        Ok(v) => Ok(v),
        Err(_) if !err.trim().is_empty() => Err(err.trim().to_string()),
        Err(_) => Err("crashed while loading".into()),
    }
}

/// Runs `cmd` to completion and returns its (stdout, stderr), killing it after `timeout`.
fn run_captured(mut cmd: Command, timeout: Duration) -> Result<(String, String), String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run the scanner: {e}"))?;
    // The pipes are drained on their own threads: a child that fills a pipe buffer blocks on
    // write and would never exit while we only poll for it.
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out while loading".into());
            }
        }
    }
    Ok((out.join().unwrap_or_default(), err.join().unwrap_or_default()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_captured_drains_output_larger_than_a_pipe_buffer() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2"]);
        let (out, err) = run_captured(cmd, Duration::from_secs(5)).unwrap();
        assert_eq!(out.len(), 300_000);
        assert_eq!(err.len(), 300_000);
    }

    #[test]
    fn run_captured_kills_on_timeout() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let start = Instant::now();
        assert!(run_captured(cmd, Duration::from_millis(100)).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
