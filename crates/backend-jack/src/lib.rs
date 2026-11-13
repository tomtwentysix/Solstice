use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, MidiIn, PortFlags, ProcessScope};
use solstice_engine::{Engine, MidiEvent, Shared};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

pub const INPUTS: usize = 16;
/// Interface outputs the engine can play to, the main pair included.
pub const OUTPUTS: usize = 32;
const MAX_MIDI: usize = 512;
const AUDIO_TYPE: &str = "32 bit float mono audio";
const MIDI_TYPE: &str = "8 bit raw midi";

pub struct Backend {
    client: jack::AsyncClient<Notify, Process>,
    pub sample_rate: u32,
    /// Interface outputs, the main pair included, as there were when the backend started.
    pub outputs: usize,
    midi_name: String,
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
    /// Outputs 3 onwards.
    extra: Vec<jack::Port<AudioOut>>,
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
        let count = self.extra.len();
        let mut extra: [&mut [f32]; OUTPUTS - 2] = std::array::from_fn(|_| &mut [][..]);
        for (slot, port) in extra.iter_mut().zip(&mut self.extra) {
            *slot = port.as_mut_slice(ps);
        }
        self.engine.process_outputs(&ins, l, r, &mut extra[..count], &self.events);
        Control::Continue
    }
}

type CreateThread = unsafe extern "C" fn(
    *mut libc::c_void,
    *mut libc::pthread_t,
    libc::c_int,
    libc::c_int,
    extern "C" fn(*mut libc::c_void) -> *mut libc::c_void,
    *mut libc::c_void,
) -> libc::c_int;

extern "C" fn thread_main(arg: *mut libc::c_void) -> *mut libc::c_void {
    let f = unsafe { Box::from_raw(arg.cast::<Box<dyn FnOnce() + Send>>()) };
    f();
    std::ptr::null_mut()
}

/// Starts worker threads the way JACK starts its own, at real-time priority just under the
/// process thread. PipeWire grants that (through RTKit) only to threads it creates itself.
/// Whether a thread really got it is up to the server; the pool checks.
fn spawner(client: &Client) -> Option<impl Fn(Box<dyn FnOnce() + Send>) -> bool> {
    type Priority = unsafe extern "C" fn(*mut libc::c_void) -> libc::c_int;
    // The jack crate loads libjack itself, so look the functions up in that copy.
    let lib = [c"libjack.so.0", c"libjack.so"]
        .iter()
        .map(|n| unsafe { libc::dlopen(n.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD) })
        .find(|h| !h.is_null())?;
    let sym = |name: &std::ffi::CStr| unsafe { libc::dlsym(lib, name.as_ptr()) };
    let (prio, create) = (sym(c"jack_client_real_time_priority"), sym(c"jack_client_create_thread"));
    if prio.is_null() || create.is_null() {
        return None;
    }
    let prio: Priority = unsafe { std::mem::transmute(prio) };
    let create: CreateThread = unsafe { std::mem::transmute(create) };
    let raw = client.raw().cast::<libc::c_void>();
    // Unknown until the client is active; servers clamp the request to what they allow.
    let rt = match unsafe { prio(raw) } {
        p if p > 1 => p - 1,
        _ => 10,
    };
    Some(move |f: Box<dyn FnOnce() + Send>| {
        let arg = Box::into_raw(Box::new(f)).cast::<libc::c_void>();
        let mut thread: libc::pthread_t = 0;
        let ok = unsafe { create(raw, &mut thread, rt, 1, thread_main, arg) } == 0;
        if !ok {
            drop(unsafe { Box::from_raw(arg.cast::<Box<dyn FnOnce() + Send>>()) });
        } else {
            unsafe { libc::pthread_detach(thread) };
        }
        ok
    })
}

/// Asks PipeWire to run its graph at `rate`, or at its own choice with 0. This changes the rate
/// for every application on the server, and the change reaches clients a moment later, so a
/// connected client has to reconnect to pick it up. A plain JACK server fixes its rate when it
/// starts, so there this fails.
pub fn request_rate(rate: u32) -> Result<(), String> {
    let out = std::process::Command::new("pw-metadata")
        .args(["-n", "settings", "0", "clock.force-rate", &rate.to_string()])
        .output()
        .map_err(|_| "Only PipeWire can change its sample rate while running; restart JACK at the new rate instead".to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

impl Backend {
    /// Opens the JACK (or PipeWire JACK) client, registers ports and wires them to the hardware.
    /// `workers` extra real-time threads share the track processing.
    pub fn start(name: &str, workers: usize, make_engine: impl FnOnce(u32) -> Engine) -> Result<Backend, jack::Error> {
        let (client, _) = Client::new(name, ClientOptions::NO_START_SERVER)?;
        let sample_rate = client.sample_rate() as u32;
        let inputs = (0..INPUTS)
            .map(|i| client.register_port(&format!("in_{}", i + 1), AudioIn::default()))
            .collect::<Result<Vec<_>, _>>()?;
        let out_l = client.register_port("out_L", AudioOut::default())?;
        let out_r = client.register_port("out_R", AudioOut::default())?;
        let playback = client.ports(None, Some(AUDIO_TYPE), PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT).len();
        let extra = (3..=playback.min(OUTPUTS))
            .map(|i| client.register_port(&format!("out_{i}"), AudioOut::default()))
            .collect::<Result<Vec<_>, _>>()?;
        let midi = client.register_port("midi_in", MidiIn::default())?;
        let mut engine = make_engine(sample_rate);
        if workers > 0
            && let Some(spawn) = spawner(&client)
        {
            engine.set_workers(workers, &spawn, true);
        }

        let in_names: Vec<String> = inputs.iter().filter_map(|p| p.name().ok()).collect();
        let mut out_names = vec![out_l.name()?, out_r.name()?];
        out_names.extend(extra.iter().filter_map(|p| p.name().ok()));
        let outputs = 2 + extra.len();
        let midi_name = midi.name()?;
        let shared = engine.shared();

        let process = Process { engine, inputs, out_l, out_r, extra, midi, events: Vec::with_capacity(MAX_MIDI) };
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
        Ok(Backend { client, sample_rate, outputs, midi_name })
    }

    /// Hardware MIDI sources (keyboards, controllers), by port name.
    pub fn midi_sources(&self) -> Vec<String> {
        self.client.as_client().ports(None, Some(MIDI_TYPE), PortFlags::IS_PHYSICAL | PortFlags::IS_OUTPUT)
    }

    pub fn midi_connected(&self, source: &str) -> bool {
        let c = self.client.as_client();
        c.port_by_name(&self.midi_name).is_some_and(|p| p.is_connected_to(source).unwrap_or(false))
    }

    pub fn set_midi_connected(&self, source: &str, on: bool) {
        let c = self.client.as_client();
        let _ = if on {
            c.connect_ports_by_name(source, &self.midi_name)
        } else {
            c.disconnect_ports_by_name(source, &self.midi_name)
        };
    }

    /// Hardware audio outputs, in the order `out_L`, `out_R`, `out_3` ... feed them.
    pub fn playback_ports(&self) -> Vec<String> {
        self.client.as_client().ports(None, Some(AUDIO_TYPE), PortFlags::IS_PHYSICAL | PortFlags::IS_INPUT)
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
