pub const PPQ: u32 = 960;

/// Converts between samples, ticks and bars for a constant tempo and signature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeMap {
    pub sample_rate: u32,
    pub tempo: f64,
    pub sig_num: u8,
    pub sig_den: u8,
}

impl TimeMap {
    pub fn new(sample_rate: u32, tempo: f64, sig_num: u8, sig_den: u8) -> Self {
        TimeMap { sample_rate, tempo, sig_num, sig_den }
    }

    pub fn samples_per_tick(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / self.tempo / PPQ as f64
    }

    pub fn ticks_per_beat(&self) -> u64 {
        PPQ as u64 * 4 / self.sig_den as u64
    }

    pub fn ticks_per_bar(&self) -> u64 {
        self.ticks_per_beat() * self.sig_num as u64
    }

    pub fn ticks_to_samples(&self, ticks: u64) -> u64 {
        (ticks as f64 * self.samples_per_tick()).round() as u64
    }

    pub fn samples_to_ticks(&self, samples: u64) -> u64 {
        (samples as f64 / self.samples_per_tick()).round() as u64
    }

    pub fn beat_len(&self) -> u64 {
        self.ticks_to_samples(self.ticks_per_beat())
    }

    pub fn bar_len(&self) -> u64 {
        self.ticks_to_samples(self.ticks_per_bar())
    }

    /// Returns 1-based (bar, beat, sixteenth, tick) as shown in the LCD.
    pub fn position(&self, samples: u64) -> (u64, u64, u64, u64) {
        let ticks = (samples as f64 / self.samples_per_tick()) as u64;
        let bar = ticks / self.ticks_per_bar();
        let rem = ticks % self.ticks_per_bar();
        let beat = rem / self.ticks_per_beat();
        let rem = rem % self.ticks_per_beat();
        let six = rem / (PPQ as u64 / 4);
        (bar + 1, beat + 1, six + 1, rem % (PPQ as u64 / 4))
    }

    pub fn seconds(&self, samples: u64) -> f64 {
        samples as f64 / self.sample_rate as f64
    }

    /// Rounds `samples` to the nearest multiple of `grid_ticks`.
    pub fn snap(&self, samples: u64, grid_ticks: u64) -> u64 {
        if grid_ticks == 0 {
            return samples;
        }
        let ticks = samples as f64 / self.samples_per_tick();
        let snapped = (ticks / grid_ticks as f64).round() as u64 * grid_ticks;
        self.ticks_to_samples(snapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        let t = TimeMap::new(48000, 120.0, 4, 4);
        assert_eq!(t.beat_len(), 24000);
        assert_eq!(t.bar_len(), 96000);
        assert_eq!(t.position(0), (1, 1, 1, 0));
        assert_eq!(t.position(96000 + 24000 + 6000), (2, 2, 2, 0));
        assert_eq!(t.snap(25000, PPQ as u64), 24000);
        let six_eight = TimeMap::new(48000, 120.0, 6, 8);
        assert_eq!(six_eight.bar_len(), 72000);
    }
}
