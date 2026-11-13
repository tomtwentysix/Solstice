use crate::{Note, PPQ};
use serde::{Deserialize, Serialize};

/// A timing and velocity feel that repeats every `length` ticks.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Groove {
    pub name: String,
    pub length: u32,
    pub hits: Vec<GrooveHit>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GrooveHit {
    pub pos: u32,
    pub velocity: u8,
}

impl Groove {
    /// Starts closer together than a 64th note become one hit at the earliest start, with their
    /// velocities averaged.
    pub fn from_notes(name: &str, notes: &[Note], length: u32) -> Self {
        let length = length.max(1);
        let mut starts: Vec<&Note> = notes.iter().filter(|n| n.start < length).collect();
        starts.sort_by_key(|n| n.start);
        let mut clusters: Vec<(u32, Vec<u8>)> = Vec::new();
        for n in starts {
            match clusters.last_mut() {
                Some((pos, vels)) if n.start - *pos < PPQ / 16 => vels.push(n.velocity),
                _ => clusters.push((n.start, vec![n.velocity])),
            }
        }
        let hits = clusters
            .into_iter()
            .map(|(pos, vels)| {
                let sum: u32 = vels.iter().map(|v| *v as u32).sum();
                GrooveHit { pos, velocity: (sum as f32 / vels.len() as f32).round() as u8 }
            })
            .collect();
        Groove { name: name.to_string(), length, hits }
    }
}

fn swing(name: String, step: u32, percent: u32) -> Groove {
    let length = step * 2;
    Groove {
        name,
        length,
        hits: vec![GrooveHit { pos: 0, velocity: 100 }, GrooveHit { pos: length * percent / 100, velocity: 100 }],
    }
}

pub fn factory() -> Vec<Groove> {
    let percents = [54, 58, 62, 66];
    let mut grooves: Vec<Groove> = percents.iter().map(|p| swing(format!("Swing 8ths {p}%"), PPQ / 2, *p)).collect();
    grooves.extend(percents.iter().map(|p| swing(format!("Swing 16ths {p}%"), PPQ / 4, *p)));
    let sixteenth = PPQ / 4;
    let late = sixteenth / 10;
    grooves.push(Groove {
        name: "Laid Back 16ths".into(),
        length: PPQ,
        hits: vec![
            GrooveHit { pos: 0, velocity: 110 },
            GrooveHit { pos: sixteenth + late, velocity: 85 },
            GrooveHit { pos: sixteenth * 2, velocity: 95 },
            GrooveHit { pos: sixteenth * 3 + late, velocity: 85 },
        ],
    });
    grooves
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_notes_merges_and_sorts() {
        let notes = [
            Note::new(480, 10, 62, 80),
            Note::new(0, 10, 60, 100),
            Note::new(20, 10, 64, 60),
            Note::new(1920, 10, 60, 100),
        ];
        let g = Groove::from_notes("Mine", &notes, 1920);
        assert_eq!(g.hits, vec![GrooveHit { pos: 0, velocity: 80 }, GrooveHit { pos: 480, velocity: 80 }]);
    }

    #[test]
    fn factory_swing_positions() {
        let f = factory();
        let g = f.iter().find(|g| g.name == "Swing 16ths 66%").unwrap();
        assert_eq!((g.length, g.hits[1].pos), (PPQ / 2, PPQ / 2 * 66 / 100));
    }
}
