//! CLAP plug-in hosting: scanning `.clap` modules and running their plug-ins in the engine.
//! Main-thread calls (lifecycle, parameters, state) come from the UI thread; `process` runs
//! on the audio thread through `Processor`.

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::params::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_vst3::{ClassInfo, ParamInfo};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};

/// Class IDs of CLAP plug-ins start with this, so slots can tell them from VST3 classes.
pub const PREFIX: &str = "clap:";

pub fn default_paths() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::env::var_os("CLAP_PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join(".clap"));
    }
    v.extend(["/usr/lib64/clap", "/usr/lib/clap", "/usr/local/lib/clap"].map(PathBuf::from));
    v
}

pub fn find_modules(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "clap") && p.is_file() {
                out.push(p);
            } else if p.is_dir() && depth < 4 {
                walk(&p, out, depth + 1);
            }
        }
    }
    let mut out = Vec::new();
    for d in dirs {
        walk(d, &mut out, 0);
    }
    out.sort();
    out.dedup();
    out
}

/// A loaded module. Modules stay loaded for the life of the process: CLAP's `deinit` may
/// only run once nothing from it is left, which isn't worth tracking.
struct Module {
    factory: *const clap_plugin_factory,
}

unsafe impl Send for Module {}
unsafe impl Sync for Module {}

fn module(path: &Path) -> Result<&'static Module, String> {
    static MODULES: OnceLock<Mutex<HashMap<PathBuf, &'static Module>>> = OnceLock::new();
    let mut map = MODULES.get_or_init(Default::default).lock().unwrap();
    if let Some(m) = map.get(path) {
        return Ok(m);
    }
    let cpath = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let handle = unsafe { libc::dlopen(cpath.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if handle.is_null() {
        let err = unsafe { CStr::from_ptr(libc::dlerror()) };
        return Err(err.to_string_lossy().into_owned());
    }
    let entry = unsafe { libc::dlsym(handle, c"clap_entry".as_ptr()) } as *const clap_plugin_entry;
    if entry.is_null() {
        return Err("not a CLAP module".into());
    }
    let entry = unsafe { &*entry };
    if !clap_sys::version::clap_version_is_compatible(entry.clap_version) {
        return Err("needs a newer version of CLAP".into());
    }
    let ok = unsafe { entry.init.map(|f| f(cpath.as_ptr())).unwrap_or(false) };
    if !ok {
        return Err("the module failed to initialise".into());
    }
    let factory = unsafe { entry.get_factory.map(|f| f(CLAP_PLUGIN_FACTORY_ID.as_ptr())).unwrap_or(std::ptr::null()) }
        as *const clap_plugin_factory;
    if factory.is_null() {
        return Err("the module has no plug-in factory".into());
    }
    let m: &'static Module = Box::leak(Box::new(Module { factory }));
    map.insert(path.to_path_buf(), m);
    Ok(m)
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// CLAP features as the VST3-style subcategories the rest of Solstice groups by.
fn subcategories(features: &[String]) -> String {
    let mut out = Vec::new();
    let has = |f: &str| features.iter().any(|x| x == f);
    out.push(if has("instrument") { "Instrument" } else { "Fx" });
    for (feature, cat) in [
        ("equalizer", "EQ"),
        ("compressor", "Dynamics"),
        ("limiter", "Dynamics"),
        ("gate", "Dynamics"),
        ("expander", "Dynamics"),
        ("reverb", "Reverb"),
        ("delay", "Delay"),
        ("distortion", "Distortion"),
        ("filter", "Filter"),
        ("analyzer", "Analyzer"),
        ("synthesizer", "Synth"),
        ("sampler", "Sampler"),
        ("drum", "Drum"),
        ("mono", "Mono"),
        ("stereo", "Stereo"),
    ] {
        if has(feature) && !out.contains(&cat) {
            out.push(cat);
        }
    }
    out.join("|")
}

/// The plug-ins in a `.clap` module. Run in the scanner process.
pub fn scan(path: &Path) -> Result<Vec<ClassInfo>, String> {
    let m = module(path)?;
    let f = unsafe { &*m.factory };
    let count = unsafe { f.get_plugin_count.map(|c| c(m.factory)).unwrap_or(0) };
    let mut out = Vec::new();
    for i in 0..count {
        let d = unsafe { f.get_plugin_descriptor.map(|g| g(m.factory, i)).unwrap_or(std::ptr::null()) };
        if d.is_null() {
            continue;
        }
        let d = unsafe { &*d };
        let mut features = Vec::new();
        let mut p = d.features;
        while !p.is_null() && unsafe { !(*p).is_null() } {
            features.push(unsafe { text(*p) });
            p = unsafe { p.add(1) };
        }
        out.push(ClassInfo {
            uid: format!("{PREFIX}{}", unsafe { text(d.id) }),
            name: unsafe { text(d.name) },
            vendor: unsafe { text(d.vendor) },
            subcategories: subcategories(&features),
            path: path.to_path_buf(),
        });
    }
    Ok(out)
}

/// The host side of one plug-in instance; `clap_host::host_data` points here.
struct HostData {
    callback: AtomicBool,
}

unsafe extern "C" fn host_get_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    std::ptr::null()
}
unsafe extern "C" fn host_request_restart(_: *const clap_host) {}
unsafe extern "C" fn host_request_process(_: *const clap_host) {}
unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    let data = unsafe { (*host).host_data as *const HostData };
    if !data.is_null() {
        unsafe { (*data).callback.store(true, Relaxed) };
    }
}

