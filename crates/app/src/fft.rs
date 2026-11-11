use std::f32::consts::PI;

/// In-place radix-2 FFT; `re` and `im` must have the same power-of-two length.
pub fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

/// Hann-windowed magnitude spectrum in dBFS, one value per bin up to Nyquist.
pub fn spectrum_db(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    let window: Vec<f32> = (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos()).collect();
    let gain: f32 = window.iter().sum::<f32>() / 2.0;
    let mut re: Vec<f32> = samples.iter().zip(&window).map(|(s, w)| s * w).collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    (0..n / 2).map(|k| 20.0 * ((re[k] * re[k] + im[k] * im[k]).sqrt() / gain).max(1e-9).log10()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_lands_in_its_bin() {
        let n = 1024;
        let bin = 64;
        let s: Vec<f32> = (0..n).map(|i| (2.0 * PI * bin as f32 * i as f32 / n as f32).sin() * 0.5).collect();
        let db = spectrum_db(&s);
        let peak = db.iter().enumerate().fold((0, f32::MIN), |m, (i, v)| if *v > m.1 { (i, *v) } else { m });
        assert_eq!(peak.0, bin);
        assert!((peak.1 - 20.0 * 0.5f32.log10()).abs() < 0.1, "{}", peak.1);
    }
}
