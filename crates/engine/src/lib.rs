pub mod audio;
pub mod eq;
mod graph;
pub mod measure;
pub mod monitor;
pub mod offline;
pub mod pool;
pub mod recorder;

pub use graph::{Graph, Options, Resources, compile, pan_gains, side_chain_keys};

use monitor::{MonitorState, monitor_source};
use rtrb::{Consumer, Producer, RingBuffer};
use solstice_model::{Beat, Id};
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed, Ordering::Release};
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
    /// Level matching for a reference track, in dB on top of its fader; set by the UI.
    pub match_db: AtomicF32,
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
            match_db: AtomicF32::new(0.0),
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
    /// The token of the last `Suspend` the RT thread has obeyed, or 0 while it runs.
    pub suspended: AtomicU32,
    /// Frames takes are moved back for the interface: its round trip plus the user's adjustment.
    pub record_offset: AtomicU32,
    /// One second of input captured by `Command::Measure`, complete once `measured` is set.
    pub measure: Box<[AtomicF32]>,
    pub measured: AtomicBool,
    /// Set while a ring from `Handle::tap_master` should be fed; clearing it hands the ring back.
    pub master_tap_on: AtomicBool,
    /// Frames of master output the tap ring had no room for.
    pub master_tap_dropped: AtomicU64,
    /// The main outputs play the reference tracks instead of the mix.
    pub reference: AtomicBool,
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Play,
    Stop,
    Locate(u64),
    Record(bool),
    /// Starts recording after this many frames of count-in, played with the click.
    RecordCountIn(u64),
    /// Plays `start..end` and writes the master output as a take for `BOUNCE_TRACK`.
    Bounce { start: u64, end: u64 },
    /// A MIDI message for the live MIDI track, as if played on a keyboard.
    Midi([u8; 3]),
    /// A non-zero token stops the transport and then leaves the graph, and so every plug-in,
    /// alone until `Suspend(0)`; `Shared::suspended` holds the token once that is so.
    Suspend(u32),
    /// Stops the transport, plays `measure::probe` alone out of interface output `output` (0 and
    /// 1 are the main pair) and captures input `input` into `Shared::measure`.
    Measure { output: u8, input: u8 },
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
    /// Which pass over the cycle this was recorded in, counting from 0.
    pub pass: u32,
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
    taps: Producer<Producer<f32>>,
    tap_garbage: Consumer<Producer<f32>>,
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
        while self.tap_garbage.pop().is_ok() {}
    }

    /// Starts copying the master output, as sent to the interface, into a new ring of `frames`
    /// interleaved stereo frames and returns its reading end. A period that doesn't fit is
    /// dropped whole and counted in `Shared::master_tap_dropped`.
    pub fn tap_master(&mut self, frames: usize) -> Consumer<f32> {
        self.collect();
        let (tx, rx) = RingBuffer::new(frames * 2);
        self.shared.master_tap_on.store(true, Relaxed);
        let _ = self.taps.push(tx);
        rx
    }

    pub fn untap_master(&mut self) {
        self.shared.master_tap_on.store(false, Relaxed);
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
    /// Recording waits until the playhead reaches this, after a count-in.
    rec_from: u64,
    /// Cycle wraps since recording last started.
    rec_pass: u32,
    /// Whether the playhead last reached the cycle start by wrapping, rather than by a start or
    /// locate: only then can a note just after the cycle start belong to the previous pass.
    wrapped: bool,
    /// Takes from before the last wrap still run, until the input from that pass has come in.
    tail: bool,
    ui_midi: [MidiEvent; 64],
    ui_midi_len: usize,
    pool: Option<pool::Pool>,
    master_tap: Option<Producer<f32>>,
    taps: Consumer<Producer<f32>>,
    tap_garbage: Producer<Producer<f32>>,
    suspended: bool,
    suspend_pending: u32,
    measure: Option<Measure>,
    /// How far the main outputs have crossfaded from the mix (0) to the reference tracks (1).
    reference_mix: f32,
}

