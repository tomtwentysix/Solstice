//! CLAP plug-in hosting: scanning `.clap` modules and running their plug-ins in the engine.
//! Main-thread calls (lifecycle, parameters, state) come from the UI thread; `process` runs
//! on the audio thread through `Processor`.

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports};
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::timer_support::*;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::clap_process;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_vst3::{ClassInfo, ParamInfo};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

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

/// The host side of one plug-in instance; `clap_host::host_data` points here. Plug-ins may
/// call in from any thread, so everything they touch is atomic or main-thread only.
struct HostData {
    callback: AtomicBool,
    main: ThreadId,
    rescan_values: AtomicBool,
    dirty: AtomicBool,
    /// A size the editor asked for: ((width << 32) | height) + 1, or 0 for none.
    resize: AtomicU64,
    /// The plug-in let go of its editor: 1 hidden, 2 destroyed.
    gui_closed: AtomicU8,
    /// A floating editor is open, which the plug-in may ask to show or hide.
    floating: AtomicBool,
    /// What it asked for last: 1 show, 2 hide, 0 nothing since.
    visibility: AtomicU8,
    timers: Mutex<Timers>,
    /// Registered descriptors and their `CLAP_POSIX_FD_*` flags.
    fds: Mutex<Vec<(i32, u32)>>,
}

impl HostData {
    fn new() -> HostData {
        HostData {
            callback: AtomicBool::new(false),
            main: std::thread::current().id(),
            rescan_values: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            resize: AtomicU64::new(0),
            gui_closed: AtomicU8::new(0),
            floating: AtomicBool::new(false),
            visibility: AtomicU8::new(0),
            timers: Mutex::default(),
            fds: Mutex::default(),
        }
    }
}

fn host_data<'a>(host: *const clap_host) -> Option<&'a HostData> {
    if host.is_null() {
        return None;
    }
    let data = unsafe { (*host).host_data as *const HostData };
    (!data.is_null()).then(|| unsafe { &*data })
}

#[derive(Default)]
struct Timers {
    next: u32,
    /// (id, period, next due).
    list: Vec<(u32, Duration, Instant)>,
}

impl Timers {
    fn register(&mut self, period: Duration, now: Instant) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let period = period.max(Duration::from_millis(1));
        self.list.push((id, period, now + period));
        id
    }

    fn unregister(&mut self, id: u32) -> bool {
        let n = self.list.len();
        self.list.retain(|t| t.0 != id);
        self.list.len() != n
    }

    fn contains(&self, id: u32) -> bool {
        self.list.iter().any(|t| t.0 == id)
    }

    /// The timers whose time has come, each rescheduled a period from now.
    fn due(&mut self, now: Instant) -> Vec<u32> {
        let mut out = Vec::new();
        for (id, period, next) in &mut self.list {
            if *next <= now {
                out.push(*id);
                *next = now + *period;
            }
        }
        out
    }
}

thread_local!(static AUDIO: Cell<bool> = const { Cell::new(false) });

unsafe extern "C" fn host_request_restart(_: *const clap_host) {}
unsafe extern "C" fn host_request_process(_: *const clap_host) {}
unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    if let Some(d) = host_data(host) {
        d.callback.store(true, Relaxed);
    }
}

unsafe extern "C" fn gui_resize_hints_changed(_: *const clap_host) {}
unsafe extern "C" fn gui_request_resize(host: *const clap_host, width: u32, height: u32) -> bool {
    let Some(d) = host_data(host) else { return false };
    let (w, h) = (width.min(i32::MAX as u32) as u64, height.min(i32::MAX as u32) as u64);
    d.resize.store(((w << 32) | h) + 1, Relaxed);
    true
}
fn request_visibility(host: *const clap_host, show: bool) -> bool {
    // An embedded editor is shown with the plug-in window, which is the user's to open.
    let Some(d) = host_data(host).filter(|d| d.floating.load(Relaxed)) else { return false };
    d.visibility.store(if show { 1 } else { 2 }, Relaxed);
    true
}
unsafe extern "C" fn gui_request_show(host: *const clap_host) -> bool {
    request_visibility(host, true)
}
unsafe extern "C" fn gui_request_hide(host: *const clap_host) -> bool {
    request_visibility(host, false)
}
unsafe extern "C" fn gui_closed(host: *const clap_host, was_destroyed: bool) {
    if let Some(d) = host_data(host) {
        d.gui_closed.store(if was_destroyed { 2 } else { 1 }, Relaxed);
    }
}

unsafe extern "C" fn timer_register(host: *const clap_host, period_ms: u32, id: *mut clap_id) -> bool {
    let Some(d) = host_data(host) else { return false };
    if id.is_null() {
        return false;
    }
    let t = d.timers.lock().unwrap().register(Duration::from_millis(period_ms as u64), Instant::now());
    unsafe { *id = t };
    true
}
unsafe extern "C" fn timer_unregister(host: *const clap_host, id: clap_id) -> bool {
    host_data(host).is_some_and(|d| d.timers.lock().unwrap().unregister(id))
}

unsafe extern "C" fn fd_register(host: *const clap_host, fd: i32, flags: u32) -> bool {
    let Some(d) = host_data(host) else { return false };
    let mut fds = d.fds.lock().unwrap();
    if fds.iter().any(|f| f.0 == fd) {
        return false;
    }
    fds.push((fd, flags));
    true
}
unsafe extern "C" fn fd_modify(host: *const clap_host, fd: i32, flags: u32) -> bool {
    let Some(d) = host_data(host) else { return false };
    let mut fds = d.fds.lock().unwrap();
    match fds.iter_mut().find(|f| f.0 == fd) {
        Some(f) => {
            f.1 = flags;
            true
        }
        None => false,
    }
}
unsafe extern "C" fn fd_unregister(host: *const clap_host, fd: i32) -> bool {
    let Some(d) = host_data(host) else { return false };
    let mut fds = d.fds.lock().unwrap();
    let n = fds.len();
    fds.retain(|f| f.0 != fd);
    fds.len() != n
}

// RESCAN_ALL and INFO need the plug-in deactivated, which this host never does. New values
// mean an edit, a preset loaded from the plug-in's interface say, and plug-ins needn't also
// call mark_dirty for it.
unsafe extern "C" fn params_rescan(host: *const clap_host, flags: u32) {
    let Some(d) = host_data(host) else { return };
    if flags & CLAP_PARAM_RESCAN_VALUES != 0 {
        d.dirty.store(true, Relaxed);
    }
    if flags & (CLAP_PARAM_RESCAN_VALUES | CLAP_PARAM_RESCAN_TEXT) != 0 {
        d.rescan_values.store(true, Relaxed);
    }
}
unsafe extern "C" fn params_clear(_: *const clap_host, _: clap_id, _: u32) {}
// The plug-in is always active, so a flush may only happen in `process`, which runs anyway
// while the engine does.
unsafe extern "C" fn params_request_flush(_: *const clap_host) {}

