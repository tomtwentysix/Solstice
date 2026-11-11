use super::*;
use crate::audio::{AudioData, write_wav};
use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use solstice_model::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Mutex;

#[global_allocator]
static ALLOC: AllocDisabler = AllocDisabler;

const SR: u32 = 48000;
const BLOCK: usize = 256;

#[derive(Default)]
struct Res {
    audio: HashMap<String, Arc<AudioData>>,
    plugins: HashMap<Id, Arc<PluginCell>>,
    strips: RefCell<HashMap<Option<Id>, Arc<Strip>>>,
    rings: RefCell<HashMap<Id, Producer<f32>>>,
}

impl Resources for Res {
    fn audio(&self, file: &str) -> Option<Arc<AudioData>> {
        self.audio.get(file).cloned()
    }
    fn plugin(&self, slot: Id) -> Option<Arc<PluginCell>> {
        self.plugins.get(&slot).cloned()
    }
    fn strip(&self, id: Option<Id>) -> Arc<Strip> {
        self.strips.borrow_mut().entry(id).or_default().clone()
    }
    fn record_ring(&self, track: Id) -> Option<Producer<f32>> {
        self.rings.borrow_mut().remove(&track)
    }
}

fn opts() -> Options {
    Options {
        max_block: BLOCK,
        inputs: 2,
        auto_input: true,
        low_latency: None,
        midi_track: None,
        midi_thru: true,
        click_gain: 0.5,
        click_accent: true,
    }
}

fn constant_file(name: &str, value: f32, frames: usize) -> Arc<AudioData> {
    let dir = std::env::temp_dir().join(format!("solstice-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    write_wav(&path, SR, &[vec![value; frames]]).unwrap();
    Arc::new(AudioData::open(&path).unwrap())
}

fn audio_track(p: &mut Project, file: &str, start: u64, length: u64) -> Id {
    let t = p.add_track(TrackKind::Audio);
    let id = p.alloc_id();
    p.track_mut(t).unwrap().regions.push(Region {
        id,
        name: "r".into(),
        start,
        length,
        muted: false,
        loops: 0,
        content: Content::Audio(AudioContent { file: file.into(), offset: 0, gain_db: 0.0, fade_in: 0, fade_out: 0 }),
    });
    t
}

/// Renders `blocks` periods with the given input signal and returns the stereo output.
fn render(engine: &mut Engine, blocks: usize, input: &[f32]) -> [Vec<f32>; 2] {
    let mut out = [Vec::with_capacity(blocks * BLOCK), Vec::with_capacity(blocks * BLOCK)];
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for b in 0..blocks {
        let slice = &input[(b * BLOCK).min(input.len())..((b + 1) * BLOCK).min(input.len())];
        let padded: Vec<f32> = slice.iter().copied().chain(std::iter::repeat(0.0)).take(BLOCK).collect();
        let ins = [&padded[..], &padded[..]];
        assert_no_alloc(|| engine.process(&ins, &mut l, &mut r, &[]));
        out[0].extend_from_slice(&l);
        out[1].extend_from_slice(&r);
    }
    out
}

fn start(p: &Project, res: &Res) -> (Engine, Handle, rtrb::Consumer<RecEvent>) {
    let (engine, mut handle, rec) = new(SR);
    handle.set_graph(compile(p, res, &opts()));
    (engine, handle, rec)
}

#[test]
fn pan_laws() {
    let c = pan_gains(1.0, 0.0, true);
    assert!((c[0] - 1.0).abs() < 1e-6 && (c[1] - 1.0).abs() < 1e-6);
    let hard = pan_gains(1.0, -1.0, true);
    assert!((hard[0] - std::f32::consts::SQRT_2).abs() < 1e-6 && hard[1].abs() < 1e-6);
    assert_eq!(pan_gains(1.0, 0.5, false), [0.5, 1.0]);
}

#[test]
fn plays_region_at_exact_sample() {
    let mut p = Project::new("t", SR);
    audio_track(&mut p, "a.wav", 100, 1000);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("a.wav", 0.5, 2000));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 8, &[]);
    assert_eq!(out[0][99], 0.0);
    assert!((out[0][100] - 0.5).abs() < 1e-6 && (out[1][100] - 0.5).abs() < 1e-6);
    assert!((out[0][1099] - 0.5).abs() < 1e-6);
    assert_eq!(out[0][1100], 0.0);
    assert_eq!(h.shared.position.load(Relaxed), 8 * BLOCK as u64);
}

#[test]
fn solo_mutes_others() {
    let mut p = Project::new("t", SR);
    let a = audio_track(&mut p, "a.wav", 0, 1000);
    audio_track(&mut p, "b.wav", 0, 1000);
    p.track_mut(a).unwrap().channel.solo = true;
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("sa.wav", 0.25, 1000));
    res.audio.insert("b.wav".into(), constant_file("sb.wav", 0.5, 1000));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 2, &[]);
    assert!((out[0][0] - 0.25).abs() < 1e-6);
}

