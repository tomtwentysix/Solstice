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
        isolate: None,
        freeze: None,
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
        color: None,
        content: Content::Audio(AudioContent::new(file)),
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
        sidechain: None,
    });
    tr.regions.push(Region {
        id: rid,
        name: "m".into(),
        start: 300,
        length: beat * 4,
        muted: false,
        loops: 0,
        color: None,
        content: Content::Midi(MidiContent {
            notes: vec![Note::new(0, PPQ / 960 * 10, 60, 90)],
            controllers: vec![
                Controller { tick: 1, number: 1, value: 64 },
                Controller { tick: 2, number: PITCH_BEND, value: 16383 },
            ],
            ..Default::default()
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

fn automated(mode: AutoMode, touched: bool) -> f32 {
    let mut p = Project::new("t", SR);
    let tr = audio_track(&mut p, "a.wav", 0, 4000);
    let ch = &mut p.track_mut(tr).unwrap().channel;
    ch.auto_mode = mode;
    ch.lane_mut(AutoTarget::Volume).points = vec![AutoPoint { pos: 0, value: -6.0 }];
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("auto.wav", 0.5, 4000));
    res.strip(Some(tr)).touched.store(touched, Relaxed);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 4, &[])[0][700]
}

#[test]
fn automation_follows_mode() {
    let at_minus_6 = 0.5 * db_to_gain(-6.0);
    assert!((automated(AutoMode::Read, false) - at_minus_6).abs() < 1e-4);
    assert!((automated(AutoMode::Touch, false) - at_minus_6).abs() < 1e-4);
    // Write, Off and a held control in Touch all ignore the lane and use the fader (0 dB).
    assert!((automated(AutoMode::Write, false) - 0.5).abs() < 1e-4);
    assert!((automated(AutoMode::Off, false) - 0.5).abs() < 1e-4);
    assert!((automated(AutoMode::Touch, true) - 0.5).abs() < 1e-4);
}

struct ParamSpy(Arc<Mutex<Vec<(u32, f64)>>>);

impl Plugin for ParamSpy {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}
    fn set_param(&mut self, id: u32, value: f64) {
        if let Ok(mut v) = self.0.try_lock()
            && v.len() < v.capacity()
        {
            v.push((id, value));
        }
    }
}

#[test]
fn plugin_parameters_are_automated_once_per_change() {
    let mut p = Project::new("t", SR);
    let tr = audio_track(&mut p, "a.wav", 0, 100);
    let slot = p.alloc_id();
    let ch = &mut p.track_mut(tr).unwrap().channel;
    ch.inserts.push(PluginSlot { id: slot, uid: String::new(), path: "".into(), name: "spy".into(), bypass: false, state: vec![], sidechain: None });
    ch.lane_mut(AutoTarget::Plugin { slot, param: 7 }).points =
        vec![AutoPoint { pos: 0, value: 0.25 }, AutoPoint { pos: BLOCK as u64 * 2, value: 0.75 }];
    let seen = Arc::new(Mutex::new(Vec::with_capacity(16)));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("p.wav", 0.0, 100));
    res.plugins.insert(slot, PluginCell::new(Box::new(ParamSpy(seen.clone())), 0));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 4, &[]);
    let got = seen.lock().unwrap().clone();
    assert_eq!(got.first(), Some(&(7, 0.25)));
    assert_eq!(got.last(), Some(&(7, 0.75)));
    assert!(got.len() <= 4);
}

#[test]
fn live_notes_reach_the_ui_with_transport_state() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Instrument);
    let slot = p.alloc_id();
    p.track_mut(t).unwrap().instrument =
        Some(PluginSlot { id: slot, uid: String::new(), path: "".into(), name: "spy".into(), bypass: false, state: vec![], sidechain: None });
    let seen = Arc::new(Mutex::new(Vec::with_capacity(64)));
    let mut res = Res::default();
    res.plugins.insert(slot, PluginCell::new(Box::new(Spy(seen.clone())), 0));
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &Options { midi_track: Some(t), ..opts() }));

    h.send(Command::Midi([0x90, 60, 100]));
    render(&mut e, 1, &[]);
    let n = h.live_notes.pop().unwrap();
    assert!(!n.playing && !n.recording);
    assert_eq!(n.data, [0x90, 60, 100]);
    assert_eq!(seen.lock().unwrap()[0].data, [0x90, 60, 100]);

    h.send(Command::Record(true));
    render(&mut e, 1, &[]);
    h.send(Command::Midi([0x80, 60, 0]));
    render(&mut e, 1, &[]);
    let n = h.live_notes.pop().unwrap();
    assert!(n.playing && n.recording);
    assert_eq!(n.pos, BLOCK as u64);
}

/// Records over a cycle of 100..1100, so the wrap falls 232 frames into the fourth block.
fn cycle_recorder(pdc: u32) -> (Engine, Handle) {
    cycle_recorder_at(100, pdc)
}

/// A 1000-frame cycle from `start`, which wraps 232 frames into the fourth block.
fn cycle_recorder_at(start: u64, pdc: u32) -> (Engine, Handle) {
    let mut p = Project::new("t", SR);
    p.cycle = Range::new(start, start + 1000);
    p.cycle_on = true;
    let t = p.add_track(TrackKind::Instrument);
    let mut g = compile(&p, &Res::default(), &Options { midi_track: Some(t), ..opts() });
    g.pdc = pdc;
    let (e, mut h, _rec) = new(SR);
    h.set_graph(g);
    (e, h)
}

fn play_block(e: &mut Engine, events: &[MidiEvent]) {
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let silence = vec![0.0; BLOCK];
    let ins = [&silence[..], &silence[..]];
    assert_no_alloc(|| e.process(&ins, &mut l, &mut r, events));
}

fn note_on(frame: u32) -> MidiEvent {
    MidiEvent { frame, data: [0x90, 60, 100] }
}

#[test]
fn recorded_notes_know_their_cycle_pass() {
    let (mut e, mut h) = cycle_recorder(0);
    h.send(Command::Record(true));
    for _ in 0..3 {
        play_block(&mut e, &[]);
    }
    play_block(&mut e, &[note_on(100), note_on(240)]);
    let before = h.live_notes.pop().unwrap();
    assert_eq!((before.pos, before.pass), (868 + 100, 0));
    let after = h.live_notes.pop().unwrap();
    assert_eq!((after.pos, after.pass), (108, 1));
    assert!(after.recording);

    play_block(&mut e, &[note_on(0)]);
    assert_eq!(h.live_notes.pop().unwrap().pass, 1);
    h.send(Command::Record(true));
    play_block(&mut e, &[note_on(0)]);
    assert_eq!(h.live_notes.pop().unwrap().pass, 0, "recording again starts from the first pass");
}

#[test]
fn plugin_delay_moves_notes_after_a_wrap_back_a_pass() {
    let (mut e, mut h) = cycle_recorder(64);
    h.send(Command::Record(true));
    for _ in 0..3 {
        play_block(&mut e, &[]);
    }
    play_block(&mut e, &[note_on(240), note_on(250)]);
    // Heard 56 frames before the cycle end, so it belongs to the end of the first pass.
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (1044, 0));
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (1054, 0));
    play_block(&mut e, &[note_on(100)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (124 + 100 - 64, 1));
}

