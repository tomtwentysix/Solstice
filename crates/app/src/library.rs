//! The libraries of files built-in effects play, under the data directory: Neural Amp Modeler
//! captures, kept in a Guitar and a Bass folder, and impulse responses. A file in use is copied
//! into the project's Audio Files.

use crate::studio::{Change, Ctx};
use gtk::{gio, glib};
use solstice_dsp::Native;
use solstice_dsp::slot::{FileDesc, Loaded};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Library {
    dir: &'static str,
    ext: &'static str,
    /// Folders the panel switches between. Without any, files sit in the library itself.
    pub folders: &'static [&'static str],
    /// One file, in messages.
    pub noun: &'static str,
    pub undo: &'static str,
}

pub const CAPTURES: Library = Library { dir: "captures", ext: "nam", folders: &["Guitar", "Bass"], noun: "capture", undo: "Load Capture" };
pub const IRS: Library = Library { dir: "irs", ext: "wav", folders: &[], noun: "impulse response", undo: "Load Impulse Response" };

/// Larger files are not real captures or responses; it also bounds what a zip entry may
/// inflate to.
const MAX_SIZE: usize = 64 << 20;

impl Library {
    pub fn root(&self) -> PathBuf {
        match crate::settings::dev_root() {
            Some(d) => d.join("data").join(self.dir),
            None => glib::user_data_dir().join("solstice").join(self.dir),
        }
    }

    pub fn folder(&self, i: usize) -> PathBuf {
        match self.folders.get(i) {
            Some(f) => self.root().join(f),
            None => self.root(),
        }
    }

    /// Which folder `file` came from.
    pub fn folder_of(&self, file: &Path) -> usize {
        (0..self.folders.len()).find(|&i| file.starts_with(self.folder(i))).unwrap_or(0)
    }

