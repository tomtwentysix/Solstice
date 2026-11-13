//! Streaming band-limited rate conversion at a fixed ratio, for running a capture at the rate
//! it was made at. Every buffer is sized up front, so `push` and `pull` never allocate.

use std::f64::consts::PI;

/// Kernel positions per input sample; taps between them are interpolated.
const PHASES: usize = 64;
/// Kernel half-width in input samples when not narrowed for downsampling.
const HALF: usize = 16;

pub struct Stream {
    ring: Vec<f32>,
    mask: usize,
    /// Samples pushed so far, counting the silent lead-in.
    written: u64,
    /// The next output's position in the input, as a whole sample and a fraction.
    pos: u64,
    frac: f64,
    step: f64,
    half: usize,
    /// The kernel from -half to +half input samples, PHASES points per sample, plus a guard.
    kernel: Vec<f32>,
}

impl Stream {
    /// Converts `from` Hz to `to` Hz. `backlog` is the most input that will wait unread at
    /// once; `delay` is silence, in input samples, that the output starts with.
    pub fn new(from: f64, to: f64, backlog: usize, delay: f64) -> Stream {
        let step = from / to;
        let narrow = step.max(1.0);
        let half = (HALF as f64 * narrow).ceil() as usize;
        // Cutoff just below the lower Nyquist, in cycles per input sample.
        let fc = 0.45 / narrow;
        let kernel = (0..=2 * half * PHASES + 1)
            .map(|k| {
                let x = k as f64 / PHASES as f64 - half as f64;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { (2.0 * PI * fc * x).sin() / (2.0 * PI * fc * x) };
                let w = (x / half as f64).clamp(-1.0, 1.0);
                let blackman = 0.42 + 0.5 * (PI * w).cos() + 0.08 * (2.0 * PI * w).cos();
                (2.0 * fc * sinc * blackman) as f32
            })
            .collect();
        let whole = delay.max(0.0).ceil();
        let len = (backlog + whole as usize + 4 * half + 16).next_power_of_two();
        let lead = half as u64 + 1 + whole as u64;
        Stream { ring: vec![0.0; len], mask: len - 1, written: lead, pos: half as u64 + 1, frac: whole - delay.max(0.0), step, half, kernel }
    }

    pub fn push(&mut self, x: f32) {
        self.ring[self.written as usize & self.mask] = x;
        self.written += 1;
    }

    /// Whether enough input has arrived for the next output.
    pub fn ready(&self) -> bool {
        self.written > self.pos + self.half as u64
    }

    pub fn pull(&mut self) -> f32 {
        let fk = self.frac * PHASES as f64;
        let k0 = fk as usize;
        let f = (fk - k0 as f64) as f32;
        let first = self.pos + self.half as u64;
        let mut acc = 0.0;
        for j in 0..2 * self.half {
            let x = self.ring[(first - j as u64) as usize & self.mask];
            let k = j * PHASES + k0;
            let h = self.kernel[k] + (self.kernel[k + 1] - self.kernel[k]) * f;
            acc += x * h;
        }
        self.frac += self.step;
        let whole = self.frac.floor();
        self.pos += whole as u64;
        self.frac -= whole;
        acc
    }

    /// The kernel half-width, in input samples.
    pub fn half(&self) -> usize {
        self.half
    }
}

/// A down-and-back pair around a process that runs at another rate. The return leg starts late
/// by enough that it never waits for input within a block.
pub struct Around {
    pub there: Stream,
    pub back: Stream,
    /// Frames at the outer rate.
    pub latency: u32,
}

impl Around {
    /// `outer` is the session rate, `inner` the rate the process runs at, `block` the most
    /// outer frames per call.
    pub fn new(outer: f64, inner: f64, block: usize) -> Around {
        let r = inner / outer;
        let there = Stream::new(outer, inner, block, 0.0);
        let half_back = Stream::new(inner, outer, 0, 0.0).half();
        // A whole number of outer frames, so delay compensation lines up exactly.
        let latency = ((half_back as f64 + there.half() as f64 * r + 4.0) / r).ceil();
        let back = Stream::new(inner, outer, (block as f64 * r).ceil() as usize + 4, latency * r);
        Around { there, back, latency: latency as u32 }
    }

    /// Runs `f` on `io` at the inner rate, in chunks of at most `scratch.len()`.
    pub fn run(&mut self, io: &mut [f32], scratch: &mut [f32], mut f: impl FnMut(&mut [f32])) {
        let mut n = 0;
        for &x in io.iter() {
            self.there.push(x);
            while self.there.ready() {
                scratch[n] = self.there.pull();
                n += 1;
                if n == scratch.len() {
                    f(scratch);
                    scratch.iter().for_each(|&y| self.back.push(y));
                    n = 0;
                }
            }
        }
        if n > 0 {
            f(&mut scratch[..n]);
            scratch[..n].iter().for_each(|&y| self.back.push(y));
        }
        for y in io.iter_mut() {
            *y = if self.back.ready() { self.back.pull() } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * PI * freq * i as f64 / rate).sin() as f32).collect()
    }

    #[test]
    fn round_trip_is_a_clean_delay() {
        for (outer, inner) in [(44100.0, 48000.0), (96000.0, 48000.0), (48000.0, 44100.0)] {
            let input = sine(1000.0, outer, 48000);
            let mut out = input.clone();
            let mut a = Around::new(outer, inner, 1024);
            let mut scratch = vec![0.0; 512];
            let sizes = [1024, 1, 300, 17, 512, 999];
            let (mut at, mut k) = (0, 0);
            assert_no_alloc::assert_no_alloc(|| {
                while at < out.len() {
                    let n = sizes[k % sizes.len()].min(out.len() - at);
                    a.run(&mut out[at..at + n], &mut scratch, |_| {});
                    at += n;
                    k += 1;
                }
            });
            let lat = a.latency as usize;
            let err = (4000..input.len()).map(|i| (out[i] - input[i - lat]).abs()).fold(0.0, f32::max);
            assert!(err < 0.02, "{outer} via {inner}: error {err} at latency {lat}");
        }
    }

    #[test]
    fn downsampling_removes_what_the_inner_rate_cannot_hold() {
        // 30 kHz at 96 kHz would alias to 18 kHz at 48 kHz.
        let input = sine(30000.0, 96000.0, 96000);
        let mut s = Stream::new(96000.0, 48000.0, input.len(), 0.0);
        let mut out = Vec::new();
        for x in input {
            s.push(x);
            while s.ready() {
                out.push(s.pull());
            }
        }
        let peak = out[2000..].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak < 0.01, "{peak}");
    }
}
