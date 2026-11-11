pub mod audio;
pub mod eq;
mod graph;
pub mod monitor;
pub mod pool;
pub mod recorder;

pub use graph::{Graph, Options, Resources, compile, pan_gains};

use monitor::{MonitorState, monitor_source};
use rtrb::{Consumer, Producer, RingBuffer};
use solstice_model::Id;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MidiEvent {
    pub frame: u32,
    pub data: [u8; 3],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessContext {
    pub position: u64,
    pub tempo: f64,
    pub sig_num: u8,
    pub sig_den: u8,
    pub playing: bool,
}

/// A stereo in-place processor. Called only from the RT thread.
pub trait Plugin: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[MidiEvent], ctx: &ProcessContext);

    /// A host automation value (normalised 0..1) for the next `process` call.
    fn set_param(&mut self, _id: u32, _value: f64) {}

    /// The side-chain key for the next `process` call, or `None` when there is none.
    fn sidechain(&mut self, _key: Option<[&[f32]; 2]>) {}

    /// MIDI effects in a track's MIDI FX slots: rewrite `input` into `out`, sorted by frame.
    /// `out` is preallocated; push only while `out.len() < out.capacity()`.
    fn process_midi(&mut self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _frames: usize, _ctx: &ProcessContext) {
        out.extend_from_slice(&input[..input.len().min(out.capacity())]);
    }
}

pub struct PluginCell {
    inner: UnsafeCell<Box<dyn Plugin>>,
    pub bypass: AtomicBool,
    pub latency: AtomicU32,
}

// The RT thread is the only caller of `process`; everything else is atomics.
unsafe impl Sync for PluginCell {}

impl PluginCell {
    pub fn new(plugin: Box<dyn Plugin>, latency: u32) -> Arc<Self> {
        Arc::new(PluginCell { inner: UnsafeCell::new(plugin), bypass: AtomicBool::new(false), latency: AtomicU32::new(latency) })
    }

    fn process(&self, l: &mut [f32], r: &mut [f32], events: &[MidiEvent], ctx: &ProcessContext) {
        unsafe { (*self.inner.get()).process(l, r, events, ctx) }
    }

    fn set_param(&self, id: u32, value: f64) {
        unsafe { (*self.inner.get()).set_param(id, value) }
    }

    fn sidechain(&self, key: Option<[&[f32]; 2]>) {
        unsafe { (*self.inner.get()).sidechain(key) }
    }

    fn process_midi(&self, input: &[MidiEvent], out: &mut Vec<MidiEvent>, frames: usize, ctx: &ProcessContext) {
        unsafe { (*self.inner.get()).process_midi(input, out, frames, ctx) }
    }

    pub fn is_bypassed(&self) -> bool {
        self.bypass.load(Relaxed)
    }

    pub fn latency(&self) -> u32 {
        self.latency.load(Relaxed)
    }
}

#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        AtomicF32(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

/// Live channel controls shared between the faders and the RT thread, plus its meter.
pub struct Strip {
    gain: AtomicF32,
    pan: AtomicF32,
    pub mute: AtomicBool,
    peaks: [AtomicU32; 2],
    /// Gains the RT thread applied last period, kept here so a graph swap doesn't re-ramp.
    applied: [AtomicF32; 3],
    /// Channel EQ filter memory. Lives here rather than in the graph so it survives the graph
    /// rebuilds that happen while a band is dragged. Only the RT thread touches it.
    eq_state: UnsafeCell<eq::EqState>,
    /// Post-EQ mono signal for the EQ analyser, written only while `scope_on` is set.
    scope: Box<[AtomicU32]>,
    scope_pos: AtomicUsize,
    pub scope_on: AtomicBool,
    /// Set by the UI while a fader, knob or button of this channel is held (Touch and Latch).
    pub touched: AtomicBool,
    latched: AtomicBool,
    /// What automation is currently setting, so faders can follow it during playback.
    pub auto_active: AtomicBool,
    pub auto_db: AtomicF32,
    pub auto_pan: AtomicF32,
}

pub const SCOPE_LEN: usize = 8192;

// The RT thread is the only user of `eq_state`; everything else is atomic.
unsafe impl Sync for Strip {}

