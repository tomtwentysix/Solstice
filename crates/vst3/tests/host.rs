use solstice_engine::{MidiEvent, ProcessContext};
use solstice_vst3::{Instance, scan};
use std::path::PathBuf;

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
