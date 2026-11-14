//! Offline renders for Export Mix, Export Stems, Bounce in Place and Track Freeze. Each job is
//! compiled here and rendered on a thread of its own, faster than real time, while the live
//! engine is suspended: both would otherwise run the same plug-in instances at once.

use crate::studio::{Change, Ctx, MAX_BLOCK, Studio};
use adw::prelude::*;
use gtk::glib;
use rtrb::Producer;
use solstice_dsp::loudness::Meter;
use solstice_engine::audio::AudioData;
use solstice_engine::export::{self, Dither, Format};
use solstice_engine::offline::{self, Outcome, Progress, Rendered};
use solstice_engine::{Command, Options, PluginCell, Resources, Shared, Strip};
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering::{Acquire, Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long the engine gets to acknowledge a suspend. PipeWire may stop scheduling a client
/// that has no links, so past this the connection is dropped instead and made again after.
const PAUSE_TIMEOUT: Duration = Duration::from_secs(1);

/// One render: which track or bus to play on its own (all of them for `None`), the span, and
/// where the result goes.
#[derive(Clone, Debug)]
pub struct BounceJob {
    pub track: Option<Id>,
    pub start: u64,
    pub end: u64,
    pub target: BounceTarget,
}

#[derive(Clone, Debug)]
pub enum BounceTarget {
    File(PathBuf, FileSpec),
    /// A new track under `track` with the result, the bounced regions muted.
    InPlace { regions: Vec<Id> },
    Freeze,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FileSpec {
    pub format: Format,
    pub dither: Dither,
    /// Target integrated loudness (LUFS) and true-peak ceiling (dBTP).
    pub normalise: Option<(f32, f32)>,
    /// Measure the result and say how loud it came out.
    pub report: bool,
}

/// What the Export dialog asks for, kept in the settings between exports.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ExportChoices {
    pub format: Format,
    pub dither: Dither,
    pub normalise: bool,
    /// Export the cycle range rather than the whole project.
    pub cycle: bool,
    /// Seconds after the end for reverbs and delays to ring out.
    pub tail: f64,
}

impl Default for ExportChoices {
    fn default() -> Self {
        ExportChoices { format: Format::Wav24, dither: Dither::Tpdf, normalise: false, cycle: false, tail: 0.0 }
    }
}

/// A track or bus offered for stem export.
#[derive(Clone, Debug, PartialEq)]
pub struct Stem {
    pub id: Id,
    pub name: String,
    pub bus: bool,
    pub checked: bool,
}

/// Every track and audio bus but reference tracks and cue mixes; tracks with regions and every bus start ticked.
pub fn stems(p: &Project) -> Vec<Stem> {
    let tracks = p.tracks.iter().filter(|t| !t.reference).map(|t| Stem {
        id: t.id,
        name: t.name.clone(),
        bus: false,
        checked: !t.regions.is_empty(),
    });
    let buses = p.buses.iter().filter(|b| !b.vca && !b.cue).map(|b| Stem { id: b.id, name: b.name.clone(), bus: true, checked: true });
    tracks.chain(buses).collect()
}

/// The span to export: the cycle range when asked for and on, or the whole mix, plus the tail.
pub fn export_span(p: &Project, cycle: bool, tail: f64, sr: u32) -> Option<(u64, u64)> {
    let (start, end) = if cycle && has_cycle(p) { (p.cycle.start, p.cycle.end) } else { (0, p.mix_length()) };
    (end > start).then(|| (start, end + (tail.max(0.0) * sr as f64) as u64))
}

pub fn has_cycle(p: &Project) -> bool {
    p.cycle_on && !p.cycle.is_empty()
}

/// The gain in dB that brings `integrated` to `target` without the true peak passing `ceiling`,
/// and whether the ceiling is what set it.
pub fn normalise_gain(integrated: f64, true_peak: f64, target: f64, ceiling: f64) -> (f64, bool) {
    if !integrated.is_finite() {
        return (0.0, false);
    }
    let gain = target - integrated;
    if true_peak.is_finite() && true_peak + gain > ceiling { (ceiling - true_peak, true) } else { (gain, false) }
}

fn signed(v: f64, digits: usize) -> String {
    format!("{v:.digits$}").replace('-', "−")
}

/// Renders a mix or stem to its file. With `normalise`, it goes to a float file first, is
/// measured, and is then written in its format at the gain that reaches the target.
fn export_file(
    g: solstice_engine::Graph,
    sr: u32,
    span: std::ops::Range<u64>,
    workers: usize,
    progress: &Progress,
    path: &Path,
    spec: FileSpec,
) -> Done {
    let mut meter = (spec.report || spec.normalise.is_some()).then(|| Meter::new(sr as f64));
    let mut tap = |l: &[f32], r: &[f32]| {
        if let Some(m) = meter.as_mut() {
            m.push(l, r);
        }
    };
    let Some((target, ceiling)) = spec.normalise else {
        let rendered = offline::render_file(g, sr, span, workers, progress, path, spec.format, spec.dither, &mut tap);
        let note = match (&rendered.0, meter) {
            (Ok(Outcome::Finished { .. }), Some(m)) if spec.report => Some(loudness_note(m.integrated(), m.true_peak(), None)),
            _ => None,
        };
        return (rendered, note);
    };
    let mut temp = path.as_os_str().to_owned();
    temp.push(".float");
    let temp = PathBuf::from(temp);
    let (result, graph) = offline::render_file(g, sr, span, workers, progress, &temp, Format::WavFloat, Dither::None, &mut tap);
    let Ok(Outcome::Finished { frames }) = result else { return ((result, graph), None) };
    let m = meter.expect("measured");
    let (gain, limited) = normalise_gain(m.integrated(), m.true_peak(), target as f64, ceiling as f64);
    let converted = export::convert(&temp, path, spec.format, spec.dither, solstice_model::db_to_gain(gain as f32), &progress.cancel);
    let _ = std::fs::remove_file(&temp);
    let result = match converted {
        Ok(true) => Ok(Outcome::Finished { frames }),
        Ok(false) => Ok(Outcome::Cancelled),
        Err(e) => Err(e),
    };
    let finished = matches!(result, Ok(Outcome::Finished { .. }));
    let note = finished.then(|| loudness_note(m.integrated() + gain, m.true_peak() + gain, limited.then_some(ceiling)));
    ((result, graph), note)
}

/// "Exported at −14.0 LUFS, −1.2 dBTP", saying so when the peak ceiling held the gain back.
fn loudness_note(integrated: f64, true_peak: f64, ceiling: Option<f32>) -> String {
    if !integrated.is_finite() {
        return "Exported; the mix is too quiet to measure".into();
    }
    match ceiling {
        Some(c) => {
            let ceiling = signed(c as f64, 1);
            format!("Exported at {} LUFS; the {} dBTP ceiling limited the gain", signed(integrated, 1), ceiling.trim_end_matches(".0"))
        }
        None => format!("Exported at {} LUFS, {} dBTP", signed(integrated, 1), signed(true_peak, 1)),
    }
}

/// The render's outcome and, for a mix, how loud it came out.
type Done = (Rendered, Option<String>);

pub struct Running {
    pub job: BounceJob,
    pub progress: Arc<Progress>,
    thread: JoinHandle<Done>,
    /// Holds the graph's plug-ins as well, so that even a render thread that panics never
    /// drops the last reference to one.
    plugins: Vec<Arc<PluginCell>>,
    path: PathBuf,
    /// The track's freeze key when the render was compiled.
    key: u64,
}

pub enum RenderState {
    Idle,
    Pausing { token: u32, shared: Arc<Shared>, since: Instant },
    Running(Running),
    /// A job has finished and its result is going into the project; the engine is still held.
    Finishing,
}

impl RenderState {
    pub fn is_busy(&self) -> bool {
        matches!(self, RenderState::Pausing { .. } | RenderState::Running(_))
    }
}

/// The live plug-ins and audio, with strips of its own: an export takes each channel's fader
/// as it is now, without Dim, and a freeze takes none.
struct OfflineRes<'a> {
    studio: &'a Studio,
    freeze: bool,
    strips: RefCell<HashMap<Option<Id>, Arc<Strip>>>,
    plugins: RefCell<Vec<Arc<PluginCell>>>,
}

