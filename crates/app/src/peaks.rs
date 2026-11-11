use solstice_engine::audio::AudioData;

const BASE: usize = 256;
const FACTOR: usize = 4;

/// Min/max overview of a file at several resolutions, channels merged.
/// Level `k` holds one pair per `256 * 4^k` frames.
pub struct Peaks {
    levels: Vec<Vec<(f32, f32)>>,
}

impl Peaks {
    pub fn build(data: &AudioData) -> Peaks {
        let mut base = data.peaks(0, BASE);
        for ch in 1..data.channels {
            for (b, o) in base.iter_mut().zip(data.peaks(ch, BASE)) {
                b.0 = b.0.min(o.0);
                b.1 = b.1.max(o.1);
            }
        }
        let mut levels = vec![base];
        while levels.last().unwrap().len() > 64 {
            let prev = levels.last().unwrap();
            let next = prev
                .chunks(FACTOR)
                .map(|c| c.iter().fold((0f32, 0f32), |a, p| (a.0.min(p.0), a.1.max(p.1))))
                .collect();
            levels.push(next);
        }
        Peaks { levels }
    }

    /// One min/max pair per pixel column for `cols` columns starting at `frame`.
    pub fn columns(&self, frame: f64, frames_per_px: f64, cols: usize) -> Vec<(f32, f32)> {
        let mut level = 0;
        let mut block = BASE as f64;
        while level + 1 < self.levels.len() && block * FACTOR as f64 <= frames_per_px {
            level += 1;
            block *= FACTOR as f64;
        }
        let data = &self.levels[level];
        (0..cols)
            .map(|c| {
                let a = ((frame + c as f64 * frames_per_px) / block) as usize;
                let b = (((frame + (c + 1) as f64 * frames_per_px) / block).ceil() as usize).max(a + 1);
                data.get(a..b.min(data.len()))
                    .unwrap_or(&[])
                    .iter()
                    .fold((0f32, 0f32), |acc, p| (acc.0.min(p.0), acc.1.max(p.1)))
            })
            .collect()
    }
}
