//! Factory presets for the built-in plug-ins. Each lists only the parameters that differ from
//! the plug-in's defaults.

use crate::{Bank, spec};

pub struct Preset {
    pub name: &'static str,
    pub values: &'static [(u32, f32)],
}

const fn p(name: &'static str, values: &'static [(u32, f32)]) -> Preset {
    Preset { name, values }
}

// Threshold, Ratio, Attack, Release, Knee, Make-Up
const COMPRESSOR: &[Preset] = &[
    p("Vocal Leveller", &[(0, -20.0), (1, 3.0), (2, 5.0), (3, 120.0), (4, 8.0), (5, 4.0)]),
    p("Vocal Upfront", &[(0, -26.0), (1, 6.0), (2, 2.0), (3, 80.0), (4, 4.0), (5, 7.0)]),
    p("Podcast Voice", &[(0, -24.0), (1, 4.0), (2, 5.0), (3, 150.0), (4, 8.0), (5, 6.0)]),
    p("Kick Punch", &[(0, -16.0), (1, 4.0), (2, 25.0), (3, 80.0), (4, 3.0), (5, 3.0)]),
    p("Snare Snap", &[(0, -18.0), (1, 5.0), (2, 10.0), (3, 60.0), (4, 2.0), (5, 4.0)]),
    p("Drum Bus Glue", &[(0, -14.0), (1, 2.0), (2, 30.0), (3, 100.0), (4, 6.0), (5, 2.0)]),
    p("Drum Smash", &[(0, -40.0), (1, 20.0), (2, 0.5), (3, 50.0), (4, 0.0), (5, 14.0)]),
    p("Bass Even", &[(0, -22.0), (1, 4.0), (2, 15.0), (3, 150.0), (4, 6.0), (5, 4.0)]),
    p("Acoustic Guitar", &[(0, -20.0), (1, 2.5), (2, 15.0), (3, 150.0), (4, 8.0), (5, 3.0)]),
    p("Gentle Levelling", &[(0, -12.0), (1, 1.5), (2, 20.0), (3, 250.0), (4, 12.0), (5, 1.0)]),
    p("Mix Bus Glue", &[(0, -10.0), (1, 1.5), (2, 30.0), (3, 300.0), (4, 12.0), (5, 1.0)]),
];

// Gain, Output Level, Release
const LIMITER: &[Preset] = &[
    p("Safety", &[(0, 0.0), (1, -0.3), (2, 50.0)]),
    p("Transparent Master", &[(0, 2.0), (1, -1.0), (2, 100.0)]),
    p("Loud Master", &[(0, 8.0), (1, -0.3), (2, 40.0)]),
    p("Podcast", &[(0, 4.0), (1, -1.0), (2, 150.0)]),
    p("Drum Bus Peaks", &[(0, 3.0), (1, -1.0), (2, 20.0)]),
];

// Threshold, Reduction, Attack, Hold, Release
const GATE: &[Preset] = &[
    p("Kick", &[(0, -30.0), (1, -80.0), (2, 0.1), (3, 40.0), (4, 80.0)]),
    p("Snare", &[(0, -28.0), (1, -80.0), (2, 0.1), (3, 60.0), (4, 120.0)]),
    p("Toms", &[(0, -35.0), (1, -30.0), (2, 0.2), (3, 100.0), (4, 250.0)]),
    p("Vocal Breaths", &[(0, -45.0), (1, -15.0), (2, 1.0), (3, 80.0), (4, 200.0)]),
    p("Guitar Amp Hum", &[(0, -55.0), (1, -40.0), (2, 1.0), (3, 50.0), (4, 150.0)]),
    p("Podcast Room", &[(0, -50.0), (1, -20.0), (2, 2.0), (3, 150.0), (4, 300.0)]),
];