struct Measure {
    output: usize,
    input: usize,
    done: usize,
}

pub fn new(sample_rate: u32) -> (Engine, Handle, Consumer<RecEvent>) {
    let (cmd_tx, cmd_rx) = RingBuffer::new(256);
    let (graph_tx, graph_rx) = RingBuffer::new(16);
    let (garbage_tx, garbage_rx) = RingBuffer::new(64);
    let (rec_tx, rec_rx) = RingBuffer::new(1024);
    let (midi_tx, midi_rx) = RingBuffer::new(8192);
    let (tap_tx, tap_rx) = RingBuffer::new(4);
    let (tap_garbage_tx, tap_garbage_rx) = RingBuffer::new(8);
    let measure = (0..sample_rate).map(|_| AtomicF32::default()).collect();
    let shared = Arc::new(Shared { measure, ..Default::default() });
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
        rec_from: 0,
        rec_pass: 0,
        wrapped: false,
        tail: false,
        ui_midi: [MidiEvent::default(); 64],
        ui_midi_len: 0,
        pool: None,
        master_tap: None,
        taps: tap_rx,
        tap_garbage: tap_garbage_tx,
        suspended: false,
        suspend_pending: 0,
        measure: None,
        reference_mix: 0.0,
    };
    let handle = Handle {
        commands: cmd_tx,
        graphs: graph_tx,
        garbage: garbage_rx,
        live_notes: midi_rx,
        shared,
        taps: tap_tx,
        tap_garbage: tap_garbage_rx,
    };
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

    /// One audio period with only the main pair. Allocation-free and lock-free.
    pub fn process(&mut self, inputs: &[&[f32]], out_l: &mut [f32], out_r: &mut [f32], midi_in: &[MidiEvent]) {
        self.process_outputs(inputs, out_l, out_r, &mut [], midi_in);
    }

    /// One audio period. `extra` are the interface outputs after the main pair, from output 3 on.
    pub fn process_outputs(
        &mut self,
        inputs: &[&[f32]],
        out_l: &mut [f32],
        out_r: &mut [f32],
        extra: &mut [&mut [f32]],
        midi_in: &[MidiEvent],
    ) {
        let started = Instant::now();
        let n = out_l.len();
        out_l.fill(0.0);
        out_r.fill(0.0);
        for o in extra.iter_mut() {
            o.fill(0.0);
        }
        if self.suspended {
            // Only a resume matters; it stays queued so run_commands keeps what follows it.
            loop {
                match self.commands.peek() {
                    Ok(Command::Suspend(0)) => break,
                    Ok(_) => {
                        let _ = self.commands.pop();
                    }
                    Err(_) => {
                        self.shared.load.set(0.0);
                        return;
                    }
                }
            }
            self.suspended = false;
            self.shared.suspended.store(0, Release);
        }
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
            let (cycle, pdc) = (g.cycle, g.pdc as u64);
            let t = &mut g.tracks[i];
            let ui = self.ui_midi[..self.ui_midi_len].iter();
            let hw = midi_in.iter().filter(|e| (e.frame as usize) < n);
            for (e, from_ui) in ui.map(|e| (e, true)).chain(hw.map(|e| (e, false))) {
                if from_ui || g.midi_thru {
                    t.push_event(e.frame, e.data);
                }
                if matches!(e.data[0] & 0xf0, 0x80 | 0x90) {
                    let (pos, pass) = if self.playing { self.played_at(cycle, pdc, e.frame) } else { (self.pos, self.rec_pass) };
                    let _ = self.live_notes.push(LiveNote {
                        pos,
                        data: e.data,
                        recording: self.recording && self.playing && self.pos >= self.rec_from,
                        playing: self.playing,
                        pass,
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
                None if t.channel.hw.is_some() => continue,
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
                None if bus.hw.is_some() => continue,
                None => &mut master.buf,
            };
            add(dst, &bus.buf, n);
        }
        let key_of = |src: usize| tracks.get(src).map(|t| [&t.channel.buf[0][..n], &t.channel.buf[1][..n]]);
        master.run(n, None, &mut [], &ctx, &key_of);
        out_l[..n].copy_from_slice(&master.buf[0][..n]);
        out_r[..n].copy_from_slice(&master.buf[1][..n]);
        self.feed_tap(&out_l[..n], &out_r[..n]);
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
        // After the bounce has taken the master: what goes to the interface directly is not in the mix.
        for c in g.tracks.iter().filter(|t| !t.reference).map(|t| &t.channel).chain(&g.buses) {
            if let Some((first, stereo)) = c.hw {
                to_hardware(c, first, stereo, n, g.outputs, out_l, out_r, extra);
            }
        }
        self.play_reference(&g, n, out_l, out_r);
        self.click.render(&mut out_l[..n], &mut out_r[..n], self.sample_rate, g.click_gain, g.click_accent);
        if let Some(m) = self.measure.as_mut() {
            // Only the probe goes out, so nothing monitored can be mistaken for its return.
            out_l[..n].fill(0.0);
            out_r[..n].fill(0.0);
            for o in extra.iter_mut() {
                o.fill(0.0);
            }
            let input = inputs.get(m.input).copied().unwrap_or(&[]);
            let mut none: [f32; 0] = [];
            let out = output_port(m.output, out_l, out_r, extra).unwrap_or(&mut none);
            for i in 0..n {
                let k = m.done + i;
                if let Some(o) = out.get_mut(i) {
                    *o = measure::probe(k, self.sample_rate);
                }
                if let Some(slot) = self.shared.measure.get(k) {
                    slot.set(input.get(i).copied().unwrap_or(0.0));
                }
            }
            m.done += n;
            if m.done >= self.shared.measure.len() {
                self.measure = None;
                self.shared.measured.store(true, Release);
            }
        }

        if self.suspend_pending != 0 {
            self.suspended = true;
            self.shared.suspended.store(std::mem::take(&mut self.suspend_pending), Release);
        }
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

    /// Crossfades the main pair between the mix and the reference tracks over about 10 ms,
    /// starting at this period's boundary.
    fn play_reference(&mut self, g: &Graph, n: usize, out_l: &mut [f32], out_r: &mut [f32]) {
        let refs = g.outputs >= 2 && g.tracks.iter().any(|t| t.reference);
        let target = if refs && self.shared.reference.load(Relaxed) { 1.0 } else { 0.0 };
        let from = self.reference_mix;
        if from == 0.0 && target == 0.0 {
            return;
        }
        let step = (target - from).signum() / (0.01 * self.sample_rate as f32);
        let x = |i: usize| (from + step * (i + 1) as f32).clamp(0.0, 1.0);
        for i in 0..n {
            let keep = 1.0 - x(i);
            out_l[i] *= keep;
            out_r[i] *= keep;
        }
        for t in g.tracks.iter().filter(|t| t.reference) {
            let [bl, br] = &t.channel.buf;
            for i in 0..n {
                out_l[i] += bl[i] * x(i);
                out_r[i] += br[i] * x(i);
            }
        }
        self.reference_mix = if n == 0 { from } else { x(n - 1) };
    }

    fn feed_tap(&mut self, l: &[f32], r: &[f32]) {
        while let Ok(t) = self.taps.pop() {
            if let Some(old) = self.master_tap.replace(t) {
                self.give_back(old);
            }
        }
        if !self.shared.master_tap_on.load(Relaxed) {
            if let Some(old) = self.master_tap.take() {
                self.give_back(old);
            }
            return;
        }
        let Some(ring) = self.master_tap.as_mut() else { return };
        match ring.write_chunk_uninit(l.len() * 2) {
            Ok(mut chunk) => {
                let (s1, s2) = chunk.as_mut_slices();
                for (k, slot) in s1.iter_mut().chain(s2.iter_mut()).enumerate() {
                    slot.write(if k % 2 == 0 { l[k / 2] } else { r[k / 2] });
                }
                unsafe { chunk.commit_all() };
            }
            Err(_) => {
                self.shared.master_tap_dropped.fetch_add(l.len() as u64, Relaxed);
            }
        }
    }

    fn give_back(&mut self, tap: Producer<f32>) {
        if let Err(rtrb::PushError::Full(tap)) = self.tap_garbage.push(tap) {
            std::mem::forget(tap);
        }
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
                    if !self.playing {
                        self.rec_pass = 0;
                        self.wrapped = false;
                        if let Some(c) = g.cycle {
                            self.pos = c.start;
                        }
                        reseed(g);
                    }
                    self.playing = true;
                }
                Command::Stop => self.stop(g),
                Command::Suspend(0) => self.suspend_pending = 0,
                Command::Suspend(token) => {
                    self.stop(g);
                    self.suspend_pending = token;
                }
                Command::Locate(pos) => {
                    self.pos = pos;
                    self.wrapped = false;
                    self.stop_takes(g);
                    self.release_all(g, 0);
                    reseed(g);
                }
                Command::Bounce { start, end } if self.bounce_ring.is_some() && end > start => {
                    self.pos = start;
                    self.playing = true;
                    self.recording = false;
                    self.bounce = Some((start, end + g.pdc as u64));
                    self.bounce_skip = g.pdc as u64;
                    self.bounce_written = 0;
                    self.release_all(g, 0);
                    reseed(g);
                    let _ = self.rec_events.push(RecEvent::Start { track: BOUNCE_TRACK, pos: start, channels: 2 });
                }
                Command::Bounce { .. } => {}
                Command::Measure { output, input } => {
                    self.stop(g);
                    self.shared.measured.store(false, Relaxed);
                    self.measure = Some(Measure { output: output as usize, input: input as usize, done: 0 });
                }
                Command::Midi(data) => {
                    if self.ui_midi_len < self.ui_midi.len() {
                        self.ui_midi[self.ui_midi_len] = MidiEvent { frame: 0, data };
                        self.ui_midi_len += 1;
                    }
                }
                Command::RecordCountIn(frames) => {
                    let start = match g.cycle {
                        Some(c) if !self.playing => c.start,
                        _ => self.pos,
                    };
                    self.rec_from = start;
                    self.rec_pass = 0;
                    self.wrapped = false;
                    self.pos = start.saturating_sub(frames);
                    self.recording = true;
                    self.playing = true;
                }
                Command::Record(on) => {
                    self.recording = on;
                    self.rec_from = 0;
                    if on {
                        self.rec_pass = 0;
                        if !self.playing {
                            self.wrapped = false;
                            if let Some(c) = g.cycle {
                                self.pos = c.start;
                            }
                        }
                        self.playing = true;
                    } else {
                        self.stop_takes(g);
                    }
                }
            }
        }
    }

    fn stop(&mut self, g: &mut Graph) {
        self.finish_bounce();
        self.playing = false;
        self.recording = false;
        self.stop_takes(g);
        self.release_all(g, 0);
    }

    /// The graph, for freeing on the thread that should do it.
    pub fn into_graph(mut self) -> Option<Box<Graph>> {
        self.graph.take().or_else(|| self.graphs.pop().ok())
    }

    fn finish_bounce(&mut self) {
        if self.bounce.take().is_some() {
            let _ = self.rec_events.push(RecEvent::Stop { track: BOUNCE_TRACK, frames: self.bounce_written });
        }
    }

    fn stop_takes(&mut self, g: &mut Graph) {
        self.tail = false;
        for t in g.tracks.iter_mut().filter(|t| t.rt.rec_active) {
            t.rt.rec_active = false;
            let _ = self.rec_events.push(RecEvent::Stop { track: t.id, frames: t.rt.rec_written });
        }
    }

    /// Where a note played `frame` frames into this block belongs, and in which cycle pass. Its
    /// position is moved back by the plug-in delay, as that is when the player heard the
    /// arrangement; a note just after a wrap can so belong at the end of the previous pass.
    /// A block holds at most one wrap, since the cycle is never shorter than a block.
    fn played_at(&self, cycle: Option<solstice_model::Range>, pdc: u64, frame: u32) -> (u64, u32) {
        let mut raw = self.pos + frame as u64;
        let mut pass = self.rec_pass;
        let mut wrapped = self.wrapped;
        let cycle = cycle.filter(|_| self.bounce.is_none());
        if let Some(c) = cycle
            && self.pos < c.end
            && raw >= c.end
        {
            raw = c.start + (raw - c.end);
            pass += 1;
            wrapped = true;
        }
        if let Some(c) = cycle
            && wrapped
            && pass > 0
            && raw >= c.start
            && raw - c.start < pdc
        {
            return (c.end.saturating_sub(pdc - (raw - c.start)), pass - 1);
        }
        (raw.saturating_sub(pdc), pass)
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
        if self.recording {
            limit(self.rec_from);
            // Takes follow these boundaries as they were heard, `shift` frames later, which for
            // the end of a cycle pass is early in the next.
            let shift = self.rec_shift(g);
            if shift > 0 {
                let mut heard = |b: u64| {
                    limit(b + shift);
                    if let Some(c) = g.cycle
                        && b + shift >= c.end
                    {
                        limit(c.start + (b + shift - c.end));
                    }
                };
                heard(0);
                heard(self.rec_from);
                if let Some(p) = g.punch {
                    heard(p.start);
                    heard(p.end);
                }
                if let Some(c) = g.cycle {
                    limit(c.start + shift);
                }
            }
        }
        len.max(1) as usize
    }

    fn rec_shift(&self, g: &Graph) -> u64 {
        g.pdc as u64 + self.shared.record_offset.load(Relaxed) as u64
    }

    /// Where input arriving at `pos` belongs: `shift` frames earlier, as what's played along to
    /// was heard late by the plug-in delay and the interface's round trip. Just after a wrap that
    /// is the end of the previous pass (true). None is before zero, or the lead-in to the
    /// cycle's first pass.
    fn rec_at(&self, cycle: Option<solstice_model::Range>, pos: u64, shift: u64) -> Option<(u64, bool)> {
        if let Some(c) = cycle
            && pos >= c.start
            && pos - c.start < shift
            && pos < c.end
        {
            let into = pos - c.start;
            return (self.wrapped && self.rec_pass > 0).then(|| (c.end.saturating_sub(shift - into), true));
        }
        pos.checked_sub(shift).map(|m| (m, false))
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
        let at = self.rec_at(g.cycle, pos, self.rec_shift(g));
        if self.tail && !at.is_some_and(|(_, tail)| tail) {
            self.stop_takes(g);
        }
        let rec_at = at.map(|(m, _)| m).filter(|&m| m >= self.rec_from && g.punch.is_none_or(|p| p.contains(m)));
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
            self.record(t, inputs, rec_at, off, len);
        }
        if self.playing {
            if g.metronome || (self.recording && pos < self.rec_from) {
                // The click goes straight to the output, so it waits out the plug-in delay too.
                let p = pos as i64 - g.pdc as i64;
                let skip = (-p).max(0) as usize;
                if skip < len {
                    self.click.schedule(&g.time, (p + skip as i64) as u64, len - skip, off + skip, g.click_groups_only);
                }
            }
            self.pos += len as u64;
            if let Some(c) = g.cycle
                && self.pos >= c.end
                && pos < c.end
            {
                self.pos = c.start;
                self.rec_pass = self.rec_pass.saturating_add(1);
                self.wrapped = true;
                self.tail = true;
                self.release_all(g, (off + len - 1) as u32);
            }
        }
    }

    /// Records this segment's input for `t`; `at` is where it belongs, when that is to be recorded.
    fn record(&mut self, t: &mut graph::TrackNode, inputs: &[&[f32]], at: Option<u64>, off: usize, len: usize) {
        let Some((a, b)) = t.input else { return };
        let want = self.recording && self.playing && at.is_some() && t.armed && t.rt.rec.is_some();
        if want && !t.rt.rec_active {
            let channels = if b.is_some() { 2 } else { 1 };
            let _ = self.rec_events.push(RecEvent::Start { track: t.id, pos: at.unwrap_or(0), channels });
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

/// Restarts every track's chance rolls, so playing from the same place repeats them.
fn reseed(g: &mut Graph) {
    for t in &mut g.tracks {
        t.rt.rng = graph::rng_seed(t.id);
    }
}

/// Interface output `k`, counting the main pair as 0 and 1.
fn output_port<'a>(k: usize, l: &'a mut [f32], r: &'a mut [f32], extra: &'a mut [&mut [f32]]) -> Option<&'a mut [f32]> {
    match k {
        0 => Some(l),
        1 => Some(r),
        _ => extra.get_mut(k - 2).map(|s| &mut **s),
    }
}

