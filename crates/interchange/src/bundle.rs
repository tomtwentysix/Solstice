//! What can be read openly from a project bundle: the song settings in
//! `Alternatives/<n>/MetaData.plist`, the recordings in `Media/Audio Files` (or an `Audio Files`
//! folder beside the bundle) with the record position stamped in each, and any Final Cut Pro
//! XML or MIDI exports saved next to it, and tracks exported as audio files into a `Bounces`
//! folder. The arrangement itself is in an undocumented `ProjectData` file and is not read.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BundleMeta {
    pub name: String,
    pub tempo: Option<f64>,
    pub signature: Option<(u8, u8)>,
    pub sample_rate: Option<u32>,
    pub key: Option<String>,
    pub audio_dir: Option<PathBuf>,
    pub audio_files: Vec<PathBuf>,
    pub recordings: Vec<Recording>,
    /// Tracks exported as audio files, each rendered from the project start with its edits.
    pub track_exports: Vec<Recording>,
    /// Final Cut Pro XML (`.fcpxml`, `.fcpxmld`) and MIDI files exported from this project.
    pub exports: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub path: PathBuf,
    /// The track it was recorded on, from the file name: "Gtr L#03.wav" was take 3 on "Gtr L".
    pub track: String,
    /// Seconds from the project start where it was recorded, when the file carries a time stamp.
    /// Negative when the pass started before the project start.
    pub position: Option<f64>,
}

/// Bar 1 sits at 01:00:00:00 unless the project was set otherwise, and recordings are stamped
/// with their timecode position. Stamps from `PRE_ROLL` before that hour on count from it, so a
/// pass started just ahead of bar 1 lands just before it rather than an hour into the song.
const PROJECT_START: f64 = 3600.0;
const PRE_ROLL: f64 = 600.0;

/// The `bext` time reference of a Broadcast Wave file, in seconds since midnight. Only the
/// chunk headers are read. Zero means not stamped.
pub fn bwf_time_reference(path: &Path) -> Option<f64> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if !matches!(&head[0..4], b"RIFF" | b"RF64" | b"BW64") || &head[8..12] != b"WAVE" {
        return None;
    }
    let (mut rate, mut reference) = (None, None);
    let mut chunk = [0u8; 8];
    while (rate.is_none() || reference.is_none()) && f.read_exact(&mut chunk).is_ok() {
        let size = u32::from_le_bytes(chunk[4..8].try_into().unwrap()) as u64;
        let next = f.stream_position().ok()? + size + (size & 1);
        match &chunk[0..4] {
            b"fmt " if size >= 8 => {
                let mut fmt = [0u8; 8];
                f.read_exact(&mut fmt).ok()?;
                rate = Some(u32::from_le_bytes(fmt[4..8].try_into().unwrap()));
            }
            // Description, originator, reference, date and time come first: 338 bytes.
            b"bext" if size >= 346 => {
                f.seek(SeekFrom::Current(338)).ok()?;
                let mut t = [0u8; 8];
                f.read_exact(&mut t).ok()?;
                reference = Some(u64::from_le_bytes(t));
            }
            // An RF64 data chunk's size is in ds64; nothing needed lies past it.
            b"data" if size == u32::MAX as u64 => break,
            _ => {}
        }
        f.seek(SeekFrom::Start(next)).ok()?;
    }
    let (rate, reference) = (rate.filter(|r| *r > 0)?, reference.filter(|r| *r > 0)?);
    Some(reference as f64 / rate as f64)
}

fn track_name(file: &Path) -> String {
    let stem = file.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    match stem.rsplit_once('#') {
        Some((name, take)) if take.starts_with(|c: char| c.is_ascii_digit()) && !name.trim().is_empty() => {
            name.trim_end_matches([' ', '_', '-']).to_string()
        }
        _ => stem,
    }
}

