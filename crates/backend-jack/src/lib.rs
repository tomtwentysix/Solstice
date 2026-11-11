use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, MidiIn, PortFlags, ProcessScope};
use solstice_engine::{Engine, MidiEvent, Shared};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

pub const INPUTS: usize = 16;
const MAX_MIDI: usize = 512;
const AUDIO_TYPE: &str = "32 bit float mono audio";
const MIDI_TYPE: &str = "8 bit raw midi";

pub struct Backend {
    client: jack::AsyncClient<Notify, Process>,
    pub sample_rate: u32,
}

struct Notify {
    shared: Arc<Shared>,
}

impl jack::NotificationHandler for Notify {
    fn xrun(&mut self, _: &Client) -> Control {
        self.shared.xruns.fetch_add(1, Relaxed);
        Control::Continue
    }
}

struct Process {
    engine: Engine,
    inputs: Vec<jack::Port<AudioIn>>,
    out_l: jack::Port<AudioOut>,
    out_r: jack::Port<AudioOut>,
    midi: jack::Port<MidiIn>,
    events: Vec<MidiEvent>,
}

impl jack::ProcessHandler for Process {
    fn process(&mut self, _: &Client, ps: &ProcessScope) -> Control {
        self.events.clear();
        for m in self.midi.iter(ps) {
            if m.bytes.len() <= 3 && self.events.len() < MAX_MIDI {
                let mut data = [0u8; 3];
                data[..m.bytes.len()].copy_from_slice(m.bytes);
                self.events.push(MidiEvent { frame: m.time, data });
            }
        }
        let mut ins: [&[f32]; INPUTS] = [&[]; INPUTS];
        for (slot, port) in ins.iter_mut().zip(&self.inputs) {
            *slot = port.as_slice(ps);
        }
        let l = self.out_l.as_mut_slice(ps);
        let r = self.out_r.as_mut_slice(ps);
        self.engine.process(&ins, l, r, &self.events);
        Control::Continue
    }
}

impl Backend {
    /// Opens the JACK (or PipeWire JACK) client, registers ports and wires them to the hardware.
    pub fn start(name: &str, make_engine: impl FnOnce(u32) -> Engine) -> Result<Backend, jack::Error> {
        let (client, _) = Client::new(name, ClientOptions::NO_START_SERVER)?;
        let sample_rate = client.sample_rate() as u32;
        let inputs = (0..INPUTS)
            .map(|i| client.register_port(&format!("in_{}", i + 1), AudioIn::default()))
            .collect::<Result<Vec<_>, _>>()?;
        let out_l = client.register_port("out_L", AudioOut::default())?;
        let out_r = client.register_port("out_R", AudioOut::default())?;
        let midi = client.register_port("midi_in", MidiIn::default())?;
        let engine = make_engine(sample_rate);

        let in_names: Vec<String> = inputs.iter().filter_map(|p| p.name().ok()).collect();
        let out_names = [out_l.name()?, out_r.name()?];
        let midi_name = midi.name()?;
        let shared = engine.shared();

        let process = Process { engine, inputs, out_l, out_r, midi, events: Vec::with_capacity(MAX_MIDI) };
        let client = client.activate_async(Notify { shared }, process)?;

        let c = client.as_client();
        let phys = |ty: &str, flags| c.ports(None, Some(ty), PortFlags::IS_PHYSICAL | flags);
        for (src, dst) in phys(AUDIO_TYPE, PortFlags::IS_OUTPUT).iter().zip(&in_names) {
            let _ = c.connect_ports_by_name(src, dst);
        }
        for (src, dst) in out_names.iter().zip(phys(AUDIO_TYPE, PortFlags::IS_INPUT)) {
            let _ = c.connect_ports_by_name(src, &dst);
        }
        for src in phys(MIDI_TYPE, PortFlags::IS_OUTPUT) {
            let _ = c.connect_ports_by_name(&src, &midi_name);
        }
        Ok(Backend { client, sample_rate })
    }

    pub fn buffer_size(&self) -> u32 {
        self.client.as_client().buffer_size()
    }

    pub fn set_buffer_size(&self, frames: u32) -> Result<(), jack::Error> {
        self.client.as_client().set_buffer_size(frames)
    }

    /// Names of the physical capture ports, in the order they feed `in_1`, `in_2`, ...
    pub fn capture_ports(&self) -> Vec<String> {
        self.client.as_client().ports(None, Some(AUDIO_TYPE), PortFlags::IS_PHYSICAL | PortFlags::IS_OUTPUT)
    }

    /// Round-trip latency in frames as the server reports it for the physical ports, which
    /// includes the periods on each side.
    pub fn round_trip(&self) -> u32 {
        let c = self.client.as_client();
        let range = |name: &str, mode| {
            c.port_by_name(name).and_then(|p| p.get_latency_range(mode).ok()).map_or(0, |r| r.1)
        };
        let ins = self.capture_ports();
        let outs = c.ports(None, Some(AUDIO_TYPE), PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT);
        let capture = ins.first().map_or(0, |n| range(n, jack::LatencyType::Capture));
        let playback = outs.first().map_or(0, |n| range(n, jack::LatencyType::Playback));
        capture + playback
    }
}
