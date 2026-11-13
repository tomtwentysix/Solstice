//! Solstice's own plug-ins, written in Rust and run directly by the engine.

pub mod bank;
pub mod convolve;
pub mod drumkit;
pub mod effects;
pub mod ir;
pub mod loudness;
pub mod midifx;
pub mod nam;
pub mod presets;
pub mod resample;
pub mod sampler;
pub mod slot;
pub mod synth;

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
    Spec { id: "solstice:saturation", name: "Saturation", subcategories: "Fx|Distortion", params: &effects::SATURATION },
    Spec { id: "solstice:de-esser", name: "De-esser", subcategories: "Fx|Dynamics", params: &effects::DE_ESSER },
    Spec { id: "solstice:nam", name: "Neural Amp Modeler", subcategories: "Fx|Amp|Pedal", params: &nam::NAM },
    Spec { id: "solstice:ir", name: "Impulse Response", subcategories: "Fx|Cabinet|Reverb", params: &ir::IR },
    Spec { id: "solstice:synth", name: "Solstice Synth", subcategories: "Instrument|Synth", params: &synth::SYNTH },
    Spec { id: "solstice:sampler", name: "Sampler", subcategories: "Instrument|Sampler", params: &sampler::SAMPLER },
    Spec { id: "solstice:drums", name: "Drum Kit", subcategories: "Instrument|Drum", params: &sampler::SAMPLER },
    Spec { id: "solstice:arpeggiator", name: "Arpeggiator", subcategories: "MIDI FX", params: &midifx::ARPEGGIATOR },
    Spec { id: "solstice:chord-trigger", name: "Chord Trigger", subcategories: "MIDI FX", params: &midifx::CHORD_TRIGGER },
    Spec { id: "solstice:note-repeater", name: "Note Repeater", subcategories: "MIDI FX", params: &midifx::NOTE_REPEATER },
    Spec { id: "solstice:scale-quantize", name: "Scale Quantize", subcategories: "MIDI FX", params: &midifx::SCALE_QUANTIZE },
    Spec { id: "solstice:transposer", name: "Transposer", subcategories: "MIDI FX", params: &midifx::TRANSPOSER },
    Spec { id: "solstice:velocity", name: "Velocity Processor", subcategories: "MIDI FX", params: &midifx::VELOCITY },
];

/// Whether a catalog subcategory string marks a MIDI effect.
pub fn is_midi_fx(subcategories: &str) -> bool {
    subcategories.starts_with("MIDI FX")
}

pub fn spec(id: &str) -> Option<&'static Spec> {
    ALL.iter().find(|s| s.id == id)
}

/// A running built-in plug-in: its parameters and latency, plus the processor for the engine.
pub struct Native {
    pub bank: Arc<Bank>,
    pub latency: u32,
    pub spec: &'static Spec,
    /// The zones of the Sampler and Drum Kit.
    pub samples: Option<Arc<sampler::SampleMap>>,
    /// The capture Neural Amp Modeler plays.
    pub captures: Option<Arc<nam::Captures>>,
    /// The response the impulse response loader convolves with.
    pub irs: Option<Arc<ir::Irs>>,
    pub sample_rate: f64,
}

impl Native {
    /// The latency now, which for Neural Amp Modeler depends on the capture.
    pub fn current_latency(&self) -> u32 {
        self.latency + self.captures.as_ref().map_or(0, |c| c.latency())
    }

    pub fn is_drum_kit(&self) -> bool {
        self.spec.id == "solstice:drums"
    }
}

pub fn create(id: &str, sample_rate: f64) -> Option<(Native, Box<dyn Plugin>)> {
    let spec = spec(id)?;
    let bank = Arc::new(Bank::new(spec.params));
    let sr = sample_rate as f32;
    let samples = matches!(id, "solstice:sampler" | "solstice:drums").then(|| Arc::new(sampler::SampleMap::default()));
    let captures = (id == "solstice:nam").then(|| Arc::new(nam::Captures::default()));
    let irs = (id == "solstice:ir").then(|| Arc::new(ir::Irs::default()));
    let (plugin, latency): (Box<dyn Plugin>, u32) = match id {
        "solstice:synth" => (Box::new(synth::Synth::new(bank.clone(), sr)), 0),
        "solstice:sampler" | "solstice:drums" => {
            (Box::new(sampler::Sampler::new(bank.clone(), samples.clone().unwrap(), sr)), 0)
        }
        "solstice:compressor" => (Box::new(effects::Compressor::new(bank.clone(), sr)), 0),
        "solstice:limiter" => (Box::new(effects::Limiter::new(bank.clone(), sr)), effects::Limiter::latency_for(sr)),
        "solstice:gate" => (Box::new(effects::Gate::new(bank.clone(), sr)), 0),
        "solstice:delay" => (Box::new(effects::Delay::new(bank.clone(), sr)), 0),
        "solstice:reverb" => (Box::new(effects::Reverb::new(bank.clone(), sr)), 0),
        "solstice:saturation" => (Box::new(effects::Saturation::new(bank.clone(), sr)), effects::Saturation::LATENCY),
        "solstice:de-esser" => (Box::new(effects::DeEsser::new(bank.clone(), sr)), 0),
        "solstice:nam" => (Box::new(nam::Nam::new(bank.clone(), captures.clone().unwrap(), sr)), 0),
        "solstice:ir" => (Box::new(ir::ImpulseResponse::new(bank.clone(), irs.clone().unwrap())), 0),
        "solstice:arpeggiator" => (Box::new(midifx::Arpeggiator::new(bank.clone(), sr)), 0),
        "solstice:chord-trigger" => (Box::new(midifx::ChordTrigger::new(bank.clone())), 0),
        "solstice:note-repeater" => (Box::new(midifx::NoteRepeater::new(bank.clone(), sr)), 0),
        "solstice:scale-quantize" => (Box::new(midifx::ScaleQuantize::new(bank.clone())), 0),
        "solstice:transposer" => (Box::new(midifx::Transposer::new(bank.clone())), 0),
        "solstice:velocity" => (Box::new(midifx::VelocityProcessor::new(bank.clone())), 0),
        _ => return None,
    };
    Some((Native { bank, latency, spec, samples, captures, irs, sample_rate }, plugin))
}

#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
