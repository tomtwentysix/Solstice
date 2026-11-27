use crate::bank::{Bank, ParamDef};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Decoded audio at the engine's sample rate.
pub struct SampleData {
    pub channels: Vec<Vec<f32>>,
}

impl SampleData {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }
}

/// A zone as saved and edited: everything except the audio itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneDesc {
    pub name: String,
    pub path: PathBuf,
    pub root: u8,
    pub lo: u8,
    pub hi: u8,
    pub vel_lo: u8,
    pub vel_hi: u8,
    /// Cents.
    pub tune: f32,
    pub gain_db: f32,
    /// -1..1.
    pub pan: f32,
    pub loop_points: Option<(usize, usize)>,
    /// Plays to the end regardless of note-off, as drum hits do.
    pub one_shot: bool,
    /// Notes in the same non-zero group cut each other off (closed and open hi-hat).
    pub choke: u8,
}

impl ZoneDesc {
    pub fn new(path: impl Into<PathBuf>, root: u8) -> ZoneDesc {
        let path = path.into();
        let name = path.file_stem().map_or("Sample".into(), |s| s.to_string_lossy().into_owned());
        ZoneDesc {
            name,
            path,
            root,
            lo: root,
            hi: root,
            vel_lo: 1,
            vel_hi: 127,
            tune: 0.0,
            gain_db: 0.0,
            pan: 0.0,
            loop_points: None,
            one_shot: false,
            choke: 0,
        }
    }

