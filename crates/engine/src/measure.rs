use std::f32::consts::TAU;

pub const PROBE_LEN: usize = 512;

/// Sample `i` of the loopback probe: a Hann-windowed linear sweep from 500 Hz to a quarter of
/// the sample rate, peaking at -6 dBFS. A sweep survives an interface's DC blocking and
/// anti-aliasing filters better than a lone impulse and still correlates to a sharp peak.
pub fn probe(i: usize, sample_rate: u32) -> f32 {
    if i >= PROBE_LEN {
        return 0.0;
    }
    let sr = sample_rate as f32;
    let (f0, f1) = (500.0, sr / 4.0);
    let t = i as f32 / sr;
    let span = PROBE_LEN as f32 / sr;
    let phase = TAU * (f0 * t + (f1 - f0) * t * t / (2.0 * span));
    let window = 0.5 - 0.5 * (TAU * i as f32 / (PROBE_LEN - 1) as f32).cos();
    0.5 * window * phase.sin()
}

/// Where the probe starts in `captured`, in frames, found by cross-correlation. `None` when no
/// lag stands clearly above the rest or the loop returns less than -40 dB of the probe.
pub fn find_delay(captured: &[f32], sample_rate: u32) -> Option<usize> {
    let reference: Vec<f32> = (0..PROBE_LEN).map(|i| probe(i, sample_rate)).collect();
    let energy: f32 = reference.iter().map(|r| r * r).sum();
    let lags = captured.len().checked_sub(PROBE_LEN)? + 1;
    let corr: Vec<f32> =
        (0..lags).map(|k| reference.iter().zip(&captured[k..]).map(|(r, c)| r * c).sum::<f32>()).collect();
    // An interface may invert polarity, so the strongest lag either way counts.
    let (lag, peak) = corr.iter().enumerate().map(|(k, c)| (k, c.abs())).fold((0, 0.0), |a, b| if b.1 > a.1 { b } else { a });
    let rms = (corr.iter().map(|c| c * c).sum::<f32>() / lags as f32).sqrt();
    (peak >= 0.01 * energy && peak >= 8.0 * rms).then_some(lag)
}
