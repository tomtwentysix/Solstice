mod history;
pub mod midi_tools;
mod time;

pub use history::History;
pub use time::{BarLine, PPQ, SigChange, TempoChange, TimeMap};

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
    /// Dim: drops the stereo output by 20 dB without moving its fader.
    #[serde(default)]
    pub dim: bool,
    /// Tempo changes after the opening `tempo`.
    #[serde(default)]
    pub tempo_changes: Vec<TempoChange>,
    /// Time signature changes after the opening one.
    #[serde(default)]
    pub sig_changes: Vec<SigChange>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// Arrangement markers: named sections (Intro, Verse...) that move as blocks.
    #[serde(default)]
    pub sections: Vec<Section>,
    #[serde(default)]
    pub key_changes: Vec<KeyChange>,
    #[serde(default)]
    pub stacks: Vec<Stack>,
    pub next_id: Id,
}

/// A Track Stack: a folder of adjacent tracks that can be collapsed. A summing stack
/// also routes its tracks into an aux, which acts as the stack's channel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Stack {
    pub id: Id,
    pub name: String,
    pub bus: Option<Id>,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub id: Id,
    pub pos: u64,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Section {
    pub id: Id,
    pub start: u64,
    pub length: u64,
    pub name: String,
}

impl Section {
    pub fn end(&self) -> u64 {
        self.start + self.length
    }
}

/// The key from a bar onwards, such as "C major" or "A minor".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KeyChange {
    pub bar: u64,
    pub key: String,
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
    /// MIDI effects ahead of the instrument, in order.
    #[serde(default)]
    pub midi_fx: Vec<PluginSlot>,
    pub regions: Vec<Region>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bus {
    pub id: Id,
    pub name: String,
    pub channel: Channel,
    /// A VCA fader: carries no audio; its fader and mute scale the channels assigned to it.
    #[serde(default)]
    pub vca: bool,
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
    #[serde(default)]
    pub automation: Vec<AutomationLane>,
    #[serde(default)]
    pub auto_mode: AutoMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vca: Option<Id>,
}

/// Automation modes. Read plays lanes back; Touch writes while a control is held;
/// Latch keeps writing from the first touch until the transport stops; Write overwrites
/// everything played over.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum AutoMode {
    Off,
    #[default]
    Read,
    Touch,
    Latch,
    Write,
}

impl AutoMode {
    pub const ALL: [AutoMode; 5] = [AutoMode::Off, AutoMode::Read, AutoMode::Touch, AutoMode::Latch, AutoMode::Write];

    pub fn label(self) -> &'static str {
        match self {
            AutoMode::Off => "Off",
            AutoMode::Read => "Read",
            AutoMode::Touch => "Touch",
            AutoMode::Latch => "Latch",
            AutoMode::Write => "Write",
        }
    }
}

/// What a lane controls. Values are in the control's own units: dB for volume and sends,
/// -1..1 for pan, 0 or 1 for mute, 0..1 for plug-in parameters.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AutoTarget {
    Volume,
    Pan,
    Mute,
    Send(usize),
    Plugin { slot: Id, param: u32 },
}

impl AutoTarget {
    pub fn range(self) -> (f32, f32) {
        match self {
            AutoTarget::Volume => (-60.0, 6.0),
            AutoTarget::Send(_) => (-60.0, 6.0),
            AutoTarget::Pan => (-1.0, 1.0),
            AutoTarget::Mute | AutoTarget::Plugin { .. } => (0.0, 1.0),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutoPoint {
    pub pos: u64,
    pub value: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AutomationLane {
    pub target: AutoTarget,
    /// Sorted by position.
    pub points: Vec<AutoPoint>,
}

impl AutomationLane {
    /// The lane's value at `pos`: linear between points, held before the first and after the
    /// last. Mute steps rather than ramps.
    pub fn value_at(&self, pos: u64) -> Option<f32> {
        let pts = &self.points;
        let first = pts.first()?;
        if pos <= first.pos {
            return Some(first.value);
        }
        let i = pts.partition_point(|p| p.pos <= pos);
        let a = pts[i - 1];
        let Some(b) = pts.get(i) else { return Some(a.value) };
        if self.target == AutoTarget::Mute {
            return Some(a.value);
        }
        let t = (pos - a.pos) as f32 / (b.pos - a.pos).max(1) as f32;
        Some(a.value + (b.value - a.value) * t)
    }

    /// Replaces points in `from..=to` with `points` (already within the range).
    pub fn write(&mut self, from: u64, to: u64, points: &[AutoPoint]) {
        self.points.retain(|p| p.pos < from || p.pos > to);
        self.points.extend_from_slice(points);
        self.points.sort_by_key(|p| p.pos);
    }
}

impl Channel {
    pub fn lane(&self, target: AutoTarget) -> Option<&AutomationLane> {
        self.automation.iter().find(|l| l.target == target)
    }

    pub fn lane_mut(&mut self, target: AutoTarget) -> &mut AutomationLane {
        if let Some(i) = self.automation.iter().position(|l| l.target == target) {
            return &mut self.automation[i];
        }
        self.automation.push(AutomationLane { target, points: Vec::new() });
        self.automation.last_mut().unwrap()
    }
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
            automation: Vec::new(),
            auto_mode: AutoMode::Read,
            vca: None,
        }
    }
}

/// The built-in channel EQ: low cut, low shelf, four bells, high shelf,
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
    /// The track whose signal keys this plug-in's detector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidechain: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Region {
    pub id: Id,
    pub name: String,
    pub start: u64,
    pub length: u64,
    pub muted: bool,
    /// Extra repeats after the region itself, as when the loop handle is dragged out.
    #[serde(default)]
    pub loops: u32,
    pub content: Content,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start + self.length
    }

