use crate::import;
use crate::studio::{Change, Ctx};
use gtk::{gio, glib};
use solstice_engine::audio::AudioData;
use solstice_interchange::{fcpxml, logicx, smf};
use solstice_model::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub type Notify = Rc<dyn Fn(String)>;

/// Imports whatever the user picked: a `.logicx` bundle, a `.fcpxmld` bundle, an `.fcpxml`
/// file or a MIDI file.
pub fn import_path(ctx: &Ctx, path: PathBuf, notify: Notify) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    match ext.as_str() {
        "logicx" => import_logicx(ctx, &path, notify),
        "mid" | "midi" | "smf" => import_midi(ctx, &path, notify),
        "fcpxmld" => import_xml(ctx, path.join("Info.fcpxml"), notify),
        _ if path.is_dir() && path.join("Info.fcpxml").is_file() => import_xml(ctx, path.join("Info.fcpxml"), notify),
        _ => import_xml(ctx, path, notify),
    }
}

fn import_logicx(ctx: &Ctx, path: &Path, notify: Notify) {
    let meta = match logicx::read(path) {
        Ok(m) => m,
        Err(e) => return notify(format!("Could not read the Logic project: {e}")),
    };
    let mut notes = Vec::new();
    if meta.tempo.is_some() || meta.signature.is_some() {
        ctx.edit("Import Logic Song Settings", |p| {
            if let Some(t) = meta.tempo {
                p.tempo = t;
            }
            if let Some((n, d)) = meta.signature {
                p.sig_num = n.max(1);
                p.sig_den = d.max(1);
            }
        });
        notes.push(format!(
            "{} bpm, {}",
            meta.tempo.map_or("?".into(), |t| format!("{t:.1}")),
            meta.signature.map_or("?".into(), |(n, d)| format!("{n}/{d}"))
        ));
    }
    if let Some(k) = &meta.key {
        notes.push(format!("key {k}"));
    }
    let rate = ctx.s().sample_rate();
    if let Some(sr) = meta.sample_rate.filter(|sr| *sr != rate) {
        notes.push(format!("recorded at {sr} Hz, imports will be converted to {rate} Hz"));
    }
    if let Some(dir) = meta.audio_dir {
        ctx.m().browse_to = Some(dir);
        ctx.emit(Change::Browse);
        notes.push(format!("{} audio files in the Browser", meta.audio_files.len()));
    }
    notify(format!(
        "Imported {}: {}. For the arrangement, export Final Cut Pro XML from Logic and import that too.",
        meta.name,
        notes.join(", ")
    ));
}

/// Looks for a file by name near the XML: Logic writes the Mac's absolute paths, which won't
/// exist here, but the media usually travels in the same folder or a `.logicx` beside it.
fn find_media(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        let mut subdirs = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == name) && p.is_file() {
                return Some(p);
            }
            if p.is_dir() && depth > 0 {
                subdirs.push(p);
            }
        }
        subdirs.into_iter().find_map(|d| walk(&d, name, depth - 1))
    }
    roots.iter().find_map(|r| walk(r, name, 5))
}

struct XmlResult {
    arrangement: fcpxml::Arrangement,
    /// Source URL to the imported file name, its length in frames and channel count.
    imported: HashMap<String, (String, u64, usize)>,
    missing: Vec<String>,
}

