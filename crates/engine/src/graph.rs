use crate::audio::AudioData;
use crate::{MidiEvent, PluginCell, ProcessContext, Strip};
use rtrb::Producer;
use solstice_model::{Channel, Content, Id, Output, Project, Range, SendMode, TimeMap, TrackKind, db_to_gain};
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
}

pub struct ChannelNode {
    pub strip: Arc<Strip>,
    pub inserts: Vec<Arc<PluginCell>>,
    pub sends: Vec<SendNode>,
    /// Bus index, or `None` for the master.
    pub output: Option<usize>,
    pub audible: bool,
    pub mono: bool,
    pub eq: Option<crate::eq::EqCoeffs>,
    pub buf: [Vec<f32>; 2],
}

pub struct SendNode {
    pub bus: usize,
    pub gain: f32,
    pub mode: SendMode,
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
    pub input: Option<(usize, Option<usize>)>,
    pub armed: bool,
    pub monitor: bool,
    pub rt: TrackRt,
    pub ev_buf: Vec<MidiEvent>,
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
}

impl ChannelNode {
    fn new(strip: Arc<Strip>, max_block: usize) -> Self {
        ChannelNode {
            strip,
            inserts: Vec::new(),
            sends: Vec::new(),
            output: None,
            audible: true,
            mono: false,
            eq: None,
            buf: [vec![0.0; max_block], vec![0.0; max_block]],
        }
    }

    pub(crate) fn clear(&mut self, n: usize) {
        self.buf[0][..n].fill(0.0);
        self.buf[1][..n].fill(0.0);
    }

    /// Runs inserts, sends, fader and pan in place on `buf`; sends mix into `bus_in`.
    pub(crate) fn run(&mut self, n: usize, skip_latency_over: Option<u32>, bus_in: &mut [[Vec<f32>; 2]], ctx: &ProcessContext) {
        let [l, r] = &mut self.buf;
        let (l, r) = (&mut l[..n], &mut r[..n]);
        if self.mono {
            r.copy_from_slice(l);
        }
        if let Some(eq) = &self.eq {
            self.strip.apply_eq(eq, l, r);
        }
        for p in &self.inserts {
            if p.is_bypassed() || skip_latency_over.is_some_and(|t| p.latency() > t) {
                continue;
            }
            p.process(l, r, &[], ctx);
        }
        let muted = !self.audible || self.strip.mute.load(std::sync::atomic::Ordering::Relaxed);
        let send = |mode: SendMode, l: &[f32], r: &[f32], bus_in: &mut [[Vec<f32>; 2]]| {
            for s in self.sends.iter().filter(|s| s.mode == mode && !muted) {
                mix_into(&mut bus_in[s.bus], l, r, s.gain);
            }
        };
        send(SendMode::PreFader, l, r, bus_in);
        let pan = pan_gains(1.0, self.strip.pan(), self.mono);
        let target = [if muted { 0.0 } else { self.strip.gain() }, pan[0], pan[1]];
        let from = self.strip.ramp_from(target);
        let step: [f32; 3] = std::array::from_fn(|k| (target[k] - from[k]) / n as f32);
        for i in 0..n {
            let g = from[0] + step[0] * i as f32;
            l[i] *= g;
            r[i] *= g;
        }
        send(SendMode::PostFader, l, r, bus_in);
        for i in 0..n {
            l[i] *= from[1] + step[1] * i as f32;
            r[i] *= from[2] + step[2] * i as f32;
        }
        send(SendMode::PostPan, l, r, bus_in);
        self.strip.meter(l, r);
    }
}

fn mix_into(dst: &mut [Vec<f32>; 2], l: &[f32], r: &[f32], gain: f32) {
    for (d, s) in dst[0].iter_mut().zip(l) {
        *d += s * gain;
    }
    for (d, s) in dst[1].iter_mut().zip(r) {
        *d += s * gain;
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
    let any_solo = p.any_solo();
    // A loop can only come from a hand-edited file; the UI refuses one. Drop bus-to-bus routing.
    let (bus_order, bus_routing) = match p.bus_order() {
        Some(o) => (o, true),
        None => ((0..p.buses.len()).collect(), false),
    };
    let audible = |c: &Channel| !any_solo || c.solo || c.solo_safe;
    let fs = p.sample_rate as f64;
    let route = |c: &Channel, node: &mut ChannelNode, allow_bus: bool| {
        node.eq = c.eq.enabled.then(|| crate::eq::design(&c.eq, fs));
        node.output = match c.output {
            Output::Bus(id) if allow_bus => bus_index(id),
            _ => None,
        };
        node.sends = c
            .sends
            .iter()
            .filter(|_| allow_bus)
            .filter_map(|s| Some(SendNode { bus: bus_index(s.bus)?, gain: db_to_gain(s.level_db), mode: s.mode }))
            .collect();
    };

    let mut buses: Vec<ChannelNode> = p
        .buses
        .iter()
        .map(|b| {
            let mut c = ChannelNode::new(res.strip(Some(b.id)), mb);
            c.inserts = b.channel.inserts.iter().filter_map(|s| res.plugin(s.id)).collect();
            c.audible = audible(&b.channel);
            route(&b.channel, &mut c, bus_routing);
            c
        })
        .collect();

    let mut tracks = Vec::with_capacity(p.tracks.len());
    for t in &p.tracks {
        let mut c = ChannelNode::new(res.strip(Some(t.id)), mb);
        c.inserts = t.channel.inserts.iter().filter_map(|s| res.plugin(s.id)).collect();
        c.mono = t.kind == TrackKind::Audio && !t.stereo && !t.channel.inserts.iter().any(|s| res.class_is_stereo(&s.uid));
        c.audible = audible(&t.channel);
        route(&t.channel, &mut c, true);
        if t.channel.solo {
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
                        if let Some(data) = res.audio(&a.file) {
                            regions.push(AudioRegionNode {
                                start,
                                end,
                                offset: a.offset,
                                gain: db_to_gain(a.gain_db),
                                fade_in: a.fade_in,
                                fade_out: a.fade_out,
                                data,
                            });
                        }
                    }
                    Content::Midi(m) if t.kind == TrackKind::Instrument => {
                        for n in &m.notes {
                            let on = start + time.ticks_to_samples(n.start as u64);
                            let off = (start + time.ticks_to_samples((n.start + n.length) as u64)).min(end);
                            if on >= end {
                                continue;
                            }
                            events.push((on, [0x90, n.pitch & 0x7f, n.velocity.clamp(1, 127)]));
                            events.push((off, [0x80, n.pitch & 0x7f, 0]));
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
            input,
            armed: t.armed,
            monitor: t.monitor,
            rt: TrackRt {
                rec: if t.armed && t.kind == TrackKind::Audio { res.record_ring(t.id) } else { None },
                ..Default::default()
            },
            ev_buf: Vec::with_capacity(MAX_EVENTS),
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
    master.inserts = p.master.inserts.iter().filter_map(|s| res.plugin(s.id)).collect();
    master.eq = p.master.eq.enabled.then(|| crate::eq::design(&p.master.eq, fs));

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
    }
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