impl Default for Strip {
    fn default() -> Self {
        Strip {
            gain: AtomicF32::new(1.0),
            pan: AtomicF32::new(0.0),
            mute: AtomicBool::new(false),
            peaks: Default::default(),
            applied: [AtomicF32::new(-1.0), AtomicF32::new(-1.0), AtomicF32::new(-1.0)],
            eq_state: UnsafeCell::new(eq::EqState::default()),
            scope: (0..SCOPE_LEN).map(|_| AtomicU32::new(0)).collect(),
            scope_pos: AtomicUsize::new(0),
            scope_on: AtomicBool::new(false),
            touched: AtomicBool::new(false),
            latched: AtomicBool::new(false),
            auto_active: AtomicBool::new(false),
            auto_db: AtomicF32::new(0.0),
            auto_pan: AtomicF32::new(0.0),
        }
    }
}

impl Strip {
    pub fn gain(&self) -> f32 {
        self.gain.get()
    }
    pub fn pan(&self) -> f32 {
        self.pan.get()
    }
    pub fn set(&self, volume_db: f32, pan: f32, mute: bool) {
        self.gain.set(solstice_model::db_to_gain(volume_db));
        self.pan.set(pan);
        self.mute.store(mute, Relaxed);
    }

    /// Returns the fader and pan gains to ramp from, jumping straight to `target` on first use.
    fn ramp_from(&self, target: [f32; 3]) -> [f32; 3] {
        let prev: [f32; 3] = std::array::from_fn(|k| self.applied[k].get());
        for (a, t) in self.applied.iter().zip(target) {
            a.set(t);
        }
        if prev[0] < 0.0 { target } else { prev }
    }

    fn apply_eq(&self, c: &eq::EqCoeffs, l: &mut [f32], r: &mut [f32]) {
        unsafe { (*self.eq_state.get()).process(c, l, r) }
        if self.scope_on.load(Relaxed) {
            let mut pos = self.scope_pos.load(Relaxed);
            for (a, b) in l.iter().zip(r.iter()) {
                self.scope[pos].store(((a + b) * 0.5).to_bits(), Relaxed);
                pos = (pos + 1) % SCOPE_LEN;
            }
            self.scope_pos.store(pos, Relaxed);
        }
    }

    /// The last `SCOPE_LEN` post-EQ samples, oldest first.
    pub fn scope(&self) -> Vec<f32> {
        let pos = self.scope_pos.load(Relaxed);
        (0..SCOPE_LEN).map(|i| f32::from_bits(self.scope[(pos + i) % SCOPE_LEN].load(Relaxed))).collect()
    }

    fn meter(&self, l: &[f32], r: &[f32]) {
        for (peak, buf) in self.peaks.iter().zip([l, r]) {
            let p = buf.iter().fold(0f32, |m, s| m.max(s.abs()));
            // Non-negative floats order the same as their bit patterns.
            peak.fetch_max(p.to_bits(), Relaxed);
        }
    }

    /// Peak since the last call, per channel.
    pub fn take_peaks(&self) -> [f32; 2] {
        [f32::from_bits(self.peaks[0].swap(0, Relaxed)), f32::from_bits(self.peaks[1].swap(0, Relaxed))]
    }
}

#[derive(Default)]
pub struct Shared {
    pub position: AtomicU64,
    pub playing: AtomicBool,
    pub recording: AtomicBool,
    /// DSP time as a fraction of the buffer period.
    pub load: AtomicF32,
    pub xruns: AtomicU32,
    /// Plug-in delay compensation at the master output, in frames.
    pub pdc: AtomicU32,
    /// Worker threads that reached real-time priority and share the processing.
    pub workers: Arc<AtomicUsize>,
    pub overruns: AtomicU32,
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Play,
    Stop,
    Locate(u64),
    Record(bool),
    /// Plays `start..end` and writes the master output as a take for `BOUNCE_TRACK`.
    Bounce { start: u64, end: u64 },
    /// A MIDI message for the live MIDI track, as if played on a keyboard.
    Midi([u8; 3]),
}

pub const BOUNCE_TRACK: Id = Id::MAX;

struct SharedSlice(*mut graph::TrackNode);
unsafe impl Sync for SharedSlice {}

impl SharedSlice {
    /// Safety: `i` is in bounds and no other thread holds the same index.
    unsafe fn get(&self, i: usize) -> &mut graph::TrackNode {
        unsafe { &mut *self.0.add(i) }
    }