// Time, Sync, Feedback, Mix, Ping-Pong, Low Cut, High Cut, Stereo Offset.
// Sync: 0 Off, 1 1/16, 2 1/16 dotted, 3 1/8 triplet, 4 1/8, 5 1/8 dotted, 6 1/4, 7 1/2.
const DELAY: &[Preset] = &[
    p("Quarter Note", &[(1, 6.0), (2, 30.0), (3, 25.0), (5, 150.0), (6, 7000.0)]),
    p("Dotted Eighth", &[(1, 5.0), (2, 40.0), (3, 25.0), (5, 200.0), (6, 6000.0)]),
    p("Ping-Pong Eighths", &[(1, 4.0), (2, 45.0), (3, 30.0), (4, 1.0)]),
    p("Slapback", &[(0, 110.0), (2, 0.0), (3, 25.0), (5, 120.0), (6, 5000.0)]),
    p("Tape Echo", &[(0, 300.0), (2, 55.0), (3, 30.0), (5, 300.0), (6, 3000.0)]),
    p("Dub Echo", &[(1, 5.0), (2, 70.0), (3, 35.0), (4, 1.0), (5, 400.0), (6, 2500.0)]),
    p("Wide Doubler", &[(0, 15.0), (2, 0.0), (3, 35.0), (5, 100.0), (6, 12000.0), (7, 12.0)]),
    p("Aux Quarter Note", &[(1, 6.0), (2, 35.0), (3, 100.0), (5, 200.0), (6, 6000.0)]),
    p("Aux Dotted Eighth", &[(1, 5.0), (2, 40.0), (3, 100.0), (4, 1.0), (5, 250.0), (6, 5000.0)]),
];

// Size, Damping, Width, Mix, Pre-Delay
const REVERB: &[Preset] = &[
    p("Small Room", &[(0, 30.0), (1, 60.0), (2, 80.0), (3, 20.0), (4, 5.0)]),
    p("Drum Room", &[(0, 40.0), (1, 55.0), (2, 90.0), (3, 18.0), (4, 0.0)]),
    p("Vocal Plate", &[(0, 55.0), (1, 35.0), (3, 22.0), (4, 30.0)]),
    p("Medium Hall", &[(0, 70.0), (1, 50.0), (3, 25.0), (4, 20.0)]),
    p("Large Hall", &[(0, 90.0), (1, 45.0), (3, 30.0), (4, 40.0)]),
    p("Dark Chamber", &[(0, 60.0), (1, 80.0), (2, 90.0), (3, 25.0), (4, 15.0)]),
    p("Ambient Wash", &[(0, 98.0), (1, 30.0), (3, 45.0), (4, 60.0)]),
    p("Aux Plate", &[(0, 55.0), (1, 35.0), (3, 100.0), (4, 30.0)]),
    p("Aux Hall", &[(0, 75.0), (1, 50.0), (3, 100.0), (4, 25.0)]),
];

