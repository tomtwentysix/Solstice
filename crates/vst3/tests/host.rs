use solstice_engine::{MidiEvent, Plugin, ProcessContext};
use solstice_vst3::{Instance, scan};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

// The shim's module and instance registries are only ever used from the app's UI thread, so
// tests that load plug-ins take turns.
fn shim() -> MutexGuard<'static, ()> {
    static SHIM: Mutex<()> = Mutex::new(());
    SHIM.lock().unwrap_or_else(|e| e.into_inner())
}

fn sample(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vst3shim/build-examples/VST3/Release").join(name);
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipping: build the SDK samples with vst3shim/build-samples.sh");
        None
    }
}

const CTX: ProcessContext = ProcessContext { position: 0, tempo: 120.0, sig_num: 4, sig_den: 4, playing: true };

#[test]
fn delay_processes_and_restores_state() {
    let _shim = shim();
    let Some(path) = sample("adelay.vst3") else { return };
    let classes = scan(&path).unwrap();
    let info = classes.iter().find(|c| !c.is_instrument()).expect("effect class");
    let inst = Instance::open(info, 48000.0, 512).unwrap();
    let params = inst.params();
    assert!(!params.is_empty());
    let id = params.iter().find(|p| p.steps == 0).expect("continuous parameter").id;
    inst.set_param(id, 0.25);

    // Parameter changes reach the processor, and so its state, on the next process call.
    let mut proc = inst.processor();
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    l[0] = 1.0;
    r[0] = 1.0;
    proc.process(&mut l, &mut r, &[], &CTX);
    assert!(l.iter().all(|s| s.is_finite()));
    let state = inst.state();
    assert!(!state.is_empty());

    let other = Instance::open(info, 48000.0, 512).unwrap();
    assert!(other.set_state(&state));
    assert!((other.param(id) - 0.25).abs() < 1e-6);
}

#[test]
fn synth_plays_notes() {
    let _shim = shim();
    let Some(path) = sample("mda-vst3.vst3") else { return };
    let classes = scan(&path).unwrap();
    let info = classes.iter().find(|c| c.is_instrument() && c.name.contains("JX10")).expect("JX10");
    let inst = Instance::open(info, 48000.0, 512).unwrap();
    let mut proc = inst.processor();
    let (mut l, mut r) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    proc.process(&mut l, &mut r, &[], &CTX);
    assert!(l.iter().all(|&s| s == 0.0));
    let on = [MidiEvent { frame: 0, data: [0x90, 60, 100] }];
    let mut energy = 0.0;
    for i in 0..10 {
        l.fill(0.0);
        r.fill(0.0);
        proc.process(&mut l, &mut r, if i == 0 { &on } else { &[] }, &CTX);
        energy += l.iter().map(|s| s * s).sum::<f32>();
    }
    assert!(energy > 0.01, "no output from synth: {energy}");
}

#[test]
fn talkbox_side_chain() {
    let _shim = shim();
    let Some(path) = sample("mda-vst3.vst3") else { return };
    let classes = scan(&path).unwrap();
    let info = classes.iter().find(|c| c.name.contains("TalkBox")).expect("TalkBox");
    let inst = Instance::open(info, 48000.0, 512).unwrap();
    assert!(inst.has_sidechain());
    if let Some(delay) = sample("adelay.vst3") {
        let info = scan(&delay).unwrap().into_iter().find(|c| !c.is_instrument()).unwrap();
        assert!(!Instance::open(&info, 48000.0, 512).unwrap().has_sidechain());
    }

    let mut proc = inst.processor();
    let mut seed = 1u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
    };
    let mut phase = 0.0f32;
    let mut run = |proc: &mut Box<dyn Plugin>, key: Option<f32>| {
        let mut energy = 0.0;
        for _ in 0..40 {
            let mut l: Vec<f32> = (0..512).map(|_| noise()).collect();
            let mut r: Vec<f32> = (0..512).map(|_| noise()).collect();
            let k: Vec<f32> = (0..512)
                .map(|_| {
                    phase += 220.0 / 48000.0;
                    key.map_or(0.0, |a| a * (phase * std::f32::consts::TAU).sin())
                })
                .collect();
            proc.sidechain(key.map(|_| [&k[..], &k[..]]));
            proc.process(&mut l, &mut r, &[], &CTX);
            assert!(l.iter().chain(&r).all(|s| s.is_finite()));
            energy += l.iter().map(|s| s * s).sum::<f32>();
        }
        energy
    };

    // With its side-chain off, TalkBox takes its modulator from the main input.
    assert!(run(&mut proc, None) > 0.1, "unkeyed TalkBox is silent");
    assert!(inst.set_sidechain_active(true));
    assert!(!inst.set_sidechain_active(true));
    let silent = run(&mut proc, Some(0.0));
    let keyed = run(&mut proc, Some(0.5));
    assert!(silent < 1e-6, "a silent key still produced {silent}");
    assert!(keyed > 0.1, "the key didn't reach the side-chain input: {keyed}");
    assert!(inst.set_sidechain_active(false));
    assert!(run(&mut proc, None) > 0.1);
}

#[test]
fn side_chain_switches_while_processing() {
    let _shim = shim();
    let Some(path) = sample("mda-vst3.vst3") else { return };
    let info = scan(&path).unwrap().into_iter().find(|c| c.name.contains("TalkBox")).expect("TalkBox");
    let inst = Instance::open(&info, 48000.0, 256).unwrap();
    let mut proc = inst.processor();
    let key: Vec<f32> = (0..256).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    let input_l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.37).sin() * 0.3).collect();
    let input_r: Vec<f32> = (0..256).map(|i| (i as f32 * 0.21).cos() * 0.3).collect();
    let block = |proc: &mut Box<dyn Plugin>| {
        let (mut l, mut r) = (input_l.clone(), input_r.clone());
        proc.sidechain(Some([&key, &key]));
        proc.process(&mut l, &mut r, &[], &CTX);
        assert!(l.iter().chain(&r).all(|v| v.is_finite()));
        l == input_l && r == input_r
    };
    let done = AtomicBool::new(false);
    let passed = AtomicBool::new(false);
    let toggles = std::thread::scope(|s| {
        let rt = s.spawn(|| {
            while !done.load(Relaxed) {
                if block(&mut proc) {
                    passed.store(true, Relaxed);
                }
            }
        });
        let mut k = 0;
        while k < 2000 && !(passed.load(Relaxed) && k % 2 == 1) {
            assert!(inst.set_sidechain_active(k % 2 == 0));
            k += 1;
        }
        done.store(true, Relaxed);
        rt.join().unwrap();
        k
    });
    assert!(passed.load(Relaxed), "no block passed through untouched in {toggles} switches");
    assert!(!inst.set_sidechain_active(true), "the side-chain ends switched on");
    assert!(!block(&mut proc), "processing resumes once the side-chain is on");
}
