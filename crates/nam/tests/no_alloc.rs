use solstice_nam::Model;
use std::cell::Cell;
use std::ffi::{c_int, c_void};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

// assert_no_alloc only sees Rust's global allocator, and the C++ core allocates with malloc and
// operator new (which calls malloc), so the C entry points themselves are interposed for this
// binary and forwarded to glibc's own.
unsafe extern "C" {
    fn __libc_malloc(n: usize) -> *mut c_void;
    fn __libc_calloc(n: usize, size: usize) -> *mut c_void;
    fn __libc_realloc(p: *mut c_void, n: usize) -> *mut c_void;
    fn __libc_free(p: *mut c_void);
    fn __libc_memalign(align: usize, n: usize) -> *mut c_void;
    fn __libc_valloc(n: usize) -> *mut c_void;
    fn __libc_pvalloc(n: usize) -> *mut c_void;
}

thread_local! {
    static WATCHED: Cell<bool> = const { Cell::new(false) };
    static CAUGHT: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = WATCHED.try_with(|w| {
        if w.get() {
            let _ = CAUGHT.try_with(|c| c.set(c.get() + 1));
        }
    });
}

fn watch<R>(f: impl FnOnce() -> R) -> (R, usize) {
    CAUGHT.with(|c| c.set(0));
    WATCHED.with(|w| w.set(true));
    let r = f();
    WATCHED.with(|w| w.set(false));
    (r, CAUGHT.with(|c| c.get()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(n: usize) -> *mut c_void {
    note();
    unsafe { __libc_malloc(n) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(n: usize, size: usize) -> *mut c_void {
    note();
    unsafe { __libc_calloc(n, size) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(p: *mut c_void, n: usize) -> *mut c_void {
    note();
    unsafe { __libc_realloc(p, n) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(p: *mut c_void) {
    if !p.is_null() {
        note();
    }
    unsafe { __libc_free(p) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memalign(align: usize, n: usize) -> *mut c_void {
    note();
    unsafe { __libc_memalign(align, n) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn aligned_alloc(align: usize, n: usize) -> *mut c_void {
    note();
    unsafe { __libc_memalign(align, n) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_memalign(out: *mut *mut c_void, align: usize, n: usize) -> c_int {
    note();
    let p = unsafe { __libc_memalign(align, n) };
    if p.is_null() {
        return 12;
    }
    unsafe { *out = p };
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn valloc(n: usize) -> *mut c_void {
    note();
    unsafe { __libc_valloc(n) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvalloc(n: usize) -> *mut c_void {
    note();
    unsafe { __libc_pvalloc(n) }
}

const RATE: f64 = 48000.0;
const BLOCK: usize = 256;

fn captures() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../namshim/NeuralAmpModelerCore/example_models");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "nam"))
        .collect();
    v.sort();
    v.extend(generated());
    v
}

// The examples have no ConvNet or Linear capture, so small ones are written here. A receptive
// field of 2048 takes Linear's FFT path and 64 its direct one.
fn generated() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("nam-captures");
    std::fs::create_dir_all(&dir).unwrap();
    let linear = |rf: usize| {
        let w: Vec<String> = (0..rf)
            .map(|i| format!("{}", (-0.001 * i as f64).exp() * 0.01))
            .collect();
        format!(
            r#"{{"version":"0.5.4","architecture":"Linear","sample_rate":48000,"config":{{"receptive_field":{rf},"bias":true}},"weights":[{},0.03]}}"#,
            w.join(",")
        )
    };
    let convnet = r#"{"version":"0.5.4","architecture":"ConvNet","sample_rate":48000,"config":{"channels":2,"dilations":[1,2],"batchnorm":false,"activation":"ReLU"},"weights":[0.5,0.5,0.5,0.5,0,0,0.25,0.25,0.25,0.25,0.25,0.25,0.25,0.25,0,0,0.5,0.5,0]}"#;
    let batchnorm = r#"{"version":"0.5.4","architecture":"ConvNet","sample_rate":48000,"config":{"channels":1,"dilations":[1],"batchnorm":true,"activation":"Tanh"},"weights":[0.7,0.3,0,1,1,0,0.00001,0.9,0.01]}"#;
    [
        ("convnet.nam", convnet.to_string()),
        ("convnet_batchnorm.nam", batchnorm.to_string()),
        ("linear_direct.nam", linear(64)),
        ("linear_fft.nam", linear(2048)),
    ]
    .into_iter()
    .map(|(name, json)| {
        let p = dir.join(name);
        std::fs::write(&p, json).unwrap();
        p
    })
    .collect()
}

fn signal() -> Vec<f32> {
    (0..BLOCK * 64)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            0.4 * (t * 110.0 * std::f32::consts::TAU).sin()
                + 0.1 * (t * 330.0 * std::f32::consts::TAU).sin()
        })
        .collect()
}

#[test]
fn interposer_sees_cpp_allocations() {
    let path = &captures()[0];
    let (model, caught) = watch(|| Model::load(path, RATE, BLOCK));
    assert!(model.is_ok());
    assert!(
        caught > 0,
        "loading a capture should have been seen allocating"
    );
    let (_, caught) = watch(|| drop(model));
    assert!(caught > 0, "freeing a capture should have been seen");
}

#[test]
fn processes_and_swaps_captures_without_allocating() {
    static PENDING: AtomicPtr<Model> = AtomicPtr::new(std::ptr::null_mut());
    static RETIRED: AtomicPtr<Model> = AtomicPtr::new(std::ptr::null_mut());
    static DONE: AtomicBool = AtomicBool::new(false);

    let paths = captures();
    assert!(paths.len() >= 2);
    let input = signal();
    let rt = std::thread::spawn(move || {
        let mut out = vec![0.0f32; BLOCK];
        let mut current: *mut Model = std::ptr::null_mut();
        let mut swaps = 0usize;
        let mut sound = true;
        let mut heard = false;
        let ((), caught) = watch(|| {
            let sizes = [BLOCK, 64, 1, BLOCK - 1, 128, 17];
            let mut pos = 0;
            let mut k = 0;
            while !DONE.load(Ordering::Acquire) || !PENDING.load(Ordering::Acquire).is_null() {
                let next = PENDING.swap(std::ptr::null_mut(), Ordering::AcqRel);
                if !next.is_null() {
                    if !current.is_null() {
                        RETIRED.store(current, Ordering::Release);
                    }
                    current = next;
                    swaps += 1;
                }
                if let Some(m) = unsafe { current.as_mut() } {
                    let n = sizes[k % sizes.len()];
                    k += 1;
                    if pos + n > input.len() {
                        pos = 0;
                    }
                    m.process(&input[pos..pos + n], &mut out[..n]);
                    pos += n;
                    sound &= out[..n].iter().all(|s| s.is_finite());
                    heard |= out[..n].iter().any(|s| s.abs() > 1e-6);
                }
            }
        });
        if !current.is_null() {
            drop(unsafe { Box::from_raw(current) });
        }
        (caught, swaps, sound, heard)
    });

    let mut loaded = 0;
    for p in &paths {
        let m = match Model::load(p, RATE, BLOCK) {
            Ok(m) => m,
            Err(e) => panic!("{}: {e}", p.display()),
        };
        loaded += 1;
        PENDING.store(Box::into_raw(Box::new(m)), Ordering::Release);
        while !PENDING.load(Ordering::Acquire).is_null() {
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(50));
        if loaded > 1 {
            let old = loop {
                let old = RETIRED.swap(std::ptr::null_mut(), Ordering::AcqRel);
                if !old.is_null() {
                    break old;
                }
                std::thread::yield_now();
            };
            drop(unsafe { Box::from_raw(old) });
        }
    }
    DONE.store(true, Ordering::Release);
    let (caught, swaps, sound, heard) = rt.join().unwrap();
    assert_eq!(swaps, loaded);
    assert!(sound, "a capture produced non-finite output");
    assert!(heard, "no capture produced any output");
    assert_eq!(
        caught, 0,
        "the audio thread allocated or freed {caught} times"
    );
}
