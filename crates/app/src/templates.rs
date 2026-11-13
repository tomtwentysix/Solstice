//! Project templates for the Project Chooser: Solstice's own, built in code, and the user's,
//! which are saved projects kept in `<data dir>/templates/`.

use crate::plugins::new_slot;
use gtk::glib;
use solstice_model::*;
use std::path::{Path, PathBuf};

/// Drum Kit samples written into new projects start with this, so an unused project holding
/// only them still counts as empty.
pub const KIT_PREFIX: &str = "Solstice Kit ";

pub struct Template {
    pub name: &'static str,
    pub description: &'static str,
    pub icon: &'static str,
    pub tempo: f64,
    build: fn(&mut Builder),
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "Empty Project",
        description: "Start with the tracks you choose below",
        icon: "document-new-symbolic",
        tempo: 120.0,
        build: |_| {},
    },
    Template {
        name: "Songwriter",
        description: "Vocal, acoustic guitar, keys, bass and drums, with reverb and delay",
        icon: "audio-input-microphone-symbolic",
        tempo: 96.0,
        build: songwriter,
    },
    Template {
        name: "Electronic",
        description: "Drums, bass, lead, chords, arpeggio and pad on Solstice Synth",
        icon: "audio-speakers-symbolic",
        tempo: 124.0,
        build: electronic,
    },
    Template {
        name: "Hip Hop",
        description: "Drums, 808, keys and vocals",
        icon: "media-playlist-repeat-symbolic",
        tempo: 90.0,
        build: hip_hop,
    },
    Template {
        name: "Band Recording",
        description: "A track per input for drums, bass, guitars, keys and vocals",
        icon: "audio-x-generic-symbolic",
        tempo: 120.0,
        build: band,
    },
    Template {
        name: "Podcast",
        description: "Host and guest microphones with voice processing, and a music bed",
        icon: "system-users-symbolic",
        tempo: 120.0,
        build: podcast,
    },
];

pub struct Builder<'a> {
    p: &'a mut Project,
    audio_dir: &'a Path,
    next_input: u32,
}

