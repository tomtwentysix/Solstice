use crate::audio::AudioData;
use crate::{MidiEvent, PluginCell, ProcessContext, Strip};
use rtrb::Producer;
use solstice_model::{
    AutoMode, AutoTarget, AutomationLane, Channel, Content, Id, Output, Project, Range, SendMode, TimeMap, TrackKind, db_to_gain,
};
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

pub const MAX_EVENTS: usize = 1024;

/// An immutable snapshot of the project, compiled on the UI thread and handed to the RT thread.
pub struct Graph {
    pub tracks: Vec<TrackNode>,
    pub buses: Vec<ChannelNode>,
    /// What tracks and earlier buses have sent to each bus this period.
    pub bus_in: Vec<[Vec<f32>; 2]>,
    /// Bus indices in processing order: every bus after the buses that feed it.
    pub bus_order: Vec<usize>,
    pub master: ChannelNode,
    pub time: TimeMap,
    pub cycle: Option<Range>,
    pub punch: Option<Range>,
    pub metronome: bool,
    pub auto_input: bool,
    pub low_latency: Option<u32>,
    pub midi_track: Option<usize>,
    /// Live MIDI plays the selected instrument; off, it is only recorded.
    pub midi_thru: bool,
    pub click_gain: f32,
    pub click_accent: bool,
    pub max_block: usize,
    /// A fresh master record ring for bouncing; the RT thread keeps the last one it was given.
    pub bounce_ring: Option<Producer<f32>>,
    /// Plug-in latency at the master output, which the timeline compensates for.
    pub pdc: u32,
    /// Track indices processed first, then those keyed by side-chains from the first.
    pub waves: (Vec<usize>, Vec<usize>),
}

pub struct ChannelNode {
    pub strip: Arc<Strip>,
    pub inserts: Vec<Arc<PluginCell>>,
    /// For each insert, the track keying its side-chain.
    pub keys: Vec<Option<KeyTap>>,
    pub sends: Vec<SendNode>,
    /// Bus index, or `None` for the master.
    pub output: Option<usize>,
    pub audible: bool,
    pub mono: bool,
    pub eq: Option<crate::eq::EqCoeffs>,
    pub auto: Vec<AutoNode>,
    pub auto_mode: AutoMode,
    pub buf: [Vec<f32>; 2],
    /// Compensation on the way to `output`.
    pub out_delay: Option<Delay>,
    /// The signal after the inserts, kept for pre- and post-fader sends.
    pre: Option<[Vec<f32>; 2]>,
    /// The VCA fader this channel follows.
    pub vca: Option<Arc<Strip>>,
    /// This period's fader and pan ramps (start and per-sample step) and mute.
    ramp: ([f32; 3], [f32; 3]),
    muted: bool,
    /// Live input in low latency mode this period: compensation delays are skipped too.
    live: bool,
}

pub struct SendNode {
    pub bus: usize,
    pub gain: f32,
    pub mode: SendMode,
    /// Position in the channel's send list in the project, which automation lanes refer to.
    pub index: usize,
    pub delay: Option<Delay>,
}

/// A side-chain feed into one insert, delayed to line up with the signal at that insert. A key
/// that arrives later than the signal it keys stays late: the signal is not held back for it.
pub struct KeyTap {
    /// Track index.
    pub src: usize,
    /// The source track's id, which survives tracks being added or reordered.
    src_id: Id,
    pub(crate) delay: Option<Delay>,
    buf: Option<[Vec<f32>; 2]>,
}

impl KeyTap {
    fn new(src: usize, src_id: Id) -> Self {
        KeyTap { src, src_id, delay: None, buf: None }
    }

    /// The key for this period. The line keeps running in live mode, when compensation is
    /// skipped, and while the insert is bypassed, so neither replays stale audio afterwards.
    fn key<'a>(&'a mut self, src: [&'a [f32]; 2], live: bool) -> [&'a [f32]; 2] {
        let (Some(d), Some([bl, br])) = (self.delay.as_mut(), self.buf.as_mut()) else { return src };
        let n = src[0].len().min(src[1].len()).min(bl.len());
        bl[..n].copy_from_slice(&src[0][..n]);
        br[..n].copy_from_slice(&src[1][..n]);
        d.run(&mut bl[..n], &mut br[..n]);
        if live { src } else { [&bl[..n], &br[..n]] }
    }
}