impl Resources for OfflineRes<'_> {
    fn audio(&self, file: &str) -> Option<Arc<AudioData>> {
        Resources::audio(self.studio, file)
    }

    fn plugin(&self, slot: Id) -> Option<Arc<PluginCell>> {
        let cell = Resources::plugin(self.studio, slot)?;
        self.plugins.borrow_mut().push(cell.clone());
        Some(cell)
    }

    fn strip(&self, id: Option<Id>) -> Arc<Strip> {
        let mut strips = self.strips.borrow_mut();
        let strip = strips.entry(id).or_insert_with(|| {
            let s = Strip::default();
            if !self.freeze
                && let Some(ch) = self.studio.project.channel(id)
            {
                s.set(ch.volume_db, ch.pan, ch.mute);
            }
            Arc::new(s)
        });
        strip.clone()
    }

    fn record_ring(&self, _: Id) -> Option<Producer<f32>> {
        None
    }

    fn class_is_stereo(&self, uid: &str) -> bool {
        Resources::class_is_stereo(self.studio, uid)
    }
}

pub fn freezable(t: &Track) -> bool {
    t.instrument.is_some() || !t.channel.inserts.is_empty()
}

/// Unfreezes the tracks if they are all frozen, and otherwise freezes the rest of them.
pub fn toggle_freeze(ctx: &Ctx, ids: &[Id], parent: &impl IsA<gtk::Widget>) {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let all_frozen = ids.iter().all(|id| ctx.s().project.track(*id).is_some_and(|t| t.freeze.is_some()));
    if all_frozen {
        ctx.unfreeze(&ids);
    } else if ctx.freeze(&ids) {
        progress_dialog(ctx, parent);
    }
}

fn file_name(name: &str) -> String {
    name.replace(['/', '\\'], "-")
}

/// One job per chosen stem, named "<Project> - <Name>", with a number after any name taken twice.
pub fn stem_jobs(p: &Project, ids: &[Id], dir: &Path, (start, end): (u64, u64), choices: &ExportChoices) -> Vec<BounceJob> {
    let spec = FileSpec { format: choices.format, dither: choices.dither, normalise: None, report: false };
    let mut taken: Vec<String> = Vec::new();
    stems(p)
        .into_iter()
        .filter(|s| ids.contains(&s.id))
        .map(|s| {
            let base = file_name(&format!("{} - {}", p.name, s.name));
            let mut name = base.clone();
            let mut n = 1;
            while taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
                n += 1;
                name = format!("{base} {n}");
            }
            taken.push(name.clone());
            let path = dir.join(format!("{name}.{}", choices.format.extension()));
            BounceJob { track: Some(s.id), start, end, target: BounceTarget::File(path, spec) }
        })
        .collect()
}

enum Step {
    Wait,
    Begin,
    Restart,
    Finished,
    /// A plug-in the render uses crashed; the render is cancelled.
    Crashed,
}