#[test]
fn plugin_delay_steps_back_a_pass_from_a_cycle_at_zero() {
    let (mut e, mut h) = cycle_recorder_at(0, 64);
    h.send(Command::Record(true));
    for _ in 0..3 {
        play_block(&mut e, &[]);
    }
    play_block(&mut e, &[note_on(240)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (1000 - 56, 0));
    play_block(&mut e, &[note_on(10), note_on(100)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (1000 - 30, 0));
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (124 - 64, 1));
}

#[test]
fn plugin_delay_longer_than_the_cycle_start_steps_back_in_full() {
    let (mut e, mut h) = cycle_recorder_at(32, 64);
    h.send(Command::Record(true));
    for _ in 0..3 {
        play_block(&mut e, &[]);
    }
    play_block(&mut e, &[note_on(240)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (1032 - 56, 0));
}

#[test]
fn count_in_starts_recording_from_the_first_pass() {
    let (mut e, mut h) = cycle_recorder(0);
    h.send(Command::Record(true));
    for _ in 0..4 {
        play_block(&mut e, &[]);
    }
    play_block(&mut e, &[note_on(0)]);
    assert_eq!(h.live_notes.pop().unwrap().pass, 1);
    h.send(Command::Stop);
    h.send(Command::RecordCountIn(BLOCK as u64));
    play_block(&mut e, &[note_on(0)]);
    assert_eq!(h.live_notes.pop().unwrap().pass, 0);
}

#[test]
fn only_a_wrap_moves_notes_back_a_pass() {
    let (mut e, mut h) = cycle_recorder(64);
    h.send(Command::Play);
    for _ in 0..6 {
        play_block(&mut e, &[]);
    }
    h.send(Command::Stop);
    play_block(&mut e, &[]);
    h.send(Command::Play);
    play_block(&mut e, &[note_on(10)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (110 - 64, 0), "playback restarting at the cycle start is not a wrap");

    h.send(Command::Record(true));
    for _ in 0..4 {
        play_block(&mut e, &[]);
    }
    h.send(Command::Locate(100));
    play_block(&mut e, &[note_on(10)]);
    let n = h.live_notes.pop().unwrap();
    assert_eq!((n.pos, n.pass), (110 - 64, 1), "nor is a locate to it");
}

struct Up12;

impl Plugin for Up12 {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}
    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _: usize, _: &ProcessContext) {
        for e in input {
            out.push(MidiEvent { frame: e.frame, data: [e.data[0], e.data[1] + 12, e.data[2]] });
        }
    }
}

#[test]
fn midi_fx_rewrite_events_before_the_instrument() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Instrument);
    let (inst, fx1, fx2) = (p.alloc_id(), p.alloc_id(), p.alloc_id());
    let slot = |id, name: &str| PluginSlot { id, uid: String::new(), path: "".into(), name: name.into(), bypass: false, state: vec![], sidechain: None };
    let tr = p.track_mut(t).unwrap();
    tr.instrument = Some(slot(inst, "spy"));
    tr.midi_fx = vec![slot(fx1, "up"), slot(fx2, "up")];
    let seen = Arc::new(Mutex::new(Vec::with_capacity(64)));
    let mut res = Res::default();
    res.plugins.insert(inst, PluginCell::new(Box::new(Spy(seen.clone())), 0));
    res.plugins.insert(fx1, PluginCell::new(Box::new(Up12), 0));
    let bypassed = PluginCell::new(Box::new(Up12), 0);
    bypassed.bypass.store(true, Relaxed);
    res.plugins.insert(fx2, bypassed);
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &Options { midi_track: Some(t), ..opts() }));
    h.send(Command::Midi([0x90, 60, 100]));
    render(&mut e, 2, &[]);
    assert_eq!(seen.lock().unwrap()[0].data, [0x90, 72, 100]);
}

/// Delays audio by a fixed amount and says so, like a look-ahead limiter.
struct Lag(graph::Delay);

impl Plugin for Lag {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        self.0.run(l, r);
    }
}

#[test]
fn plugin_delay_is_compensated_across_tracks_and_buses() {
    let mut p = Project::new("t", SR);
    let plain = audio_track(&mut p, "a.wav", 300, 1000);
    let lagged = audio_track(&mut p, "a.wav", 300, 1000);
    let via_bus = audio_track(&mut p, "a.wav", 300, 1000);
    let bus = p.add_bus();
    let slot = p.alloc_id();
    p.track_mut(lagged).unwrap().channel.inserts.push(PluginSlot {
        id: slot,
        uid: String::new(),
        path: "".into(),
        name: "lag".into(),
        bypass: false,
        state: vec![],
        sidechain: None,
    });
    p.track_mut(via_bus).unwrap().channel.output = Output::Bus(bus);
    let _ = plain;
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("pdc.wav", 0.25, 1000));
    res.plugins.insert(slot, PluginCell::new(Box::new(Lag(graph::Delay::new(100).unwrap())), 100));
    let g = compile(&p, &res, &opts());
    assert_eq!(g.pdc, 100);
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Play);
    let out = render(&mut e, 4, &[]);
    let first = out[0].iter().position(|v| v.abs() > 1e-6).unwrap();
    assert_eq!(first, 400, "every path arrives together, 100 frames late");
    assert!((out[0][first] - out[0][700]).abs() < 1e-5, "all three tracks sound from the first sample");
}

#[test]
fn parallel_tracks_match_serial() {
    let mut p = Project::new("t", SR);
    let bus = p.add_bus();
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("mc.wav", 0.3, 2000));
    for k in 0..6u64 {
        let t = audio_track(&mut p, "a.wav", k * 37, 1500);
        let slot = p.alloc_id();
        let ch = &mut p.track_mut(t).unwrap().channel;
        ch.volume_db = -(k as f32);
        ch.pan = k as f32 / 6.0 - 0.5;
        ch.sends.push(Send { bus, level_db: -6.0, mode: if k % 2 == 0 { SendMode::PreFader } else { SendMode::PostPan } });
        ch.inserts.push(PluginSlot { id: slot, uid: String::new(), path: "".into(), name: "lag".into(), bypass: false, state: vec![], sidechain: None });
        res.plugins.insert(slot, PluginCell::new(Box::new(Lag(graph::Delay::new(10 + k as u32 * 5).unwrap())), 10 + k as u32 * 5));
    }
    let run = |workers: usize| {
        let (mut e, mut h, _rec) = new(SR);
        if workers > 0 {
            e.set_workers(workers, &pool::std_spawn, false);
            while e.workers() < workers {
                std::thread::yield_now();
            }
        }
        h.set_graph(compile(&p, &res, &opts()));
        h.send(Command::Play);
        render(&mut e, 8, &[])
    };
    let serial = run(0);
    assert!(serial[0].iter().any(|v| v.abs() > 0.01));
    assert_eq!(serial, run(3));
}

