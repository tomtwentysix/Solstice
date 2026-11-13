//! The capture library for Neural Amp Modeler: `.nam` files in `<data dir>/captures/`, kept in
//! a Guitar and a Bass folder. A capture in use is copied into the project's Audio Files.

use crate::studio::{Change, Ctx};
use gtk::{gio, glib};
use solstice_dsp::Native;
use solstice_dsp::nam::{Capture, CaptureDesc};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const FOLDERS: [&str; 2] = ["Guitar", "Bass"];

/// Captures larger than this are not real ones; it also bounds what a zip entry may inflate to.
const MAX_SIZE: usize = 64 << 20;

pub fn library() -> PathBuf {
    match crate::settings::dev_root() {
        Some(d) => d.join("data/captures"),
        None => glib::user_data_dir().join("solstice/captures"),
    }
}

fn is_capture(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("nam"))
}

pub fn is_importable(p: &Path) -> bool {
    is_capture(p) || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// The captures in a library folder and any folders inside it, sorted by name.
pub fn list(folder: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for p in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
            if p.is_dir() {
                walk(&p, out);
            } else if is_capture(&p) {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(folder, &mut v);
    v.sort_by_key(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()));
    v
}

/// Writes `data` into `dir` as `name`, numbering the name if a different file has it. A file
/// with the same name and contents is reused.
fn place(dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stem = Path::new(name).file_stem().map_or("Capture".into(), |s| s.to_string_lossy().into_owned());
    let mut n = 1;
    loop {
        let candidate = dir.join(if n == 1 { format!("{stem}.nam") } else { format!("{stem} {n}.nam") });
        match std::fs::read(&candidate) {
            Ok(existing) if existing == data => return Ok(candidate),
            Ok(_) => n += 1,
            Err(_) => {
                std::fs::write(&candidate, data)?;
                return Ok(candidate);
            }
        }
    }
}

/// Copies captures, and the captures inside zips, into a library folder. Returns those added
/// and a message for each file that failed. Blocking.
pub fn import(files: &[PathBuf], folder: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (mut added, mut failed) = (Vec::new(), Vec::new());
    for f in files {
        let name = f.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let entries = if is_capture(f) {
            std::fs::read(f).map(|d| vec![(name.clone(), d)]).map_err(|e| e.to_string())
        } else {
            std::fs::read(f).map_err(|e| e.to_string()).and_then(|d| unzip_captures(&d))
        };
        match entries {
            Ok(v) if v.is_empty() => failed.push(format!("{name} holds no captures")),
            Ok(v) => {
                for (entry, data) in v {
                    match place(folder, &entry, &data) {
                        Ok(p) => added.push(p),
                        Err(e) => failed.push(format!("{entry}: {e}")),
                    }
                }
            }
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    (added, failed)
}

fn u16_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// The `.nam` entries of a zip archive, by file name without their folders. Stored and
/// deflated entries are read; ZIP64 archives and encryption are not.
fn unzip_captures(d: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    const END: u32 = 0x0605_4b50;
    const CENTRAL: u32 = 0x0201_4b50;
    const LOCAL: u32 = 0x0403_4b50;
    let bad = || "not a zip archive that can be read".to_string();
    // The end record sits in the last 22 bytes plus up to 64 KiB of comment.
    let floor = d.len().saturating_sub(22 + 65535);
    let end = (floor..d.len().saturating_sub(21)).rev().find(|&i| u32_at(d, i) == Some(END as usize)).ok_or_else(bad)?;
    let count = u16_at(d, end + 10).ok_or_else(bad)?;
    let mut at = u32_at(d, end + 16).ok_or_else(bad)?;
    let mut out = Vec::new();
    for _ in 0..count {
        if u32_at(d, at) != Some(CENTRAL as usize) {
            return Err(bad());
        }
        let flags = u16_at(d, at + 8).ok_or_else(bad)?;
        let method = u16_at(d, at + 10).ok_or_else(bad)?;
        let packed = u32_at(d, at + 20).ok_or_else(bad)?;
        let (name_len, extra_len, comment_len) = (u16_at(d, at + 28).ok_or_else(bad)?, u16_at(d, at + 30).ok_or_else(bad)?, u16_at(d, at + 32).ok_or_else(bad)?);
        let local = u32_at(d, at + 42).ok_or_else(bad)?;
        let path = String::from_utf8_lossy(d.get(at + 46..at + 46 + name_len).ok_or_else(bad)?).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        let name = path.rsplit(['/', '\\']).next().unwrap_or_default().to_string();
        // Archives made on macOS carry resource-fork copies under __MACOSX and "._" names.
        if !is_capture(Path::new(&name)) || name.starts_with("._") || path.contains("__MACOSX") {
            continue;
        }
        if flags & 1 != 0 {
            return Err(format!("{name} is encrypted"));
        }
        if u32_at(d, local) != Some(LOCAL as usize) {
            return Err(bad());
        }
        let start = local + 30 + u16_at(d, local + 26).ok_or_else(bad)? + u16_at(d, local + 28).ok_or_else(bad)?;
        let raw = d.get(start..start + packed).ok_or_else(bad)?;
        let data = match method {
            0 => raw.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(raw, MAX_SIZE).map_err(|_| format!("{name} could not be unpacked"))?,
            _ => return Err(format!("{name} is packed in a way that cannot be read")),
        };
        out.push((name, data));
    }
    Ok(out)
}

/// Copies a capture into the project's Audio Files and returns its name there. Blocking.
pub fn into_project(src: &Path, audio_dir: &Path) -> Result<String, String> {
    let data = std::fs::read(src).map_err(|e| e.to_string())?;
    if data.len() > MAX_SIZE {
        return Err("the file is too large to be a capture".into());
    }
    let name = src.file_name().map_or_else(|| "Capture.nam".into(), |n| n.to_string_lossy().into_owned());
    let placed = place(audio_dir, &name, &data).map_err(|e| e.to_string())?;
    Ok(placed.file_name().map_or(name, |n| n.to_string_lossy().into_owned()))
}

/// Where a saved capture is now: in the project, else where it came from.
pub fn resolve(desc: &CaptureDesc, audio_dir: &Path) -> Option<PathBuf> {
    let own = if desc.file.is_absolute() { desc.file.clone() } else { audio_dir.join(&desc.file) };
    [own, desc.source.clone()].into_iter().find(|p| !p.as_os_str().is_empty() && p.is_file())
}

/// Applies saved state to a Neural Amp Modeler, loading its capture unless it already plays it.
pub fn restore(native: &Native, data: &[u8], audio_dir: &Path) {
    let Some(captures) = &native.captures else { return };
    let want = solstice_dsp::nam::set_state(&native.bank, data);
    if want == captures.desc() && captures.missing().is_none() {
        return;
    }
    let Some(desc) = want else {
        captures.set(None);
        return;
    };
    match resolve(&desc, audio_dir) {
        Some(path) => match Capture::load(&path, desc.clone(), native.sample_rate) {
            Ok(c) => captures.set(Some(c)),
            Err(e) => captures.set_missing(desc, e),
        },
        None => captures.set_missing(desc, "the file is missing".into()),
    }
}

fn slot_of(ctx: &Ctx, native: &Native) -> Option<solstice_model::Id> {
    ctx.s().plugins.iter().find(|(_, l)| l.inst.native().is_some_and(|n| std::ptr::eq(n.as_ref(), native))).map(|(id, _)| *id)
}

/// Copies a library capture into the project and loads it in the background, then plays it as
/// one undo step.
pub fn use_capture(ctx: &Ctx, native: &Arc<Native>, src: PathBuf) {
    let dir = ctx.s().bundle.audio_dir();
    let sr = native.sample_rate;
    let (ctx, native) = (ctx.clone(), native.clone());
    glib::spawn_future_local(async move {
        let loaded = gio::spawn_blocking(move || {
            let file = into_project(&src, &dir)?;
            Capture::load(&dir.join(&file), CaptureDesc { file: file.into(), source: src }, sr)
        })
        .await
        .unwrap_or_else(|_| Err("loading stopped unexpectedly".into()));
        let capture = match loaded {
            Ok(c) => c,
            Err(e) => {
                ctx.message(&format!("Could not load the capture: {e}"));
                return;
            }
        };
        let Some(captures) = &native.captures else { return };
        let before = solstice_dsp::nam::state(&native.bank, captures.desc().as_ref());
        captures.set(Some(capture));
        match slot_of(&ctx, &native) {
            Some(sid) => ctx.m().plugin_state_edit(sid, before, "Load Capture"),
            None => ctx.m().history.mark_dirty(),
        }
        ctx.emit(Change::Project);
    });
}

/// Imports dropped or chosen files into a library folder in the background, then plays the
/// first capture they held. `done` runs on the main thread once the library has changed.
pub fn import_and_use(ctx: &Ctx, native: &Arc<Native>, files: Vec<PathBuf>, folder: &str, done: impl Fn() + 'static) {
    let dir = library().join(folder);
    let (ctx, native) = (ctx.clone(), native.clone());
    glib::spawn_future_local(async move {
        let (added, failed) = gio::spawn_blocking(move || import(&files, &dir)).await.unwrap_or_default();
        if !failed.is_empty() {
            ctx.message(&format!("Could not import {}", failed.join(", ")));
        }
        if let Some(first) = added.first() {
            use_capture(&ctx, &native, first.clone());
        }
        done();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("solstice-captures-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A zip as archivers write it, each entry stored or deflated.
    fn zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        for (name, data, deflate) in entries {
            let packed = if *deflate { miniz_oxide::deflate::compress_to_vec(data, 6) } else { data.to_vec() };
            let method: u16 = if *deflate { 8 } else { 0 };
            let offset = out.len() as u32;
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend([20, 0, 0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0; 8]);
            out.extend((packed.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&packed);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend([20, 0, 20, 0, 0, 0]);
            central.extend(method.to_le_bytes());
            central.extend([0; 8]);
            central.extend((packed.len() as u32).to_le_bytes());
            central.extend((data.len() as u32).to_le_bytes());
            central.extend((name.len() as u16).to_le_bytes());
            central.extend([0; 12]);
            central.extend(offset.to_le_bytes());
            central.extend(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend([0; 4]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(at.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    #[test]
    fn imports_captures_and_zips() {
        let d = dir("import");
        let lib = d.join("Guitar");
        let big: &[u8] = br#"{"architecture":"Linear","weights":[0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1,0.1]}"#;
        std::fs::write(d.join("Clean.nam"), b"clean").unwrap();
        std::fs::write(
            d.join("pack.zip"),
            zip(&[("Pack/Crunch.nam", big, true), ("Pack/Lead.nam", b"lead", false), ("Pack/readme.txt", b"hi", false), ("__MACOSX/Pack/._Lead.nam", b"x", false)]),
        )
        .unwrap();
        std::fs::write(d.join("empty.zip"), zip(&[("readme.txt", b"hi", false)])).unwrap();
        let (added, failed) = import(&[d.join("Clean.nam"), d.join("pack.zip"), d.join("empty.zip")], &lib);
        assert_eq!(added, vec![lib.join("Clean.nam"), lib.join("Crunch.nam"), lib.join("Lead.nam")]);
        assert_eq!(failed, vec!["empty.zip holds no captures".to_string()]);
        assert_eq!(std::fs::read(lib.join("Crunch.nam")).unwrap(), big);
        assert_eq!(list(&lib).len(), 3);
        // The same file again is the same capture; a different one with its name is numbered.
        std::fs::write(d.join("Lead.nam"), b"another").unwrap();
        let (again, _) = import(&[d.join("Clean.nam"), d.join("Lead.nam")], &lib);
        assert_eq!(again, vec![lib.join("Clean.nam"), lib.join("Lead 2.nam")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn project_copies_resolve_before_the_source() {
        let d = dir("project");
        let audio = d.join("Audio Files");
        std::fs::write(d.join("Amp.nam"), b"amp").unwrap();
        let file = into_project(&d.join("Amp.nam"), &audio).unwrap();
        assert_eq!(file, "Amp.nam");
        let desc = CaptureDesc { file: file.into(), source: d.join("Amp.nam") };
        assert_eq!(resolve(&desc, &audio), Some(audio.join("Amp.nam")));
        std::fs::remove_file(audio.join("Amp.nam")).unwrap();
        assert_eq!(resolve(&desc, &audio), Some(d.join("Amp.nam")));
        std::fs::remove_file(d.join("Amp.nam")).unwrap();
        assert_eq!(resolve(&desc, &audio), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restoring_state_loads_the_capture_once_and_keeps_a_missing_one() {
        let d = dir("restore");
        let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../namshim/NeuralAmpModelerCore/example_models/wavenet.nam");
        let (native, _) = solstice_dsp::create("solstice:nam", 44100.0).unwrap();
        let host = crate::host::Host::Native(Arc::new(native));
        let n = host.native().unwrap();
        let bank = solstice_dsp::Bank::new(&solstice_dsp::nam::NAM);
        let found = CaptureDesc { file: "wavenet.nam".into(), source: example };
        let state = solstice_dsp::nam::state(&bank, Some(&found));
        assert!(host.set_state_in(&state, &d));
        let first = n.captures.as_ref().unwrap().current().expect("loaded from its source");
        assert!(host.latency() > 0, "a 48 kHz capture in a 44.1 kHz session is resampled");
        assert_eq!(host.state(), state);
        host.set_state_in(&state, &d);
        assert!(Arc::ptr_eq(&first, &n.captures.as_ref().unwrap().current().unwrap()));

        let gone = CaptureDesc { file: "Gone.nam".into(), source: d.join("Gone.nam") };
        let state = solstice_dsp::nam::state(&bank, Some(&gone));
        host.set_state_in(&state, &d);
        assert!(n.captures.as_ref().unwrap().current().is_none());
        assert_eq!(host.state(), state, "saving keeps a capture that could not be found");
        assert_eq!(host.latency(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn library_stays_in_the_test_root() {
        assert!(library().starts_with(std::env::temp_dir()));
    }
}
