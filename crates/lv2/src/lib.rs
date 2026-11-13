//! LV2 plug-in hosting through lilv. Scanning only reads the bundles' Turtle files, so it runs
//! in the app. Lifecycle, parameters, state and interfaces are driven from the UI thread; `run`
//! happens on the audio thread through `Processor`.

mod ffi;

use ffi::*;
use serde::{Deserialize, Serialize};
use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_vst3::{ClassInfo, ParamInfo};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// Class IDs of LV2 plug-ins start with this, followed by the plug-in's URI.
pub const PREFIX: &str = "lv2:";

const LV2: &str = "http://lv2plug.in/ns/lv2core#";
const AUDIO_PORT: &CStr = c"http://lv2plug.in/ns/lv2core#AudioPort";
const CONTROL_PORT: &CStr = c"http://lv2plug.in/ns/lv2core#ControlPort";
const CV_PORT: &CStr = c"http://lv2plug.in/ns/lv2core#CVPort";
const INPUT_PORT: &CStr = c"http://lv2plug.in/ns/lv2core#InputPort";
const IS_SIDE_CHAIN: &CStr = c"http://lv2plug.in/ns/lv2core#isSideChain";
const TOGGLED: &CStr = c"http://lv2plug.in/ns/lv2core#toggled";
const INTEGER: &CStr = c"http://lv2plug.in/ns/lv2core#integer";
const ENUMERATION: &CStr = c"http://lv2plug.in/ns/lv2core#enumeration";
const SAMPLE_RATE: &CStr = c"http://lv2plug.in/ns/lv2core#sampleRate";
const LOGARITHMIC: &CStr = c"http://lv2plug.in/ns/ext/port-props#logarithmic";
const NOT_ON_GUI: &CStr = c"http://lv2plug.in/ns/ext/port-props#notOnGUI";
const UNIT: &CStr = c"http://lv2plug.in/ns/extensions/units#unit";
const ATOM_PORT: &CStr = c"http://lv2plug.in/ns/ext/atom#AtomPort";
const ATOM_SEQUENCE: &CStr = c"http://lv2plug.in/ns/ext/atom#Sequence";
const ATOM_CHUNK: &CStr = c"http://lv2plug.in/ns/ext/atom#Chunk";
const ATOM_FLOAT: &CStr = c"http://lv2plug.in/ns/ext/atom#Float";
const ATOM_DOUBLE: &CStr = c"http://lv2plug.in/ns/ext/atom#Double";
const ATOM_INT: &CStr = c"http://lv2plug.in/ns/ext/atom#Int";
const EVENT_TRANSFER: &CStr = c"http://lv2plug.in/ns/ext/atom#eventTransfer";
const MIDI_EVENT: &CStr = c"http://lv2plug.in/ns/ext/midi#MidiEvent";
const MINIMUM_SIZE: &CStr = c"http://lv2plug.in/ns/ext/resize-port#minimumSize";
const RDF_TYPE: &CStr = c"http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const STATE_INTERFACE: &CStr = c"http://lv2plug.in/ns/ext/state#interface";
const WORKER_INTERFACE: &CStr = c"http://lv2plug.in/ns/ext/worker#interface";
const URID_MAP: &CStr = c"http://lv2plug.in/ns/ext/urid#map";
const URID_UNMAP: &CStr = c"http://lv2plug.in/ns/ext/urid#unmap";
const OPTIONS: &CStr = c"http://lv2plug.in/ns/ext/options#options";
const BOUNDED_BLOCK: &CStr = c"http://lv2plug.in/ns/ext/buf-size#boundedBlockLength";
const MAX_BLOCK: &CStr = c"http://lv2plug.in/ns/ext/buf-size#maxBlockLength";
const MIN_BLOCK: &CStr = c"http://lv2plug.in/ns/ext/buf-size#minBlockLength";
const NOMINAL_BLOCK: &CStr = c"http://lv2plug.in/ns/ext/buf-size#nominalBlockLength";
const PARAM_SAMPLE_RATE: &CStr = c"http://lv2plug.in/ns/ext/parameters#sampleRate";
const WORKER_SCHEDULE: &CStr = c"http://lv2plug.in/ns/ext/worker#schedule";
const LOG: &CStr = c"http://lv2plug.in/ns/ext/log#log";
const INSTANCE_ACCESS: &CStr = c"http://lv2plug.in/ns/ext/instance-access";
const DATA_ACCESS: &CStr = c"http://lv2plug.in/ns/ext/data-access";
const X11_UI: &CStr = c"http://lv2plug.in/ns/extensions/ui#X11UI";
const UI_PARENT: &CStr = c"http://lv2plug.in/ns/extensions/ui#parent";
const UI_RESIZE: &CStr = c"http://lv2plug.in/ns/extensions/ui#resize";
const UI_IDLE: &CStr = c"http://lv2plug.in/ns/extensions/ui#idleInterface";
const UI_TOUCH: &CStr = c"http://lv2plug.in/ns/extensions/ui#touch";
const UI_PORT_MAP: &CStr = c"http://lv2plug.in/ns/extensions/ui#portMap";
const UI_UPDATE_RATE: &CStr = c"http://lv2plug.in/ns/extensions/ui#updateRate";

/// Features a plug-in may require. The last three need nothing from the host: inputs are never
/// processed in place.
const SUPPORTED: [&CStr; 9] = [
    URID_MAP,
    URID_UNMAP,
    OPTIONS,
    BOUNDED_BLOCK,
    WORKER_SCHEDULE,
    LOG,
    c"http://lv2plug.in/ns/lv2core#inPlaceBroken",
    c"http://lv2plug.in/ns/lv2core#hardRTCapable",
    c"http://lv2plug.in/ns/lv2core#isLive",
];

thread_local!(static AUDIO: Cell<bool> = const { Cell::new(false) });

/// For log.c, which drops messages logged from inside `run`.
#[unsafe(no_mangle)]
pub extern "C" fn solstice_lv2_on_audio_thread() -> bool {
    AUDIO.get()
}

/// One URID per URI for the whole process, so state and atoms mean the same in every instance.
/// Every URI the host uses on the audio thread is mapped up front; plug-ins that map from `run`
/// take this lock briefly.
#[derive(Default)]
struct Urids {
    ids: HashMap<CString, u32>,
    uris: Vec<CString>,
}

static URIDS: LazyLock<Mutex<Urids>> = LazyLock::new(Mutex::default);

fn urid(uri: &CStr) -> u32 {
    let mut u = URIDS.lock().unwrap();
    if let Some(id) = u.ids.get(uri) {
        return *id;
    }
    u.uris.push(uri.to_owned());
    let id = u.uris.len() as u32;
    u.ids.insert(uri.to_owned(), id);
    id
}

unsafe extern "C" fn map_uri(_: *mut c_void, uri: *const c_char) -> u32 {
    if uri.is_null() { 0 } else { urid(unsafe { CStr::from_ptr(uri) }) }
}

// The strings never move: a CString's buffer stays put when the Vec grows.
unsafe extern "C" fn unmap_uri(_: *mut c_void, id: u32) -> *const c_char {
    let u = URIDS.lock().unwrap();
    id.checked_sub(1).and_then(|i| u.uris.get(i as usize)).map_or(std::ptr::null(), |s| s.as_ptr())
}

/// Where plug-ins are looked for: `LV2_PATH`, or the usual folders.
pub fn search_path() -> String {
    if let Some(p) = std::env::var("LV2_PATH").ok().filter(|p| !p.is_empty()) {
        return p;
    }
    let mut dirs: Vec<String> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".lv2").to_string_lossy().into_owned());
    }
    dirs.extend(["/usr/local/lib64/lv2", "/usr/lib64/lv2", "/usr/local/lib/lv2", "/usr/lib/lv2"].map(String::from));
    dirs.join(":")
}

struct World {
    ptr: *mut LilvWorld,
    nodes: HashMap<&'static CStr, *mut LilvNode>,
    bundles: HashSet<PathBuf>,
}

unsafe impl Send for World {}

impl World {
    fn new() -> World {
        World { ptr: unsafe { lilv_world_new() }, nodes: HashMap::new(), bundles: HashSet::new() }
    }

    fn load_all(&mut self) {
        let path = CString::new(search_path()).unwrap_or_default();
        unsafe {
            let node = lilv_new_string(self.ptr, path.as_ptr());
            lilv_world_set_option(self.ptr, LILV_OPTION_LV2_PATH.as_ptr(), node);
            lilv_node_free(node);
            lilv_world_load_all(self.ptr);
        }
    }

    fn load_bundle(&mut self, dir: &Path) {
        if dir.as_os_str().is_empty() || !self.bundles.insert(dir.to_path_buf()) {
            return;
        }
        let path = CString::new(dir.as_os_str().as_encoded_bytes()).unwrap_or_default();
        unsafe {
            let uri = lilv_new_file_uri(self.ptr, std::ptr::null(), path.as_ptr());
            // lilv wants bundle URIs to end in a slash.
            let with_slash = CString::new(format!("{}/", text(lilv_node_as_uri(uri)).trim_end_matches('/'))).unwrap_or_default();
            lilv_node_free(uri);
            let uri = lilv_new_uri(self.ptr, with_slash.as_ptr());
            lilv_world_load_bundle(self.ptr, uri);
            lilv_node_free(uri);
        }
    }

    fn node(&mut self, uri: &'static CStr) -> *const LilvNode {
        let world = self.ptr;
        *self.nodes.entry(uri).or_insert_with(|| unsafe { lilv_new_uri(world, uri.as_ptr()) })
    }

    fn plugin(&mut self, uri: &str) -> Option<*const LilvPlugin> {
        let uri = CString::new(uri).ok()?;
        let p = unsafe {
            let node = lilv_new_uri(self.ptr, uri.as_ptr());
            let p = lilv_plugins_get_by_uri(lilv_world_get_all_plugins(self.ptr), node);
            lilv_node_free(node);
            p
        };
        (!p.is_null()).then_some(p)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        unsafe {
            for n in self.nodes.values() {
                lilv_node_free(*n);
            }
            lilv_world_free(self.ptr);
        }
    }
}

/// The world instances are created from. Bundles are loaded into it as plug-ins are opened.
/// lilv isn't thread-safe, so every call into it holds this lock.
fn world() -> MutexGuard<'static, World> {
    static WORLD: OnceLock<Mutex<World>> = OnceLock::new();
    WORLD.get_or_init(|| Mutex::new(World::new())).lock().unwrap()
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// Frees `node` once read.
unsafe fn take_string(node: *mut LilvNode) -> String {
    if node.is_null() {
        return String::new();
    }
    let s = unsafe { text(lilv_node_as_string(node)) };
    unsafe { lilv_node_free(node) };
    s
}

