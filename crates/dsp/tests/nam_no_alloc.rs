use solstice_dsp::nam::{Capture, CaptureDesc};
use solstice_engine::ProcessContext;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

solstice_nam::alloc_check!();
use alloc_check::watch;

const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

fn captures() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../namshim/NeuralAmpModelerCore/example_models");
    ["wavenet.nam", "lstm.nam", "A2.nam"].iter().map(|f| dir.join(f)).collect()
}

#[test]
fn the_interposer_sees_rust_allocations_too() {
    let (v, caught) = watch(|| vec![0u8; 100]);
    assert!(caught > 0);
    drop(v);
}

// 44.1 kHz against captures made at 48 kHz, so the resampling path runs as well.
#[test]
fn processes_and_swaps_captures_without_allocating() {
    let sr = 44100.0;
    let (native, mut plugin) = solstice_dsp::create("solstice:nam", sr).unwrap();
    let slot = native.captures.clone().unwrap();
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
    for p in captures().iter().chain(captures().iter()) {
        let c = Capture::load(p, CaptureDesc { file: p.clone(), source: PathBuf::new() }, sr).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(c.latency > 0, "a 48 kHz capture at 44.1 kHz should be resampled");
        slot.set(Some(c));
        std::thread::sleep(Duration::from_millis(60));
    }
    slot.set(None);
    std::thread::sleep(Duration::from_millis(30));
    done.store(true, Ordering::Release);
    let (caught, finite, heard) = rt.join().unwrap();
    assert!(finite, "a capture produced non-finite output");
    assert!(heard, "no capture produced any output");
    assert_eq!(caught, 0, "the audio thread allocated or freed {caught} times");
    assert_eq!(native.current_latency(), 0);
}