#[test]
fn cycle_wraps_sample_accurately() {
    let mut p = Project::new("t", SR);
    audio_track(&mut p, "a.wav", 0, 1000);
    p.cycle = Range::new(0, 1000);
    p.cycle_on = true;
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("c.wav", 1.0, 1000));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 8, &[]);
    assert!(out[0][BLOCK..].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    assert_eq!(h.shared.position.load(Relaxed), (8 * BLOCK as u64) % 1000);
}

struct Spy(Arc<Mutex<Vec<MidiEvent>>>);

impl Plugin for Spy {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], events: &[MidiEvent], _: &ProcessContext) {
        if let Ok(mut v) = self.0.try_lock() {
            if v.capacity() - v.len() >= events.len() {
                v.extend_from_slice(events);
            }
        }
    }
}

#[test]
fn midi_lands_on_frame() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Instrument);
    let slot = p.alloc_id();
    let rid = p.alloc_id();
    let beat = p.time().beat_len();
    let tr = p.track_mut(t).unwrap();
    tr.instrument = Some(PluginSlot {
        id: slot,
        uid: String::new(),
        path: "".into(),
        name: "spy".into(),
        bypass: false,
        state: vec![],
    });
    tr.regions.push(Region {
        id: rid,
        name: "m".into(),
        start: 300,
        length: beat * 4,
        muted: false,
        loops: 0,
        content: Content::Midi(MidiContent {
            notes: vec![Note { start: 0, length: PPQ / 960 * 10, pitch: 60, velocity: 90 }],
            controllers: vec![
                Controller { tick: 1, number: 1, value: 64 },
                Controller { tick: 2, number: PITCH_BEND, value: 16383 },
            ],
        }),
    });
    let seen = Arc::new(Mutex::new(Vec::with_capacity(64)));
    let mut res = Res::default();
    res.plugins.insert(slot, PluginCell::new(Box::new(Spy(seen.clone())), 0));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 3, &[]);
    let note_len = p.time().ticks_to_samples(10);
    let ev = seen.lock().unwrap().clone();
    assert_eq!(ev[0], MidiEvent { frame: 300 - BLOCK as u32, data: [0x90, 60, 90] });
    // Controller data follows the note, one and two ticks (25 and 50 samples) in.
    assert_eq!(ev[1], MidiEvent { frame: 325 - BLOCK as u32, data: [0xB0, 1, 64] });
    assert_eq!(ev[2], MidiEvent { frame: 350 - BLOCK as u32, data: [0xE0, 0x7f, 0x7f] });
    assert_eq!(ev[3].frame, ((300 + note_len) % BLOCK as u64) as u32);
    assert_eq!(ev[3].data[0], 0x80);
}

#[test]
fn records_input_take() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Audio);
    p.track_mut(t).unwrap().armed = true;
    let res = Res::default();
    let (prod, cons) = rtrb::RingBuffer::new(SR as usize);
    res.rings.borrow_mut().insert(t, prod);
    let (mut e, mut h, rec_events) = start(&p, &res);
    let rec = recorder::spawn(rec_events, SR);
    let dir = std::env::temp_dir().join(format!("solstice-rec-{}", std::process::id()));
    rec.control.send(recorder::Control::SetDir(dir.clone())).unwrap();
    rec.control.send(recorder::Control::Register(t, cons)).unwrap();
    h.locate_and_record(512);
    let input: Vec<f32> = (0..BLOCK * 4).map(|i| i as f32 / 10000.0).collect();
    let out = render(&mut e, 4, &input);
    // Armed and recording with auto input: the input is monitored.
    assert!((out[0][10] - 10.0 / 10000.0).abs() < 1e-6);
    h.send(Command::Stop);
    render(&mut e, 1, &[]);
    let take = rec.takes.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!((take.track, take.start, take.frames), (t, 512, BLOCK as u64 * 4));
    let data = AudioData::open(&dir.join(&take.file)).unwrap();
    assert_eq!(data.sample(0, 100), 100.0 / 10000.0);
}

impl Handle {
    fn locate_and_record(&mut self, pos: u64) {
        self.send(Command::Locate(pos));
        self.send(Command::Record(true));
    }
}