    /// Safety: `i` is in bounds and no thread is writing to that track.
    unsafe fn read(&self, i: usize) -> &graph::TrackNode {
        unsafe { &*self.0.add(i) }
    }
}

/// One track's own work for a period: MIDI FX, instrument and channel strip. Touches nothing
/// outside the track, so tracks can run in parallel.
#[allow(clippy::too_many_arguments)]
fn process_track<'k>(
    t: &mut graph::TrackNode,
    key_of: &dyn Fn(usize) -> Option<[&'k [f32]; 2]>,
    n: usize,
    ctx: &ProcessContext,
    playing: bool,
    recording: bool,
    auto_input: bool,
    low_latency: Option<u32>,
) {
    for fx in &t.midi_fx {
        if !fx.is_bypassed() {
            t.fx_buf.clear();
            fx.process_midi(&t.ev_buf, &mut t.fx_buf, n, ctx);
            std::mem::swap(&mut t.ev_buf, &mut t.fx_buf);
        }
    }
    let [l, r] = &mut t.channel.buf;
    if let Some(inst) = &t.instrument {
        inst.process(&mut l[..n], &mut r[..n], &t.ev_buf, ctx);
    }
    let src = monitor_source(MonitorState {
        armed: t.armed && t.input.is_some(),
        monitor: t.monitor && t.input.is_some(),
        playing,
        recording,
        in_punch: true,
        auto_input,
    });
    let skip = if src.input { low_latency } else { None };
    t.channel.process(n, skip, ctx, key_of);
}

/// A note played live, from a MIDI input or the UI, as the UI hears about it.
#[derive(Clone, Copy, Debug)]
pub struct LiveNote {
    pub pos: u64,
    pub data: [u8; 3],
    pub recording: bool,
    pub playing: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum RecEvent {
    Start { track: Id, pos: u64, channels: u16 },
    Stop { track: Id, frames: u64 },
}

pub struct Handle {
    pub commands: Producer<Command>,
    pub graphs: Producer<Box<Graph>>,
    pub garbage: Consumer<Box<Graph>>,
    pub live_notes: Consumer<LiveNote>,
    pub shared: Arc<Shared>,
}

impl Handle {
    pub fn send(&mut self, cmd: Command) {
        let _ = self.commands.push(cmd);
    }

    /// Queues a new graph and frees any the RT thread has handed back.
    pub fn set_graph(&mut self, g: Graph) {
        self.collect();
        let _ = self.graphs.push(Box::new(g));
    }

    pub fn collect(&mut self) {
        while self.garbage.pop().is_ok() {}
    }
}

pub struct Engine {
    graph: Option<Box<Graph>>,
    graphs: Consumer<Box<Graph>>,
    garbage: Producer<Box<Graph>>,
    commands: Consumer<Command>,
    rec_events: Producer<RecEvent>,
    live_notes: Producer<LiveNote>,
    shared: Arc<Shared>,
    sample_rate: u32,
    pos: u64,
    playing: bool,
    recording: bool,
    click: Click,
    bounce_ring: Option<Producer<f32>>,
    bounce: Option<(u64, u64)>,
    bounce_written: u64,
    /// Frames of the bounce still to drop at the start: the compensation delay.
    bounce_skip: u64,
    ui_midi: [MidiEvent; 64],
    ui_midi_len: usize,
    pool: Option<pool::Pool>,
}

pub fn new(sample_rate: u32) -> (Engine, Handle, Consumer<RecEvent>) {
    let (cmd_tx, cmd_rx) = RingBuffer::new(256);
    let (graph_tx, graph_rx) = RingBuffer::new(16);
    let (garbage_tx, garbage_rx) = RingBuffer::new(64);
    let (rec_tx, rec_rx) = RingBuffer::new(1024);
    let (midi_tx, midi_rx) = RingBuffer::new(8192);
    let shared = Arc::new(Shared::default());
    let engine = Engine {
        graph: None,
        graphs: graph_rx,
        garbage: garbage_tx,
        commands: cmd_rx,
        rec_events: rec_tx,
        live_notes: midi_tx,
        shared: shared.clone(),
        sample_rate,
        pos: 0,
        playing: false,
        recording: false,
        click: Click::default(),
        bounce_ring: None,
        bounce: None,
        bounce_written: 0,
        bounce_skip: 0,
        ui_midi: [MidiEvent::default(); 64],
        ui_midi_len: 0,
        pool: None,
    };
    let handle = Handle { commands: cmd_tx, graphs: graph_tx, garbage: garbage_rx, live_notes: midi_rx, shared };
    (engine, handle, rec_rx)
}

impl Engine {
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// Spreads track processing over `count` extra threads started by `spawn`. Workers
    /// without real-time scheduling sit out when `require_rt` is set. Call before starting.
    pub fn set_workers(&mut self, count: usize, spawn: pool::Spawn, require_rt: bool) {
        self.pool = (count > 0).then(|| pool::Pool::new(count, spawn, require_rt, self.shared.workers.clone()));
    }