/// The plug-in itself, shared by the UI-side `Instance` and the engine-side `Processor`.
/// The last owner to go (the UI thread, once the engine has let go) tears it down.
struct Handle {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    host_data: Box<HostData>,
    processing: AtomicBool,
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn ext<T>(&self, id: &CStr) -> Option<&T> {
        let p = unsafe { (*self.plugin).get_extension.map(|g| g(self.plugin, id.as_ptr())).unwrap_or(std::ptr::null()) };
        (!p.is_null()).then(|| unsafe { &*(p as *const T) })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let p = unsafe { &*self.plugin };
        unsafe {
            if self.processing.load(Relaxed)
                && let Some(f) = p.stop_processing
            {
                f(self.plugin);
            }
            if let Some(f) = p.deactivate {
                f(self.plugin);
            }
            if let Some(f) = p.destroy {
                f(self.plugin);
            }
        }
    }
}

#[derive(Clone)]
struct Param {
    id: u32,
    title: String,
    min: f64,
    max: f64,
    default: f64,
    stepped: bool,
    hidden: bool,
    read_only: bool,
}

pub struct Instance {
    handle: Arc<Handle>,
    pub info: ClassInfo,
    params: Vec<Param>,
    /// Parameter changes for the audio thread, as (id, plain value).
    to_proc: Mutex<rtrb::Producer<(u32, f64)>>,
    from_ui: Mutex<Option<rtrb::Consumer<(u32, f64)>>>,
    /// Values sent since the last read back from the plug-in, for when it isn't processing.
    sent: Mutex<HashMap<u32, f64>>,
    ports: (u32, u32),
    latency: u32,
    sample_rate: f64,
}

const NAME: &CStr = c"Solstice";
const VENDOR: &CStr = c"SolarFlare Developments";
const URL: &CStr = c"";
const VERSION: &CStr = c"0.1";