impl Ctx {
    /// Export Mix: the whole mix to one file. Returns false if there is nothing to export.
    pub fn export_mix(&self, path: PathBuf, choices: &ExportChoices, normalise: Option<(f32, f32)>) -> bool {
        let job = {
            let s = self.s();
            let Some((start, end)) = export_span(&s.project, choices.cycle, choices.tail, s.sample_rate()) else { return false };
            let spec = FileSpec { format: choices.format, dither: choices.dither, normalise, report: true };
            BounceJob { track: None, start, end, target: BounceTarget::File(path, spec) }
        };
        self.queue_render(vec![job])
    }

    /// Export Stems: each chosen track or bus on its own, at the levels it has in the mix.
    pub fn export_stems(&self, dir: &Path, ids: &[Id], choices: &ExportChoices) -> usize {
        let jobs = {
            let s = self.s();
            match export_span(&s.project, choices.cycle, choices.tail, s.sample_rate()) {
                Some(span) => stem_jobs(&s.project, ids, dir, span, choices),
                None => Vec::new(),
            }
        };
        let n = jobs.len();
        if self.queue_render(jobs) { n } else { 0 }
    }

    /// Renders the selected regions of one track, through its plug-ins, to a new track below
    /// it and mutes the originals.
    pub fn bounce_in_place(&self) -> bool {
        let ids = self.selected_region_ids();
        let job = {
            let s = self.s();
            let Some((ti, _)) = ids.first().and_then(|id| s.project.find_region(*id)) else { return false };
            let t = &s.project.tracks[ti];
            let chosen: Vec<&Region> = t.regions.iter().filter(|r| ids.contains(&r.id)).collect();
            let start = chosen.iter().map(|r| r.start).min().unwrap_or(0);
            // A second of tail for reverbs and delays to ring out.
            let end = chosen.iter().map(|r| r.span_end()).max().unwrap_or(0) + s.sample_rate() as u64;
            BounceJob {
                track: Some(t.id),
                start,
                end,
                target: BounceTarget::InPlace { regions: chosen.iter().map(|r| r.id).collect() },
            }
        };
        self.queue_render(vec![job])
    }

    /// Track Freeze. Returns whether anything was queued.
    pub fn freeze(&self, ids: &[Id]) -> bool {
        let mut refused = Vec::new();
        let jobs: Vec<BounceJob> = {
            let s = self.s();
            let p = &s.project;
            // Two seconds of tail for reverbs and delays to ring out.
            let tail = 2 * s.sample_rate() as u64;
            let end = |t: &Track| if t.reference { p.length() } else { p.mix_length() } + tail;
            ids.iter()
                .filter_map(|id| p.track(*id))
                .filter(|t| freezable(t) && live_freeze(p, t).is_none())
                .filter(|t| {
                    let keyed = t.channel.inserts.iter().any(|s| s.sidechain.is_some_and(|k| k != t.id));
                    if keyed {
                        refused.push(t.name.clone());
                    }
                    !keyed
                })
                .map(|t| BounceJob { track: Some(t.id), start: 0, end: end(t), target: BounceTarget::Freeze })
                .collect()
        };
        for name in refused {
            self.message(&format!("“{name}” cannot be frozen while another track keys its side-chain"));
        }
        let any = !jobs.is_empty();
        any && self.queue_render(jobs)
    }

    pub fn unfreeze(&self, ids: &[Id]) {
        if !ids.iter().any(|id| self.s().project.track(*id).is_some_and(|t| t.freeze.is_some())) {
            return;
        }
        self.edit("Unfreeze Track", |p| {
            for id in ids {
                if let Some(t) = p.track_mut(*id) {
                    t.freeze = None;
                }
            }
        });
    }

    /// Stops the current render and drops those waiting.
    pub fn cancel_render(&self) {
        let mut s = self.m();
        s.bounce_queue.clear();
        if let RenderState::Running(r) = &s.render {
            r.progress.cancel.store(true, Relaxed);
        }
    }

    /// Returns whether the jobs were queued: none are while a plug-in has crashed, since a
    /// render would leave it out of the mix.
    fn queue_render(&self, jobs: Vec<BounceJob>) -> bool {
        if jobs.is_empty() {
            return false;
        }
        let crashed: Vec<String> = {
            let s = self.s();
            s.crashed_slots().into_iter().filter_map(|sid| crate::plugins::slot_name(&s.project, sid)).collect()
        };
        if !crashed.is_empty() {
            self.message(&format!("{} crashed. Reload them before exporting.", crashed.join(", ")));
            return false;
        }
        let begin = {
            let mut s = self.m();
            let s = &mut *s;
            let idle = matches!(s.render, RenderState::Idle);
            if idle {
                s.batch = (0, 0);
                s.render_rate = s.sample_rate();
                s.render_errors.clear();
                s.render_notes.clear();
            }
            s.batch.1 += jobs.len();
            s.bounce_queue.extend(jobs);
            if idle && let Some(io) = s.io.as_mut() {
                s.render_token = s.render_token.wrapping_add(1).max(1);
                io.handle.send(Command::Suspend(s.render_token));
                s.render = RenderState::Pausing { token: s.render_token, shared: io.handle.shared.clone(), since: Instant::now() };
            }
            idle && s.io.is_none()
        };
        if begin {
            self.next_render();
        } else {
            self.emit(Change::Render);
        }
        true
    }

