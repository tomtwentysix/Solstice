//! Solstice's own plug-ins, written in Rust and run directly by the engine.

pub mod bank;
pub mod effects;

pub use bank::{Bank, ParamDef};
use solstice_engine::Plugin;
use std::sync::Arc;

/// Class IDs of built-in plug-ins start with this, so slots can tell them from VST3 classes.
pub const PREFIX: &str = "solstice:";

pub struct Spec {
    pub id: &'static str,
    pub name: &'static str,
    pub subcategories: &'static str,
    pub params: &'static [ParamDef],
}

pub const ALL: &[Spec] = &[
    Spec { id: "solstice:compressor", name: "Compressor", subcategories: "Fx|Dynamics", params: &effects::COMPRESSOR },
    Spec { id: "solstice:limiter", name: "Limiter", subcategories: "Fx|Dynamics", params: &effects::LIMITER },
    Spec { id: "solstice:gate", name: "Noise Gate", subcategories: "Fx|Dynamics", params: &effects::GATE },
    Spec { id: "solstice:delay", name: "Stereo Delay", subcategories: "Fx|Delay", params: &effects::DELAY },
    Spec { id: "solstice:reverb", name: "Space Reverb", subcategories: "Fx|Reverb", params: &effects::REVERB },
];

pub fn spec(id: &str) -> Option<&'static Spec> {
    ALL.iter().find(|s| s.id == id)
}

/// A running built-in plug-in: its parameters and latency, plus the processor for the engine.
pub struct Native {
    pub bank: Arc<Bank>,
    pub latency: u32,
    pub spec: &'static Spec,
}

pub fn create(id: &str, sample_rate: f64) -> Option<(Native, Box<dyn Plugin>)> {
    let spec = spec(id)?;
    let bank = Arc::new(Bank::new(spec.params));
    let sr = sample_rate as f32;
    let (plugin, latency): (Box<dyn Plugin>, u32) = match id {
        "solstice:compressor" => (Box::new(effects::Compressor::new(bank.clone(), sr)), 0),
        "solstice:limiter" => (Box::new(effects::Limiter::new(bank.clone(), sr)), effects::Limiter::latency_for(sr)),
        "solstice:gate" => (Box::new(effects::Gate::new(bank.clone(), sr)), 0),
        "solstice:delay" => (Box::new(effects::Delay::new(bank.clone(), sr)), 0),
        "solstice:reverb" => (Box::new(effects::Reverb::new(bank.clone(), sr)), 0),
        _ => return None,
    };
    Some((Native { bank, latency, spec }, plugin))
}