#[test]
fn bounces_master() {
    let mut p = Project::new("t", SR);
    audio_track(&mut p, "a.wav", 0, 1000);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("bounce.wav", 0.5, 1000));
    let (prod, cons) = rtrb::RingBuffer::new(SR as usize * 2);
    let (mut e, mut h, rec_events) = new(SR);
    let mut g = compile(&p, &res, &opts());
    g.bounce_ring = Some(prod);
    h.set_graph(g);
    let rec = recorder::spawn(rec_events, SR);
    let dir = std::env::temp_dir().join(format!("solstice-bounce-{}", std::process::id()));
    rec.control.send(recorder::Control::SetDir(dir.clone())).unwrap();
    rec.control.send(recorder::Control::Register(BOUNCE_TRACK, cons)).unwrap();
    h.send(Command::Bounce { start: 100, end: 700 });
    render(&mut e, 4, &[]);
    assert!(!h.shared.playing.load(Relaxed));
    let take = rec.takes.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!((take.track, take.start, take.frames), (BOUNCE_TRACK, 100, 600));
    let data = AudioData::open(&dir.join(&take.file)).unwrap();
    assert_eq!((data.channels, data.frames), (2, 600));
    assert!((data.sample(1, 10) - 0.5).abs() < 1e-6);
}

#[test]
fn routes_through_bus_chain_with_send_modes() {
    let mut p = Project::new("t", SR);
    let tr = audio_track(&mut p, "a.wav", 0, 2000);
    let a = p.add_bus();
    let b = p.add_bus();
    // Track -> post-fader send to A (track fader at -inf so only a pre-fader send would pass).
    p.track_mut(tr).unwrap().channel.output = Output::Bus(b);
    p.track_mut(tr).unwrap().channel.sends.push(Send { bus: a, level_db: 0.0, mode: SendMode::PreFader });
    p.track_mut(tr).unwrap().channel.volume_db = -96.0;
    // A -> B -> master.
    p.buses[0].channel.output = Output::Bus(b);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("chain.wav", 0.5, 2000));
    let (mut e, mut h, _rec) = start(&p, &res);
    for (id, ch) in [(Some(tr), &p.tracks[0].channel)] {
        res.strip(id).set(ch.volume_db, ch.pan, ch.mute);
    }
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Play);
    let out = render(&mut e, 2, &[]);
    // Only the pre-fader send reaches the output, via A then B.
    assert!((out[0][300] - 0.5).abs() < 1e-5, "{}", out[0][300]);
}

#[test]
fn solo_safe_and_solo_through_buses() {
    let mut p = Project::new("t", SR);
    let a = audio_track(&mut p, "a.wav", 0, 1000);
    let b = audio_track(&mut p, "b.wav", 0, 1000);
    let bus1 = p.add_bus();
    let bus2 = p.add_bus();
    p.track_mut(a).unwrap().channel.output = Output::Bus(bus1);
    p.buses[0].channel.output = Output::Bus(bus2);
    p.track_mut(a).unwrap().channel.solo = true;
    p.track_mut(b).unwrap().channel.solo_safe = true;
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("ssa.wav", 0.25, 1000));
    res.audio.insert("b.wav".into(), constant_file("ssb.wav", 0.5, 1000));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 2, &[]);
    assert!((out[0][300] - 0.75).abs() < 1e-5, "{}", out[0][300]);
}

#[test]
fn play_in_cycle_starts_at_cycle_start() {
    let mut p = Project::new("t", SR);
    p.cycle = Range::new(10_000, 20_000);
    p.cycle_on = true;
    let res = Res::default();
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Locate(50_000));
    render(&mut e, 1, &[]);
    h.send(Command::Play);
    render(&mut e, 1, &[]);
    assert_eq!(h.shared.position.load(Relaxed), 10_000 + BLOCK as u64);
}

#[test]
fn channel_eq_runs_in_the_strip() {
    let mut p = Project::new("t", SR);
    let tr = audio_track(&mut p, "a.wav", 0, 4000);
    let ch = &mut p.track_mut(tr).unwrap().channel;
    ch.eq.enabled = true;
    // A constant signal is pure DC, which a low cut removes entirely.
    ch.eq.bands[0].on = true;
    ch.eq.bands[0].freq = 200.0;
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("eq.wav", 0.5, 4000));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 12, &[]);
    assert!(out[0][3000].abs() < 0.01, "{}", out[0][3000]);
}

#[test]
fn looped_region_repeats() {
    let mut p = Project::new("t", SR);
    let tr = audio_track(&mut p, "a.wav", 0, 300);
    p.track_mut(tr).unwrap().regions[0].loops = 2;
    let mut res = Res::default();
    // A ramp, so the repeat point is recognisable.
    let dir = std::env::temp_dir().join(format!("solstice-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ramp: Vec<f32> = (0..300).map(|i| i as f32 / 1000.0).collect();
    audio::write_wav(&dir.join("loop.wav"), SR, &[ramp]).unwrap();
    res.audio.insert("a.wav".into(), Arc::new(AudioData::open(&dir.join("loop.wav")).unwrap()));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let out = render(&mut e, 4, &[]);
    assert!((out[0][310] - 0.010).abs() < 1e-6, "{}", out[0][310]);
    assert!((out[0][650] - 0.050).abs() < 1e-6, "{}", out[0][650]);
    assert_eq!(out[0][900], 0.0);
}
