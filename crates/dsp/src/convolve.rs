//! Partitioned convolution with no latency. The first `HEAD` taps run directly; the rest run in
//! two uniformly partitioned FFT stages, short blocks then long ones. A stage with blocks of k
//! samples starts k samples into the response, so a block's result is due just as its input is
//! complete. Every buffer is sized when the response is set, so `run` never allocates. The long
//! stage does its work once every `LONG` samples rather than spreading it out.

/// Taps run directly, and the short stage's block size.
const HEAD: usize = 64;
/// The long stage's block size, and where it starts.
const LONG: usize = 1024;

/// A radix-2 complex FFT of one power-of-two size, with its tables made up front.
pub(crate) struct Fft {
    rev: Vec<u32>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub(crate) fn new(n: usize) -> Fft {
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let angle = |j: usize| 2.0 * std::f64::consts::PI * j as f64 / n as f64;
        Fft { rev, cos: (0..n / 2).map(|j| angle(j).cos() as f32).collect(), sin: (0..n / 2).map(|j| angle(j).sin() as f32).collect() }
    }

    pub(crate) fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        for (i, &j) in self.rev.iter().enumerate() {
            let j = j as usize;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let (half, step) = (len / 2, n / len);
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * step], -self.sin[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            len <<= 1;
        }
    }
}

/// Convolves with `taps` of the response, in blocks of `k`, its output `k` samples late.
struct Stage {
    k: usize,
    parts: usize,
    fft: Fft,
    /// Spectra of the response's partitions, then of the latest inputs, `k + 1` bins each since
    /// the rest mirror them.
    h_re: Vec<f32>,
    h_im: Vec<f32>,
    x_re: Vec<f32>,
    x_im: Vec<f32>,
    /// Which input spectrum is the newest.
    newest: usize,
    input: Vec<f32>,
    out: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    pos: usize,
}

impl Stage {
    fn new(taps: &[f32], k: usize) -> Stage {
        let n = 2 * k;
        let bins = k + 1;
        let parts = taps.len().div_ceil(k);
        let fft = Fft::new(n);
        let (mut h_re, mut h_im) = (Vec::with_capacity(parts * bins), Vec::with_capacity(parts * bins));
        let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
        for part in taps.chunks(k) {
            re.fill(0.0);
            im.fill(0.0);
            // The inverse transform's 1/n is folded in here.
            part.iter().zip(&mut re).for_each(|(h, r)| *r = h / n as f32);
            fft.forward(&mut re, &mut im);
            h_re.extend_from_slice(&re[..bins]);
            h_im.extend_from_slice(&im[..bins]);
        }
        Stage {
            k,
            parts,
            fft,
            h_re,
            h_im,
            x_re: vec![0.0; parts * bins],
            x_im: vec![0.0; parts * bins],
            newest: 0,
            input: vec![0.0; n],
            out: vec![0.0; k],
            re,
            im,
            pos: 0,
        }
    }

    #[inline]
    fn push(&mut self, x: f32) -> f32 {
        let y = self.out[self.pos];
        self.input[self.k + self.pos] = x;
        self.pos += 1;
        if self.pos == self.k {
            self.block();
            self.pos = 0;
        }
        y
    }