/// Plug-in delay compensation on one connection: holds a signal back so it reaches its
/// destination in step with that destination's slowest input.
pub struct Delay {
    buf: [Vec<f32>; 2],
    pos: usize,
}

impl Delay {
    pub fn new(frames: u32) -> Option<Delay> {
        (frames > 0).then(|| Delay { buf: [vec![0.0; frames as usize], vec![0.0; frames as usize]], pos: 0 })
    }

    pub fn frames(&self) -> usize {
        self.buf[0].len()
    }

    #[inline]
    pub fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let p = self.pos;
        let out = (self.buf[0][p], self.buf[1][p]);
        self.buf[0][p] = l;
        self.buf[1][p] = r;
        self.pos = if p + 1 == self.buf[0].len() { 0 } else { p + 1 };
        out
    }

    pub fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            (*a, *b) = self.tick(*a, *b);
        }
    }

    /// Carries over what an older graph's line still holds, so a rebuild doesn't drop audio.
    fn inherit(&mut self, old: &Delay) {
        if old.frames() == self.frames() {
            self.buf[0].copy_from_slice(&old.buf[0]);
            self.buf[1].copy_from_slice(&old.buf[1]);
            self.pos = old.pos;
        }
    }
}

pub enum AutoKind {
    Volume,
    Pan,
    Mute,
    Send(usize),
    Param(Arc<PluginCell>, u32),
}

pub struct AutoNode {
    pub kind: AutoKind,
    pub lane: AutomationLane,
    /// Last parameter value sent, so plug-ins only hear changes.
    last: f32,
}

pub struct AudioRegionNode {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub gain: f32,
    pub fade_in: u64,
    pub fade_out: u64,
    pub data: Arc<AudioData>,
}

/// Per-track state that belongs to the RT thread and is carried over when graphs are swapped.
#[derive(Default)]
pub struct TrackRt {
    pub rec: Option<Producer<f32>>,
    pub rec_active: bool,
    pub rec_written: u64,
    pub held: [u64; 2],
}

pub struct TrackNode {
    pub id: Id,
    pub kind: TrackKind,
    pub channel: ChannelNode,
    pub regions: Vec<AudioRegionNode>,
    /// Absolute sample position and MIDI bytes, sorted by position with note-offs first.
    pub events: Vec<(u64, [u8; 3])>,
    pub instrument: Option<Arc<PluginCell>>,
    pub midi_fx: Vec<Arc<PluginCell>>,
    pub input: Option<(usize, Option<usize>)>,
    pub armed: bool,
    pub monitor: bool,
    pub rt: TrackRt,
    pub ev_buf: Vec<MidiEvent>,
    /// The other half of `ev_buf` while MIDI FX rewrite it.
    pub fx_buf: Vec<MidiEvent>,
}

pub trait Resources {
    fn audio(&self, file: &str) -> Option<Arc<AudioData>>;
    fn plugin(&self, slot: Id) -> Option<Arc<PluginCell>>;
    fn strip(&self, channel: Option<Id>) -> Arc<Strip>;
    /// A new record ring for a track that has just been armed; `None` keeps the RT thread's ring.
    fn record_ring(&self, track: Id) -> Option<Producer<f32>>;
    fn bounce_ring(&self) -> Option<Producer<f32>> {
        None
    }
    /// Whether a plug-in class outputs stereo, which turns a mono strip stereo after it.
    fn class_is_stereo(&self, _uid: &str) -> bool {
        false
    }
}

pub struct Options {
    pub max_block: usize,
    pub inputs: usize,
    pub auto_input: bool,
    pub low_latency: Option<u32>,
    pub midi_track: Option<Id>,
    pub midi_thru: bool,
    pub click_gain: f32,
    pub click_accent: bool,
    /// Plays one track on its own for an export or bounce in place: everything else is
    /// silent and the stereo output's plug-ins are left out.
    pub isolate: Option<Id>,
}

impl ChannelNode {
    fn new(strip: Arc<Strip>, max_block: usize) -> Self {
        ChannelNode {
            strip,
            inserts: Vec::new(),
            keys: Vec::new(),
            sends: Vec::new(),
            output: None,
            audible: true,
            mono: false,
            eq: None,
            auto: Vec::new(),
            auto_mode: AutoMode::Read,
            buf: [vec![0.0; max_block], vec![0.0; max_block]],
            out_delay: None,
            pre: None,
            vca: None,
            ramp: ([0.0; 3], [0.0; 3]),
            muted: false,
            live: false,
        }
    }