    fn line(&self) -> String {
        let (ls, le) = self.loop_points.map_or((String::new(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.path.display(),
            self.root,
            self.lo,
            self.hi,
            self.vel_lo,
            self.vel_hi,
            self.tune,
            self.gain_db,
            self.pan,
            ls,
            le,
            self.one_shot as u8,
            self.choke,
            self.name.replace('|', "/")
        )
    }

    fn parse(line: &str) -> Option<ZoneDesc> {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 14 {
            return None;
        }
        let loop_points = match (f[9].parse(), f[10].parse()) {
            (Ok(a), Ok(b)) => Some((a, b)),
            _ => None,
        };
        Some(ZoneDesc {
            path: PathBuf::from(f[0]),
            root: f[1].parse().ok()?,
            lo: f[2].parse().ok()?,
            hi: f[3].parse().ok()?,
            vel_lo: f[4].parse().ok()?,
            vel_hi: f[5].parse().ok()?,
            tune: f[6].parse().ok()?,
            gain_db: f[7].parse().ok()?,
            pan: f[8].parse().ok()?,
            loop_points,
            one_shot: f[11] == "1",
            choke: f[12].parse().ok()?,
            name: f[13..].join("|"),
        })
    }
}

/// Saved state for sample instruments: the parameter bank, then one line per zone.
pub fn state(bank: &Bank, zones: &[ZoneDesc]) -> Vec<u8> {
    let mut s = String::from_utf8(bank.state()).unwrap_or_default();
    s.push_str("#zones\n");
    for z in zones {
        s.push_str(&z.line());
        s.push('\n');
    }
    s.into_bytes()
}

/// Restores the bank and returns the zones, whose audio the caller loads.
pub fn set_state(bank: &Bank, data: &[u8]) -> Vec<ZoneDesc> {
    let text = String::from_utf8_lossy(data);
    let (params, zones) = text.split_once("#zones\n").unwrap_or((&text, ""));
    bank.set_state(params.as_bytes());
    zones.lines().filter_map(ZoneDesc::parse).collect()
}

#[derive(Clone)]
pub struct Zone {
    pub desc: ZoneDesc,
    pub sample: Arc<SampleData>,
}

/// The zones a sample instrument plays, shared with the RT thread. Replaced maps are kept here
/// until no voice still plays from them, so the RT thread never frees one.
#[derive(Default)]
pub struct SampleMap {
    current: Mutex<Arc<Vec<Zone>>>,
    retired: Mutex<Vec<Arc<Vec<Zone>>>>,
}

impl SampleMap {
    pub fn set(&self, zones: Vec<Zone>) {
        let new = Arc::new(zones);
        let old = std::mem::replace(&mut *self.current.lock().unwrap(), new);
        let mut retired = self.retired.lock().unwrap();
        retired.push(old);
        retired.retain(|m| Arc::strong_count(m) > 1);
    }

    pub fn zones(&self) -> Arc<Vec<Zone>> {
        self.current.lock().unwrap().clone()
    }

    pub fn descs(&self) -> Vec<ZoneDesc> {
        self.zones().iter().map(|z| z.desc.clone()).collect()
    }

    /// For the RT thread: the current map if it isn't being swapped right now.
    fn try_zones(&self) -> Option<Arc<Vec<Zone>>> {
        self.current.try_lock().ok().map(|m| m.clone())
    }
}

pub static SAMPLER: [ParamDef; 4] = [
    ParamDef::new(0, "Attack", 0.5, 2000.0, 1.0, "ms").log(),
    ParamDef::new(1, "Release", 1.0, 5000.0, 150.0, "ms").log(),
    ParamDef::new(2, "Volume", -40.0, 6.0, 0.0, "dB"),
    ParamDef::new(3, "Velocity", 0.0, 100.0, 80.0, "%"),
];

const VOICES: usize = 32;

/// Eight octaves either way; SFZ transpose and tune are unbounded text, and "nan" parses.
fn clamp_cents(c: f32) -> f32 {
    if c.is_finite() { c.clamp(-9600.0, 9600.0) } else { 0.0 }
}

#[derive(Clone)]
struct Voice {
    map: Option<Arc<Vec<Zone>>>,
    zone: usize,
    note: u8,
    pos: f64,
    step: f64,
    gains: [f32; 2],
    level: f32,
    releasing: bool,
    held: bool,
    age: u64,
}

impl Voice {
    fn active(&self) -> bool {
        self.map.is_some()
    }
}

/// Key- and velocity-mapped sample playback, used by both the Sampler and the Drum Kit.
pub struct Sampler {
    bank: Arc<Bank>,
    map: Arc<SampleMap>,
    current: Arc<Vec<Zone>>,
    sr: f32,
    voices: Vec<Voice>,
    clock: u64,
    sustain: bool,
}

impl Sampler {
    pub fn new(bank: Arc<Bank>, map: Arc<SampleMap>, sr: f32) -> Self {
        let current = map.zones();
        let idle = Voice { map: None, zone: 0, note: 0, pos: 0.0, step: 0.0, gains: [0.0; 2], level: 0.0, releasing: false, held: false, age: 0 };
        Sampler { bank, map, current, sr, voices: vec![idle; VOICES], clock: 0, sustain: false }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    fn note_on(&mut self, note: u8, vel: u8) {
        let zones = self.current.clone();
        let vel_amount = self.bank.get(3) / 100.0;
        let tune = 0.0;
        for (zi, z) in zones.iter().enumerate() {
            let d = &z.desc;
            if note < d.lo || note > d.hi || vel < d.vel_lo || vel > d.vel_hi {
                continue;
            }
            if d.choke != 0 {
                for v in self.voices.iter_mut().filter(|v| v.active()) {
                    let same = v.map.as_ref().and_then(|m| m.get(v.zone)).is_some_and(|o| o.desc.choke == d.choke);
                    if same {
                        v.releasing = true;
                        v.held = false;
                    }
                }
            }
            self.clock += 1;
            let i = self
                .voices
                .iter()
                .position(|v| !v.active())
                .unwrap_or_else(|| (0..VOICES).min_by_key(|i| self.voices[*i].age).unwrap());
            let semis = note as f64 - d.root as f64 + clamp_cents(d.tune + tune) as f64 / 100.0;
            let vel_gain = 1.0 - vel_amount + vel_amount * vel as f32 / 127.0;
            let g = 10f32.powf(d.gain_db / 20.0) * vel_gain;
            let pan = d.pan.clamp(-1.0, 1.0);
            self.voices[i] = Voice {
                map: Some(zones.clone()),
                zone: zi,
                note,
                pos: 0.0,
                step: 2f64.powf(semis / 12.0),
                gains: [g * (1.0 - pan.max(0.0)), g * (1.0 + pan.min(0.0))],
                level: 0.0,
                releasing: false,
                held: true,
                age: self.clock,
            };
        }
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.active() && v.note == note && v.held) {
            v.held = false;
            let one_shot = v.map.as_ref().and_then(|m| m.get(v.zone)).is_some_and(|z| z.desc.one_shot);
            if !self.sustain && !one_shot {
                v.releasing = true;
            }
        }
    }
}

impl Plugin for Sampler {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], events: &[MidiEvent], _: &ProcessContext) {
        if let Some(z) = self.map.try_zones()
            && !Arc::ptr_eq(&z, &self.current)
        {
            self.current = z;
        }
        let attack = 1.0 / (self.bank.get(0) * 0.001 * self.sr).max(1.0);
        let release = 1.0 / (self.bank.get(1) * 0.001 * self.sr).max(1.0);
        let volume = 10f32.powf(self.bank.get(2) / 20.0);
        let mut next = 0;
        for i in 0..l.len() {
            while next < events.len() && events[next].frame as usize <= i {
                let d = events[next].data;
                match d[0] & 0xf0 {
                    0x90 if d[2] > 0 => self.note_on(d[1], d[2]),
                    0x80 | 0x90 => self.note_off(d[1]),
                    0xB0 if d[1] == 64 => {
                        self.sustain = d[2] >= 64;
                        if !self.sustain {
                            for v in self.voices.iter_mut().filter(|v| v.active() && !v.held) {
                                v.releasing = true;
                            }
                        }
                    }
                    0xB0 if d[1] == 120 || d[1] == 123 => {
                        for v in self.voices.iter_mut() {
                            v.releasing = true;
                        }
                    }
                    _ => {}
                }
                next += 1;
            }
            let (mut ol, mut or) = (0.0, 0.0);
            for v in self.voices.iter_mut().filter(|v| v.active()) {
                let Some(zone) = v.map.as_ref().and_then(|m| m.get(v.zone)) else {
                    v.map = None;
                    continue;
                };
                let data = &zone.sample;
                let frames = data.frames();
                let idx = v.pos as usize;
                if !v.pos.is_finite() || idx.saturating_add(1) >= frames {
                    v.map = None;
                    continue;
                }
                let frac = (v.pos - idx as f64) as f32;
                let read = |c: usize| {
                    let ch = &data.channels[c.min(data.channels.len() - 1)];
                    ch[idx] + (ch[idx + 1] - ch[idx]) * frac
                };
                v.level = if v.releasing { (v.level - release).max(0.0) } else { (v.level + attack).min(1.0) };
                if v.releasing && v.level <= 0.0 {
                    v.map = None;
                    continue;
                }
                ol += read(0) * v.gains[0] * v.level;
                or += read(1) * v.gains[1] * v.level;
                v.pos += v.step;
                if let Some((a, b)) = zone.desc.loop_points
                    && b > a
                    && v.pos >= b as f64
                    && !v.releasing
                {
                    v.pos -= (b - a) as f64;
                }
            }
            l[i] += ol * volume;
            r[i] += or * volume;
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        self.bank.set_norm(id, value as f32);
    }
}

/// General MIDI drum notes for the Drum Kit's 16 pads, with default names and choke groups.
pub const PADS: [(u8, &str, u8); 16] = [
    (36, "Kick", 0),
    (38, "Snare", 0),
    (37, "Side Stick", 0),
    (39, "Clap", 0),
    (42, "Closed Hat", 1),
    (44, "Pedal Hat", 1),
    (46, "Open Hat", 1),
    (49, "Crash", 0),
    (41, "Low Tom", 0),
    (45, "Mid Tom", 0),
    (48, "High Tom", 0),
    (51, "Ride", 0),
    (54, "Tambourine", 0),
    (56, "Cowbell", 0),
    (70, "Shaker", 0),
    (75, "Claves", 0),
];

/// A Drum Kit zone for pad `i`: a one-shot on the pad's note.
pub fn pad_zone(i: usize, path: impl Into<PathBuf>) -> ZoneDesc {
    let (note, name, choke) = PADS[i];
    ZoneDesc { name: name.into(), one_shot: true, choke, ..ZoneDesc::new(path, note) }
}

/// Parses SFZ text into zones, resolving sample paths against `dir`. Supports the common
/// opcodes: sample, key, lokey, hikey, pitch_keycenter, lovel, hivel, tune, transpose, volume,
/// pan, loop_mode, loop_start, loop_end, and default_path, with <global> and <group> defaults.
pub fn parse_sfz(text: &str, dir: &Path) -> Vec<ZoneDesc> {
    #[derive(Clone, Default)]
    struct Ops(Vec<(String, String)>);
    impl Ops {
        fn get(&self, k: &str) -> Option<&str> {
            self.0.iter().rev().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
        }
    }
    #[derive(PartialEq)]
    enum Section {
        Global,
        Group,
        Region,
        Other,
    }
    let mut section = Section::Global;
    let mut global = Ops::default();
    let mut group = Ops::default();
    let mut region: Option<Ops> = None;
    let mut default_path = String::new();
    let mut out = Vec::new();
    let finish = |r: Ops, out: &mut Vec<ZoneDesc>, default_path: &str| {
        let Some(sample) = r.get("sample") else { return };
        let path = dir.join(default_path).join(sample.replace('\\', "/"));
        let num = |k: &str| r.get(k).and_then(note_number);
        let key = num("key");
        let root = num("pitch_keycenter").or(key).unwrap_or(60);
        let mut z = ZoneDesc::new(path, root);
        z.lo = num("lokey").or(key).unwrap_or(0);
        z.hi = num("hikey").or(key).unwrap_or(127);
        z.vel_lo = r.get("lovel").and_then(|v| v.parse().ok()).unwrap_or(1);
        z.vel_hi = r.get("hivel").and_then(|v| v.parse().ok()).unwrap_or(127);
        let transpose: f32 = r.get("transpose").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        z.tune = clamp_cents(r.get("tune").and_then(|v| v.parse().ok()).unwrap_or(0.0) + transpose * 100.0);
        z.gain_db = r.get("volume").and_then(|v| v.parse().ok()).unwrap_or(0.0);
        z.pan = r.get("pan").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) / 100.0;
        let looping = matches!(r.get("loop_mode"), Some("loop_continuous" | "loop_sustain"));
        if let (true, Some(a), Some(b)) = (looping, r.get("loop_start"), r.get("loop_end")) {
            z.loop_points = a.parse().ok().zip(b.parse().ok());
        }
        z.one_shot = r.get("loop_mode") == Some("one_shot");
        out.push(z);
    };
    for raw in text.lines() {
        let line = raw.split("//").next().unwrap_or("");
        let mut rest = line.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('<') {
                let (header, tail) = after.split_once('>').unwrap_or((after, ""));
                if let Some(r) = region.take() {
                    finish(r, &mut out, &default_path);
                }
                section = match header {
                    "global" => {
                        global = Ops::default();
                        group = Ops::default();
                        Section::Global
                    }
                    "group" | "master" => {
                        group = global.clone();
                        Section::Group
                    }
                    "region" => {
                        // Regions inherit their group, or the global defaults outside a group.
                        let base = if group.0.is_empty() { global.clone() } else { group.clone() };
                        region = Some(base);
                        Section::Region
                    }
                    _ => Section::Other,
                };
                rest = tail.trim_start();
                continue;
            }
            let Some((key, after)) = rest.split_once('=') else { break };
            let key = key.trim().to_string();
            // A value runs to the next "name=" or header; sample paths may contain spaces.
            let mut end = after.len();
            for (i, _) in after.match_indices(' ') {
                let next = after[i..].trim_start();
                let looks_like_op = next.split_once('=').is_some_and(|(k, _)| !k.is_empty() && !k.contains(' '));
                if next.starts_with('<') || looks_like_op {
                    end = i;
                    break;
                }
            }
            let value = after[..end].trim().to_string();
            rest = after[end..].trim_start();
            if key == "default_path" {
                default_path = value;
                continue;
            }
            match section {
                Section::Region => region.get_or_insert_with(Ops::default).0.push((key, value)),
                Section::Group => group.0.push((key, value)),
                Section::Global => global.0.push((key, value)),
                Section::Other => {}
            }
        }
    }
    if let Some(r) = region.take() {
        finish(r, &mut out, &default_path);
    }
    out
}