unsafe fn file_path(uri: *const LilvNode) -> Option<PathBuf> {
    if uri.is_null() {
        return None;
    }
    let p = unsafe { lilv_file_uri_parse(lilv_node_as_uri(uri), std::ptr::null_mut()) };
    if p.is_null() {
        return None;
    }
    let path = PathBuf::from(unsafe { text(p) });
    unsafe { lilv_free(p as *mut c_void) };
    Some(path)
}

/// The URIs in a node list, which is freed.
unsafe fn take_uris(nodes: *mut LilvNodes) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = unsafe { lilv_nodes_begin(nodes) };
    while !unsafe { lilv_nodes_is_end(nodes, i) } {
        let n = unsafe { lilv_nodes_get(nodes, i) };
        if unsafe { lilv_node_is_uri(n) } {
            out.push(unsafe { text(lilv_node_as_uri(n)) });
        }
        i = unsafe { lilv_nodes_next(nodes, i) };
    }
    unsafe { lilv_nodes_free(nodes) };
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Audio,
    Control,
    Atom,
    Cv,
    Other,
}

#[derive(Clone, Debug)]
struct PortInfo {
    index: u32,
    kind: Kind,
    input: bool,
    symbol: CString,
    sidechain: bool,
    midi: bool,
    /// Bytes for an atom port's buffer.
    size: usize,
}

const ATOM_SIZE: usize = 8192;

fn ports(w: &mut World, plugin: *const LilvPlugin) -> Vec<PortInfo> {
    let n = unsafe { lilv_plugin_get_num_ports(plugin) };
    let (audio, control, cv, atom, input) = (w.node(AUDIO_PORT), w.node(CONTROL_PORT), w.node(CV_PORT), w.node(ATOM_PORT), w.node(INPUT_PORT));
    let (sidechain, midi, min_size) = (w.node(IS_SIDE_CHAIN), w.node(MIDI_EVENT), w.node(MINIMUM_SIZE));
    (0..n)
        .map(|index| unsafe {
            let port = lilv_plugin_get_port_by_index(plugin, index);
            let is = |class| lilv_port_is_a(plugin, port, class);
            let kind = if is(audio) {
                Kind::Audio
            } else if is(control) {
                Kind::Control
            } else if is(atom) {
                Kind::Atom
            } else if is(cv) {
                Kind::Cv
            } else {
                Kind::Other
            };
            let input = is(input);
            let size = lilv_port_get(plugin, port, min_size);
            let min = if size.is_null() { 0 } else { lilv_node_as_float(size).max(0.0) as usize };
            lilv_node_free(size);
            PortInfo {
                index,
                kind,
                input,
                symbol: CString::new(text(lilv_node_as_string(lilv_port_get_symbol(plugin, port)))).unwrap_or_default(),
                sidechain: lilv_port_has_property(plugin, port, sidechain),
                midi: kind == Kind::Atom && input && lilv_port_supports_event(plugin, port, midi),
                size: min.max(ATOM_SIZE).next_multiple_of(8),
            }
        })
        .collect()
}

/// The audio ports processed as the track's signal: up to two inputs and two outputs. Further
/// inputs, and those marked as side-chains, take the side-chain key.
struct Layout {
    inputs: Vec<u32>,
    outputs: Vec<u32>,
    keys: Vec<u32>,
}

fn layout(ports: &[PortInfo]) -> Layout {
    let mut l = Layout { inputs: Vec::new(), outputs: Vec::new(), keys: Vec::new() };
    for p in ports.iter().filter(|p| p.kind == Kind::Audio) {
        match (p.input, p.sidechain) {
            (true, false) if l.inputs.len() < 2 => l.inputs.push(p.index),
            (true, _) => l.keys.push(p.index),
            (false, false) if l.outputs.len() < 2 => l.outputs.push(p.index),
            (false, _) => {}
        }
    }
    l
}

/// LV2 plug-in classes as the VST3-style subcategories the rest of Solstice groups by.
fn subcategories(types: &[String], ports: &[PortInfo]) -> String {
    let l = layout(ports);
    let has = |t: &str| types.iter().any(|x| x.strip_prefix(LV2) == Some(t));
    let instrument = has("InstrumentPlugin") || (l.inputs.is_empty() && ports.iter().any(|p| p.midi));
    let mut out = vec![if instrument { "Instrument" } else { "Fx" }];
    for (class, cat) in [
        ("EQPlugin", "EQ"),
        ("ParaEQPlugin", "EQ"),
        ("MultiEQPlugin", "EQ"),
        ("DynamicsPlugin", "Dynamics"),
        ("CompressorPlugin", "Dynamics"),
        ("LimiterPlugin", "Dynamics"),
        ("GatePlugin", "Dynamics"),
        ("ExpanderPlugin", "Dynamics"),
        ("EnvelopePlugin", "Dynamics"),
        ("ReverbPlugin", "Reverb"),
        ("DelayPlugin", "Delay"),
        ("DistortionPlugin", "Distortion"),
        ("WaveshaperPlugin", "Distortion"),
        ("FilterPlugin", "Filter"),
        ("LowpassPlugin", "Filter"),
        ("HighpassPlugin", "Filter"),
        ("BandpassPlugin", "Filter"),
        ("CombPlugin", "Filter"),
        ("AllpassPlugin", "Filter"),
        ("AnalyserPlugin", "Analyzer"),
        ("ModulatorPlugin", "Modulation"),
        ("ChorusPlugin", "Modulation"),
        ("FlangerPlugin", "Modulation"),
        ("PhaserPlugin", "Modulation"),
        ("PitchPlugin", "Pitch Shift"),
        ("SpatialPlugin", "Spatial"),
        ("UtilityPlugin", "Tools"),
        ("MixerPlugin", "Tools"),
        ("AmplifierPlugin", "Tools"),
    ] {
        if has(class) && !out.contains(&cat) {
            out.push(cat);
        }
    }
    match l.outputs.len() {
        1 => out.push("Mono"),
        2 => out.push("Stereo"),
        _ => {}
    }
    out.join("|")
}

/// The first feature the plug-in requires that this host doesn't provide.
fn missing_feature(plugin: *const LilvPlugin) -> Option<String> {
    let required = unsafe { take_uris(lilv_plugin_get_required_features(plugin)) };
    required.into_iter().find(|f| !SUPPORTED.iter().any(|s| s.to_bytes() == f.as_bytes()))
}

/// Every installed plug-in the host can run.
pub fn scan_all() -> Vec<ClassInfo> {
    let mut w = World::new();
    w.load_all();
    let rdf_type = w.node(RDF_TYPE);
    let plugins = unsafe { lilv_world_get_all_plugins(w.ptr) };
    let mut out = Vec::new();
    let mut i = unsafe { lilv_plugins_begin(plugins) };
    while !unsafe { lilv_plugins_is_end(plugins, i) } {
        let plugin = unsafe { lilv_plugins_get(plugins, i) };
        i = unsafe { lilv_plugins_next(plugins, i) };
        if missing_feature(plugin).is_some() {
            continue;
        }
        let ports = ports(&mut w, plugin);
        if !ports.iter().any(|p| p.kind == Kind::Audio && !p.input) {
            continue;
        }
        let types = unsafe { take_uris(lilv_plugin_get_value(plugin, rdf_type)) };
        let uri = unsafe { text(lilv_node_as_uri(lilv_plugin_get_uri(plugin))) };
        out.push(ClassInfo {
            uid: format!("{PREFIX}{uri}"),
            name: unsafe { take_string(lilv_plugin_get_name(plugin)) },
            vendor: unsafe { take_string(lilv_plugin_get_author_name(plugin)) },
            subcategories: subcategories(&types, &ports),
            path: unsafe { file_path(lilv_plugin_get_bundle_uri(plugin)) }
                .map(|p| PathBuf::from(p.to_string_lossy().trim_end_matches('/')))
                .unwrap_or_default(),
        });
    }
    out
}

/// A control port as a parameter. Its id is the port index.
#[derive(Clone, Debug)]
struct Param {
    port: u32,
    title: String,
    units: String,
    min: f32,
    max: f32,
    default: f32,
    log: bool,
    integer: bool,
    toggled: bool,
    enumeration: bool,
    /// Named values, by value.
    points: Vec<(f32, String)>,
    hidden: bool,
    output: bool,
}

impl Param {
    fn steps(&self) -> i32 {
        if self.enumeration && !self.points.is_empty() {
            self.points.len() as i32 - 1
        } else if self.toggled {
            1
        } else if self.integer {
            (self.max - self.min).round().max(0.0) as i32
        } else {
            0
        }
    }

    fn plain(&self, norm: f64) -> f32 {
        let norm = norm.clamp(0.0, 1.0) as f32;
        if self.enumeration && !self.points.is_empty() {
            let i = (norm * (self.points.len() - 1) as f32).round() as usize;
            return self.points[i.min(self.points.len() - 1)].0;
        }
        if self.toggled {
            return if norm >= 0.5 { self.max } else { self.min };
        }
        let v = if self.log && self.min > 0.0 && self.max > self.min {
            self.min * (self.max / self.min).powf(norm)
        } else {
            self.min + norm * (self.max - self.min)
        };
        if self.integer { v.round() } else { v }
    }

    fn norm(&self, plain: f32) -> f64 {
        if self.enumeration && self.points.len() > 1 {
            let nearest = self
                .points
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1.0 - plain).abs().total_cmp(&(b.1.0 - plain).abs()))
                .map_or(0, |(i, _)| i);
            return nearest as f64 / (self.points.len() - 1) as f64;
        }
        if self.max <= self.min {
            return 0.0;
        }
        if self.toggled {
            return if plain > (self.min + self.max) / 2.0 { 1.0 } else { 0.0 };
        }
        let n = if self.log && self.min > 0.0 && plain > 0.0 {
            (plain / self.min).ln() / (self.max / self.min).ln()
        } else {
            (plain - self.min) / (self.max - self.min)
        };
        n.clamp(0.0, 1.0) as f64
    }

    fn text(&self, plain: f32) -> String {
        if let Some((_, label)) = self.points.iter().find(|(v, _)| (v - plain).abs() < 1e-4) {
            return label.clone();
        }
        if self.toggled {
            return if plain > (self.min + self.max) / 2.0 { "On" } else { "Off" }.into();
        }
        if self.integer { format!("{plain:.0}") } else { format!("{plain:.2}") }
    }
}

fn unit_symbol(uri: &str) -> &'static str {
    match uri.strip_prefix("http://lv2plug.in/ns/extensions/units#").unwrap_or("") {
        "db" => "dB",
        "hz" => "Hz",
        "khz" => "kHz",
        "mhz" => "MHz",
        "ms" => "ms",
        "s" => "s",
        "min" => "min",
        "pc" => "%",
        "bpm" => "BPM",
        "semitone12TET" => "st",
        "cent" => "ct",
        "degree" => "°",
        "frame" => "frames",
        _ => "",
    }
}