    /// Latency of the plug-ins that will run on this strip.
    fn latency(&self) -> u32 {
        self.latency_before(self.inserts.len())
    }

    /// Latency of the plug-ins ahead of insert `j`.
    fn latency_before(&self, j: usize) -> u32 {
        self.inserts[..j].iter().filter(|p| !p.is_bypassed()).map(|p| p.latency()).sum()
    }

    pub(crate) fn inherit(&mut self, old: &ChannelNode) {
        if let (Some(d), Some(o)) = (self.out_delay.as_mut(), old.out_delay.as_ref()) {
            d.inherit(o);
        }
        for s in &mut self.sends {
            let prev = old.sends.iter().find(|o| o.index == s.index && o.bus == s.bus);
            if let (Some(d), Some(o)) = (s.delay.as_mut(), prev.and_then(|o| o.delay.as_ref())) {
                d.inherit(o);
            }
        }
        for (cell, tap) in self.inserts.iter().zip(&mut self.keys) {
            let Some(tap) = tap else { continue };
            let prev = old.inserts.iter().zip(&old.keys).find_map(|(c, k)| {
                k.as_ref().filter(|k| k.src_id == tap.src_id && Arc::ptr_eq(c, cell))
            });
            if let (Some(d), Some(o)) = (tap.delay.as_mut(), prev.and_then(|o| o.delay.as_ref())) {
                d.inherit(o);
            }
        }
    }

    pub(crate) fn clear(&mut self, n: usize) {
        self.buf[0][..n].fill(0.0);
        self.buf[1][..n].fill(0.0);
    }

    /// Runs inserts, sends, fader and pan in place on `buf`; sends mix into `bus_in`.
    pub(crate) fn run<'k>(
        &mut self,
        n: usize,
        skip_latency_over: Option<u32>,
        bus_in: &mut [[Vec<f32>; 2]],
        ctx: &ProcessContext,
        key_of: &dyn Fn(usize) -> Option<[&'k [f32]; 2]>,
    ) {
        self.process(n, skip_latency_over, ctx, key_of);
        self.route(n, bus_in);
    }

    /// Everything that touches only this strip: inserts, fader and pan, in place on `buf`.
    /// Safe to run on any thread alongside other strips.
    pub(crate) fn process<'k>(
        &mut self,
        n: usize,
        skip_latency_over: Option<u32>,
        ctx: &ProcessContext,
        key_of: &dyn Fn(usize) -> Option<[&'k [f32]; 2]>,
    ) {
        let (vol, pan_auto, mute_auto) = self.automate(ctx);
        let live = skip_latency_over.is_some();
        self.live = live;
        let [l, r] = &mut self.buf;
        let (l, r) = (&mut l[..n], &mut r[..n]);
        if self.mono {
            r.copy_from_slice(l);
        }
        if let Some(eq) = &self.eq {
            self.strip.apply_eq(eq, l, r);
        }
        for (p, tap) in self.inserts.iter().zip(self.keys.iter_mut()) {
            let key = tap.as_mut().and_then(|t| key_of(t.src).map(|k| t.key(k, live)));
            if p.is_bypassed() || skip_latency_over.is_some_and(|t| p.latency() > t) {
                continue;
            }
            // Always called, so a plug-in whose key was just removed stops using the old one.
            p.sidechain(key);
            p.process(l, r, &[], ctx);
        }
        let vca_muted = self.vca.as_ref().is_some_and(|v| v.mute.load(Relaxed));
        self.muted = !self.audible || vca_muted || mute_auto.unwrap_or_else(|| self.strip.mute.load(Relaxed));
        if let Some([pl, pr]) = self.pre.as_mut() {
            pl[..n].copy_from_slice(l);
            pr[..n].copy_from_slice(r);
        }
        let pan = pan_gains(1.0, pan_auto.unwrap_or_else(|| self.strip.pan()), self.mono);
        let gain = vol.map_or_else(|| self.strip.gain(), db_to_gain) * self.vca.as_ref().map_or(1.0, |v| v.gain());
        let target = [if self.muted { 0.0 } else { gain }, pan[0], pan[1]];
        let from = self.strip.ramp_from(target);
        let step: [f32; 3] = std::array::from_fn(|k| (target[k] - from[k]) / n as f32);
        self.ramp = (from, step);
        for i in 0..n {
            let g = from[0] + step[0] * i as f32;
            let (pl, pr) = (from[1] + step[1] * i as f32, from[2] + step[2] * i as f32);
            l[i] *= g * pl;
            r[i] *= g * pr;
        }
        self.strip.meter(l, r);
    }

