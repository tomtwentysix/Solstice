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
}

impl Drop for Model {
    fn drop(&mut self) {
        unsafe { sn_free(self.ptr) };
    }
}
