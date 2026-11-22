//! Standard MIDI Files: notes per track (or per channel in a type 0 file), plus the tempo map,
//! time signatures, markers and key from whichever track carries them.

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
    /// The opening tempo and signature, also the first entries of `tempos` and `signatures`.
    pub tempo: Option<f64>,
    pub signature: Option<(u8, u8)>,
    /// (tick, quarter notes per minute), sorted, one per tick.
    pub tempos: Vec<(u64, f64)>,
    /// (tick, numerator, denominator), sorted, one per tick.
    pub signatures: Vec<(u64, u8, u8)>,
    pub markers: Vec<(u64, String)>,
    /// The first key signature, named as in `crate::key_name`.
    pub key: Option<String>,
    pub tracks: Vec<Track>,
}

pub fn parse(bytes: &[u8]) -> Result<MidiFile, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a MIDI file: {e}"))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as u32,
        Timing::Timecode(..) => return Err("timecode-based MIDI files are not supported".into()),
    };
    let mut tempos = Vec::new();
    let mut signatures = Vec::new();
    let mut markers = Vec::new();
    let mut key = None;
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
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if us.as_int() > 0 => {
                    tempos.push((now, 60_000_000.0 / us.as_int() as f64));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) if n > 0 && d < 8 => {
                    signatures.push((now, n, 1u8 << d));
                }
                TrackEventKind::Meta(MetaMessage::Marker(t) | MetaMessage::CuePoint(t)) => {
                    let text = String::from_utf8_lossy(t).trim().to_string();
                    if !text.is_empty() && !markers.iter().any(|(at, m)| *at == now && *m == text) {
                        markers.push((now, text));
                    }
                }
                TrackEventKind::Meta(MetaMessage::KeySignature(sharps, minor)) if key.is_none() => {
                    key = Some(crate::key_name((sharps as i32 * 7).rem_euclid(12) as u8 + if minor { 9 } else { 0 }, minor));
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
    // A later event at the same tick wins, as when a file restates the opening tempo.
    tempos.sort_by_key(|(t, _)| *t);
    tempos.reverse();
    tempos.dedup_by_key(|(t, _)| *t);
    tempos.reverse();
    signatures.sort_by_key(|(t, ..)| *t);
    signatures.reverse();
    signatures.dedup_by_key(|(t, ..)| *t);
    signatures.reverse();
    markers.sort_by_key(|(t, _)| *t);
    let tempo = tempos.first().map(|(_, t)| *t);
    let signature = signatures.first().map(|&(_, n, d)| (n, d));
    Ok(MidiFile { ppq, tempo, signature, tempos, signatures, markers, key, tracks })
}

/// The 0-based bar each signature starts at, counting bars of the signature before it.
/// Changes that fall inside a bar move to the start of the next one.
pub fn signature_bars(signatures: &[(u64, u8, u8)], ppq: u32) -> Vec<(u64, u8, u8)> {
    let mut out: Vec<(u64, u8, u8)> = Vec::new();
    let (mut at_tick, mut at_bar, mut cur) = (0u64, 0u64, (4u8, 4u8));
    for &(tick, n, d) in signatures {
        let bar_ticks = (ppq as u64 * 4 * cur.0 as u64 / cur.1 as u64).max(1);
        let bar = at_bar + (tick.saturating_sub(at_tick)).div_ceil(bar_ticks);
        at_tick += (bar - at_bar) * bar_ticks;
        at_bar = bar;
        cur = (n, d);
        if out.last().is_some_and(|l| l.0 == bar) {
            out.pop();
        }
        out.push((bar, n, d));
    }
    out
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
        assert_eq!(f.tempos, vec![(0, 120.0)]);
        assert_eq!(f.tracks.len(), 1);
        assert_eq!(f.tracks[0].name, "Piano");
        assert_eq!(
            f.tracks[0].notes,
            vec![Note { start: 0, length: 480, pitch: 60, velocity: 100 }, Note { start: 480, length: 240, pitch: 64, velocity: 90 }]
        );
    }

    #[test]
    fn reads_tempo_map_markers_and_key() {
        let conductor = vec![
            ev(0, TrackEventKind::Meta(MetaMessage::Tempo(500_000.into()))),
            ev(0, TrackEventKind::Meta(MetaMessage::TimeSignature(4, 2, 24, 8))),
            ev(0, TrackEventKind::Meta(MetaMessage::KeySignature(-3, true))),
            ev(0, TrackEventKind::Meta(MetaMessage::Marker(b"Intro"))),
            ev(1920, TrackEventKind::Meta(MetaMessage::Marker(b"Verse"))),
            ev(1920, TrackEventKind::Meta(MetaMessage::Tempo(400_000.into()))),
            ev(0, TrackEventKind::Meta(MetaMessage::TimeSignature(7, 3, 24, 8))),
            ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ];
        let smf = Smf { header: Header::new(Format::Parallel, Timing::Metrical(480.into())), tracks: vec![conductor] };
        let mut bytes = Vec::new();
        smf.write_std(&mut bytes).unwrap();
        let f = parse(&bytes).unwrap();
        assert_eq!(f.tempos, vec![(0, 120.0), (3840, 150.0)]);
        assert_eq!(f.signatures, vec![(0, 4, 4), (3840, 7, 8)]);
        assert_eq!(f.markers, vec![(0, "Intro".to_string()), (1920, "Verse".to_string())]);
        assert_eq!(f.key.as_deref(), Some("C minor"));
        assert_eq!(signature_bars(&f.signatures, f.ppq), vec![(0, 4, 4), (2, 7, 8)]);
    }

    #[test]
    fn signature_changes_inside_a_bar_move_to_the_next() {
        assert_eq!(signature_bars(&[(0, 3, 4), (1000, 4, 4)], 480), vec![(0, 3, 4), (1, 4, 4)]);
        assert_eq!(signature_bars(&[(0, 4, 4), (0, 6, 8)], 480), vec![(0, 6, 8)]);
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
