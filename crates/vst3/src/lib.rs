use serde::{Deserialize, Serialize};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    );
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
    fn sv_idle();
}

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
        let ptr = unsafe {
            sv_open(cstr(&info.path).as_ptr(), uid.as_ptr(), sample_rate, max_block as i32, err.as_mut_ptr(), err.len())
        };
        if ptr.is_null() {
            return Err(err_string(&err));
        }
        let sidechain = unsafe { sv_has_sidechain(ptr) != 0 };
        Ok(Arc::new(Instance { ptr, info: info.clone(), max_block, sidechain }))
    }

    pub fn latency(&self) -> u32 {
        unsafe { sv_latency(self.ptr) }
    }

    pub fn has_input(&self) -> bool {
        unsafe { sv_has_input(self.ptr) != 0 }
    }

    /// Whether the plug-in has an auxiliary audio input to key.
    pub fn has_sidechain(&self) -> bool {
        self.sidechain
    }

    /// Switches the side-chain input on or off, briefly suspending processing; returns whether
    /// it changed. Call from the UI thread only. The latency may differ afterwards.
    pub fn set_sidechain_active(&self, on: bool) -> bool {
        unsafe { sv_set_sidechain_active(self.ptr, on as i32) != 0 }
    }

    pub fn state(&self) -> Vec<u8> {
        let mut data = std::ptr::null();
        let mut len = 0;
        if unsafe { sv_get_state(self.ptr, &mut data, &mut len) } == 0 || data.is_null() {
            return Vec::new();
        }
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        !data.is_empty() && unsafe { sv_set_state(self.ptr, data.as_ptr(), data.len()) != 0 }
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        let n = unsafe { sv_param_count(self.ptr) };
        (0..n)
            .filter_map(|i| {
                let mut raw: SvParamInfo = unsafe { std::mem::zeroed() };
                if unsafe { sv_param_info(self.ptr, i, &mut raw) } == 0 {
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
        unsafe { sv_param_get(self.ptr, id) }
    }

    pub fn set_param(&self, id: u32, value: f64) {
        unsafe { sv_param_set(self.ptr, id, value) }
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        let mut buf = [0 as c_char; 128];
        unsafe { sv_param_string(self.ptr, id, value, buf.as_mut_ptr(), buf.len()) };
        err_string(&buf)
    }

    pub fn has_editor(&self) -> bool {
        unsafe { sv_has_editor(self.ptr) != 0 }
    }

    pub fn open_editor(&self, title: &str) -> bool {
        let t = CString::new(title).unwrap_or_default();
        unsafe { sv_open_editor(self.ptr, t.as_ptr()) != 0 }
    }

    /// Opens the editor inside an X11 window, at a position in that window's pixels.
    pub fn open_editor_in(&self, parent: u64, x: i32, y: i32) -> bool {
        unsafe { sv_open_editor_in(self.ptr, parent as std::ffi::c_ulong, x, y) != 0 }
    }

    /// The editor's size in pixels, which the plug-in may change while open.
    pub fn editor_size(&self) -> (i32, i32) {
        let (mut w, mut h) = (0, 0);
        unsafe { sv_editor_size(self.ptr, &mut w, &mut h) };
        (w, h)
    }

    pub fn move_editor(&self, x: i32, y: i32) {
        unsafe { sv_move_editor(self.ptr, x, y) }
    }

    pub fn close_editor(&self) {
        unsafe { sv_close_editor(self.ptr) }
    }

    pub fn editor_open(&self) -> bool {
        unsafe { sv_editor_open(self.ptr) != 0 }
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
        unsafe { sv_close(self.ptr) }
    }
}

/// Main-thread housekeeping for every open plug-in: run loop, editor windows, parameter echoes.
pub fn idle() {
    unsafe { sv_idle() }
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
        let Some([l, r]) = key else {
            return unsafe { sv_sidechain(self.inst.ptr, std::ptr::null(), 0) };
        };
        // The shim copies the key into the plug-in's own bus buffers before returning.
        let ptrs = [l.as_ptr(), r.as_ptr()];
        unsafe { sv_sidechain(self.inst.ptr, ptrs.as_ptr(), l.len().min(r.len()) as i32) }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], events: &[MidiEvent], ctx: &ProcessContext) {
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
        unsafe {
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
        self.params.clear();
    }
}
