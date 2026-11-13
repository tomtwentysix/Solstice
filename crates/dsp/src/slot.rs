//! Hands a file-backed resource, a capture or an impulse response, to an effect running on the
//! audio thread. Loading and freeing happen elsewhere; the audio thread only swaps a pointer.

use crate::Native;
use crate::bank::Bank;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A file as saved: `file` as the project holds it (relative to its audio folder once copied
/// in) and `source`, the file it came from, for when `file` is missing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileDesc {
    pub file: PathBuf,
    pub source: PathBuf,
}

impl FileDesc {
    pub fn name(&self) -> String {
        self.file.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned())
    }
}

pub trait Loaded: Sized + Send + Sync + 'static {
    /// Marks the file in saved state, after the parameter bank.
    const TAG: &'static str;

    fn load(path: &Path, desc: FileDesc, sample_rate: f64) -> Result<Self, String>;
    fn desc(&self) -> &FileDesc;
    fn slot(native: &Native) -> Option<&Arc<Slot<Self>>>;

    /// Frames of latency it adds.
    fn latency(&self) -> u32 {
        0
    }
}

/// What an effect plays. Replaced items are kept here until the audio thread has let go of
/// them, so it never frees one.
pub struct Slot<T> {
    current: Mutex<Option<Arc<T>>>,
    retired: Mutex<Vec<Arc<T>>>,
    /// A saved file that could not be loaded, kept so saving does not forget it.
    missing: Mutex<Option<(FileDesc, String)>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Slot<T> {
        Slot { current: Mutex::new(None), retired: Mutex::new(Vec::new()), missing: Mutex::new(None) }
    }
}

impl<T: Loaded> Slot<T> {
    pub fn set(&self, item: Option<T>) {
        *self.missing.lock().unwrap() = None;
        let new = item.map(Arc::new);
        let old = std::mem::replace(&mut *self.current.lock().unwrap(), new);
        let mut retired = self.retired.lock().unwrap();
        retired.extend(old);
        retired.retain(|c| Arc::strong_count(c) > 1);
    }

    pub fn current(&self) -> Option<Arc<T>> {
        self.current.lock().unwrap().clone()
    }

    /// Plays nothing, remembering `desc` and why it could not be loaded.
    pub fn set_missing(&self, desc: FileDesc, error: String) {
        self.set(None);
        *self.missing.lock().unwrap() = Some((desc, error));
    }

    pub fn missing(&self) -> Option<(FileDesc, String)> {
        self.missing.lock().unwrap().clone()
    }

    pub fn desc(&self) -> Option<FileDesc> {
        self.current().map(|c| c.desc().clone()).or_else(|| self.missing().map(|m| m.0))
    }

    pub fn latency(&self) -> u32 {
        self.current().map_or(0, |c| c.latency())
    }

    /// For the audio thread: points `held` at the current item, unless it is being swapped right
    /// now. Dropping the old pointer never frees it, as `retired` still holds it.
    pub(crate) fn follow(&self, held: &mut Option<Arc<T>>) {
        let Ok(c) = self.current.try_lock() else { return };
        let same = match (&*c, &*held) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            *held = c.clone();
        }
    }

    /// Saved state: the parameter bank, then the file and its source.
    pub fn state(&self, bank: &Bank) -> Vec<u8> {
        state::<T>(bank, self.desc().as_ref())
    }
}

pub fn state<T: Loaded>(bank: &Bank, desc: Option<&FileDesc>) -> Vec<u8> {
    let mut s = String::from_utf8(bank.state()).unwrap_or_default();
    if let Some(d) = desc {
        s.push_str(&format!("{}\n{}\n{}\n", T::TAG, d.file.display(), d.source.display()));
    }
    s.into_bytes()
}

/// Restores the bank and returns the file, which the caller loads.
pub fn set_state<T: Loaded>(bank: &Bank, data: &[u8]) -> Option<FileDesc> {
    let text = String::from_utf8_lossy(data);
    let marker = format!("{}\n", T::TAG);
    let (params, rest) = text.split_once(&marker).unwrap_or((&text, ""));
    bank.set_state(params.as_bytes());
    let mut lines = rest.lines();
    let file = PathBuf::from(lines.next().filter(|l| !l.is_empty())?);
    Some(FileDesc { file, source: lines.next().map(PathBuf::from).unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nam::{Capture, NAM};

    #[test]
    fn state_round_trip() {
        let bank = Bank::new(&NAM);
        bank.set(0, 6.0);
        let desc = FileDesc { file: "Clean Amp.nam".into(), source: "/lib/Guitar/Clean Amp.nam".into() };
        let data = state::<Capture>(&bank, Some(&desc));
        assert!(String::from_utf8_lossy(&data).contains("#capture\nClean Amp.nam\n"));
        let back = Bank::new(&NAM);
        assert_eq!(set_state::<Capture>(&back, &data), Some(desc));
        assert_eq!(back.get(0), 6.0);
        assert_eq!(set_state::<Capture>(&back, &state::<Capture>(&bank, None)), None);
    }
}