unsafe extern "C" fn state_mark_dirty(host: *const clap_host) {
    if let Some(d) = host_data(host) {
        d.dirty.store(true, Relaxed);
    }
}

unsafe extern "C" fn is_main_thread(host: *const clap_host) -> bool {
    host_data(host).is_some_and(|d| d.main == std::thread::current().id())
}
unsafe extern "C" fn is_audio_thread(_: *const clap_host) -> bool {
    AUDIO.get()
}

unsafe extern "C" fn latency_changed(_: *const clap_host) {}

static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_resize_hints_changed),
    request_resize: Some(gui_request_resize),
    request_show: Some(gui_request_show),
    request_hide: Some(gui_request_hide),
    closed: Some(gui_closed),
};
static HOST_TIMER: clap_host_timer_support =
    clap_host_timer_support { register_timer: Some(timer_register), unregister_timer: Some(timer_unregister) };
static HOST_FD: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(fd_register),
    modify_fd: Some(fd_modify),
    unregister_fd: Some(fd_unregister),
};
static HOST_PARAMS: clap_host_params =
    clap_host_params { rescan: Some(params_rescan), clear: Some(params_clear), request_flush: Some(params_request_flush) };
static HOST_STATE: clap_host_state = clap_host_state { mark_dirty: Some(state_mark_dirty) };
static HOST_THREAD: clap_host_thread_check =
    clap_host_thread_check { is_main_thread: Some(is_main_thread), is_audio_thread: Some(is_audio_thread) };
static HOST_LATENCY: clap_host_latency = clap_host_latency { changed: Some(latency_changed) };

// Plug-ins may ask from any thread, the audio thread included, so this mustn't allocate.
unsafe extern "C" fn host_get_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    fn ptr<T>(ext: &'static T) -> *const c_void {
        ext as *const T as *const c_void
    }
    if id.is_null() {
        return std::ptr::null();
    }
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_GUI {
        ptr(&HOST_GUI)
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        ptr(&HOST_TIMER)
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        ptr(&HOST_FD)
    } else if id == CLAP_EXT_PARAMS {
        ptr(&HOST_PARAMS)
    } else if id == CLAP_EXT_STATE {
        ptr(&HOST_STATE)
    } else if id == CLAP_EXT_THREAD_CHECK {
        ptr(&HOST_THREAD)
    } else if id == CLAP_EXT_LATENCY {
        ptr(&HOST_LATENCY)
    } else {
        std::ptr::null()
    }
}

/// The plug-in itself, shared by the UI-side `Instance` and the engine-side `Processor`.
/// The last owner to go (the UI thread, once the engine has let go) tears it down.
struct Handle {
    plugin: *const clap_plugin,
    _host: Box<clap_host>,
    host_data: Box<HostData>,
    processing: AtomicBool,
    /// Periods processed, to tell whether the engine is running.
    blocks: AtomicU64,
    /// Set by `hostwatch` when the yabridge host serving this plug-in exits.
    dead: Arc<AtomicBool>,
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
        // Tearing down a plug-in whose host has died would wait for that host forever.
        if self.dead.load(Relaxed) {
            return;
        }
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

/// What the plug-in reports from `process`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Out {
    /// A parameter's new plain value.
    Value(u32, f64),
    /// A gesture on a parameter began (true) or ended.
    Gesture(u32, bool),
}

fn decode(h: &clap_event_header) -> Option<Out> {
    if h.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return None;
    }
    match h.type_ {
        CLAP_EVENT_PARAM_VALUE if h.size as usize >= std::mem::size_of::<clap_event_param_value>() => {
            let e = unsafe { &*(h as *const clap_event_header as *const clap_event_param_value) };
            (e.note_id == -1 && e.key == -1).then_some(Out::Value(e.param_id, e.value))
        }
        CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END
            if h.size as usize >= std::mem::size_of::<clap_event_param_gesture>() =>
        {
            let e = unsafe { &*(h as *const clap_event_header as *const clap_event_param_gesture) };
            Some(Out::Gesture(e.param_id, h.type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN))
        }
        _ => None,
    }
}

/// Each parameter's latest plain value as f64 bits, in parameter order, written by whichever
/// thread changed it last. NaN means unknown: ask the plug-in.
type Values = Arc<[AtomicU64]>;

/// How long the engine must have processed nothing before an open editor's values are read
/// back from the plug-in instead.
const ENGINE_IDLE: Duration = Duration::from_millis(250);

/// The plug-in's own interface: embedded in a window of ours, or floating in its own.
struct Editor {
    /// The X11 window it sits in; 0 when floating.
    window: u64,
    size: (i32, i32),
}

pub struct Instance {
    handle: Arc<Handle>,
    pub info: ClassInfo,
    params: Vec<Param>,
    values: Values,
    /// Parameter changes for the audio thread, as (id, plain value).
    to_proc: Mutex<rtrb::Producer<(u32, f64)>>,
    gestures: Mutex<rtrb::Consumer<Out>>,
    /// The engine side's ends of those queues, until `processor` takes them.
    proc_ends: Mutex<Option<(rtrb::Consumer<(u32, f64)>, rtrb::Producer<Out>)>>,
    /// Parameters the user is holding in the plug-in's interface.
    held: Mutex<Vec<u32>>,
    /// The engine's period count when `idle` last saw it change, and when.
    engine_seen: Mutex<(u64, Instant)>,
    /// The plug-in's values when last polled while the engine was idle; empty when it isn't.
    polled: Mutex<Vec<f64>>,
    /// The X11 interface: Some(false) embeds, Some(true) only floats.
    gui: Option<bool>,
    editor: Mutex<Option<Editor>>,
    ports: (u32, u32),
    latency: u32,
    sample_rate: f64,
    bridged: bool,
    /// The last state read from or given to the plug-in, returned once it has died.
    kept: Mutex<Vec<u8>>,
}

const NAME: &CStr = c"Solstice";
const VENDOR: &CStr = c"SolarFlare Developments";
const URL: &CStr = c"";
const VERSION: &CStr = c"0.1";