#[test]
fn notes_follow_tempo_changes() {
    let mut p = Project::new("t", SR);
    let bar = 4 * PPQ as u64;
    p.tempo_changes.push(TempoChange { tick: bar, tempo: 60.0 });
    let t = p.add_track(TrackKind::Instrument);
    let rid = p.alloc_id();
    p.track_mut(t).unwrap().regions.push(Region {
        id: rid,
        name: "m".into(),
        start: 0,
        length: SR as u64 * 10,
        muted: false,
        loops: 0,
        color: None,
        content: Content::Midi(MidiContent {
            notes: vec![Note::new(2 * bar as u32, PPQ, 60, 100)],
            ..Default::default()
        }),
    });
    let g = compile(&p, &Res::default(), &opts());
    // A bar at 120 (2 s) plus a bar at 60 (4 s), then a beat at 60 (1 s).
    assert_eq!(g.tracks[0].events[0], graph::Ev { pos: 6 * SR as u64, data: [0x90, 60, 100], chance: 100 });
    assert_eq!(g.tracks[0].events[1].pos, 7 * SR as u64);
}

#[test]
fn take_folder_plays_its_comp() {
    let mut p = Project::new("t", SR);
    let t = audio_track(&mut p, "one.wav", 0, 1000);
    let mut res = Res::default();
    res.audio.insert("one.wav".into(), constant_file("one.wav", 0.25, 2000));
    res.audio.insert("two.wav".into(), constant_file("two.wav", 0.5, 2000));
    let Content::Audio(a) = &mut p.track_mut(t).unwrap().regions[0].content else { panic!() };
    a.add_take(Take { name: "Take 2".into(), file: "two.wav".into(), offset: 0 });
    a.set_comp(0, 500, 0, 1000);
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Play);
    let out = render(&mut e, 4, &[]);
    let level = |i: usize| out[0][i] / out[0][100];
    assert!((level(800) - 2.0).abs() < 1e-4, "the second half plays take 2");
    assert!(level(500) > 1.0 && level(500) < 2.0, "the join crossfades");
}

/// Remembers the loudest side-chain sample it was given.
struct KeySpy(Arc<std::sync::atomic::AtomicU32>);

impl Plugin for KeySpy {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}
    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        let peak = key.map_or(0.0, |[l, _]| l.iter().fold(0f32, |m, v| m.max(v.abs())));
        self.0.store(peak.to_bits(), Relaxed);
    }
}

#[test]
fn side_chain_keys_come_from_the_source_track() {
    let mut p = Project::new("t", SR);
    let keyed = audio_track(&mut p, "a.wav", 10_000, 10);
    let source = audio_track(&mut p, "a.wav", 0, 2000);
    let slot = p.alloc_id();
    p.track_mut(keyed).unwrap().channel.inserts.push(PluginSlot {
        id: slot,
        uid: String::new(),
        path: "".into(),
        name: "spy".into(),
        bypass: false,
        state: vec![],
        sidechain: Some(source),
    });
    let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("sc.wav", 0.5, 2000));
    res.plugins.insert(slot, PluginCell::new(Box::new(KeySpy(peak.clone())), 0));
    let g = compile(&p, &res, &opts());
    assert_eq!(g.waves, (vec![1], vec![0]));
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Play);
    render(&mut e, 2, &[]);
    assert!((f32::from_bits(peak.load(Relaxed)) - 0.5).abs() < 0.01);
}

fn insert(p: &mut Project, channel: Option<Id>, sidechain: Option<Id>) -> Id {
    let id = p.alloc_id();
    p.channel_mut(channel).unwrap().inserts.push(PluginSlot {
        id,
        uid: String::new(),
        path: "".into(),
        name: "fx".into(),
        bypass: false,
        state: vec![],
        sidechain,
    });
    id
}

/// Records the transport position where its side-chain key first rises above 0.5.
struct KeyOnset {
    at: Option<usize>,
    seen: Arc<AtomicU64>,
}

impl KeyOnset {
    fn cell() -> (Arc<PluginCell>, Arc<AtomicU64>) {
        let seen = Arc::new(AtomicU64::new(u64::MAX));
        (PluginCell::new(Box::new(KeyOnset { at: None, seen: seen.clone() }), 0), seen)
    }
}

impl Plugin for KeyOnset {
    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        self.at = key.and_then(|[l, _]| l.iter().position(|v| v.abs() > 0.5));
    }
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], ctx: &ProcessContext) {
        if let Some(i) = self.at.take() {
            let _ = self.seen.compare_exchange(u64::MAX, ctx.position + i as u64, Relaxed, Relaxed);
        }
    }
}

fn lag(frames: u32) -> Arc<PluginCell> {
    PluginCell::new(Box::new(Lag(graph::Delay::new(frames).unwrap())), frames)
}

#[test]
fn side_chain_key_is_delay_compensated() {
    let mut p = Project::new("t", SR);
    let keyed = audio_track(&mut p, "a.wav", 10_000, 10);
    let source = audio_track(&mut p, "click.wav", 1000, 1);
    let mut res = Res::default();
    res.audio.insert("click.wav".into(), constant_file("click-track.wav", 1.0, 1));
    res.plugins.insert(insert(&mut p, Some(keyed), None), lag(100));
    let (onset, seen) = KeyOnset::cell();
    res.plugins.insert(insert(&mut p, Some(keyed), Some(source)), onset);
    let g = compile(&p, &res, &opts());
    let tap = g.tracks[0].channel.keys[1].as_ref().unwrap();
    assert_eq!(tap.delay.as_ref().map(|d| d.frames()), Some(100));
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Play);
    render(&mut e, 8, &[]);
    assert_eq!(seen.load(Relaxed), 1100, "the key lines up with the signal behind the 100-frame plug-in");
}

#[test]
fn removing_the_side_chain_clears_the_key() {
    let mut p = Project::new("t", SR);
    let keyed = audio_track(&mut p, "a.wav", 10_000, 10);
    let source = audio_track(&mut p, "a.wav", 0, 4000);
    let slot = insert(&mut p, Some(keyed), Some(source));
    let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("unkey.wav", 0.5, 4000));
    res.plugins.insert(slot, PluginCell::new(Box::new(KeySpy(peak.clone())), 0));
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Play);
    render(&mut e, 2, &[]);
    assert!(f32::from_bits(peak.load(Relaxed)) > 0.4);
    p.track_mut(keyed).unwrap().channel.inserts[0].sidechain = None;
    h.set_graph(compile(&p, &res, &opts()));
    render(&mut e, 2, &[]);
    assert_eq!(f32::from_bits(peak.load(Relaxed)), 0.0);
}