impl Builder<'_> {
    fn slot(&mut self, plugin: &str, preset: &str) -> PluginSlot {
        let info = crate::host::builtin_catalog().into_iter().find(|c| c.uid == plugin).expect("built-in plug-in");
        let mut slot = new_slot(self.p, &info);
        slot.state = solstice_dsp::presets::state(plugin, preset);
        slot
    }

    fn track(&mut self, kind: TrackKind, name: &str) -> Id {
        let id = self.p.add_track(kind);
        self.p.track_mut(id).unwrap().name = name.into();
        id
    }

    /// An audio track on the next free input (two for stereo).
    fn audio(&mut self, name: &str, stereo: bool) -> Id {
        let id = self.track(TrackKind::Audio, name);
        let t = self.p.track_mut(id).unwrap();
        (t.stereo, t.input) = (stereo, self.next_input);
        self.next_input += if stereo { 2 } else { 1 };
        id
    }

    fn synth(&mut self, name: &str, preset: &str) -> Id {
        let id = self.track(TrackKind::Instrument, name);
        let slot = self.slot("solstice:synth", preset);
        self.p.track_mut(id).unwrap().instrument = Some(slot);
        id
    }

    /// A Drum Kit loaded with Solstice's synthesised kit, written into the project.
    fn drums(&mut self, name: &str, preset: &str) -> Id {
        let id = self.track(TrackKind::Instrument, name);
        let mut slot = self.slot("solstice:drums", preset);
        let mut zones = Vec::new();
        for (i, hit) in solstice_dsp::drumkit::kit(self.p.sample_rate).into_iter().enumerate() {
            let file = format!("{KIT_PREFIX}{}.wav", solstice_dsp::sampler::PADS[i].1);
            if solstice_engine::audio::write_wav(&self.audio_dir.join(&file), self.p.sample_rate, &[hit]).is_ok() {
                zones.push(solstice_dsp::sampler::pad_zone(i, file));
            }
        }
        let bank = solstice_dsp::Bank::new(&solstice_dsp::sampler::SAMPLER);
        bank.set_state(&slot.state);
        slot.state = solstice_dsp::sampler::state(&bank, &zones);
        self.p.track_mut(id).unwrap().instrument = Some(slot);
        id
    }

    fn midi_fx(&mut self, track: Id, plugin: &str, preset: &str) {
        let slot = self.slot(plugin, preset);
        self.p.track_mut(track).unwrap().midi_fx.push(slot);
    }

    fn insert(&mut self, channel: Option<Id>, plugin: &str, preset: &str) {
        let slot = self.slot(plugin, preset);
        if let Some(ch) = self.p.channel_mut(channel) {
            ch.inserts.push(slot);
        }
    }

    fn eq(&mut self, channel: Option<Id>, preset: &str) {
        if let (Some(eq), Some(ch)) = (crate::channel_eq::preset(preset), self.p.channel_mut(channel)) {
            ch.eq = eq;
        }
    }

    fn bus(&mut self, name: &str, plugin: &str, preset: &str) -> Id {
        let id = self.p.add_bus();
        self.p.buses.last_mut().unwrap().name = name.into();
        self.insert(Some(id), plugin, preset);
        id
    }

    fn send(&mut self, from: Id, bus: Id, level_db: f32) {
        if let Some(ch) = self.p.channel_mut(Some(from)) {
            ch.sends.push(Send { bus, level_db, mode: SendMode::PostPan, pan: None });
        }
    }

    fn volume(&mut self, channel: Id, db: f32) {
        if let Some(ch) = self.p.channel_mut(Some(channel)) {
            ch.volume_db = db;
        }
    }

    /// Sends each track to a reverb and a delay bus at the given levels.
    fn effects(&mut self, reverb: &str, delay: &str, sends: &[(Id, f32, f32)]) {
        let rv = self.bus("Reverb", "solstice:reverb", reverb);
        let dl = self.bus("Delay", "solstice:delay", delay);
        for &(t, r, d) in sends {
            if r > -60.0 {
                self.send(t, rv, r);
            }
            if d > -60.0 {
                self.send(t, dl, d);
            }
        }
    }
}

const OFF: f32 = -99.0;

fn vocal(b: &mut Builder, name: &str, upfront: bool) -> Id {
    let v = b.audio(name, false);
    b.eq(Some(v), "Vocal Clarity");
    b.insert(Some(v), "solstice:compressor", if upfront { "Vocal Upfront" } else { "Vocal Leveller" });
    v
}

fn songwriter(b: &mut Builder) {
    let vox = vocal(b, "Lead Vocal", false);
    let gtr = b.audio("Acoustic Guitar", false);
    b.eq(Some(gtr), "Acoustic Guitar");
    b.insert(Some(gtr), "solstice:compressor", "Acoustic Guitar");
    let keys = b.synth("Keys", "Electric Keys");
    b.eq(Some(keys), "Bright Keys");
    b.synth("Bass", "Sub Bass");
    let drums = b.drums("Drums", "Dynamic");
    b.insert(Some(drums), "solstice:compressor", "Drum Bus Glue");
    b.volume(drums, -4.0);
    b.effects("Aux Plate", "Aux Quarter Note", &[(vox, -12.0, -18.0), (gtr, -15.0, OFF), (keys, -15.0, OFF), (drums, -22.0, OFF)]);
}

fn electronic(b: &mut Builder) {
    let drums = b.drums("Drums", "Punchy");
    b.insert(Some(drums), "solstice:compressor", "Drum Bus Glue");
    b.synth("Bass", "Reese Bass");
    let lead = b.synth("Lead", "Fat Saw Lead");
    let chords = b.synth("Chords", "Supersaw Chords");
    let arp = b.synth("Arp", "Arp Pluck");
    b.midi_fx(arp, "solstice:arpeggiator", "Sixteenths Up");
    let pad = b.synth("Pad", "Warm Pad");
    b.eq(Some(pad), "Thin Pad");
    b.effects("Aux Hall", "Aux Dotted Eighth", &[(lead, -14.0, -16.0), (chords, -16.0, OFF), (arp, -14.0, -12.0), (pad, -12.0, OFF)]);
    b.insert(None, "solstice:limiter", "Transparent Master");
}

