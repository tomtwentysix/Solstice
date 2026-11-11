//! What can be read openly from a Logic `.logicx` bundle: the song settings in
//! `Alternatives/<n>/MetaData.plist` and the recordings in `Media/Audio Files`. The arrangement
//! itself is in Logic's undocumented `ProjectData` and is not read.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogicMeta {
    pub name: String,
    pub tempo: Option<f64>,
    pub signature: Option<(u8, u8)>,
    pub sample_rate: Option<u32>,
    pub key: Option<String>,
    pub audio_dir: Option<PathBuf>,
    pub audio_files: Vec<PathBuf>,
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

pub fn read(bundle: &Path) -> Result<LogicMeta, String> {
    if !bundle.is_dir() {
        return Err("a Logic project is a folder ending in .logicx".into());
    }
    let mut meta = LogicMeta {
        name: bundle.file_stem().map_or("Logic Project".into(), |s| s.to_string_lossy().into_owned()),
        ..Default::default()
    };
    // The first alternative is the project as last saved unless another was made current.
    let mut alternatives: Vec<PathBuf> = std::fs::read_dir(bundle.join("Alternatives"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.join("MetaData.plist").is_file()).collect())
        .unwrap_or_default();
    alternatives.sort();
    let Some(alt) = alternatives.first() else {
        return Err("no Alternatives/…/MetaData.plist inside; is this a Logic Pro X or later project?".into());
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
    let key = dict.get("SongKey").and_then(|v| v.as_string()).map(str::to_string);
    let gender = dict.get("SongGenderKey").and_then(|v| v.as_string()).map(str::to_string);
    meta.key = key.map(|k| match gender {
        Some(g) => format!("{k} {g}"),
        None => k,
    });

    let audio = bundle.join("Media").join("Audio Files");
    if audio.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&audio)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO.contains(&e.to_lowercase().as_str())));
        files.sort();
        meta.audio_files = files;
        meta.audio_dir = Some(audio);
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_metadata_and_media() {
        let dir = std::env::temp_dir().join(format!("solstice-logicx-{}", std::process::id())).join("Song.logicx");
        let alt = dir.join("Alternatives/000");
        std::fs::create_dir_all(&alt).unwrap();
        std::fs::create_dir_all(dir.join("Media/Audio Files")).unwrap();
        std::fs::write(dir.join("Media/Audio Files/Vox#01.wav"), b"").unwrap();
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
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn rejects_non_bundles() {
        assert!(read(Path::new("/definitely/not/here.logicx")).is_err());
    }
}