#[test]
fn bus_insert_key_lines_up_with_bus_input() {
    let mut p = Project::new("t", SR);
    let bus = p.add_bus();
    let other = p.add_bus();
    let sibling = audio_track(&mut p, "a.wav", 10_000, 10);
    let source = audio_track(&mut p, "click.wav", 1000, 1);
    p.track_mut(sibling).unwrap().channel.output = Output::Bus(bus);
    p.track_mut(source).unwrap().channel.output = Output::Bus(other);
    let mut res = Res::default();
    res.audio.insert("click.wav".into(), constant_file("click-bus.wav", 1.0, 1));
    res.plugins.insert(insert(&mut p, Some(sibling), None), lag(100));
    let (onset, seen) = KeyOnset::cell();
    res.plugins.insert(insert(&mut p, Some(bus), Some(source)), onset);
    let g = compile(&p, &res, &opts());
    assert_eq!(g.buses[0].keys[0].as_ref().unwrap().delay.as_ref().map(|d| d.frames()), Some(100));
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Play);
    render(&mut e, 8, &[]);
    assert_eq!(seen.load(Relaxed), 1100, "the key waits for the bus's slowest input");
}

#[test]
fn dropped_keys_get_no_delay() {
    let mut p = Project::new("t", SR);
    let a = audio_track(&mut p, "a.wav", 0, 10);
    let b = audio_track(&mut p, "a.wav", 0, 10);
    let c = audio_track(&mut p, "a.wav", 0, 10);
    let mut res = Res::default();
    res.plugins.insert(insert(&mut p, Some(a), None), lag(50));
    let dropped = insert(&mut p, Some(a), Some(b));
    res.plugins.insert(dropped, KeyOnset::cell().0);
    res.plugins.insert(insert(&mut p, Some(b), None), lag(50));
    let kept = insert(&mut p, Some(b), Some(c));
    res.plugins.insert(kept, KeyOnset::cell().0);
    let own = insert(&mut p, Some(c), Some(c));
    res.plugins.insert(own, KeyOnset::cell().0);
    let keys = side_chain_keys(&p, &|slot| res.plugins.contains_key(&slot));
    assert_eq!(keys.get(&kept), Some(&2));
    assert!(!keys.contains_key(&dropped) && !keys.contains_key(&own));
    let g = compile(&p, &res, &opts());
    assert!(g.tracks[0].channel.keys[1].is_none(), "b is keyed itself, so it can't key a");
    assert!(g.tracks[2].channel.keys[0].is_none());
    assert_eq!(g.tracks[1].channel.keys[1].as_ref().unwrap().delay.as_ref().map(|d| d.frames()), Some(50));
    assert_eq!(g.waves, (vec![2], vec![0, 1]));
}

/// A keyed track with a 100-frame plug-in ahead of its keyed insert, keyed by a one-frame click
/// at 1000, so the key's compensation line holds the click from 1000 until 1100.
fn keyed_click(p: &mut Project, res: &mut Res, file: &str) -> (Arc<PluginCell>, Arc<AtomicU64>) {
    let keyed = audio_track(p, "a.wav", 10_000, 10);
    let source = audio_track(p, "click.wav", 1000, 1);
    res.audio.insert("click.wav".into(), constant_file(file, 1.0, 1));
    res.plugins.insert(insert(p, Some(keyed), None), lag(100));
    let (onset, seen) = KeyOnset::cell();
    res.plugins.insert(insert(p, Some(keyed), Some(source)), onset.clone());
    (onset, seen)
}

#[test]
fn rebuilding_keeps_the_key_in_flight() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, seen) = keyed_click(&mut p, &mut res, "click-rebuild.wav");
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Play);
    render(&mut e, 4, &[]);
    // A new track above the source moves it to another index; the line still holds the click.
    let t = p.add_track(TrackKind::Audio);
    let t = p.tracks.remove(p.tracks.iter().position(|x| x.id == t).unwrap());
    p.tracks.insert(0, t);
    h.set_graph(compile(&p, &res, &opts()));
    render(&mut e, 4, &[]);
    assert_eq!(seen.load(Relaxed), 1100, "the click in the key's line survives the rebuild");
}

#[test]
fn bypassed_keyed_insert_does_not_replay_its_key() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (onset, seen) = keyed_click(&mut p, &mut res, "click-bypass.wav");
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Play);
    render(&mut e, 4, &[]);
    onset.bypass.store(true, Relaxed);
    render(&mut e, 4, &[]);
    onset.bypass.store(false, Relaxed);
    render(&mut e, 4, &[]);
    assert_eq!(seen.load(Relaxed), u64::MAX, "the click left the line while the insert was bypassed");
}

#[test]
fn unloaded_keyed_insert_leaves_its_track_free_to_key() {
    let mut p = Project::new("t", SR);
    let a = audio_track(&mut p, "a.wav", 0, 10);
    let b = audio_track(&mut p, "a.wav", 0, 10);
    let c = audio_track(&mut p, "a.wav", 0, 10);
    insert(&mut p, Some(a), Some(c));
    let keyed = insert(&mut p, Some(b), Some(a));
    let mut res = Res::default();
    res.plugins.insert(keyed, KeyOnset::cell().0);
    let keys = side_chain_keys(&p, &|slot| res.plugins.contains_key(&slot));
    assert_eq!(keys.get(&keyed), Some(&0));
    let g = compile(&p, &res, &opts());
    assert!(g.tracks[1].channel.keys[0].is_some());
    assert_eq!(g.waves, (vec![0, 2], vec![1]));
}

#[test]
fn count_in_delays_the_take() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Audio);
    p.track_mut(t).unwrap().armed = true;
    let (prod, cons) = rtrb::RingBuffer::new(SR as usize);
    let res = Res::default();
    res.rings.borrow_mut().insert(t, prod);
    let (mut e, mut h, mut rec) = new(SR);
    h.set_graph(compile(&p, &res, &opts()));
    h.send(Command::Locate(1000));
    render(&mut e, 1, &[]);
    h.send(Command::RecordCountIn(600));
    render(&mut e, 4, &vec![0.5; BLOCK * 4]);
    let Ok(RecEvent::Start { pos, .. }) = rec.pop() else { panic!("no take started") };
    assert_eq!(pos, 1000);
    // Only what came in after the 600-frame count-in was recorded.
    assert_eq!(cons.slots(), BLOCK * 4 - 600);
}

fn armed_recorder(offset: u32) -> (Engine, Handle, rtrb::Consumer<RecEvent>, rtrb::Consumer<f32>) {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Audio);
    p.track_mut(t).unwrap().armed = true;
    let (prod, cons) = rtrb::RingBuffer::new(SR as usize);
    let res = Res::default();
    res.rings.borrow_mut().insert(t, prod);
    let (e, h, rec) = start(&p, &res);
    h.shared.record_offset.store(offset, Relaxed);
    (e, h, rec, cons)
}

#[test]
fn take_starts_earlier_by_the_record_offset() {
    let (mut e, mut h, mut rec, cons) = armed_recorder(300);
    h.locate_and_record(1000);
    render(&mut e, 2, &vec![0.5; BLOCK * 2]);
    let Ok(RecEvent::Start { pos, .. }) = rec.pop() else { panic!("no take started") };
    assert_eq!(pos, 700);
    assert_eq!(cons.slots(), BLOCK * 2);
}

