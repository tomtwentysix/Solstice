use solstice_model::{PPQ, Pattern, PatternRow, Step};

const KICK: u8 = 36;
const STICK: u8 = 37;
const SNARE: u8 = 38;
const CLAP: u8 = 39;
const LOW_TOM: u8 = 41;
const HAT: u8 = 42;
const PEDAL: u8 = 44;
const MID_TOM: u8 = 45;
const OPEN: u8 = 46;
const HIGH_TOM: u8 = 48;
const CRASH: u8 = 49;
const RIDE: u8 = 51;
const TAMBOURINE: u8 = 54;
const COWBELL: u8 = 56;
const SHAKER: u8 = 70;
const CLAVES: u8 = 75;

const S16: u32 = PPQ / 4;
const S8: u32 = PPQ / 2;
const S8T: u32 = PPQ / 3;

pub struct Starter {
    pub genre: &'static str,
    pub name: &'static str,
    rate: u32,
    swing: f32,
    /// Each row in step notation; see `parse_row`.
    rows: &'static [(u8, &'static str)],
}

const fn starter(genre: &'static str, name: &'static str, rate: u32, swing: f32, rows: &'static [(u8, &'static str)]) -> Starter {
    Starter { genre, name, rate, swing, rows }
}

pub const STARTERS: &[Starter] = &[
    starter("Rock", "Straight Eighths", S16, 0.0, &[(KICK, "x.......x.x....."), (SNARE, "....X.......X..."), (HAT, "x.x.x.x.x.x.x.x.")]),
    starter("Rock", "Driving", S16, 0.0, &[(KICK, "x......xx.x....."), (SNARE, "....X.......X..."), (HAT, "X.x.X.x.X.x.X.x."), (CRASH, "x...............")]),
    starter("Rock", "Half Time", S16, 0.0, &[(KICK, "x.....x...x....."), (SNARE, "........X......."), (HAT, "x.x.x.x.x.x.x.x.")]),
    starter("Rock", "Ride and Floor", S16, 0.0, &[(KICK, "x.....x.x......."), (SNARE, "....X.......X..o"), (RIDE, "x.x.x.x.x.x.x.x.")]),
    starter("Pop", "Motown", S16, 0.0, &[(KICK, "x.....x.x......."), (SNARE, "x...x...x...x..."), (TAMBOURINE, "....x.......x...")]),
    starter("Pop", "Ballad", S8, 0.0, &[(KICK, "x..x....x..x...."), (STICK, "..x...x...x...x."), (HAT, "xxxxxxxxxxxxxxxx")]),
    starter("Pop", "Dance Pop", S16, 0.0, &[(KICK, "x...x...x...x..."), (CLAP, "....x.......x..."), (HAT, "..x...x...x...x."), (SHAKER, "xoxoxoxoxoxoxoxo")]),
    starter("Funk", "Ghost Notes", S16, 0.1, &[(KICK, "x.x...x...x..x.."), (SNARE, "....X..o.o..X..o"), (HAT, "XxXxXxXxXxXxXxXx")]),
    starter("Funk", "Syncopated", S16, 0.15, &[(KICK, "x..x..x...x..x.."), (SNARE, "....X..?.?..X.?."), (HAT, "x.x.x.x.x.x.xox."), (OPEN, ".............x..")]),
    starter("Disco", "Disco", S16, 0.0, &[(KICK, "x...x...x...x..."), (SNARE, "....x.......x..."), (HAT, "x...x...x...x..."), (OPEN, "..x...x...x...x.")]),
    starter("House", "Four on the Floor", S16, 0.0, &[(KICK, "x...x...x...x..."), (CLAP, "....x.......x..."), (OPEN, "..x...x...x...x."), (SHAKER, "xoxoxoxoxoxoxoxo")]),
    starter("House", "Deep House", S16, 0.3, &[(KICK, "x...x...x...x..."), (CLAP, "....x.......x..."), (HAT, "..x...x...x...xo"), (STICK, "...x..x....x.?..")]),
    starter("Techno", "Techno", S16, 0.0, &[(KICK, "x...x...x...x..."), (CLAP, "....x.......x..."), (HAT, "..x...x...x...3."), (RIDE, "x.x.x.x.x.x.x.x.")]),
    // The rimshot and cowbell loop over 12 and 10 steps against the 16-step bar.
    starter("Techno", "Polymeter", S16, 0.0, &[(KICK, "x...x...x...x..."), (STICK, "x..x..x.x..."), (COWBELL, "..x..o..x."), (HAT, "..x...x...x...x.")]),
    starter("Electro", "Electro", S16, 0.0, &[(KICK, "x.....x...x....."), (CLAP, "....x.......x..."), (HAT, "xxxxXxxxxxxxXxxx"), (COWBELL, "...x.....x...x..")]),
    starter("Hip-Hop", "Boom Bap", S16, 0.5, &[(KICK, "x.....x..xx....."), (SNARE, "....X.......X..."), (HAT, "x.x.x.x.x.x.x.x.")]),
    starter("Hip-Hop", "Lo-Fi", S16, 0.6, &[(KICK, "x......x..x....."), (SNARE, "....x.......x..o"), (HAT, "x.o.x.o.x.o.x.?."), (SHAKER, "..x...x...x...x.")]),
    starter(
        "Trap",
        "Trap",
        S16,
        0.0,
        &[
            (KICK, "x......x..x.....x.....x...x....."),
            (CLAP, "........X...............X......."),
            (HAT, "x.x.x.x.x.x.3.x.x.x.x.x.x4x.x.x."),
            (OPEN, "..............x................."),
        ],
    ),
    starter("Drum and Bass", "Two-Step", S16, 0.0, &[(KICK, "x.........x....."), (SNARE, "....X.......X..."), (HAT, "x.x.x.x.x.x.x.x."), (RIDE, "..x...x...x...x.")]),
    starter("Drum and Bass", "Breakbeat", S16, 0.0, &[(KICK, "x.x.......xx...."), (SNARE, "....X..o.o..X..o"), (RIDE, "x.x.x.x.x.x.x.x.")]),
    starter("Garage", "Two-Step Garage", S16, 0.55, &[(KICK, "x.........x..x.."), (CLAP, "....x.......x..."), (HAT, "..x...x...x...x."), (SHAKER, "xoxoxoxoxoxoxoxo")]),
    starter("Reggae", "One Drop", S16, 0.5, &[(KICK, "........x......."), (STICK, "........x......."), (HAT, "x.x.x.x.x.x.x.x.")]),
    starter("Reggaeton", "Dembow", S16, 0.0, &[(KICK, "x...x...x...x..."), (SNARE, "...x..x....x..x."), (HAT, "x.x.x.x.x.x.x.x.")]),
    starter("Latin", "Son Clave", S16, 0.0, &[(CLAVES, "x..x..x...x.x..."), (KICK, "...x...x...x...x"), (COWBELL, "x.x.x.x.x.x.x.x."), (SHAKER, "xoxoxoxoxoxoxoxo")]),
    starter("Latin", "Bossa Nova", S8, 0.0, &[(KICK, "x..xx..xx..xx..x"), (STICK, "x..x..x...x..x.."), (HAT, "xxxxxxxxxxxxxxxx")]),
    starter("Shuffle", "Shuffle", S8T, 0.0, &[(KICK, "x.....x....."), (SNARE, "...X.....X.."), (HAT, "x.xx.xx.xx.x")]),
    starter("Shuffle", "Half-Time Shuffle", S8T, 0.0, &[(KICK, "x....xx....."), (SNARE, ".o.oXo.o.oXo"), (HAT, "x.xx.xx.xx.x")]),
    starter("Jazz", "Swing Ride", S8T, 0.0, &[(RIDE, "x..x.xx..x.x"), (PEDAL, "...x.....x.."), (KICK, "o..o..o..o..")]),
    starter("Metal", "Double Kick", S16, 0.0, &[(KICK, "xxxxxxxxxxxxxxxx"), (SNARE, "....X.......X..."), (RIDE, "x...x...x...x..."), (CRASH, "x...............")]),
    starter("Metal", "Blast Beat", S16, 0.0, &[(KICK, "x.x.x.x.x.x.x.x."), (SNARE, ".x.x.x.x.x.x.x.x"), (CRASH, "x.x.x.x.x.x.x.x.")]),
    starter("Fills", "Tom Run", S16, 0.0, &[(HIGH_TOM, "xxxx............"), (MID_TOM, "....xxxx........"), (LOW_TOM, "........xxxx...."), (SNARE, "............2233")]),
    starter("Fills", "Snare Build", S16, 0.0, &[(SNARE, "o.o.x.x.xxxxXX44"), (KICK, "x...x...x...x...")]),
];