impl Instance {
    pub fn open(info: &ClassInfo, sample_rate: f64, max_block: usize) -> Result<Arc<Instance>, String> {
        let m = module(&info.path)?;
        let id = CString::new(info.uid.strip_prefix(PREFIX).unwrap_or(&info.uid)).map_err(|e| e.to_string())?;
        let host_data = Box::new(HostData { callback: AtomicBool::new(false) });
        let host = Box::new(clap_host {
            clap_version: CLAP_VERSION,
            host_data: &*host_data as *const HostData as *mut c_void,
            name: NAME.as_ptr(),
            vendor: VENDOR.as_ptr(),
            url: URL.as_ptr(),
            version: VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_request_process),
            request_callback: Some(host_request_callback),
        });
        let f = unsafe { &*m.factory };
        let plugin = unsafe { f.create_plugin.map(|c| c(m.factory, &*host, id.as_ptr())).unwrap_or(std::ptr::null()) };
        if plugin.is_null() {
            return Err("the plug-in could not be created".into());
        }
        let p = unsafe { &*plugin };
        if !unsafe { p.init.map(|i| i(plugin)).unwrap_or(false) } {
            unsafe { p.destroy.map(|d| d(plugin)) };
            return Err("the plug-in failed to initialise".into());
        }
        let handle = Arc::new(Handle { plugin, _host: host, host_data, processing: AtomicBool::new(false) });
        if !unsafe { p.activate.map(|a| a(plugin, sample_rate, 1, max_block as u32)).unwrap_or(false) } {
            return Err("the plug-in failed to activate".into());
        }

