use crate::import;
use crate::studio::{Change, Ctx};
use gtk::{gio, glib};
use solstice_engine::audio::AudioData;
use solstice_interchange::{bundle, fcpxml, smf};
use solstice_model::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub type Notify = Rc<dyn Fn(String)>;

/// Whether `import_path` can take this path, so drops and the Open dialog can hand it over.
pub fn is_importable(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    matches!(ext.as_str(), "logicx" | "fcpxmld" | "fcpxml" | "mid" | "midi" | "smf")
        || (path.is_dir() && (path.join("Info.fcpxml").is_file() || bundle_in(path).is_some()))
}

/// The one project bundle inside a folder, for when the folder around it was picked.
fn bundle_in(dir: &Path) -> Option<PathBuf> {
    let mut found = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("logicx")));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Imports whatever the user picked: a project bundle (or the folder holding one), a
/// `.fcpxmld` bundle, an `.fcpxml` file or a MIDI file.
pub fn import_path(ctx: &Ctx, path: PathBuf, notify: Notify) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    match ext.as_str() {
        "logicx" => import_bundle(ctx, path, notify),
        "mid" | "midi" | "smf" => import_midi(ctx, &path, notify),
        "fcpxmld" => import_xml(ctx, path.join("Info.fcpxml"), notify),
        _ if path.is_dir() && path.join("Info.fcpxml").is_file() => import_xml(ctx, path.join("Info.fcpxml"), notify),
        _ if path.is_dir() => match bundle_in(&path) {
            Some(b) => import_bundle(ctx, b, notify),
            None => notify("That folder holds no project bundle to import".into()),
        },
        _ => import_xml(ctx, path, notify),
    }
}

/// Looks for a file by name near the XML: the export holds the Mac's absolute paths, which won't
/// exist here, but the media usually travels in the same folder or a project bundle beside it.
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

/// An imported file's name in the project, its length in frames and channel count.
type Imported = (String, u64, usize);

fn import_audio(src: &Path, dir: &Path, sr: u32) -> Result<Imported, String> {
    let (name, frames) = import::import(src, dir, sr)?;
    let channels = AudioData::open(&dir.join(&name)).map_or(1, |a| a.channels);
    Ok((name, frames, channels))
}

struct XmlResult {
    arrangement: fcpxml::Arrangement,
    /// Keyed by the clip's source URL.
    imported: HashMap<String, Imported>,
    missing: Vec<String>,
}

/// Reads the XML and decodes its media; runs off the main thread.
fn load_xml(path: &Path, extra_roots: &[PathBuf], dir: &Path, sr: u32) -> Result<XmlResult, String> {
    let xml = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let arrangement = fcpxml::parse(&xml)?;
    let mut roots = extra_roots.to_vec();
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
        match file.map(|f| import_audio(&f, dir, sr)) {
            Some(Ok(i)) => {
                imported.insert(clip.src.clone(), i);
            }
            _ => missing.push(clip.src.clone()),
        }
    }
    Ok(XmlResult { arrangement, imported, missing })
}

fn missing_names(r: &XmlResult) -> Vec<String> {
    r.missing
        .iter()
        .map(|s| Path::new(&fcpxml::url_to_path(s)).file_name().map_or(s.clone(), |n| n.to_string_lossy().into_owned()))
        .collect()
}

fn secs_to_samples(p: &Project, s: f64) -> u64 {
    (s * p.sample_rate as f64).round().max(0.0) as u64
}

