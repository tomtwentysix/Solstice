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
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const PEAK_BLOCK: usize = 512;

pub struct Take {
    pub track: Id,
    pub file: String,
    pub start: u64,
    pub frames: u64,
    /// Frames the disk could not keep up with, written as silence.
    pub dropped: u64,
    /// Why the take stopped short of what was played, such as a full disk.
    pub error: Option<String>,
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

/// How often a take's header is brought up to date, so a crash leaves a playable file.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// A WAV records its size in 32 bits; a take that would pass this goes on in a new file.
const SPLIT_BYTES: u64 = 4_000_000_000;

struct Writer {
    file: String,
    dir: PathBuf,
    track: Id,
    spec: hound::WavSpec,
    start: u64,
    /// Frames written before the current file began, when a long take has been split.
    part_from: u64,
    part: u32,
    split_bytes: u64,
    channels: usize,
    /// Frames taken from the ring, the count the audio thread's Stop refers to.
    from_ring: u64,
    /// Frames in the file: those plus silence for dropped blocks.
    written: u64,
    dropped: u64,
    gaps: VecDeque<(u64, u64)>,
    target: Option<u64>,
    /// `None` when the file could not be created: the take's audio is still read from the
    /// ring, so the next take on the track starts with its own.
    wav: Option<hound::WavWriter<BufWriter<File>>>,
    error: Option<String>,
    flushed: Instant,
    peak: (f32, f32, usize),
}

impl Writer {
    /// Ends the current file as a take of its own and goes on in a new one.
    fn split(&mut self, takes: &Sender<Take>) {
        self.part += 1;
        let stem = self.file.trim_end_matches(".wav").split(" part ").next().unwrap_or("Take").to_string();
        let mut n = self.part + 1;
        let opened = loop {
            let name = format!("{stem} part {n}.wav");
            match std::fs::OpenOptions::new().write(true).create_new(true).open(self.dir.join(&name)) {
                Ok(f) => break hound::WavWriter::new(BufWriter::new(f), self.spec).map(|w| (name, w)).map_err(|e| e.to_string()),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => break Err(e.to_string()),
            }
        };
        let (name, wav) = match opened {
            Ok(o) => o,
            Err(e) => {
                self.error.get_or_insert(e);
                return;
            }
        };
        let start = self.start + self.written;
        note_started(&self.dir, self.track, start, &name);
        let old = self.wav.replace(wav);
        let file = std::mem::replace(&mut self.file, name);
        let error = old.and_then(|w| w.finalize().err()).map(|e| e.to_string());
        let _ = takes.send(Take { track: self.track, file, start: self.start + self.part_from, frames: self.written - self.part_from, dropped: 0, error });
        self.part_from = self.written;
    }

    fn part_full(&self) -> bool {
        self.wav.is_some() && (self.written - self.part_from) * self.channels as u64 * 4 >= self.split_bytes
    }

    fn put(&mut self, s: f32) {
        if self.error.is_none()
            && let Some(wav) = self.wav.as_mut()
            && let Err(e) = wav.write_sample(s)
        {
            self.error = Some(e.to_string());
        }
        self.peak.0 = self.peak.0.min(s);
        self.peak.1 = self.peak.1.max(s);
    }

    /// Ends a frame; returns a finished peak block.
    fn frame_done(&mut self) -> Option<(f32, f32)> {
        self.written += 1;
        self.peak.2 += 1;
        if self.peak.2 < PEAK_BLOCK {
            return None;
        }
        let p = (self.peak.0, self.peak.1);
        self.peak = (0.0, 0.0, 0);
        Some(p)
    }

