use solstice_dsp::ir::Ir;
use solstice_dsp::slot::{FileDesc, Loaded};
use solstice_engine::ProcessContext;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

solstice_nam::alloc_check!();
use alloc_check::watch;

const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

fn write(path: &Path, rate: u32, channels: &[Vec<f32>]) {
    let mut w = hound::WavWriter::create(path, solstice_engine::audio::wav_spec(channels.len() as u16, rate)).unwrap();
    for f in 0..channels[0].len() {
        channels.iter().for_each(|c| w.write_sample(c[f]).unwrap());
    }
    w.finalize().unwrap();
}

/// Decaying noise, `seconds` long.
fn response(rate: u32, seconds: f32, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..(rate as f32 * seconds) as usize)
        .map(|i| {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            ((s >> 8) as f32 / (1u32 << 23) as f32 - 1.0) * (-(i as f32) / (rate as f32 * seconds / 6.0)).exp()
        })
        .collect()
}

// A short mono cabinet, a stereo one at another rate, and a two-second stereo room, so every
// stage and the resampling on load are covered.
fn files(dir: &Path) -> Vec<PathBuf> {
    let set = [("cab.wav", 48000, vec![response(48000, 0.02, 1)]), ("stereo.wav", 44100, vec![response(44100, 0.1, 2), response(44100, 0.1, 3)]), ("room.wav", 96000, vec![response(96000, 2.0, 4), response(96000, 2.0, 5)])];
    set.into_iter()
        .map(|(name, rate, ch)| {
            write(&dir.join(name), rate, &ch);
            dir.join(name)
        })
        .collect()
}

#[test]
fn processes_and_swaps_responses_without_allocating() {
    let dir = std::env::temp_dir().join(format!("solstice-ir-alloc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths = files(&dir);
    let sr = 48000.0;
    let (native, mut plugin) = solstice_dsp::create("solstice:ir", sr).unwrap();
    let slot = native.irs.clone().unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let rt = {
        let done = done.clone();
        std::thread::spawn(move || {
            let mut l = vec![0.0f32; 4096];
            let mut r = vec![0.0f32; 4096];
            let sizes = [256, 64, 1, 4096, 17, 1500];
            let (mut k, mut t) = (0, 0usize);
            let mut finite = true;
            let mut heard = false;
            let ((), caught) = watch(|| {
                while !done.load(Ordering::Acquire) {
                    let n = sizes[k % sizes.len()];
                    k += 1;
                    for i in 0..n {
                        let x = 0.3 * (t as f32 * 220.0 * std::f32::consts::TAU / sr as f32).sin();
                        (l[i], r[i]) = (x, x * 0.5);
                        t += 1;
                    }
                    plugin.process(&mut l[..n], &mut r[..n], &[], &CTX);
                    finite &= l[..n].iter().chain(&r[..n]).all(|s| s.is_finite());
                    heard |= l[..n].iter().any(|s| s.abs() > 1e-4);
                }
            });
            drop(plugin);
            (caught, finite, heard)
        })
    };
    for p in paths.iter().chain(paths.iter()) {
        let ir = Ir::load(p, FileDesc { file: p.clone(), source: PathBuf::new() }, sr).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        slot.set(Some(ir));
        std::thread::sleep(Duration::from_millis(60));
    }
    slot.set(None);
    std::thread::sleep(Duration::from_millis(30));
    done.store(true, Ordering::Release);
    let (caught, finite, heard) = rt.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(finite, "a response produced non-finite output");
    assert!(heard, "no response produced any output");
    assert_eq!(caught, 0, "the audio thread allocated or freed {caught} times");
}