    /// Where the region and its loop repeats end.
    pub fn span_end(&self) -> u64 {
        self.start + self.length * (self.loops as u64 + 1)
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
    /// A take folder: every take recorded over this region. `file` and `offset` mirror the
    /// take the comp starts with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub takes: Vec<Take>,
    /// Which take plays where, as sections from the region start; the first starts at 0.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comp: Vec<CompSection>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Take {
    pub name: String,
    pub file: String,
    /// Frame in the file at the region start.
    pub offset: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompSection {
    pub start: u64,
    pub take: usize,
}

/// A stretch of a region that plays one file: start and end relative to the region start.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece<'a> {
    pub start: u64,
    pub end: u64,
    pub file: &'a str,
    pub offset: u64,
}

impl AudioContent {
    pub fn new(file: impl Into<String>) -> Self {
        AudioContent { file: file.into(), offset: 0, gain_db: 0.0, fade_in: 0, fade_out: 0, takes: Vec::new(), comp: Vec::new() }
    }

    pub fn is_folder(&self) -> bool {
        !self.takes.is_empty()
    }

    /// What plays over a region of `length`: the comp's sections, or the one file.
    pub fn pieces(&self, length: u64) -> Vec<Piece<'_>> {
        if !self.is_folder() || self.comp.is_empty() {
            return vec![Piece { start: 0, end: length, file: &self.file, offset: self.offset }];
        }
        let mut out = Vec::new();
        for (i, c) in self.comp.iter().enumerate() {
            let end = self.comp.get(i + 1).map_or(length, |n| n.start).min(length);
            if let Some(t) = self.takes.get(c.take)
                && end > c.start
            {
                out.push(Piece { start: c.start, end, file: &t.file, offset: t.offset + c.start });
            }
        }
        out
    }

    /// Swipe comping: `take` plays over `from..to`.
    pub fn set_comp(&mut self, from: u64, to: u64, take: usize, length: u64) {
        if take >= self.takes.len() || from >= to {
            return;
        }
        let at = |comp: &[CompSection], pos: u64| comp.iter().rev().find(|c| c.start <= pos).map_or(0, |c| c.take);
        let mut cuts: Vec<u64> = self.comp.iter().map(|c| c.start).chain([0, from, to.min(length)]).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let old = self.comp.clone();
        let mut comp: Vec<CompSection> = Vec::new();
        for c in cuts.into_iter().filter(|c| *c < length) {
            let t = if c >= from && c < to { take } else { at(&old, c) };
            if comp.last().is_none_or(|l| l.take != t) {
                comp.push(CompSection { start: c, take: t });
            }
        }
        self.comp = comp;
        self.sync_mirror();
    }

    /// Keeps `file` and `offset` on the take the comp starts with.
    fn sync_mirror(&mut self) {
        if let Some(t) = self.comp.first().and_then(|c| self.takes.get(c.take)) {
            self.file = t.file.clone();
            self.offset = t.offset;
        }
    }

    /// The region start moved by `delta` frames (trimming or splitting): every take moves
    /// with it and the comp keeps its place on the timeline.
    pub fn shift_start(&mut self, delta: i64) {
        self.offset = (self.offset as i64 + delta).max(0) as u64;
        if !self.is_folder() {
            return;
        }
        for t in &mut self.takes {
            t.offset = (t.offset as i64 + delta).max(0) as u64;
        }
        let first = self.comp.iter().rev().find(|c| c.start as i64 <= delta.max(0)).map_or(0, |c| c.take);
        let mut comp = vec![CompSection { start: 0, take: first }];
        for c in &self.comp {
            let s = c.start as i64 - delta;
            if s > 0 && comp.last().is_some_and(|l| l.take != c.take) {
                comp.push(CompSection { start: s as u64, take: c.take });
            }
        }
        self.comp = comp;
        self.sync_mirror();
    }