    /// Moves renders along; called every frame and by the progress dialog.
    pub fn poll_render(&self) {
        let step = {
            let s = self.s();
            match &s.render {
                RenderState::Pausing { token, shared, since } => {
                    // An engine started since has been given no graph, so it holds no plug-ins.
                    let swapped = s.io.as_ref().is_none_or(|io| !Arc::ptr_eq(&io.handle.shared, shared));
                    if swapped || shared.suspended.load(Acquire) == *token {
                        Step::Begin
                    } else if since.elapsed() > PAUSE_TIMEOUT {
                        Step::Restart
                    } else {
                        Step::Wait
                    }
                }
                RenderState::Running(r) if s.render_crash.is_none() && r.plugins.iter().any(|c| c.crashed.load(Relaxed)) => {
                    Step::Crashed
                }
                RenderState::Running(r) if r.thread.is_finished() => Step::Finished,
                _ => Step::Wait,
            }
        };
        match step {
            Step::Wait => {}
            Step::Crashed => {
                let mut s = self.m();
                let s = &mut *s;
                let RenderState::Running(r) = &s.render else { return };
                let name = r
                    .plugins
                    .iter()
                    .find(|c| c.crashed.load(Relaxed))
                    .and_then(|c| s.plugins.iter().find(|(_, l)| Arc::ptr_eq(&l.cell, c)))
                    .and_then(|(sid, _)| crate::plugins::slot_name(&s.project, *sid))
                    .unwrap_or_else(|| "A plug-in".into());
                r.progress.cancel.store(true, Relaxed);
                s.render_crash = Some(name);
            }
            Step::Begin => self.next_render(),
            Step::Restart => {
                {
                    let mut s = self.m();
                    s.stop_audio();
                    s.restart_audio = true;
                }
                self.emit(Change::Engine);
                self.next_render();
            }
            Step::Finished => {
                let RenderState::Running(r) = std::mem::replace(&mut self.m().render, RenderState::Finishing) else { return };
                self.finish_job(r);
                self.next_render();
            }
        }
    }

    fn next_render(&self) {
        loop {
            let job = self.m().bounce_queue.pop_front();
            let Some(job) = job else {
                self.end_renders();
                return;
            };
            let started = self.m().start_job(job);
            match started {
                Ok(()) => break,
                Err(e) => self.m().render_errors.push(format!("Could not start the render: {e}")),
            }
        }
        self.emit(Change::Render);
    }

    fn finish_job(&self, run: Running) {
        let Running { job, thread, plugins, path, key, .. } = run;
        let ((result, graph), note) = thread.join().unwrap_or_else(|_| ((Err(std::io::Error::other("the render failed")), None), None));
        drop(graph);
        drop(plugins);
        let crash = self.m().render_crash.take();
        if let Some(name) = crash {
            if matches!(result, Ok(Outcome::Finished { .. })) {
                let _ = std::fs::remove_file(&path);
            }
            self.m().render_errors.push(format!("{name} crashed during the render, so nothing was written."));
            return;
        }
        if let Some(note) = note {
            self.m().render_notes.push(note);
        }
        let frames = match result {
            Ok(Outcome::Finished { frames }) => frames,
            Ok(Outcome::Cancelled) => return,
            Err(e) => {
                self.m().render_errors.push(format!("Could not write {}: {e}", path.display()));
                return;
            }
        };
        let Some(file) = path.file_name().map(|f| f.to_string_lossy().into_owned()) else { return };
        match job.target {
            BounceTarget::File(..) => {}
            BounceTarget::InPlace { regions } => {
                let Some(track) = job.track else { return };
                self.edit("Bounce in Place", |p| {
                    let Some(i) = p.tracks.iter().position(|t| t.id == track) else { return };
                    let name = format!("{} Bounce", p.tracks[i].name);
                    p.add_track(TrackKind::Audio);
                    let mut t = p.tracks.pop().unwrap();
                    t.name = name.clone();
                    t.stereo = true;
                    let id = p.alloc_id();
                    t.regions.push(Region {
                        id,
                        name,
                        start: job.start,
                        length: frames,
                        muted: false,
                        loops: 0,
                        color: None,
                        content: Content::Audio(AudioContent::new(file.clone())),
                    });
                    p.tracks.insert(i + 1, t);
                    for r in p.tracks[i].regions.iter_mut().filter(|r| regions.contains(&r.id)) {
                        r.muted = true;
                    }
                });
            }
            BounceTarget::Freeze => {
                let Some(track) = job.track else { return };
                self.m().audio(&file);
                // A track edited during the render keeps playing live: the key no longer matches.
                self.edit("Freeze Track", |p| {
                    if let Some(t) = p.track_mut(track) {
                        t.freeze = Some(Freeze { file: file.clone(), start: job.start, frames, key });
                    }
                });
            }
        }
    }

    fn end_renders(&self) {
        let (restarted, errors, notes) = {
            let mut s = self.m();
            let s = &mut *s;
            s.render = RenderState::Idle;
            if let Some(io) = s.io.as_mut() {
                io.handle.send(Command::Suspend(0));
            }
            let restart = std::mem::take(&mut s.restart_audio) && s.io.is_none();
            if restart {
                s.start_audio();
            } else {
                s.rebuild();
            }
            (restart, std::mem::take(&mut s.render_errors), std::mem::take(&mut s.render_notes))
        };
        if restarted {
            self.emit(Change::Engine);
        }
        self.emit(Change::Render);
        let all: Vec<String> = errors.into_iter().chain(notes).collect();
        if !all.is_empty() {
            self.message(&all.join("\n"));
        }
    }
}