/// Adds a track per lane; returns the number of regions placed.
fn place_xml(p: &mut Project, r: &XmlResult) -> usize {
    let mut regions = 0;
    for lane in &r.arrangement.lanes {
        let id = p.add_track(TrackKind::Audio);
        let mut stereo = false;
        let mut new_regions = Vec::new();
        for clip in &lane.clips {
            let Some((file, frames, channels)) = r.imported.get(&clip.src) else { continue };
            let offset = secs_to_samples(p, clip.offset).min(*frames);
            let length = secs_to_samples(p, clip.duration).min(frames - offset);
            if length == 0 {
                continue;
            }
            stereo |= *channels > 1;
            let rid = p.alloc_id();
            new_regions.push(Region {
                id: rid,
                name: clip.name.clone(),
                start: secs_to_samples(p, clip.start),
                length,
                muted: false,
                loops: 0,
                color: None,
                content: Content::Audio(AudioContent {
                    file: file.clone(),
                    offset,
                    gain_db: clip.gain_db,
                    fade_in: secs_to_samples(p, clip.fade_in).min(length),
                    fade_out: secs_to_samples(p, clip.fade_out).min(length),
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
    regions
}

/// Adds markers that aren't already there, at sample positions.
fn add_markers(p: &mut Project, markers: impl IntoIterator<Item = (u64, String)>) -> usize {
    let mut added = 0;
    for (pos, name) in markers {
        if p.markers.iter().any(|m| m.pos == pos && m.name == name) {
            continue;
        }
        let id = p.alloc_id();
        p.markers.push(Marker { id, pos, name });
        added += 1;
    }
    p.markers.sort_by_key(|m| m.pos);
    added
}

fn set_key(p: &mut Project, key: &str) {
    p.key_changes.retain(|k| k.bar != 0);
    p.key_changes.insert(0, KeyChange { bar: 0, key: key.to_string() });
}

/// Takes the file's tempo map and signatures in place of the project's, moving what is already
/// there to keep its bars.
fn apply_time_map(p: &mut Project, file: &smf::MidiFile) {
    let old = p.time();
    let scale = |t: u64| t * PPQ as u64 / file.ppq.max(1) as u64;
    let mut tempos = file.tempos.iter().map(|&(t, bpm)| (scale(t), bpm));
    if let Some(&(0, bpm)) = file.tempos.first() {
        p.tempo = bpm;
        tempos.next();
    }
    p.tempo_changes = tempos.map(|(tick, tempo)| TempoChange { tick, tempo }).collect();
    let mut sigs = smf::signature_bars(&file.signatures, file.ppq).into_iter();
    if let Some(&(0, n, d)) = smf::signature_bars(&file.signatures, file.ppq).first() {
        p.sig_num = n;
        p.sig_den = d;
        p.sig_groups.clear();
        sigs.next();
    }
    p.sig_changes = sigs.map(|(bar, num, den)| SigChange { bar, num, den, groups: Vec::new() }).collect();
    p.retime(&old);
}

/// Adds an instrument track per MIDI track that has notes; returns how many.
fn place_midi(p: &mut Project, file: &smf::MidiFile) -> usize {
    let time = p.time();
    let scale = |t: u64| t * PPQ as u64 / file.ppq.max(1) as u64;
    let mut count = 0;
    for track in file.tracks.iter().filter(|t| !t.notes.is_empty()) {
        let first = scale(track.notes.iter().map(|n| n.start).min().unwrap_or(0));
        let last = track.notes.iter().map(|n| scale(n.start + n.length)).max().unwrap_or(0);
        let start_tick = time.bar_tick(time.bar_at(time.ticks_to_samples(first)));
        let end_tick = time.bar_tick(time.bar_at(time.ticks_to_samples(last)) + 1);
        let notes = track
            .notes
            .iter()
            .map(|n| Note::new((scale(n.start) - start_tick) as u32, scale(n.length).max(1) as u32, n.pitch, n.velocity.max(1)))
            .collect();
        let id = p.add_track(TrackKind::Instrument);
        let rid = p.alloc_id();
        let start = time.ticks_to_samples(start_tick);
        let t = p.track_mut(id).unwrap();
        t.name = track.name.clone();
        t.regions.push(Region {
            id: rid,
            name: track.name.clone(),
            start,
            length: time.ticks_to_samples(end_tick).saturating_sub(start).max(1),
            muted: false,
            loops: 0,
            color: None,
            content: Content::Midi(MidiContent { notes, ..Default::default() }),
        });
        count += 1;
    }
    count
}

fn midi_markers(p: &Project, file: &smf::MidiFile) -> Vec<(u64, String)> {
    let time = p.time();
    file.markers.iter().map(|(t, name)| (time.ticks_to_samples(*t * PPQ as u64 / file.ppq.max(1) as u64), name.clone())).collect()
}

fn import_xml(ctx: &Ctx, path: PathBuf, notify: Notify) {
    let (dir, sr) = {
        let s = ctx.s();
        (s.bundle.audio_dir(), s.project.sample_rate)
    };
    let ctx = ctx.clone();
    notify("Importing Final Cut Pro XML…".into());
    glib::spawn_future_local(async move {
        let r = match gio::spawn_blocking(move || load_xml(&path, &[], &dir, sr)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return notify(format!("Could not import: {e}")),
            Err(_) => return notify("Import failed unexpectedly".into()),
        };
        let mut regions = 0;
        let lanes = r.arrangement.lanes.len();
        ctx.edit("Import Final Cut Pro XML", |p| {
            regions = place_xml(p, &r);
            let markers: Vec<_> = r.arrangement.markers.iter().map(|(s, n)| (secs_to_samples(p, *s), n.clone())).collect();
            add_markers(p, markers);
        });
        let mut msg = format!("Imported {lanes} tracks and {regions} regions from “{}”", r.arrangement.name);
        let missing = missing_names(&r);
        if !missing.is_empty() {
            msg.push_str(&format!(". Missing media: {}. Put the files next to the XML and import again.", missing.join(", ")));
        }
        notify(msg);
    });
}

fn import_midi(ctx: &Ctx, path: &Path, notify: Notify) {
    let file = match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| smf::parse(&b)) {
        Ok(f) => f,
        Err(e) => return notify(format!("Could not import the MIDI file: {e}")),
    };
    // A fresh project takes the file's tempo map; an existing arrangement keeps its own.
    let fresh = ctx.s().project.length() == 0;
    let mut count = 0;
    ctx.edit("Import MIDI File", |p| {
        if fresh {
            apply_time_map(p, &file);
            if let Some(k) = &file.key {
                set_key(p, k);
            }
        }
        let markers = midi_markers(p, &file);
        add_markers(p, markers);
        count = place_midi(p, &file);
    });
    let tempo = if fresh { file.tempo.map(|t| format!(" at {t:.1} bpm")).unwrap_or_default() } else { String::new() };
    notify(format!("Imported {count} MIDI tracks{tempo}. Choose an instrument for each in the mixer or Library."));
}

/// A recording from the bundle, decoded into the project, with where it goes.
struct Placed {
    track: String,
    start: f64,
    file: Imported,
}

#[derive(Default)]
struct BundleResult {
    meta: bundle::BundleMeta,
    xml: Option<XmlResult>,
    xml_error: Option<String>,
    midi: Vec<smf::MidiFile>,
    placed: Vec<Placed>,
    /// Whether `placed` holds exported tracks rather than the original recordings.
    from_track_exports: bool,
    failed: Vec<String>,
}

/// Reads the bundle, its exports and its recordings; runs off the main thread.
fn load_bundle(path: &Path, dir: &Path, sr: u32) -> Result<BundleResult, String> {
    let meta = bundle::read(path)?;
    let mut r = BundleResult::default();
    let roots: Vec<PathBuf> = meta.audio_dir.iter().cloned().chain([path.to_path_buf()]).collect();
    for export in &meta.exports {
        let ext = export.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
        match ext.as_str() {
            "mid" | "midi" => match std::fs::read(export).map_err(|e| e.to_string()).and_then(|b| smf::parse(&b)) {
                Ok(f) => r.midi.push(f),
                Err(e) => r.failed.push(format!("{}: {e}", export.display())),
            },
            _ if r.xml.is_none() => {
                let xml = if ext == "fcpxmld" { export.join("Info.fcpxml") } else { export.clone() };
                match load_xml(&xml, &roots, dir, sr) {
                    Ok(x) => r.xml = Some(x),
                    Err(e) => r.xml_error = Some(e),
                }
            }
            _ => {}
        }
    }
    // Without an exported arrangement, exported tracks come next: they carry every edit. Failing
    // those, recordings go back where they were recorded.
    if r.xml.is_none() {
        r.from_track_exports = !meta.track_exports.is_empty();
        let sources = if r.from_track_exports { &meta.track_exports } else { &meta.recordings };
        for rec in sources {
            let Some(start) = rec.position else { continue };
            match import_audio(&rec.path, dir, sr) {
                Ok(file) => r.placed.push(Placed { track: rec.track.clone(), start, file }),
                Err(e) => r.failed.push(format!("{}: {e}", rec.path.display())),
            }
        }
    }
    r.meta = meta;
    Ok(r)
}

/// Puts each recording on its track. Passes recorded from the same place join as a take folder,
/// the last one playing; other overlaps go to another track of the same name.
fn place_recordings(p: &mut Project, placed: &[Placed]) -> (usize, usize) {
    let mut order: Vec<&Placed> = placed.iter().collect();
    order.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut names: Vec<&str> = Vec::new();
    for pl in &order {
        if !names.contains(&pl.track.as_str()) {
            names.push(&pl.track);
        }
    }
    // A frame either side counts as the same place, for stamps rounded at another rate.
    let same = |a: u64, b: u64| a.abs_diff(b) <= 1;
    let (mut tracks, mut regions) = (0, 0);
    for name in names {
        let mut lanes: Vec<Vec<Region>> = Vec::new();
        let mut stereo = false;
        for pl in order.iter().filter(|pl| pl.track == name) {
            let (file, frames, channels) = &pl.file;
            // A pass started before the project start loses the part that would sit before it.
            let skip = secs_to_samples(p, -pl.start);
            if skip >= *frames {
                continue;
            }
            let frames = frames - skip;
            stereo |= *channels > 1;
            let start = secs_to_samples(p, pl.start);
            let mut fitted = false;
            for lane in &mut lanes {
                if let Some(r) = lane.iter_mut().find(|r| same(r.start, start))
                    && let Content::Audio(a) = &mut r.content
                {
                    let n = a.takes.len().max(1) + 1;
                    a.add_take(Take { name: format!("Take {n}"), file: file.clone(), offset: skip });
                    r.length = r.length.max(frames);
                    fitted = true;
                    break;
                }
            }
            if !fitted {
                let id = p.alloc_id();
                let region = Region {
                    id,
                    name: file.trim_end_matches(".wav").to_string(),
                    start,
                    length: frames,
                    muted: false,
                    loops: 0,
                    color: None,
                    content: Content::Audio(AudioContent { offset: skip, ..AudioContent::new(file.clone()) }),
                };
                match lanes.iter_mut().find(|l| l.iter().all(|r| r.start + r.length <= start || start + frames <= r.start)) {
                    Some(lane) => lane.push(region),
                    None => lanes.push(vec![region]),
                }
                regions += 1;
            }
        }
        for lane in lanes {
            let id = p.add_track(TrackKind::Audio);
            let t = p.track_mut(id).unwrap();
            t.name = name.to_string();
            t.stereo = stereo;
            t.regions = lane;
            tracks += 1;
        }
    }
    (tracks, regions)
}

fn import_bundle(ctx: &Ctx, path: PathBuf, notify: Notify) {
    let (dir, sr) = {
        let s = ctx.s();
        (s.bundle.audio_dir(), s.project.sample_rate)
    };
    let ctx = ctx.clone();
    notify("Importing the project…".into());
    glib::spawn_future_local(async move {
        let r = match gio::spawn_blocking(move || load_bundle(&path, &dir, sr)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return notify(format!("Could not read the project bundle: {e}")),
            Err(_) => return notify("Import failed unexpectedly".into()),
        };
        for e in &r.failed {
            eprintln!("solstice: import failed: {e}");
        }
        let meta = &r.meta;
        // The MIDI export with the most tempo and signature changes is the fullest map.
        let map = r.midi.iter().max_by_key(|f| f.tempos.len() + f.signatures.len()).filter(|f| !f.tempos.is_empty());
        let mut notes = Vec::new();
        let (mut tracks, mut regions, mut markers) = (0, 0, 0);
        // As with a MIDI file, an existing arrangement keeps its own tempo map and key.
        let fresh = ctx.s().project.length() == 0;
        ctx.edit("Import Project Bundle", |p| {
            if fresh {
                let old = p.time();
                if let Some(t) = meta.tempo {
                    p.tempo = t;
                }
                if let Some((n, d)) = meta.signature {
                    p.sig_num = n.max(1);
                    p.sig_den = d.max(1);
                    p.sig_groups.clear();
                }
                p.retime(&old);
                if let Some(f) = map {
                    apply_time_map(p, f);
                }
                if let Some(k) = meta.key.as_ref().or(r.midi.iter().find_map(|f| f.key.as_ref())) {
                    set_key(p, k);
                }
            }
            if let Some(x) = &r.xml {
                tracks += x.arrangement.lanes.len();
                regions += place_xml(p, x);
                let m: Vec<_> = x.arrangement.markers.iter().map(|(s, n)| (secs_to_samples(p, *s), n.clone())).collect();
                markers += add_markers(p, m);
            } else {
                let (t, n) = place_recordings(p, &r.placed);
                tracks += t;
                regions += n;
            }
            for f in &r.midi {
                let m = midi_markers(p, f);
                markers += add_markers(p, m);
                let n = place_midi(p, f);
                tracks += n;
                regions += n;
            }
        });
        if fresh {
            let time = ctx.s().project.time();
            notes.push(format!("{:.1} bpm in {}/{}", time.tempo_at(0), time.sig_at(0).0, time.sig_at(0).1));
            if let Some(f) = map.filter(|f| f.tempos.len() > 1) {
                notes.push(format!("{} tempo changes", f.tempos.len() - 1));
            }
            if let Some(k) = ctx.s().project.key_changes.first() {
                notes.push(k.key.clone());
            }
        } else {
            notes.push("the open project's tempo and key kept".into());
        }
        if tracks > 0 {
            notes.push(format!("{tracks} tracks with {regions} regions"));
        }
        if markers > 0 {
            notes.push(format!("{markers} markers"));
        }
        let rate = ctx.s().sample_rate();
        if let Some(sr) = meta.sample_rate.filter(|sr| *sr != rate) {
            notes.push(format!("audio converted from {sr} Hz to {rate} Hz"));
        }
        let unplaced = meta.recordings.iter().filter(|rec| rec.position.is_none()).count();
        let from_recordings = r.xml.is_none() && !r.from_track_exports;
        if let Some(dir) = meta.audio_dir.clone().filter(|_| from_recordings && unplaced > 0) {
            ctx.m().browse_to = Some(dir);
            ctx.emit(Change::Browse);
            notes.push(format!("{unplaced} audio files without a recorded position are in the Browser"));
        }
        let mut msg = format!("Imported {}: {}.", meta.name, notes.join(", "));
        if let Some(x) = &r.xml {
            let missing = missing_names(x);
            if !missing.is_empty() {
                msg.push_str(&format!(" Missing media: {}.", missing.join(", ")));
            }
        } else if let Some(e) = &r.xml_error {
            let instead = if r.from_track_exports { "the exported tracks were used" } else { "recordings were placed where they were recorded" };
            msg.push_str(&format!(" The Final Cut Pro XML beside it could not be read ({e}), so {instead}."));
        } else if r.from_track_exports {
            msg.push_str(" Tracks were taken from the audio files exported into the Bounces folder.");
            if let Some(dir) = meta.audio_dir.clone() {
                ctx.m().browse_to = Some(dir);
                ctx.emit(Change::Browse);
                msg.push_str(" The original recordings are in the Browser.");
            }
        } else if !r.placed.is_empty() {
            msg.push_str(" Recordings were placed where they were recorded; for edits and fades, export Final Cut Pro XML next to the bundle, or export all tracks as audio files into its Bounces folder, and import again.");
        }
        if !r.failed.is_empty() {
            msg.push_str(&format!(" {} files could not be read.", r.failed.len()));
        }
        notify(msg);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(track: &str, start: f64, file: &str, frames: u64) -> Placed {
        Placed { track: track.into(), start, file: (file.into(), frames, 1) }
    }

    #[test]
    fn recordings_go_to_their_tracks_with_takes_folded() {
        let mut p = Project::new("T", 48000);
        let recs = [
            placed("Gtr", 2.0, "Gtr#01.wav", 48000),
            placed("Gtr", 2.0, "Gtr#02.wav", 96000),
            placed("Gtr", 10.0, "Gtr#03.wav", 48000),
            placed("Gtr", 10.5, "Gtr#04.wav", 48000),
            placed("Bass", 0.0, "Bass#01.wav", 480000),
        ];
        assert_eq!(place_recordings(&mut p, &recs), (3, 4));
        let names: Vec<_> = p.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Bass", "Gtr", "Gtr"]);
        let gtr = &p.tracks[1];
        assert_eq!(gtr.regions.len(), 2);
        assert_eq!((gtr.regions[0].start, gtr.regions[0].length), (96000, 96000));
        let Content::Audio(a) = &gtr.regions[0].content else { panic!() };
        assert_eq!(a.takes.len(), 2);
        assert_eq!(a.file, "Gtr#02.wav");
        assert_eq!(p.tracks[2].regions[0].start, 504000);
    }

    #[test]
    fn recordings_started_before_the_project_are_trimmed() {
        let mut p = Project::new("T", 48000);
        let recs = [placed("Vox", -0.5, "Vox#01.wav", 48000), placed("Vox", -2.0, "Vox#02.wav", 48000)];
        assert_eq!(place_recordings(&mut p, &recs), (1, 1));
        let r = &p.tracks[0].regions[0];
        assert_eq!((r.start, r.length), (0, 24000));
        let Content::Audio(a) = &r.content else { panic!() };
        assert_eq!((a.file.as_str(), a.offset), ("Vox#01.wav", 24000));
    }

    #[test]
    fn midi_time_map_replaces_the_projects() {
        let mut p = Project::new("T", 48000);
        let file = smf::MidiFile {
            ppq: 480,
            tempo: Some(90.0),
            signature: Some((6, 8)),
            tempos: vec![(0, 90.0), (480 * 12, 140.0)],
            signatures: vec![(0, 6, 8), (480 * 12, 4, 4)],
            markers: vec![(480 * 3, "Verse".into())],
            key: Some("E minor".into()),
            tracks: Vec::new(),
        };
        apply_time_map(&mut p, &file);
        assert_eq!((p.tempo, p.sig_num, p.sig_den), (90.0, 6, 8));
        assert_eq!(p.tempo_changes, vec![TempoChange { tick: 960 * 12, tempo: 140.0 }]);
        assert_eq!(p.sig_changes, vec![SigChange { bar: 4, num: 4, den: 4, groups: Vec::new() }]);
        let m = midi_markers(&p, &file);
        assert_eq!(add_markers(&mut p, m.clone()), 1);
        assert_eq!(add_markers(&mut p, m), 0);
        assert_eq!(p.markers[0].pos, 96000);
    }
}
