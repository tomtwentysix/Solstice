use std::path::{Path, PathBuf};

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    v.sort();
    v
}

pub fn wine_prefixes(home: &Path, wineprefix: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = wineprefix.map(Path::to_path_buf).into_iter().collect();
    candidates.push(home.join(".wine"));
    // Bottles keeps one prefix per bottle, natively or under its Flatpak; Lutris games live under ~/Games by default
    candidates.extend(subdirs(&home.join(".local/share/bottles/bottles")));
    candidates.extend(subdirs(&home.join(".var/app/com.usebottles.bottles/data/bottles/bottles")));
    candidates.extend(subdirs(&home.join("Games")));
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if c.join("drive_c").is_dir() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

pub fn windows_plugin_dirs(prefix: &Path) -> Vec<PathBuf> {
    let common = prefix.join("drive_c/Program Files/Common Files");
    ["VST3", "CLAP"].iter().map(|d| common.join(d)).filter(|p| p.is_dir()).collect()
}

pub fn linux_plugin_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let standard: Vec<PathBuf> = solstice_vst3::default_paths().into_iter().chain(solstice_clap::default_paths()).collect();
    let mut out = Vec::new();
    for root in roots {
        walk(root, 0, &standard, &mut out);
    }
    out
}

fn walk(dir: &Path, depth: u32, standard: &[PathBuf], out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    let is_plugin = |p: &PathBuf| p.extension().is_some_and(|x| x == "vst3" || x == "clap");
    if entries.iter().any(is_plugin) && !standard.iter().any(|s| s == dir) && !out.iter().any(|o| o == dir) {
        out.push(dir.to_path_buf());
    }
    if depth < 3 {
        for p in entries.iter().filter(|p| p.is_dir() && !is_plugin(p)) {
            walk(p, depth + 1, standard, out);
        }
    }
}

pub fn extra_roots(home: &Path) -> Vec<PathBuf> {
    vec![PathBuf::from("/opt"), home.join(".local/lib")]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("solstice-plugin-search-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_prefixes_and_their_plugin_folders() {
        let home = tempdir();
        let vst3 = home.join(".wine/drive_c/Program Files/Common Files/VST3");
        std::fs::create_dir_all(&vst3).unwrap();
        std::fs::create_dir_all(home.join(".local/share/bottles/bottles/Audio/drive_c")).unwrap();
        std::fs::create_dir_all(home.join("Games/not-a-prefix")).unwrap();
        let prefixes = wine_prefixes(&home, Some(&home.join(".wine")));
        assert_eq!(prefixes, [home.join(".wine"), home.join(".local/share/bottles/bottles/Audio")]);
        assert_eq!(windows_plugin_dirs(&prefixes[0]), [vst3]);
        assert!(windows_plugin_dirs(&prefixes[1]).is_empty());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn finds_linux_plugins_outside_the_standard_folders() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("Vendor/lib/vst3/Synth.vst3")).unwrap();
        std::fs::create_dir_all(root.join("Other/nothing")).unwrap();
        assert_eq!(linux_plugin_dirs(&[root.clone()]), [root.join("Vendor/lib/vst3")]);
        assert!(linux_plugin_dirs(&[root.join("missing")]).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