#[test]
fn take_before_zero_drops_its_lead_in() {
    let (mut e, mut h, mut rec, mut cons) = armed_recorder(300);
    h.locate_and_record(100);
    let input: Vec<f32> = (0..BLOCK * 4).map(|i| i as f32).collect();
    render(&mut e, 4, &input);
    let Ok(RecEvent::Start { pos, .. }) = rec.pop() else { panic!("no take started") };
    assert_eq!(pos, 0);
    assert_eq!(cons.slots(), BLOCK * 4 - 200);
    assert_eq!(cons.pop(), Ok(200.0));
    h.send(Command::Stop);
    render(&mut e, 1, &[]);
    let Ok(RecEvent::Stop { frames, .. }) = rec.pop() else { panic!("no take stopped") };
    assert_eq!(frames, (BLOCK * 4 - 200) as u64);
}

#[test]
fn measure_finds_the_loopback_delay() {
    let (mut e, mut h, _rec) = start(&Project::new("t", SR), &Res::default());
    let delay = BLOCK + 37;
    h.send(Command::Measure { output: 1, input: 1 });
    let mut sent: Vec<f32> = vec![0.0; delay];
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let silent = vec![0.0; BLOCK];
    for b in 0..SR as usize / BLOCK + 1 {
        let back: Vec<f32> = sent[b * BLOCK..(b + 1) * BLOCK].iter().map(|s| -0.3 * s).collect();
        let ins = [&silent[..], &back[..]];
        assert_no_alloc(|| e.process(&ins, &mut l, &mut r, &[]));
        assert!(l.iter().all(|s| *s == 0.0));
        sent.extend_from_slice(&r);
    }
    assert!(h.shared.measured.load(Relaxed));
    let captured: Vec<f32> = h.shared.measure.iter().map(|s| s.get()).collect();
    assert_eq!(measure::find_delay(&captured, SR), Some(delay));
}

#[test]
fn measure_rejects_silence_and_noise() {
    assert_eq!(measure::find_delay(&vec![0.0; 4800], SR), None);
    let mut x = 1u32;
    let noise: Vec<f32> = (0..4800)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as f32 / u32::MAX as f32 - 0.5
        })
        .collect();
    assert_eq!(measure::find_delay(&noise, SR), None);
    let mut faint = noise.iter().map(|n| n * 0.001).collect::<Vec<f32>>();
    for i in 0..measure::PROBE_LEN {
        faint[1234 + i] += measure::probe(i, SR) * 0.5;
    }
    assert_eq!(measure::find_delay(&faint, SR), Some(1234));
}

#[test]
fn vca_scales_and_mutes_its_channels() {
    let mut p = Project::new("t", SR);
    let t = audio_track(&mut p, "a.wav", 0, 4000);
    let vca = p.add_vca();
    p.track_mut(t).unwrap().channel.vca = Some(vca);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("vca.wav", 0.5, 4000));
    let g = compile(&p, &res, &opts());
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Play);
    let before = render(&mut e, 2, &[])[0][300];
    res.strip(Some(vca)).set(-6.0206, 0.0, false);
    let after = render(&mut e, 2, &[])[0][BLOCK + 100];
    assert!((after / before - 0.5).abs() < 1e-4, "{before} {after}");
    res.strip(Some(vca)).set(-6.0206, 0.0, true);
    let muted = render(&mut e, 2, &[])[0][BLOCK + 100];
    assert_eq!(muted, 0.0);
}

#[test]
fn isolating_a_track_silences_the_rest_and_skips_master_plugins() {
    let mut p = Project::new("t", SR);
    let a = audio_track(&mut p, "a.wav", 0, 2000);
    let b = audio_track(&mut p, "a.wav", 0, 2000);
    let slot = p.alloc_id();
    p.master.inserts.push(PluginSlot {
        id: slot,
        uid: String::new(),
        path: "".into(),
        name: "lag".into(),
        bypass: false,
        state: vec![],
        sidechain: None,
    });
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("iso.wav", 0.25, 2000));
    res.plugins.insert(slot, PluginCell::new(Box::new(Lag(graph::Delay::new(50).unwrap())), 50));
    let level = |isolate: Option<Id>| {
        let (mut e, mut h, _rec) = new(SR);
        h.set_graph(compile(&p, &res, &Options { isolate, ..opts() }));
        h.send(Command::Play);
        let out = render(&mut e, 2, &[]);
        (out[0][10], out[0][300])
    };
    let (both_start, both) = level(None);
    let (alone_start, alone) = level(Some(a));
    assert_eq!(both_start, 0.0, "the master plug-in delays the full mix");
    assert!(alone_start > 0.0, "an isolated track skips it");
    assert!((alone - both / 2.0).abs() < 1e-5);
    let _ = b;
}

#[test]
fn low_latency_monitoring_skips_compensation() {
    let mut p = Project::new("t", SR);
    let live = p.add_track(TrackKind::Audio);
    p.track_mut(live).unwrap().armed = true;
    let other = audio_track(&mut p, "a.wav", 0, 10);
    let slot = p.alloc_id();
    p.track_mut(other).unwrap().channel.inserts.push(PluginSlot {
        id: slot,
        uid: String::new(),
        path: "".into(),
        name: "lag".into(),
        bypass: false,
        state: vec![],
        sidechain: None,
    });
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("ll.wav", 0.0, 10));
    res.plugins.insert(slot, PluginCell::new(Box::new(Lag(graph::Delay::new(100).unwrap())), 100));
    let first_sound = |low_latency: Option<u32>| {
        let g = compile(&p, &res, &Options { low_latency, ..opts() });
        assert_eq!(g.pdc, 100);
        let (mut e, mut h, _rec) = new(SR);
        h.set_graph(g);
        let mut input = vec![0.0; BLOCK];
        input[10] = 1.0;
        let out = render(&mut e, 1, &input);
        out[0].iter().position(|v| v.abs() > 1e-6)
    };
    assert_eq!(first_sound(None), Some(110), "normally the live input waits with everything else");
    assert_eq!(first_sound(Some(64)), Some(10), "in low latency mode it comes straight through");
}

/// Counts its calls and adds a constant, so silence shows it did not run.
struct Counter(Arc<AtomicUsize>, f32);

impl Plugin for Counter {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        self.0.fetch_add(1, Relaxed);
        l.iter_mut().chain(r.iter_mut()).for_each(|s| *s += self.1);
    }
}

fn slot(id: Id, sidechain: Option<Id>) -> PluginSlot {
    PluginSlot { id, uid: String::new(), path: "".into(), name: "test".into(), bypass: false, state: vec![], sidechain }
}

/// An instrument track whose instrument is a `Counter`.
fn counted_instrument(p: &mut Project, res: &mut Res) -> (Id, Arc<AtomicUsize>) {
    let t = p.add_track(TrackKind::Instrument);
    let sid = p.alloc_id();
    p.track_mut(t).unwrap().instrument = Some(slot(sid, None));
    let calls = Arc::new(AtomicUsize::new(0));
    res.plugins.insert(sid, PluginCell::new(Box::new(Counter(calls.clone(), 0.25)), 0));
    (t, calls)
}