// 0 Osc 1 Wave, 1 Osc 2 Wave (0 Saw, 1 Square, 2 Triangle, 3 Sine), 2 Osc 2 Semitones,
// 3 Osc 2 Detune, 4 Osc Mix, 5 Cutoff, 6 Resonance, 7 Filter Env Amount, 8-11 Filter ADSR,
// 12-15 Amp ADSR, 16 LFO Rate, 17 LFO to Pitch, 18 LFO to Cutoff, 19 Glide, 20 Volume
const SYNTH: &[Preset] = &[
    p(
        "Warm Pad",
        &[(1, 0.0), (3, 12.0), (5, 1200.0), (6, 10.0), (7, 20.0), (8, 800.0), (9, 1500.0), (10, 60.0), (11, 1500.0),
          (12, 600.0), (13, 1000.0), (14, 90.0), (15, 1800.0), (16, 0.3), (18, 10.0), (20, -10.0)],
    ),
    p(
        "String Ensemble",
        &[(1, 0.0), (3, 18.0), (5, 2500.0), (6, 5.0), (7, 10.0), (12, 300.0), (13, 800.0), (14, 85.0), (15, 900.0),
          (16, 5.0), (17, 4.0), (20, -9.0)],
    ),
    p(
        "Brass Stab",
        &[(1, 0.0), (3, 10.0), (5, 900.0), (6, 15.0), (7, 60.0), (8, 60.0), (9, 400.0), (10, 50.0), (11, 200.0),
          (12, 40.0), (13, 300.0), (14, 85.0), (15, 200.0)],
    ),
    p(
        "Electric Keys",
        &[(0, 2.0), (1, 3.0), (2, 12.0), (3, 0.0), (4, 30.0), (5, 3000.0), (6, 5.0), (7, 25.0), (9, 600.0), (10, 20.0),
          (12, 2.0), (13, 1500.0), (14, 40.0), (15, 400.0), (20, -8.0)],
    ),
    p(
        "Pluck",
        &[(3, 8.0), (5, 600.0), (6, 30.0), (7, 80.0), (8, 1.0), (9, 250.0), (10, 0.0), (11, 250.0), (12, 1.0),
          (13, 400.0), (14, 0.0), (15, 300.0)],
    ),
    p(
        "Arp Pluck",
        &[(0, 1.0), (1, 0.0), (5, 1500.0), (6, 40.0), (7, 60.0), (9, 150.0), (10, 10.0), (12, 1.0), (13, 200.0),
          (14, 30.0), (15, 150.0), (20, -9.0)],
    ),
    p(
        "Fat Saw Lead",
        &[(1, 0.0), (3, 15.0), (5, 4000.0), (6, 25.0), (7, 30.0), (8, 2.0), (9, 400.0), (10, 50.0), (12, 2.0),
          (14, 90.0), (15, 200.0), (19, 40.0), (20, -8.0)],
    ),
    p("Square Lead", &[(0, 1.0), (1, 1.0), (2, 12.0), (3, 5.0), (4, 30.0), (5, 3500.0), (6, 15.0), (19, 30.0), (20, -8.0)]),
    p(
        "Supersaw Chords",
        &[(1, 0.0), (3, 25.0), (5, 6000.0), (6, 10.0), (7, 15.0), (12, 5.0), (14, 85.0), (15, 500.0), (20, -10.0)],
    ),
    p(
        "Sub Bass",
        &[(0, 3.0), (1, 3.0), (3, 0.0), (4, 0.0), (5, 800.0), (6, 0.0), (7, 0.0), (12, 2.0), (13, 300.0), (14, 100.0),
          (15, 80.0)],
    ),
    p(
        "808 Bass",
        &[(0, 3.0), (1, 2.0), (3, 0.0), (4, 15.0), (5, 1500.0), (6, 0.0), (7, 20.0), (9, 200.0), (12, 1.0),
          (13, 1800.0), (14, 0.0), (15, 300.0), (19, 60.0)],
    ),
    p(
        "Acid Bass",
        &[(4, 0.0), (5, 400.0), (6, 75.0), (7, 70.0), (8, 1.0), (9, 180.0), (10, 0.0), (11, 150.0), (12, 1.0),
          (13, 300.0), (14, 70.0), (15, 60.0), (19, 50.0)],
    ),
    p("Reese Bass", &[(1, 0.0), (3, 30.0), (5, 700.0), (6, 20.0), (7, 10.0), (14, 100.0), (15, 100.0), (16, 0.4), (18, 15.0)]),
    p("Wobble Bass", &[(2, -12.0), (5, 500.0), (6, 50.0), (7, 0.0), (14, 100.0), (15, 100.0), (16, 4.0), (18, 60.0)]),
];

// Attack, Release, Volume, Velocity
const SAMPLER: &[Preset] = &[
    p("Natural", &[(0, 1.0), (1, 300.0), (3, 80.0)]),
    p("Staccato", &[(0, 0.5), (1, 40.0), (3, 90.0)]),
    p("Slow Swell", &[(0, 600.0), (1, 1500.0), (3, 30.0)]),
    p("Organ (No Velocity)", &[(0, 3.0), (1, 80.0), (3, 0.0)]),
];