    /// Mixes the sends into their buses and delays the output for compensation. Sends tap
    /// the signal after the inserts (pre fader), after the fader, or after pan.
    pub(crate) fn route(&mut self, n: usize, bus_in: &mut [[Vec<f32>; 2]]) {
        let (from, step) = self.ramp;
        let muted = self.muted;
        let [l, r] = &self.buf;
        for s in &mut self.sends {
            let gain = if muted { 0.0 } else { s.gain };
            if s.delay.is_none() && muted {
                continue;
            }
            let [dl, dr] = &mut bus_in[s.bus];
            for i in 0..n {
                let (a, b) = match (s.mode, self.pre.as_ref()) {
                    (SendMode::PostPan, _) | (_, None) => (l[i], r[i]),
                    (SendMode::PreFader, Some([pl, pr])) => (pl[i], pr[i]),
                    (SendMode::PostFader, Some([pl, pr])) => {
                        let g = from[0] + step[0] * i as f32;
                        (pl[i] * g, pr[i] * g)
                    }
                };
                // The line keeps running while muted so unmuting doesn't replay stale audio.
                let (a, b) = match s.delay.as_mut() {
                    Some(d) if !self.live => d.tick(a, b),
                    _ => (a, b),
                };
                dl[i] += a * gain;
                dr[i] += b * gain;
            }
        }
        if let Some(d) = self.out_delay.as_mut().filter(|_| !self.live) {
            let [l, r] = &mut self.buf;
            d.run(&mut l[..n], &mut r[..n]);
        }
    }

    /// Applies automation for this period. Returns volume (dB), pan and mute when a lane sets
    /// them; sends and plug-in parameters are set directly.
    fn automate(&mut self, ctx: &ProcessContext) -> (Option<f32>, Option<f32>, Option<bool>) {
        let strip = &self.strip;
        let touched = strip.touched.load(Relaxed);
        if !ctx.playing {
            strip.latched.store(false, Relaxed);
        } else if touched && self.auto_mode == AutoMode::Latch {
            strip.latched.store(true, Relaxed);
        }
        let follow = match self.auto_mode {
            AutoMode::Off | AutoMode::Write => false,
            AutoMode::Read => true,
            AutoMode::Touch => !touched,
            AutoMode::Latch => !touched && !strip.latched.load(Relaxed),
        };
        let (mut vol, mut pan, mut mute) = (None, None, None);
        if follow {
            for a in &mut self.auto {
                let Some(v) = a.lane.value_at(ctx.position) else { continue };
                match &a.kind {
                    AutoKind::Volume => vol = Some(v),
                    AutoKind::Pan => pan = Some(v),
                    AutoKind::Mute => mute = Some(v > 0.5),
                    AutoKind::Send(i) => {
                        if let Some(s) = self.sends.iter_mut().find(|s| s.index == *i) {
                            s.gain = db_to_gain(v);
                        }
                    }
                    AutoKind::Param(cell, id) => {
                        if a.last.is_nan() || (v - a.last).abs() > 1e-6 {
                            cell.set_param(*id, v as f64);
                            a.last = v;
                        }
                    }
                }
            }
        }
        strip.auto_active.store(vol.is_some() || pan.is_some(), Relaxed);
        if let Some(v) = vol {
            strip.auto_db.set(v);
        }
        if let Some(p) = pan {
            strip.auto_pan.set(p);
        }
        (vol, pan, mute)
    }
}

/// Mono sources use a -3 dB compensated constant-power law (unity at centre); stereo sources
/// use a balance control, as Logic does.
pub fn pan_gains(gain: f32, pan: f32, mono: bool) -> [f32; 2] {
    let pan = pan.clamp(-1.0, 1.0);
    if mono {
        let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let k = gain * std::f32::consts::SQRT_2;
        [theta.cos() * k, theta.sin() * k]
    } else {
        [gain * (1.0 - pan.max(0.0)), gain * (1.0 + pan.min(0.0))]
    }
}

