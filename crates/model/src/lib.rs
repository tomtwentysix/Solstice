pub mod groove;
mod history;
pub mod midi_tools;
mod pattern;
mod time;

pub use groove::{Groove, GrooveHit};
pub use history::History;
pub use pattern::{Pattern, PatternRow, Step, xorshift};
pub use time::{BarLine, Beat, PPQ, SigChange, TempoChange, TimeMap, default_groups};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub type Id = u64;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    /// The file format the project was written in; see `FORMAT`.
    #[serde(default)]
    pub format: u32,
    pub name: String,
    pub sample_rate: u32,
    pub tempo: f64,
    pub sig_num: u8,
    pub sig_den: u8,
    /// Click grouping of the opening signature, as in `SigChange::groups`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sig_groups: Vec<u8>,
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
    /// What the finished mix is aimed at, for the Loudness window and export.
    #[serde(default)]
    pub master_target: LoudnessTarget,
    pub next_id: Id,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoudnessTarget {
    #[default]
    Streaming,
    Broadcast,
    Podcast,
    Club,
    None,
}

impl LoudnessTarget {
    pub const ALL: [LoudnessTarget; 5] =
        [LoudnessTarget::Streaming, LoudnessTarget::Broadcast, LoudnessTarget::Podcast, LoudnessTarget::Club, LoudnessTarget::None];

    pub fn name(self) -> &'static str {
        match self {
            LoudnessTarget::Streaming => "Streaming (−14 LUFS, −1 dBTP)",
            LoudnessTarget::Broadcast => "Broadcast (−23 LUFS, −1 dBTP)",
            LoudnessTarget::Podcast => "Podcast (−16 LUFS, −1 dBTP)",
            LoudnessTarget::Club => "Club/CD (−9 LUFS, −0.3 dBTP)",
            LoudnessTarget::None => "None",
        }
    }

    /// Integrated loudness in LUFS and the true-peak ceiling in dBTP.
    pub fn levels(self) -> Option<(f32, f32)> {
        match self {
            LoudnessTarget::Streaming => Some((-14.0, -1.0)),
            LoudnessTarget::Broadcast => Some((-23.0, -1.0)),
            LoudnessTarget::Podcast => Some((-16.0, -1.0)),
            LoudnessTarget::Club => Some((-9.0, -0.3)),
            LoudnessTarget::None => None,
        }
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<Freeze>,
    /// The drum rows the step sequencer shows, in display order. Empty means the defaults.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drum_map: Vec<DrumRow>,
    /// The inactive alternatives, sorted by name; the active one's regions are `regions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
    /// The active alternative's name. Empty means "A".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alternative: String,
    /// A commercial mix to compare against: it plays straight to the main outputs while the
    /// Reference switch is on, and never reaches the master, buses or exports.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reference: bool,
    /// Integrated loudness of a reference track's regions, for level matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_lufs: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Alternative {
    pub name: String,
    pub regions: Vec<Region>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DrumRow {
    pub pitch: u8,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub solo: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Track {
    /// Whether the drum map lets notes of `pitch` play: not muted, and soloed if any row is.
    pub fn plays_pitch(&self, pitch: u8) -> bool {
        let row = self.drum_map.iter().find(|r| r.pitch == pitch);
        let any_solo = self.drum_map.iter().any(|r| r.solo);
        !row.is_some_and(|r| r.mute) && (!any_solo || row.is_some_and(|r| r.solo))
    }

    pub fn alternative_name(&self) -> &str {
        if self.alternative.is_empty() { "A" } else { &self.alternative }
    }

    /// Every alternative's name in menu order, with its index in `alternatives`, or `None`
    /// for the active one.
    pub fn alternative_names(&self) -> Vec<(String, Option<usize>)> {
        let mut v: Vec<(String, Option<usize>)> = self.alternatives.iter().enumerate().map(|(i, a)| (a.name.clone(), Some(i))).collect();
        v.push((self.alternative_name().to_string(), None));
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// A, B, … Z, then A2, B2 and so on.
    fn free_alternative_name(&self) -> String {
        let taken = |n: &str| n == self.alternative_name() || self.alternatives.iter().any(|a| a.name == n);
        (1..)
            .flat_map(|round| ('A'..='Z').map(move |c| if round == 1 { c.to_string() } else { format!("{c}{round}") }))
            .find(|n| !taken(n))
            .unwrap()
    }

    fn sort_alternatives(&mut self) {
        self.alternatives.sort_by(|a, b| a.name.cmp(&b.name));
    }
}

/// Track Freeze: the track's instrument and inserts rendered to `file`, which plays
/// from `start` in their place while `key` still matches `freeze_key`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Freeze {
    pub file: String,
    pub start: u64,
    pub frames: u64,
    pub key: u64,
}

/// A hash of everything a freeze renders: the track without its fader, pan, mute, solo, sends,
/// routing and mix automation, plus the tempo and signature map. Plug-in states are left out
/// because they are only copied into the project on save. `DefaultHasher` may change between
/// toolchains, which only costs a re-freeze.
pub fn freeze_key(p: &Project, t: &Track) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut t = t.clone();
    t.name.clear();
    t.color = 0;
    t.input = 0;
    t.armed = false;
    t.monitor = false;
    t.stack = None;
    t.freeze = None;
    t.alternatives.clear();
    t.alternative.clear();
    for r in &mut t.regions {
        r.color = None;
    }
    t.drum_map.retain(|r| r.mute || r.solo);
    t.drum_map.sort_by_key(|r| r.pitch);
    for r in &mut t.drum_map {
        r.name.clear();
        r.hidden = false;
    }
    let c = &mut t.channel;
    c.volume_db = 0.0;
    c.pan = 0.0;
    c.mute = false;
    c.solo = false;
    c.solo_safe = false;
    c.sends.clear();
    c.output = Output::Master;
    c.vca = None;
    c.automation.retain(|l| matches!(l.target, AutoTarget::Plugin { .. }));
    for s in c.inserts.iter_mut().chain(t.instrument.iter_mut()).chain(t.midi_fx.iter_mut()) {
        s.state.clear();
    }
    // Click grouping only changes the click.
    let sigs: Vec<SigChange> = p.sig_changes.iter().map(|c| SigChange { groups: Vec::new(), ..c.clone() }).collect();
    let mut h = std::hash::DefaultHasher::new();
    serde_json::to_vec(&t).unwrap_or_default().hash(&mut h);
    serde_json::to_vec(&(p.sample_rate, p.tempo, p.sig_num, p.sig_den, &p.tempo_changes, &sigs))
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

/// The track's freeze, if it still matches the track and so plays in its place.
pub fn live_freeze<'a>(p: &Project, t: &'a Track) -> Option<&'a Freeze> {
    t.freeze.as_ref().filter(|f| f.key == freeze_key(p, t))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Bus {
    pub id: Id,
    pub name: String,
    pub channel: Channel,
    /// A VCA fader: carries no audio; its fader and mute scale the channels assigned to it.
    #[serde(default)]
    pub vca: bool,
    /// A performer's headphone mix on an interface output: solo leaves it alone and the
    /// tracks feeding it never route to it as their output.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cue: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Output {
    Master,
    Bus(Id),
    /// Straight to interface outputs, bypassing the master: `first` counts from 0 across all of
    /// them, so 0 and 1 are the main pair.
    Hardware { first: u16, stereo: bool },
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
    /// Polarity invert, applied ahead of the inserts.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<StripKind>,
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

/// A strip type: an optional way of drawing a channel's inserts as the sections of a signal
/// chain. It only changes how the strip is drawn and what its sections offer.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StripKind {
    GuitarDi,
    BassDi,
}

impl StripKind {
    pub const ALL: [StripKind; 2] = [StripKind::GuitarDi, StripKind::BassDi];

    pub fn label(self) -> &'static str {
        match self {
            StripKind::GuitarDi => "Guitar DI",
            StripKind::BassDi => "Bass DI",
        }
    }

    /// In signal order.
    pub fn sections(self) -> &'static [StripSection] {
        match self {
            StripKind::GuitarDi | StripKind::BassDi => &[StripSection::Pedals, StripSection::Amp, StripSection::Cabinet, StripSection::Effects],
        }
    }

    fn rank(self, s: StripSection) -> Option<usize> {
        self.sections().iter().position(|x| *x == s)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StripSection {
    Pedals,
    Amp,
    Cabinet,
    Effects,
}

impl StripSection {
    pub fn label(self) -> &'static str {
        match self {
            StripSection::Pedals => "Pedals",
            StripSection::Amp => "Amp",
            StripSection::Cabinet => "Cabinet",
            StripSection::Effects => "Effects",
        }
    }

    /// The plug-in category a section insists on; the others take anything.
    pub fn requires(self) -> Option<&'static str> {
        match self {
            StripSection::Amp => Some("Amp"),
            StripSection::Cabinet => Some("Cabinet"),
            StripSection::Pedals | StripSection::Effects => None,
        }
    }

    pub fn accepts<S: AsRef<str>>(self, categories: &[S]) -> bool {
        self.requires().is_none_or(|r| categories.iter().any(|c| c.as_ref() == r))
    }
}

impl Channel {
    /// Sets or clears the strip type without reordering, adding or removing inserts. A new
    /// type with different sections files every insert under its last section; clearing it
    /// makes every insert plain.
    pub fn set_kind(&mut self, kind: Option<StripKind>) {
        let keep = match (self.kind, kind) {
            (Some(old), Some(new)) => old.sections() == new.sections(),
            _ => false,
        };
        self.kind = kind;
        if keep {
            return;
        }
        let last = kind.and_then(|k| k.sections().last().copied());
        for s in &mut self.inserts {
            s.section = last;
        }
    }