    fn done(&self) -> bool {
        self.target.is_some_and(|t| self.from_ring >= t && self.gaps.front().is_none_or(|g| g.0 > t))
    }
}

/// Starts the disk thread that drains record rings into 32-bit float WAVs.
pub fn spawn(events: Consumer<RecEvent>, sample_rate: u32) -> Recorder {
    spawn_split_at(events, sample_rate, SPLIT_BYTES)
}

/// As `spawn`, splitting takes into files of at most `split_bytes` of audio.
pub fn spawn_split_at(mut events: Consumer<RecEvent>, sample_rate: u32, split_bytes: u64) -> Recorder {
    let (control_tx, control_rx) = channel();
    let (take_tx, take_rx) = channel();
    let live: Arc<Mutex<HashMap<Id, LiveTake>>> = Arc::default();
    let live2 = live.clone();
    std::thread::Builder::new()
        .name("solstice-disk".into())
        .spawn(move || {
            let mut dir = std::env::temp_dir();
            let mut rings: HashMap<Id, Consumer<f32>> = HashMap::new();
            // A track's takes in order: a new one can start before the last has drained.
            let mut writers: HashMap<Id, VecDeque<Writer>> = HashMap::new();
            let mut counter = 0u32;
            loop {
                let mut closed = false;
                loop {
                    match control_rx.try_recv() {
                        Ok(Control::Register(id, c)) => {
                            rings.insert(id, c);
                        }
                        Ok(Control::SetDir(d)) => dir = d,
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            closed = true;
                            break;
                        }
                    }
                }
                // Counted before the events are read: anything in the ring was pushed after the
                // events that come before it, so no take's data is read before its Start, Gap or
                // Stop is known.
                let ready: HashMap<Id, usize> = rings.iter().map(|(id, r)| (*id, r.slots())).collect();
                while let Ok(ev) = events.pop() {
                    match ev {
                        RecEvent::Start { track, pos, channels } => {
                            counter += 1;
                            let stamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |d| d.as_secs());
                            let _ = std::fs::create_dir_all(&dir);
                            let (file, wav, error) = match create(&dir, track, stamp, &mut counter, channels, sample_rate) {
                                Ok((file, wav)) => {
                                    note_started(&dir, track, pos, &file);
                                    live2.lock().unwrap().insert(track, LiveTake { start: pos, ..Default::default() });
                                    (file, Some(wav), None)
                                }
                                Err(e) => (String::new(), None, Some(format!("the take could not be created: {e}"))),
                            };
                            writers.entry(track).or_default().push_back(Writer {
                                file,
                                dir: dir.clone(),
                                track,
                                spec: wav_spec(channels, sample_rate),
                                start: pos,
                                part_from: 0,
                                part: 0,
                                split_bytes,
                                channels: channels as usize,
                                from_ring: 0,
                                written: 0,
                                dropped: 0,
                                gaps: VecDeque::new(),
                                target: None,
                                wav,
                                error,
                                flushed: Instant::now(),
                                peak: (0.0, 0.0, 0),
                            });
                        }
                        RecEvent::Gap { track, at, frames } => {
                            if let Some(w) = writers.get_mut(&track).and_then(|q| q.iter_mut().rev().find(|w| w.target.is_none())) {
                                w.gaps.push_back((at, frames));
                            }
                        }
                        RecEvent::Stop { track, frames } => {
                            if let Some(w) = writers.get_mut(&track).and_then(|q| q.iter_mut().find(|w| w.target.is_none())) {
                                w.target = Some(frames);
                            }
                        }
                    }
                }
                drain(&mut rings, &ready, &mut writers, &live2, &take_tx, closed);
                if closed {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
        .expect("spawn disk thread");
    Recorder { control: control_tx, takes: take_rx, live }
}

/// The journal of takes started in a folder since the project was last saved, so a take cut
/// off by a crash can be found again. One line per take: track, start and file, tab-separated.
const JOURNAL: &str = ".takes";

fn note_started(dir: &std::path::Path, track: Id, start: u64, file: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(JOURNAL)) {
        let _ = writeln!(f, "{track}\t{start}\t{file}");
        let _ = f.sync_data();
    }
}

/// Takes started in `dir` since the journal was last cleared, as (track, start, file).
pub fn journal(dir: &std::path::Path) -> Vec<(Id, u64, String)> {
    let Ok(text) = std::fs::read_to_string(dir.join(JOURNAL)) else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, '\t');
            Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?, f.next()?.to_string()))
        })
        .collect()
}

pub fn clear_journal(dir: &std::path::Path) {
    let _ = std::fs::remove_file(dir.join(JOURNAL));
}

/// Keeps only the journal's takes whose file `keep` accepts.
pub fn retain_journal(dir: &std::path::Path, keep: impl Fn(&str) -> bool) {
    let left: Vec<_> = journal(dir).into_iter().filter(|(_, _, file)| keep(file)).collect();
    if left.is_empty() {
        return clear_journal(dir);
    }
    let text: String = left.iter().map(|(track, start, file)| format!("{track}\t{start}\t{file}\n")).collect();
    let _ = std::fs::write(dir.join(JOURNAL), text);
}