/// "60", "c4" or "C#3" to a MIDI note, with C4 = 60 as in SFZ.
pub fn note_number(s: &str) -> Option<u8> {
    if let Ok(n) = s.parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).ok();
    }
    let s = s.to_lowercase();
    let mut chars = s.chars();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, octave) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().ok()?;
    u8::try_from(octave.saturating_add(1).saturating_mul(12).saturating_add(base + acc).clamp(0, 127)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;
    const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

    fn ramp(n: usize) -> Arc<SampleData> {
        Arc::new(SampleData { channels: vec![(0..n).map(|i| i as f32 / n as f32).collect()] })
    }

    fn sampler(zones: Vec<ZoneDesc>) -> (Sampler, Arc<SampleMap>) {
        let map = Arc::new(SampleMap::default());
        map.set(zones.into_iter().map(|desc| Zone { desc, sample: ramp(48000) }).collect());
        let bank = Arc::new(Bank::new(&SAMPLER));
        bank.set(0, 0.5);
        (Sampler::new(bank, map.clone(), SR), map)
    }

    fn play(s: &mut Sampler, ev: &[MidiEvent], n: usize) -> Vec<f32> {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        s.process(&mut l, &mut r, ev, &CTX);
        l
    }

    #[test]
    fn plays_zones_at_pitch() {
        let mut z = ZoneDesc::new("a.wav", 60);
        z.lo = 48;
        z.hi = 72;
        let (mut s, _) = sampler(vec![z]);
        // An octave up plays the ramp twice as fast.
        let out = play(&mut s, &[MidiEvent { frame: 0, data: [0x90, 72, 127] }], 1000);
        assert!((out[500] - 1000.0 / 48000.0 * 0.995).abs() < 2e-3, "{}", out[500]);
        // A note outside every zone starts nothing.
        let before = s.active_voices();
        play(&mut s, &[MidiEvent { frame: 0, data: [0x90, 30, 127] }], 10);
        assert_eq!(s.active_voices(), before);
    }

    #[test]
    fn velocity_layers_and_chokes() {
        let mut soft = pad_zone(4, "closed.wav");
        soft.vel_hi = 63;
        let mut hard = pad_zone(4, "closed-hard.wav");
        hard.vel_lo = 64;
        let open = pad_zone(6, "open.wav");
        let (mut s, _) = sampler(vec![soft, hard, open]);
        play(&mut s, &[MidiEvent { frame: 0, data: [0x90, 46, 100] }], 100);
        assert_eq!(s.active_voices(), 1);
        // Closed hat chokes the open one; only one velocity layer answers.
        play(&mut s, &[MidiEvent { frame: 0, data: [0x90, 42, 30] }], 48000 / 2);
        assert_eq!(s.active_voices(), 1);
    }

    #[test]
    fn state_round_trip() {
        let bank = Bank::new(&SAMPLER);
        let mut z = ZoneDesc::new("/x/Piano C4.wav", 60);
        z.loop_points = Some((100, 2000));
        z.choke = 2;
        let data = state(&bank, &[z.clone()]);
        let back = set_state(&Bank::new(&SAMPLER), &data);
        assert_eq!(back, vec![z]);
    }

    #[test]
    fn parses_sfz() {
        let sfz = "// test\n<control> default_path=Samples/\n<global> volume=-3\n<group> lovel=1 hivel=80\n<region> sample=Soft Piano C4.wav key=c4\n<region> sample=Soft D4.wav lokey=d4 hikey=e4 pitch_keycenter=62 loop_mode=loop_continuous loop_start=10 loop_end=90\n<group>\n<region> sample=Kick.wav key=36 loop_mode=one_shot pan=-50";
        let zones = parse_sfz(sfz, Path::new("/lib"));
        assert_eq!(zones.len(), 3);
        assert_eq!(zones[0].path, PathBuf::from("/lib/Samples/Soft Piano C4.wav"));
        assert_eq!((zones[0].lo, zones[0].hi, zones[0].root, zones[0].vel_hi), (60, 60, 60, 80));
        assert_eq!(zones[0].gain_db, -3.0);
        assert_eq!((zones[1].lo, zones[1].hi, zones[1].root), (62, 64, 62));
        assert_eq!(zones[1].loop_points, Some((10, 90)));
        assert!(zones[2].one_shot);
        assert_eq!(zones[2].pan, -0.5);
        assert_eq!(zones[2].vel_hi, 127);
        assert_eq!(note_number("C#3"), Some(49));
    }

    #[test]
    fn note_number_saturates_huge_octaves() {
        assert_eq!(note_number("c999999999"), Some(127));
        assert_eq!(note_number("c-999999999"), Some(0));
    }

    #[test]
    fn extreme_tune_stays_in_bounds() {
        let zones = parse_sfz("<region> sample=a.wav key=60 transpose=1e30\n<region> sample=b.wav key=60 tune=nan", Path::new("/"));
        assert_eq!(zones[0].tune, 9600.0);
        assert_eq!(zones[1].tune, 0.0);
        for tune in [f32::INFINITY, f32::NAN, 1e30] {
            let mut z = ZoneDesc::new("a.wav", 60);
            z.tune = tune;
            let (mut s, _) = sampler(vec![z]);
            let out = play(&mut s, &[MidiEvent { frame: 0, data: [0x90, 60, 127] }], 100);
            assert!(out.iter().all(|x| x.is_finite()));
        }
    }
}