    pub fn workers(&self) -> usize {
        self.pool.as_ref().map_or(0, |p| p.workers())
    }

    /// One audio period. Allocation-free and lock-free.
    pub fn process(&mut self, inputs: &[&[f32]], out_l: &mut [f32], out_r: &mut [f32], midi_in: &[MidiEvent]) {
        let started = Instant::now();
        let n = out_l.len();
        out_l.fill(0.0);
        out_r.fill(0.0);
        self.swap_graph();
        let Some(mut g) = self.graph.take() else { return };
        let n = n.min(g.max_block);

        for t in &mut g.tracks {
            t.channel.clear(n);
            t.ev_buf.clear();
        }
        for b in &mut g.bus_in {
            b[0][..n].fill(0.0);
            b[1][..n].fill(0.0);
        }
        g.master.clear(n);

        self.run_commands(&mut g);
        let ctx = ProcessContext {
            position: self.pos,
            tempo: g.time.tempo_at(self.pos),
            sig_num: g.time.sig_at(self.pos).0,
            sig_den: g.time.sig_at(self.pos).1,
            playing: self.playing,
        };

        if let Some(i) = g.midi_track {
            let t = &mut g.tracks[i];
            let ui = self.ui_midi[..self.ui_midi_len].iter();
            let hw = midi_in.iter().filter(|e| (e.frame as usize) < n);
            for (e, from_ui) in ui.map(|e| (e, true)).chain(hw.map(|e| (e, false))) {
                if from_ui || g.midi_thru {
                    t.push_event(e.frame, e.data);
                }
                if matches!(e.data[0] & 0xf0, 0x80 | 0x90) {
                    let pos = if self.playing { (self.pos + e.frame as u64).saturating_sub(g.pdc as u64) } else { self.pos };
                    let _ = self.live_notes.push(LiveNote {
                        pos,
                        data: e.data,
                        recording: self.recording && self.playing,
                        playing: self.playing,
                    });
                }
            }
        }

        self.ui_midi_len = 0;
        let block_start = self.pos;
        let mut off = 0;
        while off < n {
            if self.bounce.is_some() {
                g.cycle = None;
            }
            let len = if self.playing { self.segment_len(&g, n - off) } else { n - off };
            self.run_segment(&mut g, inputs, off, len);
            off += len;
        }

        if let Some(i) = g.midi_track {
            // Stable and allocation-free; live events are few and mostly in order already.
            let ev = &mut g.tracks[i].ev_buf;
            for j in 1..ev.len() {
                let mut k = j;
                while k > 0 && ev[k - 1].frame > ev[k].frame {
                    ev.swap(k - 1, k);
                    k -= 1;
                }
            }
        }

        let Graph { tracks, buses, bus_in, bus_order, master, auto_input, low_latency, waves, .. } = &mut *g;
        let (playing, recording, auto_input, low_latency) = (self.playing, self.recording, *auto_input, *low_latency);
        let shared_tracks = SharedSlice(tracks.as_mut_ptr());
        // Tracks keyed by another track's side-chain go in a second wave, once their keys
        // are ready; within a wave each index is handed out once, so no two threads share a track.
        for wave in [&*waves.0, &*waves.1] {
            let key_of = |src: usize| {
                let k = unsafe { shared_tracks.read(src) };
                Some([&k.channel.buf[0][..n], &k.channel.buf[1][..n]])
            };
            let strip = |w: usize| {
                let t = unsafe { shared_tracks.get(wave[w]) };
                process_track(t, &key_of, n, &ctx, playing, recording, auto_input, low_latency);
            };
            match &self.pool {
                Some(pool) => pool.run(wave.len(), &strip),
                None => (0..wave.len()).for_each(strip),
            }
        }
        for t in tracks.iter_mut() {
            t.channel.route(n, bus_in);
            let dst = match t.channel.output {
                Some(b) => &mut bus_in[b],
                None => &mut master.buf,
            };
            add(dst, &t.channel.buf, n);
        }
        for &i in bus_order.iter() {
            let bus = &mut buses[i];
            for c in 0..2 {
                bus.buf[c][..n].copy_from_slice(&bus_in[i][c][..n]);
            }
            let key_of = |src: usize| tracks.get(src).map(|t| [&t.channel.buf[0][..n], &t.channel.buf[1][..n]]);
            bus.run(n, None, bus_in, &ctx, &key_of);
            let dst = match bus.output {
                Some(b) => &mut bus_in[b],
                None => &mut master.buf,
            };
            add(dst, &bus.buf, n);
        }
        let key_of = |src: usize| tracks.get(src).map(|t| [&t.channel.buf[0][..n], &t.channel.buf[1][..n]]);
        master.run(n, None, &mut [], &ctx, &key_of);
        out_l[..n].copy_from_slice(&master.buf[0][..n]);
        out_r[..n].copy_from_slice(&master.buf[1][..n]);
        if let Some((_, end)) = self.bounce {
            // `end` includes the compensation delay; the first `bounce_skip` frames are its lead-in.
            let frames = (end.saturating_sub(block_start) as usize).min(n);
            let skip = (self.bounce_skip as usize).min(frames);
            self.bounce_skip -= skip as u64;
            let len = frames - skip;
            if let Some(ring) = self.bounce_ring.as_mut()
                && let Ok(mut chunk) = ring.write_chunk_uninit(len * 2)
            {
                let (s1, s2) = chunk.as_mut_slices();
                for k in 0..len * 2 {
                    let f = skip + k / 2;
                    let v = if k % 2 == 0 { out_l[f] } else { out_r[f] };
                    if k < s1.len() { s1[k].write(v) } else { s2[k - s1.len()].write(v) };
                }
                unsafe { chunk.commit_all() };
                self.bounce_written += len as u64;
            }
            if block_start + n as u64 >= end {
                self.finish_bounce();
                self.playing = false;
                self.pos = end - g.pdc as u64;
            }
        }
        self.click.render(&mut out_l[..n], &mut out_r[..n], self.sample_rate, g.click_gain, g.click_accent);

        self.graph = Some(g);
        self.shared.position.store(self.pos, Relaxed);
        self.shared.playing.store(self.playing, Relaxed);
        self.shared.recording.store(self.recording, Relaxed);
        let budget = n as f64 / self.sample_rate as f64;
        self.shared.load.set((started.elapsed().as_secs_f64() / budget) as f32);
    }

