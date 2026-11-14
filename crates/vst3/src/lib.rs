use serde::{Deserialize, Serialize};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

pub mod hostwatch;

#[repr(C)]
struct SvPlugin {
    _private: [u8; 0],
}

#[repr(C)]
struct SvMidi {
    frame: u32,
    data: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SvParam {
    id: u32,
    value: f64,
}

#[repr(C)]
struct SvContext {
    position: i64,
    tempo: f64,
    sig_num: i32,
    sig_den: i32,
    playing: i32,
}

#[repr(C)]
struct SvParamInfo {
    id: u32,
    title: [c_char; 128],
    units: [c_char; 32],
    default_value: f64,
    steps: i32,
    flags: i32,
}

type ScanCallback = extern "C" fn(*mut c_void, *const c_char, *const c_char, *const c_char, *const c_char);

unsafe extern "C" {
    fn sv_scan(path: *const c_char, cb: ScanCallback, ctx: *mut c_void, err: *mut c_char, len: usize) -> i32;
    fn sv_open(path: *const c_char, uid: *const c_char, sr: f64, max_block: i32, err: *mut c_char, len: usize)
    -> *mut SvPlugin;
    fn sv_close(p: *mut SvPlugin);
    fn sv_process(
        p: *mut SvPlugin,
        inp: *const *const f32,
        out: *const *mut f32,
        frames: i32,
        events: *const SvMidi,
        n: i32,
        params: *const SvParam,
        n_params: i32,
        ctx: *const SvContext,
    ) -> i32;
    fn sv_sidechain(p: *mut SvPlugin, key: *const *const f32, frames: i32);
    fn sv_has_sidechain(p: *mut SvPlugin) -> i32;
    fn sv_set_sidechain_active(p: *mut SvPlugin, on: i32) -> i32;
    fn sv_latency(p: *mut SvPlugin) -> u32;
    fn sv_has_input(p: *mut SvPlugin) -> i32;
    fn sv_get_state(p: *mut SvPlugin, data: *mut *const u8, len: *mut usize) -> i32;
    fn sv_set_state(p: *mut SvPlugin, data: *const u8, len: usize) -> i32;
    fn sv_param_count(p: *mut SvPlugin) -> i32;
    fn sv_param_info(p: *mut SvPlugin, index: i32, out: *mut SvParamInfo) -> i32;
    fn sv_param_get(p: *mut SvPlugin, id: u32) -> f64;
    fn sv_param_set(p: *mut SvPlugin, id: u32, value: f64);
    fn sv_param_string(p: *mut SvPlugin, id: u32, value: f64, out: *mut c_char, len: usize);
    fn sv_has_editor(p: *mut SvPlugin) -> i32;
    fn sv_open_editor(p: *mut SvPlugin, title: *const c_char) -> i32;
    fn sv_open_editor_in(p: *mut SvPlugin, parent: std::ffi::c_ulong, x: i32, y: i32) -> i32;
    fn sv_editor_size(p: *mut SvPlugin, w: *mut i32, h: *mut i32);
    fn sv_move_editor(p: *mut SvPlugin, x: i32, y: i32);
    fn sv_close_editor(p: *mut SvPlugin);
    fn sv_editor_open(p: *mut SvPlugin) -> i32;
    fn sv_editor_window(p: *mut SvPlugin) -> std::ffi::c_ulong;
    fn sv_idle();
    fn sv_abandon(p: *mut SvPlugin);
}

/// Open instances, so `idle` can take a dead one out of the shim's housekeeping before it
/// calls into it.
static OPEN: Mutex<Vec<(usize, Arc<AtomicBool>, PathBuf)>> = Mutex::new(Vec::new());
static BRIDGES: Mutex<Option<hostwatch::Bridges>> = Mutex::new(None);

const MAX_EVENTS: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClassInfo {
    pub uid: String,
    pub name: String,
    pub vendor: String,
    pub subcategories: String,
    pub path: PathBuf,
}

impl ClassInfo {
    pub fn is_instrument(&self) -> bool {
        self.subcategories.split('|').any(|c| c == "Instrument")
    }
}

fn cstr(p: &Path) -> CString {
    CString::new(p.as_os_str().as_encoded_bytes()).unwrap_or_default()
}

fn err_string(buf: &[c_char]) -> String {
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

/// Loads a module and lists its audio processor classes. Run this in `solstice-scan`, never in
/// the app, since a broken plug-in can crash the process that loads it.
pub fn scan(path: &Path) -> Result<Vec<ClassInfo>, String> {
    extern "C" fn cb(ctx: *mut c_void, uid: *const c_char, name: *const c_char, vendor: *const c_char, sub: *const c_char) {
        let (out, path) = unsafe { &mut *(ctx as *mut (Vec<ClassInfo>, PathBuf)) };
        let s = |p| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        out.push(ClassInfo { uid: s(uid), name: s(name), vendor: s(vendor), subcategories: s(sub), path: path.clone() });
    }
    let mut ctx = (Vec::new(), path.to_path_buf());
    let mut err = [0 as c_char; 512];
    let n = unsafe { sv_scan(cstr(path).as_ptr(), cb, &mut ctx as *mut _ as *mut c_void, err.as_mut_ptr(), err.len()) };
    if n < 0 { Err(err_string(&err)) } else { Ok(ctx.0) }
}

pub fn default_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join(".vst3"));
    }
    v.extend(["/usr/lib64/vst3", "/usr/lib/vst3", "/usr/local/lib/vst3"].map(PathBuf::from));
    v
}