impl Instance {
    pub fn open(info: &ClassInfo, sample_rate: f64, max_block: usize) -> Result<Arc<Instance>, String> {
        let m = module(&info.path)?;
        let id = CString::new(info.uid.strip_prefix(PREFIX).unwrap_or(&info.uid)).map_err(|e| e.to_string())?;
        let host_data = Box::new(HostData::new());
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
        let (plugin, hosts) = solstice_vst3::hostwatch::opening(|| unsafe {
            f.create_plugin.map(|c| c(m.factory, &*host, id.as_ptr())).unwrap_or(std::ptr::null())
        });
        if plugin.is_null() {
            return Err("the plug-in could not be created".into());
        }
        let p = unsafe { &*plugin };
        if !unsafe { p.init.map(|i| i(plugin)).unwrap_or(false) } {
            unsafe { p.destroy.map(|d| d(plugin)) };
            return Err("the plug-in failed to initialise".into());
        }
        let dead = Arc::new(AtomicBool::new(false));
        for &pid in &hosts {
            solstice_vst3::hostwatch::watch(pid, dead.clone());
        }
        let handle = Arc::new(Handle {
            plugin,
            _host: host,
            host_data,
            processing: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            dead,
        });
        if !unsafe { p.activate.map(|a| a(plugin, sample_rate, 1, max_block as u32)).unwrap_or(false) } {
            return Err("the plug-in failed to activate".into());
        }
        let mut inst = Instance::new(handle, info.clone(), sample_rate);
        inst.bridged = !hosts.is_empty();
        Ok(Arc::new(inst))
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn dead_for_test(kept: Vec<u8>) -> Arc<Instance> {
        let host_data = Box::new(HostData::new());
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
        let handle = Arc::new(Handle {
            plugin: std::ptr::null(),
            _host: host,
            host_data,
            processing: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            dead: Arc::new(AtomicBool::new(true)),
        });
        let (tx, rx) = rtrb::RingBuffer::new(16);
        let (gtx, grx) = rtrb::RingBuffer::new(16);
        let info = ClassInfo { uid: String::new(), name: String::new(), vendor: String::new(), subcategories: String::new(), path: Default::default() };
        Arc::new(Instance {
            handle,
            info,
            params: Vec::new(),
            values: Values::from(Vec::new()),
            to_proc: Mutex::new(tx),
            gestures: Mutex::new(grx),
            proc_ends: Mutex::new(Some((rx, gtx))),
            held: Mutex::default(),
            engine_seen: Mutex::new((0, Instant::now())),
            polled: Mutex::default(),
            gui: Some(false),
            editor: Mutex::default(),
            ports: (2, 2),
            latency: 0,
            sample_rate: 48000.0,
            bridged: true,
            kept: Mutex::new(kept),
        })
    }

    pub fn crashed(&self) -> bool {
        self.handle.dead.load(Relaxed)
    }

    /// Whether a yabridge host was found for this instance, so a crash would be noticed.
    pub fn bridged(&self) -> bool {
        self.bridged
    }

    /// Reads what the host needs from an initialised, active plug-in.
    fn new(handle: Arc<Handle>, info: ClassInfo, sample_rate: f64) -> Instance {
        let plugin = handle.plugin;
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
        let gui = handle.ext::<clap_plugin_gui>(CLAP_EXT_GUI).and_then(|g| {
            let supported = |floating| unsafe { g.is_api_supported.is_some_and(|f| f(plugin, CLAP_WINDOW_API_X11.as_ptr(), floating)) };
            if supported(false) {
                Some(false)
            } else {
                supported(true).then_some(true)
            }
        });
        let values: Values = params.iter().map(|_| AtomicU64::new(f64::NAN.to_bits())).collect();
        let (tx, rx) = rtrb::RingBuffer::new(1024);
        let (gtx, grx) = rtrb::RingBuffer::new(256);
        let inst = Instance {
            handle,
            info,
            params,
            values,
            to_proc: Mutex::new(tx),
            gestures: Mutex::new(grx),
            proc_ends: Mutex::new(Some((rx, gtx))),
            held: Mutex::default(),
            engine_seen: Mutex::new((0, Instant::now())),
            polled: Mutex::default(),
            gui,
            editor: Mutex::default(),
            ports,
            latency,
            sample_rate,
            bridged: false,
            kept: Mutex::default(),
        };
        inst.read_values();
        inst
    }

    /// Each parameter's value as the plug-in has it now, NaN where it won't say.
    fn plugin_values(&self) -> Vec<f64> {
        if self.crashed() {
            return vec![f64::NAN; self.params.len()];
        }
        let get = self.handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS).and_then(|ext| ext.get_value);
        self.params
            .iter()
            .map(|p| {
                let mut v = f64::NAN;
                match get {
                    Some(get) if unsafe { get(self.handle.plugin, p.id, &mut v) } => v,
                    _ => f64::NAN,
                }
            })
            .collect()
    }

    fn read_values(&self) {
        for (slot, v) in self.values.iter().zip(self.plugin_values()) {
            slot.store(v.to_bits(), Relaxed);
        }
    }

    /// Takes up values the plug-in changed since it was last polled, as edits. Values the host
    /// set are left alone: they reach the plug-in once the engine runs again.
    fn poll_values(&self) {
        let now = self.plugin_values();
        let mut polled = self.polled.lock().unwrap();
        if polled.len() == now.len() {
            for ((slot, v), was) in self.values.iter().zip(&now).zip(polled.iter()) {
                if !v.is_nan() && (v - was).abs() > 1e-9 {
                    slot.store(v.to_bits(), Relaxed);
                    self.handle.host_data.dirty.store(true, Relaxed);
                }
            }
        }
        *polled = now;
    }