#[test]
fn suspend_ack_and_silence() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, calls) = counted_instrument(&mut p, &mut res);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 1, &[]);
    h.send(Command::Suspend(7));
    render(&mut e, 1, &[]);
    assert_eq!(h.shared.suspended.load(Relaxed), 7);
    assert!(!h.shared.playing.load(Relaxed));
    let before = calls.load(Relaxed);
    let out = render(&mut e, 3, &[]);
    assert_eq!(calls.load(Relaxed), before, "no plug-in runs while suspended");
    assert!(out[0].iter().chain(&out[1]).all(|v| *v == 0.0));
    h.send(Command::Suspend(0));
    let out = render(&mut e, 1, &[]);
    assert_eq!(h.shared.suspended.load(Relaxed), 0);
    assert_eq!(calls.load(Relaxed), before + 1);
    assert!((out[0][10] - 0.25).abs() < 1e-6);
}

#[test]
fn suspend_then_resume_same_batch() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, calls) = counted_instrument(&mut p, &mut res);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Suspend(7));
    h.send(Command::Suspend(0));
    render(&mut e, 2, &[]);
    assert_eq!(h.shared.suspended.load(Relaxed), 0);
    assert_eq!(calls.load(Relaxed), 2);
}

#[test]
fn only_a_resume_ends_a_suspend() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, calls) = counted_instrument(&mut p, &mut res);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Suspend(7));
    render(&mut e, 1, &[]);
    let before = calls.load(Relaxed);
    let shared = h.shared.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let ui = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Relaxed) {
                h.send(Command::Locate(1));
            }
            h
        })
    };
    let (mut l, mut r) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    for _ in 0..100_000 {
        assert_no_alloc(|| e.process(&[], &mut l, &mut r, &[]));
        assert_eq!(shared.suspended.load(Relaxed), 7, "resumed without Suspend(0)");
    }
    stop.store(true, Relaxed);
    let mut h = ui.join().unwrap();
    assert_eq!(calls.load(Relaxed), before);
    h.send(Command::Suspend(0));
    render(&mut e, 1, &[]);
    assert_eq!(shared.suspended.load(Relaxed), 0);
    assert_eq!(calls.load(Relaxed), before + 1);
}

#[test]
fn commands_after_resume_survive() {
    let p = Project::new("t", SR);
    let res = Res::default();
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Suspend(3));
    render(&mut e, 1, &[]);
    h.send(Command::Locate(100));
    render(&mut e, 1, &[]);
    h.send(Command::Locate(200));
    h.send(Command::Suspend(0));
    h.send(Command::Locate(5000));
    render(&mut e, 1, &[]);
    assert_eq!(h.shared.suspended.load(Relaxed), 0);
    assert_eq!(h.shared.position.load(Relaxed), 5000);
}

#[test]
fn suspend_releases_notes() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Instrument);
    let (sid, rid) = (p.alloc_id(), p.alloc_id());
    let tr = p.track_mut(t).unwrap();
    tr.instrument = Some(slot(sid, None));
    tr.regions.push(Region {
        id: rid,
        name: "m".into(),
        start: 0,
        length: SR as u64 * 4,
        muted: false,
        loops: 0,
        color: None,
        content: Content::Midi(MidiContent { notes: vec![Note::new(10, PPQ * 4, 60, 90)], controllers: vec![], ..Default::default() }),
    });
    let seen = Arc::new(Mutex::new(Vec::with_capacity(64)));
    let mut res = Res::default();
    res.plugins.insert(sid, PluginCell::new(Box::new(Spy(seen.clone())), 0));
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 2, &[]);
    assert_eq!(seen.lock().unwrap().last().map(|e| e.data), Some([0x90, 60, 90]));
    h.send(Command::Suspend(1));
    render(&mut e, 1, &[]);
    assert_eq!(h.shared.suspended.load(Relaxed), 1);
    assert_eq!(seen.lock().unwrap().last().map(|e| e.data), Some([0x80, 60, 0]));
}

/// Tracks through plug-ins with latency, a bus and automation: enough to tell renders apart.
fn mix_project() -> (Project, Res) {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let dir = std::env::temp_dir().join(format!("solstice-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wave: Vec<f32> = (0..3000).map(|i| (i as f32 / 300.0).sin() * 0.5).collect();
    audio::write_wav(&dir.join("mix.wav"), SR, &[wave]).unwrap();
    res.audio.insert("a.wav".into(), Arc::new(AudioData::open(&dir.join("mix.wav")).unwrap()));
    let bus = p.add_bus();
    for k in 0..3u64 {
        let t = audio_track(&mut p, "a.wav", k * 50, 2500);
        let sid = p.alloc_id();
        let ch = &mut p.track_mut(t).unwrap().channel;
        ch.inserts.push(slot(sid, None));
        ch.sends.push(Send { bus, level_db: -6.0, mode: SendMode::PostPan });
        ch.lane_mut(AutoTarget::Pan).points = vec![AutoPoint { pos: 0, value: -0.5 }, AutoPoint { pos: 3000, value: 0.5 }];
        let lag = 30 + k as u32 * 40;
        res.plugins.insert(sid, PluginCell::new(Box::new(Lag(graph::Delay::new(lag).unwrap())), lag));
    }
    (p, res)
}

fn offline_collect(g: Graph, span: std::ops::Range<u64>, workers: usize) -> (offline::Outcome, [Vec<f32>; 2], Option<Box<Graph>>) {
    let progress = offline::Progress::default();
    let mut out = [Vec::new(), Vec::new()];
    let (result, graph) = offline::render(g, SR, span, workers, &progress, &mut |l, r| {
        out[0].extend_from_slice(l);
        out[1].extend_from_slice(r);
        Ok(())
    });
    (result.unwrap(), out, graph)
}

#[test]
fn offline_matches_live() {
    let (start_at, len) = (200u64, 3000u64);
    let (p, res) = mix_project();
    let g = compile(&p, &res, &opts());
    let pdc = g.pdc as usize;
    assert!(pdc > 0);
    let (mut e, mut h, _rec) = new(SR);
    h.set_graph(g);
    h.send(Command::Locate(start_at));
    h.send(Command::Play);
    let blocks = (len as usize + pdc).div_ceil(BLOCK);
    let live = render(&mut e, blocks, &[]);
    let live = [live[0][pdc..pdc + len as usize].to_vec(), live[1][pdc..pdc + len as usize].to_vec()];

    let (p, res) = mix_project();
    let (outcome, out, _) = offline_collect(compile(&p, &res, &opts()), start_at..start_at + len, 2);
    assert_eq!(outcome, offline::Outcome::Finished { frames: len });
    assert!(live[0].iter().any(|v| v.abs() > 0.01));
    assert_eq!(out, live);
}

#[test]
fn offline_uses_max_block() {
    let mut p = Project::new("t", SR);
    audio_track(&mut p, "a.wav", 0, 5000);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("len.wav", 0.5, 5000));
    let (outcome, out, _) = offline_collect(compile(&p, &res, &opts()), 100..1100, 0);
    assert_eq!(outcome, offline::Outcome::Finished { frames: 1000 });
    assert_eq!((out[0].len(), out[1].len()), (1000, 1000));
    assert!(out[0].iter().all(|v| (v - 0.5).abs() < 1e-6));
}

#[test]
fn offline_compensates_latency() {
    let mut p = Project::new("t", SR);
    let t = audio_track(&mut p, "a.wav", 300, 1000);
    let sid = p.alloc_id();
    p.track_mut(t).unwrap().channel.inserts.push(slot(sid, None));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("lat.wav", 0.5, 1000));
    res.plugins.insert(sid, PluginCell::new(Box::new(Lag(graph::Delay::new(100).unwrap())), 100));
    let (_, out, _) = offline_collect(compile(&p, &res, &opts()), 0..2000, 0);
    assert_eq!(out[0].iter().position(|v| v.abs() > 1e-6), Some(300));
    assert_eq!(out[0].iter().rposition(|v| v.abs() > 1e-6), Some(1299));
}