pub fn compile(p: &Project, res: &dyn Resources, opts: &Options) -> Graph {
    let time = p.time();
    let mb = opts.max_block;
    let bus_index = |id: Id| p.buses.iter().position(|b| b.id == id);
    let isolate = opts.isolate;
    let any_solo = isolate.is_some() || p.any_solo();
    // A loop can only come from a hand-edited file; the UI refuses one. Drop bus-to-bus routing.
    let (bus_order, bus_routing) = match p.bus_order() {
        Some(o) => (o, true),
        None => ((0..p.buses.len()).collect(), false),
    };
    let audible = |c: &Channel| !any_solo || (isolate.is_none() && (c.solo || c.solo_safe));
    let fs = p.sample_rate as f64;
    let loaded = |slot: Id| res.plugin(slot).is_some();
    let keyed = keyed_tracks(p, &loaded);
    let key_srcs = side_chain_keys(p, &loaded);
    let inserts = |c: &Channel| -> (Vec<Arc<PluginCell>>, Vec<Option<KeyTap>>) {
        c.inserts
            .iter()
            .filter_map(|s| {
                let tap = key_srcs.get(&s.id).map(|&k| KeyTap::new(k, p.tracks[k].id));
                Some((res.plugin(s.id)?, tap))
            })
            .unzip()
    };
    let route = |c: &Channel, node: &mut ChannelNode, allow_bus: bool| {
        node.vca = c.vca.filter(|id| p.buses.iter().any(|b| b.id == *id && b.vca)).map(|id| res.strip(Some(id)));
        node.eq = c.eq.enabled.then(|| crate::eq::design(&c.eq, fs));
        node.auto_mode = c.auto_mode;
        node.auto = c
            .automation
            .iter()
            .filter(|l| !l.points.is_empty())
            .filter_map(|l| {
                let kind = match l.target {
                    AutoTarget::Volume => AutoKind::Volume,
                    AutoTarget::Pan => AutoKind::Pan,
                    AutoTarget::Mute => AutoKind::Mute,
                    AutoTarget::Send(i) => AutoKind::Send(i),
                    AutoTarget::Plugin { slot, param } => AutoKind::Param(res.plugin(slot)?, param),
                };
                Some(AutoNode { kind, lane: l.clone(), last: f32::NAN })
            })
            .collect();
        node.output = match c.output {
            Output::Bus(id) if allow_bus => bus_index(id),
            _ => None,
        };
        node.sends = c
            .sends
            .iter()
            .enumerate()
            .filter(|_| allow_bus)
            .filter_map(|(index, s)| {
                Some(SendNode { bus: bus_index(s.bus)?, gain: db_to_gain(s.level_db), mode: s.mode, index, delay: None })
            })
            .collect();
        if node.sends.iter().any(|s| s.mode != SendMode::PostPan) {
            node.pre = Some([vec![0.0; mb], vec![0.0; mb]]);
        }
    };

    let mut buses: Vec<ChannelNode> = p
        .buses
        .iter()
        .map(|b| {
            let mut c = ChannelNode::new(res.strip(Some(b.id)), mb);
            (c.inserts, c.keys) = inserts(&b.channel);
            c.audible = audible(&b.channel);
            route(&b.channel, &mut c, bus_routing);
            c
        })
        .collect();

    let mut tracks = Vec::with_capacity(p.tracks.len());
    for t in &p.tracks {
        let mut c = ChannelNode::new(res.strip(Some(t.id)), mb);
        (c.inserts, c.keys) = inserts(&t.channel);
        c.mono = t.kind == TrackKind::Audio && !t.stereo && !t.channel.inserts.iter().any(|s| res.class_is_stereo(&s.uid));
        let soloed = isolate.map_or(t.channel.solo, |id| id == t.id);
        c.audible = if isolate.is_some() { soloed } else { audible(&t.channel) };
        route(&t.channel, &mut c, true);
        if soloed {
            for i in c.sends.iter().map(|s| s.bus).chain(c.output) {
                buses[i].audible = true;
            }
        }

        let mut regions = Vec::new();
        let mut events = Vec::new();
        // A looped region plays as back-to-back copies of itself.
        for r in t.regions.iter().filter(|r| !r.muted) {
            for k in 0..=r.loops as u64 {
                let shift = k * r.length;
                let (start, end) = (r.start + shift, r.end() + shift);
                match &r.content {
                    Content::Audio(a) => {
                        // A comp's sections meet with short crossfades so the joins don't click.
                        let xfade = (p.sample_rate / 200) as u64;
                        for piece in a.pieces(r.length) {
                            let Some(data) = res.audio(piece.file) else { continue };
                            let first = piece.start == 0;
                            let last = piece.end >= r.length;
                            let lead = if first { 0 } else { (xfade / 2).min(piece.start).min(piece.offset) };
                            let tail = if last { 0 } else { xfade / 2 };
                            regions.push(AudioRegionNode {
                                start: start + piece.start - lead,
                                end: (start + piece.end + tail).min(end),
                                offset: piece.offset - lead,
                                gain: db_to_gain(a.gain_db),
                                fade_in: if first { a.fade_in } else { xfade },
                                fade_out: if last { a.fade_out } else { xfade },
                                data,
                            });
                        }
                    }
                    Content::Midi(m) if t.kind == TrackKind::Instrument => {
                        for n in &m.notes {
                            let on = time.at(start, n.start as u64);
                            let off = time.at(start, (n.start + n.length) as u64).min(end);
                            if on >= end {
                                continue;
                            }
                            events.push((on, [0x90, n.pitch & 0x7f, n.velocity.clamp(1, 127)]));
                            events.push((off, [0x80, n.pitch & 0x7f, 0]));
                        }
                        for c in &m.controllers {
                            let at = time.at(start, c.tick as u64);
                            if at >= end {
                                continue;
                            }
                            let data = if c.number == solstice_model::PITCH_BEND {
                                let v = c.value.min(16383);
                                [0xE0, (v & 0x7f) as u8, (v >> 7) as u8]
                            } else {
                                [0xB0, c.number & 0x7f, c.value.min(127) as u8]
                            };
                            events.push((at, data));
                        }
                    }
                    Content::Midi(_) => {}
                }
            }
        }
        events.sort_by_key(|&(pos, d)| (pos, d[0] == 0x90));

        let input = match t.kind {
            TrackKind::Audio => {
                let i = t.input as usize;
                if t.stereo && i + 1 < opts.inputs {
                    Some((i, Some(i + 1)))
                } else if i < opts.inputs {
                    Some((i, None))
                } else {
                    None
                }
            }
            TrackKind::Instrument => None,
        };

        tracks.push(TrackNode {
            id: t.id,
            kind: t.kind,
            channel: c,
            regions,
            events,
            instrument: t.instrument.as_ref().and_then(|s| res.plugin(s.id)),
            midi_fx: t.midi_fx.iter().filter_map(|s| res.plugin(s.id)).collect(),
            input,
            armed: t.armed,
            monitor: t.monitor,
            rt: TrackRt {
                rec: if t.armed && t.kind == TrackKind::Audio { res.record_ring(t.id) } else { None },
                ..Default::default()
            },
            ev_buf: Vec::with_capacity(MAX_EVENTS),
            fx_buf: if t.midi_fx.is_empty() { Vec::new() } else { Vec::with_capacity(MAX_EVENTS) },
        });
    }

    // Buses downstream of a soloed track stay audible, however deep the chain.
    for &i in &bus_order {
        if buses[i].audible && any_solo {
            let next: Vec<usize> = buses[i].sends.iter().map(|s| s.bus).chain(buses[i].output).collect();
            for j in next {
                buses[j].audible = true;
            }
        }
    }

    let mut master = ChannelNode::new(res.strip(None), mb);
    if isolate.is_none() {
        (master.inserts, master.keys) = inserts(&p.master);
    }
    master.eq = p.master.eq.enabled.then(|| crate::eq::design(&p.master.eq, fs));
    route(&p.master, &mut master, false);

    let pdc = compensate(&mut tracks, &mut buses, &bus_order, &mut master, mb);
    let waves = (0..tracks.len()).partition(|i| !keyed[*i]);

    let midi_track = opts
        .midi_track
        .and_then(|id| p.tracks.iter().position(|t| t.id == id && t.kind == TrackKind::Instrument))
        .or_else(|| p.tracks.iter().position(|t| t.armed && t.kind == TrackKind::Instrument));

    Graph {
        tracks,
        bus_in: (0..buses.len()).map(|_| [vec![0.0; mb], vec![0.0; mb]]).collect(),
        bus_order,
        buses,
        master,
        time,
        cycle: (p.cycle_on && !p.cycle.is_empty()).then_some(p.cycle),
        punch: p.punch_on.then_some(p.punch),
        metronome: p.metronome,
        auto_input: opts.auto_input,
        low_latency: opts.low_latency,
        midi_track,
        midi_thru: opts.midi_thru,
        click_gain: opts.click_gain,
        click_accent: opts.click_accent,
        max_block: mb,
        bounce_ring: res.bounce_ring(),
        pdc,
        waves,
    }
}