    /// The engine side. There is one per instance; a second call returns a silent stand-in.
    pub fn processor(&self, max_block: usize) -> Box<dyn Plugin> {
        match self.proc_ends.lock().unwrap().take() {
            Some((rx, gestures)) => {
                let ranges = self.params.iter().map(|p| (p.id, p.min, p.max, p.stepped)).collect();
                let out = OutCtx {
                    handle: self.handle.clone(),
                    gestures,
                    values: self.values.clone(),
                    ids: self.params.iter().map(|p| p.id).collect(),
                };
                Box::new(Processor::new(self.handle.clone(), rx, out, ranges, self.ports, max_block, self.sample_rate))
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

    fn index(&self, id: u32) -> Option<usize> {
        self.params.iter().position(|p| p.id == id)
    }

    /// Normalised 0..1.
    pub fn param(&self, id: u32) -> f64 {
        let Some(i) = self.index(id) else { return 0.0 };
        let p = &self.params[i];
        let v = f64::from_bits(self.values[i].load(Relaxed));
        if !v.is_nan() {
            return norm(p, v);
        }
        let mut v = p.default;
        if !self.crashed()
            && let Some(ext) = self.handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS)
        {
            unsafe { ext.get_value.map(|g| g(self.handle.plugin, id, &mut v)) };
        }
        norm(p, v)
    }

    pub fn set_param(&self, id: u32, value: f64) {
        let Some(i) = self.index(id) else { return };
        let p = &self.params[i];
        let plain = p.min + value.clamp(0.0, 1.0) * (p.max - p.min);
        let plain = if p.stepped { plain.round() } else { plain };
        self.values[i].store(plain.to_bits(), Relaxed);
        let _ = self.to_proc.lock().unwrap().push((id, plain));
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        let Some(i) = self.index(id).filter(|_| !self.crashed()) else { return String::new() };
        let p = &self.params[i];
        let plain = p.min + value.clamp(0.0, 1.0) * (p.max - p.min);
        let mut buf = [0 as c_char; 64];
        let ok = self.handle.ext::<clap_plugin_params>(CLAP_EXT_PARAMS).is_some_and(|ext| unsafe {
            ext.value_to_text.map(|t| t(self.handle.plugin, id, plain, buf.as_mut_ptr(), buf.len() as u32)).unwrap_or(false)
        });
        if ok { unsafe { text(buf.as_ptr()) } } else { format!("{plain:.2}") }
    }

    pub fn state(&self) -> Vec<u8> {
        if self.crashed() {
            return self.kept.lock().unwrap().clone();
        }
        let Some(ext) = self.handle.ext::<clap_plugin_state>(CLAP_EXT_STATE) else { return Vec::new() };
        let mut data: Vec<u8> = Vec::new();
        unsafe extern "C" fn write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
            let v = unsafe { &mut *((*s).ctx as *mut Vec<u8>) };
            v.extend_from_slice(unsafe { std::slice::from_raw_parts(buf as *const u8, size as usize) });
            size as i64
        }
        let stream = clap_ostream { ctx: &mut data as *mut Vec<u8> as *mut c_void, write: Some(write) };
        let ok = unsafe { ext.save.map(|s| s(self.handle.plugin, &stream)).unwrap_or(false) };
        if !ok {
            return Vec::new();
        }
        *self.kept.lock().unwrap() = data.clone();
        data
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        self.clear_held();
        if self.crashed() {
            return false;
        }
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
        if ok {
            *self.kept.lock().unwrap() = data.to_vec();
        }
        self.read_values();
        // Loading isn't an edit, though some plug-ins say it is.
        self.handle.host_data.dirty.store(false, Relaxed);
        ok
    }

    /// Whether the plug-in changed its own state (from its interface, say) since last asked.
    pub fn take_dirty(&self) -> bool {
        self.handle.host_data.dirty.swap(false, Relaxed)
    }

    /// Parameters the user is holding in the plug-in's interface.
    pub fn held(&self) -> Vec<u32> {
        self.held.lock().unwrap().clone()
    }

    /// Lets go of every held parameter, for when the end of a gesture may never arrive.
    pub fn clear_held(&self) {
        self.held.lock().unwrap().clear();
    }

    /// Main-thread housekeeping: callbacks, timers and descriptors the plug-in asked for,
    /// gestures from its interface, and editor resizes. Call every frame.
    pub fn idle(&self) {
        self.idle_at(Instant::now());
    }

    fn idle_at(&self, now: Instant) {
        if self.crashed() {
            return;
        }
        let plugin = self.handle.plugin;
        let d = &self.handle.host_data;
        if d.callback.swap(false, Relaxed) {
            let p = unsafe { &*plugin };
            unsafe { p.on_main_thread.map(|f| f(plugin)) };
        }
        self.poll_fds();
        let due = d.timers.lock().unwrap().due(now);
        if !due.is_empty()
            && let Some(ext) = self.handle.ext::<clap_plugin_timer_support>(CLAP_EXT_TIMER_SUPPORT)
        {
            for id in due {
                // An earlier timer may have unregistered this one.
                if d.timers.lock().unwrap().contains(id) {
                    unsafe { ext.on_timer.map(|f| f(plugin, id)) };
                }
            }
        }
        {
            let mut q = self.gestures.lock().unwrap();
            let mut held = self.held.lock().unwrap();
            while let Ok(e) = q.pop() {
                if let Out::Gesture(id, begin) = e {
                    held.retain(|h| *h != id);
                    if begin {
                        held.push(id);
                    } else {
                        d.dirty.store(true, Relaxed);
                    }
                }
            }
        }
        if d.rescan_values.swap(false, Relaxed) {
            self.read_values();
        }
        // Without periods to report them in, edits in the interface have to be asked for.
        let engine_idle = {
            let blocks = self.handle.blocks.load(Relaxed);
            let mut seen = self.engine_seen.lock().unwrap();
            if seen.0 != blocks {
                *seen = (blocks, now);
            }
            now.duration_since(seen.1) >= ENGINE_IDLE
        };
        if engine_idle && self.editor_open() {
            self.poll_values();
        } else {
            self.polled.lock().unwrap().clear();
        }
        if d.gui_closed.load(Relaxed) != 0 {
            self.close_editor();
        }
        let show = d.visibility.swap(0, Relaxed);
        let floating = self.editor.lock().unwrap().as_ref().is_some_and(|e| e.window == 0);
        if show != 0
            && floating
            && let Some(gui) = self.gui_ext()
        {
            let f = if show == 1 { gui.show } else { gui.hide };
            unsafe { f.map(|f| f(plugin)) };
        }
        let r = d.resize.swap(0, Relaxed);
        if r != 0
            && let Some(e) = self.editor.lock().unwrap().as_mut().filter(|e| e.window != 0)
        {
            let (w, h) = (((r - 1) >> 32) as i32, ((r - 1) & 0xffff_ffff) as i32);
            solstice_vst3::x11::resize(e.window, w, h);
            e.size = (w.max(1), h.max(1));
        }
    }

    fn poll_fds(&self) {
        let fds = self.handle.host_data.fds.lock().unwrap().clone();
        if fds.is_empty() {
            return;
        }
        let Some(ext) = self.handle.ext::<clap_plugin_posix_fd_support>(CLAP_EXT_POSIX_FD_SUPPORT) else { return };
        let mut polled: Vec<libc::pollfd> = fds
            .iter()
            .map(|&(fd, flags)| {
                let mut events = 0;
                if flags & CLAP_POSIX_FD_READ != 0 {
                    events |= libc::POLLIN;
                }
                if flags & CLAP_POSIX_FD_WRITE != 0 {
                    events |= libc::POLLOUT;
                }
                libc::pollfd { fd, events, revents: 0 }
            })
            .collect();
        if unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, 0) } <= 0 {
            return;
        }
        for p in polled.iter().filter(|p| p.revents != 0) {
            let mut flags = 0;
            if p.revents & libc::POLLIN != 0 {
                flags |= CLAP_POSIX_FD_READ;
            }
            if p.revents & libc::POLLOUT != 0 {
                flags |= CLAP_POSIX_FD_WRITE;
            }
            if p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                flags |= CLAP_POSIX_FD_ERROR;
            }
            // An earlier descriptor's handler may have unregistered this one.
            if !self.handle.host_data.fds.lock().unwrap().iter().any(|f| f.0 == p.fd) {
                continue;
            }
            unsafe { ext.on_fd.map(|f| f(self.handle.plugin, p.fd, flags)) };
        }
    }

    fn gui_ext(&self) -> Option<&clap_plugin_gui> {
        if self.crashed() {
            return None;
        }
        self.handle.ext::<clap_plugin_gui>(CLAP_EXT_GUI)
    }

    /// Whether the plug-in has an X11 interface, embedded or floating.
    pub fn has_editor(&self) -> bool {
        self.gui.is_some() && !self.crashed()
    }

    /// Whether the interface can be shown inside another window.
    pub fn can_embed(&self) -> bool {
        self.gui == Some(false) && !self.crashed()
    }

    pub fn editor_open(&self) -> bool {
        self.editor.lock().unwrap().is_some()
    }

    /// Opens the interface in a window of its own, for plug-ins that can't be embedded.
    pub fn open_editor(&self, title: &str) -> bool {
        let mut editor = self.editor.lock().unwrap();
        let Some(gui) = self.gui_ext() else { return false };
        let plugin = self.handle.plugin;
        if editor.is_some() {
            unsafe { gui.show.map(|f| f(plugin)) };
            return true;
        }
        if self.gui != Some(true) {
            return false;
        }
        self.handle.host_data.gui_closed.store(0, Relaxed);
        if !unsafe { gui.create.is_some_and(|f| f(plugin, CLAP_WINDOW_API_X11.as_ptr(), true)) } {
            return false;
        }
        let title = CString::new(title).unwrap_or_default();
        unsafe { gui.suggest_title.map(|f| f(plugin, title.as_ptr())) };
        if !unsafe { gui.show.is_some_and(|f| f(plugin)) } {
            unsafe { gui.destroy.map(|f| f(plugin)) };
            return false;
        }
        *editor = Some(Editor { window: 0, size: (0, 0) });
        self.handle.host_data.floating.store(true, Relaxed);
        true
    }

    /// Opens the interface inside the X11 window `parent`, at a position in its pixels.
    pub fn open_editor_in(&self, parent: u64, x: i32, y: i32) -> bool {
        let mut editor = self.editor.lock().unwrap();
        if editor.is_some() {
            return true;
        }
        let Some(gui) = self.gui_ext().filter(|_| self.can_embed() && parent != 0) else { return false };
        let plugin = self.handle.plugin;
        let d = &self.handle.host_data;
        d.gui_closed.store(0, Relaxed);
        d.resize.store(0, Relaxed);
        if !unsafe { gui.create.is_some_and(|f| f(plugin, CLAP_WINDOW_API_X11.as_ptr(), false)) } {
            return false;
        }
        let (mut w, mut h) = (0u32, 0u32);
        if !unsafe { gui.get_size.is_some_and(|f| f(plugin, &mut w, &mut h)) } || w == 0 || h == 0 {
            (w, h) = (640, 400);
        }
        let size = (w.min(i32::MAX as u32) as i32, h.min(i32::MAX as u32) as i32);
        let window = solstice_vst3::x11::create_child(parent, x, y, size.0, size.1);
        let target = clap_window { api: CLAP_WINDOW_API_X11.as_ptr(), specific: clap_window_handle { x11: window as clap_xwnd } };
        let ok = window != 0 && unsafe { gui.set_parent.is_some_and(|f| f(plugin, &target)) };
        if !ok {
            unsafe { gui.destroy.map(|f| f(plugin)) };
            solstice_vst3::x11::destroy(window);
            return false;
        }
        unsafe { gui.show.map(|f| f(plugin)) };
        solstice_vst3::x11::map(window);
        *editor = Some(Editor { window, size });
        true
    }

    /// The embedded interface's size in pixels, which the plug-in may change while open.
    pub fn editor_size(&self) -> (i32, i32) {
        self.editor.lock().unwrap().as_ref().map_or((0, 0), |e| e.size)
    }

    pub fn move_editor(&self, x: i32, y: i32) {
        if let Some(e) = self.editor.lock().unwrap().as_ref().filter(|e| e.window != 0) {
            solstice_vst3::x11::move_to(e.window, x, y);
        }
    }

    pub fn close_editor(&self) {
        self.clear_held();
        let Some(e) = self.editor.lock().unwrap().take() else { return };
        let d = &self.handle.host_data;
        d.floating.store(false, Relaxed);
        d.visibility.store(0, Relaxed);
        let destroyed = d.gui_closed.swap(0, Relaxed) == 2;
        if let Some(gui) = self.gui_ext() {
            let plugin = self.handle.plugin;
            if !destroyed {
                unsafe { gui.hide.map(|f| f(plugin)) };
            }
            unsafe { gui.destroy.map(|f| f(plugin)) };
        }
        solstice_vst3::x11::destroy(e.window);
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.close_editor();
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

/// Where the plug-in's output events go, on the audio thread.
struct OutCtx {
    handle: Arc<Handle>,
    gestures: rtrb::Producer<Out>,
    values: Values,
    /// Parameter ids in `values` order.
    ids: Vec<u32>,
}

unsafe extern "C" fn out_push(list: *const clap_output_events, ev: *const clap_event_header) -> bool {
    if ev.is_null() {
        return false;
    }
    let out = unsafe { &mut *((*list).ctx as *mut OutCtx) };
    match decode(unsafe { &*ev }) {
        Some(Out::Value(id, v)) => {
            if let Some(i) = out.ids.iter().position(|x| *x == id) {
                // Plug-ins echo what the host sends them, and some report every value after a
                // load; only a change to a value already known is an edit.
                let old = f64::from_bits(out.values[i].swap(v.to_bits(), Relaxed));
                if (old - v).abs() > 1e-9 {
                    out.handle.host_data.dirty.store(true, Relaxed);
                }
            }
        }
        // A full queue drops the gesture; the next one for the parameter puts it right.
        Some(g) => {
            let _ = out.gestures.push(g);
        }
        None => {}
    }
    true
}

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header { size: std::mem::size_of::<T>() as u32, time, space_id: CLAP_CORE_EVENT_SPACE_ID, type_, flags: 0 }
}