fn params(w: &mut World, plugin: *const LilvPlugin, ports: &[PortInfo], sample_rate: f32) -> Vec<Param> {
    let n = ports.len();
    let (mut mins, mut maxs, mut defs) = (vec![f32::NAN; n], vec![f32::NAN; n], vec![f32::NAN; n]);
    unsafe { lilv_plugin_get_port_ranges_float(plugin, mins.as_mut_ptr(), maxs.as_mut_ptr(), defs.as_mut_ptr()) };
    let latency = unsafe { lilv_plugin_has_latency(plugin).then(|| lilv_plugin_get_latency_port_index(plugin)) };
    let (toggled, integer, enumeration, rate) = (w.node(TOGGLED), w.node(INTEGER), w.node(ENUMERATION), w.node(SAMPLE_RATE));
    let (log, not_on_gui, unit) = (w.node(LOGARITHMIC), w.node(NOT_ON_GUI), w.node(UNIT));
    ports
        .iter()
        .filter(|p| p.kind == Kind::Control)
        .map(|p| unsafe {
            let i = p.index as usize;
            let port = lilv_plugin_get_port_by_index(plugin, p.index);
            let has = |prop| lilv_port_has_property(plugin, port, prop);
            let scale = if has(rate) { sample_rate } else { 1.0 };
            let min = if mins[i].is_nan() { 0.0 } else { mins[i] * scale };
            let max = if maxs[i].is_nan() { 1.0 } else { maxs[i] * scale };
            let default = if defs[i].is_nan() { min } else { defs[i] * scale };
            let mut points = Vec::new();
            let sp = lilv_port_get_scale_points(plugin, port);
            if !sp.is_null() {
                let mut it = lilv_scale_points_begin(sp);
                while !lilv_scale_points_is_end(sp, it) {
                    let point = lilv_scale_points_get(sp, it);
                    points.push((lilv_node_as_float(lilv_scale_point_get_value(point)), text(lilv_node_as_string(lilv_scale_point_get_label(point)))));
                    it = lilv_scale_points_next(sp, it);
                }
                lilv_scale_points_free(sp);
            }
            points.sort_by(|a, b| a.0.total_cmp(&b.0));
            let unit_node = lilv_port_get(plugin, port, unit);
            let units = if !unit_node.is_null() && lilv_node_is_uri(unit_node) { unit_symbol(&text(lilv_node_as_uri(unit_node))) } else { "" };
            lilv_node_free(unit_node);
            Param {
                port: p.index,
                title: take_string(lilv_port_get_name(plugin, port)),
                units: units.into(),
                min,
                max,
                default,
                log: has(log),
                integer: has(integer),
                toggled: has(toggled),
                enumeration: has(enumeration),
                points,
                hidden: has(not_on_gui) || latency == Some(p.index),
                output: !p.input,
            }
        })
        .collect()
}

/// The plug-in's X11 interface, as found in its bundle.
struct UiInfo {
    uri: CString,
    binary: PathBuf,
    /// With a trailing slash, as LV2 passes it.
    bundle: CString,
}

fn x11_ui(w: &mut World, plugin: *const LilvPlugin) -> Option<UiInfo> {
    let x11 = w.node(X11_UI);
    let uis = unsafe { lilv_plugin_get_uis(plugin) };
    if uis.is_null() {
        return None;
    }
    let mut found = None;
    let mut i = unsafe { lilv_uis_begin(uis) };
    while found.is_none() && !unsafe { lilv_uis_is_end(uis, i) } {
        let ui = unsafe { lilv_uis_get(uis, i) };
        i = unsafe { lilv_uis_next(uis, i) };
        if !unsafe { lilv_ui_is_a(ui, x11) } {
            continue;
        }
        let (Some(binary), Some(bundle)) = (unsafe { file_path(lilv_ui_get_binary_uri(ui)) }, unsafe { file_path(lilv_ui_get_bundle_uri(ui)) })
        else {
            continue;
        };
        let bundle = format!("{}/", bundle.to_string_lossy().trim_end_matches('/'));
        found = Some(UiInfo {
            uri: CString::new(unsafe { text(lilv_node_as_uri(lilv_ui_get_uri(ui))) }).unwrap_or_default(),
            binary,
            bundle: CString::new(bundle).unwrap_or_default(),
        });
    }
    unsafe { lilv_uis_free(uis) };
    found
}

/// Messages in a byte ring: a u32 tag, a u32 length, then the bytes.
fn push_msg(q: &mut rtrb::Producer<u8>, tag: u32, data: &[u8]) -> bool {
    let Ok(chunk) = q.write_chunk_uninit(8 + data.len()) else { return false };
    let head = tag.to_ne_bytes().into_iter().chain((data.len() as u32).to_ne_bytes());
    chunk.fill_from_iter(head.chain(data.iter().copied()));
    true
}

/// The next message's tag, its bytes in `out`. A message longer than `out`'s capacity is
/// dropped, leaving `out` empty, so the audio thread never allocates.
fn pop_msg(q: &mut rtrb::Consumer<u8>, out: &mut Vec<u8>) -> Option<u32> {
    let mut head = [0u8; 8];
    {
        let chunk = q.read_chunk(8).ok()?;
        let (a, b) = chunk.as_slices();
        for (h, v) in head.iter_mut().zip(a.iter().chain(b)) {
            *h = *v;
        }
    }
    let tag = u32::from_ne_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_ne_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let chunk = q.read_chunk(8 + len).ok()?;
    out.clear();
    if len <= out.capacity() {
        let (a, b) = chunk.as_slices();
        out.extend(a.iter().chain(b).skip(8));
    }
    chunk.commit_all();
    Some(tag)
}

/// Instances with a worker, served by one thread. Each job has its own lock, held around its
/// `work` calls: a state restore holds it to keep `work` out, and removing a job empties it,
/// which waits for a call under way, so an instance is never freed under it.
struct Job {
    lv2: LV2_Handle,
    iface: *const LV2_Worker_Interface,
    requests: rtrb::Consumer<u8>,
    responses: rtrb::Producer<u8>,
}

unsafe impl Send for Job {}

type JobCell = Arc<Mutex<Option<Job>>>;

static JOBS: Mutex<Vec<JobCell>> = Mutex::new(Vec::new());

unsafe extern "C" fn worker_respond(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    let q = unsafe { &mut *(handle as *mut rtrb::Producer<u8>) };
    let data = if size == 0 || data.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) } };
    if push_msg(q, 0, data) { LV2_WORKER_SUCCESS } else { LV2_WORKER_ERR_NO_SPACE }
}

fn add_job(job: Job) -> JobCell {
    static STARTED: OnceLock<()> = OnceLock::new();
    let cell = Arc::new(Mutex::new(Some(job)));
    JOBS.lock().unwrap().push(cell.clone());
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("lv2-worker".into())
            .spawn(|| {
                let mut buf = Vec::with_capacity(RING);
                let mut jobs: Vec<JobCell> = Vec::new();
                loop {
                    jobs.clone_from(&JOBS.lock().unwrap());
                    for cell in &jobs {
                        let mut job = cell.lock().unwrap();
                        let Some(job) = job.as_mut() else { continue };
                        while pop_msg(&mut job.requests, &mut buf).is_some() {
                            let Some(work) = (unsafe { (*job.iface).work }) else { continue };
                            let respond = &mut job.responses as *mut rtrb::Producer<u8> as *mut c_void;
                            unsafe { work(job.lv2, worker_respond, respond, buf.len() as u32, buf.as_ptr() as *const c_void) };
                        }
                    }
                    jobs.clear();
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
            .expect("worker thread");
    });
    cell
}

const RING: usize = 1 << 16;

unsafe extern "C" fn schedule_work(handle: *mut c_void, size: u32, data: *const c_void) -> u32 {
    let q = unsafe { &*(handle as *const Mutex<rtrb::Producer<u8>>) };
    // Only `run` and a state restore schedule, never at once; the lock is never waited for.
    let Ok(mut q) = q.try_lock() else { return LV2_WORKER_ERR_UNKNOWN };
    let data = if size == 0 || data.is_null() { &[][..] } else { unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) } };
    if push_msg(&mut q, 0, data) { LV2_WORKER_SUCCESS } else { LV2_WORKER_ERR_NO_SPACE }
}

/// What the host hands the plug-in at instantiation. Boxed so the pointers into it hold.
struct Features {
    map: LV2_URID_Map,
    unmap: LV2_URID_Unmap,
    schedule: LV2_Worker_Schedule,
    log: LV2_Log_Log,
    blocks: [i32; 3],
    sample_rate: f32,
    options: [LV2_Options_Option; 5],
    requests: Box<Mutex<rtrb::Producer<u8>>>,
    list: Vec<LV2_Feature>,
    ptrs: Vec<*const LV2_Feature>,
}

fn option(key: &CStr, type_: &CStr, size: usize, value: *const c_void) -> LV2_Options_Option {
    LV2_Options_Option { context: 0, subject: 0, key: urid(key), size: size as u32, type_: urid(type_), value }
}

impl Features {
    fn new(sample_rate: f64, max_block: usize, requests: rtrb::Producer<u8>) -> Box<Features> {
        let mut f = Box::new(Features {
            map: LV2_URID_Map { handle: std::ptr::null_mut(), map: map_uri },
            unmap: LV2_URID_Unmap { handle: std::ptr::null_mut(), unmap: unmap_uri },
            schedule: LV2_Worker_Schedule { handle: std::ptr::null_mut(), schedule_work },
            log: LV2_Log_Log {
                handle: std::ptr::null_mut(),
                printf: solstice_lv2_log_printf as *const c_void,
                vprintf: solstice_lv2_log_vprintf as *const c_void,
            },
            blocks: [max_block as i32, 1, max_block as i32],
            sample_rate: sample_rate as f32,
            options: unsafe { std::mem::zeroed() },
            requests: Box::new(Mutex::new(requests)),
            list: Vec::new(),
            ptrs: Vec::new(),
        });
        f.schedule.handle = &*f.requests as *const Mutex<rtrb::Producer<u8>> as *mut c_void;
        let int = std::mem::size_of::<i32>();
        f.options = [
            option(MAX_BLOCK, ATOM_INT, int, &f.blocks[0] as *const i32 as *const c_void),
            option(MIN_BLOCK, ATOM_INT, int, &f.blocks[1] as *const i32 as *const c_void),
            option(NOMINAL_BLOCK, ATOM_INT, int, &f.blocks[2] as *const i32 as *const c_void),
            option(PARAM_SAMPLE_RATE, ATOM_FLOAT, 4, &f.sample_rate as *const f32 as *const c_void),
            unsafe { std::mem::zeroed() },
        ];
        let ptr = |x: *const c_void| x as *mut c_void;
        f.list = vec![
            LV2_Feature { uri: URID_MAP.as_ptr(), data: ptr(&f.map as *const _ as *const c_void) },
            LV2_Feature { uri: URID_UNMAP.as_ptr(), data: ptr(&f.unmap as *const _ as *const c_void) },
            LV2_Feature { uri: OPTIONS.as_ptr(), data: ptr(f.options.as_ptr() as *const c_void) },
            LV2_Feature { uri: BOUNDED_BLOCK.as_ptr(), data: std::ptr::null_mut() },
            LV2_Feature { uri: WORKER_SCHEDULE.as_ptr(), data: ptr(&f.schedule as *const _ as *const c_void) },
            LV2_Feature { uri: LOG.as_ptr(), data: ptr(&f.log as *const _ as *const c_void) },
        ];
        f.ptrs = f.list.iter().map(|x| x as *const LV2_Feature).chain([std::ptr::null()]).collect();
        f
    }
}

