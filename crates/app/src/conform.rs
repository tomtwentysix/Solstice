//! Keeps the audio a project plays at the project's sample rate. Playback reads files sample
//! for sample, so a file at another rate would play at the wrong speed and pitch; it is
//! converted once, beside the original, and the project points at the copy.

use solstice_engine::audio::{AudioData, write_wav};
use solstice_engine::resample::resample;
use solstice_model::{AudioContent, Bundle, Content, Project};
use std::collections::HashMap;
use std::path::Path;

/// Returns the name, in `dir`, of a copy of `path` at `rate`, with the rate the file had, or
/// `None` when it is already at `rate`. An earlier copy is reused.
pub fn convert_file(path: &Path, dir: &Path, rate: u32) -> Result<Option<(String, u32)>, String> {
    let data = AudioData::open(path).map_err(|e| e.to_string())?;
    if data.sample_rate == rate {
        return Ok(None);
    }
    let stem = path.file_stem().map_or("Audio".into(), |s| s.to_string_lossy().into_owned());
    let name = format!("{stem} ({rate} Hz).wav");
    let target = dir.join(&name);
    if AudioData::open(&target).is_ok_and(|d| d.sample_rate == rate) {
        return Ok(Some((name, data.sample_rate)));
    }
    let channels: Vec<Vec<f32>> = (0..data.channels)
        .map(|c| {
            let raw: Vec<f32> = (0..data.frames).map(|f| data.sample(c, f)).collect();
            resample(&raw, data.sample_rate, rate)
        })
        .collect();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    write_wav(&target, rate, &channels).map_err(|e| e.to_string())?;
    Ok(Some((name, data.sample_rate)))
}

/// Converts every file the project's audio regions and takes use that is not at the project's
/// rate. Returns how many files were converted and the ones that could not be.
pub fn conform(project: &mut Project, bundle: &Bundle) -> (usize, Vec<String>) {
    let rate = project.sample_rate;
    let dir = bundle.audio_dir();
    let mut done: HashMap<String, Option<(String, u32)>> = HashMap::new();
    let mut failed = Vec::new();
    let mut fix = |file: &mut String, offset: &mut u64| {
        let result = done.entry(file.clone()).or_insert_with(|| match convert_file(&bundle.audio_path(file), &dir, rate) {
            Ok(r) => r,
            Err(e) => {
                failed.push(format!("{file}: {e}"));
                None
            }
        });
        if let Some((name, from)) = result {
            *offset = (*offset as f64 * rate as f64 / *from as f64).round() as u64;
            *file = name.clone();
        }
    };
    for t in &mut project.tracks {
        for r in t.regions.iter_mut().chain(t.alternatives.iter_mut().flat_map(|a| a.regions.iter_mut())) {
            if let Content::Audio(a) = &mut r.content {
                let AudioContent { file, offset, takes, .. } = a;
                fix(file, offset);
                for take in takes {
                    fix(&mut take.file, &mut take.offset);
                }
            }
        }
    }
    let converted = done.values().filter(|r| r.is_some()).count();
    (converted, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstice_model::{Region, Take, TrackKind};

    #[test]
    fn files_at_another_rate_are_converted_and_offsets_follow() {
        let dir = std::env::temp_dir().join(format!("solstice-conform-{}", std::process::id()));
        let bundle = Bundle::new(&dir);
        std::fs::create_dir_all(bundle.audio_dir()).unwrap();
        let tone: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin()).collect();
        write_wav(&bundle.audio_path("guitar.wav"), 44100, &[tone.clone()]).unwrap();
        write_wav(&bundle.audio_path("bass.wav"), 48000, &[tone]).unwrap();

        let mut p = Project::new("A", 48000);
        let t = p.add_track(TrackKind::Audio);
        let id = p.alloc_id();
        let mut content = AudioContent::new("guitar.wav");
        content.offset = 4410;
        content.takes = vec![Take { name: "Take 1".into(), file: "guitar.wav".into(), offset: 0 }, Take { name: "Take 2".into(), file: "bass.wav".into(), offset: 100 }];
        p.track_mut(t).unwrap().regions.push(Region { id, name: "R".into(), start: 0, length: 48000, muted: false, loops: 0, color: None, content: Content::Audio(content) });

        let (converted, failed) = conform(&mut p, &bundle);
        assert_eq!((converted, failed.len()), (1, 0));
        let Content::Audio(a) = &p.tracks[0].regions[0].content else { panic!() };
        assert_eq!((a.file.as_str(), a.offset), ("guitar (48000 Hz).wav", 4800));
        assert_eq!((a.takes[0].file.as_str(), a.takes[1].file.as_str(), a.takes[1].offset), ("guitar (48000 Hz).wav", "bass.wav", 100));
        let copy = AudioData::open(&bundle.audio_path(&a.file)).unwrap();
        assert_eq!((copy.sample_rate, copy.frames), (48000, 48000));
        assert!(bundle.audio_path("guitar.wav").exists());

        // A second pass finds nothing left to do.
        assert_eq!(conform(&mut p, &bundle).0, 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