#[test]
fn offline_cancel() {
    let mut p = Project::new("t", SR);
    audio_track(&mut p, "a.wav", 0, 5000);
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("cancel.wav", 0.5, 5000));
    let dir = std::env::temp_dir().join(format!("solstice-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.wav");

    let progress = offline::Progress::default();
    progress.cancel.store(true, Relaxed);
    let (result, graph) = offline::render_wav(compile(&p, &res, &opts()), SR, 0..5000, 0, &progress, &path);
    assert_eq!(result.unwrap(), offline::Outcome::Cancelled);
    assert!(graph.is_some());
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none(), "no file, partial or not, is left");

    let progress = offline::Progress::default();
    let (result, _) = offline::render_wav(compile(&p, &res, &opts()), SR, 0..5000, 0, &progress, &path);
    assert_eq!(result.unwrap(), offline::Outcome::Finished { frames: 5000 });
    assert_eq!(progress.done.load(Relaxed), 5000);
    let data = AudioData::open(&path).unwrap();
    assert_eq!((data.channels, data.frames), (2, 5000));
    assert!((data.sample(1, 4999) - 0.5).abs() < 1e-6);
}

/// Notes which thread drops it.
struct DropSpy(Arc<Mutex<Option<std::thread::ThreadId>>>);

impl Plugin for DropSpy {
    fn process(&mut self, _: &mut [f32], _: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {}
}

impl Drop for DropSpy {
    fn drop(&mut self) {
        *self.0.lock().unwrap() = Some(std::thread::current().id());
    }
}

#[test]
fn offline_returns_graph() {
    let mut p = Project::new("t", SR);
    let t = audio_track(&mut p, "a.wav", 0, 1000);
    let sid = p.alloc_id();
    p.track_mut(t).unwrap().channel.inserts.push(slot(sid, None));
    let dropped = Arc::new(Mutex::new(None));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("ret.wav", 0.5, 1000));
    res.plugins.insert(sid, PluginCell::new(Box::new(DropSpy(dropped.clone())), 0));
    let g = compile(&p, &res, &opts());
    drop(res);
    let graph = std::thread::spawn(move || offline_collect(g, 0..1000, 1).2).join().unwrap();
    assert!(dropped.lock().unwrap().is_none(), "the render thread did not free the plug-in");
    assert_eq!(graph.as_ref().unwrap().tracks[0].channel.inserts.len(), 1);
    drop(graph);
    assert_eq!(*dropped.lock().unwrap(), Some(std::thread::current().id()));
}

#[test]
fn isolate_prunes() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (a, a_calls) = counted_instrument(&mut p, &mut res);
    let (_, b_calls) = counted_instrument(&mut p, &mut res);
    let (_, out, _) = offline_collect(compile(&p, &res, &Options { isolate: Some(a), ..opts() }), 0..1000, 0);
    assert!(a_calls.load(Relaxed) > 0);
    assert_eq!(b_calls.load(Relaxed), 0);
    assert!((out[0][500] - 0.25).abs() < 1e-6);
}

#[test]
fn freeze_render_is_pre_fader() {
    let mut p = Project::new("t", SR);
    let t = audio_track(&mut p, "a.wav", 0, 4000);
    let bus = p.add_bus();
    let sid = p.alloc_id();
    let ch = &mut p.track_mut(t).unwrap().channel;
    ch.volume_db = -20.0;
    ch.sends.push(Send { bus, level_db: 0.0, mode: SendMode::PostPan });
    ch.inserts.push(slot(sid, None));
    ch.lane_mut(AutoTarget::Volume).points = vec![AutoPoint { pos: 0, value: -20.0 }];
    ch.lane_mut(AutoTarget::Pan).points = vec![AutoPoint { pos: 0, value: -1.0 }];
    ch.lane_mut(AutoTarget::Plugin { slot: sid, param: 3 }).points = vec![AutoPoint { pos: 0, value: 0.75 }];
    p.master.eq.enabled = true;
    p.master.eq.bands[0].on = true;
    p.master.eq.bands[0].freq = 200.0;
    let seen = Arc::new(Mutex::new(Vec::with_capacity(16)));
    let mut res = Res::default();
    res.audio.insert("a.wav".into(), constant_file("frz.wav", 0.5, 4000));
    res.plugins.insert(sid, PluginCell::new(Box::new(ParamSpy(seen.clone())), 0));
    let (_, out, _) = offline_collect(compile(&p, &res, &Options { freeze: Some(t), ..opts() }), 0..4000, 0);
    assert!((out[0][3000] - 0.5).abs() < 1e-6 && (out[1][3000] - 0.5).abs() < 1e-6, "{} {}", out[0][3000], out[1][3000]);
    assert_eq!(seen.lock().unwrap().first(), Some(&(3, 0.75)));
}

#[test]
fn frozen_track_plays_file() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (inst, calls) = counted_instrument(&mut p, &mut res);
    let rid = p.alloc_id();
    p.track_mut(inst).unwrap().regions.push(Region {
        id: rid,
        name: "m".into(),
        start: 0,
        length: 2000,
        muted: false,
        loops: 0,
        color: None,
        content: Content::Midi(MidiContent::default()),
    });
    let audio = audio_track(&mut p, "a.wav", 0, 2000);
    let fx = p.alloc_id();
    p.track_mut(audio).unwrap().channel.inserts.push(slot(fx, None));
    let fx_calls = Arc::new(AtomicUsize::new(0));
    res.plugins.insert(fx, PluginCell::new(Box::new(Counter(fx_calls.clone(), 0.0)), 0));
    res.audio.insert("a.wav".into(), constant_file("live.wav", 0.0, 2000));
    res.audio.insert("frozen.wav".into(), constant_file("frozen.wav", 0.25, 2000));
    res.strip(Some(audio)).set(0.0, -1.0, false);
    for id in [inst, audio] {
        let key = freeze_key(&p, p.track(id).unwrap());
        p.track_mut(id).unwrap().freeze = Some(Freeze { file: "frozen.wav".into(), start: 0, frames: 2000, key });
    }
    let solo = |p: &Project, id: Id| {
        let mut p = p.clone();
        p.track_mut(id).unwrap().channel.solo = true;
        p
    };

    let (_, out, _) = offline_collect(compile(&solo(&p, inst), &res, &opts()), 0..1000, 0);
    assert_eq!(calls.load(Relaxed), 0, "a frozen instrument is not played");
    assert!((out[0][500] - 0.25).abs() < 1e-6);

    let (_, out, _) = offline_collect(compile(&solo(&p, audio), &res, &opts()), 0..1000, 0);
    assert_eq!(fx_calls.load(Relaxed), 0);
    let left = 0.25 * std::f32::consts::SQRT_2;
    assert!((out[0][500] - left).abs() < 1e-5 && out[1][500].abs() < 1e-6, "the mono pan law stays: {} {}", out[0][500], out[1][500]);

    let mut stale = solo(&p, inst);
    stale.region_mut(rid).unwrap().length = 3000;
    offline_collect(compile(&stale, &res, &opts()), 0..1000, 0);
    assert!(calls.load(Relaxed) > 0, "a stale freeze plays live");

    calls.store(0, Relaxed);
    res.audio.remove("frozen.wav");
    offline_collect(compile(&solo(&p, inst), &res, &opts()), 0..1000, 0);
    assert!(calls.load(Relaxed) > 0, "a missing freeze file plays live");
}

