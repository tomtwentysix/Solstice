//! A small electronic kit synthesised from scratch, so the Drum Kit has sounds before any
//! samples are dragged in. One mono hit per pad, in `sampler::PADS` order.

use std::f32::consts::TAU;

struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

/// One-pole high-pass, for hats and cymbals.
struct HighPass {
    a: f32,
    x: f32,
    y: f32,
}

impl HighPass {
    fn new(hz: f32, sr: f32) -> Self {
        let rc = 1.0 / (TAU * hz);
        HighPass { a: rc / (rc + 1.0 / sr), x: 0.0, y: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        self.y = self.a * (self.y + x - self.x);
        self.x = x;
        self.y
    }
}

/// Two-pole band-pass (RBJ, constant peak gain).
struct BandPass {
    b0: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl BandPass {
    fn new(hz: f32, q: f32, sr: f32) -> Self {
        let w = TAU * hz / sr;
        let alpha = w.sin() / (2.0 * q);
        let a0 = 1.0 + alpha;
        BandPass { b0: alpha / a0, a1: -2.0 * w.cos() / a0, a2: (1.0 - alpha) / a0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * (x - self.x2) - self.a1 * self.y1 - self.a2 * self.y2;
        (self.x2, self.x1, self.y2, self.y1) = (self.x1, x, self.y1, y);
        y
    }
}

fn env(t: f32, decay: f32) -> f32 {
    (-t / decay).exp()
}

fn render(sr: f32, seconds: f32, mut f: impl FnMut(f32) -> f32) -> Vec<f32> {
    let mut v: Vec<f32> = (0..(sr * seconds) as usize).map(|i| f(i as f32 / sr)).collect();
    let peak = v.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    let gain = 0.89 / peak;
    let fade = (sr * 0.005) as usize;
    let n = v.len();
    for (i, s) in v.iter_mut().enumerate() {
        *s *= gain * ((n - i) as f32 / fade as f32).min(1.0);
    }
    v
}

/// A pitched drum: a sine that drops from `from` to `to` Hz, with optional noise on top.
fn tom(sr: f32, from: f32, to: f32, decay: f32, noise: f32, seed: u32) -> Vec<f32> {
    let mut n = Noise(seed);
    let mut phase = 0.0f32;
    render(sr, decay * 6.0, |t| {
        let hz = to + (from - to) * env(t, 0.04);
        phase += TAU * hz / sr;
        phase.sin() * env(t, decay) + n.next() * noise * env(t, 0.02)
    })
}

/// Metallic noise for hats and cymbals: square waves at inharmonic ratios, as the 808 does.
fn metal(sr: f32, decay: f32, hp: f32, seconds: f32, noise: f32, seed: u32) -> Vec<f32> {
    const RATIOS: [f32; 6] = [1.0, 1.4471, 1.6170, 1.9265, 2.5028, 2.6637];
    let mut phases = [0.0f32; 6];
    let mut filter = HighPass::new(hp, sr);
    let mut filter2 = HighPass::new(hp, sr);
    let mut n = Noise(seed);
    render(sr, seconds, |t| {
        let mut s = 0.0;
        for (p, r) in phases.iter_mut().zip(RATIOS) {
            *p = (*p + 330.0 * r / sr).fract();
            s += if *p < 0.5 { 1.0 } else { -1.0 };
        }
        filter2.run(filter.run(s / 6.0 + n.next() * noise)) * env(t, decay)
    })
}

pub fn kit(sample_rate: u32) -> Vec<Vec<f32>> {
    let sr = sample_rate as f32;
    let kick = {
        let mut phase = 0.0f32;
        render(sr, 0.6, |t| {
            let hz = 48.0 + 110.0 * env(t, 0.03);
            phase += TAU * hz / sr;
            (phase.sin() * env(t, 0.25) * 1.4).tanh() + (t < 0.002) as u8 as f32 * 0.3
        })
    };
    let snare = {
        let (mut n, mut bp) = (Noise(7), BandPass::new(4000.0, 0.6, sr));
        let mut phase = 0.0f32;
        render(sr, 0.4, |t| {
            phase += TAU * (185.0 + 60.0 * env(t, 0.01)) / sr;
            phase.sin() * env(t, 0.05) * 0.8 + bp.run(n.next()) * env(t, 0.09) * 2.0
        })
    };
    let side_stick = {
        let (mut n, mut bp) = (Noise(11), BandPass::new(1700.0, 4.0, sr));
        render(sr, 0.12, |t| bp.run(n.next()) * env(t, 0.015) * 6.0 + (TAU * 480.0 * t).sin() * env(t, 0.01))
    };
    let clap = {
        let (mut n, mut bp) = (Noise(13), BandPass::new(1200.0, 1.2, sr));
        render(sr, 0.45, |t| {
            // Three quick bursts then the tail, as in the 808's clap.
            let bursts = [0.0, 0.011, 0.023].iter().map(|s| if t >= *s { env(t - s, 0.006) } else { 0.0 }).fold(0.0, f32::max);
            let tail = if t >= 0.03 { env(t - 0.03, 0.12) * 0.6 } else { 0.0 };
            bp.run(n.next()) * (bursts + tail)
        })
    };
    let closed_hat = metal(sr, 0.035, 7000.0, 0.25, 0.3, 17);
    let pedal_hat = metal(sr, 0.06, 6000.0, 0.3, 0.2, 19);
    let open_hat = metal(sr, 0.35, 7000.0, 1.6, 0.3, 23);
    let crash = metal(sr, 0.9, 4500.0, 3.5, 1.0, 29);
    let ride = {
        let mut bell = 0.0f32;
        let mut wash = metal(sr, 1.2, 5000.0, 3.0, 0.4, 31).into_iter();
        render(sr, 3.0, |t| {
            bell += TAU * 2900.0 / sr;
            wash.next().unwrap_or(0.0) * 0.8 + bell.sin() * env(t, 0.4) * 0.15
        })
    };
    let tambourine = {
        let mut jingle = metal(sr, 0.12, 8000.0, 0.5, 0.6, 37).into_iter();
        render(sr, 0.5, |t| jingle.next().unwrap_or(0.0) * (1.0 + 0.5 * (TAU * 22.0 * t).sin()))
    };
    let cowbell = {
        let (mut a, mut b, mut bp) = (0.0f32, 0.0f32, BandPass::new(800.0, 1.0, sr));
        render(sr, 0.5, |t| {
            a = (a + 540.0 / sr).fract();
            b = (b + 800.0 / sr).fract();
            let s = (if a < 0.5 { 1.0 } else { -1.0 }) + (if b < 0.5 { 1.0 } else { -1.0 });
            bp.run(s) * (env(t, 0.02) * 0.6 + env(t, 0.12) * 0.4)
        })
    };
    let shaker = {
        let (mut n, mut hp) = (Noise(41), HighPass::new(5000.0, sr));
        render(sr, 0.18, |t| hp.run(n.next()) * (t / 0.03).min(1.0) * env(t, 0.05))
    };
    let claves = render(sr, 0.15, |t| (TAU * 2500.0 * t).sin() * env(t, 0.025));
    vec![
        kick,
        snare,
        side_stick,
        clap,
        closed_hat,
        pedal_hat,
        open_hat,
        crash,
        tom(sr, 140.0, 85.0, 0.22, 0.05, 43),
        tom(sr, 190.0, 120.0, 0.2, 0.05, 47),
        tom(sr, 260.0, 170.0, 0.18, 0.05, 53),
        ride,
        tambourine,
        cowbell,
        shaker,
        claves,
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_pad_sounds() {
        let kit = super::kit(48000);
        assert_eq!(kit.len(), crate::sampler::PADS.len());
        for (i, hit) in kit.iter().enumerate() {
            let peak = hit.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.5 && peak <= 0.9, "pad {i} peaks at {peak}");
            assert!(hit.iter().all(|s| s.is_finite()));
        }
    }
}