fn hip_hop(b: &mut Builder) {
    let drums = b.drums("Drums", "Punchy");
    b.insert(Some(drums), "solstice:compressor", "Drum Bus Glue");
    b.synth("808", "808 Bass");
    let keys = b.synth("Keys", "Electric Keys");
    let vox = vocal(b, "Vocal", true);
    let adlib = b.audio("Ad-Libs", false);
    b.eq(Some(adlib), "Backing Vocals");
    b.insert(Some(adlib), "solstice:compressor", "Vocal Upfront");
    b.volume(adlib, -6.0);
    b.effects("Aux Plate", "Aux Quarter Note", &[(keys, -18.0, OFF), (vox, -16.0, -20.0), (adlib, -12.0, -10.0)]);
    b.insert(None, "solstice:limiter", "Transparent Master");
}

fn band(b: &mut Builder) {
    let drum_bus = b.bus("Drums", "solstice:compressor", "Drum Bus Glue");
    let kick = b.audio("Kick", false);
    b.eq(Some(kick), "Kick");
    b.insert(Some(kick), "solstice:gate", "Kick");
    let snare = b.audio("Snare", false);
    b.eq(Some(snare), "Snare");
    b.insert(Some(snare), "solstice:gate", "Snare");
    let oh = b.audio("Overheads", true);
    b.eq(Some(oh), "Overheads");
    for t in [kick, snare, oh] {
        b.p.channel_mut(Some(t)).unwrap().output = Output::Bus(drum_bus);
    }
    let bass = b.audio("Bass", false);
    b.eq(Some(bass), "Bass");
    b.insert(Some(bass), "solstice:compressor", "Bass Even");
    let mut guitars = Vec::new();
    for name in ["Guitar 1", "Guitar 2"] {
        let g = b.audio(name, false);
        b.eq(Some(g), "Electric Guitar");
        guitars.push(g);
    }
    if let [g1, g2] = guitars[..] {
        b.p.channel_mut(Some(g1)).unwrap().pan = -0.6;
        b.p.channel_mut(Some(g2)).unwrap().pan = 0.6;
    }
    let keys = b.audio("Keys", true);
    b.eq(Some(keys), "Bright Keys");
    let vox = vocal(b, "Lead Vocal", false);
    let bvs = b.audio("Backing Vocals", false);
    b.eq(Some(bvs), "Backing Vocals");
    b.insert(Some(bvs), "solstice:compressor", "Vocal Leveller");
    let rv = b.bus("Reverb", "solstice:reverb", "Aux Hall");
    for (t, db) in [(snare, -18.0), (keys, -16.0), (vox, -14.0), (bvs, -10.0)] {
        b.send(t, rv, db);
    }
    for g in guitars {
        b.send(g, rv, -20.0);
    }
}

fn podcast(b: &mut Builder) {
    for name in ["Host", "Guest 1", "Guest 2"] {
        let v = b.audio(name, false);
        b.insert(Some(v), "solstice:gate", "Podcast Room");
        b.eq(Some(v), "Podcast Voice");
        b.insert(Some(v), "solstice:compressor", "Podcast Voice");
    }
    let music = b.audio("Music", true);
    b.volume(music, -14.0);
    b.insert(None, "solstice:limiter", "Podcast");
}

/// Details from the Project Chooser applied to a new project.
pub struct Details {
    pub tempo: f64,
    pub sig: (u8, u8),
    pub key: Option<String>,
    /// Tracks for the Empty Project.
    pub audio_tracks: u32,
    pub instrument_tracks: u32,
}

pub fn build(t: &Template, d: &Details, sample_rate: u32) -> (Project, Bundle) {
    let (dir, name) = crate::studio::untitled_dir();
    build_in(t, d, sample_rate, dir, &name)
}