    /// Where a plug-in added to `section` goes: after that section's last insert, or for an
    /// empty section ahead of the first insert of a later one.
    pub fn section_insert_index(&self, section: StripSection) -> usize {
        let Some(kind) = self.kind else { return self.inserts.len() };
        if let Some(i) = self.inserts.iter().rposition(|s| s.section == Some(section)) {
            return i + 1;
        }
        let Some(want) = kind.rank(section) else { return self.inserts.len() };
        self.inserts
            .iter()
            .position(|s| s.section.and_then(|x| kind.rank(x)).is_some_and(|r| r > want))
            .unwrap_or(self.inserts.len())
    }

    /// Whether an insert in `section` may sit at `index` (counted before it is placed) without
    /// the sections going backwards. A plain insert fits anywhere.
    pub fn can_place(&self, index: usize, section: Option<StripSection>) -> bool {
        let Some(section) = section else { return true };
        let Some(want) = self.kind.and_then(|k| k.rank(section)) else { return false };
        let rank = |s: &PluginSlot| s.section.and_then(|x| self.kind.and_then(|k| k.rank(x)));
        let index = index.min(self.inserts.len());
        let before = self.inserts[..index].iter().filter_map(rank).max();
        let after = self.inserts[index..].iter().filter_map(rank).min();
        before.is_none_or(|b| b <= want) && after.is_none_or(|a| a >= want)
    }

    /// Whether the inserts' sections run in the strip type's order and belong to it.
    pub fn sections_in_order(&self) -> bool {
        let Some(kind) = self.kind else { return self.inserts.iter().all(|s| s.section.is_none()) };
        let mut last = 0;
        for s in &self.inserts {
            let Some(sec) = s.section else { continue };
            let Some(r) = kind.rank(sec) else { return false };
            if r < last {
                return false;
            }
            last = r;
        }
        true
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
            invert: false,
            kind: None,
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
    /// The send's own pan, for pre- and post-fader sends, which tap ahead of the channel's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pan: Option<f32>,
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
    /// Which section of a typed strip the insert is drawn in; none for a plain insert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<StripSection>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
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
    /// Frames from the region start for audio, ticks for MIDI.
    pub start: u64,
    pub take: usize,
}

/// The comp with `take` playing over `from..to`, merging neighbouring sections of one take.
fn recomp(comp: &[CompSection], from: u64, to: u64, take: usize, length: u64) -> Vec<CompSection> {
    let at = |pos: u64| comp.iter().rev().find(|c| c.start <= pos).map_or(0, |c| c.take);
    let mut cuts: Vec<u64> = comp.iter().map(|c| c.start).chain([0, from, to.min(length)]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<CompSection> = Vec::new();
    for c in cuts.into_iter().filter(|c| *c < length) {
        let t = if c >= from && c < to { take } else { at(c) };
        if out.last().is_none_or(|l| l.take != t) {
            out.push(CompSection { start: c, take: t });
        }
    }
    out
}

/// The comp after the region start moves by `delta`, keeping each section's place on the timeline.
fn shift_comp(comp: &[CompSection], delta: i64) -> Vec<CompSection> {
    let first = comp.iter().rev().find(|c| c.start as i64 <= delta.max(0)).map_or(0, |c| c.take);
    let mut out = vec![CompSection { start: 0, take: first }];
    for c in comp {
        let s = c.start as i64 - delta;
        if s > 0 && out.last().is_some_and(|l| l.take != c.take) {
            out.push(CompSection { start: s as u64, take: c.take });
        }
    }
    out
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
        self.comp = recomp(&self.comp, from, to, take, length);
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
        self.comp = shift_comp(&self.comp, delta);
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
    /// In a take folder, the notes the comp plays. Editors work on these, and
    /// `Project::settle_midi_folders` writes their changes back into the takes.
    pub notes: Vec<Note>,
    /// Controller and pitch-bend data, sorted by tick. Shared by every take, since recording
    /// only captures notes.
    #[serde(default)]
    pub controllers: Vec<Controller>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub takes: Vec<MidiTake>,
    /// Which take plays where; a note belongs to the section its start falls in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comp: Vec<CompSection>,
    /// The step pattern `notes` were baked from. It is dropped once anything else edits them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<Pattern>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MidiTake {
    pub name: String,
    pub notes: Vec<Note>,
}

fn sort_notes(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.start, n.pitch, n.length, n.velocity));
}

/// Splits off the notes starting at or after `cut`, moved to start from it, and shortens the
/// rest to end there.
fn split_notes(notes: &mut Vec<Note>, cut: u32) -> Vec<Note> {
    let right = notes.iter().filter(|n| n.start >= cut).map(|n| Note { start: n.start - cut, ..*n }).collect();
    notes.retain(|n| n.start < cut);
    for n in notes.iter_mut() {
        n.length = n.length.min(cut - n.start);
    }
    right
}

/// Moves notes `delta` ticks earlier, dropping any that would start before the region.
fn shift_notes(notes: &mut Vec<Note>, delta: i64) {
    notes.retain(|n| n.start as i64 - delta >= 0);
    for n in notes.iter_mut() {
        n.start = (n.start as i64 - delta) as u32;
    }
}

impl MidiContent {
    pub fn is_folder(&self) -> bool {
        !self.takes.is_empty()
    }

    /// The comp as (start, end, take) in ticks. The last section runs on past the region end,
    /// so notes beyond a trimmed end are kept.
    fn sections(&self) -> Vec<(u32, u32, usize)> {
        if self.comp.is_empty() {
            return vec![(0, u32::MAX, 0)];
        }
        let tick = |c: &CompSection| c.start.min(u32::MAX as u64) as u32;
        (0..self.comp.len())
            .map(|i| {
                let start = if i == 0 { 0 } else { tick(&self.comp[i]) };
                (start, self.comp.get(i + 1).map_or(u32::MAX, tick), self.comp[i].take)
            })
            .collect()
    }

    /// What the comp plays: each section's notes from its take.
    pub fn resolved(&self) -> Vec<Note> {
        let mut out = Vec::new();
        for (start, end, take) in self.sections() {
            if let Some(t) = self.takes.get(take) {
                out.extend(t.notes.iter().filter(|n| n.start >= start && n.start < end));
            }
        }
        sort_notes(&mut out);
        out
    }

    pub fn resolve(&mut self) {
        self.notes = self.resolved();
    }

    /// Puts the notes into the takes the comp plays at their starts. Running it again, or
    /// resolving afterwards, changes nothing.
    pub fn write_back(&mut self) {
        for (start, end, take) in self.sections() {
            let Some(t) = self.takes.get_mut(take) else { continue };
            let within = |n: &Note| n.start >= start && n.start < end;
            t.notes.retain(|n| !within(n));
            t.notes.extend(self.notes.iter().filter(|n| within(n)));
            sort_notes(&mut t.notes);
        }
    }

    /// Writes edited notes back into the takes. Returns whether there were any.
    pub fn settle(&mut self) -> bool {
        if !self.is_folder() {
            return false;
        }
        let mut mine = self.notes.clone();
        sort_notes(&mut mine);
        if mine == self.resolved() {
            return false;
        }
        self.write_back();
        true
    }

    /// Swipe comping: `take` plays over `from..to`, in ticks.
    pub fn set_comp(&mut self, from: u64, to: u64, take: usize, length: u64) {
        if take >= self.takes.len() || from >= to {
            return;
        }
        self.settle();
        self.comp = recomp(&self.comp, from, to, take, length);
        self.resolve();
    }

    /// Adds a take, which becomes the one that plays. The first turns the notes into Take 1.
    pub fn add_take(&mut self, take: MidiTake) {
        if self.is_folder() {
            self.settle();
        } else {
            self.takes.push(MidiTake { name: "Take 1".into(), notes: self.notes.clone() });
        }
        self.takes.push(take);
        self.comp = vec![CompSection { start: 0, take: self.takes.len() - 1 }];
        self.resolve();
    }

    /// The region start moved by `delta` ticks: notes keep their place on the timeline, and
    /// any left before the new start are dropped.
    pub fn shift_ticks(&mut self, delta: i64) {
        self.settle();
        shift_notes(&mut self.notes, delta);
        self.controllers.retain(|c| c.tick as i64 - delta >= 0);
        for c in &mut self.controllers {
            c.tick = (c.tick as i64 - delta) as u32;
        }
        if self.is_folder() {
            for t in &mut self.takes {
                shift_notes(&mut t.notes, delta);
            }
            self.comp = shift_comp(&self.comp, delta);
        }
    }

    /// Cuts at `cut` ticks, keeping the left part here and returning the right part.
    pub fn split_at(&mut self, cut: u32) -> MidiContent {
        self.settle();
        let notes = split_notes(&mut self.notes, cut);
        let controllers = self.controllers.iter().filter(|c| c.tick >= cut).map(|c| Controller { tick: c.tick - cut, ..*c }).collect();
        self.controllers.retain(|c| c.tick < cut);
        let mut right = MidiContent { notes, controllers, ..Default::default() };
        if self.is_folder() {
            right.takes = self.takes.iter_mut().map(|t| MidiTake { name: t.name.clone(), notes: split_notes(&mut t.notes, cut) }).collect();
            right.comp = shift_comp(&self.comp, cut as i64);
            self.comp.retain(|c| c.start < cut as u64);
        }
        if let Some(p) = &mut self.pattern
            && self.notes == p.bake(cut)
        {
            p.span = cut;
        }
        right
    }

    /// Makes `p` the region's pattern and its notes the pattern baked over `span` ticks.
    pub fn set_pattern(&mut self, mut p: Pattern, span: u32) {
        p.span = span;
        self.notes = p.bake(span);
        self.pattern = Some(p);
    }

    /// Keeps only what the comp plays.
    pub fn flatten(&mut self) {
        self.settle();
        self.takes.clear();
        self.comp.clear();
    }
}