impl Studio {
    fn start_job(&mut self, job: BounceJob) -> std::io::Result<()> {
        self.batch.0 += 1;
        let name = job.track.and_then(|id| self.project.track(id)).map(|t| file_name(&t.name)).unwrap_or_default();
        let path = match &job.target {
            BounceTarget::File(p, _) => p.clone(),
            // Fresh names, so the audio cache and undo never meet a file that was overwritten.
            BounceTarget::InPlace { .. } => self.bundle.audio_path(&format!("{name} Bounce {}.wav", self.project.alloc_id())),
            BounceTarget::Freeze => self.bundle.audio_path(&format!("Freeze {name} {}.wav", self.project.alloc_id())),
        };
        if !matches!(job.target, BounceTarget::File(..)) {
            std::fs::create_dir_all(self.bundle.audio_dir())?;
        }
        let freeze = matches!(job.target, BounceTarget::Freeze);
        let key = job.track.and_then(|id| self.project.track(id)).map_or(0, |t| freeze_key(&self.project, t));
        let opts = Options {
            max_block: MAX_BLOCK,
            inputs: 0,
            auto_input: false,
            low_latency: None,
            midi_track: None,
            midi_thru: false,
            click_gain: 0.0,
            click_accent: false,
            click_groups_only: false,
            outputs: 0,
            isolate: job.track,
            freeze: job.track.filter(|_| freeze),
        };
        let res = OfflineRes { studio: self, freeze, strips: RefCell::default(), plugins: RefCell::default() };
        let graph = solstice_engine::compile(&self.project, &res, &opts);
        let plugins = res.plugins.into_inner();
        let progress = Arc::new(Progress::default());
        let (sr, workers, span) = (self.render_rate, self.settings.workers(), job.start..job.end);
        let (p, out) = (progress.clone(), path.clone());
        let spec = match &job.target {
            BounceTarget::File(_, spec) => Some(*spec),
            _ => None,
        };
        let thread = std::thread::Builder::new().name("solstice-render".into()).spawn(move || match spec {
            Some(spec) => export_file(graph, sr, span, workers, &p, &out, spec),
            None => (offline::render_wav(graph, sr, span, workers, &p, &out), None),
        })?;
        self.render = RenderState::Running(Running { job, progress, thread, plugins, path, key });
        Ok(())
    }

    /// Stops any render at once and gives the engine back, as when the window closes.
    pub fn abort_render(&mut self) {
        self.bounce_queue.clear();
        match std::mem::replace(&mut self.render, RenderState::Idle) {
            RenderState::Idle => return,
            RenderState::Running(r) => {
                r.progress.cancel.store(true, Relaxed);
                let _ = r.thread.join();
            }
            _ => {}
        }
        if let Some(io) = self.io.as_mut() {
            io.handle.send(Command::Suspend(0));
        }
    }
}

fn heading(s: &Studio) -> String {
    let RenderState::Running(r) = &s.render else { return "Preparing to Render".into() };
    match &r.job.target {
        BounceTarget::Freeze => {
            let name = r.job.track.and_then(|id| s.project.track(id)).map(|t| t.name.clone()).unwrap_or_default();
            format!("Freezing “{name}”")
        }
        BounceTarget::File(..) => "Exporting".into(),
        BounceTarget::InPlace { .. } => "Bouncing".into(),
    }
}

