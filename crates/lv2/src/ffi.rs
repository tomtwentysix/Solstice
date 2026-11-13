//! The parts of lilv and the LV2 headers this host uses.

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_void};

pub enum LilvWorld {}
pub enum LilvPlugin {}
pub enum LilvPluginClass {}
pub enum LilvPort {}
pub enum LilvScalePoint {}
pub enum LilvUI {}
pub enum LilvNode {}
pub enum LilvState {}
pub type LilvIter = c_void;
pub type LilvNodes = c_void;
pub type LilvPlugins = c_void;
pub type LilvScalePoints = c_void;
pub type LilvUIs = c_void;

#[repr(C)]
pub struct LilvInstance {
    pub lv2_descriptor: *const LV2_Descriptor,
    pub lv2_handle: LV2_Handle,
    pub pimpl: *mut c_void,
}

pub type LilvGetPortValueFunc = unsafe extern "C" fn(*const c_char, *mut c_void, *mut u32, *mut u32) -> *const c_void;
pub type LilvSetPortValueFunc = unsafe extern "C" fn(*const c_char, *mut c_void, *const c_void, u32, u32);

unsafe extern "C" {
    pub fn lilv_free(ptr: *mut c_void);
    pub fn lilv_file_uri_parse(uri: *const c_char, hostname: *mut *mut c_char) -> *mut c_char;
    pub fn lilv_new_uri(world: *mut LilvWorld, uri: *const c_char) -> *mut LilvNode;
    pub fn lilv_new_file_uri(world: *mut LilvWorld, host: *const c_char, path: *const c_char) -> *mut LilvNode;
    pub fn lilv_new_string(world: *mut LilvWorld, s: *const c_char) -> *mut LilvNode;
    pub fn lilv_node_free(node: *mut LilvNode);
    pub fn lilv_node_as_uri(node: *const LilvNode) -> *const c_char;
    pub fn lilv_node_as_string(node: *const LilvNode) -> *const c_char;
    pub fn lilv_node_as_float(node: *const LilvNode) -> f32;
    pub fn lilv_node_is_uri(node: *const LilvNode) -> bool;

    pub fn lilv_nodes_free(nodes: *mut LilvNodes);
    pub fn lilv_nodes_begin(nodes: *const LilvNodes) -> *mut LilvIter;
    pub fn lilv_nodes_get(nodes: *const LilvNodes, i: *const LilvIter) -> *const LilvNode;
    pub fn lilv_nodes_next(nodes: *const LilvNodes, i: *mut LilvIter) -> *mut LilvIter;
    pub fn lilv_nodes_is_end(nodes: *const LilvNodes, i: *const LilvIter) -> bool;
    pub fn lilv_nodes_get_first(nodes: *const LilvNodes) -> *const LilvNode;

    pub fn lilv_scale_points_free(points: *mut LilvScalePoints);
    pub fn lilv_scale_points_begin(points: *const LilvScalePoints) -> *mut LilvIter;
    pub fn lilv_scale_points_get(points: *const LilvScalePoints, i: *const LilvIter) -> *const LilvScalePoint;
    pub fn lilv_scale_points_next(points: *const LilvScalePoints, i: *mut LilvIter) -> *mut LilvIter;
    pub fn lilv_scale_points_is_end(points: *const LilvScalePoints, i: *const LilvIter) -> bool;
    pub fn lilv_scale_point_get_label(point: *const LilvScalePoint) -> *const LilvNode;
    pub fn lilv_scale_point_get_value(point: *const LilvScalePoint) -> *const LilvNode;

    pub fn lilv_uis_free(uis: *mut LilvUIs);
    pub fn lilv_uis_begin(uis: *const LilvUIs) -> *mut LilvIter;
    pub fn lilv_uis_get(uis: *const LilvUIs, i: *const LilvIter) -> *const LilvUI;
    pub fn lilv_uis_next(uis: *const LilvUIs, i: *mut LilvIter) -> *mut LilvIter;
    pub fn lilv_uis_is_end(uis: *const LilvUIs, i: *const LilvIter) -> bool;

    pub fn lilv_plugins_begin(plugins: *const LilvPlugins) -> *mut LilvIter;
    pub fn lilv_plugins_get(plugins: *const LilvPlugins, i: *const LilvIter) -> *const LilvPlugin;
    pub fn lilv_plugins_next(plugins: *const LilvPlugins, i: *mut LilvIter) -> *mut LilvIter;
    pub fn lilv_plugins_is_end(plugins: *const LilvPlugins, i: *const LilvIter) -> bool;
    pub fn lilv_plugins_get_by_uri(plugins: *const LilvPlugins, uri: *const LilvNode) -> *const LilvPlugin;

    pub fn lilv_world_new() -> *mut LilvWorld;
    pub fn lilv_world_set_option(world: *mut LilvWorld, uri: *const c_char, value: *const LilvNode);
    pub fn lilv_world_free(world: *mut LilvWorld);
    pub fn lilv_world_load_all(world: *mut LilvWorld);
    pub fn lilv_world_load_bundle(world: *mut LilvWorld, bundle_uri: *const LilvNode);
    pub fn lilv_world_get_all_plugins(world: *const LilvWorld) -> *const LilvPlugins;

    pub fn lilv_plugin_get_uri(plugin: *const LilvPlugin) -> *const LilvNode;
    pub fn lilv_plugin_get_bundle_uri(plugin: *const LilvPlugin) -> *const LilvNode;
    pub fn lilv_plugin_get_name(plugin: *const LilvPlugin) -> *mut LilvNode;
    pub fn lilv_plugin_get_value(plugin: *const LilvPlugin, predicate: *const LilvNode) -> *mut LilvNodes;
    pub fn lilv_plugin_get_required_features(plugin: *const LilvPlugin) -> *mut LilvNodes;
    pub fn lilv_plugin_has_extension_data(plugin: *const LilvPlugin, uri: *const LilvNode) -> bool;
    pub fn lilv_plugin_get_num_ports(plugin: *const LilvPlugin) -> u32;
    pub fn lilv_plugin_get_port_ranges_float(plugin: *const LilvPlugin, min: *mut f32, max: *mut f32, def: *mut f32);
    pub fn lilv_plugin_has_latency(plugin: *const LilvPlugin) -> bool;
    pub fn lilv_plugin_get_latency_port_index(plugin: *const LilvPlugin) -> u32;
    pub fn lilv_plugin_get_port_by_index(plugin: *const LilvPlugin, index: u32) -> *const LilvPort;
    pub fn lilv_plugin_get_author_name(plugin: *const LilvPlugin) -> *mut LilvNode;
    pub fn lilv_plugin_instantiate(plugin: *const LilvPlugin, rate: f64, features: *const *const LV2_Feature) -> *mut LilvInstance;
    pub fn lilv_instance_free(instance: *mut LilvInstance);
    pub fn lilv_plugin_get_uis(plugin: *const LilvPlugin) -> *mut LilvUIs;

    pub fn lilv_port_get(plugin: *const LilvPlugin, port: *const LilvPort, predicate: *const LilvNode) -> *mut LilvNode;
    pub fn lilv_port_get_value(plugin: *const LilvPlugin, port: *const LilvPort, predicate: *const LilvNode) -> *mut LilvNodes;
    pub fn lilv_port_has_property(plugin: *const LilvPlugin, port: *const LilvPort, property: *const LilvNode) -> bool;
    pub fn lilv_port_supports_event(plugin: *const LilvPlugin, port: *const LilvPort, event_type: *const LilvNode) -> bool;
    pub fn lilv_port_get_symbol(plugin: *const LilvPlugin, port: *const LilvPort) -> *const LilvNode;
    pub fn lilv_port_get_name(plugin: *const LilvPlugin, port: *const LilvPort) -> *mut LilvNode;
    pub fn lilv_port_is_a(plugin: *const LilvPlugin, port: *const LilvPort, class: *const LilvNode) -> bool;
    pub fn lilv_port_get_scale_points(plugin: *const LilvPlugin, port: *const LilvPort) -> *mut LilvScalePoints;

    pub fn lilv_ui_get_uri(ui: *const LilvUI) -> *const LilvNode;
    pub fn lilv_ui_is_a(ui: *const LilvUI, class: *const LilvNode) -> bool;
    pub fn lilv_ui_get_bundle_uri(ui: *const LilvUI) -> *const LilvNode;
    pub fn lilv_ui_get_binary_uri(ui: *const LilvUI) -> *const LilvNode;

    pub fn lilv_state_new_from_string(world: *mut LilvWorld, map: *mut LV2_URID_Map, s: *const c_char) -> *mut LilvState;
    pub fn lilv_state_new_from_instance(
        plugin: *const LilvPlugin,
        instance: *mut LilvInstance,
        map: *mut LV2_URID_Map,
        scratch_dir: *const c_char,
        copy_dir: *const c_char,
        link_dir: *const c_char,
        save_dir: *const c_char,
        get_value: Option<LilvGetPortValueFunc>,
        user_data: *mut c_void,
        flags: u32,
        features: *const *const LV2_Feature,
    ) -> *mut LilvState;
    pub fn lilv_state_free(state: *mut LilvState);
    pub fn lilv_state_restore(
        state: *const LilvState,
        instance: *mut LilvInstance,
        set_value: Option<LilvSetPortValueFunc>,
        user_data: *mut c_void,
        flags: u32,
        features: *const *const LV2_Feature,
    );
    pub fn lilv_state_to_string(
        world: *mut LilvWorld,
        map: *mut LV2_URID_Map,
        unmap: *mut LV2_URID_Unmap,
        state: *const LilvState,
        uri: *const c_char,
        base_uri: *const c_char,
    ) -> *mut c_char;
}

