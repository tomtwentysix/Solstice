//! Standard MIDI Files: notes per track (or per channel in a type 0 file), plus the first tempo
//! and time signature.

use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    pub start: u64,
    pub length: u64,
    pub pitch: u8,
    pub velocity: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub name: String,
    pub channel: u8,
    /// Times in ticks at the file's `ppq`.
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MidiFile {
    pub ppq: u32,
    pub tempo: Option<f64>,
    pub signature: Option<(u8, u8)>,
    pub tracks: Vec<Track>,
}

pub fn parse(bytes: &[u8]) -> Result<MidiFile, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a MIDI file: {e}"))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as u32,
        Timing::Timecode(..) => return Err("timecode-based MIDI files are not supported".into()),
    };
    let mut tempo = None;
    let mut signature = None;
    let mut tracks = Vec::new();
    for events in &smf.tracks {
        let mut name = String::new();
        let mut now = 0u64;
        // Notes are split by channel so a type 0 file still yields one track per part.
        let mut by_channel: HashMap<u8, Vec<Note>> = HashMap::new();
        let mut open: HashMap<(u8, u8), (u64, u8)> = HashMap::new();
        for e in events {
            now += e.delta.as_int() as u64;
            match e.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if tempo.is_none() => {
                    tempo = Some(60_000_000.0 / us.as_int() as f64);
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) if signature.is_none() => {
                    signature = Some((n, 1u8 << d));
                }
                TrackEventKind::Meta(MetaMessage::TrackName(t)) if name.is_empty() => {
                    name = String::from_utf8_lossy(t).trim().to_string();
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    let (on, key, vel) = match message {
                        MidiMessage::NoteOn { key, vel } => (vel.as_int() > 0, key.as_int(), vel.as_int()),
                        MidiMessage::NoteOff { key, vel } => (false, key.as_int(), vel.as_int()),
                        _ => continue,
                    };
                    if on {
                        open.insert((ch, key), (now, vel));
                    } else if let Some((s, v)) = open.remove(&(ch, key)) {
                        by_channel.entry(ch).or_default().push(Note { start: s, length: (now - s).max(1), pitch: key, velocity: v });
                    }
                }
                _ => {}
            }
        }
        // Notes still held at the end of the track last until it ends.
        for ((ch, key), (s, v)) in open {
            by_channel.entry(ch).or_default().push(Note { start: s, length: (now - s).max(1), pitch: key, velocity: v });
        }
        let several = by_channel.len() > 1;
        let mut chans: Vec<_> = by_channel.into_iter().collect();
        chans.sort_by_key(|(c, _)| *c);
        for (ch, mut notes) in chans {
            notes.sort_by_key(|n| (n.start, n.pitch));
            let base = if name.is_empty() { format!("MIDI {}", tracks.len() + 1) } else { name.clone() };
            let name = if several { format!("{base} (Ch {})", ch + 1) } else { base };
            tracks.push(Track { name, channel: ch, notes });
        }
    }
    Ok(MidiFile { ppq, tempo, signature, tracks })
}

#[cfg(test)]
mod tests {
    use super::*;
    use midly::{Format, Header, TrackEvent};

    fn ev(delta: u32, kind: TrackEventKind<'static>) -> TrackEvent<'static> {
        TrackEvent { delta: delta.into(), kind }
    }

    fn note(delta: u32, on: bool, ch: u8, key: u8, vel: u8) -> TrackEvent<'static> {
        let message = if on {
            MidiMessage::NoteOn { key: key.into(), vel: vel.into() }
        } else {
            MidiMessage::NoteOff { key: key.into(), vel: 0.into() }
        };
        ev(delta, TrackEventKind::Midi { channel: ch.into(), message })
    }

    #[test]
    fn reads_tempo_signature_and_notes() {
        let conductor = vec![
            ev(0, TrackEventKind::Meta(MetaMessage::Tempo(500_000.into()))),
            ev(0, TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8))),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let piano = vec![
            ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Piano"))),
            note(0, true, 0, 60, 100),
            note(480, false, 0, 60, 0),
            note(0, true, 0, 64, 90),
            // A note-on with velocity 0 is a note-off.
            note(240, true, 0, 64, 0),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let smf = Smf { header: Header::new(Format::Parallel, Timing::Metrical(480.into())), tracks: vec![conductor, piano] };
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let f = parse(&bytes).unwrap();
        assert_eq!(f.ppq, 480);
        assert_eq!(f.tempo, Some(120.0));
        assert_eq!(f.signature, Some((3, 4)));
        assert_eq!(f.tracks.len(), 1);
        assert_eq!(f.tracks[0].name, "Piano");
        assert_eq!(
            f.tracks[0].notes,
            vec![Note { start: 0, length: 480, pitch: 60, velocity: 100 }, Note { start: 480, length: 240, pitch: 64, velocity: 90 }]
        );
    }

    #[test]
    fn splits_type_0_by_channel() {
        let t = vec![note(0, true, 0, 60, 100), note(0, true, 9, 36, 110), note(100, false, 0, 60, 0), note(0, false, 9, 36, 0)];
        let smf = Smf { header: Header::new(Format::SingleTrack, Timing::Metrical(96.into())), tracks: vec![t] };
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let f = parse(&bytes).unwrap();
        assert_eq!(f.tracks.len(), 2);
        assert_eq!(f.tracks[1].channel, 9);
        assert_eq!(f.tracks[1].notes[0].pitch, 36);
    }
}