    /// Adds a take, which becomes the one that plays, as for a new pass.
    pub fn add_take(&mut self, take: Take) {
        if !self.is_folder() {
            let first = Take { name: "Take 1".into(), file: self.file.clone(), offset: self.offset };
            self.takes.push(first);
        }
        self.takes.push(take);
        self.comp = vec![CompSection { start: 0, take: self.takes.len() - 1 }];
        self.sync_mirror();
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiContent {
    pub notes: Vec<Note>,
    /// Controller and pitch-bend data, sorted by tick.
    #[serde(default)]
    pub controllers: Vec<Controller>,
}

/// `PITCH_BEND` in `number` marks pitch bend, whose value spans 0–16383 with 8192 centred;
/// otherwise `number` is a MIDI CC and `value` is 0–127.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Controller {
    pub tick: u32,
    pub number: u8,
    pub value: u16,
}

pub const PITCH_BEND: u8 = 255;

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
            tempo_changes: Vec::new(),
            sig_changes: Vec::new(),
            markers: Vec::new(),
            sections: Vec::new(),
            key_changes: Vec::new(),
            stacks: Vec::new(),
            next_id: 1,
        }
    }

    pub fn time(&self) -> TimeMap {
        TimeMap::with_changes(self.sample_rate, self.tempo, self.sig_num, self.sig_den, &self.tempo_changes, &self.sig_changes)
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
            midi_fx: Vec::new(),
            regions: Vec::new(),
            stack: None,
        });
        id
    }

    /// Duplicate Track: a copy of a track below it, with its plug-ins, regions and automation.
    /// Returns the new track's id.
    pub fn duplicate_track(&mut self, id: Id) -> Option<Id> {
        let i = self.tracks.iter().position(|t| t.id == id)?;
        let mut t = self.tracks[i].clone();
        t.id = self.alloc_id();
        t.armed = false;
        let mut slots = std::collections::HashMap::new();
        for s in t.channel.inserts.iter_mut().chain(t.instrument.iter_mut()).chain(t.midi_fx.iter_mut()) {
            let new = self.alloc_id();
            slots.insert(s.id, new);
            s.id = new;
        }
        for r in &mut t.regions {
            r.id = self.alloc_id();
        }
        for lane in &mut t.channel.automation {
            if let AutoTarget::Plugin { slot, .. } = &mut lane.target
                && let Some(new) = slots.get(slot)
            {
                *slot = *new;
            }
        }
        let new = t.id;
        self.tracks.insert(i + 1, t);
        Some(new)
    }

    /// Puts tracks in a new stack, gathered under the first of them. A summing stack gets an
    /// aux that their outputs feed. Returns the stack's id.
    pub fn create_stack(&mut self, ids: &[Id], summing: bool) -> Option<Id> {
        let mut members: Vec<usize> = ids.iter().filter_map(|id| self.tracks.iter().position(|t| t.id == *id)).collect();
        members.sort_unstable();
        members.dedup();
        let first = *members.first()?;
        // Members already in another stack leave it.
        let taken: Vec<Track> = members.iter().rev().map(|&i| self.tracks.remove(i)).collect();
        let at = first.min(self.tracks.len());
        let id = self.alloc_id();
        let name = format!("Stack {}", self.stacks.len() + 1);
        let bus = summing.then(|| {
            let b = self.add_bus();
            self.buses.last_mut().unwrap().name = name.clone();
            b
        });
        for (k, mut t) in taken.into_iter().rev().enumerate() {
            t.stack = Some(id);
            if let Some(b) = bus {
                t.channel.output = Output::Bus(b);
            }
            self.tracks.insert(at + k, t);
        }
        self.stacks.push(Stack { id, name, bus, collapsed: false });
        self.tidy_stacks();
        Some(id)
    }

    /// Takes the tracks out of a stack. A summing stack's tracks go back to where its aux
    /// went, and the aux is removed.
    pub fn ungroup_stack(&mut self, id: Id) {
        let Some(i) = self.stacks.iter().position(|s| s.id == id) else { return };
        let stack = self.stacks.remove(i);
        let out = stack.bus.and_then(|b| self.buses.iter().find(|x| x.id == b)).map_or(Output::Master, |b| b.channel.output);
        for t in self.tracks.iter_mut().filter(|t| t.stack == Some(id)) {
            t.stack = None;
            if stack.bus.is_some_and(|b| t.channel.output == Output::Bus(b)) {
                t.channel.output = out;
            }
        }
        if let Some(b) = stack.bus
            && !self.channels().any(|c| c.output == Output::Bus(b) || c.sends.iter().any(|s| s.bus == b))
        {
            self.buses.retain(|x| x.id != b);
        }
    }

    fn channels(&self) -> impl Iterator<Item = &Channel> {
        self.tracks.iter().map(|t| &t.channel).chain(self.buses.iter().map(|b| &b.channel))
    }

    /// Drops stacks with no tracks left, as after deleting them.
    pub fn tidy_stacks(&mut self) {
        let used: Vec<Id> = self.tracks.iter().filter_map(|t| t.stack).collect();
        self.stacks.retain(|s| used.contains(&s.id));
    }

    pub fn stack(&self, id: Id) -> Option<&Stack> {
        self.stacks.iter().find(|s| s.id == id)
    }

    /// Moves a track up (negative) or down the list.
    pub fn move_track(&mut self, id: Id, by: isize) {
        let Some(i) = self.tracks.iter().position(|t| t.id == id) else { return };
        let to = (i as isize + by).clamp(0, self.tracks.len() as isize - 1) as usize;
        let t = self.tracks.remove(i);
        self.tracks.insert(to, t);
    }

    pub fn add_bus(&mut self) -> Id {
        let id = self.alloc_id();
        let name = format!("Bus {}", self.buses.len() + 1);
        self.buses.push(Bus { id, name, channel: Channel::default(), vca: false });
        id
    }

    pub fn add_vca(&mut self) -> Id {
        let id = self.alloc_id();
        let name = format!("VCA {}", self.buses.iter().filter(|b| b.vca).count() + 1);
        self.buses.push(Bus { id, name, channel: Channel::default(), vca: true });
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
                r.shift_start(cut as i64);
                r.fade_in = 0;
                l.fade_out = 0;
                l.comp.retain(|c| c.start < cut);
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
                l.controllers.retain(|c| c.tick < cut_ticks);
                r.controllers.retain(|c| c.tick >= cut_ticks);
                for c in &mut r.controllers {
                    c.tick -= cut_ticks;
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

    /// Repeat: copies regions to start at the end of the selection, keeping their
    /// tracks and relative positions. Returns the new regions' ids.
    pub fn repeat_regions(&mut self, ids: &[Id]) -> Vec<Id> {
        let picked: Vec<(usize, Region)> = ids.iter().filter_map(|id| self.find_region(*id).map(|(t, r)| (t, r.clone()))).collect();
        let (Some(first), Some(last)) = (picked.iter().map(|(_, r)| r.start).min(), picked.iter().map(|(_, r)| r.span_end()).max())
        else {
            return Vec::new();
        };
        self.paste_regions(&picked, last - first, 0)
    }

    /// Adds copies of `regions` (track index, region) shifted by `shift` samples and `rows`
    /// tracks, skipping any that would land on a missing or different-kind track.
    pub fn paste_regions(&mut self, regions: &[(usize, Region)], shift: u64, rows: i64) -> Vec<Id> {
        let mut out = Vec::new();
        for (ti, r) in regions {
            let to = *ti as i64 + rows;
            let Some(t) = usize::try_from(to).ok().filter(|t| *t < self.tracks.len()) else { continue };
            let kind_ok = matches!(
                (&r.content, self.tracks[t].kind),
                (Content::Audio(_), TrackKind::Audio) | (Content::Midi(_), TrackKind::Instrument)
            );
            if !kind_ok {
                continue;
            }
            let id = self.alloc_id();
            let mut copy = r.clone();
            copy.id = id;
            copy.start += shift;
            self.tracks[t].regions.push(copy);
            out.push(id);
        }
        out
    }

    /// Moves regions by `delta` samples, stopping at zero.
    pub fn nudge_regions(&mut self, ids: &[Id], delta: i64) {
        for t in &mut self.tracks {
            for r in t.regions.iter_mut().filter(|r| ids.contains(&r.id)) {
                r.start = (r.start as i64 + delta).max(0) as u64;
            }
        }
    }

    /// Join: joins regions on one track into the first of them. MIDI regions
    /// merge their notes; audio regions only join when they are consecutive pieces of the same
    /// file, since anything else would need a bounce.
    pub fn join_regions(&mut self, ids: &[Id]) -> Result<Id, &'static str> {
        let mut picked: Vec<(usize, Region)> = ids.iter().filter_map(|id| self.find_region(*id).map(|(t, r)| (t, r.clone()))).collect();
        if picked.len() < 2 {
            return Err("Select at least two regions to join");
        }
        let track = picked[0].0;
        if picked.iter().any(|(t, _)| *t != track) {
            return Err("Regions must be on the same track to join");
        }
        picked.sort_by_key(|(_, r)| r.start);
        let regions: Vec<Region> = picked.into_iter().map(|(_, r)| r).collect();
        let first = &regions[0];
        let start = first.start;
        let end = regions.iter().map(|r| r.end()).max().unwrap();
        let joined = match &first.content {
            Content::Midi(_) => {
                let time = self.time();
                let mut notes = Vec::new();
                let mut controllers = Vec::new();
                for r in &regions {
                    let Content::Midi(m) = &r.content else { return Err("Can't join audio and MIDI regions") };
                    let shift = time.samples_to_ticks(r.start - start) as u32;
                    notes.extend(m.notes.iter().map(|n| Note { start: n.start + shift, ..*n }));
                    controllers.extend(m.controllers.iter().map(|c| Controller { tick: c.tick + shift, ..*c }));
                }
                notes.sort_by_key(|n| (n.start, n.pitch));
                controllers.sort_by_key(|c| c.tick);
                Content::Midi(MidiContent { notes, controllers })
            }
            Content::Audio(a) => {
                for w in regions.windows(2) {
                    let (Content::Audio(x), Content::Audio(y)) = (&w[0].content, &w[1].content) else {
                        return Err("Can't join audio and MIDI regions");
                    };
                    let contiguous = !x.is_folder()
                        && !y.is_folder()
                        && x.file == y.file
                        && w[1].start == w[0].end()
                        && y.offset == x.offset + w[0].length;
                    if !contiguous {
                        return Err("Only neighbouring pieces of the same recording can be joined");
                    }
                }
                let last = match &regions.last().unwrap().content {
                    Content::Audio(l) => l.fade_out,
                    _ => 0,
                };
                Content::Audio(AudioContent { fade_out: last, ..a.clone() })
            }
        };
        let keep = first.id;
        let rest: Vec<Id> = regions[1..].iter().map(|r| r.id).collect();
        self.tracks[track].regions.retain(|r| !rest.contains(&r.id));
        let r = self.region_mut(keep).unwrap();
        r.length = end - start;
        r.loops = 0;
        r.content = joined;
        Ok(keep)
    }

    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.channel.solo) || self.buses.iter().any(|b| b.channel.solo)
    }

    pub fn length(&self) -> u64 {
        self.tracks.iter().flat_map(|t| t.regions.iter()).map(|r| r.span_end()).max().unwrap_or(0)
    }

    /// Splits every region that spans `pos`, so edits can treat each side on its own.
    fn split_all_at(&mut self, pos: u64) {
        let ids: Vec<Id> =
            self.tracks.iter().flat_map(|t| t.regions.iter()).filter(|r| r.start < pos && r.end() > pos && r.loops == 0).map(|r| r.id).collect();
        for id in ids {
            self.split_region(id, pos);
        }
    }

    fn automation_points(&mut self) -> impl Iterator<Item = &mut Vec<AutoPoint>> {
        let chans = std::iter::once(&mut self.master)
            .chain(self.tracks.iter_mut().map(|t| &mut t.channel))
            .chain(self.buses.iter_mut().map(|b| &mut b.channel));
        chans.flat_map(|c| c.automation.iter_mut().map(|l| &mut l.points))
    }

    /// Opens a gap of `len` at `at`: everything from there on moves later.
    pub fn insert_time(&mut self, at: u64, len: u64) {
        self.split_all_at(at);
        let shift = |p: &mut u64| {
            if *p >= at {
                *p += len;
            }
        };
        for r in self.tracks.iter_mut().flat_map(|t| t.regions.iter_mut()) {
            shift(&mut r.start);
        }
        for pts in self.automation_points() {
            pts.iter_mut().for_each(|p| shift(&mut p.pos));
        }
        self.markers.iter_mut().for_each(|m| shift(&mut m.pos));
        for s in &mut self.sections {
            if s.start >= at {
                s.start += len;
            } else if s.end() > at {
                s.length += len;
            }
        }
    }

    /// Removes `start..end` and closes the gap.
    pub fn cut_time(&mut self, start: u64, end: u64) {
        let len = end - start;
        self.split_all_at(start);
        self.split_all_at(end);
        for t in &mut self.tracks {
            t.regions.retain(|r| r.start < start || r.start >= end);
            for r in t.regions.iter_mut().filter(|r| r.start >= end) {
                r.start -= len;
            }
        }
        for pts in self.automation_points() {
            pts.retain(|p| p.pos < start || p.pos >= end);
            pts.iter_mut().filter(|p| p.pos >= end).for_each(|p| p.pos -= len);
        }
        self.markers.retain(|m| m.pos < start || m.pos >= end);
        self.markers.iter_mut().filter(|m| m.pos >= end).for_each(|m| m.pos -= len);
        self.sections.retain(|s| s.start < start || s.start >= end);
        for s in &mut self.sections {
            if s.start >= end {
                s.start -= len;
            } else if s.end() > start {
                s.length -= s.end().min(end) - start;
            }
        }
    }

    /// Arrangement Duplicate: a copy of the section and everything in it goes right
    /// after it, and the rest of the song moves later to make room.
    pub fn duplicate_section(&mut self, id: Id) -> Option<Id> {
        let sec = self.sections.iter().find(|s| s.id == id)?.clone();
        self.insert_time(sec.end(), sec.length);
        self.split_all_at(sec.start);
        let copies: Vec<(usize, Region)> = self
            .tracks
            .iter()
            .enumerate()
            .flat_map(|(i, t)| t.regions.iter().filter(|r| r.start >= sec.start && r.start < sec.end()).map(move |r| (i, r.clone())))
            .collect();
        for (i, mut r) in copies {
            r.id = self.alloc_id();
            r.start += sec.length;
            self.tracks[i].regions.push(r);
        }
        for pts in self.automation_points() {
            let extra: Vec<AutoPoint> =
                pts.iter().filter(|p| p.pos >= sec.start && p.pos < sec.end()).map(|p| AutoPoint { pos: p.pos + sec.length, ..*p }).collect();
            pts.extend(extra);
            pts.sort_by_key(|p| p.pos);
        }
        let new = self.alloc_id();
        self.sections.push(Section { id: new, start: sec.end(), length: sec.length, name: sec.name });
        self.sections.sort_by_key(|s| s.start);
        Some(new)
    }

    /// Deletes a section with everything in it and closes the gap.
    pub fn delete_section(&mut self, id: Id) {
        if let Some(sec) = self.sections.iter().find(|s| s.id == id).cloned() {
            self.cut_time(sec.start, sec.end());
            self.sections.retain(|s| s.id != id);
        }
    }

    /// After the tempo map changes from `old`, keeps regions, cycle and punch at the same bar
    /// positions. MIDI regions keep their musical length too; audio plays at
    /// its own speed, so its length stays.
    pub fn retime(&mut self, old: &TimeMap) {
        let new = self.time();
        let moved = |pos: u64| new.tick_to_sample_f(old.sample_to_tick_f(pos as f64)).round() as u64;
        for t in &mut self.tracks {
            for r in &mut t.regions {
                let start = moved(r.start);
                if matches!(r.content, Content::Midi(_)) {
                    r.length = moved(r.start + r.length).saturating_sub(start).max(1);
                }
                r.start = start;
            }
        }
        for range in [&mut self.cycle, &mut self.punch] {
            *range = Range::new(moved(range.start), moved(range.end));
        }
        // Markers, sections and automation are stored in samples too, so they keep their bars.
        for m in &mut self.markers {
            m.pos = moved(m.pos);
        }
        for sec in &mut self.sections {
            let start = moved(sec.start);
            sec.length = moved(sec.start + sec.length).saturating_sub(start).max(1);
            sec.start = start;
        }
        let channels = self.tracks.iter_mut().map(|t| &mut t.channel).chain(self.buses.iter_mut().map(|b| &mut b.channel)).chain([&mut self.master]);
        for ch in channels {
            for lane in &mut ch.automation {
                for pt in &mut lane.points {
                    pt.pos = moved(pt.pos);
                }
            }
        }
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
        std::fs::rename(tmp, self.dir.join("project.json"))?;
        self.clear_autosave();
        Ok(())
    }

    pub fn load(&self) -> std::io::Result<Project> {
        let data = std::fs::read(self.dir.join("project.json"))?;
        Ok(serde_json::from_slice(&data)?)
    }

    fn autosave_path(&self) -> PathBuf {
        self.dir.join("autosave.json")
    }

    /// Writes a crash-recovery copy beside `project.json`, leaving the saved project untouched.
    pub fn autosave(&self, project: &Project) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("autosave.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(project)?)?;
        std::fs::rename(tmp, self.autosave_path())
    }

    /// The autosave, if it is newer than the last real save (or there has been none).
    pub fn recoverable(&self) -> Option<Project> {
        let auto = std::fs::metadata(self.autosave_path()).and_then(|m| m.modified()).ok()?;
        let saved = std::fs::metadata(self.dir.join("project.json")).and_then(|m| m.modified()).ok();
        if saved.is_some_and(|s| s >= auto) {
            return None;
        }
        serde_json::from_slice(&std::fs::read(self.autosave_path()).ok()?).ok()
    }

    pub fn clear_autosave(&self) {
        let _ = std::fs::remove_file(self.autosave_path());
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
    #[test]
    fn stacks_gather_route_and_ungroup() {
        let mut p = Project::new("T", 48000);
        let a = p.add_track(TrackKind::Audio);
        let b = p.add_track(TrackKind::Audio);
        let c = p.add_track(TrackKind::Audio);
        let s = p.create_stack(&[a, c], true).unwrap();
        let order: Vec<Id> = p.tracks.iter().map(|t| t.id).collect();
        assert_eq!(order, vec![a, c, b]);
        let bus = p.stack(s).unwrap().bus.unwrap();
        assert_eq!(p.track(c).unwrap().channel.output, Output::Bus(bus));
        assert_eq!(p.track(b).unwrap().stack, None);
        p.ungroup_stack(s);
        assert!(p.stacks.is_empty());
        assert!(p.buses.is_empty());
        assert_eq!(p.track(a).unwrap().channel.output, Output::Master);
        assert_eq!(p.track(a).unwrap().stack, None);
    }

    #[test]
    fn duplicate_track_gets_fresh_ids() {
        let mut p = Project::new("T", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let slot = PluginSlot { id: p.alloc_id(), uid: "x".into(), path: Default::default(), name: "X".into(), bypass: false, state: vec![1], sidechain: None };
        let sid = slot.id;
        p.track_mut(t).unwrap().instrument = Some(slot);
        p.track_mut(t).unwrap().channel.automation.push(AutomationLane { target: AutoTarget::Plugin { slot: sid, param: 0 }, points: vec![] });
        let copy = p.duplicate_track(t).unwrap();
        let c = p.track(copy).unwrap();
        let new_sid = c.instrument.as_ref().unwrap().id;
        assert_ne!(new_sid, sid);
        assert_eq!(c.instrument.as_ref().unwrap().state, vec![1]);
        assert_eq!(c.channel.automation[0].target, AutoTarget::Plugin { slot: new_sid, param: 0 });
        assert_eq!(p.tracks[1].id, copy);
        p.move_track(copy, -1);
        assert_eq!(p.tracks[0].id, copy);
    }

    #[test]
    fn retime_keeps_markers_sections_and_automation_on_their_bars() {
        let mut p = Project::new("T", 48000);
        let bar = p.time().bar_len();
        p.markers.push(Marker { id: 1, pos: bar * 2, name: "M".into() });
        p.sections.push(Section { id: 2, start: bar, length: bar * 2, name: "S".into() });
        p.master.automation.push(AutomationLane { target: AutoTarget::Volume, points: vec![AutoPoint { pos: bar * 3, value: 0.0 }] });
        let old = p.time();
        p.tempo = 60.0;
        p.retime(&old);
        let bar = p.time().bar_len();
        assert_eq!(p.markers[0].pos, bar * 2);
        assert_eq!((p.sections[0].start, p.sections[0].length), (bar, bar * 2));
        assert_eq!(p.master.automation[0].points[0].pos, bar * 3);
    }

    use super::*;

    fn folder() -> AudioContent {
        let mut a = AudioContent::new("t1.wav");
        a.add_take(Take { name: "Take 2".into(), file: "t2.wav".into(), offset: 0 });
        a.add_take(Take { name: "Take 3".into(), file: "t3.wav".into(), offset: 0 });
        a
    }

    #[test]
    fn swipe_comping() {
        let mut a = folder();
        assert_eq!(a.pieces(1000).iter().map(|p| p.file).collect::<Vec<_>>(), vec!["t3.wav"]);
        a.set_comp(200, 500, 0, 1000);
        a.set_comp(400, 700, 1, 1000);
        let got: Vec<(u64, u64, &str, u64)> = a.pieces(1000).iter().map(|p| (p.start, p.end, p.file, p.offset)).collect();
        assert_eq!(got, vec![(0, 200, "t3.wav", 0), (200, 400, "t1.wav", 200), (400, 700, "t2.wav", 400), (700, 1000, "t3.wav", 700)]);
        // Choosing a whole take merges everything back into one section.
        a.set_comp(0, 1000, 2, 1000);
        assert_eq!(a.comp, vec![CompSection { start: 0, take: 2 }]);
    }

    #[test]
    fn splitting_a_folder_keeps_the_comp_in_place() {
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Audio);
        let id = p.alloc_id();
        let mut a = folder();
        a.set_comp(300, 1000, 0, 1000);
        p.track_mut(t).unwrap().regions.push(Region { id, name: "f".into(), start: 0, length: 1000, muted: false, loops: 0, content: Content::Audio(a) });
        let right = p.split_region(id, 500).unwrap();
        let Content::Audio(r) = &p.region_mut(right).unwrap().content else { panic!() };
        let got: Vec<(u64, u64, &str, u64)> = r.pieces(500).iter().map(|p| (p.start, p.end, p.file, p.offset)).collect();
        assert_eq!(got, vec![(0, 500, "t1.wav", 500)]);
        let Content::Audio(l) = &p.region_mut(id).unwrap().content else { panic!() };
        assert_eq!(l.pieces(500).len(), 2);
    }

    #[test]
    fn arrangement_duplicate_and_delete() {
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Audio);
        for (start, len) in [(0, 100), (100, 100), (150, 100)] {
            let id = p.alloc_id();
            p.track_mut(t).unwrap().regions.push(Region {
                id,
                name: "r".into(),
                start,
                length: len,
                muted: false,
                loops: 0,
                content: Content::Audio(AudioContent::new("a.wav")),
            });
        }
        let verse = p.alloc_id();
        p.sections.push(Section { id: verse, start: 100, length: 100, name: "Verse".into() });
        p.markers.push(Marker { id: 99, pos: 250, name: "Solo".into() });
        p.duplicate_section(verse).unwrap();
        let mut starts: Vec<(u64, u64)> = p.tracks[0].regions.iter().map(|r| (r.start, r.length)).collect();
        starts.sort();
        // The region straddling the section end is split; its tail moves with the song.
        assert_eq!(starts, vec![(0, 100), (100, 100), (150, 50), (200, 100), (250, 50), (300, 50)]);
        assert_eq!(p.markers[0].pos, 350);
        assert_eq!(p.sections.len(), 2);
        p.delete_section(verse);
        let mut starts: Vec<(u64, u64)> = p.tracks[0].regions.iter().map(|r| (r.start, r.length)).collect();
        starts.sort();
        assert_eq!(starts, vec![(0, 100), (100, 100), (150, 50), (200, 50)]);
        assert_eq!(p.sections[0].start, 100);
    }

    #[test]
    fn retime_keeps_bar_positions() {
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        let bar = p.time().bar_len();
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "m".into(),
            start: 2 * bar,
            length: bar,
            muted: false,
            loops: 0,
            content: Content::Midi(MidiContent::default()),
        });
        let old = p.time();
        p.tempo = 60.0;
        p.retime(&old);
        let r = &p.tracks[0].regions[0];
        assert_eq!((r.start, r.length), (4 * bar, 2 * bar));
    }

    fn audio_region(p: &mut Project, start: u64, length: u64) -> Id {
        let id = p.alloc_id();
        let t = p.add_track(TrackKind::Audio);
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "r".into(),
            start,
            length,
            muted: false,
            loops: 0,
            content: Content::Audio(AudioContent {
                file: "a.wav".into(),
                offset: 10,
                gain_db: 0.0,
                fade_in: 5,
                fade_out: 5,
                takes: Vec::new(),
                comp: Vec::new(),
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
            sidechain: None,
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
    fn repeat_join_and_nudge() {
        let mut p = Project::new("Test", 48000);
        let a = audio_region(&mut p, 100, 1000);
        let copies = p.repeat_regions(&[a]);
        assert_eq!(copies.len(), 1);
        assert_eq!(p.find_region(copies[0]).unwrap().1.start, 1100);

        // Splitting then joining an audio region gives the original back.
        let right = p.split_region(a, 600).unwrap();
        let joined = p.join_regions(&[a, right]).unwrap();
        let (_, r) = p.find_region(joined).unwrap();
        assert_eq!((r.start, r.length), (100, 1000));
        let Content::Audio(c) = &r.content else { panic!() };
        assert_eq!(c.offset, 10);
        // The repeat is a separate copy, not a continuation, so it can't be joined.
        assert!(p.join_regions(&[joined, copies[0]]).is_err());

        p.nudge_regions(&[joined], -500);
        assert_eq!(p.find_region(joined).unwrap().1.start, 0);
    }

    #[test]
    fn loops_extend_the_span() {
        let mut p = Project::new("Test", 48000);
        let a = audio_region(&mut p, 0, 1000);
        p.region_mut(a).unwrap().loops = 3;
        assert_eq!(p.find_region(a).unwrap().1.span_end(), 4000);
        assert_eq!(p.length(), 4000);
    }

    #[test]
    fn automation_values() {
        let mut lane = AutomationLane { target: AutoTarget::Volume, points: vec![] };
        assert_eq!(lane.value_at(0), None);
        lane.write(0, 1000, &[AutoPoint { pos: 100, value: -12.0 }, AutoPoint { pos: 300, value: 0.0 }]);
        assert_eq!(lane.value_at(0), Some(-12.0));
        assert_eq!(lane.value_at(200), Some(-6.0));
        assert_eq!(lane.value_at(5000), Some(0.0));
        lane.write(150, 400, &[AutoPoint { pos: 200, value: 3.0 }]);
        assert_eq!(lane.points.len(), 2);
        let mute = AutomationLane {
            target: AutoTarget::Mute,
            points: vec![AutoPoint { pos: 0, value: 0.0 }, AutoPoint { pos: 100, value: 1.0 }],
        };
        assert_eq!(mute.value_at(99), Some(0.0));
        assert_eq!(mute.value_at(100), Some(1.0));
    }

    #[test]
    fn autosave_recovery() {
        let dir = std::env::temp_dir().join(format!("solstice-autosave-{}", std::process::id()));
        let b = Bundle::new(&dir);
        let mut p = Project::new("A", 48000);
        b.save(&p).unwrap();
        assert!(b.recoverable().is_none());
        std::thread::sleep(std::time::Duration::from_millis(20));
        p.add_track(TrackKind::Audio);
        b.autosave(&p).unwrap();
        assert_eq!(b.recoverable().unwrap().tracks.len(), 1);
        b.save(&p).unwrap();
        assert!(b.recoverable().is_none());
        let _ = std::fs::remove_dir_all(dir);
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
            loops: 0,
            content: Content::Midi(MidiContent { notes, ..Default::default() }),
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