/// For each track, whether one of its loaded inserts is keyed by another track. Keyed tracks
/// are processed once their keys are ready, so they can't key another track's inserts in turn.
fn keyed_tracks(p: &Project, loaded: &dyn Fn(Id) -> bool) -> Vec<bool> {
    p.tracks
        .iter()
        .map(|t| {
            let mut keys = t.channel.inserts.iter().filter(|s| loaded(s.id)).filter_map(|s| s.sidechain);
            keys.any(|k| k != t.id && p.track(k).is_some())
        })
        .collect()
}

/// The track index keying each insert slot whose side-chain the engine feeds, by slot id.
/// Buses and the master can be keyed by any track; a track by any other track that isn't
/// keyed itself. Slots whose plug-in didn't load key nothing, so their tracks stay free to key
/// others; `loaded` must agree with what `Resources::plugin` returns.
pub fn side_chain_keys(p: &Project, loaded: &dyn Fn(Id) -> bool) -> HashMap<Id, usize> {
    let keyed = keyed_tracks(p, loaded);
    let index = |id: Id| p.tracks.iter().position(|t| t.id == id);
    let mut keys = HashMap::new();
    for (i, t) in p.tracks.iter().enumerate() {
        for s in &t.channel.inserts {
            if let Some(k) = s.sidechain.and_then(index).filter(|&k| k != i && !keyed[k]) {
                keys.insert(s.id, k);
            }
        }
    }
    for c in p.buses.iter().map(|b| &b.channel).chain([&p.master]) {
        for s in &c.inserts {
            if let Some(k) = s.sidechain.and_then(index) {
                keys.insert(s.id, k);
            }
        }
    }
    keys
}