        let mut params = Vec::new();
        if let Some(ext) = handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS) {
            let n = unsafe { ext.count.map(|c| c(plugin)).unwrap_or(0) };
            for i in 0..n {
                let mut pi: clap_param_info = unsafe { std::mem::zeroed() };
                if unsafe { ext.get_info.map(|g| g(plugin, i, &mut pi)).unwrap_or(false) } {
                    params.push(Param {
                        id: pi.id,
                        title: unsafe { text(pi.name.as_ptr()) },
                        min: pi.min_value,
                        max: pi.max_value,
                        default: pi.default_value,
                        stepped: pi.flags & CLAP_PARAM_IS_STEPPED != 0,
                        hidden: pi.flags & CLAP_PARAM_IS_HIDDEN != 0,
                        read_only: pi.flags & CLAP_PARAM_IS_READONLY != 0,
                    });
                }
            }
        }
        let channels = |input: bool| -> u32 {
            let Some(ext) = handle.ext::<clap_plugin_audio_ports>(CLAP_EXT_AUDIO_PORTS) else { return 2 };
            if unsafe { ext.count.map(|c| c(plugin, input)).unwrap_or(0) } == 0 {
                return 0;
            }
            let mut info: clap_audio_port_info = unsafe { std::mem::zeroed() };
            if unsafe { ext.get.map(|g| g(plugin, 0, input, &mut info)).unwrap_or(false) } { info.channel_count.min(2) } else { 2 }
        };
        let ports = (channels(true), channels(false));
        let latency = handle.ext::<clap_plugin_latency>(CLAP_EXT_LATENCY).map_or(0, |l| unsafe { l.get.map(|g| g(plugin)).unwrap_or(0) });
        let (tx, rx) = rtrb::RingBuffer::new(1024);
        Ok(Arc::new(Instance {
            handle,
            info: info.clone(),
            params,
            to_proc: Mutex::new(tx),
            from_ui: Mutex::new(Some(rx)),
            sent: Mutex::default(),
            ports,
            latency,
            sample_rate,
        }))
    }

    /// The engine side. There is one per instance; a second call returns a silent stand-in.
    pub fn processor(&self, max_block: usize) -> Box<dyn Plugin> {
        match self.from_ui.lock().unwrap().take() {
            Some(rx) => {
                let ranges = self.params.iter().map(|p| (p.id, p.min, p.max, p.stepped)).collect();
                Box::new(Processor::new(self.handle.clone(), rx, ranges, self.ports, max_block, self.sample_rate))
            }
            None => Box::new(Silent),
        }
    }

    pub fn latency(&self) -> u32 {
        self.latency
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        self.params
            .iter()
            .map(|p| ParamInfo {
                id: p.id,
                title: p.title.clone(),
                units: String::new(),
                default_value: norm(p, p.default),
                steps: if p.stepped { (p.max - p.min).round() as i32 } else { 0 },
                read_only: p.read_only,
                hidden: p.hidden,
            })
            .collect()
    }

    fn param_def(&self, id: u32) -> Option<&Param> {
        self.params.iter().find(|p| p.id == id)
    }

    /// Normalised 0..1.
    pub fn param(&self, id: u32) -> f64 {
        let Some(p) = self.param_def(id) else { return 0.0 };
        if let Some(v) = self.sent.lock().unwrap().get(&id) {
            return norm(p, *v);
        }
        let mut v = p.default;
        if let Some(ext) = self.handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS) {
            unsafe { ext.get_value.map(|g| g(self.handle.plugin, id, &mut v)) };
        }
        norm(p, v)
    }

    pub fn set_param(&self, id: u32, value: f64) {
        let Some(p) = self.param_def(id) else { return };
        let plain = p.min + value.clamp(0.0, 1.0) * (p.max - p.min);
        let plain = if p.stepped { plain.round() } else { plain };
        self.sent.lock().unwrap().insert(id, plain);
        let _ = self.to_proc.lock().unwrap().push((id, plain));
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        let Some(p) = self.param_def(id) else { return String::new() };
        let plain = p.min + value.clamp(0.0, 1.0) * (p.max - p.min);
        let mut buf = [0 as c_char; 64];
        let ok = self.handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS).is_some_and(|ext| unsafe {
            ext.value_to_text.map(|t| t(self.handle.plugin, id, plain, buf.as_mut_ptr(), buf.len() as u32)).unwrap_or(false)
        });
        if ok { unsafe { text(buf.as_ptr()) } } else { format!("{plain:.2}") }
    }

    pub fn state(&self) -> Vec<u8> {
        let Some(ext) = self.handle.ext::<clap_plugin_state>(CLAP_EXT_STATE) else { return Vec::new() };
        let mut data: Vec<u8> = Vec::new();
        unsafe extern "C" fn write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
            let v = unsafe { &mut *((*s).ctx as *mut Vec<u8>) };
            v.extend_from_slice(unsafe { std::slice::from_raw_parts(buf as *const u8, size as usize) });
            size as i64
        }
        let stream = clap_ostream { ctx: &mut data as *mut Vec<u8> as *mut c_void, write: Some(write) };
        let ok = unsafe { ext.save.map(|s| s(self.handle.plugin, &stream)).unwrap_or(false) };
        if ok { data } else { Vec::new() }
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        let Some(ext) = self.handle.ext::<clap_plugin_state>(CLAP_EXT_STATE) else { return false };
        struct Reader<'a> {
            data: &'a [u8],
            pos: usize,
        }
        unsafe extern "C" fn read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
            let r = unsafe { &mut *((*s).ctx as *mut Reader) };
            let n = (size as usize).min(r.data.len() - r.pos);
            unsafe { std::ptr::copy_nonoverlapping(r.data[r.pos..].as_ptr(), buf as *mut u8, n) };
            r.pos += n;
            n as i64
        }
        let mut reader = Reader { data, pos: 0 };
        let stream = clap_istream { ctx: &mut reader as *mut Reader as *mut c_void, read: Some(read) };
        let ok = unsafe { ext.load.map(|l| l(self.handle.plugin, &stream)).unwrap_or(false) };
        self.sent.lock().unwrap().clear();
        ok
    }

    /// Runs the plug-in's main-thread callback if it asked for one. Call regularly.
    pub fn idle(&self) {
        if self.handle.host_data.callback.swap(false, Relaxed) {
            let p = unsafe { &*self.handle.plugin };
            unsafe { p.on_main_thread.map(|f| f(self.handle.plugin)) };
        }
    }
}

fn norm(p: &Param, v: f64) -> f64 {
    if p.max > p.min { ((v - p.min) / (p.max - p.min)).clamp(0.0, 1.0) } else { 0.0 }
}

/// Stands in when a processor has already been handed out.
struct Silent;

impl Plugin for Silent {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        l.fill(0.0);
        r.fill(0.0);
    }
}

