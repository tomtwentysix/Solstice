use solstice_dsp::Native;
use solstice_vst3::{ClassInfo, Instance, ParamInfo};
use std::path::PathBuf;
use std::sync::Arc;

/// A loaded plug-in as the UI sees it: a VST3, CLAP or LV2 instance, or one of Solstice's own.
#[derive(Clone)]
pub enum Host {
    Vst3(Arc<Instance>),
    Clap(Arc<solstice_clap::Instance>),
    Lv2(Arc<solstice_lv2::Instance>),
    Native(Arc<Native>),
}

/// Catalog entries for the built-in plug-ins, listed alongside scanned VST3s.
pub fn builtin_catalog() -> Vec<ClassInfo> {
    solstice_dsp::ALL
        .iter()
        .map(|s| ClassInfo {
            uid: s.id.to_string(),
            name: s.name.to_string(),
            vendor: "Solstice".to_string(),
            subcategories: s.subcategories.to_string(),
            path: PathBuf::new(),
        })
        .collect()
}

impl Host {
    pub fn info(&self) -> ClassInfo {
        match self {
            Host::Vst3(i) => i.info.clone(),
            Host::Clap(i) => i.info.clone(),
            Host::Lv2(i) => i.info.clone(),
            Host::Native(n) => ClassInfo {
                uid: n.spec.id.to_string(),
                name: n.spec.name.to_string(),
                vendor: "Solstice".to_string(),
                subcategories: n.spec.subcategories.to_string(),
                path: PathBuf::new(),
            },
        }
    }

    pub fn latency(&self) -> u32 {
        match self {
            Host::Vst3(i) => i.latency(),
            Host::Clap(i) => i.latency(),
            Host::Lv2(i) => i.latency(),
            Host::Native(n) => n.current_latency(),
        }
    }

    pub fn state(&self) -> Vec<u8> {
        match self {
            Host::Vst3(i) => i.state(),
            Host::Clap(i) => i.state(),
            Host::Lv2(i) => i.state(),
            Host::Native(n) => match (&n.samples, &n.captures, &n.irs) {
                (Some(map), ..) => solstice_dsp::sampler::state(&n.bank, &map.descs()),
                (_, Some(c), _) => c.state(&n.bank),
                (_, _, Some(i)) => i.state(&n.bank),
                _ => n.bank.state(),
            },
        }
    }

    /// Restores saved state; sample instruments load their zones' audio from `audio_dir`,
    /// Neural Amp Modeler its capture and the impulse response loader its response.
    pub fn set_state_in(&self, data: &[u8], audio_dir: &std::path::Path) -> bool {
        match self {
            Host::Vst3(i) => i.set_state(data),
            Host::Clap(i) => i.set_state(data),
            Host::Lv2(i) => i.set_state(data),
            Host::Native(n) => match &n.samples {
                Some(map) => {
                    let descs = solstice_dsp::sampler::set_state(&n.bank, data);
                    map.set(descs.iter().filter_map(|d| crate::samples::load_zone(d, audio_dir, n.sample_rate)).collect());
                    true
                }
                None if n.captures.is_some() => {
                    crate::library::restore::<solstice_dsp::nam::Capture>(n, data, audio_dir);
                    true
                }
                None if n.irs.is_some() => {
                    crate::library::restore::<solstice_dsp::ir::Ir>(n, data, audio_dir);
                    true
                }
                None => n.bank.set_state(data),
            },
        }
    }

