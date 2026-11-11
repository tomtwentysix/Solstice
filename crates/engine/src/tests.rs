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
        content: Content::Midi(MidiContent {
            notes: vec![Note { start: 2 * bar as u32, length: PPQ, pitch: 60, velocity: 100 }],
            controllers: vec![],
        }),
    });
    let g = compile(&p, &Res::default(), &opts());
    // A bar at 120 (2 s) plus a bar at 60 (4 s), then a beat at 60 (1 s).
    assert_eq!(g.tracks[0].events[0], (6 * SR as u64, [0x90, 60, 100]));
    assert_eq!(g.tracks[0].events[1].0, 7 * SR as u64);
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

#[test]
fn count_in_delays_the_take() {
    let mut p = Project::new("t", SR);
    let t = p.add_track(TrackKind::Audio);
    p.track_mut(t).unwrap().armed = true;
    let (prod, cons) = rtrb::RingBuffer::new(SR as usize);
    let mut res = Res::default();
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