/// Finds `.vst3` bundles (or bare modules) under the given folders.
pub fn find_modules(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "vst3") {
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

pub struct Instance {
    ptr: *mut SvPlugin,
    pub info: ClassInfo,
    max_block: usize,
    sidechain: bool,
    /// Set by `hostwatch` when the yabridge host serving this plug-in exits.
    dead: Arc<AtomicBool>,
    bridged: bool,
    /// The last state read from or given to the plug-in, returned once it has died.
    kept: Mutex<Vec<u8>>,
    /// Where the module was loaded from: `info.path`, or a copy once that path's host died.
    path: PathBuf,
}

// The shim confines processor calls to the RT thread and controller calls to the main thread.
unsafe impl Send for Instance {}
unsafe impl Sync for Instance {}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub title: String,
    pub units: String,
    pub default_value: f64,
    pub steps: i32,
    pub read_only: bool,
    pub hidden: bool,
}

impl Instance {
    pub fn open(info: &ClassInfo, sample_rate: f64, max_block: usize) -> Result<Arc<Instance>, String> {
        let uid = CString::new(info.uid.clone()).map_err(|e| e.to_string())?;
        let mut err = [0 as c_char; 512];
        let path = BRIDGES
            .lock()
            .unwrap()
            .get_or_insert_default()
            .load_path(&info.path, &hostwatch::copies_dir())
            .map_err(|e| format!("its Windows host crashed and it could not be loaded again: {e}"))?;
        let (ptr, hosts) = hostwatch::hosts_for(&path, || unsafe {
            sv_open(cstr(&path).as_ptr(), uid.as_ptr(), sample_rate, max_block as i32, err.as_mut_ptr(), err.len())
        });
        if ptr.is_null() {
            return Err(err_string(&err));
        }
        let sidechain = unsafe { sv_has_sidechain(ptr) != 0 };
        let dead = Arc::new(AtomicBool::new(false));
        for &pid in &hosts {
            hostwatch::watch(pid, dead.clone());
        }
        let bridged = !hosts.is_empty();
        if bridged {
            OPEN.lock().unwrap().push((ptr as usize, dead.clone(), path.clone()));
        }
        Ok(Arc::new(Instance { ptr, info: info.clone(), max_block, sidechain, dead, bridged, kept: Mutex::default(), path }))
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn dead_for_test(kept: Vec<u8>) -> Arc<Instance> {
        Arc::new(Instance {
            ptr: std::ptr::null_mut(),
            info: ClassInfo { uid: String::new(), name: String::new(), vendor: String::new(), subcategories: String::new(), path: PathBuf::new() },
            max_block: 64,
            sidechain: false,
            dead: Arc::new(AtomicBool::new(true)),
            bridged: true,
            kept: Mutex::new(kept),
            path: PathBuf::from("/test/Dead.vst3"),
        })
    }

    pub fn crashed(&self) -> bool {
        self.dead.load(Relaxed)
    }

    /// Whether a yabridge host was found for this instance, so a crash would be noticed.
    pub fn bridged(&self) -> bool {
        self.bridged
    }

    fn live(&self) -> Option<*mut SvPlugin> {
        (!self.crashed()).then_some(self.ptr)
    }

    pub fn latency(&self) -> u32 {
        self.live().map_or(0, |p| unsafe { sv_latency(p) })
    }

    pub fn has_input(&self) -> bool {
        self.live().is_some_and(|p| unsafe { sv_has_input(p) != 0 })
    }

    /// Whether the plug-in has an auxiliary audio input to key.
    pub fn has_sidechain(&self) -> bool {
        self.sidechain
    }

    /// Switches the side-chain input on or off, briefly suspending processing; returns whether
    /// it changed. Call from the UI thread only. The latency may differ afterwards.
    pub fn set_sidechain_active(&self, on: bool) -> bool {
        self.live().is_some_and(|p| unsafe { sv_set_sidechain_active(p, on as i32) != 0 })
    }

    pub fn state(&self) -> Vec<u8> {
        let Some(p) = self.live() else { return self.kept.lock().unwrap().clone() };
        let mut data = std::ptr::null();
        let mut len = 0;
        // A failed read is not an empty plug-in: saving nothing would wipe its settings.
        if unsafe { sv_get_state(p, &mut data, &mut len) } == 0 || data.is_null() {
            return self.kept.lock().unwrap().clone();
        }
        let state = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        *self.kept.lock().unwrap() = state.clone();
        state
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        let Some(p) = self.live() else { return false };
        let ok = !data.is_empty() && unsafe { sv_set_state(p, data.as_ptr(), data.len()) != 0 };
        if ok {
            *self.kept.lock().unwrap() = data.to_vec();
        }
        ok
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let Some(ptr) = self.live() else { return Vec::new() };
        let n = unsafe { sv_param_count(ptr) };
        (0..n)
            .filter_map(|i| {
                let mut raw: SvParamInfo = unsafe { std::mem::zeroed() };
                if unsafe { sv_param_info(ptr, i, &mut raw) } == 0 {
                    return None;
                }
                Some(ParamInfo {
                    id: raw.id,
                    title: err_string(&raw.title),
                    units: err_string(&raw.units),
                    default_value: raw.default_value,
                    steps: raw.steps,
                    // ParameterInfo::kIsReadOnly and kIsHidden.
                    read_only: raw.flags & (1 << 1) != 0,
                    hidden: raw.flags & (1 << 4) != 0,
                })
            })
            .collect()
    }

    pub fn param(&self, id: u32) -> f64 {
        self.live().map_or(0.0, |p| unsafe { sv_param_get(p, id) })
    }

    pub fn set_param(&self, id: u32, value: f64) {
        if let Some(p) = self.live() {
            unsafe { sv_param_set(p, id, value) }
        }
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        let Some(p) = self.live() else { return String::new() };
        let mut buf = [0 as c_char; 128];
        unsafe { sv_param_string(p, id, value, buf.as_mut_ptr(), buf.len()) };
        err_string(&buf)
    }

    pub fn has_editor(&self) -> bool {
        self.live().is_some_and(|p| unsafe { sv_has_editor(p) != 0 })
    }

    pub fn open_editor(&self, title: &str) -> bool {
        let t = CString::new(title).unwrap_or_default();
        self.live().is_some_and(|p| unsafe { sv_open_editor(p, t.as_ptr()) != 0 })
    }

    /// Opens the editor inside an X11 window, at a position in that window's pixels.
    pub fn open_editor_in(&self, parent: u64, x: i32, y: i32) -> bool {
        self.live().is_some_and(|p| unsafe { sv_open_editor_in(p, parent as std::ffi::c_ulong, x, y) != 0 })
    }

    /// The editor's size in pixels, which the plug-in may change while open.
    pub fn editor_size(&self) -> (i32, i32) {
        let (mut w, mut h) = (0, 0);
        if let Some(p) = self.live() {
            unsafe { sv_editor_size(p, &mut w, &mut h) };
        }
        (w, h)
    }

    pub fn move_editor(&self, x: i32, y: i32) {
        if let Some(p) = self.live() {
            unsafe { sv_move_editor(p, x, y) }
        }
    }

    pub fn close_editor(&self) {
        if let Some(p) = self.live() {
            unsafe { sv_close_editor(p) }
        }
    }

    pub fn editor_open(&self) -> bool {
        self.live().is_some_and(|p| unsafe { sv_editor_open(p) != 0 })
    }

    /// The X11 window the editor is embedded in, 0 when there is none.
    pub fn editor_window(&self) -> u64 {
        self.live().map_or(0, |p| unsafe { sv_editor_window(p) } as u64)
    }

    /// The RT-side processor for the engine.
    pub fn processor(self: &Arc<Self>) -> Box<dyn Plugin> {
        Box::new(Processor {
            inst: self.clone(),
            input: [vec![0.0; self.max_block], vec![0.0; self.max_block]],
            events: Vec::with_capacity(MAX_EVENTS),
            params: Vec::with_capacity(MAX_PARAMS),
        })
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.bridged && !self.ptr.is_null() {
            OPEN.lock().unwrap().retain(|(p, ..)| *p != self.ptr as usize);
        }
        // Dropped before `idle` saw the crash: forget it in the shim here instead.
        if self.crashed() {
            if !self.ptr.is_null() {
                unsafe { sv_abandon(self.ptr) };
            }
            BRIDGES.lock().unwrap().get_or_insert_default().died(&self.path);
        }
        // Closing a plug-in whose host has died would wait for that host forever.
        if let Some(p) = self.live() {
            unsafe { sv_close(p) }
        }
    }
}

/// Main-thread housekeeping for every open plug-in: run loop, editor windows, parameter echoes.
pub fn idle() {
    OPEN.lock().unwrap().retain(|(p, dead, path)| {
        let gone = dead.load(Relaxed);
        if gone {
            unsafe { sv_abandon(*p as *mut SvPlugin) };
            BRIDGES.lock().unwrap().get_or_insert_default().died(path);
        }
        !gone
    });
    unsafe { sv_idle() }
}

/// Child windows on the shim's X11 connection, for CLAP editors shown inside Solstice.
pub mod x11 {
    use std::ffi::{CStr, c_char, c_ulong};