/// One input event, big enough for any kind this host sends.
#[repr(C)]
#[derive(Clone, Copy)]
union Event {
    header: clap_event_header,
    note: clap_event_note,
    midi: clap_event_midi,
    param: clap_event_param_value,
}

const MAX_EVENTS: usize = 1024;

struct InEvents {
    events: Vec<Event>,
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    let ev = unsafe { &*((*list).ctx as *const InEvents) };
    ev.events.len() as u32
}

unsafe extern "C" fn in_get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    let ev = unsafe { &*((*list).ctx as *const InEvents) };
    ev.events.get(index as usize).map_or(std::ptr::null(), |e| unsafe { &e.header as *const _ })
}

unsafe extern "C" fn out_push(_: *const clap_output_events, _: *const clap_event_header) -> bool {
    true
}

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header { size: std::mem::size_of::<T>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

/// The audio-thread side: turns each period into a `clap_process` call.
pub struct Processor {
    handle: Arc<Handle>,
    params: rtrb::Consumer<(u32, f64)>,
    /// Each parameter's id, range and whether it's stepped, for automation's 0..1 values.
    ranges: Vec<(u32, f64, f64, bool)>,
    sample_rate: f64,
    events: Box<InEvents>,
    input: [Vec<f32>; 2],
    ports: (u32, u32),
    steady: i64,
}

unsafe impl Send for Processor {}

impl Processor {
    fn new(
        handle: Arc<Handle>,
        params: rtrb::Consumer<(u32, f64)>,
        ranges: Vec<(u32, f64, f64, bool)>,
        ports: (u32, u32),
        max_block: usize,
        sample_rate: f64,
    ) -> Self {
        Processor {
            handle,
            params,
            ranges,
            sample_rate,
            events: Box::new(InEvents { events: Vec::with_capacity(MAX_EVENTS) }),
            input: [vec![0.0; max_block], vec![0.0; max_block]],
            ports,
            steady: 0,
        }
    }

    fn push(&mut self, e: Event) {
        if self.events.events.len() < MAX_EVENTS {
            self.events.events.push(e);
        }
    }

    fn push_param(&mut self, id: u32, value: f64) {
        let mut e: clap_event_param_value = unsafe { std::mem::zeroed() };
        e.header = header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE);
        e.param_id = id;
        (e.note_id, e.port_index, e.channel, e.key) = (-1, -1, -1, -1);
        e.value = value;
        self.push(Event { param: e });
    }
}