/// Mixes a channel routed to the interface into its outputs. A mono output takes the average of
/// both sides; its pan was held at centre, so that is the channel's own level.
#[allow(clippy::too_many_arguments)]
fn to_hardware(
    c: &graph::ChannelNode,
    first: usize,
    stereo: bool,
    n: usize,
    outputs: usize,
    l: &mut [f32],
    r: &mut [f32],
    extra: &mut [&mut [f32]],
) {
    let [bl, br] = &c.buf;
    if stereo {
        for (k, src) in [(first, bl), (first + 1, br)] {
            if k < outputs
                && let Some(out) = output_port(k, l, r, extra)
            {
                for (o, s) in out.iter_mut().zip(&src[..n]) {
                    *o += s;
                }
            }
        }
    } else if first < outputs
        && let Some(out) = output_port(first, l, r, extra)
    {
        for (i, o) in out.iter_mut().take(n).enumerate() {
            *o += (bl[i] + br[i]) * 0.5;
        }
    }
}

fn add(dst: &mut [Vec<f32>; 2], src: &[Vec<f32>; 2], n: usize) {
    for c in 0..2 {
        for (d, s) in dst[c][..n].iter_mut().zip(&src[c][..n]) {
            *d += s;
        }
    }
}

/// Metronome: a short decaying sine, highest on the downbeat, then group starts, then the
/// remaining beats a little quieter.
struct Click {
    pending: Option<(usize, Beat)>,
    phase: f32,
    remaining: usize,
    beat: Beat,
}