    unsafe extern "C" {
        fn x11_create_window(width: i32, height: i32, title: *const c_char) -> c_ulong;
        fn x11_create_child(parent: c_ulong, x: i32, y: i32, width: i32, height: i32) -> c_ulong;
        fn x11_move(w: c_ulong, x: i32, y: i32);
        fn x11_resize_child(w: c_ulong, width: i32, height: i32);
        fn x11_map(w: c_ulong);
        fn x11_destroy(w: c_ulong);
        fn x11_geometry(w: c_ulong, x: *mut i32, y: *mut i32, width: *mut i32, height: *mut i32) -> i32;
        fn x11_announce(w: c_ulong, x: i32, y: i32, width: i32, height: i32);
    }

    /// A top-level window; 0 when there is no X server.
    pub fn create_window(width: i32, height: i32, title: &CStr) -> u64 {
        unsafe { x11_create_window(width.max(1), height.max(1), title.as_ptr()) as u64 }
    }

    /// 0 when there is no X server.
    pub fn create_child(parent: u64, x: i32, y: i32, width: i32, height: i32) -> u64 {
        unsafe { x11_create_child(parent as c_ulong, x, y, width.max(1), height.max(1)) as u64 }
    }

    pub fn move_to(w: u64, x: i32, y: i32) {
        unsafe { x11_move(w as c_ulong, x, y) }
    }

