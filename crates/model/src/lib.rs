mod history;
mod time;

pub use history::History;
pub use time::{PPQ, TimeMap};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Id = u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub name: String,
    pub sample_rate: u32,
    pub tempo: f64,
    pub sig_num: u8,
    pub sig_den: u8,
    pub tracks: Vec<Track>,
    pub buses: Vec<Bus>,
    pub master: Channel,
    pub cycle: Range,
    pub cycle_on: bool,
    pub punch: Range,
    pub punch_on: bool,
    pub metronome: bool,
    /// Logic's Dim: drops the stereo output by 20 dB without moving its fader.
    #[serde(default)]
    pub dim: bool,
    pub next_id: Id,
}

/// Timeline span in samples, `end` exclusive.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    pub fn new(start: u64, end: u64) -> Self {
        Range { start: start.min(end), end: start.max(end) }
    }
    pub fn len(&self) -> u64 {
        self.end - self.start
    }
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
    pub fn contains(&self, pos: u64) -> bool {
        pos >= self.start && pos < self.end
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Instrument,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub id: Id,
    pub name: String,
    pub kind: TrackKind,
    pub color: u8,
    pub channel: Channel,
    /// First hardware input (0-based); stereo tracks use `input` and `input + 1`.
    pub input: u32,
    pub stereo: bool,
    pub armed: bool,
    pub monitor: bool,
    pub instrument: Option<PluginSlot>,
    pub regions: Vec<Region>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bus {
    pub id: Id,
    pub name: String,
    pub channel: Channel,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Output {
    Master,
    Bus(Id),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    /// Stays audible while other channels are soloed.
    #[serde(default)]
    pub solo_safe: bool,
    pub inserts: Vec<PluginSlot>,
    pub sends: Vec<Send>,
    pub output: Output,
    #[serde(default)]
    pub eq: ChannelEq,
}

impl Default for Channel {
    fn default() -> Self {
        Channel {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            solo_safe: false,
            inserts: Vec::new(),
            sends: Vec::new(),
            output: Output::Master,
            eq: ChannelEq::default(),
        }
    }
}

/// The built-in channel EQ, laid out like Logic's: low cut, low shelf, four bells, high shelf,
/// high cut. Band kinds are fixed by position.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelEq {
    pub enabled: bool,
    pub bands: [EqBand; EQ_BANDS],
}

pub const EQ_BANDS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EqKind {
    LowCut,
    LowShelf,
    Bell,
    HighShelf,
    HighCut,
}

impl EqKind {
    pub fn of(band: usize) -> EqKind {
        match band {
            0 => EqKind::LowCut,
            1 => EqKind::LowShelf,
            6 => EqKind::HighShelf,
            7 => EqKind::HighCut,
            _ => EqKind::Bell,
        }
    }

    pub fn is_cut(self) -> bool {
        matches!(self, EqKind::LowCut | EqKind::HighCut)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct EqBand {
    pub on: bool,
    pub freq: f32,
    pub gain_db: f32,
    pub q: f32,
    /// Cut bands only: 6, 12, 18, 24, 36 or 48 dB per octave.
    pub slope: u8,
}

pub const EQ_SLOPES: [u8; 6] = [6, 12, 18, 24, 36, 48];

impl Default for ChannelEq {
    fn default() -> Self {
        let band = |on, freq, q| EqBand { on, freq, gain_db: 0.0, q, slope: 24 };
        ChannelEq {
            enabled: false,
            bands: [
                band(false, 30.0, 0.71),
                band(true, 80.0, 0.71),
                band(true, 200.0, 1.1),
                band(true, 500.0, 1.1),
                band(true, 1500.0, 1.1),
                band(true, 5000.0, 1.1),
                band(true, 10000.0, 0.71),
                band(false, 18000.0, 0.71),
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Send {
    pub bus: Id,
    pub level_db: f32,
    pub mode: SendMode,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SendMode {
    PreFader,
    /// After the fader, before the pan control.
    PostFader,
    #[default]
    PostPan,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PluginSlot {
    pub id: Id,
    /// VST3 class ID as 32 hex digits.
    pub uid: String,
    pub path: PathBuf,
    pub name: String,
    pub bypass: bool,
    #[serde(with = "b64")]
    pub state: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Region {
    pub id: Id,
    pub name: String,
    pub start: u64,
    pub length: u64,
    pub muted: bool,
    pub content: Content,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start + self.length
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Content {
    Audio(AudioContent),
    Midi(MidiContent),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AudioContent {
    /// Relative to the project's `Audio Files` folder.
    pub file: String,
    /// Frame offset into the file where the region starts.
    pub offset: u64,
    pub gain_db: f32,
    pub fade_in: u64,
    pub fade_out: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiContent {
    pub notes: Vec<Note>,
}

/// Note times are in ticks (`PPQ` per quarter note) from the region start.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub start: u32,
    pub length: u32,
    pub pitch: u8,
    pub velocity: u8,
}

pub const TRACK_COLORS: usize = 8;

impl Project {
    pub fn new(name: &str, sample_rate: u32) -> Self {
        let bar = TimeMap::new(sample_rate, 120.0, 4, 4).bar_len();
        Project {
            name: name.to_string(),
            sample_rate,
            tempo: 120.0,
            sig_num: 4,
            sig_den: 4,
            tracks: Vec::new(),
            buses: Vec::new(),
            master: Channel::default(),
            cycle: Range::new(0, bar * 4),
            cycle_on: false,
            punch: Range::new(bar * 2, bar * 4),
            punch_on: false,
            metronome: false,
            dim: false,
            next_id: 1,
        }
    }

    pub fn time(&self) -> TimeMap {
        TimeMap::new(self.sample_rate, self.tempo, self.sig_num, self.sig_den)
    }

    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn add_track(&mut self, kind: TrackKind) -> Id {
        let id = self.alloc_id();
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = match kind {
            TrackKind::Audio => format!("Audio {n}"),
            TrackKind::Instrument => format!("Inst {n}"),
        };
        self.tracks.push(Track {
            id,
            name,
            kind,
            color: (self.tracks.len() % TRACK_COLORS) as u8,
            channel: Channel::default(),
            input: 0,
            stereo: kind == TrackKind::Instrument,
            armed: false,
            monitor: false,
            instrument: None,
            regions: Vec::new(),
        });
        id
    }

    pub fn add_bus(&mut self) -> Id {
        let id = self.alloc_id();
        let name = format!("Bus {}", self.buses.len() + 1);
        self.buses.push(Bus { id, name, channel: Channel::default() });
        id
    }

    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn channel_mut(&mut self, id: Option<Id>) -> Option<&mut Channel> {
        let Some(id) = id else { return Some(&mut self.master) };
        if let Some(t) = self.tracks.iter_mut().find(|t| t.id == id) {
            return Some(&mut t.channel);
        }
        self.buses.iter_mut().find(|b| b.id == id).map(|b| &mut b.channel)
    }

    pub fn channel(&self, id: Option<Id>) -> Option<&Channel> {
        let Some(id) = id else { return Some(&self.master) };
        if let Some(t) = self.track(id) {
            return Some(&t.channel);
        }
        self.buses.iter().find(|b| b.id == id).map(|b| &b.channel)
    }

    /// Returns the track index and region.
    pub fn find_region(&self, id: Id) -> Option<(usize, &Region)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(i, t)| t.regions.iter().find(|r| r.id == id).map(|r| (i, r)))
    }

    pub fn region_mut(&mut self, id: Id) -> Option<&mut Region> {
        self.tracks.iter_mut().flat_map(|t| t.regions.iter_mut()).find(|r| r.id == id)
    }

    pub fn remove_region(&mut self, id: Id) -> Option<Region> {
        for t in &mut self.tracks {
            if let Some(i) = t.regions.iter().position(|r| r.id == id) {
                return Some(t.regions.remove(i));
            }
        }
        None
    }

    /// Splits a region at an absolute timeline position, returning the new right-hand region's id.
    pub fn split_region(&mut self, id: Id, at: u64) -> Option<Id> {
        let new_id = self.next_id;
        let time = self.time();
        let region = self.region_mut(id)?;
        if at <= region.start || at >= region.end() {
            return None;
        }
        let cut = at - region.start;
        let mut right = region.clone();
        right.id = new_id;
        right.start = at;
        right.length = region.length - cut;
        region.length = cut;
        match (&mut region.content, &mut right.content) {
            (Content::Audio(l), Content::Audio(r)) => {
                r.offset += cut;
                r.fade_in = 0;
                l.fade_out = 0;
            }
            (Content::Midi(l), Content::Midi(r)) => {
                let cut_ticks = time.samples_to_ticks(cut) as u32;
                l.notes.retain(|n| n.start < cut_ticks);
                for n in &mut l.notes {
                    n.length = n.length.min(cut_ticks - n.start);
                }
                r.notes.retain(|n| n.start >= cut_ticks);
                for n in &mut r.notes {
                    n.start -= cut_ticks;
                }
            }
            _ => unreachable!(),
        }
        self.next_id += 1;
        let track = self.tracks.iter_mut().find(|t| t.regions.iter().any(|r| r.id == id))?;
        track.regions.push(right);
        Some(new_id)
    }

    /// Buses that `bus` feeds directly, through its output or its sends.
    fn bus_targets(&self, bus: &Bus) -> Vec<Id> {
        let mut v: Vec<Id> = bus.channel.sends.iter().map(|s| s.bus).collect();
        if let Output::Bus(b) = bus.channel.output {
            v.push(b);
        }
        v
    }

    /// Bus indices ordered so every bus comes after the buses feeding it, or `None` if the
    /// routing contains a feedback loop.
    pub fn bus_order(&self) -> Option<Vec<usize>> {
        let n = self.buses.len();
        let index = |id: Id| self.buses.iter().position(|b| b.id == id);
        let mut incoming = vec![0usize; n];
        let edges: Vec<Vec<usize>> = self
            .buses
            .iter()
            .map(|b| self.bus_targets(b).into_iter().filter_map(index).collect())
            .collect();
        for e in edges.iter().flatten() {
            incoming[*e] += 1;
        }
        let mut ready: Vec<usize> = (0..n).filter(|i| incoming[*i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = ready.pop() {
            order.push(i);
            for &j in &edges[i] {
                incoming[j] -= 1;
                if incoming[j] == 0 {
                    ready.push(j);
                }
            }
        }
        (order.len() == n).then_some(order)
    }

    /// Whether routing `from` (a track or bus) into bus `to` would create a feedback loop.
    pub fn would_loop(&self, from: Id, to: Id) -> bool {
        if self.track(from).is_some() {
            return false;
        }
        let mut probe = self.clone();
        if let Some(b) = probe.buses.iter_mut().find(|b| b.id == from) {
            b.channel.sends.push(Send { bus: to, level_db: 0.0, mode: SendMode::PostPan });
        }
        probe.bus_order().is_none()
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.channel.solo) || self.buses.iter().any(|b| b.channel.solo)
    }

    pub fn length(&self) -> u64 {
        self.tracks.iter().flat_map(|t| t.regions.iter()).map(|r| r.end()).max().unwrap_or(0)
    }
}

/// A project on disk is a `Name.solstice` folder holding `project.json` and `Audio Files/`.
pub struct Bundle {
    pub dir: PathBuf,
}

impl Bundle {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Bundle { dir: dir.into() }
    }

    pub fn audio_dir(&self) -> PathBuf {
        self.dir.join("Audio Files")
    }

    pub fn audio_path(&self, file: &str) -> PathBuf {
        self.audio_dir().join(file)
    }

    pub fn save(&self, project: &Project) -> std::io::Result<()> {
        std::fs::create_dir_all(self.audio_dir())?;
        let json = serde_json::to_vec_pretty(project)?;
        let tmp = self.dir.join("project.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, self.dir.join("project.json"))
    }

    pub fn load(&self) -> std::io::Result<Project> {
        let data = std::fs::read(self.dir.join("project.json"))?;
        Ok(serde_json::from_slice(&data)?)
    }

    pub fn is_bundle(dir: &Path) -> bool {
        dir.join("project.json").is_file()
    }
}

mod b64 {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -96.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio_region(p: &mut Project, start: u64, length: u64) -> Id {
        let id = p.alloc_id();
        let t = p.add_track(TrackKind::Audio);
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "r".into(),
            start,
            length,
            muted: false,
            content: Content::Audio(AudioContent {
                file: "a.wav".into(),
                offset: 10,
                gain_db: 0.0,
                fade_in: 5,
                fade_out: 5,
            }),
        });
        id
    }

    #[test]
    fn round_trip() {
        let mut p = Project::new("Test", 48000);
        audio_region(&mut p, 100, 1000);
        p.master.inserts.push(PluginSlot {
            id: 99,
            uid: "00".repeat(16),
            path: "/x.vst3".into(),
            name: "X".into(),
            bypass: false,
            state: vec![1, 2, 3, 255],
        });
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn split_audio() {
        let mut p = Project::new("Test", 48000);
        let id = audio_region(&mut p, 100, 1000);
        let right = p.split_region(id, 600).unwrap();
        let (_, l) = p.find_region(id).unwrap();
        assert_eq!((l.start, l.length), (100, 500));
        let (_, r) = p.find_region(right).unwrap();
        assert_eq!((r.start, r.length), (600, 500));
        let Content::Audio(a) = &r.content else { panic!() };
        assert_eq!((a.offset, a.fade_in), (510, 0));
        assert!(p.split_region(id, 100).is_none());
    }

    #[test]
    fn bus_order_and_loops() {
        let mut p = Project::new("Test", 48000);
        let a = p.add_bus();
        let b = p.add_bus();
        let c = p.add_bus();
        p.buses[0].channel.output = Output::Bus(c);
        p.buses[1].channel.sends.push(Send { bus: a, level_db: 0.0, mode: SendMode::PostPan });
        let order = p.bus_order().unwrap();
        let pos = |i: usize| order.iter().position(|x| *x == i).unwrap();
        assert!(pos(1) < pos(0) && pos(0) < pos(2));
        assert!(p.would_loop(c, b));
        assert!(!p.would_loop(a, c));
        let t = p.add_track(TrackKind::Audio);
        assert!(!p.would_loop(t, a));
    }

    #[test]
    fn split_midi() {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        let beat = p.time().ticks_to_samples(PPQ as u64);
        let notes = vec![
            Note { start: 0, length: PPQ * 2, pitch: 60, velocity: 100 },
            Note { start: PPQ * 2, length: PPQ, pitch: 62, velocity: 100 },
        ];
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "m".into(),
            start: 0,
            length: beat * 4,
            muted: false,
            content: Content::Midi(MidiContent { notes }),
        });
        let right = p.split_region(id, beat).unwrap();
        let (_, l) = p.find_region(id).unwrap();
        let Content::Midi(lm) = &l.content else { panic!() };
        assert_eq!(lm.notes, vec![Note { start: 0, length: PPQ, pitch: 60, velocity: 100 }]);
        let (_, r) = p.find_region(right).unwrap();
        let Content::Midi(rm) = &r.content else { panic!() };
        assert_eq!(rm.notes, vec![Note { start: PPQ, length: PPQ, pitch: 62, velocity: 100 }]);
    }
}