    fn swap_graph(&mut self) {
        let mut newest = None;
        while let Ok(g) = self.graphs.pop() {
            if let Some(old) = newest.replace(g) {
                self.discard(old);
            }
        }
        let Some(mut new) = newest else { return };
        if let Some(ring) = new.bounce_ring.take() {
            if let Some(old) = self.bounce_ring.replace(ring) {
                new.bounce_ring = Some(old);
            }
        }
        if let Some(mut old) = self.graph.take() {
            for nb in &mut new.buses {
                if let Some(ob) = old.buses.iter().find(|b| Arc::ptr_eq(&b.strip, &nb.strip)) {
                    nb.inherit(ob);
                }
            }
            new.master.inherit(&old.master);
            for nt in &mut new.tracks {
                if let Some(ot) = old.tracks.iter_mut().find(|t| t.id == nt.id) {
                    nt.channel.inherit(&ot.channel);
                    let fresh = nt.rt.rec.take();
                    std::mem::swap(&mut nt.rt, &mut ot.rt);
                    if fresh.is_some() {
                        // The old ring stays with the old graph and is freed on the UI thread.
                        ot.rt.rec = std::mem::replace(&mut nt.rt.rec, fresh);
                    }
                }
            }
            for ot in old.tracks.iter_mut().filter(|t| t.rt.rec_active) {
                let still = new.tracks.iter().any(|t| t.id == ot.id && t.armed);
                if !still {
                    let _ = self.rec_events.push(RecEvent::Stop { track: ot.id, frames: ot.rt.rec_written });
                }
            }
            for nt in new.tracks.iter_mut().filter(|t| t.rt.rec_active && !t.armed) {
                nt.rt.rec_active = false;
            }
            self.discard(old);
        }
        self.shared.pdc.store(new.pdc, Relaxed);
        self.graph = Some(new);
    }

