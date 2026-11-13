use solstice_dsp::sampler::{SampleData, Zone, ZoneDesc, note_number};
use solstice_engine::audio::AudioData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Zone paths are stored relative to the project's Audio Files folder when the sample lives
/// there, so projects can be moved and copied.
pub fn resolve(path: &Path, audio_dir: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() } else { audio_dir.join(path) }
}

/// Loads a zone at `sample_rate`. A file at another rate is converted into `audio_dir` first,
/// and the zone, with its loop points, moves to the copy so it keeps its pitch.
pub fn load_zone(desc: &ZoneDesc, audio_dir: &Path, sample_rate: f64) -> Option<Zone> {
    let mut desc = desc.clone();
    if let Ok(Some((name, from))) = crate::conform::convert_file(&resolve(&desc.path, audio_dir), audio_dir, sample_rate.round() as u32) {
        let scale = |f: usize| (f as f64 * sample_rate / from as f64).round() as usize;
        desc.loop_points = desc.loop_points.map(|(a, b)| (scale(a), scale(b)));
        desc.path = PathBuf::from(name);
    }
    let data = AudioData::open(&resolve(&desc.path, audio_dir)).ok()?;
    let channels = (0..data.channels.min(2)).map(|c| (0..data.frames).map(|f| data.sample(c, f)).collect()).collect();
    Some(Zone { desc, sample: Arc::new(SampleData { channels }) })
}

/// Converts a sample into the project and returns its path relative to Audio Files.
pub fn add_file(src: &Path, audio_dir: &Path, sample_rate: u32) -> Result<PathBuf, String> {
    let (name, _) = crate::import::import(src, audio_dir, sample_rate)?;
    Ok(PathBuf::from(name))
}

/// Picks a note out of a file name such as "Piano C4.wav" or "kick_36.wav".
pub fn note_from_name(name: &str) -> Option<u8> {
    let stem = Path::new(name).file_stem()?.to_string_lossy().into_owned();
    let words: Vec<&str> = stem.split(|c: char| c == ' ' || c == '_' || c == '-' || c == '.').filter(|w| !w.is_empty()).collect();
    words.iter().rev().find_map(|w| {
        let looks_like_note = w.chars().next().is_some_and(|c| "abcdefgABCDEFG".contains(c)) && w.chars().last().is_some_and(|c| c.is_ascii_digit());
        let midi_number = w.chars().all(|c| c.is_ascii_digit()) && w.len() <= 3;
        if looks_like_note || midi_number { note_number(w) } else { None }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_from_names() {
        assert_eq!(note_from_name("Piano C4.wav"), Some(60));
        assert_eq!(note_from_name("strings_A#3.flac"), Some(58));
        assert_eq!(note_from_name("kick_36.wav"), Some(36));
        assert_eq!(note_from_name("Snare.wav"), None);
    }
}