/// Reads one row of step notation. Each character is a step:
///
/// `x` a hit, `X` an accent, `o` a ghost note, `?` a hit that plays half the time,
/// `2` to `8` that many hits within the step, `_` holds the step before through this one,
/// `.` or `-` a rest. Spaces and `|` are ignored, so bars can be marked off.
///
/// Returns the steps that are on and how many steps the row has.
pub fn parse_row(pitch: u8, text: &str) -> Result<(PatternRow, u32), String> {
    let mut row = PatternRow::new(pitch);
    let mut len = 0u32;
    for ch in text.chars() {
        let index = len as u16;
        let step = match ch {
            ' ' | '|' => continue,
            '.' | '-' => None,
            'x' => Some(Step::new(index, 96)),
            'X' => Some(Step::new(index, 120)),
            'o' => Some(Step::new(index, 45)),
            '?' => Some(Step { chance: 50, ..Step::new(index, 96) }),
            '_' => Some(Step { tie: true, ..Step::new(index, 96) }),
            '2'..='8' => Some(Step { repeat: ch as u8 - b'0', ..Step::new(index, 90) }),
            _ => return Err(format!("unknown step '{ch}'")),
        };
        if let Some(s) = step {
            row.set(s);
        }
        len += 1;
    }
    Ok((row, len))
}