/// The plug-in itself, shared by the UI-side `Instance` and the engine-side `Processor`.
/// The last owner to go (the UI thread, once the engine has let go) tears it down.
struct Handle {
    instance: *mut LilvInstance,
    plugin: *const LilvPlugin,
    features: Box<Features>,
    job: Option<JobCell>,
    /// Each port's latest value as f32 bits: control inputs as set, control outputs as the
    /// plug-in last wrote them.
    values: Box<[AtomicU32]>,
    /// The audio thread copies every control input from `values` before its next run.
    resync: AtomicBool,
    /// The plug-in was run on other buffers; the audio thread connects its own again.
    reconnect: AtomicBool,
    /// Held by `run`, and by a state restore, which mustn't run alongside it.
    busy: AtomicBool,
    dirty: AtomicBool,
    /// An interface is open and wants the plug-in's atom output.
    ui_open: AtomicBool,
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn desc(&self) -> &LV2_Descriptor {
        unsafe { &*(*self.instance).lv2_descriptor }
    }

    fn lv2(&self) -> LV2_Handle {
        unsafe { (*self.instance).lv2_handle }
    }

    fn ext<T>(&self, uri: &CStr) -> Option<&T> {
        let p = self.desc().extension_data.map_or(std::ptr::null(), |f| unsafe { f(uri.as_ptr()) });
        (!p.is_null()).then(|| unsafe { &*(p as *const T) })
    }

    fn value(&self, port: u32) -> f32 {
        self.values.get(port as usize).map_or(0.0, |v| f32::from_bits(v.load(Relaxed)))
    }

    /// Runs `f` while the audio thread and the worker are kept out of the plug-in.
    fn exclusive<R>(&self, f: impl FnOnce() -> R) -> R {
        let _work = self.job.as_ref().map(|j| j.lock().unwrap());
        while self.busy.compare_exchange(false, true, Acquire, Relaxed).is_err() {
            std::thread::sleep(Duration::from_micros(200));
        }
        let r = f();
        self.busy.store(false, Release);
        r
    }

    /// Runs the plug-in once on silence, as its latency port only holds a value once it has run.
    /// The audio thread then reconnects its own buffers, as these are gone.
    fn measure_latency(&self, ports: &[PortInfo], port: u32, max_block: usize) {
        let Some(run) = self.desc().run else { return };
        let mut tmp = Ports::new(ports, &self.values, max_block);
        tmp.connect(self, ports, &Uris::new());
        unsafe { run(self.lv2(), max_block.min(256) as u32) };
        if let Some(v) = self.values.get(port as usize) {
            v.store(tmp.controls[port as usize].to_bits(), Relaxed);
        }
        self.reconnect.store(true, Release);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(cell) = &self.job {
            JOBS.lock().unwrap().retain(|j| !Arc::ptr_eq(j, cell));
            *cell.lock().unwrap() = None;
        }
        if let Some(f) = self.desc().deactivate {
            unsafe { f(self.lv2()) };
        }
        let _w = world();
        unsafe { lilv_instance_free(self.instance) };
    }
}

/// An atom port's buffer, 8-byte aligned as atoms must be.
struct AtomBuf {
    port: u32,
    input: bool,
    midi: bool,
    buf: Vec<u64>,
}

impl AtomBuf {
    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.buf.as_mut_ptr() as *mut u8, self.buf.len() * 8) }
    }

    /// An empty sequence for an input, or room to write one for an output.
    fn reset(&mut self, u: &Uris) {
        let cap = self.buf.len() * 8;
        let seq = self.buf.as_mut_ptr() as *mut LV2_Atom_Sequence;
        unsafe {
            if self.input {
                (*seq).atom = LV2_Atom { size: 8, type_: u.sequence };
                (*seq).body = LV2_Atom_Sequence_Body { unit: 0, pad: 0 };
            } else {
                (*seq).atom = LV2_Atom { size: (cap - 8) as u32, type_: u.chunk };
            }
        }
    }

    fn append(&mut self, frames: i64, type_: u32, data: &[u8]) -> bool {
        let bytes = self.bytes();
        let end = 8 + u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let need = 16 + data.len().next_multiple_of(8);
        if end + need > bytes.len() {
            return false;
        }
        bytes[end..end + 8].copy_from_slice(&frames.to_ne_bytes());
        bytes[end + 8..end + 12].copy_from_slice(&(data.len() as u32).to_ne_bytes());
        bytes[end + 12..end + 16].copy_from_slice(&type_.to_ne_bytes());
        bytes[end + 16..end + 16 + data.len()].copy_from_slice(data);
        let size = (end - 8 + need) as u32;
        bytes[0..4].copy_from_slice(&size.to_ne_bytes());
        true
    }

    /// Each event in an output sequence as a whole atom: header and body.
    fn events(&mut self, sequence: u32, mut f: impl FnMut(&[u8])) {
        let bytes = self.bytes();
        let size = u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let type_ = u32::from_ne_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if type_ != sequence {
            return;
        }
        let end = (8 + size).min(bytes.len());
        let mut at = 16;
        while at + 16 <= end {
            let len = u32::from_ne_bytes([bytes[at + 8], bytes[at + 9], bytes[at + 10], bytes[at + 11]]) as usize;
            if at + 16 + len > end {
                break;
            }
            f(&bytes[at + 8..at + 16 + len]);
            at += 16 + len.next_multiple_of(8);
        }
    }
}

/// URIDs the audio thread uses, mapped up front.
#[derive(Clone, Copy)]
struct Uris {
    sequence: u32,
    chunk: u32,
    midi: u32,
}

impl Uris {
    fn new() -> Uris {
        Uris { sequence: urid(ATOM_SEQUENCE), chunk: urid(ATOM_CHUNK), midi: urid(MIDI_EVENT) }
    }
}

/// Every port's buffer, connected once. The main audio outputs are reconnected to the engine's
/// buffers on each run.
struct Ports {
    controls: Box<[f32]>,
    /// Audio and CV ports: (index, buffer).
    audio: Vec<(u32, Vec<f32>)>,
    atoms: Vec<AtomBuf>,
    /// Anything else: an empty buffer, so no port is left unconnected.
    other: Vec<u64>,
    inputs: Vec<usize>,
    outputs: Vec<u32>,
    keys: Vec<usize>,
    control_inputs: Vec<u32>,
    control_outputs: Vec<u32>,
}

impl Ports {
    fn new(ports: &[PortInfo], values: &[AtomicU32], max_block: usize) -> Ports {
        let l = layout(ports);
        let audio: Vec<(u32, Vec<f32>)> = ports
            .iter()
            .filter(|p| matches!(p.kind, Kind::Audio | Kind::Cv))
            .map(|p| (p.index, vec![0.0; max_block]))
            .collect();
        let pos = |i: u32| audio.iter().position(|a| a.0 == i).unwrap_or(0);
        Ports {
            controls: values.iter().map(|v| f32::from_bits(v.load(Relaxed))).collect(),
            inputs: l.inputs.iter().map(|i| pos(*i)).collect(),
            keys: l.keys.iter().map(|i| pos(*i)).collect(),
            outputs: l.outputs,
            audio,
            atoms: ports
                .iter()
                .filter(|p| p.kind == Kind::Atom)
                .map(|p| AtomBuf { port: p.index, input: p.input, midi: p.midi, buf: vec![0; p.size / 8] })
                .collect(),
            other: vec![0; ATOM_SIZE / 8],
            control_inputs: ports.iter().filter(|p| p.kind == Kind::Control && p.input).map(|p| p.index).collect(),
            control_outputs: ports.iter().filter(|p| p.kind == Kind::Control && !p.input).map(|p| p.index).collect(),
        }
    }

    fn connect(&mut self, h: &Handle, ports: &[PortInfo], u: &Uris) {
        let Some(connect) = h.desc().connect_port else { return };
        let lv2 = h.lv2();
        for p in ports {
            let data = match p.kind {
                Kind::Control => &mut self.controls[p.index as usize] as *mut f32 as *mut c_void,
                Kind::Audio | Kind::Cv => match self.audio.iter_mut().find(|a| a.0 == p.index) {
                    Some(a) => a.1.as_mut_ptr() as *mut c_void,
                    None => continue,
                },
                Kind::Atom => match self.atoms.iter_mut().find(|a| a.port == p.index) {
                    Some(a) => {
                        a.reset(u);
                        a.buf.as_mut_ptr() as *mut c_void
                    }
                    None => continue,
                },
                Kind::Other => self.other.as_mut_ptr() as *mut c_void,
            };
            unsafe { connect(lv2, p.index, data) };
        }
    }
}

/// The plug-in's own interface, embedded in a window of ours or in a top-level one.
struct Editor {
    desc: *const LV2UI_Descriptor,
    ui: *mut c_void,
    idle: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    window: u64,
    size: (i32, i32),
    _features: Box<UiFeatures>,
    /// Atom messages from the plug-in on their way to the interface.
    buf: Vec<u8>,
}

// Only ever used on the UI thread.
unsafe impl Send for Editor {}

struct UiFeatures {
    resize: LV2UI_Resize,
    touch: LV2UI_Touch,
    port_map: LV2UI_Port_Map,
    data_access: LV2_Extension_Data_Feature,
    update_rate: f32,
    options: [LV2_Options_Option; 3],
    list: Vec<LV2_Feature>,
    ptrs: Vec<*const LV2_Feature>,
}

const EDITOR_SIZE: (i32, i32) = (640, 400);

/// UI libraries stay loaded for the life of the process: toolkits they link register handlers
/// that crash once their code is unmapped.
fn ui_library(path: &Path) -> Option<usize> {
    static LIBS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    let mut libs = LIBS.get_or_init(Default::default).lock().unwrap();
    if let Some(h) = libs.get(path) {
        return Some(*h);
    }
    let c = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if h.is_null() {
        eprintln!("solstice: {}: {}", path.display(), unsafe { text(libc::dlerror()) });
        return None;
    }
    libs.insert(path.to_path_buf(), h as usize);
    Some(h as usize)
}