/// Plug-in delay compensation: works out when each node's output is ready, then delays every
/// connection into a bus or the master so all of its inputs line up, and every side-chain key
/// so it lines up with the signal at its insert. Returns the total latency at the master output.
fn compensate(
    tracks: &mut [TrackNode],
    buses: &mut [ChannelNode],
    bus_order: &[usize],
    master: &mut ChannelNode,
    max_block: usize,
) -> u32 {
    let mut bus_in = vec![0u32; buses.len()];
    let mut master_in = 0u32;
    let mut arrive = |dst: Option<usize>, lat: u32, bus_in: &mut [u32]| match dst {
        Some(b) => bus_in[b] = bus_in[b].max(lat),
        None => master_in = master_in.max(lat),
    };
    let inst_lat: Vec<u32> =
        tracks.iter().map(|t| t.instrument.as_ref().filter(|i| !i.is_bypassed()).map_or(0, |i| i.latency())).collect();
    let track_out: Vec<u32> = tracks.iter().zip(&inst_lat).map(|(t, &i)| t.channel.latency() + i).collect();
    for (t, &lat) in tracks.iter().zip(&track_out) {
        arrive(t.channel.output, lat, &mut bus_in);
        for s in &t.channel.sends {
            arrive(Some(s.bus), lat, &mut bus_in);
        }
    }
    let mut bus_out = vec![0u32; buses.len()];
    for &i in bus_order {
        bus_out[i] = bus_in[i] + buses[i].latency();
        let b = &buses[i];
        arrive(b.output, bus_out[i], &mut bus_in);
        for s in &b.sends {
            arrive(Some(s.bus), bus_out[i], &mut bus_in);
        }
    }
    let need = |dst: Option<usize>| dst.map_or(master_in, |b| bus_in[b]);
    let wire = |c: &mut ChannelNode, lat: u32| {
        c.out_delay = Delay::new(need(c.output) - lat);
        for s in &mut c.sends {
            s.delay = Delay::new(bus_in[s.bus] - lat);
        }
    };
    for (t, &lat) in tracks.iter_mut().zip(&track_out) {
        wire(&mut t.channel, lat);
    }
    for (i, b) in buses.iter_mut().enumerate() {
        wire(b, bus_out[i]);
    }

    // Keyed tracks read their keys straight after the source's inserts and fader, before its
    // output delay; buses and the master read them after it.
    let tap = |c: &mut ChannelNode, at: u32, have: &dyn Fn(usize) -> u32| {
        for j in 0..c.keys.len() {
            let want = at + c.latency_before(j);
            if let Some(t) = c.keys[j].as_mut() {
                t.delay = Delay::new(want.saturating_sub(have(t.src)));
                t.buf = t.delay.is_some().then(|| [vec![0.0; max_block], vec![0.0; max_block]]);
            }
        }
    };
    let outputs: Vec<Option<usize>> = tracks.iter().map(|t| t.channel.output).collect();
    let routed = |s: usize| need(outputs[s]);
    for (t, &lat) in tracks.iter_mut().zip(&inst_lat) {
        tap(&mut t.channel, lat, &|s| track_out[s]);
    }
    for (i, b) in buses.iter_mut().enumerate() {
        tap(b, bus_in[i], &routed);
    }
    tap(master, master_in, &routed);
    master_in + master.latency()
}

