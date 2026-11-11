use solstice_dsp::Native;
use solstice_vst3::{ClassInfo, Instance, ParamInfo};
use std::path::PathBuf;
use std::sync::Arc;

/// A loaded plug-in as the UI sees it: a VST3 instance or one of Solstice's own.
#[derive(Clone)]
pub enum Host {
    Vst3(Arc<Instance>),
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
            Host::Native(n) => n.latency,
        }
    }

    pub fn state(&self) -> Vec<u8> {
        match self {
            Host::Vst3(i) => i.state(),
            Host::Native(n) => match &n.samples {
                Some(map) => solstice_dsp::sampler::state(&n.bank, &map.descs()),
                None => n.bank.state(),
            },
        }
    }

    /// Restores saved state; sample instruments load their zones' audio from `audio_dir`.
    pub fn set_state_in(&self, data: &[u8], audio_dir: &std::path::Path) -> bool {
        match self {
            Host::Vst3(i) => i.set_state(data),
            Host::Native(n) => match &n.samples {
                Some(map) => {
                    let descs = solstice_dsp::sampler::set_state(&n.bank, data);
                    map.set(descs.iter().filter_map(|d| crate::samples::load_zone(d, audio_dir)).collect());
                    true
                }
                None => n.bank.set_state(data),
            },
        }
    }

    pub fn native(&self) -> Option<&Arc<Native>> {
        match self {
            Host::Native(n) => Some(n),
            Host::Vst3(_) => None,
        }
    }

    pub fn params(&self) -> Vec<ParamInfo> {
        match self {
            Host::Vst3(i) => i.params(),
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
            Host::Native(n) => n.bank.norm(id) as f64,
        }
    }

    pub fn set_param(&self, id: u32, value: f64) {
        match self {
            Host::Vst3(i) => i.set_param(id, value),
            Host::Native(n) => n.bank.set_norm(id, value as f32),
        }
    }

    pub fn param_text(&self, id: u32, value: f64) -> String {
        match self {
            Host::Vst3(i) => i.param_text(id, value),
            Host::Native(n) => {
                let plain = n.bank.defs.iter().find(|d| d.id == id).map_or(0.0, |d| d.from_norm(value as f32));
                n.bank.text(id, plain)
            }
        }
    }

    /// Gain reduction or level a built-in plug-in reports, in dB.
    pub fn meter(&self) -> Option<f32> {
        match self {
            Host::Vst3(_) => None,
            Host::Native(n) => Some(n.bank.meter()),
        }
    }

    pub fn has_editor(&self) -> bool {
        match self {
            Host::Vst3(i) => i.has_editor(),
            Host::Native(_) => false,
        }
    }

    pub fn open_editor(&self) -> bool {
        match self {
            Host::Vst3(i) => i.open_editor(&i.info.name),
            Host::Native(_) => false,
        }
    }

    pub fn editor_open(&self) -> bool {
        match self {
            Host::Vst3(i) => i.editor_open(),
            Host::Native(_) => false,
        }
    }

    pub fn close_editor(&self) {
        if let Host::Vst3(i) = self {
            i.close_editor();
        }
    }
}
