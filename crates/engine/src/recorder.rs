use crate::RecEvent;
use crate::audio::wav_spec;
use rtrb::Consumer;
use solstice_model::Id;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PEAK_BLOCK: usize = 512;

pub struct Take {
    pub track: Id,
    pub file: String,
    pub start: u64,
    pub frames: u64,
}

/// What the timeline draws for a take still being recorded.
#[derive(Clone, Default)]
pub struct LiveTake {
    pub start: u64,
    pub frames: u64,
    pub peaks: Vec<(f32, f32)>,
}

pub enum Control {
    Register(Id, Consumer<f32>),
    SetDir(PathBuf),
}

pub struct Recorder {
    pub control: Sender<Control>,
    pub takes: Receiver<Take>,
    pub live: Arc<Mutex<HashMap<Id, LiveTake>>>,
}

struct Writer {
    file: String,
    start: u64,
    channels: usize,
    written: u64,
    target: Option<u64>,
    wav: hound::WavWriter<BufWriter<File>>,
    peak: (f32, f32, usize),
}

/// Starts the disk thread that drains record rings into 32-bit float WAVs.
pub fn spawn(mut events: Consumer<RecEvent>, sample_rate: u32) -> Recorder {
    let (control_tx, control_rx) = channel();
    let (take_tx, take_rx) = channel();
    let live: Arc<Mutex<HashMap<Id, LiveTake>>> = Arc::default();
    let live2 = live.clone();
    std::thread::Builder::new()
        .name("solstice-disk".into())
        .spawn(move || {
            let mut dir = std::env::temp_dir();
            let mut rings: HashMap<Id, Consumer<f32>> = HashMap::new();
            let mut writers: HashMap<Id, Writer> = HashMap::new();
            let mut counter = 0u32;
            loop {
                loop {
                    match control_rx.try_recv() {
                        Ok(Control::Register(id, c)) => {
                            rings.insert(id, c);
                        }
                        Ok(Control::SetDir(d)) => dir = d,
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                while let Ok(ev) = events.pop() {
                    match ev {
                        RecEvent::Start { track, pos, channels } => {
                            // Data from a previous take is always drained before its Stop arrives.
                            counter += 1;
                            let stamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs());
                            let file = format!("Take {track}-{stamp}-{counter}.wav");
                            let _ = std::fs::create_dir_all(&dir);
                            match hound::WavWriter::create(dir.join(&file), wav_spec(channels, sample_rate)) {
                                Ok(wav) => {
                                    writers.insert(
                                        track,
                                        Writer {
                                            file,
                                            start: pos,
                                            channels: channels as usize,
                                            written: 0,
                                            target: None,
                                            wav,
                                            peak: (0.0, 0.0, 0),
                                        },
                                    );
                                    live2.lock().unwrap().insert(track, LiveTake { start: pos, ..Default::default() });
                                }
                                Err(e) => eprintln!("solstice: cannot create take: {e}"),
                            }
                        }
                        RecEvent::Stop { track, frames } => {
                            if let Some(w) = writers.get_mut(&track) {
                                w.target = Some(frames);
                            }
                        }
                    }
                    drain(&mut rings, &mut writers, &live2, &take_tx);
                }
                drain(&mut rings, &mut writers, &live2, &take_tx);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .expect("spawn disk thread");
    Recorder { control: control_tx, takes: take_rx, live }
}

fn drain(
    rings: &mut HashMap<Id, Consumer<f32>>,
    writers: &mut HashMap<Id, Writer>,
    live: &Mutex<HashMap<Id, LiveTake>>,
    takes: &Sender<Take>,
) {
    let mut done = Vec::new();
    for (id, w) in writers.iter_mut() {
        if let Some(ring) = rings.get_mut(id) {
            let mut frames = ring.slots() / w.channels;
            if let Some(t) = w.target {
                frames = frames.min((t - w.written) as usize);
            }
            if frames > 0
                && let Ok(chunk) = ring.read_chunk(frames * w.channels)
            {
                let mut new_peaks = Vec::new();
                for (i, s) in chunk.into_iter().enumerate() {
                    let _ = w.wav.write_sample(s);
                    w.peak.0 = w.peak.0.min(s);
                    w.peak.1 = w.peak.1.max(s);
                    if i % w.channels == w.channels - 1 {
                        w.peak.2 += 1;
                        if w.peak.2 == PEAK_BLOCK {
                            new_peaks.push((w.peak.0, w.peak.1));
                            w.peak = (0.0, 0.0, 0);
                        }
                    }
                }
                w.written += frames as u64;
                if let Some(l) = live.lock().unwrap().get_mut(id) {
                    l.frames = w.written;
                    l.peaks.extend(new_peaks);
                }
            }
        }
        if w.target.is_some_and(|t| w.written >= t || !rings.contains_key(id)) {
            done.push(*id);
        }
    }
    for id in done {
        let w = writers.remove(&id).unwrap();
        live.lock().unwrap().remove(&id);
        let frames = w.written;
        if w.wav.finalize().is_ok() && frames > 0 {
            let _ = takes.send(Take { track: id, file: w.file, start: w.start, frames });
        }
    }
}

pub const LIVE_PEAK_BLOCK: usize = PEAK_BLOCK;