impl TrackNode {
    /// Mixes regions overlapping `[pos, pos + len)` into the track buffer at `off`.
    pub(crate) fn read_regions(&mut self, pos: u64, len: usize, off: usize) {
        let end = pos + len as u64;
        let mono = self.channel.mono;
        let [bl, br] = &mut self.channel.buf;
        for r in &self.regions {
            if r.end <= pos || r.start >= end {
                continue;
            }
            let a = r.start.max(pos);
            let b = r.end.min(end);
            let d = &r.data;
            for t in a..b {
                let i = t - r.start;
                let frame = r.offset + i;
                if frame >= d.frames {
                    break;
                }
                let mut g = r.gain;
                if i < r.fade_in {
                    g *= i as f32 / r.fade_in as f32;
                }
                let left = r.end - t;
                if left <= r.fade_out {
                    g *= left as f32 / r.fade_out as f32;
                }
                let (sl, sr) = if d.channels == 1 {
                    let s = d.sample(0, frame);
                    (s, s)
                } else {
                    (d.sample(0, frame), d.sample(1, frame))
                };
                let o = off + (t - pos) as usize;
                if mono {
                    bl[o] += (sl + sr) * 0.5 * g;
                } else {
                    bl[o] += sl * g;
                    br[o] += sr * g;
                }
            }
        }
    }

    pub(crate) fn add_input(&mut self, inputs: &[&[f32]], from: usize, len: usize, off: usize) {
        let Some((a, b)) = self.input else { return };
        let [bl, br] = &mut self.channel.buf;
        for i in 0..len {
            bl[off + i] += inputs[a][from + i];
        }
        if let Some(b) = b {
            for i in 0..len {
                br[off + i] += inputs[b][from + i];
            }
        }
    }

    pub(crate) fn push_event(&mut self, frame: u32, data: [u8; 3]) {
        let pitch = (data[1] & 0x7f) as usize;
        let bit = 1u64 << (pitch % 64);
        match data[0] & 0xf0 {
            0x90 if data[2] > 0 => self.rt.held[pitch / 64] |= bit,
            0x80 | 0x90 => self.rt.held[pitch / 64] &= !bit,
            _ => {}
        }
        if self.ev_buf.len() < self.ev_buf.capacity() {
            self.ev_buf.push(MidiEvent { frame, data });
        }
    }

    pub(crate) fn release_all(&mut self, frame: u32) {
        for word in 0..2 {
            while self.rt.held[word] != 0 {
                let bit = self.rt.held[word].trailing_zeros() as u8;
                self.push_event(frame, [0x80, word as u8 * 64 + bit, 0]);
            }
        }
    }

    /// Queues the track's sequenced events in `[pos, pos + len)` at buffer offset `off`.
    pub(crate) fn read_events(&mut self, pos: u64, len: usize, off: usize) {
        let end = pos + len as u64;
        let from = self.events.partition_point(|e| e.0 < pos);
        for i in from..self.events.len() {
            let (p, d) = self.events[i];
            if p >= end {
                break;
            }
            self.push_event((off as u64 + p - pos) as u32, d);
        }
    }
}