/// The audio-thread side: turns each period into a `clap_process` call.
pub struct Processor {
    handle: Arc<Handle>,
    params: rtrb::Consumer<(u32, f64)>,
    out: Box<OutCtx>,
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
        out: OutCtx,
        ranges: Vec<(u32, f64, f64, bool)>,
        ports: (u32, u32),
        max_block: usize,
        sample_rate: f64,
    ) -> Self {
        Processor {
            handle,
            params,
            out: Box::new(out),
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
        if self.handle.dead.load(Relaxed) {
            return;
        }
        AUDIO.set(true);
        self.run(l, r, midi, ctx);
        AUDIO.set(false);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(i) = self.ranges.iter().position(|r| r.0 == id) {
            let (_, min, max, stepped) = self.ranges[i];
            let plain = min + value.clamp(0.0, 1.0) * (max - min);
            let plain = if stepped { plain.round() } else { plain };
            self.out.values[i].store(plain.to_bits(), Relaxed);
            self.push_param(id, plain);
        }
    }
}

impl Processor {
    fn run(&mut self, l: &mut [f32], r: &mut [f32], midi: &[MidiEvent], ctx: &ProcessContext) {
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
        let out_events = clap_output_events { ctx: &mut *self.out as *mut OutCtx as *mut c_void, try_push: Some(out_push) };
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
        self.handle.blocks.fetch_add(1, Relaxed);
        // A mono plug-in only writes the left channel.
        if self.ports.1 == 1 {
            r[..n].copy_from_slice(&l[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::{AllocDisabler, assert_no_alloc};

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    #[test]
    fn dead_instance_returns_kept_state() {
        let i = Instance::dead_for_test(b"kept".to_vec());
        assert!(i.crashed());
        assert_eq!(i.state(), b"kept");
        assert!(!i.set_state(b"new"));
        assert!(!i.has_editor());
        assert_eq!(i.param_text(1, 0.5), "");
        i.idle();
        i.close_editor();
        drop(i);
    }

    #[test]
    fn dead_processor_leaves_buffers_alone() {
        let i = Instance::dead_for_test(Vec::new());
        let mut p = i.processor(64);
        let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
        p.process(&mut l, &mut r, &[], &ProcessContext::default());
        assert!(l.iter().chain(&r).all(|&s| s == 0.5));
    }

    #[test]
    fn features_map_to_categories() {
        let f = |v: &[&str]| subcategories(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(f(&["audio-effect", "equalizer", "stereo"]), "Fx|EQ|Stereo");
        assert_eq!(f(&["instrument", "synthesizer"]), "Instrument|Synth");
    }

    fn param_event(id: u32, value: f64, key: i16) -> clap_event_param_value {
        let mut e: clap_event_param_value = unsafe { std::mem::zeroed() };
        e.header = header::<clap_event_param_value>(0, CLAP_EVENT_PARAM_VALUE);
        e.param_id = id;
        (e.note_id, e.port_index, e.channel, e.key) = (-1, -1, -1, key);
        e.value = value;
        e
    }

    fn gesture_event(id: u32, begin: bool) -> clap_event_param_gesture {
        let kind = if begin { CLAP_EVENT_PARAM_GESTURE_BEGIN } else { CLAP_EVENT_PARAM_GESTURE_END };
        clap_event_param_gesture { header: header::<clap_event_param_gesture>(0, kind), param_id: id }
    }

    #[test]
    fn decodes_output_events() {
        let v = param_event(3, 0.25, -1);
        assert_eq!(decode(&v.header), Some(Out::Value(3, 0.25)));
        assert_eq!(decode(&param_event(3, 0.25, 60).header), None, "per-note values are not the parameter's");
        assert_eq!(decode(&gesture_event(4, true).header), Some(Out::Gesture(4, true)));
        assert_eq!(decode(&gesture_event(4, false).header), Some(Out::Gesture(4, false)));
        let mut other = gesture_event(4, true);
        other.header.space_id = 7;
        assert_eq!(decode(&other.header), None);
    }

    #[test]
    fn offers_host_extensions() {
        for id in [
            CLAP_EXT_GUI,
            CLAP_EXT_TIMER_SUPPORT,
            CLAP_EXT_POSIX_FD_SUPPORT,
            CLAP_EXT_PARAMS,
            CLAP_EXT_STATE,
            CLAP_EXT_THREAD_CHECK,
            CLAP_EXT_LATENCY,
        ] {
            assert!(!unsafe { host_get_extension(std::ptr::null(), id.as_ptr()) }.is_null(), "{id:?}");
        }
        assert!(unsafe { host_get_extension(std::ptr::null(), c"clap.foo".as_ptr()) }.is_null());
    }

    #[test]
    fn timers_fire_on_their_period() {
        let start = Instant::now();
        let mut t = Timers::default();
        let a = t.register(Duration::from_millis(10), start);
        let b = t.register(Duration::from_millis(50), start);
        assert_ne!(a, b);
        assert!(t.due(start).is_empty());
        assert_eq!(t.due(start + Duration::from_millis(10)), vec![a]);
        assert!(t.due(start + Duration::from_millis(15)).is_empty());
        assert_eq!(t.due(start + Duration::from_millis(50)), vec![a, b]);
        assert!(t.unregister(a));
        assert!(!t.unregister(a));
        assert_eq!(t.due(start + Duration::from_millis(200)), vec![b]);
    }

    #[test]
    fn host_callbacks_reach_their_instance() {
        let data = HostData::new();
        let host = clap_host {
            clap_version: CLAP_VERSION,
            host_data: &data as *const HostData as *mut c_void,
            name: NAME.as_ptr(),
            vendor: VENDOR.as_ptr(),
            url: URL.as_ptr(),
            version: VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_request_process),
            request_callback: Some(host_request_callback),
        };
        unsafe {
            assert!(is_main_thread(&host));
            assert!(!is_audio_thread(&host));
            assert!(gui_request_resize(&host, 800, 600));
            let mut id = 0;
            assert!(timer_register(&host, 30, &mut id));
            assert!(fd_register(&host, 5, CLAP_POSIX_FD_READ));
            assert!(!fd_register(&host, 5, CLAP_POSIX_FD_READ));
            assert!(fd_modify(&host, 5, CLAP_POSIX_FD_WRITE));
            assert!(fd_unregister(&host, 5));
            assert!(timer_unregister(&host, id));
            params_rescan(&host, CLAP_PARAM_RESCAN_ALL);
            params_rescan(&host, CLAP_PARAM_RESCAN_TEXT);
            assert!(!data.dirty.load(Relaxed), "new texts aren't an edit");
            data.rescan_values.store(false, Relaxed);
            params_rescan(&host, CLAP_PARAM_RESCAN_VALUES);
            assert!(data.dirty.swap(false, Relaxed), "new values are an edit");
            state_mark_dirty(&host);
            assert!(!gui_request_show(&host), "an embedded editor can't be shown on request");
            data.floating.store(true, Relaxed);
            assert!(gui_request_hide(&host));
            assert_eq!(data.visibility.load(Relaxed), 2);
            assert!(gui_request_show(&host));
        }
        assert_eq!(data.visibility.load(Relaxed), 1);
        assert_eq!(data.resize.load(Relaxed), ((800u64 << 32) | 600) + 1);
        assert!(data.rescan_values.load(Relaxed));
        assert!(data.dirty.load(Relaxed));
        let addr = &host as *const clap_host as usize;
        let other = std::thread::spawn(move || unsafe { is_main_thread(addr as *const clap_host) }).join().unwrap();
        assert!(!other);
    }

    static FAKE_CALLS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    // A plug-in with parameters 7 and 9 (0..10) that sets 7 to 2.5 in every `process`. The
    // first time it does so from its interface, wrapped in a gesture as a dragged knob is,
    // and starts holding 9.
    unsafe extern "C" fn fake_process(_: *const clap_plugin, process: *const clap_process) -> i32 {
        let out = unsafe { &*(*process).out_events };
        let push = out.try_push.unwrap();
        let value = param_event(7, 2.5, -1);
        if FAKE_CALLS.fetch_add(1, Relaxed) == 0 {
            let (begin, end, held) = (gesture_event(7, true), gesture_event(7, false), gesture_event(9, true));
            unsafe {
                push(out, &begin.header);
                push(out, &value.header);
                push(out, &end.header);
                push(out, &held.header);
            }
        } else {
            unsafe { push(out, &value.header) };
        }
        1
    }
    unsafe extern "C" fn fake_count(_: *const clap_plugin) -> u32 {
        2
    }
    unsafe extern "C" fn fake_info(_: *const clap_plugin, index: u32, info: *mut clap_param_info) -> bool {
        let info = unsafe { &mut *info };
        info.id = if index == 0 { 7 } else { 9 };
        (info.min_value, info.max_value, info.default_value) = (0.0, 10.0, 1.0);
        true
    }
    unsafe extern "C" fn fake_value(_: *const clap_plugin, _: clap_id, v: *mut f64) -> bool {
        unsafe { *v = 1.0 };
        true
    }
    static FAKE_PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(fake_count),
        get_info: Some(fake_info),
        get_value: Some(fake_value),
        value_to_text: None,
        text_to_value: None,
        flush: None,
    };
    unsafe extern "C" fn fake_extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
        if unsafe { CStr::from_ptr(id) } == CLAP_EXT_PARAMS { &FAKE_PARAMS as *const _ as *const c_void } else { std::ptr::null() }
    }

    fn fake_instance(plugin: clap_plugin) -> Instance {
        let plugin: &'static clap_plugin = Box::leak(Box::new(plugin));
        let host_data = Box::new(HostData::new());
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
        let handle = Arc::new(Handle {
            plugin,
            _host: host,
            host_data,
            processing: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            dead: Arc::default(),
        });
        let info = ClassInfo { uid: "clap:fake".into(), name: "Fake".into(), vendor: String::new(), subcategories: "Fx".into(), path: PathBuf::new() };
        Instance::new(handle, info, 48000.0)
    }

    #[test]
    fn reads_back_edits_from_the_plugins_interface() {
        let mut plugin: clap_plugin = unsafe { std::mem::zeroed() };
        plugin.process = Some(fake_process);
        plugin.get_extension = Some(fake_extension);
        let inst = fake_instance(plugin);
        assert!(!inst.has_editor());
        assert_eq!(inst.param(7), 0.1, "unknown values come from the plug-in");

        let mut proc = inst.processor(64);
        proc.set_param(9, 0.5);
        let ctx = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        assert_no_alloc(|| proc.process(&mut l, &mut r, &[], &ctx));
        assert!(!AUDIO.get());

        assert_eq!(inst.param(7), 0.25);
        assert_eq!(inst.param(9), 0.5, "automation is read back without the plug-in");
        inst.idle();
        assert_eq!(inst.held(), vec![9]);
        assert!(inst.take_dirty());
        assert!(!inst.take_dirty());

        // Echoes of what the host sent don't count as edits; the plug-in overruling it does.
        inst.set_param(7, 0.25);
        assert_no_alloc(|| proc.process(&mut l, &mut r, &[], &ctx));
        assert!(!inst.take_dirty());
        inst.set_param(7, 0.5);
        assert_no_alloc(|| proc.process(&mut l, &mut r, &[], &ctx));
        assert!(inst.take_dirty());
        assert_eq!(inst.param(7), 0.25);

        inst.handle.host_data.rescan_values.store(true, Relaxed);
        inst.idle();
        assert_eq!(inst.param(7), 0.1);

        // A gesture whose end never comes is let go when the editor closes.
        assert_eq!(inst.held(), vec![9]);
        inst.close_editor();
        assert!(inst.held().is_empty());
    }

    // A plug-in with one parameter, 3 (0..1), whose value is QUIET_VALUE and which reports
    // QUIET_REPORT from `process` without a gesture, as a menu or typed-in value would.
    static QUIET_VALUE: AtomicU64 = AtomicU64::new(0);
    static QUIET_REPORT: AtomicU64 = AtomicU64::new(0);
    static QUIET_PARAMS: clap_plugin_params = clap_plugin_params {
        count: Some(quiet_count),
        get_info: Some(quiet_info),
        get_value: Some(quiet_value),
        value_to_text: None,
        text_to_value: None,
        flush: None,
    };
    static QUIET_STATE: clap_plugin_state = clap_plugin_state { save: None, load: Some(quiet_load) };
    unsafe extern "C" fn quiet_count(_: *const clap_plugin) -> u32 {
        1
    }
    unsafe extern "C" fn quiet_info(_: *const clap_plugin, _: u32, info: *mut clap_param_info) -> bool {
        let info = unsafe { &mut *info };
        (info.id, info.min_value, info.max_value) = (3, 0.0, 1.0);
        true
    }
    unsafe extern "C" fn quiet_value(_: *const clap_plugin, _: clap_id, v: *mut f64) -> bool {
        unsafe { *v = f64::from_bits(QUIET_VALUE.load(Relaxed)) };
        true
    }
    unsafe extern "C" fn quiet_load(_: *const clap_plugin, _: *const clap_istream) -> bool {
        true
    }
    unsafe extern "C" fn quiet_process(_: *const clap_plugin, process: *const clap_process) -> i32 {
        let out = unsafe { &*(*process).out_events };
        let value = param_event(3, f64::from_bits(QUIET_REPORT.load(Relaxed)), -1);
        unsafe { out.try_push.unwrap()(out, &value.header) };
        1
    }
    unsafe extern "C" fn quiet_extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
        match unsafe { CStr::from_ptr(id) } {
            id if id == CLAP_EXT_PARAMS => &QUIET_PARAMS as *const _ as *const c_void,
            id if id == CLAP_EXT_STATE => &QUIET_STATE as *const _ as *const c_void,
            _ => std::ptr::null(),
        }
    }

    #[test]
    fn edits_without_gestures_mark_the_state_dirty() {
        let set = |a: &AtomicU64, v: f64| a.store(v.to_bits(), Relaxed);
        set(&QUIET_VALUE, 0.2);
        set(&QUIET_REPORT, 0.2);
        let mut plugin: clap_plugin = unsafe { std::mem::zeroed() };
        plugin.process = Some(quiet_process);
        plugin.get_extension = Some(quiet_extension);
        let inst = fake_instance(plugin);
        let mut proc = inst.processor(64);
        let ctx = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: false };
        let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
        let mut run = || assert_no_alloc(|| proc.process(&mut l, &mut r, &[], &ctx));

        set(&QUIET_REPORT, 0.7);
        run();
        assert!(inst.take_dirty(), "the first change reported is an edit");
        run();
        assert!(!inst.take_dirty(), "reporting the value it has isn't");

        // A load leaves values known, so what the plug-in reports next is measured against them.
        set(&QUIET_VALUE, 0.4);
        set(&QUIET_REPORT, 0.4);
        assert!(inst.set_state(&[]));
        run();
        assert!(!inst.take_dirty());
        assert!((inst.param(3) - 0.4).abs() < 1e-9);
        assert!(inst.set_state(&[]));
        set(&QUIET_REPORT, 0.5);
        run();
        assert!(inst.take_dirty());

        // With the engine stopped, an open editor's edits are read back from the plug-in.
        *inst.editor.lock().unwrap() = Some(Editor { window: 0, size: (0, 0) });
        let stopped = Instant::now();
        inst.idle_at(stopped);
        let start = stopped + ENGINE_IDLE;
        inst.idle_at(start);
        inst.set_param(3, 0.9);
        inst.idle_at(start + Duration::from_millis(20));
        assert!(!inst.take_dirty());
        assert_eq!(inst.param(3), 0.9, "what the host set waits for the engine");
        set(&QUIET_VALUE, 0.6);
        inst.idle_at(start + Duration::from_millis(40));
        assert!(inst.take_dirty());
        assert!((inst.param(3) - 0.6).abs() < 1e-9);
        set(&QUIET_REPORT, 0.6);
        run();
        set(&QUIET_VALUE, 0.1);
        inst.idle_at(start + Duration::from_millis(60));
        assert!(!inst.take_dirty(), "while the engine runs, edits come from it");
        inst.close_editor();
    }