/// "Song_03 Gtr L.wav" or "02 - Bass.aif" from exporting all tracks is the track "Gtr L" or
/// "Bass". A bare number is only taken as the track's when zero-padded, so "808 Bass" keeps it.
fn exported_track_name(file: &Path, project: &str) -> String {
    const SEP: [char; 3] = [' ', '_', '-'];
    let stem = file.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let mut name = stem.as_str();
    if name.is_char_boundary(project.len())
        && name[..project.len()].eq_ignore_ascii_case(project)
        && name[project.len()..].starts_with(SEP)
    {
        name = name[project.len()..].trim_start_matches(SEP);
    }
    let rest = name.trim_start_matches(|c: char| c.is_ascii_digit());
    let numbered = rest.len() < name.len()
        && (rest.starts_with(['_', '.']) || rest.starts_with(" - ") || (name.starts_with('0') && rest.starts_with(' ')));
    if numbered {
        name = rest.trim_start_matches([' ', '_', '-', '.']);
    }
    if name.is_empty() { stem } else { name.to_string() }
}

/// A mix bounce carries the project's name, alone or numbered ("Song.wav", "Song 2.wav").
fn is_mix_bounce(file: &Path, project: &str) -> bool {
    let stem = file.file_stem().map_or(String::new(), |s| s.to_string_lossy().to_lowercase());
    let rest = stem.strip_prefix(&project.to_lowercase()).unwrap_or("x");
    rest.trim_start_matches([' ', '_', '-']).chars().all(|c| c.is_ascii_digit())
}

fn audio_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.retain(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO.contains(&e.to_lowercase().as_str())));
    files.sort();
    files
}

fn position(file: &Path) -> Option<f64> {
    bwf_time_reference(file).map(|t| if t >= PROJECT_START - PRE_ROLL { t - PROJECT_START } else { t })
}

/// Tracks exported into a `Bounces` folder in the bundle or, when the bundle has its folder to
/// itself, beside it. Unstamped files start where the export did, at the project start.
fn find_track_exports(bundle: &Path, name: &str, alone: bool) -> Vec<Recording> {
    let mut dirs = vec![bundle.join("Bounces"), bundle.join("Media").join("Bounces")];
    if alone && let Some(parent) = bundle.parent() {
        dirs.push(parent.join("Bounces"));
    }
    let files: Vec<PathBuf> = dirs.iter().flat_map(|d| audio_in(d)).filter(|p| !is_mix_bounce(p, name)).collect();
    // A single file there is a bounce of part of the song, not a set of tracks.
    if files.len() < 2 {
        return Vec::new();
    }
    files
        .into_iter()
        .map(|p| Recording { track: exported_track_name(&p, name), position: Some(position(&p).unwrap_or(0.0)), path: p })
        .collect()
}

fn alone_in_folder(bundle: &Path) -> bool {
    let Some(parent) = bundle.parent() else { return true };
    std::fs::read_dir(parent)
        .map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("logicx"))).count() <= 1)
        .unwrap_or(true)
}

fn is_export(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    matches!(ext.as_str(), "fcpxml" | "fcpxmld" | "mid" | "midi")
}

/// Exports inside the bundle, or beside it when they carry the project's name or the bundle is
/// the only project in its folder.
fn find_exports(bundle: &Path, name: &str, alone: bool) -> Vec<PathBuf> {
    let list = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    };
    let mut out: Vec<PathBuf> = list(bundle).into_iter().filter(|p| is_export(p)).collect();
    if let Some(parent) = bundle.parent() {
        let siblings = list(parent);
        let lower = name.to_lowercase();
        out.extend(siblings.into_iter().filter(|p| {
            is_export(p) && (alone || p.file_stem().is_some_and(|s| s.to_string_lossy().to_lowercase().starts_with(&lower)))
        }));
    }
    out.sort();
    out
}

const AUDIO: [&str; 7] = ["wav", "aif", "aiff", "caf", "mp3", "m4a", "flac"];

