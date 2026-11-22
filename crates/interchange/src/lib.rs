//! Reading other applications' project formats: Final Cut Pro XML, Standard MIDI Files, and the
//! openly readable parts of a project bundle.

pub mod fcpxml;
pub mod bundle;
pub mod smf;

const MAJOR: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "A♭", "A", "B♭", "B"];
const MINOR: [&str; 12] = ["C", "C♯", "D", "E♭", "E", "F", "F♯", "G", "G♯", "A", "B♭", "B"];

/// A key as Solstice's global Key track names it, such as "E♭ major" or "G♯ minor".
pub fn key_name(pitch_class: u8, minor: bool) -> String {
    let names = if minor { &MINOR } else { &MAJOR };
    format!("{} {}", names[pitch_class as usize % 12], if minor { "minor" } else { "major" })
}

/// Reads a written key such as "Eb", "D#" or "f♯" with a mode such as "minor" or "Moll".
pub fn parse_key(tonic: &str, mode: &str) -> Option<String> {
    let mut chars = tonic.trim().chars();
    let base = match chars.next()?.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let shift: i32 = chars
        .map(|c| match c {
            '#' | '♯' => Ok(1),
            'b' | '♭' => Ok(-1),
            _ => Err(()),
        })
        .sum::<Result<i32, ()>>()
        .ok()?;
    let mode = mode.trim().to_lowercase();
    let minor = mode.starts_with("min") || mode == "moll" || mode == "m";
    Some(key_name((base + shift).rem_euclid(12) as u8, minor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keys_as_the_key_track_does() {
        assert_eq!(parse_key("Eb", "major").as_deref(), Some("E♭ major"));
        assert_eq!(parse_key("D#", "minor").as_deref(), Some("E♭ minor"));
        assert_eq!(parse_key("Ab", "minor").as_deref(), Some("G♯ minor"));
        assert_eq!(parse_key("f♯", "Minor").as_deref(), Some("F♯ minor"));
        assert_eq!(parse_key("H", "major"), None);
    }
}