    pub fn ext(&self) -> &'static str {
        self.ext
    }

    fn holds(&self, p: &Path) -> bool {
        p.extension().is_some_and(|e| e.eq_ignore_ascii_case(self.ext))
    }

    pub fn is_importable(&self, p: &Path) -> bool {
        self.holds(p) || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    }

    /// The files in a library folder and any folders inside it, sorted by name.
    pub fn list(&self, folder: &Path) -> Vec<PathBuf> {
        fn walk(lib: &Library, dir: &Path, out: &mut Vec<PathBuf>) {
            for p in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
                if p.is_dir() {
                    walk(lib, &p, out);
                } else if lib.holds(&p) {
                    out.push(p);
                }
            }
        }
        let mut v = Vec::new();
        walk(self, folder, &mut v);
        v.sort_by_key(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()));
        v
    }

    /// Writes `data` into `dir` as `name`, numbering the name if a different file has it. A file
    /// with the same name and contents is reused.
    fn place(&self, dir: &Path, name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let stem = Path::new(name).file_stem().map_or("Imported".into(), |s| s.to_string_lossy().into_owned());
        let mut n = 1;
        loop {
            let candidate = dir.join(if n == 1 { format!("{stem}.{}", self.ext) } else { format!("{stem} {n}.{}", self.ext) });
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

    /// Copies files, and the files inside zips, into a library folder. Returns those added and a
    /// message for each file that failed. Blocking.
    pub fn import(&self, files: &[PathBuf], folder: &Path) -> (Vec<PathBuf>, Vec<String>) {
        let (mut added, mut failed) = (Vec::new(), Vec::new());
        for f in files {
            let name = f.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let entries = if self.holds(f) {
                read_limited(f).map(|d| vec![(name.clone(), d)])
            } else {
                std::fs::read(f).map_err(|e| e.to_string()).and_then(|d| self.unzip(&d))
            };
            match entries {
                Ok(v) if v.is_empty() => failed.push(format!("{name} holds no {}s", self.noun)),
                Ok(v) => {
                    for (entry, data) in v {
                        match self.place(folder, &entry, &data) {
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

    /// The library's entries of a zip archive, by file name without their folders. Stored and
    /// deflated entries are read; ZIP64 archives and encryption are not.
    fn unzip(&self, d: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
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
            if !self.holds(Path::new(&name)) || name.starts_with("._") || path.contains("__MACOSX") {
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

    /// Copies a library file into the project's Audio Files and returns its name there.
    /// Blocking.
    pub fn into_project(&self, src: &Path, audio_dir: &Path) -> Result<String, String> {
        let data = read_limited(src)?;
        let name = src.file_name().map_or_else(|| format!("Imported.{}", self.ext), |n| n.to_string_lossy().into_owned());
        let placed = self.place(audio_dir, &name, &data).map_err(|e| e.to_string())?;
        Ok(placed.file_name().map_or(name, |n| n.to_string_lossy().into_owned()))
    }
}

fn read_limited(p: &Path) -> Result<Vec<u8>, String> {
    if std::fs::metadata(p).map_err(|e| e.to_string())?.len() > MAX_SIZE as u64 {
        return Err("the file is too large".into());
    }
    std::fs::read(p).map_err(|e| e.to_string())
}

fn u16_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().ok()?) as usize)
}

fn u32_at(d: &[u8], i: usize) -> Option<usize> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().ok()?) as usize)
}

/// Where a saved file is now: in the project, else where it came from.
pub fn resolve(desc: &FileDesc, audio_dir: &Path) -> Option<PathBuf> {
    let own = if desc.file.is_absolute() { desc.file.clone() } else { audio_dir.join(&desc.file) };
    [own, desc.source.clone()].into_iter().find(|p| !p.as_os_str().is_empty() && p.is_file())
}

/// Applies saved state to an effect that plays a file, loading the file unless it already
/// plays it.
pub fn restore<T: Loaded>(native: &Native, data: &[u8], audio_dir: &Path) {
    let Some(slot) = T::slot(native) else { return };
    let want = solstice_dsp::slot::set_state::<T>(&native.bank, data);
    if want == slot.desc() && slot.missing().is_none() {
        return;
    }
    let Some(desc) = want else {
        slot.set(None);
        return;
    };
    match resolve(&desc, audio_dir) {
        Some(path) => match T::load(&path, desc.clone(), native.sample_rate) {
            Ok(c) => slot.set(Some(c)),
            Err(e) => slot.set_missing(desc, e),
        },
        None => slot.set_missing(desc, "the file is missing".into()),
    }
}

fn slot_of(ctx: &Ctx, native: &Native) -> Option<solstice_model::Id> {
    ctx.s().plugins.iter().find(|(_, l)| l.inst.native().is_some_and(|n| std::ptr::eq(n.as_ref(), native))).map(|(id, _)| *id)
}

/// Copies a library file into the project and loads it in the background, then plays it as one
/// undo step.
pub fn use_file<T: Loaded>(ctx: &Ctx, native: &Arc<Native>, lib: &'static Library, src: PathBuf) {
    let dir = ctx.s().bundle.audio_dir();
    let sr = native.sample_rate;
    let (ctx, native) = (ctx.clone(), native.clone());
    glib::spawn_future_local(async move {
        let loaded = gio::spawn_blocking(move || {
            let file = lib.into_project(&src, &dir)?;
            T::load(&dir.join(&file), FileDesc { file: file.into(), source: src }, sr)
        })
        .await
        .unwrap_or_else(|_| Err("loading stopped unexpectedly".into()));
        let item = match loaded {
            Ok(c) => c,
            Err(e) => {
                ctx.message(&format!("Could not load the {}: {e}", lib.noun));
                return;
            }
        };
        let Some(slot) = T::slot(&native) else { return };
        let before = slot.state(&native.bank);
        slot.set(Some(item));
        match slot_of(&ctx, &native) {
            Some(sid) => ctx.m().plugin_state_edit(sid, before, lib.undo),
            None => ctx.m().history.mark_dirty(),
        }
        ctx.emit(Change::Project);
    });
}

/// Imports dropped or chosen files into a library folder in the background, then plays the
/// first file they held. `done` runs on the main thread once the library has changed.
pub fn import_and_use<T: Loaded>(ctx: &Ctx, native: &Arc<Native>, lib: &'static Library, files: Vec<PathBuf>, folder: PathBuf, done: impl Fn() + 'static) {
    let (ctx, native) = (ctx.clone(), native.clone());
    glib::spawn_future_local(async move {
        let (added, failed) = gio::spawn_blocking(move || lib.import(&files, &folder)).await.unwrap_or_default();
        if !failed.is_empty() {
            ctx.message(&format!("Could not import {}", failed.join(", ")));
        }
        if let Some(first) = added.first() {
            use_file::<T>(&ctx, &native, lib, first.clone());
        }
        done();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstice_dsp::nam::Capture;

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
        let (added, failed) = CAPTURES.import(&[d.join("Clean.nam"), d.join("pack.zip"), d.join("empty.zip")], &lib);
        assert_eq!(added, vec![lib.join("Clean.nam"), lib.join("Crunch.nam"), lib.join("Lead.nam")]);
        assert_eq!(failed, vec!["empty.zip holds no captures".to_string()]);
        assert_eq!(std::fs::read(lib.join("Crunch.nam")).unwrap(), big);
        assert_eq!(CAPTURES.list(&lib).len(), 3);
        // The same file again is the same capture; a different one with its name is numbered.
        std::fs::write(d.join("Lead.nam"), b"another").unwrap();
        let (again, _) = CAPTURES.import(&[d.join("Clean.nam"), d.join("Lead.nam")], &lib);
        assert_eq!(again, vec![lib.join("Clean.nam"), lib.join("Lead 2.nam")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn project_copies_resolve_before_the_source() {
        let d = dir("project");
        let audio = d.join("Audio Files");
        std::fs::write(d.join("Amp.nam"), b"amp").unwrap();
        let file = CAPTURES.into_project(&d.join("Amp.nam"), &audio).unwrap();
        assert_eq!(file, "Amp.nam");
        let desc = FileDesc { file: file.into(), source: d.join("Amp.nam") };
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
        let found = FileDesc { file: "wavenet.nam".into(), source: example };
        let state = solstice_dsp::slot::state::<Capture>(&bank, Some(&found));
        assert!(host.set_state_in(&state, &d));
        let first = n.captures.as_ref().unwrap().current().expect("loaded from its source");
        assert!(host.latency() > 0, "a 48 kHz capture in a 44.1 kHz session is resampled");
        assert_eq!(host.state(), state);
        host.set_state_in(&state, &d);
        assert!(Arc::ptr_eq(&first, &n.captures.as_ref().unwrap().current().unwrap()));

        let gone = FileDesc { file: "Gone.nam".into(), source: d.join("Gone.nam") };
        let state = solstice_dsp::slot::state::<Capture>(&bank, Some(&gone));
        host.set_state_in(&state, &d);
        assert!(n.captures.as_ref().unwrap().current().is_none());
        assert_eq!(host.state(), state, "saving keeps a capture that could not be found");
        assert_eq!(host.latency(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn impulse_responses_import_and_restore() {
        let d = dir("irs");
        let lib = d.join("Library");
        solstice_engine::audio::write_wav(&d.join("Cab.wav"), 44100, &[vec![0.5, 0.25, 0.1]]).unwrap();
        let wav = std::fs::read(d.join("Cab.wav")).unwrap();
        std::fs::write(d.join("cabs.zip"), zip(&[("Cabs/Room.WAV", &wav, true), ("Cabs/notes.txt", b"hi", false)])).unwrap();
        let (added, failed) = IRS.import(&[d.join("Cab.wav"), d.join("cabs.zip")], &lib);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(added, vec![lib.join("Cab.wav"), lib.join("Room.wav")]);

        let (native, _) = solstice_dsp::create("solstice:ir", 48000.0).unwrap();
        let host = crate::host::Host::Native(Arc::new(native));
        let n = host.native().unwrap();
        let audio = d.join("Audio Files");
        let file = IRS.into_project(&lib.join("Room.wav"), &audio).unwrap();
        let desc = FileDesc { file: file.into(), source: lib.join("Room.wav") };
        let bank = solstice_dsp::Bank::new(&solstice_dsp::ir::IR);
        let state = solstice_dsp::slot::state::<solstice_dsp::ir::Ir>(&bank, Some(&desc));
        assert!(host.set_state_in(&state, &audio));
        let ir = n.irs.as_ref().unwrap().current().expect("loaded from the project");
        assert_eq!((ir.rate, ir.channels), (44100, 1));
        assert_eq!(host.state(), state);
        assert_eq!(host.latency(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn libraries_stay_in_the_test_root() {
        assert!(CAPTURES.folder(1).starts_with(std::env::temp_dir().join(format!("solstice-test-{}", std::process::id()))));
        assert_eq!(CAPTURES.folder_of(&CAPTURES.folder(1).join("Bass Amp.nam")), 1);
        assert_eq!(IRS.folder(0), IRS.root());
        assert!(IRS.root().ends_with("data/irs"));
    }
}