fn number(d: &plist::Dictionary, key: &str) -> Option<f64> {
    match d.get(key)? {
        plist::Value::Real(v) => Some(*v),
        plist::Value::Integer(i) => i.as_signed().map(|v| v as f64),
        plist::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn read(bundle: &Path) -> Result<BundleMeta, String> {
    if !bundle.is_dir() {
        return Err("that is not a project bundle folder".into());
    }
    let mut meta = BundleMeta {
        name: bundle.file_stem().map_or("Imported Project".into(), |s| s.to_string_lossy().into_owned()),
        ..Default::default()
    };
    // The alternative saved last is the one that was being worked on.
    let mut alternatives: Vec<PathBuf> = std::fs::read_dir(bundle.join("Alternatives"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.join("MetaData.plist").is_file()).collect())
        .unwrap_or_default();
    let saved = |p: &PathBuf| std::fs::metadata(p.join("MetaData.plist")).and_then(|m| m.modified()).ok();
    alternatives.sort_by(|a, b| saved(b).cmp(&saved(a)).then(a.cmp(b)));
    let Some(alt) = alternatives.first() else {
        return Err("no Alternatives/…/MetaData.plist inside; the bundle may be from an older version".into());
    };
    let value = plist::Value::from_file(alt.join("MetaData.plist")).map_err(|e| format!("cannot read MetaData.plist: {e}"))?;
    let dict = value.as_dictionary().ok_or("MetaData.plist is not a dictionary")?;
    meta.tempo = number(dict, "BeatsPerMinute");
    meta.sample_rate = number(dict, "SampleRate").map(|v| v as u32);
    let num = number(dict, "SongSignatureNumerator");
    let den = number(dict, "SongSignatureDenominator");
    if let (Some(n), Some(d)) = (num, den) {
        meta.signature = Some((n as u8, d as u8));
    }
    let key = dict.get("SongKey").and_then(|v| v.as_string());
    let gender = dict.get("SongGenderKey").and_then(|v| v.as_string()).unwrap_or("major");
    meta.key = key.and_then(|k| crate::parse_key(k, gender));

    // A project saved without copying its media into the bundle keeps it in the folder around it,
    // which is only its own when no other project shares that folder.
    let alone = alone_in_folder(bundle);
    let inside = bundle.join("Media").join("Audio Files");
    let beside = bundle.parent().map(|p| p.join("Audio Files"));
    let audio = if inside.is_dir() { inside } else { beside.filter(|b| alone && b.is_dir()).unwrap_or(inside) };
    if audio.is_dir() {
        let files = audio_in(&audio);
        meta.recordings = files.iter().map(|p| Recording { path: p.clone(), track: track_name(p), position: position(p) }).collect();
        meta.audio_files = files;
        meta.audio_dir = Some(audio);
    }
    meta.exports = find_exports(bundle, &meta.name, alone);
    meta.track_exports = find_track_exports(bundle, &meta.name, alone);
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_metadata_and_media() {
        let dir = std::env::temp_dir().join(format!("solstice-bundle-{}", std::process::id())).join("Song");
        let alt = dir.join("Alternatives/000");
        std::fs::create_dir_all(&alt).unwrap();
        std::fs::create_dir_all(dir.join("Media/Audio Files")).unwrap();
        std::fs::write(dir.join("Media/Audio Files/Vox#01.wav"), b"").unwrap();
        std::fs::write(dir.parent().unwrap().join("Song.fcpxml"), b"").unwrap();
        std::fs::write(dir.parent().unwrap().join("Song Drums.mid"), b"").unwrap();
        std::fs::write(dir.join("Media/Audio Files/notes.txt"), b"").unwrap();
        let mut d = plist::Dictionary::new();
        d.insert("BeatsPerMinute".into(), plist::Value::Real(92.5));
        d.insert("SampleRate".into(), plist::Value::Integer(44100.into()));
        d.insert("SongSignatureNumerator".into(), plist::Value::Integer(6.into()));
        d.insert("SongSignatureDenominator".into(), plist::Value::Integer(8.into()));
        d.insert("SongKey".into(), plist::Value::String("D".into()));
        d.insert("SongGenderKey".into(), plist::Value::String("minor".into()));
        plist::Value::Dictionary(d).to_file_binary(alt.join("MetaData.plist")).unwrap();

        let m = read(&dir).unwrap();
        assert_eq!(m.name, "Song");
        assert_eq!(m.tempo, Some(92.5));
        assert_eq!(m.sample_rate, Some(44100));
        assert_eq!(m.signature, Some((6, 8)));
        assert_eq!(m.key.as_deref(), Some("D minor"));
        assert_eq!(m.audio_files.len(), 1);
        assert_eq!(m.recordings[0].track, "Vox");
        assert_eq!(m.recordings[0].position, None);
        let names: Vec<_> = m.exports.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["Song Drums.mid", "Song.fcpxml"]);
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// A minimal Broadcast Wave file stamped `reference` samples after midnight.
    fn bwf(path: &Path, rate: u32, reference: u64) {
        let mut fmt = vec![1, 0, 1, 0];
        fmt.extend(rate.to_le_bytes());
        fmt.extend((rate * 2).to_le_bytes());
        fmt.extend([2, 0, 16, 0]);
        let mut bext = vec![0u8; 338];
        bext.extend(reference.to_le_bytes());
        bext.extend([0u8; 256]);
        let mut body = b"WAVE".to_vec();
        for (id, data) in [(b"fmt ", &fmt), (b"bext", &bext), (b"data", &vec![0u8; 4])] {
            body.extend(id);
            body.extend((data.len() as u32).to_le_bytes());
            body.extend(data);
        }
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn places_recordings_by_their_time_stamp() {
        let dir = std::env::temp_dir().join(format!("solstice-bwf-{}", std::process::id()));
        let project = dir.join("Take Me.logicx");
        std::fs::create_dir_all(project.join("Alternatives/000")).unwrap();
        std::fs::create_dir_all(dir.join("Audio Files")).unwrap();
        std::fs::write(dir.join("Unrelated.mid"), b"").unwrap();
        plist::Value::Dictionary(plist::Dictionary::new()).to_file_binary(project.join("Alternatives/000/MetaData.plist")).unwrap();
        bwf(&dir.join("Audio Files/Gtr L#02.wav"), 48000, (3600 + 12) * 48000);
        bwf(&dir.join("Audio Files/Bass#01.wav"), 48000, 3600 * 48000 - 24000);
        bwf(&dir.join("Audio Files/Click.wav"), 44100, 0);
        assert_eq!(bwf_time_reference(&dir.join("Audio Files/Gtr L#02.wav")), Some(3612.0));

        let m = read(&project).unwrap();
        let got: Vec<_> = m.recordings.iter().map(|r| (r.track.as_str(), r.position)).collect();
        assert_eq!(got, [("Bass", Some(-0.5)), ("Click", None), ("Gtr L", Some(12.0))]);
        assert_eq!(m.exports.len(), 1);

        // Another project shares the folder, so neither its media nor an unnamed export is taken.
        std::fs::create_dir_all(dir.join("Other.logicx")).unwrap();
        let m = read(&project).unwrap();
        assert!(m.recordings.is_empty());
        assert!(m.exports.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finds_tracks_exported_as_audio() {
        let dir = std::env::temp_dir().join(format!("solstice-bounces-{}", std::process::id()));
        let project = dir.join("Song.logicx");
        std::fs::create_dir_all(project.join("Alternatives/000")).unwrap();
        std::fs::create_dir_all(dir.join("Bounces")).unwrap();
        plist::Value::Dictionary(plist::Dictionary::new()).to_file_binary(project.join("Alternatives/000/MetaData.plist")).unwrap();
        for f in ["Song_01 Kick.wav", "02 - Gtr L.aif", "Song.wav", "Song 2.mp3"] {
            std::fs::write(dir.join("Bounces").join(f), b"").unwrap();
        }
        bwf(&dir.join("Bounces/Song_03 Vox.wav"), 48000, (3600 + 4) * 48000);

        let m = read(&project).unwrap();
        let got: Vec<_> = m.track_exports.iter().map(|r| (r.track.as_str(), r.position)).collect();
        assert_eq!(got, [("Gtr L", Some(0.0)), ("Kick", Some(0.0)), ("Vox", Some(4.0))]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exported_names() {
        let n = |f: &str| exported_track_name(Path::new(f), "Song");
        assert_eq!(n("Song_Bass.wav"), "Bass");
        assert_eq!(n("12_Snare Top.wav"), "Snare Top");
        assert_eq!(n("03 Vox.wav"), "Vox");
        assert_eq!(n("808 Bass.wav"), "808 Bass");
        assert_eq!(n("Songbird.wav"), "Songbird");
    }

    #[test]
    fn rejects_non_bundles() {
        assert!(read(Path::new("/definitely/not/here")).is_err());
    }
}