    pub fn resize(w: u64, width: i32, height: i32) {
        unsafe { x11_resize_child(w as c_ulong, width.max(1), height.max(1)) }
    }

    pub fn map(w: u64) {
        unsafe { x11_map(w as c_ulong) }
    }

    pub fn destroy(w: u64) {
        unsafe { x11_destroy(w as c_ulong) }
    }

    /// The window's position on the root window and its size.
    pub fn geometry(w: u64) -> Option<(i32, i32, i32, i32)> {
        let (mut x, mut y, mut width, mut height) = (0, 0, 0, 0);
        (unsafe { x11_geometry(w as c_ulong, &mut x, &mut y, &mut width, &mut height) } != 0).then_some((x, y, width, height))
    }

    /// Tells the window's clients its position on screen, as a window manager would.
    pub fn announce(w: u64, (x, y, width, height): (i32, i32, i32, i32)) {
        unsafe { x11_announce(w as c_ulong, x, y, width, height) }
    }
}

const MAX_PARAMS: usize = 256;

struct Processor {
    inst: Arc<Instance>,
    input: [Vec<f32>; 2],
    events: Vec<SvMidi>,
    params: Vec<SvParam>,
}

impl Plugin for Processor {
    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(p) = self.params.iter_mut().find(|p| p.id == id) {
            p.value = value;
        } else if self.params.len() < self.params.capacity() {
            self.params.push(SvParam { id, value });
        }
    }

    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        if self.inst.crashed() {
            return;
        }
        let Some([l, r]) = key else {
            return unsafe { sv_sidechain(self.inst.ptr, std::ptr::null(), 0) };
        };
        // The shim copies the key into the plug-in's own bus buffers before returning.
        let ptrs = [l.as_ptr(), r.as_ptr()];
        unsafe { sv_sidechain(self.inst.ptr, ptrs.as_ptr(), l.len().min(r.len()) as i32) }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[MidiEvent], ctx: &ProcessContext) {
        if self.inst.crashed() {
            return;
        }
        let n = left.len().min(self.inst.max_block);
        self.input[0][..n].copy_from_slice(&left[..n]);
        self.input[1][..n].copy_from_slice(&right[..n]);
        self.events.clear();
        for e in events.iter().take(MAX_EVENTS) {
            self.events.push(SvMidi { frame: e.frame, data: e.data });
        }
        let ins = [self.input[0].as_ptr(), self.input[1].as_ptr()];
        let outs = [left.as_mut_ptr(), right.as_mut_ptr()];
        let cx = SvContext {
            position: ctx.position as i64,
            tempo: ctx.tempo,
            sig_num: ctx.sig_num as i32,
            sig_den: ctx.sig_den as i32,
            playing: ctx.playing as i32,
        };
        let ran = unsafe {
            sv_process(
                self.inst.ptr,
                ins.as_ptr(),
                outs.as_ptr(),
                n as i32,
                self.events.as_ptr(),
                self.events.len() as i32,
                self.params.as_ptr(),
                self.params.len() as i32,
                &cx,
            )
        };
        // A block passed through while the side-chain switched; the values go with the next one.
        if ran != 0 {
            self.params.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solstice_engine::ProcessContext;

    #[test]
    fn dead_instance_returns_kept_state() {
        let i = Instance::dead_for_test(b"kept".to_vec());
        assert!(i.crashed());
        assert_eq!(i.state(), b"kept");
        assert_eq!(i.latency(), 0);
        assert!(!i.has_editor());
        assert!(i.params().is_empty());
        assert_eq!(i.editor_size(), (0, 0));
        i.close_editor();
        i.set_param(1, 0.5);
        assert!(!i.set_state(b"new"));
    }

    #[test]
    fn dead_processor_leaves_buffers_alone() {
        let i = Instance::dead_for_test(Vec::new());
        let mut p = i.processor();
        let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
        p.process(&mut l, &mut r, &[], &ProcessContext::default());
        assert!(l.iter().chain(&r).all(|&s| s == 0.5));
    }

    #[test]
    fn idle_forgets_dead_instances() {
        let dead = Arc::new(AtomicBool::new(false));
        OPEN.lock().unwrap().push((0, dead.clone(), PathBuf::from("/x/Gone.vst3")));
        idle();
        assert!(OPEN.lock().unwrap().iter().any(|(_, d, _)| Arc::ptr_eq(d, &dead)));
        dead.store(true, Relaxed);
        idle();
        assert!(!OPEN.lock().unwrap().iter().any(|(_, d, _)| Arc::ptr_eq(d, &dead)));
    }

    /// Opens a real bridged plug-in, kills its yabridge host, checks nothing then waits on it,
    /// and opens it again. Run with
    /// `SOLSTICE_BRIDGED_VST3=<path to a yabridge .vst3> cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn a_killed_yabridge_host_is_survived() {
        use std::time::{Duration, Instant};
        let Some(path) = std::env::var_os("SOLSTICE_BRIDGED_VST3") else { return };
        let info = scan(Path::new(&path)).unwrap().remove(0);
        let inst = Instance::open(&info, 48000.0, 512).unwrap();
        assert!(inst.bridged());
        let before = inst.state();
        let host = hostwatch::serving(&info.path, &hostwatch::children());
        unsafe { libc::kill(host[0], libc::SIGKILL) };
        let start = Instant::now();
        while !inst.crashed() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(inst.crashed());
        idle();
        assert_eq!(inst.state(), before);
        inst.close_editor();
        let mut p = inst.processor();
        let (mut l, mut r) = (vec![0.5; 64], vec![0.5; 64]);
        p.process(&mut l, &mut r, &[], &ProcessContext::default());
        drop(p);
        drop(inst);
        let again = Instance::open(&info, 48000.0, 512).unwrap();
        assert!(again.bridged() && !again.crashed());
        assert!(again.set_state(&before));
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    /// Embeds a real bridged editor in a window, moves the window, and checks the position
    /// yabridge last gave Wine against where the editor really is. Run with
    /// `SOLSTICE_BRIDGED_VST3=<path to a yabridge .vst3> cargo test -- --ignored --test-threads 1`.
    #[test]
    #[ignore]
    fn a_bridged_editor_knows_where_it_is() {
        use std::time::{Duration, Instant};
        let Some(path) = std::env::var_os("SOLSTICE_BRIDGED_VST3") else { return };
        let log = std::env::temp_dir().join(format!("solstice-yabridge-{}.log", std::process::id()));
        unsafe {
            std::env::set_var("YABRIDGE_DEBUG_LEVEL", "1+editor");
            std::env::set_var("YABRIDGE_DEBUG_FILE", &log);
        }
        let info = scan(Path::new(&path)).unwrap().remove(0);
        let inst = Instance::open(&info, 48000.0, 512).unwrap();
        let top = x11::create_window(900, 700, c"Solstice editor test");
        x11::map(top);
        let pump = |secs: f32| {
            let until = Instant::now() + Duration::from_secs_f32(secs);
            while Instant::now() < until {
                idle();
                std::thread::sleep(Duration::from_millis(16));
            }
        };
        pump(0.5);
        assert!(inst.open_editor_in(top, 0, 48));
        pump(2.0);
        let last_translated = || {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            text.lines().rev().find_map(|l| {
                let t = l.split("Translated coords: ").nth(1)?;
                let pos = t.rsplit(' ').next()?;
                let mut it = pos.split('+').skip(1);
                Some((it.next()?.parse::<i32>().ok()?, it.next()?.parse::<i32>().ok()?))
            })
        };
        for (x, y) in [(300, 200), (40, 400)] {
            x11::move_to(top, x, y);
            pump(1.0);
            let w = inst.editor_window();
            let g = x11::geometry(w).unwrap();
            x11::announce(w, g);
            pump(0.5);
            assert_eq!(last_translated(), Some((g.0, g.1)), "after moving to {x},{y}");
        }
        inst.close_editor();
        x11::destroy(top);
    }

    #[test]
    fn dropping_a_dead_instance_does_not_close_it() {
        drop(Instance::dead_for_test(Vec::new()));
    }

    #[test]
    fn dropping_a_dead_instance_marks_its_module() {
        drop(Instance::dead_for_test(Vec::new()));
        assert!(BRIDGES.lock().unwrap().get_or_insert_default().has_died(Path::new("/test/Dead.vst3")));
    }
}