fn build_in(t: &Template, d: &Details, sample_rate: u32, dir: PathBuf, name: &str) -> (Project, Bundle) {
    let bundle = Bundle::new(dir);
    let _ = std::fs::create_dir_all(bundle.audio_dir());
    let mut p = Project::new(name, sample_rate);
    p.tempo = d.tempo;
    (p.sig_num, p.sig_den) = d.sig;
    let audio_dir = bundle.audio_dir();
    let mut b = Builder { p: &mut p, audio_dir: &audio_dir, next_input: 0 };
    if t.name == "Empty Project" {
        for _ in 0..d.audio_tracks {
            b.p.add_track(TrackKind::Audio);
        }
        for _ in 0..d.instrument_tracks {
            let id = b.p.add_track(TrackKind::Instrument);
            let slot = b.slot("solstice:synth", "Electric Keys");
            b.p.track_mut(id).unwrap().instrument = Some(slot);
        }
    } else {
        (t.build)(&mut b);
    }
    if let Some(key) = &d.key {
        p.key_changes.push(KeyChange { bar: 0, key: key.clone() });
    }
    retime(&mut p);
    (p, bundle)
}

/// Cycle and punch ranges are made at 120 BPM in 4/4; this moves them to the chosen tempo.
fn retime(p: &mut Project) {
    let bar = p.time().bar_len();
    p.cycle = Range::new(0, bar * 4);
    p.punch = Range::new(bar * 2, bar * 4);
}

pub fn user_dir() -> PathBuf {
    match crate::settings::dev_root() {
        Some(d) => d.join("data/templates"),
        None => glib::user_data_dir().join("solstice/templates"),
    }
}

/// The user's templates, by name.
pub fn user_templates() -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(user_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| Bundle::is_bundle(p))
                .map(|p| (p.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned()), p))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn copy_audio(from: &Bundle, to: &Bundle) -> std::io::Result<()> {
    std::fs::create_dir_all(to.audio_dir())?;
    if let Ok(rd) = std::fs::read_dir(from.audio_dir()) {
        for e in rd.flatten().filter(|e| e.path().is_file()) {
            std::fs::copy(e.path(), to.audio_dir().join(e.file_name()))?;
        }
    }
    Ok(())
}

/// A new untitled project copied from one of the user's templates.
pub fn from_user(template: &Path) -> std::io::Result<(Project, Bundle)> {
    let src = Bundle::new(template);
    let mut p = src.load()?;
    let (dir, name) = crate::studio::untitled_dir();
    let bundle = Bundle::new(dir);
    copy_audio(&src, &bundle)?;
    p.name = name;
    Ok((p, bundle))
}

pub fn save_user(project: &Project, from: &Bundle, name: &str) -> std::io::Result<()> {
    let to = Bundle::new(user_dir().join(format!("{}.solstice", name.replace('/', "-"))));
    if to.dir.exists() {
        std::fs::remove_dir_all(&to.dir)?;
    }
    copy_audio(from, &to)?;
    let mut p = project.clone();
    p.name = name.into();
    to.save(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_build() {
        let dir = std::env::temp_dir().join(format!("solstice-templates-{}", std::process::id()));
        let d = Details { tempo: 100.0, sig: (3, 4), key: Some("A minor".into()), audio_tracks: 2, instrument_tracks: 1 };
        for t in TEMPLATES {
            let (p, bundle) = build_in(t, &d, 48000, dir.join(t.name), t.name);
            assert_eq!(p.tempo, 100.0);
            assert_eq!((p.sig_num, p.sig_den), (3, 4));
            assert!(!p.tracks.is_empty(), "{} has no tracks", t.name);
            for slot in p.tracks.iter().flat_map(|t| t.instrument.iter().chain(&t.midi_fx).chain(&t.channel.inserts)) {
                assert!(!slot.state.is_empty(), "{}: {} has no preset", t.name, slot.name);
            }
            let drums = p.tracks.iter().filter_map(|t| t.instrument.as_ref()).find(|s| s.uid == "solstice:drums");
            if let Some(s) = drums {
                let bank = solstice_dsp::Bank::new(&solstice_dsp::sampler::SAMPLER);
                let zones = solstice_dsp::sampler::set_state(&bank, &s.state);
                assert_eq!(zones.len(), 16);
                assert!(zones.iter().all(|z| bundle.audio_dir().join(&z.path).is_file()));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