/// Notes from live MIDI events (sample position, message), timed from `start`. Notes still
/// held end at `end`, and a note-off without a note-on before it is ignored.
pub fn notes_from_events(time: &TimeMap, start: u64, end: u64, events: &[(u64, [u8; 3])]) -> Vec<Note> {
    let tick = |pos: u64| time.ticks_between(start, pos).min(u32::MAX as u64) as u32;
    let close = |(pitch, s, velocity): (u8, u64, u8), at: u64| {
        Note::new(tick(s), tick(at).saturating_sub(tick(s)).max(1), pitch, velocity)
    };
    let mut open: Vec<(u8, u64, u8)> = Vec::new();
    let mut notes = Vec::new();
    for &(pos, d) in events {
        let on = d[0] & 0xf0 == 0x90 && d[2] > 0;
        let off = d[0] & 0xf0 == 0x80 || (d[0] & 0xf0 == 0x90 && d[2] == 0);
        if !on && !off {
            continue;
        }
        if let Some(i) = open.iter().position(|o| o.0 == d[1]) {
            notes.push(close(open.remove(i), pos));
        }
        if on {
            open.push((d[1], pos, d[2]));
        }
    }
    notes.extend(open.into_iter().map(|o| close(o, end)));
    notes.sort_by_key(|n| (n.start, n.pitch));
    notes
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
    /// Percent chance that the note plays, rolled afresh on each pass.
    #[serde(default = "full_chance", skip_serializing_if = "is_full_chance")]
    pub chance: u8,
}

fn full_chance() -> u8 {
    100
}

fn is_full_chance(c: &u8) -> bool {
    *c == 100
}

impl Note {
    pub fn new(start: u32, length: u32, pitch: u8, velocity: u8) -> Self {
        Note { start, length, pitch, velocity, chance: 100 }
    }
}

pub const TRACK_COLORS: usize = 8;

impl Project {
    pub fn new(name: &str, sample_rate: u32) -> Self {
        let bar = TimeMap::new(sample_rate, 120.0, 4, 4).bar_len();
        Project {
            format: FORMAT,
            name: name.to_string(),
            sample_rate,
            tempo: 120.0,
            sig_num: 4,
            sig_den: 4,
            sig_groups: Vec::new(),
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
            master_target: LoudnessTarget::default(),
            next_id: 1,
        }
    }