impl Plugin for Processor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], midi: &[MidiEvent], ctx: &ProcessContext) {
        let plugin = self.handle.plugin;
        let p = unsafe { &*plugin };
        if !self.handle.processing.load(Relaxed) {
            let ok = unsafe { p.start_processing.map(|s| s(plugin)).unwrap_or(true) };
            self.handle.processing.store(ok, Relaxed);
            if !ok {
                return;
            }
        }
        let n = l.len().min(self.input[0].len());
        // Automation from `set_param` was queued before this call; keep it, add the UI's.
        while let Ok((id, value)) = self.params.pop() {
            self.push_param(id, value);
        }
        for m in midi.iter().filter(|m| (m.frame as usize) < n) {
            let status = m.data[0] & 0xf0;
            let on = status == 0x90 && m.data[2] > 0;
            let off = status == 0x80 || (status == 0x90 && m.data[2] == 0);
            let e = if on || off {
                let mut e: clap_event_note = unsafe { std::mem::zeroed() };
                e.header = header::<clap_event_note>(m.frame, if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF });
                (e.note_id, e.port_index, e.channel, e.key) = (-1, 0, (m.data[0] & 0x0f) as i16, m.data[1] as i16);
                e.velocity = m.data[2] as f64 / 127.0;
                Event { note: e }
            } else {
                Event { midi: clap_event_midi { header: header::<clap_event_midi>(m.frame, CLAP_EVENT_MIDI), port_index: 0, data: m.data } }
            };
            self.push(e);
        }

        let seconds = ctx.position as f64 / self.sample_rate;
        let beats = seconds * ctx.tempo / 60.0;
        let mut transport: clap_event_transport = unsafe { std::mem::zeroed() };
        transport.header = header::<clap_event_transport>(0, CLAP_EVENT_TRANSPORT);
        transport.flags = CLAP_TRANSPORT_HAS_TEMPO
            | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
            | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
            | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE;
        if ctx.playing {
            transport.flags |= CLAP_TRANSPORT_IS_PLAYING;
        }
        transport.tempo = ctx.tempo;
        transport.song_pos_beats = (beats * CLAP_BEATTIME_FACTOR as f64) as i64;
        transport.song_pos_seconds = (seconds * CLAP_SECTIME_FACTOR as f64) as i64;
        transport.tsig_num = ctx.sig_num as u16;
        transport.tsig_denom = ctx.sig_den as u16;

        self.input[0][..n].copy_from_slice(&l[..n]);
        self.input[1][..n].copy_from_slice(&r[..n]);
        let mut in_ptrs = [self.input[0].as_mut_ptr(), self.input[1].as_mut_ptr()];
        let mut out_ptrs = [l.as_mut_ptr(), r.as_mut_ptr()];
        let audio_in = clap_audio_buffer {
            data32: in_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: self.ports.0,
            latency: 0,
            constant_mask: 0,
        };
        let mut audio_out = clap_audio_buffer {
            data32: out_ptrs.as_mut_ptr(),
            data64: std::ptr::null_mut(),
            channel_count: self.ports.1,
            latency: 0,
            constant_mask: 0,
        };
        let in_events = clap_input_events { ctx: &*self.events as *const InEvents as *mut c_void, size: Some(in_size), get: Some(in_get) };
        let out_events = clap_output_events { ctx: std::ptr::null_mut(), try_push: Some(out_push) };
        let process = clap_process {
            steady_time: self.steady,
            frames_count: n as u32,
            transport: &transport,
            audio_inputs: &audio_in,
            audio_outputs: &mut audio_out,
            audio_inputs_count: (self.ports.0 > 0) as u32,
            audio_outputs_count: (self.ports.1 > 0) as u32,
            in_events: &in_events,
            out_events: &out_events,
        };
        unsafe { p.process.map(|f| f(plugin, &process)) };
        self.events.events.clear();
        self.steady += n as i64;
        // A mono plug-in only writes the left channel.
        if self.ports.1 == 1 {
            r[..n].copy_from_slice(&l[..n]);
        }
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(&(_, min, max, stepped)) = self.ranges.iter().find(|r| r.0 == id) {
            let plain = min + value.clamp(0.0, 1.0) * (max - min);
            self.push_param(id, if stepped { plain.round() } else { plain });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_map_to_categories() {
        let f = |v: &[&str]| subcategories(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(f(&["audio-effect", "equalizer", "stereo"]), "Fx|EQ|Stereo");
        assert_eq!(f(&["instrument", "synthesizer"]), "Instrument|Synth");
    }

    /// Hosts a real plug-in end to end when LSP's CLAP bundle is installed.
    #[test]
    fn hosts_an_installed_plugin() {
        let Some(path) = find_modules(&default_paths()).into_iter().find(|p| p.ends_with("lsp-plugins.clap")) else { return };
        let classes = scan(&path).unwrap();
        assert!(classes.len() > 10);
        let info = classes.iter().find(|c| c.uid.contains("para_equalizer_x8_stereo")).unwrap_or(&classes[0]).clone();
        let inst = Instance::open(&info, 48000.0, 512).unwrap();
        assert!(!inst.params().is_empty());
        let first = inst.params()[0].id;
        inst.set_param(first, 0.5);
        let mut proc = inst.processor(512);
        let ctx = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.25f32; 512], vec![0.25f32; 512]);
            proc.process(&mut l, &mut r, &[], &ctx);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        }
        let state = inst.state();
        assert!(!state.is_empty());
        assert!(inst.set_state(&state));
        drop(proc);
    }
}