unsafe extern "C" fn ui_write(controller: *mut c_void, port: u32, size: u32, protocol: u32, buf: *const c_void) {
    let inst = unsafe { &*(controller as *const Instance) };
    if buf.is_null() {
        return;
    }
    if protocol == 0 {
        if size as usize == std::mem::size_of::<f32>() && inst.is_control_input(port) {
            let v = unsafe { *(buf as *const f32) };
            inst.set_plain(port, v);
            // The interface knows what it set; don't echo it back.
            inst.ui_sent[port as usize].store(v.to_bits(), Relaxed);
            inst.handle.dirty.store(true, Relaxed);
        }
    } else if protocol == inst.event_transfer && size >= 8 {
        let data = unsafe { std::slice::from_raw_parts(buf as *const u8, size as usize) };
        if push_msg(&mut inst.to_atoms.lock().unwrap(), port, data) {
            inst.handle.dirty.store(true, Relaxed);
        }
    }
}

unsafe extern "C" fn ui_resize(handle: *mut c_void, width: c_int, height: c_int) -> c_int {
    let inst = unsafe { &*(handle as *const Instance) };
    let (w, h) = (width.max(1) as u64, height.max(1) as u64);
    inst.resize.store(((w << 32) | h) + 1, Relaxed);
    0
}

unsafe extern "C" fn ui_touch(handle: *mut c_void, port: u32, grabbed: bool) {
    let inst = unsafe { &*(handle as *const Instance) };
    let mut held = inst.held.lock().unwrap();
    held.retain(|p| *p != port);
    if grabbed {
        held.push(port);
    }
}

unsafe extern "C" fn ui_port_index(handle: *mut c_void, symbol: *const c_char) -> u32 {
    let inst = unsafe { &*(handle as *const Instance) };
    if symbol.is_null() {
        return LV2UI_INVALID_PORT_INDEX;
    }
    let symbol = unsafe { CStr::from_ptr(symbol) };
    inst.ports.iter().find(|p| p.symbol.as_c_str() == symbol).map_or(LV2UI_INVALID_PORT_INDEX, |p| p.index)
}

/// How a plug-in's settings are saved: lilv's state when the plug-in has state of its own,
/// otherwise just its control values by port symbol.
#[derive(Serialize, Deserialize)]
#[serde(tag = "format")]
enum Saved {
    #[serde(rename = "lv2-state")]
    State { ttl: String },
    #[serde(rename = "lv2-controls")]
    Controls { controls: BTreeMap<String, f32> },
}

const STATE_URI: &CStr = c"urn:solstice:state";

pub struct Instance {
    handle: Arc<Handle>,
    pub info: ClassInfo,
    ports: Vec<PortInfo>,
    params: Vec<Param>,
    /// Control input changes for the audio thread, as (port, plain value).
    to_proc: Mutex<rtrb::Producer<(u32, f32)>>,
    /// Atoms the interface sends to the plug-in's atom inputs, tagged with the port.
    to_atoms: Mutex<rtrb::Producer<u8>>,
    /// Atoms the plug-in sends to the interface.
    from_atoms: Mutex<rtrb::Consumer<u8>>,
    proc_ends: Mutex<Option<ProcEnds>>,
    held: Mutex<Vec<u32>>,
    latency_port: Option<u32>,
    has_state: bool,
    ui: Option<UiInfo>,
    editor: Mutex<Option<Editor>>,
    /// The values each control port's interface was last told, as f32 bits.
    ui_sent: Box<[AtomicU32]>,
    /// A size the interface asked for: ((width << 32) | height) + 1, or 0 for none.
    resize: AtomicU64,
    event_transfer: u32,
    max_block: usize,
}

struct ProcEnds {
    ports: Ports,
    params: rtrb::Consumer<(u32, f32)>,
    ui_in: rtrb::Consumer<u8>,
    ui_out: rtrb::Producer<u8>,
    responses: rtrb::Consumer<u8>,
}

impl Instance {
    pub fn open(info: &ClassInfo, sample_rate: f64, max_block: usize) -> Result<Arc<Instance>, String> {
        let uri = info.uid.strip_prefix(PREFIX).unwrap_or(&info.uid);
        let (req_tx, req_rx) = rtrb::RingBuffer::new(RING);
        let features = Features::new(sample_rate, max_block, req_tx);
        let (instance, plugin, ports, params, latency_port, has_state, ui) = {
            let mut w = world();
            w.load_bundle(&info.path);
            let plugin = match w.plugin(uri) {
                Some(p) => p,
                None => {
                    w.load_all();
                    w.plugin(uri).ok_or("the plug-in is not installed")?
                }
            };
            if let Some(f) = missing_feature(plugin) {
                return Err(format!("the plug-in needs {f}, which Solstice does not provide"));
            }
            let ports = ports(&mut w, plugin);
            let params = params(&mut w, plugin, &ports, sample_rate as f32);
            let latency_port = unsafe { lilv_plugin_has_latency(plugin).then(|| lilv_plugin_get_latency_port_index(plugin)) };
            let has_state = unsafe { lilv_plugin_has_extension_data(plugin, w.node(STATE_INTERFACE)) };
            let ui = x11_ui(&mut w, plugin);
            let instance = unsafe { lilv_plugin_instantiate(plugin, sample_rate, features.ptrs.as_ptr()) };
            if instance.is_null() {
                return Err("the plug-in could not be created".into());
            }
            (instance, plugin, ports, params, latency_port, has_state, ui)
        };
        let values: Box<[AtomicU32]> = ports
            .iter()
            .map(|p| {
                let v = params.iter().find(|q| q.port == p.index).map_or(0.0, |q| q.default);
                AtomicU32::new(v.to_bits())
            })
            .collect();
        let mut handle = Handle {
            instance,
            plugin,
            features,
            job: None,
            values,
            resync: AtomicBool::new(false),
            reconnect: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            ui_open: AtomicBool::new(false),
        };
        let u = Uris::new();
        let mut proc_ports = Ports::new(&ports, &handle.values, max_block);
        proc_ports.connect(&handle, &ports, &u);
        if let Some(f) = handle.desc().activate {
            unsafe { f(handle.lv2()) };
        }
        let (resp_tx, resp_rx) = rtrb::RingBuffer::new(RING);
        if let Some(iface) = handle.ext::<LV2_Worker_Interface>(WORKER_INTERFACE) {
            handle.job = Some(add_job(Job { lv2: handle.lv2(), iface, requests: req_rx, responses: resp_tx }));
        }
        if let Some(port) = latency_port {
            handle.measure_latency(&ports, port, max_block);
        }
        let atom_out = ports.iter().any(|p| p.kind == Kind::Atom && !p.input);
        let (tx, rx) = rtrb::RingBuffer::new(1024);
        let (atx, arx) = rtrb::RingBuffer::new(RING);
        let (utx, urx) = rtrb::RingBuffer::new(if atom_out { 1 << 20 } else { 16 });
        let ui_sent = ports.iter().map(|_| AtomicU32::new(f32::NAN.to_bits())).collect();
        Ok(Arc::new(Instance {
            handle: Arc::new(handle),
            info: info.clone(),
            ports,
            params,
            to_proc: Mutex::new(tx),
            to_atoms: Mutex::new(atx),
            from_atoms: Mutex::new(urx),
            proc_ends: Mutex::new(Some(ProcEnds { ports: proc_ports, params: rx, ui_in: arx, ui_out: utx, responses: resp_rx })),
            held: Mutex::default(),
            latency_port,
            has_state,
            ui,
            editor: Mutex::default(),
            ui_sent,
            resize: AtomicU64::new(0),
            event_transfer: urid(EVENT_TRANSFER),
            max_block,
        }))
    }

    /// The engine side. There is one per instance; a second call returns a silent stand-in.
    pub fn processor(&self) -> Box<dyn Plugin> {
        match self.proc_ends.lock().unwrap().take() {
            Some(ends) => {
                let work = self.handle.job.as_ref().and_then(|_| self.handle.ext::<LV2_Worker_Interface>(WORKER_INTERFACE));
                Box::new(Processor {
                    handle: self.handle.clone(),
                    ports: ends.ports,
                    port_info: self.ports.clone(),
                    params: ends.params,
                    ui_in: ends.ui_in,
                    ui_out: ends.ui_out,
                    responses: ends.responses,
                    work_response: work.and_then(|w| w.work_response),
                    end_run: work.and_then(|w| w.end_run),
                    automation: self.params.iter().filter(|p| !p.output).cloned().collect(),
                    scratch: Vec::with_capacity(RING),
                    uris: Uris::new(),
                    keyed: true,
                    max_block: self.max_block,
                })
            }
            None => Box::new(Silent),
        }
    }

    pub fn latency(&self) -> u32 {
        self.latency_port.map_or(0, |p| self.handle.value(p).max(0.0) as u32)
    }

    /// Whether the plug-in has an audio input to key.
    pub fn has_sidechain(&self) -> bool {
        !layout(&self.ports).keys.is_empty()
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        self.params
            .iter()
            .map(|p| ParamInfo {
                id: p.port,
                title: p.title.clone(),
                units: p.units.clone(),
                default_value: p.norm(p.default),
                steps: p.steps(),
                read_only: p.output,
                hidden: p.hidden,
            })
            .collect()
    }

    fn param_of(&self, id: u32) -> Option<&Param> {
        self.params.iter().find(|p| p.port == id)
    }

    fn is_control_input(&self, port: u32) -> bool {
        self.param_of(port).is_some_and(|p| !p.output)
    }

    /// Normalised 0..1.
    pub fn param(&self, id: u32) -> f64 {
        self.param_of(id).map_or(0.0, |p| p.norm(self.handle.value(id)))
    }

    pub fn set_param(&self, id: u32, value: f64) {
        if let Some(p) = self.param_of(id).filter(|p| !p.output) {
            self.set_plain(id, p.plain(value));
        }
    }

    fn set_plain(&self, port: u32, v: f32) {
        if let Some(slot) = self.handle.values.get(port as usize) {
            slot.store(v.to_bits(), Relaxed);
            if self.to_proc.lock().unwrap().push((port, v)).is_err() {
                self.handle.resync.store(true, Release);
            }
        }
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        self.param_of(id).map_or_else(String::new, |p| p.text(p.plain(value)))
    }

    pub fn state(&self) -> Vec<u8> {
        let saved = if self.has_state {
            match self.lilv_state() {
                Some(ttl) => Saved::State { ttl },
                None => return Vec::new(),
            }
        } else {
            Saved::Controls { controls: self.controls() }
        };
        serde_json::to_vec(&saved).unwrap_or_default()
    }

    fn controls(&self) -> BTreeMap<String, f32> {
        self.ports
            .iter()
            .filter(|p| p.kind == Kind::Control && p.input)
            .map(|p| (p.symbol.to_string_lossy().into_owned(), self.handle.value(p.index)))
            .collect()
    }

    fn port_of(&self, symbol: &CStr) -> Option<u32> {
        self.ports.iter().find(|p| p.kind == Kind::Control && p.input && p.symbol.as_c_str() == symbol).map(|p| p.index)
    }