    pub fn time(&self) -> TimeMap {
        TimeMap::with_changes(self.sample_rate, self.tempo, self.sig_num, self.sig_den, &self.sig_groups, &self.tempo_changes, &self.sig_changes)
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
            freeze: None,
            drum_map: Vec::new(),
            alternatives: Vec::new(),
            alternative: String::new(),
            reference: false,
            reference_lufs: None,
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
        for r in t.regions.iter_mut().chain(t.alternatives.iter_mut().flat_map(|a| a.regions.iter_mut())) {
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

    /// Keeps the current regions as an inactive alternative and starts a new one with the next
    /// free name, empty or, with `duplicate`, a copy of them.
    pub fn new_alternative(&mut self, track: Id, duplicate: bool) -> bool {
        let Some(i) = self.tracks.iter().position(|t| t.id == track) else { return false };
        let mut regions = if duplicate { self.tracks[i].regions.clone() } else { Vec::new() };
        for r in &mut regions {
            r.id = self.alloc_id();
        }
        let t = &mut self.tracks[i];
        let name = t.free_alternative_name();
        let old = Alternative { name: t.alternative_name().to_string(), regions: std::mem::replace(&mut t.regions, regions) };
        t.alternatives.push(old);
        t.sort_alternatives();
        t.alternative = name;
        true
    }

    /// Makes `alternatives[index]` the active one.
    pub fn switch_alternative(&mut self, track: Id, index: usize) -> bool {
        let Some(t) = self.track_mut(track) else { return false };
        let active = t.alternative_name().to_string();
        let Some(alt) = t.alternatives.get_mut(index) else { return false };
        std::mem::swap(&mut t.regions, &mut alt.regions);
        t.alternative = std::mem::replace(&mut alt.name, active);
        t.sort_alternatives();
        true
    }

    /// Deletes the active alternative and activates the first remaining one.
    pub fn delete_alternative(&mut self, track: Id) -> bool {
        let Some(t) = self.track_mut(track) else { return false };
        if t.alternatives.is_empty() {
            return false;
        }
        let next = t.alternatives.remove(0);
        t.regions = next.regions;
        t.alternative = next.name;
        true
    }

    /// Renames the active alternative, unless another one has that name.
    pub fn rename_alternative(&mut self, track: Id, name: &str) -> bool {
        let Some(t) = self.track_mut(track) else { return false };
        if name.is_empty() || name == t.alternative_name() || t.alternatives.iter().any(|a| a.name == name) {
            return false;
        }
        t.alternative = name.to_string();
        true
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
        self.buses.push(Bus { id, name, channel: Channel::default(), vca: false, cue: false });
        id
    }

    pub fn add_vca(&mut self) -> Id {
        let id = self.alloc_id();
        let name = format!("VCA {}", self.buses.iter().filter(|b| b.vca).count() + 1);
        self.buses.push(Bus { id, name, channel: Channel::default(), vca: true, cue: false });
        id
    }

    /// A cue mix on the interface output pair from `first`, fed pre-fader by every track except
    /// reference tracks at its current fader level and pan, so the performer starts from the
    /// engineer's mix.
    pub fn add_cue_mix(&mut self, name: String, first: u16) -> Id {
        let id = self.alloc_id();
        let channel = Channel { output: Output::Hardware { first, stereo: true }, ..Channel::default() };
        self.buses.push(Bus { id, name, channel, vca: false, cue: true });
        for t in self.tracks.iter_mut().filter(|t| !t.reference) {
            let c = &mut t.channel;
            c.sends.push(Send { bus: id, level_db: c.volume_db, mode: SendMode::PreFader, pan: Some(c.pan) });
        }
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

    /// Re-bakes pattern regions whose span in ticks has changed, as after a resize or tempo
    /// change. A pattern whose notes something else has edited, or that has become a take
    /// folder, is dropped and its notes kept. Returns how many were dropped.
    pub fn settle_patterns(&mut self) -> usize {
        let time = self.time();
        let mut dropped = 0;
        for r in self.tracks.iter_mut().flat_map(|t| t.regions.iter_mut()) {
            let span = time.ticks_between(r.start, r.end()).min(u32::MAX as u64) as u32;
            let Content::Midi(m) = &mut r.content else { continue };
            let Some(p) = m.pattern.take() else { continue };
            if m.is_folder() || m.notes != p.bake(p.span) {
                dropped += 1;
            } else if p.span != span {
                m.set_pattern(p, span);
            } else {
                m.pattern = Some(p);
            }
        }
        dropped
    }

    /// Writes notes edited in MIDI take folders back into their takes.
    pub fn settle_midi_folders(&mut self) {
        for r in self.tracks.iter_mut().flat_map(|t| t.regions.iter_mut()) {
            if let Content::Midi(m) = &mut r.content {
                m.settle();
            }
        }
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
        let cut_ticks = time.ticks_between(region.start, at) as u32;
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
            (Content::Midi(l), Content::Midi(r)) => *r = l.split_at(cut_ticks),
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
            b.channel.sends.push(Send { bus: to, level_db: 0.0, mode: SendMode::PostPan, pan: None });
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

    /// Joins regions on one track into the first of them. MIDI regions
    /// merge their notes, flattening take folders to their comps; audio regions only join when
    /// they are consecutive pieces of the same file, since anything else would need a bounce.
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
                    let shift = time.ticks_between(start, r.start) as u32;
                    notes.extend(m.notes.iter().map(|n| Note { start: n.start + shift, ..*n }));
                    controllers.extend(m.controllers.iter().map(|c| Controller { tick: c.tick + shift, ..*c }));
                }
                notes.sort_by_key(|n| (n.start, n.pitch));
                controllers.sort_by_key(|c| c.tick);
                Content::Midi(MidiContent { notes, controllers, ..Default::default() })
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

    /// Whether a solo silences other channels. A reference track's solo doesn't touch the mix.
    pub fn any_solo(&self) -> bool {
        self.tracks.iter().any(|t| t.channel.solo && !t.reference) || self.buses.iter().any(|b| b.channel.solo && !b.cue)
    }

    pub fn length(&self) -> u64 {
        self.tracks.iter().flat_map(|t| t.regions.iter()).map(|r| r.span_end()).max().unwrap_or(0)
    }

    /// The end of the last region that is part of the mix, leaving reference tracks out.
    pub fn mix_length(&self) -> u64 {
        self.tracks.iter().filter(|t| !t.reference).flat_map(|t| t.regions.iter()).map(|r| r.span_end()).max().unwrap_or(0)
    }

    /// Splits every region that spans `pos`, so edits can treat each side on its own.
    fn split_all_at(&mut self, pos: u64) {
        let ids: Vec<Id> =
            self.tracks.iter().flat_map(|t| t.regions.iter()).filter(|r| r.start < pos && r.end() > pos && r.loops == 0).map(|r| r.id).collect();
        for id in ids {
            self.split_region(id, pos);
        }
    }

    /// Runs `f` on the active regions, then again with each inactive alternative's regions
    /// swapped into `regions`, so time edits reach every alternative.
    fn in_every_alternative(&mut self, mut f: impl FnMut(&mut Self)) {
        f(self);
        let layers = self.tracks.iter().map(|t| t.alternatives.len()).max().unwrap_or(0);
        for k in 0..layers {
            let active: Vec<Vec<Region>> = self
                .tracks
                .iter_mut()
                .map(|t| {
                    let alt = t.alternatives.get_mut(k).map(|a| std::mem::take(&mut a.regions)).unwrap_or_default();
                    std::mem::replace(&mut t.regions, alt)
                })
                .collect();
            f(self);
            for (t, regions) in self.tracks.iter_mut().zip(active) {
                let alt = std::mem::replace(&mut t.regions, regions);
                if let Some(a) = t.alternatives.get_mut(k) {
                    a.regions = alt;
                }
            }
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
        let shift = |p: &mut u64| {
            if *p >= at {
                *p += len;
            }
        };
        self.in_every_alternative(|p| {
            p.split_all_at(at);
            for r in p.tracks.iter_mut().flat_map(|t| t.regions.iter_mut()) {
                shift(&mut r.start);
            }
        });
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
        self.in_every_alternative(|p| {
            p.split_all_at(start);
            p.split_all_at(end);
            for t in &mut p.tracks {
                t.regions.retain(|r| r.start < start || r.start >= end);
                for r in t.regions.iter_mut().filter(|r| r.start >= end) {
                    r.start -= len;
                }
            }
        });
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
        self.in_every_alternative(|p| {
            p.split_all_at(sec.start);
            let copies: Vec<(usize, Region)> = p
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(i, t)| t.regions.iter().filter(|r| r.start >= sec.start && r.start < sec.end()).map(move |r| (i, r.clone())))
                .collect();
            for (i, mut r) in copies {
                r.id = p.alloc_id();
                r.start += sec.length;
                p.tracks[i].regions.push(r);
            }
        });
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
    /// Whether any region, in any alternative, plays audio files, which fix the project's rate.
    pub fn has_audio(&self) -> bool {
        self.tracks
            .iter()
            .flat_map(|t| t.regions.iter().chain(t.alternatives.iter().flat_map(|a| a.regions.iter())))
            .any(|r| matches!(r.content, Content::Audio(_)))
    }

    /// Moves the project to another sample rate, keeping everything on its bars. Refused (false)
    /// once the project has audio, since its files would need converting too.
    pub fn set_sample_rate(&mut self, rate: u32) -> bool {
        if rate == self.sample_rate {
            return true;
        }
        if self.has_audio() || rate == 0 {
            return false;
        }
        let old = self.time();
        self.sample_rate = rate;
        self.retime(&old);
        true
    }

    pub fn retime(&mut self, old: &TimeMap) {
        let new = self.time();
        let moved = |pos: u64| new.tick_to_sample_f(old.sample_to_tick_f(pos as f64)).round() as u64;
        for t in &mut self.tracks {
            for r in t.regions.iter_mut().chain(t.alternatives.iter_mut().flat_map(|a| a.regions.iter_mut())) {
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

    /// Loads `project.json`, upgraded to the current format. A bundle that was never saved
    /// opens from its autosave, or failing that is rebuilt from its recordings.
    pub fn load(&self) -> std::io::Result<Project> {
        let saved = self.dir.join("project.json");
        if saved.is_file() {
            return load_file(&saved);
        }
        if self.autosave_path().is_file() {
            return load_file(&self.autosave_path());
        }
        if self.audio_dir().is_dir() {
            return Ok(self.rebuild());
        }
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "that folder is not a Solstice project"))
    }

    /// One audio track per recording in `Audio Files`, each from the project start.
    fn rebuild(&self) -> Project {
        let name = self.dir.file_stem().map_or_else(|| "Recovered".into(), |s| s.to_string_lossy().into_owned());
        let mut files: Vec<(String, WavInfo)> = std::fs::read_dir(self.audio_dir())
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| Some((e.file_name().into_string().ok()?, wav_info(&e.path())?)))
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut p = Project::new(&name, files.first().map_or(48000, |f| f.1.sample_rate));
        for (file, info) in files {
            let stem = Path::new(&file).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
            let length = (info.frames as f64 * p.sample_rate as f64 / info.sample_rate as f64).round() as u64;
            let t = p.add_track(TrackKind::Audio);
            let id = p.alloc_id();
            let track = p.track_mut(t).expect("just added");
            track.name = stem.clone();
            track.stereo = info.channels == 2;
            track.regions.push(Region {
                id,
                name: stem,
                start: 0,
                // A file at another rate is converted when the project opens; the length is
                // already in the project's samples.
                length,
                muted: false,
                loops: 0,
                color: None,
                content: Content::Audio(AudioContent { file, offset: 0, gain_db: 0.0, fade_in: 0, fade_out: 0, takes: Vec::new(), comp: Vec::new() }),
            });
        }
        p
    }

    /// Whether `dir` can be opened: a saved project, an autosaved one, or a bundle that was
    /// never saved but holds recordings.
    pub fn is_openable(dir: &Path) -> bool {
        Self::is_bundle(dir)
            || dir.join("autosave.json").is_file()
            || (dir.extension().is_some_and(|e| e == "solstice") && dir.join("Audio Files").is_dir())
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
        // Without a saved project, `load` has already opened the autosave.
        if saved.is_none_or(|s| s >= auto) {
            return None;
        }
        load_file(&self.autosave_path()).ok()
    }

    pub fn clear_autosave(&self) {
        let _ = std::fs::remove_file(self.autosave_path());
    }

    /// Sets the autosave aside rather than deleting it, in case it was discarded by mistake.
    pub fn discard_autosave(&self) {
        let _ = std::fs::rename(self.autosave_path(), self.autosave_path().with_extension("json.discarded"));
    }

    pub fn is_bundle(dir: &Path) -> bool {
        dir.join("project.json").is_file()
    }
}

/// The project file format. Raise it when a change needs more than new fields with serde
/// defaults, and add the upgrade step to `migrate`.
pub const FORMAT: u32 = 1;

fn load_file(path: &Path) -> std::io::Result<Project> {
    let damaged = |e: serde_json::Error| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("the project file is damaged: {e}"));
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?).map_err(damaged)?;
    migrate(&mut v)?;
    serde_json::from_value(v).map_err(damaged)
}

/// Upgrades a project file's JSON to `FORMAT`. Format 0 (no `format` field) differs from 1
/// only by fields that have serde defaults, so it needs no step.
fn migrate(v: &mut serde_json::Value) -> std::io::Result<()> {
    let invalid = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let Some(obj) = v.as_object_mut() else { return Err(invalid("the project file is damaged")) };
    let format = obj.get("format").and_then(serde_json::Value::as_u64).unwrap_or(0);
    if format > FORMAT as u64 {
        return Err(invalid("the project was saved by a newer version of Solstice"));
    }
    obj.insert("format".into(), FORMAT.into());
    Ok(())
}

struct WavInfo {
    sample_rate: u32,
    channels: u16,
    frames: u64,
}

/// Reads the `fmt ` and `data` chunk headers of a RIFF WAVE file. A `data` size of 0 or one
/// past the end of the file (a recording cut short) counts the bytes actually there.
fn wav_info(path: &Path) -> Option<WavInfo> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return None;
    }
    let (mut rate, mut channels, mut block) = (0u32, 0u16, 0u16);
    loop {
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch).ok()?;
        let size = u32::from_le_bytes(ch[4..8].try_into().ok()?) as u64;
        let at = f.stream_position().ok()?;
        match &ch[0..4] {
            b"fmt " => {
                let mut fmt = [0u8; 16];
                f.read_exact(&mut fmt).ok()?;
                channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                rate = u32::from_le_bytes(fmt[4..8].try_into().ok()?);
                block = u16::from_le_bytes([fmt[12], fmt[13]]);
            }
            b"data" => {
                if block == 0 || rate == 0 {
                    return None;
                }
                let available = len.saturating_sub(at);
                let size = if size == 0 || size > available { available } else { size };
                return Some(WavInfo { sample_rate: rate, channels, frames: size / block as u64 });
            }
            _ => {}
        }
        f.seek(SeekFrom::Start(at + size + (size & 1))).ok()?;
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
        let slot = PluginSlot { id: p.alloc_id(), uid: "x".into(), path: Default::default(), name: "X".into(), bypass: false, state: vec![1], sidechain: None, section: None };
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
        p.track_mut(t).unwrap().regions.push(Region { id, name: "f".into(), start: 0, length: 1000, muted: false, loops: 0, color: None, content: Content::Audio(a) });
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
                color: None,
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
    fn time_edits_reach_inactive_alternatives() {
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Audio);
        for (start, len) in [(0, 100), (150, 100), (400, 100)] {
            let id = p.alloc_id();
            p.track_mut(t).unwrap().regions.push(Region {
                id,
                name: "r".into(),
                start,
                length: len,
                muted: false,
                loops: 0,
                color: None,
                content: Content::Audio(AudioContent::new("a.wav")),
            });
        }
        assert!(p.new_alternative(t, true));
        let verse = p.alloc_id();
        p.sections.push(Section { id: verse, start: 100, length: 100, name: "Verse".into() });
        let spans = |rs: &[Region]| {
            let mut v: Vec<(u64, u64)> = rs.iter().map(|r| (r.start, r.length)).collect();
            v.sort();
            v
        };
        let check = |p: &Project| {
            let tr = p.track(t).unwrap();
            assert_eq!(spans(&tr.regions), spans(&tr.alternatives[0].regions));
            spans(&tr.regions)
        };
        p.duplicate_section(verse).unwrap();
        assert_eq!(check(&p), vec![(0, 100), (150, 50), (250, 50), (300, 50), (500, 100)]);
        p.insert_time(270, 30);
        assert_eq!(check(&p), vec![(0, 100), (150, 50), (250, 20), (300, 30), (330, 50), (530, 100)]);
        p.delete_section(verse);
        assert_eq!(check(&p), vec![(0, 100), (150, 20), (200, 30), (230, 50), (430, 100)]);
        p.cut_time(50, 180);
        assert_eq!(check(&p), vec![(0, 50), (70, 30), (100, 50), (300, 100)]);
        let ids: Vec<Id> = p.tracks[0].regions.iter().chain(&p.tracks[0].alternatives[0].regions).map(|r| r.id).collect();
        assert!(ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id)));
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
            color: None,
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
            color: None,
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
            section: None,
        });
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn the_loudness_target_round_trips_and_defaults_for_old_files() {
        let mut p = Project::new("Test", 48000);
        p.master_target = LoudnessTarget::Club;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        v.as_object_mut().unwrap().remove("master_target");
        let old: Project = serde_json::from_value(v).unwrap();
        assert_eq!(old.master_target, LoudnessTarget::Streaming);
    }

    #[test]
    fn unfrozen_tracks_save_without_a_freeze_field() {
        let mut p = Project::new("Test", 48000);
        audio_region(&mut p, 100, 1000);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("freeze"));
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        p.tracks[0].freeze = Some(Freeze { file: "f.wav".into(), start: 0, frames: 10, key: 7 });
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn reference_tracks_round_trip_and_stay_out_of_the_mix() {
        let mut p = Project::new("Test", 48000);
        audio_region(&mut p, 100, 1000);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("reference"));
        let r = p.add_track(TrackKind::Audio);
        let id = p.alloc_id();
        let t = p.track_mut(r).unwrap();
        t.reference = true;
        t.reference_lufs = Some(-9.5);
        t.channel.solo = true;
        t.regions.push(Region { id, name: "r".into(), start: 0, length: 9000, muted: false, loops: 0, color: None, content: Content::Audio(AudioContent::new("ref.wav")) });
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        assert_eq!((p.length(), p.mix_length()), (9000, 1100));
        assert!(!p.any_solo());
    }

    #[test]
    fn freeze_key_follows_what_the_freeze_renders() {
        let mut p = Project::new("Test", 48000);
        let r = audio_region(&mut p, 100, 1000);
        let key = |p: &Project| freeze_key(p, &p.tracks[0]);
        let k = key(&p);
        let ch = &mut p.tracks[0].channel;
        ch.volume_db = -12.0;
        ch.pan = 0.5;
        ch.mute = true;
        ch.lane_mut(AutoTarget::Volume).points.push(AutoPoint { pos: 0, value: -3.0 });
        p.tracks[0].name = "Renamed".into();
        p.tracks[0].freeze = Some(Freeze { file: "f.wav".into(), start: 0, frames: 10, key: k });
        assert_eq!(key(&p), k);
        p.region_mut(r).unwrap().start = 200;
        assert_ne!(key(&p), k);
        p.region_mut(r).unwrap().start = 100;
        p.tracks[0].channel.invert = true;
        assert_ne!(key(&p), k);
        p.tracks[0].channel.invert = false;
        p.sig_changes.push(SigChange { bar: 4, num: 7, den: 8, groups: vec![] });
        let k = key(&p);
        p.sig_changes[0].groups = vec![2, 2, 3];
        p.sig_groups = vec![2, 2];
        assert_eq!(key(&p), k, "click grouping is not rendered");
        p.tempo = 90.0;
        assert_ne!(key(&p), k);
    }

    #[test]
    fn invert_defaults_off_and_is_left_out_when_off() {
        let mut p = Project::new("Test", 48000);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("invert"));
        p.master.invert = true;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        let ch: Channel = serde_json::from_str(r#"{"volume_db":0,"pan":0,"mute":false,"solo":false,"inserts":[],"sends":[],"output":"Master"}"#).unwrap();
        assert!(!ch.invert);
    }

    fn typed(sections: &[Option<StripSection>]) -> Channel {
        let mut ch = Channel { kind: Some(StripKind::GuitarDi), ..Channel::default() };
        for (i, section) in sections.iter().enumerate() {
            ch.inserts.push(PluginSlot {
                id: i as Id + 1,
                uid: String::new(),
                path: Default::default(),
                name: format!("{i}"),
                bypass: false,
                state: vec![],
                sidechain: None,
                section: *section,
            });
        }
        ch
    }

    #[test]
    fn setting_and_clearing_a_strip_type_keeps_the_plug_ins_in_order() {
        use StripSection::*;
        let mut ch = typed(&[None, None, None]);
        ch.kind = None;
        let before: Vec<Id> = ch.inserts.iter().map(|s| s.id).collect();
        ch.set_kind(Some(StripKind::GuitarDi));
        assert!(ch.inserts.iter().all(|s| s.section == Some(Effects)));
        assert!(ch.sections_in_order());
        ch.inserts[0].section = Some(Pedals);
        ch.inserts[1].section = None;
        // Guitar and bass share their sections, so switching between them keeps them.
        ch.set_kind(Some(StripKind::BassDi));
        assert_eq!(ch.inserts.iter().map(|s| s.section).collect::<Vec<_>>(), [Some(Pedals), None, Some(Effects)]);
        ch.set_kind(None);
        assert_eq!(ch.kind, None);
        assert!(ch.inserts.iter().all(|s| s.section.is_none()));
        assert_eq!(ch.inserts.iter().map(|s| s.id).collect::<Vec<_>>(), before);
        assert!(ch.sections_in_order());
    }

    #[test]
    fn sections_never_run_backwards() {
        use StripSection::*;
        let ch = typed(&[Some(Pedals), Some(Amp), None, Some(Cabinet), Some(Effects)]);
        assert!(ch.sections_in_order());
        assert!(!typed(&[Some(Amp), Some(Pedals)]).sections_in_order());
        let mut plain = typed(&[Some(Amp)]);
        plain.kind = None;
        assert!(!plain.sections_in_order());

        // Adding to a section goes after its last insert; to an empty one, ahead of the next.
        assert_eq!(ch.section_insert_index(Pedals), 1);
        assert_eq!(ch.section_insert_index(Amp), 2);
        assert_eq!(ch.section_insert_index(Effects), 5);
        let gap = typed(&[Some(Pedals), Some(Amp), None, Some(Effects)]);
        assert_eq!(gap.section_insert_index(Cabinet), 3, "the EQ stays between amp and cabinet");
        let empty = typed(&[]);
        assert_eq!(empty.section_insert_index(Amp), 0);
        let mut untyped = typed(&[None, None]);
        untyped.kind = None;
        assert_eq!(untyped.section_insert_index(Amp), 2);

        // Placing at an index: a plain insert fits anywhere, a sectioned one only in order.
        for i in 0..=5 {
            assert!(ch.can_place(i, None));
        }
        assert!(ch.can_place(0, Some(Pedals)));
        assert!(!ch.can_place(0, Some(Amp)));
        assert!(ch.can_place(2, Some(Amp)));
        assert!(ch.can_place(3, Some(Amp)));
        assert!(ch.can_place(3, Some(Cabinet)));
        assert!(!ch.can_place(4, Some(Amp)));
        assert!(ch.can_place(5, Some(Effects)));
        assert!(!ch.can_place(5, Some(Cabinet)));
        assert!(!untyped.can_place(0, Some(Pedals)));
    }

    #[test]
    fn only_amp_and_cabinet_insist_on_a_category() {
        use StripSection::*;
        let amp = ["Amp", "Pedal"];
        let reverb = ["Cabinet", "Reverb"];
        let eq = ["EQ"];
        assert!(Amp.accepts(&amp) && !Amp.accepts(&reverb) && !Amp.accepts(&eq));
        assert!(Cabinet.accepts(&reverb) && !Cabinet.accepts(&amp));
        for any in [Pedals, Effects] {
            assert!(any.accepts(&amp) && any.accepts(&reverb) && any.accepts(&eq) && any.accepts::<&str>(&[]));
        }
    }

    #[test]
    fn strip_types_are_left_out_of_old_style_projects() {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Audio);
        let slot = PluginSlot { id: p.alloc_id(), uid: "x".into(), path: Default::default(), name: "X".into(), bypass: false, state: vec![], sidechain: None, section: None };
        p.track_mut(t).unwrap().channel.inserts.push(slot);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"kind\":null") && !json.contains("\"section\":"));
        assert!(!json.contains("GuitarDi"));
        let ch: Channel = serde_json::from_str(r#"{"volume_db":0,"pan":0,"mute":false,"solo":false,"inserts":[{"id":1,"uid":"x","path":"","name":"X","bypass":false,"state":""}],"sends":[],"output":"Master"}"#).unwrap();
        assert_eq!(ch.kind, None);
        assert_eq!(ch.inserts[0].section, None);

        let ch = p.channel_mut(Some(t)).unwrap();
        ch.set_kind(Some(StripKind::BassDi));
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""kind":"BassDi""#) && json.contains(r#""section":"Effects""#));
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn hardware_output_round_trips() {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Audio);
        p.track_mut(t).unwrap().channel.output = Output::Hardware { first: 2, stereo: false };
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains(r#""Hardware":{"first":2,"stereo":false}"#));
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn click_groups_are_left_out_when_default() {
        let mut p = Project::new("Test", 48000);
        p.sig_changes.push(SigChange { bar: 4, num: 4, den: 4, groups: vec![] });
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("groups"));
        p.sig_groups = vec![3, 3];
        p.sig_changes[0].groups = vec![2, 2];
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
        let c: SigChange = serde_json::from_str(r#"{"bar":2,"num":7,"den":8}"#).unwrap();
        assert!(c.groups.is_empty());
    }

    #[test]
    fn freeze_key_survives_save_and_load() {
        let mut p = Project::new("Test", 48000);
        audio_region(&mut p, 100, 1000);
        p.tempo = 60e6 / 500001.0;
        p.tempo_changes.push(TempoChange { tick: PPQ as u64 * 4, tempo: 118.86026767126735 });
        let k = freeze_key(&p, &p.tracks[0]);
        let dir = std::env::temp_dir().join(format!("solstice-model-{}-freeze", std::process::id()));
        let bundle = Bundle::new(&dir);
        bundle.save(&p).unwrap();
        let loaded = bundle.load().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(freeze_key(&loaded, &loaded.tracks[0]), k);
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
    fn region_colours_follow_copies_and_skip_the_freeze_key() {
        let mut p = Project::new("Test", 48000);
        let a = audio_region(&mut p, 100, 1000);
        let plain = serde_json::to_value(&p.find_region(a).unwrap().1).unwrap();
        assert!(plain.get("color").is_none());
        assert_eq!(serde_json::from_value::<Region>(plain).unwrap().color, None);
        let key = freeze_key(&p, &p.tracks[0]);
        p.region_mut(a).unwrap().color = Some(3);
        assert_eq!(freeze_key(&p, &p.tracks[0]), key);

        let right = p.split_region(a, 600).unwrap();
        assert_eq!(p.find_region(right).unwrap().1.color, Some(3));
        let copy = p.repeat_regions(&[right])[0];
        assert_eq!(p.find_region(copy).unwrap().1.color, Some(3));
        p.region_mut(right).unwrap().color = Some(5);
        let joined = p.join_regions(&[right, a]).unwrap();
        assert_eq!(p.find_region(joined).unwrap().1.color, Some(3));
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
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
    fn old_projects_load_at_the_current_format_and_newer_ones_are_refused() {
        let dir = std::env::temp_dir().join(format!("solstice-format-{}", std::process::id()));
        let b = Bundle::new(&dir);
        b.save(&Project::new("A", 48000)).unwrap();
        let path = dir.join("project.json");
        let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v.as_object_mut().unwrap().remove("format");
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert_eq!(b.load().unwrap().format, FORMAT);
        v["format"] = (FORMAT + 1).into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(b.load().unwrap_err().to_string().contains("newer version"));
        std::fs::write(&path, b"[]").unwrap();
        assert!(b.load().unwrap_err().to_string().contains("damaged"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn write_wav(path: &Path, channels: u16, rate: u32, frames: u32, data_size: Option<u32>) {
        let block = channels as u32 * 4;
        let bytes = frames * block;
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(4 + 8 + 16 + 8 + 4 + 8 + bytes).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&3u16.to_le_bytes());
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * block).to_le_bytes());
        w.extend_from_slice(&(block as u16).to_le_bytes());
        w.extend_from_slice(&32u16.to_le_bytes());
        // An odd-sized chunk before the data, which RIFF pads to an even length.
        w.extend_from_slice(b"LIST");
        w.extend_from_slice(&3u32.to_le_bytes());
        w.extend_from_slice(&[1, 2, 3, 0]);
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_size.unwrap_or(bytes).to_le_bytes());
        w.resize(w.len() + bytes as usize, 0);
        std::fs::write(path, w).unwrap();
    }

    #[test]
    fn a_bundle_that_was_never_saved_opens_from_its_recordings() {
        let dir = std::env::temp_dir().join(format!("solstice-rebuild-{}.solstice", std::process::id()));
        let b = Bundle::new(&dir);
        std::fs::create_dir_all(b.audio_dir()).unwrap();
        assert!(!Bundle::is_bundle(&dir) && Bundle::is_openable(&dir));
        write_wav(&b.audio_path("Take 1.wav"), 1, 96000, 1000, None);
        write_wav(&b.audio_path("Take 2.wav"), 2, 96000, 500, Some(0));
        std::fs::write(b.audio_path("notes.txt"), b"not audio").unwrap();
        let p = b.load().unwrap();
        assert_eq!((p.sample_rate, p.format, p.tracks.len()), (96000, FORMAT, 2));
        let t = &p.tracks[0];
        assert_eq!((t.name.as_str(), t.stereo, t.regions[0].length), ("Take 1", false, 1000));
        assert_eq!((p.tracks[1].stereo, p.tracks[1].regions[0].length), (true, 500));
        let Content::Audio(a) = &t.regions[0].content else { panic!() };
        assert_eq!(a.file, "Take 1.wav");
        assert_ne!(t.regions[0].id, p.tracks[1].regions[0].id);

        let mut auto = Project::new("Autosaved", 44100);
        auto.add_track(TrackKind::Instrument);
        b.autosave(&auto).unwrap();
        assert_eq!(b.load().unwrap().name, "Autosaved");
        assert!(b.recoverable().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sample_rate_changes_keep_bars_and_refuse_audio() {
        let mut p = Project::new("A", 48000);
        let bar = p.time().bar_len();
        p.cycle = Range::new(bar, bar * 3);
        p.markers.push(Marker { id: 1, pos: bar * 2, name: "M".into() });
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "R".into(),
            start: bar,
            length: bar,
            muted: false,
            loops: 0,
            color: None,
            content: Content::Midi(MidiContent::default()),
        });
        assert!(p.set_sample_rate(96000));
        let bar96 = p.time().bar_len();
        assert_eq!(bar96, bar * 2);
        assert_eq!((p.cycle.start, p.cycle.end, p.markers[0].pos), (bar96, bar96 * 3, bar96 * 2));
        let r = &p.tracks[0].regions[0];
        assert_eq!((r.start, r.length), (bar96, bar96));

        let a = p.add_track(TrackKind::Audio);
        let id = p.alloc_id();
        p.track_mut(a).unwrap().regions.push(Region {
            id,
            name: "A".into(),
            start: 0,
            length: 10,
            muted: false,
            loops: 0,
            color: None,
            content: Content::Audio(AudioContent { file: "a.wav".into(), offset: 0, gain_db: 0.0, fade_in: 0, fade_out: 0, takes: Vec::new(), comp: Vec::new() }),
        });
        assert!(p.has_audio());
        assert!(!p.set_sample_rate(44100));
        assert_eq!(p.sample_rate, 96000);
        assert!(p.set_sample_rate(96000));
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
        p.buses[1].channel.sends.push(Send { bus: a, level_db: 0.0, mode: SendMode::PostPan, pan: None });
        let order = p.bus_order().unwrap();
        let pos = |i: usize| order.iter().position(|x| *x == i).unwrap();
        assert!(pos(1) < pos(0) && pos(0) < pos(2));
        assert!(p.would_loop(c, b));
        assert!(!p.would_loop(a, c));
        let t = p.add_track(TrackKind::Audio);
        assert!(!p.would_loop(t, a));
    }

    #[test]
    fn cue_mix_starts_from_the_mix() {
        let mut p = Project::new("Test", 48000);
        let a = p.add_track(TrackKind::Audio);
        let b = p.add_track(TrackKind::Instrument);
        let r = p.add_track(TrackKind::Audio);
        p.track_mut(r).unwrap().reference = true;
        let ch = &mut p.track_mut(a).unwrap().channel;
        ch.volume_db = -4.5;
        ch.pan = 0.3;
        p.track_mut(b).unwrap().channel.sends.push(Send { bus: 99, level_db: 0.0, mode: SendMode::PostPan, pan: None });
        let cue = p.add_cue_mix("Drummer".into(), 4);
        let bus = p.buses.iter().find(|x| x.id == cue).unwrap();
        assert!(bus.cue && bus.name == "Drummer");
        assert_eq!(bus.channel.output, Output::Hardware { first: 4, stereo: true });
        assert_eq!(p.track(a).unwrap().channel.sends, [Send { bus: cue, level_db: -4.5, mode: SendMode::PreFader, pan: Some(0.3) }]);
        assert_eq!(p.track(b).unwrap().channel.sends[1], Send { bus: cue, level_db: 0.0, mode: SendMode::PreFader, pan: Some(0.0) });
        assert!(p.track(r).unwrap().channel.sends.is_empty());
        p.buses[0].channel.solo = true;
        assert!(!p.any_solo());

        let old = serde_json::to_string(&p.buses[0]).unwrap().replace(",\"cue\":true", "");
        assert!(!serde_json::from_str::<Bus>(&old).unwrap().cue);
        let plain = p.add_bus();
        let json = serde_json::to_string(p.buses.iter().find(|x| x.id == plain).unwrap()).unwrap();
        assert!(!json.contains("cue"));
        let send = serde_json::to_string(&Send { bus: 1, level_db: 0.0, mode: SendMode::PostPan, pan: None }).unwrap();
        assert!(!send.contains("pan"));
    }

    #[test]
    fn split_midi() {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        let beat = p.time().ticks_to_samples(PPQ as u64);
        let notes = vec![
            Note::new(0, PPQ * 2, 60, 100),
            Note::new(PPQ * 2, PPQ, 62, 100),
        ];
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: "m".into(),
            start: 0,
            length: beat * 4,
            muted: false,
            loops: 0,
            color: None,
            content: Content::Midi(MidiContent { notes, ..Default::default() }),
        });
        let right = p.split_region(id, beat).unwrap();
        let (_, l) = p.find_region(id).unwrap();
        let Content::Midi(lm) = &l.content else { panic!() };
        assert_eq!(lm.notes, vec![Note::new(0, PPQ, 60, 100)]);
        let (_, r) = p.find_region(right).unwrap();
        let Content::Midi(rm) = &r.content else { panic!() };
        assert_eq!(rm.notes, vec![Note::new(PPQ, PPQ, 62, 100)]);
    }

    fn note(start: u32, pitch: u8) -> Note {
        Note::new(start, 10, pitch, 100)
    }

    fn midi_take(name: &str, notes: Vec<Note>) -> MidiTake {
        MidiTake { name: name.into(), notes }
    }

    /// Takes 1 and 2, each with a note every 100 ticks; take 2 plays.
    fn midi_folder() -> MidiContent {
        let mut m = MidiContent { notes: (0..10).map(|i| note(i * 100, 60)).collect(), ..Default::default() };
        m.add_take(midi_take("Take 2", (0..10).map(|i| note(i * 100, 72)).collect()));
        m
    }

    fn midi_region(p: &mut Project, length: u64, m: MidiContent) -> Id {
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        p.track_mut(t).unwrap().regions.push(Region { id, name: "m".into(), start: 0, length, muted: false, loops: 0, color: None, content: Content::Midi(m) });
        id
    }

    fn midi_of(p: &Project, id: Id) -> &MidiContent {
        match &p.find_region(id).unwrap().1.content {
            Content::Midi(m) => m,
            _ => panic!(),
        }
    }

    #[test]
    fn midi_add_take_wraps_existing_notes() {
        let m = midi_folder();
        assert_eq!(m.takes.len(), 2);
        assert_eq!(m.takes[0].name, "Take 1");
        assert!(m.takes[0].notes.iter().all(|n| n.pitch == 60));
        assert_eq!(m.comp, vec![CompSection { start: 0, take: 1 }]);
        assert!(m.notes.iter().all(|n| n.pitch == 72) && m.notes.len() == 10);
    }

    #[test]
    fn midi_set_comp_picks_notes_by_section_start() {
        let mut m = MidiContent { notes: vec![note(499, 60)], ..Default::default() };
        m.add_take(midi_take("Take 2", vec![note(499, 72), note(500, 72)]));
        m.takes[0].notes.push(note(500, 60));
        m.set_comp(0, 500, 0, 1000);
        assert_eq!(m.comp, vec![CompSection { start: 0, take: 0 }, CompSection { start: 500, take: 1 }]);
        assert_eq!(m.notes, vec![note(499, 60), note(500, 72)]);
    }

    #[test]
    fn settle_writes_mirror_edits_back_and_keeps_other_takes() {
        let mut p = Project::new("t", 48000);
        let mut m = midi_folder();
        m.set_comp(0, 500, 0, 1000);
        let id = midi_region(&mut p, 48000, m);
        let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) else { panic!() };
        m.notes.retain(|n| n.start != 100);
        m.notes.push(note(650, 73));
        p.settle_midi_folders();
        let m = midi_of(&p, id);
        assert!(!m.takes[0].notes.iter().any(|n| n.start == 100));
        assert!(m.takes[1].notes.contains(&note(650, 73)));
        assert!(m.takes[1].notes.contains(&note(100, 72)), "notes the comp does not play are untouched");
        assert_eq!(m.takes[0].notes.len(), 9);
        assert_eq!(m.takes[1].notes.len(), 11);
        let mut mirror = m.notes.clone();
        sort_notes(&mut mirror);
        assert_eq!(mirror, m.resolved());
        let before = m.clone();
        p.settle_midi_folders();
        assert_eq!(midi_of(&p, id), &before);
    }

    #[test]
    fn last_section_is_open_ended() {
        let mut m = midi_folder();
        m.set_comp(500, 1000, 0, 1000);
        m.takes[0].notes.push(note(5000, 61));
        m.resolve();
        assert!(m.notes.contains(&note(5000, 61)));
        m.notes.push(note(6000, 62));
        m.settle();
        assert!(m.takes[0].notes.contains(&note(6000, 62)));
    }

    #[test]
    fn split_midi_folder_splits_takes_and_comp() {
        let mut p = Project::new("t", 48000);
        let tpb = p.time().ticks_to_samples(PPQ as u64);
        let mut m = MidiContent { notes: vec![note(0, 60), note(PPQ * 3, 60)], ..Default::default() };
        m.add_take(midi_take("Take 2", vec![note(0, 72), note(PPQ * 3, 72)]));
        m.set_comp(0, PPQ as u64, 0, PPQ as u64 * 4);
        let id = midi_region(&mut p, tpb * 4, m);
        let right = p.split_region(id, tpb * 2).unwrap();
        let (l, r) = (midi_of(&p, id), midi_of(&p, right));
        assert_eq!(l.comp, vec![CompSection { start: 0, take: 0 }, CompSection { start: PPQ as u64, take: 1 }]);
        assert_eq!(l.takes[1].notes, vec![note(0, 72)]);
        assert_eq!(r.comp, vec![CompSection { start: 0, take: 1 }]);
        assert_eq!(r.takes[0].notes, vec![note(PPQ, 60)]);
        assert_eq!(r.takes[1].notes, vec![note(PPQ, 72)]);
        assert_eq!(r.notes, vec![note(PPQ, 72)]);
        assert_eq!(l.notes, vec![note(0, 60)]);
    }

    #[test]
    fn split_midi_uses_tempo_map() {
        let mut p = Project::new("t", 48000);
        let bar = 4 * PPQ;
        p.tempo_changes.push(TempoChange { tick: bar as u64, tempo: 60.0 });
        let mut m = MidiContent::default();
        m.notes = vec![note(bar + PPQ, 60)];
        let id = midi_region(&mut p, 48000 * 10, m);
        let r = p.find_region(id).unwrap().1.clone();
        // A bar at 120 lasts 2 s, so the cut a second into the 60 bpm bar lands a beat in.
        let right = p.split_region(id, r.start + 48000 * 3).unwrap();
        assert_eq!(midi_of(&p, right).notes, vec![note(0, 60)]);
    }

    #[test]
    fn trim_start_shifts_midi_takes() {
        let mut m = midi_folder();
        m.set_comp(500, 1000, 0, 1000);
        m.controllers = vec![Controller { tick: 50, number: 1, value: 3 }, Controller { tick: 250, number: 1, value: 4 }];
        m.shift_ticks(200);
        assert_eq!(m.comp, vec![CompSection { start: 0, take: 1 }, CompSection { start: 300, take: 0 }]);
        assert_eq!(m.takes[0].notes.first(), Some(&note(0, 60)));
        assert_eq!(m.takes[1].notes.len(), 8);
        assert_eq!(m.controllers, vec![Controller { tick: 50, number: 1, value: 4 }]);
        assert_eq!(m.notes, m.resolved());
    }

    #[test]
    fn flatten_keeps_comp() {
        let mut m = midi_folder();
        m.set_comp(0, 300, 0, 1000);
        let comp = m.notes.clone();
        m.flatten();
        assert!(!m.is_folder() && m.comp.is_empty());
        assert_eq!(m.notes, comp);
        assert_eq!(m.notes.iter().filter(|n| n.pitch == 60).count(), 3);
    }

    #[test]
    fn old_midi_json_loads() {
        let json = r#"{"notes":[{"start":0,"length":10,"pitch":60,"velocity":100}]}"#;
        let m: MidiContent = serde_json::from_str(json).unwrap();
        assert_eq!(m.notes, vec![note(0, 60)]);
        assert!(!m.is_folder() && m.controllers.is_empty());
    }

    #[test]
    fn plain_midi_serialises_without_take_fields() {
        let m = MidiContent { notes: vec![note(0, 60)], ..Default::default() };
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("takes") && !json.contains("comp"));
        let f = midi_folder();
        assert_eq!(serde_json::from_str::<MidiContent>(&serde_json::to_string(&f).unwrap()).unwrap(), f);
    }

    #[test]
    fn notes_from_events_closes_held_notes_and_ignores_orphan_offs() {
        let time = TimeMap::new(48000, 120.0, 4, 4);
        let beat = time.beat_len();
        let events = [
            (beat, [0x80, 64, 0]),
            (beat, [0x90, 60, 90]),
            (beat * 2, [0x90, 60, 0]),
            (beat * 3, [0x90, 62, 80]),
            (beat * 3, [0xB0, 1, 10]),
        ];
        let notes = notes_from_events(&time, 0, beat * 4, &events);
        assert_eq!(
            notes,
            vec![Note::new(PPQ, PPQ, 60, 90), Note::new(PPQ * 3, PPQ, 62, 80)]
        );
        // A note-off earlier than its note-on, as after a cycle wrap, does not underflow.
        let wrapped = notes_from_events(&time, 0, beat * 4, &[(beat * 3, [0x90, 60, 90]), (beat, [0x80, 60, 0])]);
        assert_eq!(wrapped[0].length, 1);
    }

    #[test]
    fn audio_set_comp_unchanged_after_recomp_refactor() {
        let mut a = folder();
        a.set_comp(100, 300, 0, 1000);
        a.set_comp(200, 900, 1, 1000);
        a.set_comp(950, 2000, 0, 1000);
        a.set_comp(500, 400, 1, 1000);
        assert_eq!(
            a.comp,
            vec![
                CompSection { start: 0, take: 2 },
                CompSection { start: 100, take: 0 },
                CompSection { start: 200, take: 1 },
                CompSection { start: 900, take: 2 },
                CompSection { start: 950, take: 0 },
            ]
        );
        a.shift_start(150);
        assert_eq!(a.comp, vec![CompSection { start: 0, take: 0 }, CompSection { start: 50, take: 1 }, CompSection { start: 750, take: 2 }, CompSection { start: 800, take: 0 }]);
        assert_eq!(a.file, "t1.wav");
    }

    fn kick_pattern() -> Pattern {
        let mut row = PatternRow::new(36);
        for i in [0, 4, 8, 12] {
            row.set(Step::new(i, 100));
        }
        Pattern { rows: vec![row], ..Pattern::default() }
    }

    /// A one-bar region holding `kick_pattern`.
    fn pattern_region(p: &mut Project) -> Id {
        let bar = p.time().bar_len();
        let mut m = MidiContent::default();
        m.set_pattern(kick_pattern(), 4 * PPQ);
        midi_region(p, bar, m)
    }

    #[test]
    fn settle_rebakes_a_pattern_over_its_new_span() {
        let mut p = Project::new("Test", 48000);
        let id = pattern_region(&mut p);
        assert_eq!(p.settle_patterns(), 0);
        assert_eq!(midi_of(&p, id).notes.len(), 4);
        p.region_mut(id).unwrap().length *= 2;
        assert_eq!(p.settle_patterns(), 0);
        let m = midi_of(&p, id);
        assert_eq!((m.notes.len(), m.pattern.as_ref().unwrap().span), (8, 8 * PPQ));
        // At half the tempo the same samples hold half the ticks.
        p.tempo = 60.0;
        assert_eq!(p.settle_patterns(), 0);
        assert_eq!(midi_of(&p, id).notes.len(), 4);
        let before = p.clone();
        p.settle_patterns();
        assert_eq!(p, before);
    }

    #[test]
    fn settle_drops_a_pattern_whose_notes_were_edited() {
        let mut p = Project::new("Test", 48000);
        let id = pattern_region(&mut p);
        let Content::Midi(m) = &mut p.region_mut(id).unwrap().content else { panic!() };
        for n in &mut m.notes {
            n.pitch += 2;
        }
        let transposed = m.notes.clone();
        assert_eq!(p.settle_patterns(), 1);
        let m = midi_of(&p, id);
        assert!(m.pattern.is_none());
        assert_eq!(m.notes, transposed);

        let id = pattern_region(&mut p);
        let Content::Midi(m) = &mut p.region_mut(id).unwrap().content else { panic!() };
        m.shift_ticks(PPQ as i64);
        assert_eq!(p.settle_patterns(), 1);
        assert_eq!(midi_of(&p, id).notes.len(), 3);

        // Controller edits leave the notes, and so the pattern, alone.
        let id = pattern_region(&mut p);
        let Content::Midi(m) = &mut p.region_mut(id).unwrap().content else { panic!() };
        m.controllers.push(Controller { tick: 0, number: 1, value: 64 });
        assert_eq!(p.settle_patterns(), 0);
        assert!(midi_of(&p, id).pattern.is_some());
    }

    #[test]
    fn settle_drops_a_pattern_that_became_a_take_folder() {
        let mut p = Project::new("Test", 48000);
        let id = pattern_region(&mut p);
        let Content::Midi(m) = &mut p.region_mut(id).unwrap().content else { panic!() };
        let notes = m.notes.clone();
        m.add_take(midi_take("Take 2", notes));
        assert_eq!(p.settle_patterns(), 1);
    }

    #[test]
    fn split_and_join_keep_the_pattern_only_on_the_left() {
        let mut p = Project::new("Test", 48000);
        let id = pattern_region(&mut p);
        let half = p.time().bar_len() / 2;
        let right = p.split_region(id, half).unwrap();
        assert!(midi_of(&p, right).pattern.is_none());
        assert_eq!(midi_of(&p, right).notes.len(), 2);
        assert_eq!(p.settle_patterns(), 0);
        let left = midi_of(&p, id);
        assert_eq!((left.notes.len(), left.pattern.as_ref().unwrap().span), (2, 2 * PPQ));
        let joined = p.join_regions(&[id, right]).unwrap();
        assert!(midi_of(&p, joined).pattern.is_none());
        assert_eq!(midi_of(&p, joined).notes.len(), 4);
    }

    #[test]
    fn older_projects_load_and_new_fields_save_only_when_set() {
        let mut p = Project::new("Test", 48000);
        midi_region(&mut p, 1000, MidiContent { notes: vec![Note::new(0, 10, 60, 100)], ..Default::default() });
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("chance") && !json.contains("pattern") && !json.contains("drum_map"));
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);

        pattern_region(&mut p);
        let t = &mut p.tracks[0];
        t.drum_map.push(DrumRow { pitch: 36, name: "Kick".into(), hidden: false, mute: true, solo: false });
        let Content::Midi(m) = &mut t.regions[0].content else { panic!() };
        m.notes[0].chance = 30;
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("chance") && json.contains("pattern") && json.contains("drum_map"));
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn a_project_saved_before_patterns_loads() {
        let json = r#"{
            "name": "Old", "sample_rate": 48000, "tempo": 120.0, "sig_num": 4, "sig_den": 4,
            "tracks": [{
                "id": 1, "name": "Drums", "kind": "Instrument", "color": 0,
                "channel": {"volume_db": 0.0, "pan": 0.0, "mute": false, "solo": false, "inserts": [], "sends": [], "output": "Master"},
                "input": 0, "stereo": true, "armed": false, "monitor": false, "instrument": null, "midi_fx": [],
                "regions": [{
                    "id": 2, "name": "Beat", "start": 0, "length": 96000, "muted": false, "loops": 0,
                    "content": {"Midi": {"notes": [
                        {"start": 0, "length": 240, "pitch": 36, "velocity": 100},
                        {"start": 480, "length": 240, "pitch": 38, "velocity": 90}
                    ]}}
                }]
            }],
            "buses": [],
            "master": {"volume_db": 0.0, "pan": 0.0, "mute": false, "solo": false, "inserts": [], "sends": [], "output": "Master"},
            "cycle": {"start": 0, "end": 384000}, "cycle_on": false,
            "punch": {"start": 192000, "end": 384000}, "punch_on": false,
            "metronome": false, "next_id": 3
        }"#;
        let mut p: Project = serde_json::from_str(json).unwrap();
        assert!(p.tracks[0].drum_map.is_empty());
        let m = midi_of(&p, 2);
        assert!(m.pattern.is_none());
        assert_eq!(m.notes, vec![Note::new(0, 240, 36, 100), Note::new(480, 240, 38, 90)]);
        assert!(m.notes.iter().all(|n| n.chance == 100));
        assert_eq!(p.settle_patterns(), 0);
    }

    #[test]
    fn drum_map_mute_and_solo() {
        let mut p = Project::new("Test", 48000);
        p.add_track(TrackKind::Instrument);
        let t = &mut p.tracks[0];
        assert!(t.plays_pitch(36) && t.plays_pitch(38));
        let row = |pitch, mute, solo| DrumRow { pitch, name: String::new(), hidden: false, mute, solo };
        t.drum_map = vec![row(36, true, false), row(38, false, false)];
        assert!(!t.plays_pitch(36) && t.plays_pitch(38) && t.plays_pitch(40));
        t.drum_map.push(row(42, false, true));
        assert!(!t.plays_pitch(38) && !t.plays_pitch(40) && t.plays_pitch(42));
        t.drum_map[2].mute = true;
        assert!(!t.plays_pitch(42));
    }

    #[test]
    fn freeze_key_follows_only_drum_mute_and_solo() {
        let mut p = Project::new("Test", 48000);
        p.add_track(TrackKind::Instrument);
        let key = |p: &Project| freeze_key(p, &p.tracks[0]);
        let k = key(&p);
        let row = |pitch, name: &str| DrumRow { pitch, name: name.into(), hidden: false, mute: false, solo: false };
        p.tracks[0].drum_map = vec![row(38, "Snare"), row(36, "Kick")];
        assert_eq!(key(&p), k);
        p.tracks[0].drum_map[0].hidden = true;
        p.tracks[0].drum_map[1].name = "Bass Drum".into();
        p.tracks[0].drum_map.swap(0, 1);
        assert_eq!(key(&p), k);
        p.tracks[0].drum_map[0].mute = true;
        let muted = key(&p);
        assert_ne!(muted, k);
        p.tracks[0].drum_map.swap(0, 1);
        p.tracks[0].drum_map[0].name.clear();
        assert_eq!(key(&p), muted);
    }

    #[test]
    fn alternatives_new_switch_rename_and_delete() {
        let mut p = Project::new("T", 48000);
        let r = midi_region(&mut p, 1000, MidiContent::default());
        let t = p.tracks[0].id;
        assert_eq!(p.tracks[0].alternative_name(), "A");
        assert!(p.new_alternative(t, false));
        let tr = p.track(t).unwrap();
        assert!(tr.regions.is_empty());
        assert_eq!((tr.alternative.as_str(), tr.alternatives[0].name.as_str()), ("B", "A"));
        assert_eq!(tr.alternatives[0].regions[0].id, r);
        assert!(p.find_region(r).is_none());

        assert!(p.new_alternative(t, false));
        let names: Vec<String> = p.track(t).unwrap().alternative_names().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert!(p.switch_alternative(t, 0));
        let tr = p.track(t).unwrap();
        assert_eq!(tr.alternative, "A");
        assert_eq!(tr.regions[0].id, r);
        assert_eq!(tr.alternatives.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(), ["B", "C"]);

        assert!(!p.rename_alternative(t, "B"));
        assert!(p.rename_alternative(t, "Verse"));
        assert_eq!(p.track(t).unwrap().alternative_name(), "Verse");
        assert!(p.new_alternative(t, false));
        assert_eq!(p.track(t).unwrap().alternative, "A");

        assert!(p.delete_alternative(t));
        let tr = p.track(t).unwrap();
        assert_eq!(tr.alternative, "B");
        assert_eq!(tr.alternatives.len(), 2);
        assert!(p.delete_alternative(t));
        assert!(p.delete_alternative(t));
        let tr = p.track(t).unwrap();
        assert_eq!((tr.alternative.as_str(), tr.regions[0].id), ("Verse", r));
        assert!(!p.delete_alternative(t));
    }

    #[test]
    fn duplicate_alternative_gets_fresh_ids() {
        let mut p = Project::new("T", 48000);
        let r = midi_region(&mut p, 1000, MidiContent::default());
        let t = p.tracks[0].id;
        assert!(p.new_alternative(t, true));
        let tr = p.track(t).unwrap();
        let copy = tr.regions[0].id;
        assert_ne!(copy, r);
        assert_eq!(tr.alternatives[0].regions[0].id, r);
        assert_eq!(Region { id: r, ..tr.regions[0].clone() }, tr.alternatives[0].regions[0]);
        assert!(p.alloc_id() > copy);
    }

    #[test]
    fn duplicate_track_gives_alternatives_fresh_ids() {
        let mut p = Project::new("T", 48000);
        let r = midi_region(&mut p, 1000, MidiContent::default());
        let t = p.tracks[0].id;
        p.new_alternative(t, false);
        let copy = p.duplicate_track(t).unwrap();
        let c = p.track(copy).unwrap();
        assert_eq!(c.alternative, "B");
        assert_ne!(c.alternatives[0].regions[0].id, r);
    }

    #[test]
    fn alternatives_save_load_and_old_projects_have_one() {
        let mut p = Project::new("T", 48000);
        midi_region(&mut p, 1000, MidiContent::default());
        let t = p.tracks[0].id;
        let plain = serde_json::to_string(&p).unwrap();
        assert!(!plain.contains("alternative"));
        let old: Project = serde_json::from_str(&plain).unwrap();
        assert!(old.tracks[0].alternatives.is_empty() && old.tracks[0].alternative.is_empty());
        p.new_alternative(t, true);
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<Project>(&json).unwrap(), p);
    }

    #[test]
    fn freeze_key_ignores_inactive_alternatives() {
        let mut p = Project::new("T", 48000);
        midi_region(&mut p, 1000, MidiContent::default());
        let t = p.tracks[0].id;
        let k = freeze_key(&p, &p.tracks[0]);
        p.new_alternative(t, false);
        p.switch_alternative(t, 0);
        assert_eq!(freeze_key(&p, &p.tracks[0]), k);
    }
}