    fn discard(&mut self, g: Box<Graph>) {
        if let Err(rtrb::PushError::Full(g)) = self.garbage.push(g) {
            // Never free on the RT thread; leaking beats an xrun if the UI has stalled.
            std::mem::forget(g);
        }
    }

    fn run_commands(&mut self, g: &mut Graph) {
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Play => {
                    // With cycle on, playback always starts from the top of the cycle.
                    if !self.playing
                        && let Some(c) = g.cycle
                    {
                        self.pos = c.start;
                    }
                    self.playing = true;
                }
                Command::Stop => {
                    self.finish_bounce();
                    self.playing = false;
                    self.recording = false;
                    self.stop_takes(g);
                    self.release_all(g, 0);
                }
                Command::Locate(pos) => {
                    self.pos = pos;
                    self.stop_takes(g);
                    self.release_all(g, 0);
                }
                Command::Bounce { start, end } if self.bounce_ring.is_some() && end > start => {
                    self.pos = start;
                    self.playing = true;
                    self.recording = false;
                    self.bounce = Some((start, end + g.pdc as u64));
                    self.bounce_skip = g.pdc as u64;
                    self.bounce_written = 0;
                    self.release_all(g, 0);
                    let _ = self.rec_events.push(RecEvent::Start { track: BOUNCE_TRACK, pos: start, channels: 2 });
                }
                Command::Bounce { .. } => {}
                Command::Midi(data) => {
                    if self.ui_midi_len < self.ui_midi.len() {
                        self.ui_midi[self.ui_midi_len] = MidiEvent { frame: 0, data };
                        self.ui_midi_len += 1;
                    }
                }
                Command::Record(on) => {
                    self.recording = on;
                    if on {
                        if !self.playing
                            && let Some(c) = g.cycle
                        {
                            self.pos = c.start;
                        }
                        self.playing = true;
                    } else {
                        self.stop_takes(g);
                    }
                }
            }
        }
    }

    fn finish_bounce(&mut self) {
        if self.bounce.take().is_some() {
            let _ = self.rec_events.push(RecEvent::Stop { track: BOUNCE_TRACK, frames: self.bounce_written });
        }
    }

    fn stop_takes(&mut self, g: &mut Graph) {
        for t in g.tracks.iter_mut().filter(|t| t.rt.rec_active) {
            t.rt.rec_active = false;
            let _ = self.rec_events.push(RecEvent::Stop { track: t.id, frames: t.rt.rec_written });
        }
    }

    fn release_all(&mut self, g: &mut Graph, frame: u32) {
        for t in &mut g.tracks {
            t.release_all(frame);
        }
    }

    /// Frames until the next cycle wrap or punch boundary, capped at `max`.
    fn segment_len(&self, g: &Graph, max: usize) -> usize {
        let mut len = max as u64;
        let mut limit = |b: u64| {
            if b > self.pos {
                len = len.min(b - self.pos);
            }
        };
        if let Some(c) = g.cycle {
            limit(c.end);
        }
        if let Some(p) = g.punch {
            limit(p.start);
            limit(p.end);
        }
        len.max(1) as usize
    }

    fn monitor(&self, t: &graph::TrackNode, auto_input: bool, in_punch: Option<bool>) -> monitor::MonitorSource {
        monitor_source(MonitorState {
            armed: t.armed && t.input.is_some(),
            monitor: t.monitor && t.input.is_some(),
            playing: self.playing,
            recording: self.recording,
            in_punch: in_punch.unwrap_or(true),
            auto_input,
        })
    }

    fn run_segment(&mut self, g: &mut Graph, inputs: &[&[f32]], off: usize, len: usize) {
        let pos = self.pos;
        let in_punch = g.punch.is_none_or(|p| p.contains(pos));
        let auto_input = g.auto_input;
        let pdc = g.pdc as u64;
        for i in 0..g.tracks.len() {
            let src = self.monitor(&g.tracks[i], auto_input, Some(in_punch));
            let t = &mut g.tracks[i];
            if src.playback {
                t.read_regions(pos, len, off);
                t.read_events(pos, len, off);
            }
            if src.input && inputs.len() > 0 {
                t.add_input(inputs, off, len, off);
            }
            self.record(t, inputs, pos, off, len, in_punch, pdc);
        }
        if self.playing {
            if g.metronome {
                // The click goes straight to the output, so it waits out the plug-in delay too.
                let p = pos as i64 - g.pdc as i64;
                let skip = (-p).max(0) as usize;
                if skip < len {
                    self.click.schedule(&g.time, (p + skip as i64) as u64, len - skip, off + skip);
                }
            }
            self.pos += len as u64;
            if let Some(c) = g.cycle
                && self.pos >= c.end
                && pos < c.end
            {
                self.pos = c.start;
                self.stop_takes(g);
                self.release_all(g, (off + len - 1) as u32);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record(&mut self, t: &mut graph::TrackNode, inputs: &[&[f32]], pos: u64, off: usize, len: usize, in_punch: bool, pdc: u64) {
        let Some((a, b)) = t.input else { return };
        let want = self.recording && self.playing && t.armed && in_punch && t.rt.rec.is_some();
        if want && !t.rt.rec_active {
            let channels = if b.is_some() { 2 } else { 1 };
            // What's played along to was heard `pdc` frames late, so the take belongs earlier.
            let _ = self.rec_events.push(RecEvent::Start { track: t.id, pos: pos.saturating_sub(pdc), channels });
            t.rt.rec_active = true;
            t.rt.rec_written = 0;
        } else if !want && t.rt.rec_active {
            t.rt.rec_active = false;
            let _ = self.rec_events.push(RecEvent::Stop { track: t.id, frames: t.rt.rec_written });
        }
        if !t.rt.rec_active {
            return;
        }
        let rec = t.rt.rec.as_mut().unwrap();
        let channels = if b.is_some() { 2 } else { 1 };
        // Writing partial frames would desynchronise the file, so a full ring drops whole blocks.
        if rec.slots() < len * channels {
            self.shared.overruns.fetch_add(1, Relaxed);
        } else if let Ok(mut chunk) = rec.write_chunk_uninit(len * channels) {
            let (s1, s2) = chunk.as_mut_slices();
            let mut k = 0;
            for i in 0..len {
                for ch in [Some(a), b].into_iter().flatten() {
                    let v = inputs[ch][off + i];
                    let slot = if k < s1.len() { &mut s1[k] } else { &mut s2[k - s1.len()] };
                    slot.write(v);
                    k += 1;
                }
            }
            unsafe { chunk.commit_all() };
        }
        t.rt.rec_written += len as u64;
    }
}

fn add(dst: &mut [Vec<f32>; 2], src: &[Vec<f32>; 2], n: usize) {
    for c in 0..2 {
        for (d, s) in dst[c][..n].iter_mut().zip(&src[c][..n]) {
            *d += s;
        }
    }
}

/// Metronome: a short decaying sine, higher on the downbeat.
#[derive(Default)]
struct Click {
    pending: Option<(usize, bool)>,
    phase: f32,
    remaining: usize,
    accent: bool,
}

impl Click {
    fn schedule(&mut self, time: &solstice_model::TimeMap, pos: u64, len: usize, off: usize) {
        let (at, accent) = time.next_beat(pos);
        if at >= pos && at < pos + len as u64 {
            self.pending = Some((off + (at - pos) as usize, accent));
        }
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32], sample_rate: u32, gain: f32, accent: bool) {
        let total = (sample_rate / 30) as usize;
        let pending = self.pending.take();
        for i in 0..l.len() {
            if let Some((at, accent)) = pending
                && at == i
            {
                self.remaining = total;
                self.phase = 0.0;
                self.accent = accent;
            }
            if self.remaining == 0 {
                continue;
            }
            let freq = if self.accent && accent { 1760.0 } else { 1320.0 };
            let env = self.remaining as f32 / total as f32;
            let s = (self.phase * std::f32::consts::TAU).sin() * env * env * gain;
            self.phase = (self.phase + freq / sample_rate as f32).fract();
            self.remaining -= 1;
            l[i] += s;
            r[i] += s;
        }
    }
}

#[cfg(test)]
mod tests;
