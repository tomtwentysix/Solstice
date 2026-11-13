//! Offline renders for Bounce, Export All Tracks, Bounce in Place and Track Freeze. Each job is
//! compiled here and rendered on a thread of its own, faster than real time, while the live
//! engine is suspended: both would otherwise run the same plug-in instances at once.

use crate::studio::{Change, Ctx, MAX_BLOCK, Studio};
use adw::prelude::*;
use gtk::glib;
use rtrb::Producer;
use solstice_engine::audio::AudioData;
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

/// One render: which track to play on its own (all of them for `None`), the span, and where
/// the result goes.
#[derive(Clone, Debug)]
pub struct BounceJob {
    pub track: Option<Id>,
    pub start: u64,
    pub end: u64,
    pub target: BounceTarget,
}

#[derive(Clone, Debug)]
pub enum BounceTarget {
    File(PathBuf),
    /// A new track under `track` with the result, the bounced regions muted.
    InPlace { regions: Vec<Id> },
    Freeze,
}

pub struct Running {
    pub job: BounceJob,
    pub progress: Arc<Progress>,
    thread: JoinHandle<Rendered>,
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

enum Step {
    Wait,
    Begin,
    Restart,
    Finished,
}

impl Ctx {
    /// Bounces the whole project to a file.
    pub fn bounce(&self, target: PathBuf) -> bool {
        let end = self.s().project.mix_length();
        if end == 0 {
            return false;
        }
        self.queue_render(vec![BounceJob { track: None, start: 0, end, target: BounceTarget::File(target) }]);
        true
    }

    /// Export All Tracks as Audio Files: each track on its own, from the start.
    pub fn export_tracks(&self, dir: &Path) -> usize {
        let jobs: Vec<BounceJob> = {
            let p = &self.s().project;
            let end = p.mix_length();
            p.tracks
                .iter()
                .filter(|t| !t.regions.is_empty() && !t.reference)
                .map(|t| {
                    let file = format!("{}.wav", file_name(&t.name));
                    BounceJob { track: Some(t.id), start: 0, end, target: BounceTarget::File(dir.join(file)) }
                })
                .collect()
        };
        let n = jobs.len();
        self.queue_render(jobs);
        n
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
        self.queue_render(vec![job]);
        true
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
        self.queue_render(jobs);
        any
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

    fn queue_render(&self, jobs: Vec<BounceJob>) {
        if jobs.is_empty() {
            return;
        }
        let begin = {
            let mut s = self.m();
            let s = &mut *s;
            let idle = matches!(s.render, RenderState::Idle);
            if idle {
                s.batch = (0, 0);
                s.render_rate = s.sample_rate();
                s.render_errors.clear();
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
                RenderState::Running(r) if r.thread.is_finished() => Step::Finished,
                _ => Step::Wait,
            }
        };
        match step {
            Step::Wait => {}
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
        let (result, graph) = thread.join().unwrap_or_else(|_| (Err(std::io::Error::other("the render failed")), None));
        drop(graph);
        drop(plugins);
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
            BounceTarget::File(_) => {}
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
        let (restarted, errors) = {
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
            (restart, std::mem::take(&mut s.render_errors))
        };
        if restarted {
            self.emit(Change::Engine);
        }
        self.emit(Change::Render);
        if !errors.is_empty() {
            self.message(&errors.join("\n"));
        }
    }
}

impl Studio {
    fn start_job(&mut self, job: BounceJob) -> std::io::Result<()> {
        self.batch.0 += 1;
        let name = job.track.and_then(|id| self.project.track(id)).map(|t| file_name(&t.name)).unwrap_or_default();
        let path = match &job.target {
            BounceTarget::File(p) => p.clone(),
            // Fresh names, so the audio cache and undo never meet a file that was overwritten.
            BounceTarget::InPlace { .. } => self.bundle.audio_path(&format!("{name} Bounce {}.wav", self.project.alloc_id())),
            BounceTarget::Freeze => self.bundle.audio_path(&format!("Freeze {name} {}.wav", self.project.alloc_id())),
        };
        if !matches!(job.target, BounceTarget::File(_)) {
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
        let thread = std::thread::Builder::new()
            .name("solstice-render".into())
            .spawn(move || offline::render_wav(graph, sr, span, workers, &p, &out))?;
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
        BounceTarget::File(_) if r.job.track.is_some() => "Exporting".into(),
        _ => "Bouncing".into(),
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
        d.set_body(&if of > 1 { format!("Track {n} of {of}") } else { String::new() });
        glib::ControlFlow::Continue
    });
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
        let dir = std::env::temp_dir().join(format!("solstice-render-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = Bundle::new(&dir);
        std::fs::create_dir_all(bundle.audio_dir()).unwrap();
        solstice_engine::audio::write_wav(&bundle.audio_path("in.wav"), SR, &[vec![0.5; 4800]]).unwrap();
        let mut p = Project::new(name, SR);
        let t = p.add_track(TrackKind::Audio);
        let (rid, sid) = (p.alloc_id(), p.alloc_id());
        let tr = p.track_mut(t).unwrap();
        let content = Content::Audio(AudioContent::new("in.wav"));
        tr.regions.push(Region { id: rid, name: "r".into(), start: 0, length: 4800, muted: false, loops: 0, color: None, content });
        tr.channel.volume_db = -6.0206;
        tr.channel.inserts.push(PluginSlot {
            id: sid,
            uid: "solstice:compressor".into(),
            path: "".into(),
            name: "Compressor".into(),
            bypass: true,
            state: vec![],
            sidechain: None,
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
            s.bounce_queue.push_back(BounceJob { track: None, start: 0, end: 4800, target: BounceTarget::File(out.clone()) });
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
        let (ctx, _) = studio("export");
        let dir = ctx.s().bundle.dir.join("export");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(ctx.export_tracks(&dir), 1);
        wait(&ctx);
        let data = AudioData::open(&dir.join("Audio 1.wav")).unwrap();
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
}
