use solstice_engine::audio::AudioData;

const BASE: usize = 256;
const FACTOR: usize = 4;

/// Min/max overview of a file at several resolutions, per channel.
/// Level `k` holds one pair per `256 * 4^k` frames.
pub struct Peaks {
    channels: Vec<Vec<Vec<(f32, f32)>>>,
}

impl Peaks {
    pub fn build(data: &AudioData) -> Peaks {
        let channels = (0..data.channels.max(1))
            .map(|ch| {
                let mut levels = vec![data.peaks(ch, BASE)];
                while levels.last().unwrap().len() > 64 {
                    let prev = levels.last().unwrap();
                    let next = prev
                        .chunks(FACTOR)
                        .map(|c| c.iter().fold((0f32, 0f32), |a, p| (a.0.min(p.0), a.1.max(p.1))))
                        .collect();
                    levels.push(next);
                }
                levels
            })
            .collect();
        Peaks { channels }
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// One min/max pair per pixel column of one channel, for `cols` columns from `frame`.
    pub fn channel_columns(&self, ch: usize, frame: f64, frames_per_px: f64, cols: usize) -> Vec<(f32, f32)> {
        let levels = &self.channels[ch.min(self.channels.len() - 1)];
        let mut level = 0;
        let mut block = BASE as f64;
        while level + 1 < levels.len() && block * FACTOR as f64 <= frames_per_px {
            level += 1;
            block *= FACTOR as f64;
        }
        let data = &levels[level];
        (0..cols)
            .map(|c| {
                let a = ((frame + c as f64 * frames_per_px) / block).max(0.0) as usize;
                let b = (((frame + (c + 1) as f64 * frames_per_px) / block).ceil() as usize).max(a + 1);
                data.get(a..b.min(data.len()))
                    .unwrap_or(&[])
                    .iter()
                    .fold((0f32, 0f32), |acc, p| (acc.0.min(p.0), acc.1.max(p.1)))
            })
            .collect()
    }

    /// All channels merged, for the overview drawn on regions.
    pub fn columns(&self, frame: f64, frames_per_px: f64, cols: usize) -> Vec<(f32, f32)> {
        let mut out = self.channel_columns(0, frame, frames_per_px, cols);
        for ch in 1..self.channels.len() {
            for (o, p) in out.iter_mut().zip(self.channel_columns(ch, frame, frames_per_px, cols)) {
                o.0 = o.0.min(p.0);
                o.1 = o.1.max(p.1);
            }
        }
        out
    }
}
