use std::ffi::{CStr, CString, c_char};
use std::path::Path;

#[repr(C)]
struct SnModel {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn sn_load(
        path: *const c_char,
        sr: f64,
        max_block: i32,
        err: *mut c_char,
        len: usize,
    ) -> *mut SnModel;
    fn sn_free(m: *mut SnModel);
    fn sn_expected_rate(m: *const SnModel) -> f64;
    fn sn_process(m: *mut SnModel, inp: *const f32, out: *mut f32, frames: i32);
}

/// A loaded capture. Loading and dropping allocate; `process` does not.
pub struct Model {
    ptr: *mut SnModel,
    max_block: usize,
}

// The model has no thread affinity; it is only ever used from one thread at a time.
unsafe impl Send for Model {}

impl Model {
    /// `sample_rate` is the rate the model will run at; 0 runs it at the capture's own.
    pub fn load(path: &Path, sample_rate: f64, max_block: usize) -> Result<Model, String> {
        let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let mut err = [0 as c_char; 512];
        let ptr = unsafe {
            sn_load(
                c.as_ptr(),
                sample_rate,
                max_block as i32,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        if ptr.is_null() {
            return Err(unsafe { CStr::from_ptr(err.as_ptr()) }
                .to_string_lossy()
                .into_owned());
        }
        Ok(Model { ptr, max_block })
    }

    /// The rate the capture was trained at, if it says.
    pub fn expected_rate(&self) -> Option<f64> {
        let r = unsafe { sn_expected_rate(self.ptr) };
        (r > 0.0).then_some(r)
    }

    pub fn max_block(&self) -> usize {
        self.max_block
    }

    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let n = input.len().min(output.len()).min(self.max_block);
        unsafe { sn_process(self.ptr, input.as_ptr(), output.as_mut_ptr(), n as i32) };
    }

    pub fn process_in_place(&mut self, io: &mut [f32]) {
        let n = io.len().min(self.max_block);
        unsafe { sn_process(self.ptr, io.as_ptr(), io.as_mut_ptr(), n as i32) };
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        unsafe { sn_free(self.ptr) };
    }
}

/// Defines `alloc_check::watch` in a test binary: it runs a closure and counts the heap calls
/// made on that thread meanwhile. assert_no_alloc only sees Rust's global allocator, and the C++
/// core allocates with malloc and operator new (which calls malloc), so the C entry points
/// themselves are interposed for the whole binary and forwarded to glibc's own. Rust's system
/// allocator goes through them too, so this catches both. It is a macro because the symbols
/// must live in the test binary's own objects to win over libc's.
#[macro_export]
macro_rules! alloc_check {
    () => {
        mod alloc_check {
            use std::cell::Cell;
            use std::ffi::{c_int, c_void};

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

            /// The closure's result and how many allocations and frees it made.
            pub fn watch<R>(f: impl FnOnce() -> R) -> (R, usize) {
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
        }
    };
}