/// Shows the renders' progress over `parent`, with Cancel, until they are all done.
pub fn progress_dialog(ctx: &Ctx, parent: &impl IsA<gtk::Widget>) {
    if matches!(ctx.s().render, RenderState::Idle) {
        return;
    }
    thread_local! {
        static SHOWN: RefCell<Vec<usize>> = RefCell::default();
    }
    if SHOWN.with(|s| s.borrow().contains(&ctx.key())) {
        return;
    }
    SHOWN.with(|s| s.borrow_mut().push(ctx.key()));
    let bar = gtk::ProgressBar::new();
    bar.set_margin_top(6);
    let dialog = adw::AlertDialog::builder().heading(heading(&ctx.s())).extra_child(&bar).close_response("cancel").build();
    dialog.add_response("cancel", "_Cancel");
    {
        let ctx = ctx.clone();
        dialog.connect_response(None, move |_, _| ctx.cancel_render());
    }
    let closed = Rc::new(Cell::new(false));
    {
        let (closed, key) = (closed.clone(), ctx.key());
        dialog.connect_closed(move |_| {
            closed.set(true);
            SHOWN.with(|s| s.borrow_mut().retain(|k| *k != key));
        });
    }
    let (ctx, d) = (ctx.clone(), dialog.clone());
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if closed.get() {
            return glib::ControlFlow::Break;
        }
        ctx.poll_render();
        let s = ctx.s();
        match &s.render {
            RenderState::Idle => {
                drop(s);
                d.force_close();
                return glib::ControlFlow::Break;
            }
            RenderState::Running(r) => {
                let total = r.progress.total.load(Relaxed).max(1);
                bar.set_fraction(r.progress.done.load(Relaxed) as f64 / total as f64);
            }
            _ => bar.pulse(),
        }
        d.set_heading(Some(&heading(&s)));
        let (n, of) = s.batch;
        d.set_body(&if of > 1 { format!("File {n} of {of}") } else { String::new() });
        glib::ControlFlow::Continue
    });
    crate::dialogs::clear_way(&dialog, parent);
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    const SR: u32 = 48000;

    /// A project in a fresh bundle with one audio track playing a constant through a bypassed
    /// built-in compressor, its fader at -6 dB.
    fn studio(name: &str) -> (Ctx, Id) {
        studio_playing(name, vec![0.5; 4800])
    }

    fn studio_playing(name: &str, samples: Vec<f32>) -> (Ctx, Id) {
        let dir = std::env::temp_dir().join(format!("solstice-render-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::new(&dir);
        std::fs::create_dir_all(bundle.audio_dir()).unwrap();
        let length = samples.len() as u64;
        solstice_engine::audio::write_wav(&bundle.audio_path("in.wav"), SR, &[samples]).unwrap();
        let mut p = Project::new(name, SR);
        let t = p.add_track(TrackKind::Audio);
        let (rid, sid) = (p.alloc_id(), p.alloc_id());
        let tr = p.track_mut(t).unwrap();
        let content = Content::Audio(AudioContent::new("in.wav"));
        tr.regions.push(Region { id: rid, name: "r".into(), start: 0, length, muted: false, loops: 0, color: None, content });
        tr.channel.volume_db = -6.0206;
        tr.channel.inserts.push(PluginSlot {
            id: sid,
            uid: "solstice:compressor".into(),
            path: "".into(),
            name: "Compressor".into(),
            bypass: true,
            state: vec![],
            sidechain: None,
            section: None,
        });
        let ctx = Ctx::new(Studio::new(p, bundle, Settings::default()));
        ctx.m().sync();
        (ctx, t)
    }

    fn wait(ctx: &Ctx) {
        let until = Instant::now() + Duration::from_secs(20);
        while !matches!(ctx.s().render, RenderState::Idle) {
            assert!(Instant::now() < until, "the render did not finish");
            ctx.poll_render();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Puts a crashed bridged plug-in in the track's insert slot.
    fn crash_insert(ctx: &Ctx, t: Id) {
        let sid = ctx.s().project.track(t).unwrap().channel.inserts[0].id;
        let inst = solstice_vst3::Instance::dead_for_test(b"kept".to_vec());
        let cell = PluginCell::new(inst.processor(), 0);
        ctx.m().plugins.insert(sid, crate::studio::Loaded { inst: crate::host::Host::Vst3(inst), cell, applied: Vec::new() });
        ctx.m().check_crashes();
    }

    #[test]
    fn renders_refuse_with_a_crashed_slot() {
        let (ctx, t) = studio("refuse");
        crash_insert(&ctx, t);
        assert!(!ctx.freeze(&[t]));
        assert!(matches!(ctx.s().render, RenderState::Idle));
        let out = ctx.s().bundle.dir.join("mix.wav");
        assert!(!ctx.export_mix(out.clone(), &Default::default(), None));
        assert!(!out.exists());
        let msg = ctx.m().message.take().unwrap_or_default();
        assert!(msg.contains("Compressor crashed. Reload them before exporting."), "{msg}");
    }

    #[test]
    fn a_crash_during_a_render_discards_it() {
        let (ctx, t) = studio("midcrash");
        let sid = ctx.s().project.track(t).unwrap().channel.inserts[0].id;
        let cell = ctx.s().plugins[&sid].cell.clone();
        assert!(ctx.freeze(&[t]));
        // The render may already have finished: the crash still discards it.
        while !ctx.s().render.is_busy() {}
        cell.crashed.store(true, Relaxed);
        std::thread::sleep(Duration::from_millis(50));
        wait(&ctx);
        assert!(std::fs::read_dir(ctx.s().bundle.audio_dir()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with("Freeze")));
        assert!(ctx.s().project.track(t).unwrap().freeze.is_none());
        let msg = ctx.m().message.take().unwrap_or_default();
        assert!(msg.contains("Compressor crashed during the render, so nothing was written."), "{msg}");
    }

    #[test]
    fn freeze_records_the_file_and_goes_stale_on_edit() {
        let (ctx, t) = studio("freeze");
        assert!(ctx.freeze(&[t]));
        assert!(ctx.s().render.is_busy());
        ctx.edit("Rename Track", |p| p.track_mut(t).unwrap().name = "Bass".into());
        assert_ne!(ctx.s().project.track(t).unwrap().name, "Bass", "edits wait for the render");
        wait(&ctx);

        let f = ctx.s().project.track(t).unwrap().freeze.clone().expect("frozen");
        assert_eq!((f.start, f.frames), (0, 4800 + 2 * SR as u64));
        let data = AudioData::open(&ctx.s().bundle.audio_path(&f.file)).unwrap();
        assert_eq!((data.channels, data.frames), (2, f.frames));
        assert!((data.sample(0, 100) - 0.5).abs() < 1e-6, "a freeze is taken before the fader");
        assert_eq!(data.sample(1, 4800), 0.0);

        ctx.edit("Volume", |p| p.track_mut(t).unwrap().channel.volume_db = 0.0);
        assert!(ctx.s().project.track(t).unwrap().freeze.is_some(), "the fader stays live");
        ctx.edit("Move Region", |p| p.tracks[0].regions[0].start = 100);
        assert!(ctx.s().project.track(t).unwrap().freeze.is_none(), "a change to what was rendered unfreezes");
        ctx.undo();
        assert!(ctx.s().project.track(t).unwrap().freeze.is_some());
    }

    #[test]
    fn gestures_wait_for_renders_and_unfreeze() {
        let (ctx, t) = studio("gesture");
        ctx.freeze(&[t]);
        ctx.begin_gesture();
        ctx.live(true, |p| p.tracks[0].regions[0].start = 100);
        ctx.end_gesture("Move Region");
        assert_eq!(ctx.s().project.tracks[0].regions[0].start, 0, "gestures wait for the render");
        wait(&ctx);
        assert!(ctx.s().project.track(t).unwrap().freeze.is_some());

        ctx.begin_gesture();
        ctx.live(true, |p| p.tracks[0].regions[0].start = 100);
        ctx.end_gesture("Move Region");
        assert!(ctx.s().project.track(t).unwrap().freeze.is_none());
        ctx.undo();
        assert_eq!(ctx.s().project.tracks[0].regions[0].start, 0);
        assert!(ctx.s().project.track(t).unwrap().freeze.is_some(), "the unfreeze is part of the gesture's undo step");
    }

    #[test]
    fn frozen_tracks_prefetch_their_freeze_file() {
        let (ctx, t) = studio("prefetch");
        assert_eq!(ctx.s().prefetch_spans(0), vec![("in.wav".to_string(), 0, 5 * SR as u64)]);
        ctx.freeze(&[t]);
        wait(&ctx);
        let file = ctx.s().project.track(t).unwrap().freeze.clone().unwrap().file;
        assert_eq!(ctx.s().prefetch_spans(1000), vec![(file, 1000, 5 * SR as u64)]);
    }

    #[test]
    fn renders_keep_the_rate_they_began_at() {
        let (ctx, _) = studio("rate");
        let out = ctx.s().bundle.dir.join("bounce.wav");
        {
            // As after the audio server connection was dropped for a render at 44.1 kHz.
            let mut s = ctx.m();
            s.render_rate = 44100;
            s.batch = (0, 1);
            let spec = FileSpec { format: Format::WavFloat, ..Default::default() };
            s.bounce_queue.push_back(BounceJob { track: None, start: 0, end: 4800, target: BounceTarget::File(out.clone(), spec) });
            s.render = RenderState::Pausing { token: 1, shared: Arc::new(Shared::default()), since: Instant::now() };
        }
        wait(&ctx);
        assert_eq!(AudioData::open(&out).unwrap().sample_rate, 44100);
    }

    #[test]
    fn save_as_waits_for_renders_and_skips_partial_files() {
        let (ctx, t) = studio("save-as");
        let copy = ctx.s().bundle.dir.with_extension("copy");
        let _ = std::fs::remove_dir_all(&copy);
        ctx.m().project.tracks[0].regions[0].loops = 10_000;
        ctx.freeze(&[t]);
        assert!(ctx.m().save_as(&copy).is_err());
        assert!(!copy.exists());
        ctx.cancel_render();
        wait(&ctx);
        std::fs::write(ctx.s().bundle.audio_path("left.wav.part"), b"").unwrap();
        ctx.m().save_as(&copy).unwrap();
        let files: Vec<_> = std::fs::read_dir(Bundle::new(&copy).audio_dir()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files, vec![std::ffi::OsString::from("in.wav")]);
        let _ = std::fs::remove_dir_all(&copy);
    }

    #[test]
    fn save_as_into_its_own_folder_keeps_the_audio() {
        let (ctx, _) = studio("save-as-same");
        let dir = ctx.s().bundle.dir.clone();
        let file = ctx.s().bundle.audio_path("in.wav");
        let before = std::fs::metadata(&file).unwrap().len();
        assert!(before > 0);
        // A new project's first save offers the folder it already lives in.
        ctx.m().save_as(&dir).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().len(), before);
        // The same folder written another way still counts as the same folder.
        ctx.m().save_as(&dir.join("..").join(dir.file_name().unwrap())).unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().len(), before);
    }

    #[test]
    fn refreezing_writes_a_new_file() {
        let (ctx, t) = studio("refreeze");
        ctx.freeze(&[t]);
        wait(&ctx);
        let first = ctx.s().project.track(t).unwrap().freeze.clone().unwrap().file;
        ctx.unfreeze(&[t]);
        ctx.freeze(&[t]);
        wait(&ctx);
        let second = ctx.s().project.track(t).unwrap().freeze.clone().unwrap().file;
        assert_ne!(first, second);
        assert!(ctx.s().bundle.audio_path(&first).exists());
    }

    #[test]
    fn export_takes_the_fader() {
        let (ctx, t) = studio("export");
        let dir = ctx.s().bundle.dir.join("export");
        std::fs::create_dir_all(&dir).unwrap();
        let choices = ExportChoices { format: Format::WavFloat, ..Default::default() };
        assert_eq!(ctx.export_stems(&dir, &[t], &choices), 1);
        wait(&ctx);
        let data = AudioData::open(&dir.join("export - Audio 1.wav")).unwrap();
        assert_eq!(data.frames, 4800);
        assert!((data.sample(0, 100) - 0.25).abs() < 1e-4, "{}", data.sample(0, 100));
    }

    #[test]
    fn bounce_in_place_adds_a_track() {
        let (ctx, t) = studio("in-place");
        let rid = ctx.s().project.tracks[0].regions[0].id;
        ctx.select_regions(vec![rid]);
        assert!(ctx.bounce_in_place());
        wait(&ctx);
        let p = ctx.s().project.clone();
        assert_eq!(p.tracks.len(), 2);
        assert!(p.track(t).unwrap().regions[0].muted);
        let r = &p.tracks[1].regions[0];
        assert_eq!(r.length, 4800 + SR as u64);
        let Content::Audio(a) = &r.content else { panic!("not audio") };
        assert!(ctx.s().bundle.audio_path(&a.file).exists());
    }

    #[test]
    fn cancel_leaves_nothing() {
        let (ctx, t) = studio("cancel");
        // Long enough that the render is still going when it is cancelled.
        ctx.m().project.tracks[0].regions[0].loops = 10_000;
        ctx.freeze(&[t]);
        ctx.cancel_render();
        wait(&ctx);
        assert!(ctx.s().project.track(t).unwrap().freeze.is_none());
        let files: Vec<_> = std::fs::read_dir(ctx.s().bundle.audio_dir()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files, vec![std::ffi::OsString::from("in.wav")]);
    }

    #[test]
    fn normalising_stops_at_the_peak_ceiling() {
        assert_eq!(normalise_gain(-20.0, -8.0, -14.0, -1.0), (6.0, false));
        let (gain, limited) = normalise_gain(-20.0, -3.0, -14.0, -1.0);
        assert!((gain - 2.0).abs() < 1e-9 && limited);
        assert_eq!(normalise_gain(-12.0, -0.5, -23.0, -1.0), (-11.0, false));
        assert_eq!(normalise_gain(f64::NEG_INFINITY, f64::NEG_INFINITY, -14.0, -1.0), (0.0, false));
        assert_eq!(loudness_note(-15.24, -1.0, Some(-1.0)), "Exported at −15.2 LUFS; the −1 dBTP ceiling limited the gain");
        assert_eq!(loudness_note(-9.0, -0.31, None), "Exported at −9.0 LUFS, −0.3 dBTP");
    }

    #[test]
    fn stems_leave_out_reference_tracks_and_include_buses() {
        let mut p = Project::new("Song", SR);
        let a = p.add_track(TrackKind::Audio);
        let rid = p.alloc_id();
        let content = Content::Audio(AudioContent::new("in.wav"));
        let region = Region { id: rid, name: "r".into(), start: 0, length: 10, muted: false, loops: 0, color: None, content };
        p.track_mut(a).unwrap().regions.push(region);
        let b = p.add_track(TrackKind::Instrument);
        let r = p.add_track(TrackKind::Audio);
        p.track_mut(r).unwrap().reference = true;
        let bus = p.add_bus();
        p.add_vca();
        let list = stems(&p);
        let rows: Vec<_> = list.iter().map(|s| (s.id, s.bus, s.checked)).collect();
        assert_eq!(rows, vec![(a, false, true), (b, false, false), (bus, true, true)]);

        p.track_mut(b).unwrap().name = "audio 1".into();
        p.buses[0].name = "Drums/Room".into();
        let choices = ExportChoices { format: Format::Flac24, ..Default::default() };
        let jobs = stem_jobs(&p, &[a, b, r, bus], Path::new("/out"), (0, 10), &choices);
        let files: Vec<String> = jobs
            .iter()
            .map(|j| match &j.target {
                BounceTarget::File(path, spec) => {
                    assert!(spec.normalise.is_none(), "stems keep their relative levels");
                    path.display().to_string()
                }
                _ => panic!("not a file"),
            })
            .collect();
        assert_eq!(files, ["/out/Song - Audio 1.flac", "/out/Song - audio 1 2.flac", "/out/Song - Drums-Room.flac"]);
        assert_eq!(jobs.iter().map(|j| j.track).collect::<Vec<_>>(), vec![Some(a), Some(b), Some(bus)]);
    }

    #[test]
    fn export_span_takes_the_cycle_and_tail() {
        let (ctx, _) = studio("span");
        let mut p = ctx.s().project.clone();
        p.cycle = solstice_model::Range::new(1000, 2000);
        assert_eq!(export_span(&p, false, 0.5, SR), Some((0, 4800 + SR as u64 / 2)));
        assert_eq!(export_span(&p, true, 0.0, SR), Some((0, 4800)), "the cycle is off");
        p.cycle_on = true;
        p.cycle = solstice_model::Range::new(1000, 2000);
        assert_eq!(export_span(&p, true, 1.0, SR), Some((1000, 2000 + SR as u64)));
        p.tracks.clear();
        assert_eq!(export_span(&p, false, 1.0, SR), None);
    }

    #[test]
    fn mix_export_normalises_to_the_target() {
        let sine: Vec<f32> = (0..2 * SR as usize).map(|i| 0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / SR as f32).sin()).collect();
        let (ctx, _) = studio_playing("normalise", sine);
        let dir = ctx.s().bundle.dir.join("export");
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("mix.flac");
        let choices = ExportChoices { format: Format::Flac24, dither: Dither::Tpdf, ..Default::default() };
        assert!(ctx.export_mix(out.clone(), &choices, LoudnessTarget::Broadcast.levels()));
        wait(&ctx);
        let note = ctx.m().message.take().unwrap_or_default();
        assert!(note.starts_with("Exported at −23.0 LUFS"), "{note}");
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(files, vec![std::ffi::OsString::from("mix.flac")], "the float file is removed");

        let (name, frames) = crate::import::import(&out, &dir, SR).unwrap();
        assert_eq!(frames, 2 * SR as u64);
        let data = AudioData::open(&dir.join(name)).unwrap();
        let (l, r): (Vec<f32>, Vec<f32>) = (0..frames).map(|i| (data.sample(0, i), data.sample(1, i))).unzip();
        let m = solstice_dsp::loudness::measure(&l, &r, SR as f64);
        assert!((m.integrated + 23.0).abs() < 0.1, "{}", m.integrated);

        // The sine peaks at its loudness, about −12 dBTP, so 0 LUFS would take it past the ceiling.
        assert!(ctx.export_mix(dir.join("loud.wav"), &choices, Some((0.0, -1.0))));
        wait(&ctx);
        let note = ctx.m().message.take().unwrap_or_default();
        assert_eq!(note, "Exported at −1.0 LUFS; the −1 dBTP ceiling limited the gain");
    }
}