/// An instrument track whose spy sees every event, with a region one bar long at 480 BPM (24000 samples).
fn chance_track(p: &mut Project, res: &mut Res, notes: Vec<Note>, loops: u32) -> (Id, Arc<Mutex<Vec<MidiEvent>>>) {
    p.tempo = 480.0;
    let t = p.add_track(TrackKind::Instrument);
    let slot = p.alloc_id();
    let rid = p.alloc_id();
    let bar = p.time().bar_len();
    let tr = p.track_mut(t).unwrap();
    tr.instrument = Some(PluginSlot { id: slot, uid: String::new(), path: "".into(), name: "spy".into(), bypass: false, state: vec![], sidechain: None });
    let content = Content::Midi(MidiContent { notes, ..Default::default() });
    tr.regions.push(Region { id: rid, name: "m".into(), start: 0, length: bar, muted: false, loops, color: None, content });
    let seen = Arc::new(Mutex::new(Vec::with_capacity(100_000)));
    res.plugins.insert(slot, PluginCell::new(Box::new(Spy(seen.clone())), 0));
    (t, seen)
}

/// 32 notes a bar on `pitch`, each with `chance`.
fn chancy(pitch: u8, chance: u8) -> Vec<Note> {
    (0..32).map(|i| Note { chance, ..Note::new(i * PPQ / 8, PPQ / 16, pitch, 100) }).collect()
}

fn ons(seen: &Mutex<Vec<MidiEvent>>, pitch: u8) -> usize {
    seen.lock().unwrap().iter().filter(|e| e.data[0] == 0x90 && e.data[1] == pitch).count()
}

/// Renders `blocks` periods from the start and returns where each note-on of `pitch` fell.
fn on_positions(e: &mut Engine, blocks: usize, seen: &Mutex<Vec<MidiEvent>>, pitch: u8) -> Vec<u64> {
    let mut out = Vec::new();
    for b in 0..blocks {
        render(e, 1, &[]);
        let mut v = seen.lock().unwrap();
        out.extend(v.iter().filter(|ev| ev.data[0] == 0x90 && ev.data[1] == pitch).map(|ev| (b * BLOCK) as u64 + ev.frame as u64));
        v.clear();
    }
    out
}

#[test]
fn note_chance_rolls_afresh_each_cycle_pass() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let notes = [chancy(36, 50), chancy(38, 100), chancy(40, 0)].concat();
    let (_, seen) = chance_track(&mut p, &mut res, notes, 0);
    p.cycle = Range::new(0, p.time().bar_len());
    p.cycle_on = true;
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let passes_n = 10;
    render(&mut e, passes_n * 24000 / BLOCK, &[]);
    assert_eq!(ons(&seen, 38), 32 * passes_n);
    assert_eq!(ons(&seen, 40), 0);
    let half = ons(&seen, 36);
    assert!((100..220).contains(&half), "{half} of 320 played");
    let offs = seen.lock().unwrap().iter().filter(|e| e.data[0] == 0x80).count();
    assert!(offs >= 3 * 32 * (passes_n - 1), "every note-off is sent, even for skipped notes");
}

#[test]
fn locating_repeats_the_rolls() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, seen) = chance_track(&mut p, &mut res, chancy(36, 50), 0);
    let (mut e, mut h, _rec) = start(&p, &res);
    let blocks = 24000 / BLOCK;
    h.send(Command::Play);
    render(&mut e, blocks, &[]);
    let first: Vec<MidiEvent> = seen.lock().unwrap().drain(..).filter(|e| e.data[0] == 0x90).collect();
    h.send(Command::Stop);
    h.send(Command::Locate(0));
    h.send(Command::Play);
    render(&mut e, blocks, &[]);
    let second: Vec<MidiEvent> = seen.lock().unwrap().iter().copied().filter(|e| e.data[0] == 0x90).collect();
    assert!(!first.is_empty() && first.len() < 32);
    assert_eq!(first, second);
}

#[test]
fn looped_copies_roll_on_their_own() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let (_, seen) = chance_track(&mut p, &mut res, chancy(36, 50), 3);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    let mut masks = [0u32; 4];
    for pos in on_positions(&mut e, 4 * 24000 / BLOCK, &seen, 36) {
        masks[pos as usize / 24000] |= 1 << (pos % 24000 / 750);
    }
    assert!(masks.windows(2).any(|w| w[0] != w[1]) && masks.iter().all(|m| *m != 0 && *m != u32::MAX), "{masks:x?}");
}

#[test]
fn a_track_added_while_playing_still_rolls() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    chance_track(&mut p, &mut res, chancy(36, 100), 0);
    let (mut e, mut h, _rec) = start(&p, &res);
    h.send(Command::Play);
    render(&mut e, 4, &[]);
    let (_, seen) = chance_track(&mut p, &mut res, chancy(38, 50), 0);
    h.set_graph(compile(&p, &res, &opts()));
    render(&mut e, 24000 / BLOCK, &[]);
    let played = ons(&seen, 38);
    assert!(played > 0 && played < 31, "{played} played");
}

#[test]
fn drum_map_mute_and_solo_leave_notes_out() {
    let mut p = Project::new("t", SR);
    let mut res = Res::default();
    let notes = [chancy(36, 100), chancy(38, 100), chancy(42, 100)].concat();
    let (t, _) = chance_track(&mut p, &mut res, notes, 0);
    let pitches = |p: &Project| {
        let g = compile(p, &res, &opts());
        let mut v: Vec<u8> = g.tracks[0].events.iter().map(|e| e.data[1]).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    assert_eq!(pitches(&p), vec![36, 38, 42]);
    let row = |pitch, mute, solo| DrumRow { pitch, name: String::new(), hidden: false, mute, solo };
    p.track_mut(t).unwrap().drum_map = vec![row(36, true, false)];
    assert_eq!(pitches(&p), vec![38, 42]);
    p.track_mut(t).unwrap().drum_map.push(row(42, false, true));
    assert_eq!(pitches(&p), vec![42]);
}
