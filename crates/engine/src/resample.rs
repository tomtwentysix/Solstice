//! Offline sample-rate conversion for audio brought into a project at another rate: a
//! Kaiser-windowed sinc, low-passed below the lower of the two Nyquist frequencies.

const ZERO_CROSSINGS: f64 = 32.0;
const BETA: f64 = 9.0;
/// The low-pass sits a little under Nyquist so the window's transition band stays out of the
/// alias region.
const CUTOFF: f64 = 0.95;
const WINDOW_TABLE: usize = 4096;

pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() || from == 0 || to == 0 {
        return input.to_vec();
    }
    let step = from as f64 / to as f64;
    let cutoff = CUTOFF * (1.0 / step).min(1.0);
    let radius = ZERO_CROSSINGS / cutoff;
    // The window is tabulated, since evaluating the Bessel series for every tap of every output
    // sample would dominate the conversion.
    let norm = bessel_i0(BETA);
    let table: Vec<f64> = (0..=WINDOW_TABLE)
        .map(|k| {
            let r = k as f64 / WINDOW_TABLE as f64;
            bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / norm
        })
        .collect();
    let window = |r: f64| {
        let pos = r.abs().min(1.0) * WINDOW_TABLE as f64;
        let k = (pos as usize).min(WINDOW_TABLE - 1);
        let f = pos - k as f64;
        table[k] + (table[k + 1] - table[k]) * f
    };
    let len = (input.len() as f64 / step).round() as usize;
    (0..len)
        .map(|i| {
            let t = i as f64 * step;
            let first = (t - radius).ceil().max(0.0) as usize;
            let last = ((t + radius).floor() as usize).min(input.len() - 1);
            let mut sum = 0.0;
            for (j, x) in input.iter().enumerate().take(last + 1).skip(first) {
                let d = t - j as f64;
                sum += *x as f64 * cutoff * sinc(cutoff * d) * window(d / radius);
            }
            sum as f32
        })
        .collect()
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

/// The zeroth-order modified Bessel function, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-12 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize).map(|i| (std::f64::consts::TAU * freq * i as f64 / rate as f64).sin() as f32 * 0.5).collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Frequency from the count of upward zero crossings, ignoring the filter's edges.
    fn frequency(x: &[f32], rate: u32) -> f64 {
        let body = &x[x.len() / 10..x.len() * 9 / 10];
        let ups = body.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        ups as f64 * rate as f64 / body.len() as f64
    }

    #[test]
    fn keeps_pitch_and_duration() {
        for (from, to) in [(44100, 48000), (48000, 44100), (96000, 48000), (48000, 96000)] {
            let out = resample(&sine(1000.0, from, 1.0), from, to);
            assert_eq!(out.len(), to as usize);
            let f = frequency(&out, to);
            assert!((f - 1000.0).abs() < 2.0, "{from}->{to}: {f} Hz");
            let level = rms(&out[out.len() / 10..out.len() * 9 / 10]);
            assert!((level - 0.5 / 2f64.sqrt()).abs() < 0.005, "{from}->{to}: level {level}");
        }
    }

    #[test]
    fn removes_what_the_new_rate_cannot_hold() {
        // 30 kHz would fold down to 14.1 kHz at 44.1 kHz.
        let out = resample(&sine(30000.0, 96000, 1.0), 96000, 44100);
        let level = 20.0 * rms(&out[out.len() / 10..out.len() * 9 / 10]).log10();
        assert!(level < -80.0, "alias at {level} dBFS");
    }

    #[test]
    fn same_rate_is_untouched() {
        let x = sine(440.0, 48000, 0.1);
        assert_eq!(resample(&x, 48000, 48000), x);
    }
}