impl Default for Click {
    fn default() -> Self {
        Click { pending: None, phase: 0.0, remaining: 0, beat: Beat::Plain }
    }
}

impl Click {
    fn schedule(&mut self, time: &solstice_model::TimeMap, pos: u64, len: usize, off: usize, groups_only: bool) {
        let end = pos + len as u64;
        let (mut at, mut beat) = time.next_beat(pos);
        while groups_only && beat == Beat::Plain && at < end {
            (at, beat) = time.next_beat(at + 1);
        }
        if at >= pos && at < end {
            self.pending = Some((off + (at - pos) as usize, beat));
        }
    }

    fn render(&mut self, l: &mut [f32], r: &mut [f32], sample_rate: u32, gain: f32, accent: bool) {
        let total = (sample_rate / 30) as usize;
        let pending = self.pending.take();
        for i in 0..l.len() {
            if let Some((at, beat)) = pending
                && at == i
            {
                self.remaining = total;
                self.phase = 0.0;
                self.beat = beat;
            }
            if self.remaining == 0 {
                continue;
            }
            let (freq, level) = match self.beat {
                Beat::Bar if accent => (1760.0, 1.0),
                Beat::Group if accent => (1540.0, 1.0),
                Beat::Plain if accent => (1320.0, 0.7),
                _ => (1320.0, 1.0),
            };
            let env = self.remaining as f32 / total as f32;
            let s = (self.phase * std::f32::consts::TAU).sin() * env * env * gain * level;
            self.phase = (self.phase + freq / sample_rate as f32).fract();
            self.remaining -= 1;
            l[i] += s;
            r[i] += s;
        }
    }
}

#[cfg(test)]
mod tests;
