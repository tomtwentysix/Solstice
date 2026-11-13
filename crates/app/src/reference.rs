use solstice_dsp::loudness;
use solstice_engine::audio::AudioData;
use solstice_model::{Content, Track};
use std::sync::Arc;

/// Level matching never moves a reference further than this, so a near-silent mix can't make
/// it blast.
const MAX_MATCH_DB: f32 = 24.0;
const CHUNK: usize = 4096;

/// A stretch of audio a reference track plays, as measured for level matching.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub file: String,
    pub offset: u64,
    pub frames: u64,
    pub gain_db: f32,
}

pub fn segments(t: &Track) -> Vec<Segment> {
    let mut out = Vec::new();
    for r in t.regions.iter().filter(|r| !r.muted) {
        let Content::Audio(a) = &r.content else { continue };
        for piece in a.pieces(r.length) {
            out.push(Segment { file: piece.file.to_string(), offset: piece.offset, frames: piece.end - piece.start, gain_db: a.gain_db });
        }
    }
    out
}

/// Integrated loudness of the segments played one after another, or `None` if they are silent.
pub fn measure(parts: &[(Arc<AudioData>, &Segment)], sample_rate: u32) -> Option<f32> {
    let mut meter = loudness::Meter::new(sample_rate as f64);
    let (mut l, mut r) = (Vec::with_capacity(CHUNK), Vec::with_capacity(CHUNK));
    for (data, s) in parts {
        let gain = solstice_model::db_to_gain(s.gain_db);
        let end = (s.offset + s.frames).min(data.frames);
        let mut f = s.offset;
        while f < end {
            let to = (f + CHUNK as u64).min(end);
            l.clear();
            r.clear();
            for i in f..to {
                let a = data.sample(0, i) * gain;
                l.push(a);
                r.push(if data.channels > 1 { data.sample(1, i) * gain } else { a });
            }
            meter.push(&l, &r);
            f = to;
        }
    }
    let v = meter.integrated();
    (v.is_finite() && v > -70.0).then_some(v as f32)
}

/// What the reference is matched to: the mix's integrated loudness once there is one, its
/// short-term loudness until then.
pub fn mix_loudness(m: &loudness::Meter) -> Option<f32> {
    [m.integrated(), m.short_term()].into_iter().find(|v| v.is_finite() && *v > -70.0).map(|v| v as f32)
}

/// The level-match gain to head for: `None` holds the current one while the mix is silent.
pub fn match_target(on: bool, mix: Option<f32>, reference: Option<f32>) -> Option<f32> {
    match (on, mix, reference) {
        (true, Some(m), Some(r)) => Some((m - r).clamp(-MAX_MATCH_DB, MAX_MATCH_DB)),
        (true, None, Some(_)) => None,
        _ => Some(0.0),
    }
}

/// One display frame's move towards `target`, settling within a second or so.
pub fn glide(current: f32, target: f32) -> f32 {
    let next = current + (target - current) * 0.1;
    if (target - next).abs() < 0.01 { target } else { next }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstice_model::{AudioContent, Project, Region, TrackKind};

    const SR: u32 = 48000;

    #[test]
    fn measures_the_regions_as_they_play() {
        let dir = std::env::temp_dir().join(format!("solstice-reference-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sine.wav");
        // A 1 kHz sine at −23 dBFS in both channels reads −23 LUFS.
        let a = 10f32.powf(-23.0 / 20.0);
        let sine: Vec<f32> = (0..SR as usize * 6).map(|n| a * (std::f32::consts::TAU * 1000.0 * n as f32 / SR as f32).sin()).collect();
        solstice_engine::audio::write_wav(&path, SR, &[sine.clone(), sine]).unwrap();
        let data = Arc::new(AudioData::open(&path).unwrap());

        let mut p = Project::new("t", SR);
        let id = p.add_track(TrackKind::Audio);
        let t = p.track_mut(id).unwrap();
        let mut content = AudioContent::new("sine.wav");
        content.gain_db = 6.0;
        t.regions.push(Region { id: 1, name: "r".into(), start: 0, length: 4 * SR as u64, muted: false, loops: 0, color: None, content: Content::Audio(content) });
        let content = Content::Audio(AudioContent::new("other.wav"));
        t.regions.push(Region { id: 2, name: "m".into(), start: 0, length: 100, muted: true, loops: 0, color: None, content });
        let segs = segments(t);
        assert_eq!(segs, vec![Segment { file: "sine.wav".into(), offset: 0, frames: 4 * SR as u64, gain_db: 6.0 }]);

        let lufs = measure(&[(data.clone(), &segs[0])], SR).unwrap();
        assert!((lufs + 17.0).abs() < 0.1, "{lufs}");
        let silent = Segment { offset: 10 * SR as u64, ..segs[0].clone() };
        assert_eq!(measure(&[(data, &silent)], SR), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matching_follows_the_mix_and_holds_through_silence() {
        assert_eq!(match_target(true, Some(-14.0), Some(-8.0)), Some(-6.0));
        assert_eq!(match_target(true, Some(-60.0), Some(-8.0)), Some(-MAX_MATCH_DB));
        assert_eq!(match_target(true, None, Some(-8.0)), None);
        assert_eq!(match_target(false, Some(-14.0), Some(-8.0)), Some(0.0));
        assert_eq!(match_target(true, Some(-14.0), None), Some(0.0));

        let mut m = loudness::Meter::new(SR as f64);
        assert_eq!(mix_loudness(&m), None);
        let a = 10f32.powf(-20.0 / 20.0);
        let s: Vec<f32> = (0..SR as usize).map(|n| a * (std::f32::consts::TAU * 1000.0 * n as f32 / SR as f32).sin()).collect();
        m.push(&s, &s);
        assert!((mix_loudness(&m).unwrap() + 20.0).abs() < 0.2);

        let mut g = 0.0;
        for _ in 0..80 {
            g = glide(g, -6.0);
        }
        assert_eq!(g, -6.0);
    }
}