    fn lilv_state(&self) -> Option<String> {
        struct Get<'a> {
            inst: &'a Instance,
            value: f32,
            float: u32,
        }
        unsafe extern "C" fn get_value(symbol: *const c_char, user: *mut c_void, size: *mut u32, type_: *mut u32) -> *const c_void {
            let g = unsafe { &mut *(user as *mut Get) };
            let Some(port) = g.inst.port_of(unsafe { CStr::from_ptr(symbol) }) else { return std::ptr::null() };
            g.value = g.inst.handle.value(port);
            unsafe {
                *size = std::mem::size_of::<f32>() as u32;
                *type_ = g.float;
            }
            &g.value as *const f32 as *const c_void
        }
        let mut get = Get { inst: self, value: 0.0, float: urid(ATOM_FLOAT) };
        let h = &self.handle;
        let map = &h.features.map as *const LV2_URID_Map as *mut LV2_URID_Map;
        let unmap = &h.features.unmap as *const LV2_URID_Unmap as *mut LV2_URID_Unmap;
        let w = world();
        let state = unsafe {
            lilv_state_new_from_instance(
                h.plugin,
                h.instance,
                map,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                Some(get_value),
                &mut get as *mut Get as *mut c_void,
                LV2_STATE_IS_POD | LV2_STATE_IS_PORTABLE,
                h.features.ptrs.as_ptr(),
            )
        };
        if state.is_null() {
            return None;
        }
        let s = unsafe { lilv_state_to_string(w.ptr, map, unmap, state, STATE_URI.as_ptr(), std::ptr::null()) };
        unsafe { lilv_state_free(state) };
        if s.is_null() {
            return None;
        }
        let ttl = unsafe { text(s) };
        unsafe { lilv_free(s as *mut c_void) };
        Some(ttl)
    }

    pub fn set_state(&self, data: &[u8]) -> bool {
        self.clear_held();
        let Ok(saved) = serde_json::from_slice::<Saved>(data) else { return false };
        let ok = match saved {
            Saved::Controls { controls } => {
                for (symbol, v) in controls {
                    if let Some(port) = CString::new(symbol).ok().and_then(|s| self.port_of(&s)) {
                        self.handle.values[port as usize].store(v.to_bits(), Relaxed);
                    }
                }
                true
            }
            Saved::State { ttl } => self.restore(&ttl),
        };
        self.handle.resync.store(true, Release);
        // The latency can follow the settings, such as a limiter's lookahead.
        if let Some(port) = self.latency_port {
            self.handle.exclusive(|| self.handle.measure_latency(&self.ports, port, self.max_block));
        }
        // Loading isn't an edit.
        self.handle.dirty.store(false, Relaxed);
        ok
    }

    fn restore(&self, ttl: &str) -> bool {
        struct Set<'a> {
            inst: &'a Instance,
            types: [u32; 3],
        }
        unsafe extern "C" fn set_value(symbol: *const c_char, user: *mut c_void, value: *const c_void, size: u32, type_: u32) {
            let s = unsafe { &*(user as *const Set) };
            let Some(port) = s.inst.port_of(unsafe { CStr::from_ptr(symbol) }) else { return };
            let v = match (type_, size) {
                (t, 4) if t == s.types[0] => unsafe { *(value as *const f32) },
                (t, 8) if t == s.types[1] => unsafe { *(value as *const f64) as f32 },
                (t, 4) if t == s.types[2] => unsafe { *(value as *const i32) as f32 },
                _ => return,
            };
            s.inst.handle.values[port as usize].store(v.to_bits(), Relaxed);
        }
        let Ok(ttl) = CString::new(ttl) else { return false };
        let h = &self.handle;
        let map = &h.features.map as *const LV2_URID_Map as *mut LV2_URID_Map;
        let set = Set { inst: self, types: [urid(ATOM_FLOAT), urid(ATOM_DOUBLE), urid(ATOM_INT)] };
        let w = world();
        let state = unsafe { lilv_state_new_from_string(w.ptr, map, ttl.as_ptr()) };
        if state.is_null() {
            return false;
        }
        h.exclusive(|| unsafe {
            lilv_state_restore(state, h.instance, Some(set_value), &set as *const Set as *mut c_void, 0, h.features.ptrs.as_ptr())
        });
        unsafe { lilv_state_free(state) };
        true
    }

    /// Whether the plug-in's interface changed its settings since last asked.
    pub fn take_dirty(&self) -> bool {
        self.handle.dirty.swap(false, Relaxed)
    }

    /// Parameters the user is holding in the plug-in's interface.
    pub fn held(&self) -> Vec<u32> {
        self.held.lock().unwrap().clone()
    }

    /// Lets go of every held parameter, for when the end of a gesture may never arrive.
    pub fn clear_held(&self) {
        self.held.lock().unwrap().clear();
    }

    /// Whether the plug-in has an X11 interface.
    pub fn has_editor(&self) -> bool {
        self.ui.is_some()
    }

    /// X11 interfaces always take a parent window.
    pub fn can_embed(&self) -> bool {
        self.ui.is_some()
    }

    pub fn editor_open(&self) -> bool {
        self.editor.lock().unwrap().is_some()
    }

    /// Opens the interface in a top-level window of its own.
    pub fn open_editor(&self, title: &str) -> bool {
        if self.editor_open() {
            return true;
        }
        if self.ui.is_none() {
            return false;
        }
        let title = CString::new(title).unwrap_or_default();
        let window = solstice_vst3::x11::create_window(EDITOR_SIZE.0, EDITOR_SIZE.1, &title);
        self.show_in(window)
    }

    /// Opens the interface inside the X11 window `parent`, at a position in its pixels.
    pub fn open_editor_in(&self, parent: u64, x: i32, y: i32) -> bool {
        if self.editor_open() {
            return true;
        }
        if self.ui.is_none() || parent == 0 {
            return false;
        }
        self.show_in(solstice_vst3::x11::create_child(parent, x, y, EDITOR_SIZE.0, EDITOR_SIZE.1))
    }

    fn show_in(&self, window: u64) -> bool {
        if window == 0 {
            return false;
        }
        match self.instantiate_ui(window) {
            Some(e) => {
                solstice_vst3::x11::resize(window, e.size.0, e.size.1);
                solstice_vst3::x11::map(window);
                *self.editor.lock().unwrap() = Some(e);
                self.handle.ui_open.store(true, Relaxed);
                true
            }
            None => {
                solstice_vst3::x11::destroy(window);
                false
            }
        }
    }

    fn instantiate_ui(&self, window: u64) -> Option<Editor> {
        let info = self.ui.as_ref()?;
        let lib = ui_library(&info.binary)? as *mut c_void;
        let sym = unsafe { libc::dlsym(lib, c"lv2ui_descriptor".as_ptr()) };
        if sym.is_null() {
            return None;
        }
        let descriptor: LV2UI_DescriptorFunction = unsafe { std::mem::transmute(sym) };
        let desc = (0..)
            .map(|i| unsafe { descriptor(i) })
            .take_while(|d| !d.is_null())
            .find(|d| unsafe { CStr::from_ptr((**d).uri) } == info.uri.as_c_str())?;
        let me = self as *const Instance as *mut c_void;
        let h = &self.handle;
        let mut f = Box::new(UiFeatures {
            resize: LV2UI_Resize { handle: me, ui_resize },
            touch: LV2UI_Touch { handle: me, touch: ui_touch },
            port_map: LV2UI_Port_Map { handle: me, port_index: ui_port_index },
            data_access: LV2_Extension_Data_Feature { data_access: h.desc().extension_data },
            update_rate: 30.0,
            options: unsafe { std::mem::zeroed() },
            list: Vec::new(),
            ptrs: Vec::new(),
        });
        f.options = [
            option(UI_UPDATE_RATE, ATOM_FLOAT, 4, &f.update_rate as *const f32 as *const c_void),
            option(PARAM_SAMPLE_RATE, ATOM_FLOAT, 4, &h.features.sample_rate as *const f32 as *const c_void),
            unsafe { std::mem::zeroed() },
        ];
        let ptr = |x: *const c_void| x as *mut c_void;
        f.list = vec![
            LV2_Feature { uri: UI_PARENT.as_ptr(), data: window as usize as *mut c_void },
            LV2_Feature { uri: UI_RESIZE.as_ptr(), data: ptr(&f.resize as *const _ as *const c_void) },
            LV2_Feature { uri: UI_TOUCH.as_ptr(), data: ptr(&f.touch as *const _ as *const c_void) },
            LV2_Feature { uri: UI_PORT_MAP.as_ptr(), data: ptr(&f.port_map as *const _ as *const c_void) },
            LV2_Feature { uri: UI_IDLE.as_ptr(), data: std::ptr::null_mut() },
            LV2_Feature { uri: INSTANCE_ACCESS.as_ptr(), data: h.lv2() },
            LV2_Feature { uri: DATA_ACCESS.as_ptr(), data: ptr(&f.data_access as *const _ as *const c_void) },
            LV2_Feature { uri: OPTIONS.as_ptr(), data: ptr(f.options.as_ptr() as *const c_void) },
            LV2_Feature { uri: URID_MAP.as_ptr(), data: ptr(&h.features.map as *const _ as *const c_void) },
            LV2_Feature { uri: URID_UNMAP.as_ptr(), data: ptr(&h.features.unmap as *const _ as *const c_void) },
            LV2_Feature { uri: LOG.as_ptr(), data: ptr(&h.features.log as *const _ as *const c_void) },
        ];
        f.ptrs = f.list.iter().map(|x| x as *const LV2_Feature).chain([std::ptr::null()]).collect();
        self.resize.store(0, Relaxed);
        for s in self.ui_sent.iter() {
            s.store(f32::NAN.to_bits(), Relaxed);
        }
        let plugin_uri = CString::new(self.info.uid.strip_prefix(PREFIX).unwrap_or(&self.info.uid)).ok()?;
        let mut widget = std::ptr::null_mut();
        let instantiate = unsafe { (*desc).instantiate }?;
        let ui = unsafe { instantiate(desc, plugin_uri.as_ptr(), info.bundle.as_ptr(), ui_write, me, &mut widget, f.ptrs.as_ptr()) };
        if ui.is_null() {
            return None;
        }
        let idle = unsafe { (*desc).extension_data }
            .map(|x| unsafe { x(UI_IDLE.as_ptr()) } as *const LV2UI_Idle_Interface)
            .filter(|p| !p.is_null())
            .and_then(|p| unsafe { (*p).idle });
        let r = self.resize.swap(0, Relaxed);
        let size = if r != 0 { (((r - 1) >> 32) as i32, ((r - 1) & 0xffff_ffff) as i32) } else { EDITOR_SIZE };
        let mut e = Editor { desc, ui, idle, window, size, _features: f, buf: Vec::with_capacity(1 << 20) };
        self.update_ui(&mut e);
        Some(e)
    }

    /// Tells the interface about control values that changed and atoms the plug-in sent.
    fn update_ui(&self, e: &mut Editor) {
        let Some(port_event) = (unsafe { (*e.desc).port_event }) else {
            while pop_msg(&mut self.from_atoms.lock().unwrap(), &mut e.buf).is_some() {}
            return;
        };
        for p in self.ports.iter().filter(|p| p.kind == Kind::Control) {
            let v = self.handle.value(p.index);
            let sent = &self.ui_sent[p.index as usize];
            if sent.swap(v.to_bits(), Relaxed) != v.to_bits() {
                unsafe { port_event(e.ui, p.index, 4, 0, &v as *const f32 as *const c_void) };
            }
        }
        loop {
            let Some(port) = pop_msg(&mut self.from_atoms.lock().unwrap(), &mut e.buf) else { break };
            if e.buf.len() >= 8 {
                unsafe { port_event(e.ui, port, e.buf.len() as u32, self.event_transfer, e.buf.as_ptr() as *const c_void) };
            }
        }
    }

    /// The interface's size in pixels, which it may change while open.
    pub fn editor_size(&self) -> (i32, i32) {
        self.editor.lock().unwrap().as_ref().map_or((0, 0), |e| e.size)
    }

    pub fn move_editor(&self, x: i32, y: i32) {
        if let Some(e) = self.editor.lock().unwrap().as_ref() {
            solstice_vst3::x11::move_to(e.window, x, y);
        }
    }

    pub fn close_editor(&self) {
        self.clear_held();
        let Some(e) = self.editor.lock().unwrap().take() else { return };
        self.handle.ui_open.store(false, Relaxed);
        if let Some(cleanup) = unsafe { (*e.desc).cleanup } {
            unsafe { cleanup(e.ui) };
        }
        solstice_vst3::x11::destroy(e.window);
        let mut buf = Vec::new();
        while pop_msg(&mut self.from_atoms.lock().unwrap(), &mut buf).is_some() {}
    }

    /// Main-thread housekeeping: drives the interface and passes it what changed. Call every
    /// frame.
    pub fn idle(&self) {
        let closed = {
            let mut editor = self.editor.lock().unwrap();
            let Some(e) = editor.as_mut() else { return };
            let closed = e.idle.is_some_and(|f| unsafe { f(e.ui) } != 0);
            if !closed {
                self.update_ui(e);
                let r = self.resize.swap(0, Relaxed);
                if r != 0 {
                    e.size = (((r - 1) >> 32) as i32, ((r - 1) & 0xffff_ffff) as i32);
                    solstice_vst3::x11::resize(e.window, e.size.0, e.size.1);
                }
            }
            closed
        };
        if closed {
            self.close_editor();
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.close_editor();
    }
}