const DRUMS: &[Preset] = &[
    p("Dynamic", &[(0, 0.5), (3, 100.0)]),
    p("Punchy", &[(0, 0.5), (1, 150.0), (3, 70.0)]),
    p("Fixed Velocity", &[(0, 0.5), (3, 0.0)]),
    p("Soft", &[(0, 3.0), (2, -4.0), (3, 90.0)]),
];

// Rate (0 1/4, 1 1/8, 2 1/8 triplet, 3 1/16, 4 1/16 triplet, 5 1/32, 6 1/2), Mode (0 Up, 1 Down,
// 2 Up and Down, 3 As Played, 4 Random), Octaves, Gate, Latch
const ARPEGGIATOR: &[Preset] = &[
    p("Sixteenths Up", &[(0, 3.0), (1, 0.0), (2, 1.0), (3, 50.0)]),
    p("Eighths Up and Down", &[(0, 1.0), (1, 2.0), (2, 2.0), (3, 60.0)]),
    p("Trance Gate", &[(0, 3.0), (1, 0.0), (2, 3.0), (3, 30.0)]),
    p("Triplet Cascade", &[(0, 2.0), (1, 1.0), (2, 2.0), (3, 70.0)]),
    p("Random Sixteenths", &[(0, 3.0), (1, 4.0), (2, 3.0), (3, 40.0)]),
    p("Quarter Notes", &[(0, 0.0), (1, 0.0), (2, 1.0), (3, 90.0)]),
    p("Latched Chords", &[(0, 1.0), (1, 3.0), (2, 1.0), (3, 80.0), (4, 1.0)]),
];

// Chord (0 Major, 1 Minor, 2 Diminished, 3 Augmented, 4 Sus 2, 5 Sus 4, 6 Major 7th,
// 7 Minor 7th, 8 Dominant 7th, 9 Power), Inversion
const CHORD_TRIGGER: &[Preset] = &[
    p("Major Triad", &[(0, 0.0)]),
    p("Minor Triad", &[(0, 1.0)]),
    p("Major 7th", &[(0, 6.0)]),
    p("Minor 7th", &[(0, 7.0)]),
    p("Dominant 7th", &[(0, 8.0)]),
    p("Sus 4", &[(0, 5.0)]),
    p("Power Chord", &[(0, 9.0)]),
    p("Major, First Inversion", &[(0, 0.0), (1, 1.0)]),
    p("Minor, Second Inversion", &[(0, 1.0), (1, 2.0)]),
];

// Delay (rates as the Arpeggiator), Repeats, Velocity, Transpose
const NOTE_REPEATER: &[Preset] = &[
    p("Eighth Note Echo", &[(0, 1.0), (1, 3.0), (2, -20.0)]),
    p("Sixteenth Stutter", &[(0, 3.0), (1, 4.0), (2, -10.0)]),
    p("Fading Quarter Notes", &[(0, 0.0), (1, 4.0), (2, -35.0)]),
    p("Rising Octaves", &[(0, 1.0), (1, 2.0), (2, -15.0), (3, 12.0)]),
    p("Fifth Above", &[(0, 3.0), (1, 1.0), (2, -10.0), (3, 7.0)]),
];

// Root (0 C … 11 B), Scale (0 Major, 1 Natural Minor, 2 Harmonic Minor, 3 Dorian,
// 4 Mixolydian, 5 Lydian, 6 Major Pentatonic, 7 Minor Pentatonic, 8 Blues)
const SCALE_QUANTIZE: &[Preset] = &[
    p("C Major", &[(0, 0.0), (1, 0.0)]),
    p("G Major", &[(0, 7.0), (1, 0.0)]),
    p("A Minor", &[(0, 9.0), (1, 1.0)]),
    p("E Minor", &[(0, 4.0), (1, 1.0)]),
    p("D Dorian", &[(0, 2.0), (1, 3.0)]),
    p("C Major Pentatonic", &[(0, 0.0), (1, 6.0)]),
    p("A Minor Pentatonic", &[(0, 9.0), (1, 7.0)]),
    p("E Blues", &[(0, 4.0), (1, 8.0)]),
];