/// Creates a new take file; never replaces one already there.
fn create(
    dir: &std::path::Path,
    track: Id,
    stamp: u64,
    counter: &mut u32,
    channels: u16,
    sample_rate: u32,
) -> std::io::Result<(String, hound::WavWriter<BufWriter<File>>)> {
    loop {
        let file = format!("Take {track}-{stamp}-{counter}.wav");
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&file)) {
            Ok(f) => {
                let wav = hound::WavWriter::new(BufWriter::new(f), wav_spec(channels, sample_rate)).map_err(std::io::Error::other)?;
                return Ok((file, wav));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => *counter += 1,
            Err(e) => return Err(e),
        }
    }
}

/// Writes what each ring holds into its track's takes, up to `ready` samples per ring. With
/// `closing`, takes still waiting for a Stop end where their data does.
fn drain(
    rings: &mut HashMap<Id, Consumer<f32>>,
    ready: &HashMap<Id, usize>,
    writers: &mut HashMap<Id, VecDeque<Writer>>,
    live: &Mutex<HashMap<Id, LiveTake>>,
    takes: &Sender<Take>,
    closing: bool,
) {
    for (id, queue) in writers.iter_mut() {
        let mut budget = ready.get(id).copied().unwrap_or(0);
        let mut ring = rings.get_mut(id);
        let mut new_peaks = Vec::new();
        while let Some(w) = queue.front_mut() {
            if closing && w.target.is_none() {
                w.target = Some(w.from_ring + (budget / w.channels) as u64);
            }
            loop {
                if let Some(&(at, frames)) = w.gaps.front()
                    && at <= w.from_ring
                {
                    w.gaps.pop_front();
                    w.dropped += frames;
                    for _ in 0..frames {
                        for _ in 0..w.channels {
                            w.put(0.0);
                        }
                        new_peaks.extend(w.frame_done());
                        if w.part_full() {
                            w.split(takes);
                        }
                    }
                    continue;
                }
                let mut frames = budget / w.channels;
                if let Some(t) = w.target {
                    frames = frames.min(t.saturating_sub(w.from_ring) as usize);
                }
                if let Some(&(at, _)) = w.gaps.front() {
                    frames = frames.min((at - w.from_ring) as usize);
                }
                if frames == 0 {
                    break;
                }
                let Some(Ok(chunk)) = ring.as_mut().map(|r| r.read_chunk(frames * w.channels)) else { break };
                for (i, s) in chunk.into_iter().enumerate() {
                    w.put(s);
                    if i % w.channels == w.channels - 1 {
                        new_peaks.extend(w.frame_done());
                        if w.part_full() {
                            w.split(takes);
                        }
                    }
                }
                budget -= frames * w.channels;
                w.from_ring += frames as u64;
            }
            if w.error.is_none() && w.flushed.elapsed() >= FLUSH_EVERY {
                if let Some(Err(e)) = w.wav.as_mut().map(|wav| wav.flush()) {
                    w.error = Some(e.to_string());
                }
                w.flushed = Instant::now();
            }
            if let Some(l) = live.lock().unwrap().get_mut(id) {
                l.frames = w.written;
                l.peaks.append(&mut new_peaks);
            }
            // Closing: what never arrived never will, so the take ends with what it has.
            if closing && !w.done() {
                w.target = Some(w.from_ring);
                w.gaps.clear();
            }
            if !w.done() {
                break;
            }
            let w = queue.pop_front().unwrap();
            live.lock().unwrap().remove(id);
            finish(*id, w, takes);
        }
    }
    writers.retain(|_, q| !q.is_empty());
}

fn finish(track: Id, w: Writer, takes: &Sender<Take>) {
    let Writer { file, start, written, part_from, dropped, wav, mut error, .. } = w;
    let frames = if wav.is_some() { written - part_from } else { 0 };
    if let Some(Err(e)) = wav.map(|w| w.finalize())
        && error.is_none()
    {
        error = Some(e.to_string());
    }
    if frames > 0 || error.is_some() {
        let _ = takes.send(Take { track, file, start: start + part_from, frames, dropped, error });
    }
}

pub const LIVE_PEAK_BLOCK: usize = PEAK_BLOCK;