/// Stands in when a processor has already been handed out.
struct Silent;

impl Plugin for Silent {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], _: &[MidiEvent], _: &ProcessContext) {
        l.fill(0.0);
        r.fill(0.0);
    }
}

fn midi_len(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 2,
        0xf0 => match status {
            0xf1 | 0xf3 => 2,
            0xf2 => 3,
            _ => 1,
        },
        _ => 3,
    }
}

/// The audio-thread side: one `run` per period.
pub struct Processor {
    handle: Arc<Handle>,
    ports: Ports,
    port_info: Vec<PortInfo>,
    params: rtrb::Consumer<(u32, f32)>,
    ui_in: rtrb::Consumer<u8>,
    ui_out: rtrb::Producer<u8>,
    responses: rtrb::Consumer<u8>,
    work_response: Option<unsafe extern "C" fn(LV2_Handle, u32, *const c_void) -> u32>,
    end_run: Option<unsafe extern "C" fn(LV2_Handle) -> u32>,
    /// Control inputs, for automation's 0..1 values.
    automation: Vec<Param>,
    scratch: Vec<u8>,
    uris: Uris,
    /// The side-chain inputs may hold a key; cleared once there is none.
    keyed: bool,
    max_block: usize,
}

unsafe impl Send for Processor {}

impl Plugin for Processor {
    fn process(&mut self, l: &mut [f32], r: &mut [f32], midi: &[MidiEvent], ctx: &ProcessContext) {
        // A state restore is under way: this one period is silent.
        if self.handle.busy.swap(true, Acquire) {
            l.fill(0.0);
            r.fill(0.0);
            return;
        }
        AUDIO.set(true);
        self.run(l, r, midi, ctx);
        AUDIO.set(false);
        self.handle.busy.store(false, Release);
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(p) = self.automation.iter().find(|p| p.port == id) {
            let v = p.plain(value);
            self.ports.controls[id as usize] = v;
            self.handle.values[id as usize].store(v.to_bits(), Relaxed);
        }
    }

    fn sidechain(&mut self, key: Option<[&[f32]; 2]>) {
        match key {
            Some(key) => {
                for (i, k) in self.ports.keys.iter().enumerate() {
                    let src = key[i.min(1)];
                    let n = src.len().min(self.max_block);
                    self.ports.audio[*k].1[..n].copy_from_slice(&src[..n]);
                }
                self.keyed = true;
            }
            None if self.keyed => {
                for k in &self.ports.keys {
                    self.ports.audio[*k].1.fill(0.0);
                }
                self.keyed = false;
            }
            None => {}
        }
    }
}