const TRANSPOSER: &[Preset] = &[
    p("Octave Up", &[(0, 12.0)]),
    p("Octave Down", &[(0, -12.0)]),
    p("Two Octaves Down", &[(0, -24.0)]),
    p("Fifth Up", &[(0, 7.0)]),
    p("Fourth Up", &[(0, 5.0)]),
];

// Scale, Offset, Minimum, Maximum
const VELOCITY: &[Preset] = &[
    p("Compress Dynamics", &[(0, 50.0), (1, 50.0)]),
    p("Expand Dynamics", &[(0, 150.0), (1, -30.0)]),
    p("Fixed at 100", &[(0, 0.0), (1, 100.0)]),
    p("Softer", &[(0, 70.0)]),
    p("Harder", &[(0, 120.0), (1, 15.0)]),
    p("Limit to 100", &[(3, 100.0)]),
];

pub fn factory(id: &str) -> &'static [Preset] {
    match id {
        "solstice:compressor" => COMPRESSOR,
        "solstice:limiter" => LIMITER,
        "solstice:gate" => GATE,
        "solstice:delay" => DELAY,
        "solstice:reverb" => REVERB,
        "solstice:synth" => SYNTH,
        "solstice:sampler" => SAMPLER,
        "solstice:drums" => DRUMS,
        "solstice:arpeggiator" => ARPEGGIATOR,
        "solstice:chord-trigger" => CHORD_TRIGGER,
        "solstice:note-repeater" => NOTE_REPEATER,
        "solstice:scale-quantize" => SCALE_QUANTIZE,
        "solstice:transposer" => TRANSPOSER,
        "solstice:velocity" => VELOCITY,
        _ => &[],
    }
}

pub fn find(id: &str, name: &str) -> Option<&'static Preset> {
    factory(id).iter().find(|p| p.name == name)
}

/// Sets every parameter: the preset's value where it has one, the default otherwise.
pub fn apply(bank: &Bank, preset: &Preset) {
    for d in bank.defs {
        let v = preset.values.iter().find(|(id, _)| *id == d.id).map_or(d.default, |(_, v)| *v);
        bank.set(d.id, v);
    }
}

/// Saved slot state for a plug-in loaded with this preset, for building projects without a
/// running instance.
pub fn state(id: &str, name: &str) -> Vec<u8> {
    let (Some(spec), Some(preset)) = (spec(id), find(id, name)) else { return Vec::new() };
    let bank = Bank::new(spec.params);
    apply(&bank, preset);
    bank.state()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_fit_their_parameters() {
        for spec in crate::ALL {
            let presets = factory(spec.id);
            assert!(!presets.is_empty(), "{} has no presets", spec.name);
            for preset in presets {
                for (id, v) in preset.values {
                    let d = spec.params.iter().find(|d| d.id == *id).unwrap_or_else(|| panic!("{}: {} has no parameter {id}", spec.name, preset.name));
                    assert!((d.min..=d.max).contains(v), "{}: {} sets {} to {v}", spec.name, preset.name, d.name);
                }
            }
        }
    }

    #[test]
    fn apply_resets_unlisted_parameters() {
        let bank = Bank::new(&crate::effects::REVERB);
        bank.set(2, 10.0);
        apply(&bank, find("solstice:reverb", "Vocal Plate").unwrap());
        assert_eq!(bank.get(0), 55.0);
        assert_eq!(bank.get(2), 100.0);
    }
}
