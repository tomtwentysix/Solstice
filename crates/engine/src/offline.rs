//! Faster-than-real-time rendering: a private engine driven in a loop on the caller's thread.
//!
//! The graph's plug-ins are the live ones, so the live engine must leave them alone for the
//! whole render (see `Command::Suspend`), and the graph comes back to the caller so that the
//! last references to plug-in instances are dropped on the UI thread.

use crate::{Command, Graph, pool};
use std::io;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, AtomicU64};

#[cfg(test)]
use assert_no_alloc::assert_no_alloc as guard;

#[cfg(not(test))]
fn guard<T>(f: impl FnOnce() -> T) -> T {
    f()
}

#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Finished { frames: u64 },
    Cancelled,
}

pub type Rendered = (io::Result<Outcome>, Option<Box<Graph>>);

/// Plays `span` through `g` and hands the output to `out` a block at a time, already
/// compensated for plug-in delay.
pub fn render(
    mut g: Graph,
    sr: u32,
    span: Range<u64>,
    workers: usize,
    progress: &Progress,
    out: &mut dyn FnMut(&[f32], &[f32]) -> io::Result<()>,
) -> Rendered {
    g.cycle = None;
    g.punch = None;
    g.midi_track = None;
    g.metronome = false;
    g.low_latency = None;
    g.bounce_ring = None;
    g.outputs = 0;
    for t in &mut g.tracks {
        t.armed = false;
        t.rt.rec = None;
    }
    let block = g.max_block;
    let pdc = g.pdc as u64;
    let len = span.end.saturating_sub(span.start);
    progress.total.store(len, Relaxed);
    progress.done.store(0, Relaxed);

    let (mut engine, mut handle, _rec) = crate::new(sr);
    engine.set_workers(workers, &pool::std_spawn, false);
    handle.set_graph(g);
    handle.send(Command::Locate(span.start));
    handle.send(Command::Play);

    let (mut l, mut r) = (vec![0.0; block], vec![0.0; block]);
    let (mut ran, mut written) = (0u64, 0u64);
    let result = loop {
        if written >= len {
            break Ok(Outcome::Finished { frames: written });
        }
        if progress.cancel.load(Relaxed) {
            break Ok(Outcome::Cancelled);
        }
        guard(|| engine.process(&[], &mut l, &mut r, &[]));
        let skip = pdc.saturating_sub(ran).min(block as u64) as usize;
        ran += block as u64;
        let take = ((block - skip) as u64).min(len - written) as usize;
        if take > 0 {
            if let Err(e) = out(&l[skip..skip + take], &r[skip..skip + take]) {
                break Err(e);
            }
            written += take as u64;
            progress.done.store(written, Relaxed);
        }
    };
    // One more block carries the note-offs from Stop to the instruments, so nothing hangs
    // over into live playback.
    handle.send(Command::Stop);
    guard(|| engine.process(&[], &mut l, &mut r, &[]));
    (result, engine.into_graph())
}

/// Renders to a 32-bit float stereo WAV. The file is written beside `path` and renamed into
/// place only once complete, so a cancelled or failed render leaves nothing behind.
pub fn render_wav(g: Graph, sr: u32, span: Range<u64>, workers: usize, progress: &Progress, path: &Path) -> Rendered {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    let mut writer = match hound::WavWriter::create(&part, crate::audio::wav_spec(2, sr)) {
        Ok(w) => w,
        Err(e) => return (Err(io::Error::other(e)), Some(Box::new(g))),
    };
    let (result, graph) = render(g, sr, span, workers, progress, &mut |l, r| {
        for (a, b) in l.iter().zip(r) {
            writer.write_sample(*a).map_err(io::Error::other)?;
            writer.write_sample(*b).map_err(io::Error::other)?;
        }
        Ok(())
    });
    let result = match result {
        Ok(Outcome::Finished { frames }) => writer
            .finalize()
            .map_err(io::Error::other)
            .and_then(|_| std::fs::rename(&part, path))
            .map(|_| Outcome::Finished { frames }),
        other => {
            drop(writer);
            other
        }
    };
    if !matches!(result, Ok(Outcome::Finished { .. })) {
        let _ = std::fs::remove_file(&part);
    }
    (result, graph)
}
