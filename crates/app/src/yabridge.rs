//! Windows plug-ins through yabridge, which bridges Windows VST3s installed in a Wine prefix
//! into `~/.vst3/yabridge` as Linux VST3s that Solstice hosts like any other.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ctl() -> Option<PathBuf> {
    let local = gtk::glib::user_data_dir().join("yabridge/yabridgectl");
    if local.is_file() {
        return Some(local);
    }
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join("yabridgectl")).find(|p| p.is_file())
}

pub fn lib_dir() -> Option<PathBuf> {
    ctl()?.parent().map(Path::to_path_buf)
}

/// The standard Windows VST3 folder in the default Wine prefix, if it exists.
pub fn default_vst3_folder() -> Option<PathBuf> {
    let p = gtk::glib::home_dir().join(".wine/drive_c/Program Files/Common Files/VST3");
    p.is_dir().then_some(p)
}

fn run(args: &[&str]) -> Result<String, String> {
    let ctl = ctl().ok_or("yabridge is not installed")?;
    let out = Command::new(ctl).args(args).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() { Ok(text) } else { Err(text.trim().to_string()) }
}

/// Folders yabridge watches for Windows plug-ins.
pub fn folders() -> Vec<PathBuf> {
    run(&["list"])
        .map(|t| parse_list(&t))
        .unwrap_or_default()
}

pub fn parse_list(text: &str) -> Vec<PathBuf> {
    text.lines().map(str::trim).filter(|l| l.starts_with('/')).map(PathBuf::from).collect()
}

pub fn add(folder: &Path) -> Result<String, String> {
    run(&["add", &folder.to_string_lossy()])
}

pub fn remove(folder: &Path) -> Result<String, String> {
    run(&["rm", &folder.to_string_lossy()])
}

/// Sets up or updates the bridged copies. Returns yabridgectl's summary.
pub fn sync() -> Result<String, String> {
    run(&["sync"]).map(|t| t.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("Done").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_folder_list() {
        let text = "/home/me/.wine/drive_c/Program Files/Common Files/VST3\n/home/me/other prefix/VST3\n";
        let v = parse_list(text);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1], PathBuf::from("/home/me/other prefix/VST3"));
        assert!(parse_list("No plugin locations configured.").is_empty());
    }
}