impl Starter {
    /// The pattern, its length taken from the first row. A row of another length loops on
    /// its own; an empty row is left out.
    pub fn pattern(&self) -> Result<Pattern, String> {
        let mut p = Pattern { rate: self.rate, swing: self.swing, length: 0, ..Pattern::default() };
        for (pitch, text) in self.rows {
            let (mut row, len) = parse_row(*pitch, text).map_err(|e| format!("{}: {e}", self.name))?;
            if len == 0 {
                continue;
            }
            if p.length == 0 {
                p.length = len;
            } else if len != p.length {
                row.length = Some(len);
            }
            p.rows.push(row);
        }
        if p.length == 0 {
            return Err(format!("{}: no steps", self.name));
        }
        Ok(p)
    }
}

/// The genres in the order they first appear.
pub fn genres() -> Vec<&'static str> {
    let mut out: Vec<&str> = Vec::new();
    for s in STARTERS {
        if !out.contains(&s.genre) {
            out.push(s.genre);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation() {
        let (row, len) = parse_row(38, "x.X. |o?_3").unwrap();
        assert_eq!(len, 8);
        let s = |i| *row.step(i).unwrap();
        assert_eq!((s(0).velocity, s(2).velocity, s(4).velocity), (96, 120, 45));
        assert!(row.step(1).is_none() && row.step(3).is_none());
        assert_eq!(s(5).chance, 50);
        assert!(s(6).tie);
        assert_eq!(s(7).repeat, 3);
        assert_eq!(parse_row(36, "x.y").unwrap_err(), "unknown step 'y'");
        assert_eq!(parse_row(36, "").unwrap().1, 0);
    }

    #[test]
    fn every_starter_parses_and_plays() {
        for s in STARTERS {
            let p = s.pattern().unwrap_or_else(|e| panic!("{e}"));
            assert!(!p.rows.is_empty(), "{}", s.name);
            let notes = p.bake(p.rate * p.length);
            assert!(!notes.is_empty(), "{}", s.name);
            assert!(STARTERS.iter().filter(|o| o.genre == s.genre && o.name == s.name).count() == 1, "{} twice", s.name);
        }
        assert!(genres().len() > 10);
    }

    #[test]
    fn rows_of_another_length_loop_on_their_own() {
        let s = STARTERS.iter().find(|s| s.name == "Polymeter").unwrap();
        let p = s.pattern().unwrap();
        assert_eq!(p.length, 16);
        let own: Vec<Option<u32>> = p.rows.iter().map(|r| r.length).collect();
        assert_eq!(own, vec![None, Some(12), Some(10), None]);
        // The rimshot comes back round on step 12 of the first bar.
        let notes = p.bake(p.rate * 16);
        assert!(notes.iter().any(|n| n.pitch == STICK && n.start == 12 * p.rate));
    }

    #[test]
    fn ties_hold_and_ratchets_split() {
        let s = Starter { genre: "", name: "", rate: S16, swing: 0.0, rows: &[(KICK, "x_..3...")] };
        let notes = s.pattern().unwrap().bake(8 * S16);
        assert_eq!(notes[0].length, 2 * S16);
        assert_eq!(notes[1..].iter().map(|n| n.start).collect::<Vec<_>>(), vec![4 * S16, 4 * S16 + S16 / 3, 4 * S16 + 2 * S16 / 3]);
    }
}