    static FD_PIPES: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    static FD_HOST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static FD_CALLS: Mutex<Vec<i32>> = Mutex::new(Vec::new());
    static FAKE_FD: clap_plugin_posix_fd_support = clap_plugin_posix_fd_support { on_fd: Some(fake_on_fd) };
    // Handling the first descriptor unregisters the other.
    unsafe extern "C" fn fake_on_fd(_: *const clap_plugin, fd: i32, _: u32) {
        FD_CALLS.lock().unwrap().push(fd);
        let host = FD_HOST.load(Relaxed) as *const clap_host;
        for other in FD_PIPES.lock().unwrap().iter().filter(|f| **f != fd) {
            unsafe { fd_unregister(host, *other) };
        }
    }
    unsafe extern "C" fn fd_extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
        if unsafe { CStr::from_ptr(id) } == CLAP_EXT_POSIX_FD_SUPPORT { &FAKE_FD as *const _ as *const c_void } else { std::ptr::null() }
    }

    #[test]
    fn descriptors_unregistered_while_polling_are_skipped() {
        let mut plugin: clap_plugin = unsafe { std::mem::zeroed() };
        plugin.get_extension = Some(fd_extension);
        let inst = fake_instance(plugin);
        FD_HOST.store(&*inst.handle._host as *const clap_host as usize, Relaxed);
        let mut ends = Vec::new();
        for _ in 0..2 {
            let mut p = [0i32; 2];
            assert_eq!(unsafe { libc::pipe(p.as_mut_ptr()) }, 0);
            assert_eq!(unsafe { libc::write(p[1], b"x".as_ptr() as *const c_void, 1) }, 1);
            assert!(unsafe { fd_register(&*inst.handle._host, p[0], CLAP_POSIX_FD_READ) });
            FD_PIPES.lock().unwrap().push(p[0]);
            ends.extend(p);
        }
        inst.idle();
        assert_eq!(*FD_CALLS.lock().unwrap(), vec![ends[0]]);
        for fd in ends {
            unsafe { libc::close(fd) };
        }
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
        assert!(inst.has_editor());
        assert!(!inst.editor_open());
        let first = inst.params()[0].id;
        inst.set_param(first, 0.5);
        let smooth = inst.params().into_iter().find(|p| p.steps == 0 && !p.read_only).unwrap().id;
        inst.set_param(smooth, 0.3);
        assert!((inst.param(smooth) - 0.3).abs() < 1e-6, "a value set is read back at once");
        inst.idle();
        let mut proc = inst.processor(512);
        let ctx = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };
        for _ in 0..8 {
            let (mut l, mut r) = (vec![0.25f32; 512], vec![0.25f32; 512]);
            proc.process(&mut l, &mut r, &[], &ctx);
            assert!(l.iter().chain(&r).all(|v| v.is_finite()));
            inst.idle();
        }
        let state = inst.state();
        assert!(!state.is_empty());
        assert!(inst.set_state(&state));
        drop(proc);
    }

    /// Opens LSP's interface inside a window of ours. Needs an X server, so run it by hand,
    /// for instance under xvfb-run with a private XDG_RUNTIME_DIR.
    #[test]
    #[ignore]
    fn embeds_an_installed_plugins_interface() {
        let path = find_modules(&default_paths()).into_iter().find(|p| p.ends_with("lsp-plugins.clap")).expect("LSP's CLAP bundle");
        let classes = scan(&path).unwrap();
        let info = classes.iter().find(|c| c.uid.contains("para_equalizer_x8_stereo")).unwrap_or(&classes[0]).clone();
        let inst = Instance::open(&info, 48000.0, 512).unwrap();
        assert!(inst.can_embed());
        let parent = solstice_vst3::x11::create_window(800, 600, c"Solstice");
        assert_ne!(parent, 0, "no X server");
        assert!(inst.open_editor_in(parent, 10, 20));
        assert!(inst.editor_open());
        let (w, h) = inst.editor_size();
        assert!(w > 0 && h > 0);
        for _ in 0..20 {
            inst.idle();
            std::thread::sleep(Duration::from_millis(10));
        }
        inst.move_editor(0, 0);
        inst.close_editor();
        assert!(!inst.editor_open());
        solstice_vst3::x11::destroy(parent);
    }
}