    /// Whether both are the same running instance.
    pub fn same(&self, other: &Host) -> bool {
        match (self, other) {
            (Host::Vst3(a), Host::Vst3(b)) => Arc::ptr_eq(a, b),
            (Host::Clap(a), Host::Clap(b)) => Arc::ptr_eq(a, b),
            (Host::Lv2(a), Host::Lv2(b)) => Arc::ptr_eq(a, b),
            (Host::Native(a), Host::Native(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn native(&self) -> Option<&Arc<Native>> {
        match self {
            Host::Native(n) => Some(n),
            Host::Vst3(_) | Host::Clap(_) | Host::Lv2(_) => None,
        }
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        match self {
            Host::Vst3(i) => i.params(),
            Host::Clap(i) => i.params(),
            Host::Lv2(i) => i.params(),
            Host::Native(n) => n
                .bank
                .defs
                .iter()
                .map(|d| ParamInfo {
                    id: d.id,
                    title: d.name.to_string(),
                    units: d.unit.to_string(),
                    default_value: d.to_norm(d.default) as f64,
                    steps: d.steps as i32,
                    read_only: false,
                    hidden: false,
                })
                .collect(),
        }
    }

    /// Normalised 0..1.
    pub fn param(&self, id: u32) -> f64 {
        match self {
            Host::Vst3(i) => i.param(id),
            Host::Clap(i) => i.param(id),
            Host::Lv2(i) => i.param(id),
            Host::Native(n) => n.bank.norm(id) as f64,
        }
    }

    pub fn set_param(&self, id: u32, value: f64) {
        match self {
            Host::Vst3(i) => i.set_param(id, value),
            Host::Clap(i) => i.set_param(id, value),
            Host::Lv2(i) => i.set_param(id, value),
            Host::Native(n) => n.bank.set_norm(id, value as f32),
        }
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        match self {
            Host::Vst3(i) => i.param_text(id, value),
            Host::Clap(i) => i.param_text(id, value),
            Host::Lv2(i) => i.param_text(id, value),
            Host::Native(n) => {
                let plain = n.bank.defs.iter().find(|d| d.id == id).map_or(0.0, |d| d.from_norm(value as f32));
                n.bank.text(id, plain)
            }
        }
    }

    /// Fixed when the plug-in opens. CLAP side-chain ports aren't fed yet.
    pub fn has_sidechain(&self) -> bool {
        match self {
            Host::Vst3(i) => i.has_sidechain(),
            Host::Clap(_) => false,
            Host::Lv2(i) => i.has_sidechain(),
            Host::Native(n) => matches!(n.spec.id, "solstice:compressor" | "solstice:gate"),
        }
    }

    /// Switches a VST3 plug-in's side-chain input on or off; returns whether it changed.
    pub fn set_sidechain_active(&self, on: bool) -> bool {
        match self {
            Host::Vst3(i) => i.set_sidechain_active(on),
            Host::Clap(_) | Host::Lv2(_) | Host::Native(_) => false,
        }
    }

    pub fn has_editor(&self) -> bool {
        match self {
            Host::Vst3(i) => i.has_editor(),
            Host::Clap(i) => i.has_editor(),
            Host::Lv2(i) => i.has_editor(),
            Host::Native(_) => false,
        }
    }

    /// Whether the editor can be shown inside the plug-in window rather than in its own.
    pub fn can_embed(&self) -> bool {
        match self {
            Host::Vst3(i) => i.has_editor(),
            Host::Clap(i) => i.can_embed(),
            Host::Lv2(i) => i.can_embed(),
            Host::Native(_) => false,
        }
    }

    pub fn open_editor(&self) -> bool {
        match self {
            Host::Vst3(i) => i.open_editor(&i.info.name),
            Host::Clap(i) => i.open_editor(&i.info.name),
            Host::Lv2(i) => i.open_editor(&i.info.name),
            Host::Native(_) => false,
        }
    }

    /// The editor inside an X11 window, at a position in that window's pixels.
    pub fn open_editor_in(&self, parent: u64, x: i32, y: i32) -> bool {
        match self {
            Host::Vst3(i) => i.open_editor_in(parent, x, y),
            Host::Clap(i) => i.open_editor_in(parent, x, y),
            Host::Lv2(i) => i.open_editor_in(parent, x, y),
            Host::Native(_) => false,
        }
    }

    pub fn editor_size(&self) -> (i32, i32) {
        match self {
            Host::Vst3(i) => i.editor_size(),
            Host::Clap(i) => i.editor_size(),
            Host::Lv2(i) => i.editor_size(),
            Host::Native(_) => (0, 0),
        }
    }

    pub fn move_editor(&self, x: i32, y: i32) {
        match self {
            Host::Vst3(i) => i.move_editor(x, y),
            Host::Clap(i) => i.move_editor(x, y),
            Host::Lv2(i) => i.move_editor(x, y),
            Host::Native(_) => {}
        }
    }

    pub fn editor_open(&self) -> bool {
        match self {
            Host::Vst3(i) => i.editor_open(),
            Host::Clap(i) => i.editor_open(),
            Host::Lv2(i) => i.editor_open(),
            Host::Native(_) => false,
        }
    }

    pub fn close_editor(&self) {
        match self {
            Host::Vst3(i) => i.close_editor(),
            Host::Clap(i) => i.close_editor(),
            Host::Lv2(i) => i.close_editor(),
            Host::Native(_) => {}
        }
    }

    /// Whether the plug-in changed its own settings, from its interface say, since last asked.
    pub fn take_dirty(&self) -> bool {
        match self {
            Host::Clap(i) => i.take_dirty(),
            Host::Lv2(i) => i.take_dirty(),
            Host::Vst3(_) | Host::Native(_) => false,
        }
    }

    /// Parameters the user is holding in the plug-in's own interface.
    pub fn held_params(&self) -> Vec<u32> {
        match self {
            Host::Clap(i) => i.held(),
            Host::Lv2(i) => i.held(),
            Host::Vst3(_) | Host::Native(_) => Vec::new(),
        }
    }

    pub fn clear_held(&self) {
        match self {
            Host::Clap(i) => i.clear_held(),
            Host::Lv2(i) => i.clear_held(),
            Host::Vst3(_) | Host::Native(_) => {}
        }
    }

    /// Main-thread housekeeping some plug-ins ask for.
    pub fn idle(&self) {
        match self {
            Host::Clap(i) => i.idle(),
            Host::Lv2(i) => i.idle(),
            Host::Vst3(_) | Host::Native(_) => {}
        }
    }
}