impl Processor {
    fn run(&mut self, l: &mut [f32], r: &mut [f32], midi: &[MidiEvent], _ctx: &ProcessContext) {
        let n = l.len().min(r.len()).min(self.max_block);
        if n == 0 {
            return;
        }
        let h = &*self.handle;
        let lv2 = h.lv2();
        if h.reconnect.swap(false, Acquire) {
            self.ports.connect(h, &self.port_info, &self.uris);
        }
        while let Ok((port, v)) = self.params.pop() {
            self.ports.controls[port as usize] = v;
        }
        // After the queue, which may hold changes from before a restore or miss ones it had no
        // room for: `values` always has the latest.
        if h.resync.swap(false, Acquire) {
            for p in &self.ports.control_inputs {
                self.ports.controls[*p as usize] = h.value(*p);
            }
        }
        for (i, src) in self.ports.inputs.iter().zip([&*l, &*r]) {
            self.ports.audio[*i].1[..n].copy_from_slice(&src[..n]);
        }
        for a in &mut self.ports.atoms {
            a.reset(&self.uris);
        }
        while let Some(port) = pop_msg(&mut self.ui_in, &mut self.scratch) {
            if let Some(a) = self.ports.atoms.iter_mut().find(|a| a.port == port && a.input)
                && self.scratch.len() >= 8
            {
                let type_ = u32::from_ne_bytes([self.scratch[4], self.scratch[5], self.scratch[6], self.scratch[7]]);
                a.append(0, type_, &self.scratch[8..]);
            }
        }
        if let Some(a) = self.ports.atoms.iter_mut().find(|a| a.midi) {
            for m in midi.iter().filter(|m| (m.frame as usize) < n) {
                a.append(m.frame as i64, self.uris.midi, &m.data[..midi_len(m.data[0])]);
            }
        }
        if let Some(respond) = self.work_response {
            while pop_msg(&mut self.responses, &mut self.scratch).is_some() {
                unsafe { respond(lv2, self.scratch.len() as u32, self.scratch.as_ptr() as *const c_void) };
            }
        }
        let connect = h.desc().connect_port;
        let outs = [l.as_mut_ptr(), r.as_mut_ptr()];
        if let Some(connect) = connect {
            for (port, buf) in self.ports.outputs.iter().zip(outs) {
                unsafe { connect(lv2, *port, buf as *mut c_void) };
            }
        }
        if let Some(run) = h.desc().run {
            unsafe { run(lv2, n as u32) };
        }
        if let Some(end) = self.end_run {
            unsafe { end(lv2) };
        }
        for p in &self.ports.control_outputs {
            h.values[*p as usize].store(self.ports.controls[*p as usize].to_bits(), Relaxed);
        }
        if h.ui_open.load(Relaxed) {
            let (sequence, q) = (self.uris.sequence, &mut self.ui_out);
            for a in self.ports.atoms.iter_mut().filter(|a| !a.input) {
                let port = a.port;
                a.events(sequence, |atom| {
                    push_msg(q, port, atom);
                });
            }
        }
        match self.ports.outputs.len() {
            0 => {
                l[..n].fill(0.0);
                r[..n].fill(0.0);
            }
            1 => r[..n].copy_from_slice(&l[..n]),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::{AllocDisabler, assert_no_alloc};

    #[global_allocator]
    static ALLOC: AllocDisabler = AllocDisabler;

    const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

    fn param() -> Param {
        Param {
            port: 3,
            title: "Gain".into(),
            units: String::new(),
            min: 0.0,
            max: 10.0,
            default: 1.0,
            log: false,
            integer: false,
            toggled: false,
            enumeration: false,
            points: Vec::new(),
            hidden: false,
            output: false,
        }
    }

    #[test]
    fn control_ports_normalise() {
        let lin = param();
        assert_eq!(lin.plain(0.25), 2.5);
        assert_eq!(lin.norm(2.5), 0.25);
        assert_eq!(lin.steps(), 0);

        let log = Param { min: 20.0, max: 20000.0, log: true, ..param() };
        assert!((log.plain(0.5) - 632.4555).abs() < 0.01);
        assert!((log.norm(632.4555) - 0.5).abs() < 1e-6);

        let int = Param { min: 1.0, max: 4.0, integer: true, ..param() };
        assert_eq!(int.steps(), 3);
        assert_eq!(int.plain(0.4), 2.0);

        let toggle = Param { min: 0.0, max: 1.0, toggled: true, ..param() };
        assert_eq!((toggle.steps(), toggle.plain(0.7), toggle.text(1.0)), (1, 1.0, "On".to_string()));

        let points = vec![(0.0, "Peak".to_string()), (1.0, "RMS".to_string()), (4.0, "Low-pass".to_string())];
        let menu = Param { min: 0.0, max: 4.0, enumeration: true, points, ..param() };
        assert_eq!(menu.steps(), 2);
        assert_eq!(menu.plain(1.0), 4.0);
        assert_eq!(menu.norm(1.0), 0.5);
        assert_eq!(menu.text(menu.plain(0.5)), "RMS");
    }

    #[test]
    fn classes_map_to_categories() {
        let audio = |index, input, sidechain| PortInfo {
            index,
            kind: Kind::Audio,
            input,
            symbol: CString::default(),
            sidechain,
            midi: false,
            size: 0,
        };
        let comp = [audio(0, true, false), audio(1, true, false), audio(2, true, true), audio(3, false, false), audio(4, false, false)];
        let types = vec![format!("{LV2}CompressorPlugin"), format!("{LV2}Plugin")];
        assert_eq!(subcategories(&types, &comp), "Fx|Dynamics|Stereo");
        let l = layout(&comp);
        assert_eq!((l.inputs, l.outputs, l.keys), (vec![0, 1], vec![3, 4], vec![2]));

        let midi = PortInfo { kind: Kind::Atom, midi: true, ..audio(0, true, false) };
        assert_eq!(subcategories(&[], &[midi, audio(1, false, false)]), "Instrument|Mono");
        assert_eq!(unit_symbol("http://lv2plug.in/ns/extensions/units#db"), "dB");
    }

    #[test]
    fn rings_carry_whole_messages() {
        let (mut tx, mut rx) = rtrb::RingBuffer::new(64);
        assert!(push_msg(&mut tx, 7, b"hello"));
        assert!(push_msg(&mut tx, 8, &[1; 40]));
        assert!(!push_msg(&mut tx, 9, &[0; 40]), "a message that doesn't fit is refused whole");
        let mut buf = Vec::with_capacity(16);
        assert_eq!(pop_msg(&mut rx, &mut buf), Some(7));
        assert_eq!(buf, b"hello");
        assert_eq!(pop_msg(&mut rx, &mut buf), Some(8));
        assert!(buf.is_empty(), "too long for the buffer, so dropped");
        assert_eq!(pop_msg(&mut rx, &mut buf), None);
    }

    #[test]
    fn atom_sequences_round_trip() {
        let u = Uris { sequence: 1, chunk: 2, midi: 3 };
        let mut a = AtomBuf { port: 0, input: true, midi: true, buf: vec![0; 8] };
        a.reset(&u);
        assert!(a.append(5, u.midi, &[0x90, 60, 100]));
        assert!(!a.append(6, u.midi, &[0; 40]), "no room left");
        let mut seen = Vec::new();
        a.events(u.sequence, |atom| seen.push(atom.to_vec()));
        assert_eq!(seen.len(), 1);
        assert_eq!(&seen[0][..8], [3u32.to_ne_bytes(), u.midi.to_ne_bytes()].concat());
        assert_eq!(&seen[0][8..], [0x90, 60, 100]);
        assert_eq!((midi_len(0x90), midi_len(0xc0), midi_len(0xf8)), (3, 2, 1));
    }

    fn bundle(name: &str) -> Option<PathBuf> {
        let found = search_path().split(':').map(|d| Path::new(d).join(name)).find(|p| p.is_dir());
        if found.is_none() {
            eprintln!("skipped: {name} is not installed");
        }
        found
    }

    fn open(bundle: &Path, uri: &str) -> Arc<Instance> {
        let info = ClassInfo { uid: format!("{PREFIX}{uri}"), name: uri.into(), vendor: String::new(), subcategories: String::new(), path: bundle.into() };
        Instance::open(&info, 48000.0, 512).unwrap()
    }

    const MDA: &str = "http://drobilla.net/plugins/mda/";
    const LSP: &str = "http://lsp-plug.in/plugins/lv2/";

    fn sine(i: usize) -> f32 {
        (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.25
    }

    #[test]
    fn scan_finds_installed_plugins() {
        let (Some(_), Some(_)) = (bundle("mda.lv2"), bundle("lsp-plugins.lv2")) else { return };
        let all = scan_all();
        let piano = all.iter().find(|c| c.uid == format!("{PREFIX}{MDA}Piano")).expect("mda Piano");
        assert!(piano.is_instrument());
        assert!(piano.path.ends_with("mda.lv2"));
        let comp = all.iter().find(|c| c.uid == format!("{PREFIX}{LSP}compressor_stereo")).expect("LSP Compressor");
        assert_eq!(comp.subcategories, "Fx|Dynamics|Stereo");
        assert!(!comp.vendor.is_empty());
    }

    #[test]
    fn effects_change_the_signal() {
        let Some(mda) = bundle("mda.lv2") else { return };
        let inst = open(&mda, &format!("{MDA}Overdrive"));
        let drive = inst.params().into_iter().find(|p| p.title == "Drive").unwrap().id;
        inst.set_param(drive, 1.0);
        assert_eq!(inst.param(drive), 1.0);
        let mut proc = inst.processor();
        let (mut l, mut r): (Vec<f32>, Vec<f32>) = ((0..512).map(sine).collect(), (0..512).map(sine).collect());
        let dry = l.clone();
        proc.process(&mut l, &mut r, &[], &CTX);
        assert!(l.iter().zip(&dry).any(|(a, b)| (a - b).abs() > 1e-3));
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }

    #[test]
    fn instruments_play_notes() {
        let Some(mda) = bundle("mda.lv2") else { return };
        let inst = open(&mda, &format!("{MDA}Piano"));
        let mut proc = inst.processor();
        let note = [MidiEvent { frame: 10, data: [0x90, 60, 100] }];
        let mut peak = 0.0f32;
        for block in 0..8 {
            let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
            let events = if block == 0 { &note[..] } else { &[] };
            assert_no_alloc(|| proc.process(&mut l, &mut r, events, &CTX));
            peak = l.iter().chain(&r).fold(peak, |m, v| m.max(v.abs()));
        }
        assert!(peak > 0.01, "silent: {peak}");
    }

    #[test]
    fn state_restores_changed_parameters() {
        let (Some(mda), Some(lsp)) = (bundle("mda.lv2"), bundle("lsp-plugins.lv2")) else { return };
        for (bundle, uri, state) in [(&mda, format!("{MDA}Overdrive"), "lv2-controls"), (&lsp, format!("{LSP}compressor_stereo"), "lv2-state")] {
            let a = open(bundle, &uri);
            let p = a.params().into_iter().find(|p| !p.read_only && !p.hidden && p.steps == 0).unwrap().id;
            a.set_param(p, 0.8);
            let want = a.param(p);
            let saved = a.state();
            assert!(String::from_utf8_lossy(&saved).contains(state), "{uri}");
            let b = open(bundle, &uri);
            assert!((b.param(p) - want).abs() > 1e-3);
            assert!(b.set_state(&saved));
            assert!((b.param(p) - want).abs() < 1e-4, "{uri}");
            assert!(!b.take_dirty());
            let mut proc = b.processor();
            let (mut l, mut r) = (vec![0.0f32; 64], vec![0.0f32; 64]);
            proc.process(&mut l, &mut r, &[], &CTX);
            assert!((b.param(p) - want).abs() < 1e-4);
        }
    }

    #[test]
    fn processing_does_not_allocate() {
        let Some(lsp) = bundle("lsp-plugins.lv2") else { return };
        // The sampler and compressor have workers; the sampler takes MIDI too.
        for uri in ["sampler_stereo", "compressor_stereo", "sc_compressor_stereo"] {
            let inst = open(&lsp, &format!("{LSP}{uri}"));
            let first = inst.params().into_iter().find(|p| !p.read_only).unwrap().id;
            let mut proc = inst.processor();
            proc.set_param(first, 0.5);
            let key = vec![0.1f32; 256];
            let note = [MidiEvent { frame: 0, data: [0x90, 60, 100] }];
            for i in 0..16 {
                inst.set_param(first, (i % 4) as f64 / 4.0);
                let (mut l, mut r) = ((0..256).map(sine).collect::<Vec<_>>(), (0..256).map(sine).collect::<Vec<_>>());
                assert_no_alloc(|| {
                    proc.sidechain(if i % 2 == 0 { Some([&key, &key]) } else { None });
                    proc.process(&mut l, &mut r, &note, &CTX);
                });
                assert!(l.iter().chain(&r).all(|v| v.is_finite()), "{uri}");
            }
        }
    }

    #[test]
    fn reports_latency() {
        let Some(lsp) = bundle("lsp-plugins.lv2") else { return };
        let inst = open(&lsp, &format!("{LSP}sc_limiter_stereo"));
        assert!(inst.latency() > 0);
        assert!(inst.params().iter().any(|p| p.read_only && p.hidden), "the latency port is hidden");
        assert!(inst.has_sidechain());
        assert!(inst.has_editor());
    }

    #[test]
    fn restored_settings_set_the_latency() {
        let Some(lsp) = bundle("lsp-plugins.lv2") else { return };
        let uri = format!("{LSP}sc_limiter_stereo");
        let a = open(&lsp, &uri);
        let lookahead = a.params().into_iter().find(|p| p.title == "Lookahead").unwrap().id;
        let short = a.latency();
        a.set_param(lookahead, 1.0);
        let saved = a.state();
        let b = open(&lsp, &uri);
        assert_eq!(b.latency(), short);
        let mut proc = b.processor();
        assert!(b.set_state(&saved));
        assert!(b.latency() > short, "{} after {short}", b.latency());
        // The processor plays on its own buffers again.
        let (mut l, mut r) = ((0..256).map(sine).collect::<Vec<_>>(), (0..256).map(sine).collect::<Vec<_>>());
        assert_no_alloc(|| proc.process(&mut l, &mut r, &[], &CTX));
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
    }

    #[test]
    fn the_latest_control_value_wins() {
        let Some(mda) = bundle("mda.lv2") else { return };
        let uri = format!("{MDA}Overdrive");
        let render = |mut proc: Box<dyn Plugin>| {
            let (mut l, mut r): (Vec<f32>, Vec<f32>) = ((0..512).map(sine).collect(), (0..512).map(sine).collect());
            proc.process(&mut l, &mut r, &[], &CTX);
            l
        };
        let want = open(&mda, &uri);
        let drive = want.params().into_iter().find(|p| p.title == "Drive").unwrap().id;
        want.set_param(drive, 1.0);
        let saved = want.state();
        let want = render(want.processor());
        // More changes than the queue holds, the last of them dropped.
        let flood = open(&mda, &uri);
        for _ in 0..1100 {
            flood.set_param(drive, 0.0);
        }
        flood.set_param(drive, 1.0);
        assert_eq!(render(flood.processor()), want);
        // A change queued before a restore.
        let restored = open(&mda, &uri);
        let proc = restored.processor();
        restored.set_param(drive, 0.0);
        assert!(restored.set_state(&saved));
        assert_eq!(render(proc), want);
    }

    /// Opens LSP's interface inside a window of ours. Needs an X server, so run it by hand,
    /// for instance under xvfb-run with a private XDG_RUNTIME_DIR.
    #[test]
    #[ignore]
    fn embeds_an_installed_plugins_interface() {
        let lsp = bundle("lsp-plugins.lv2").expect("LSP's LV2 bundle");
        let inst = open(&lsp, &format!("{LSP}compressor_stereo"));
        assert!(inst.can_embed());
        let parent = solstice_vst3::x11::create_window(800, 600, c"Solstice");
        assert_ne!(parent, 0, "no X server");
        assert!(inst.open_editor_in(parent, 10, 20));
        assert!(inst.editor_open());
        let (w, h) = inst.editor_size();
        assert!(w > 0 && h > 0);
        let mut proc = inst.processor();
        for _ in 0..20 {
            let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
            proc.process(&mut l, &mut r, &[], &CTX);
            inst.idle();
            std::thread::sleep(Duration::from_millis(10));
        }
        inst.move_editor(0, 0);
        inst.close_editor();
        assert!(!inst.editor_open());
        solstice_vst3::x11::destroy(parent);
    }
}