fn import_xml(ctx: &Ctx, path: PathBuf, notify: Notify) {
    let (dir, sr) = {
        let s = ctx.s();
        (s.bundle.audio_dir(), s.project.sample_rate)
    };
    let ctx = ctx.clone();
    notify("Importing Final Cut Pro XML…".into());
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || -> Result<XmlResult, String> {
            let xml = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let arrangement = fcpxml::parse(&xml)?;
            let mut roots = Vec::new();
            if let Some(p) = path.parent() {
                roots.push(p.to_path_buf());
                if let Some(pp) = p.parent() {
                    roots.push(pp.to_path_buf());
                }
            }
            let mut imported = HashMap::new();
            let mut missing = Vec::new();
            for clip in arrangement.lanes.iter().flat_map(|l| &l.clips) {
                if imported.contains_key(&clip.src) || missing.contains(&clip.src) {
                    continue;
                }
                let original = PathBuf::from(fcpxml::url_to_path(&clip.src));
                let file = if original.is_file() {
                    Some(original.clone())
                } else {
                    original.file_name().and_then(|n| n.to_str()).and_then(|n| find_media(n, &roots))
                };
                match file.map(|f| import::import(&f, &dir, sr)) {
                    Some(Ok((name, frames))) => {
                        let channels = AudioData::open(&dir.join(&name)).map_or(1, |a| a.channels);
                        imported.insert(clip.src.clone(), (name, frames, channels));
                    }
                    _ => missing.push(clip.src.clone()),
                }
            }
            Ok(XmlResult { arrangement, imported, missing })
        })
        .await;
        let r = match result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return notify(format!("Could not import: {e}")),
            Err(_) => return notify("Import failed unexpectedly".into()),
        };
        let mut regions = 0;
        let lanes = r.arrangement.lanes.len();
        ctx.edit("Import Final Cut Pro XML", |p| {
            let rate = p.sample_rate as f64;
            let secs = move |s: f64| (s * rate).round().max(0.0) as u64;
            for lane in &r.arrangement.lanes {
                let id = p.add_track(TrackKind::Audio);
                let mut stereo = false;
                let mut new_regions = Vec::new();
                for clip in &lane.clips {
                    let Some((file, frames, channels)) = r.imported.get(&clip.src) else { continue };
                    let offset = secs(clip.offset).min(*frames);
                    let length = secs(clip.duration).min(frames - offset);
                    if length == 0 {
                        continue;
                    }
                    stereo |= *channels > 1;
                    let rid = p.alloc_id();
                    new_regions.push(Region {
                        id: rid,
                        name: clip.name.clone(),
                        start: secs(clip.start),
                        length,
                        muted: false,
                        loops: 0,
                        content: Content::Audio(AudioContent {
                            file: file.clone(),
                            offset,
                            gain_db: clip.gain_db,
                            fade_in: secs(clip.fade_in).min(length),
                            fade_out: secs(clip.fade_out).min(length),
                            takes: Vec::new(),
                            comp: Vec::new(),
                        }),
                    });
                }
                regions += new_regions.len();
                let t = p.track_mut(id).unwrap();
                t.name = lane.name.clone();
                t.stereo = stereo;
                t.regions = new_regions;
            }
        });
        let mut msg = format!("Imported {lanes} tracks and {regions} regions from “{}”", r.arrangement.name);
        if !r.missing.is_empty() {
            let names: Vec<String> = r
                .missing
                .iter()
                .map(|s| Path::new(&fcpxml::url_to_path(s)).file_name().map_or(s.clone(), |n| n.to_string_lossy().into_owned()))
                .collect();
            msg.push_str(&format!(". Missing media: {}. Put the files next to the XML and import again.", names.join(", ")));
        }
        notify(msg);
    });
}

fn import_midi(ctx: &Ctx, path: &Path, notify: Notify) {
    let file = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| smf::parse(&b)) {
        Ok(f) => f,
        Err(e) => return notify(format!("Could not import the MIDI file: {e}")),
    };
    // A fresh project takes the file's tempo; an existing arrangement keeps its own.
    let fresh = ctx.s().project.length() == 0;
    let count = file.tracks.iter().filter(|t| !t.notes.is_empty()).count();
    ctx.edit("Import MIDI File", |p| {
        if fresh {
            if let Some(t) = file.tempo {
                p.tempo = t;
            }
            if let Some((n, d)) = file.signature {
                p.sig_num = n.max(1);
                p.sig_den = d.max(1);
            }
        }
        let time = p.time();
        let scale = |t: u64| t * PPQ as u64 / file.ppq.max(1) as u64;
        for track in file.tracks.iter().filter(|t| !t.notes.is_empty()) {
            let bar = time.ticks_per_bar();
            let first = scale(track.notes.iter().map(|n| n.start).min().unwrap_or(0));
            let last = track.notes.iter().map(|n| scale(n.start + n.length)).max().unwrap_or(0);
            let start_tick = first / bar * bar;
            let end_tick = (last / bar + 1) * bar;
            let notes = track
                .notes
                .iter()
                .map(|n| Note {
                    start: (scale(n.start) - start_tick) as u32,
                    length: scale(n.length).max(1) as u32,
                    pitch: n.pitch,
                    velocity: n.velocity.max(1),
                })
                .collect();
            let id = p.add_track(TrackKind::Instrument);
            let rid = p.alloc_id();
            let t = p.track_mut(id).unwrap();
            t.name = track.name.clone();
            t.regions.push(Region {
                id: rid,
                name: track.name.clone(),
                start: time.ticks_to_samples(start_tick),
                length: time.ticks_to_samples(end_tick - start_tick),
                muted: false,
                loops: 0,
                content: Content::Midi(MidiContent { notes, ..Default::default() }),
            });
        }
    });
    let tempo = if fresh { file.tempo.map(|t| format!(" at {t:.1} bpm")).unwrap_or_default() } else { String::new() };
    notify(format!("Imported {count} MIDI tracks{tempo}. Choose an instrument for each in the mixer or Library."));
}