pub const LILV_OPTION_LV2_PATH: &std::ffi::CStr = c"http://drobilla.net/ns/lilv#lv2-path";

pub type LV2_Handle = *mut c_void;

#[repr(C)]
pub struct LV2_Feature {
    pub uri: *const c_char,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct LV2_Descriptor {
    pub uri: *const c_char,
    pub instantiate: *const c_void,
    pub connect_port: Option<unsafe extern "C" fn(LV2_Handle, u32, *mut c_void)>,
    pub activate: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub run: Option<unsafe extern "C" fn(LV2_Handle, u32)>,
    pub deactivate: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub cleanup: Option<unsafe extern "C" fn(LV2_Handle)>,
    pub extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

#[repr(C)]
pub struct LV2_URID_Map {
    pub handle: *mut c_void,
    pub map: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

#[repr(C)]
pub struct LV2_URID_Unmap {
    pub handle: *mut c_void,
    pub unmap: unsafe extern "C" fn(*mut c_void, u32) -> *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LV2_Atom {
    pub size: u32,
    pub type_: u32,
}

#[repr(C)]
pub struct LV2_Atom_Sequence_Body {
    pub unit: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct LV2_Atom_Sequence {
    pub atom: LV2_Atom,
    pub body: LV2_Atom_Sequence_Body,
}

#[repr(C)]
pub struct LV2_Atom_Event {
    pub frames: i64,
    pub body: LV2_Atom,
}

#[repr(C)]
pub struct LV2_Options_Option {
    pub context: u32,
    pub subject: u32,
    pub key: u32,
    pub size: u32,
    pub type_: u32,
    pub value: *const c_void,
}

pub const LV2_WORKER_SUCCESS: u32 = 0;
pub const LV2_WORKER_ERR_UNKNOWN: u32 = 1;
pub const LV2_WORKER_ERR_NO_SPACE: u32 = 2;

pub type LV2_Worker_Respond_Function = unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32;

#[repr(C)]
pub struct LV2_Worker_Interface {
    pub work: Option<unsafe extern "C" fn(LV2_Handle, LV2_Worker_Respond_Function, *mut c_void, u32, *const c_void) -> u32>,
    pub work_response: Option<unsafe extern "C" fn(LV2_Handle, u32, *const c_void) -> u32>,
    pub end_run: Option<unsafe extern "C" fn(LV2_Handle) -> u32>,
}

#[repr(C)]
pub struct LV2_Worker_Schedule {
    pub handle: *mut c_void,
    pub schedule_work: unsafe extern "C" fn(*mut c_void, u32, *const c_void) -> u32,
}

/// The printf members are variadic, which Rust can't define, so they live in log.c.
#[repr(C)]
pub struct LV2_Log_Log {
    pub handle: *mut c_void,
    pub printf: *const c_void,
    pub vprintf: *const c_void,
}

pub const LV2_STATE_IS_POD: u32 = 1;
pub const LV2_STATE_IS_PORTABLE: u32 = 2;

#[repr(C)]
pub struct LV2_Extension_Data_Feature {
    pub data_access: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

pub type LV2UI_Write_Function = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void);

#[repr(C)]
pub struct LV2UI_Descriptor {
    pub uri: *const c_char,
    pub instantiate: Option<
        unsafe extern "C" fn(
            *const LV2UI_Descriptor,
            *const c_char,
            *const c_char,
            LV2UI_Write_Function,
            *mut c_void,
            *mut *mut c_void,
            *const *const LV2_Feature,
        ) -> *mut c_void,
    >,
    pub cleanup: Option<unsafe extern "C" fn(*mut c_void)>,
    pub port_event: Option<unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_void)>,
    pub extension_data: Option<unsafe extern "C" fn(*const c_char) -> *const c_void>,
}

pub type LV2UI_DescriptorFunction = unsafe extern "C" fn(u32) -> *const LV2UI_Descriptor;

#[repr(C)]
pub struct LV2UI_Resize {
    pub handle: *mut c_void,
    pub ui_resize: unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int,
}

#[repr(C)]
pub struct LV2UI_Idle_Interface {
    pub idle: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

#[repr(C)]
pub struct LV2UI_Touch {
    pub handle: *mut c_void,
    pub touch: unsafe extern "C" fn(*mut c_void, u32, bool),
}

#[repr(C)]
pub struct LV2UI_Port_Map {
    pub handle: *mut c_void,
    pub port_index: unsafe extern "C" fn(*mut c_void, *const c_char) -> u32,
}

pub const LV2UI_INVALID_PORT_INDEX: u32 = u32::MAX;

// Only their addresses are taken, so the va_list argument's exact type doesn't matter.
unsafe extern "C" {
    pub fn solstice_lv2_log_printf(handle: *mut c_void, type_: u32, fmt: *const c_char, ...) -> c_int;
    pub fn solstice_lv2_log_vprintf(handle: *mut c_void, type_: u32, fmt: *const c_char, ap: *mut c_void) -> c_int;
}