    /// Overlap-save over the last two blocks of input.
    fn block(&mut self) {
        let (k, bins) = (self.k, self.k + 1);
        self.re.copy_from_slice(&self.input);
        self.im.fill(0.0);
        self.fft.forward(&mut self.re, &mut self.im);
        self.newest = (self.newest + self.parts - 1) % self.parts;
        let at = self.newest * bins;
        self.x_re[at..at + bins].copy_from_slice(&self.re[..bins]);
        self.x_im[at..at + bins].copy_from_slice(&self.im[..bins]);
        self.re[..bins].fill(0.0);
        self.im[..bins].fill(0.0);
        for p in 0..self.parts {
            let x = (self.newest + p) % self.parts * bins;
            let h = p * bins;
            for b in 0..bins {
                let (xr, xi, hr, hi) = (self.x_re[x + b], self.x_im[x + b], self.h_re[h + b], self.h_im[h + b]);
                self.re[b] += xr * hr - xi * hi;
                self.im[b] += xr * hi + xi * hr;
            }
        }
        // The inverse transform as a forward one of the conjugate; only the real part is used.
        for b in 1..k {
            self.re[2 * k - b] = self.re[b];
            self.im[2 * k - b] = self.im[b];
        }
        self.im[..bins].iter_mut().for_each(|v| *v = -*v);
        self.fft.forward(&mut self.re, &mut self.im);
        self.out.copy_from_slice(&self.re[k..]);
        self.input.copy_within(k.., 0);
    }
}

pub struct Convolver {
    /// The first taps, reversed.
    head: Vec<f32>,
    /// The latest inputs, written twice so the newest `HEAD` always lie in one run.
    history: Vec<f32>,
    at: usize,
    stages: Vec<Stage>,
}

impl Convolver {
    pub fn new(response: &[f32]) -> Convolver {
        let mut head = vec![0.0; HEAD];
        response.iter().take(HEAD).enumerate().for_each(|(i, &h)| head[HEAD - 1 - i] = h);
        let mut stages = Vec::new();
        if response.len() > HEAD {
            stages.push(Stage::new(&response[HEAD..response.len().min(LONG)], HEAD));
        }
        if response.len() > LONG {
            stages.push(Stage::new(&response[LONG..], LONG));
        }
        Convolver { head, history: vec![0.0; 2 * HEAD], at: 0, stages }
    }

    pub fn run(&mut self, io: &mut [f32]) {
        for s in io {
            let x = *s;
            self.history[self.at] = x;
            self.history[self.at + HEAD] = x;
            let recent = &self.history[self.at + 1..=self.at + HEAD];
            let mut y: f32 = recent.iter().zip(&self.head).map(|(a, b)| a * b).sum();
            self.at = (self.at + 1) % HEAD;
            for st in &mut self.stages {
                y += st.push(x);
            }
            *s = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 8) as f32 / (1u32 << 23) as f32 - 1.0
            })
            .collect()
    }

    fn direct(x: &[f32], h: &[f32]) -> Vec<f32> {
        (0..x.len()).map(|n| (0..h.len().min(n + 1)).map(|j| x[n - j] as f64 * h[j] as f64).sum::<f64>() as f32).collect()
    }

    #[test]
    fn matches_direct_convolution_with_no_latency() {
        let x = noise(6000, 1);
        for len in [1, 63, 64, 65, 200, 1023, 1024, 1025, 3000] {
            let h: Vec<f32> = noise(len, len as u32).iter().enumerate().map(|(i, v)| v * (-(i as f32) / 600.0).exp()).collect();
            let want = direct(&x, &h);
            let mut c = Convolver::new(&h);
            let mut got = x.clone();
            let sizes = [1, 64, 17, 1024, 300, 5];
            let (mut at, mut k) = (0, 0);
            assert_no_alloc::assert_no_alloc(|| {
                while at < got.len() {
                    let n = sizes[k % sizes.len()].min(got.len() - at);
                    c.run(&mut got[at..at + n]);
                    at += n;
                    k += 1;
                }
            });
            let err = want.iter().zip(&got).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            let peak = want.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(err < 1e-4 * peak.max(1.0), "length {len}: error {err}");
        }
    }

    #[test]
    fn an_impulse_passes_straight_through() {
        let x = noise(3000, 7);
        let mut late = vec![0.0; 1500];
        late[1300] = 1.0;
        for (h, delay) in [(vec![1.0], 0), (late, 1300)] {
            let mut y = x.clone();
            Convolver::new(&h).run(&mut y);
            for i in delay..x.len() {
                assert!((y[i] - x[i - delay]).abs() < 1e-5, "delay {delay} at {i}");
            }
        }
    }
}
