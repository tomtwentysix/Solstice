use crate::peaks::Peaks;
use crate::render::{BounceJob, RenderState};
use crate::settings::{MidiCycle, Settings};
use gtk::glib;
use rtrb::{Producer, RingBuffer};
use solstice_backend_jack::{Backend, INPUTS};
use solstice_engine::audio::AudioData;
use solstice_engine::recorder::{self, Control, Recorder};
use solstice_engine::{Command, Handle, Options, PluginCell, Resources, Strip};
use solstice_model::*;
use solstice_vst3::{ClassInfo, Instance};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

pub const MAX_BLOCK: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Tracks, regions, routing or plug-ins changed.
    Project,
    Selection,
    Catalog,
    Peaks,
    Engine,
    /// `Studio::browse_to` has a folder for the Browser to show.
    Browse,
    /// `Studio::message` has text to show the user.
    Message,
    /// A display option changed, such as automation view.
    View,
    /// The mixers' shared view options changed.
    Mixer,
    /// Notes were played live; `Studio::live_in` holds them until the next poll.
    LiveNote,
    /// An offline render started, moved on or finished.
    Render,
}

#[derive(Clone, Copy, Default)]
pub struct Meter {
    pub level: [f32; 2],
    pub hold: [f32; 2],
    pub hold_frames: u32,
    pub clip: bool,
    /// Highest peak since the readout was last clicked, linear.
    pub max: f32,
}

pub struct AudioIo {
    pub backend: Backend,
    pub handle: Handle,
    pub recorder: Recorder,
    /// The buffer size `Shared::record_offset` was last worked out for.
    pub period: u32,
}

pub struct Loaded {
    pub inst: crate::host::Host,
    pub cell: Arc<PluginCell>,
    /// The slot state this instance last took or gave. When undo puts a different one in the
    /// project, the instance is given it.
    pub applied: Vec<u8>,
}

/// What a channel in Latch goes on writing once let go: its own controls, plug-in parameters
/// as (slot, parameter), or both.
#[derive(Default)]
struct Latched {
    strip: bool,
    params: Vec<(Id, u32)>,
}

pub struct Studio {
    pub project: Project,
    pub bundle: Bundle,
    pub history: History,
    pub selected_track: Option<Id>,
    pub selected_region: Option<Id>,
    /// Every selected region; `selected_region` is the one clicked last.
    pub selected_regions: Vec<Id>,
    /// Cut or copied regions with their track indices, for Paste.
    pub region_clipboard: Vec<(usize, Region)>,
    pub settings: Settings,
    pub io: Option<AudioIo>,
    pub io_error: Option<String>,
    pub cache: HashMap<String, Arc<AudioData>>,
    pub peaks: HashMap<String, Rc<Peaks>>,
    pub plugins: HashMap<Id, Loaded>,
    pub plugin_errors: HashMap<Id, String>,
    pub strips: HashMap<Option<Id>, Arc<Strip>>,
    pub catalog: Vec<ClassInfo>,
    rings: RefCell<HashMap<Id, Producer<f32>>>,
    registered: HashSet<Id>,
    pub render: RenderState,
    /// Renders waiting behind the current one.
    pub(crate) bounce_queue: std::collections::VecDeque<BounceJob>,
    /// Jobs started and queued in this run of renders, for "Track 2 of 5".
    pub(crate) batch: (usize, usize),
    /// The rate the plug-ins were opened at when this run of renders began, which outlasts
    /// an audio server connection dropped for it.
    pub(crate) render_rate: u32,
    pub(crate) render_token: u32,
    pub(crate) render_errors: Vec<String>,
    /// What finished exports report, such as how loud a mix came out.
    pub(crate) render_notes: Vec<String>,
    /// The audio server connection was dropped to stop the engine for a render.
    pub(crate) restart_audio: bool,
    pub midi_take: Vec<PassEvent>,
    /// Regions recorded since recording last started, so further cycle passes fold into them.
    rec_regions: Vec<Id>,
    /// Notes played during the last pass that was not recording, for Capture as Recording.
    capture: Vec<PassEvent>,
    capture_stale: bool,
    pub live_in: Vec<[u8; 3]>,
    was_recording: bool,
    was_playing: bool,
    /// Automation writing state for the current pass: channels latched on, where each last
    /// wrote, and the project before the pass for a single undo step.
    auto_latched: HashMap<Option<Id>, Latched>,
    auto_last: HashMap<Option<Id>, u64>,
    auto_snapshot: Option<Project>,
    /// Channels whose own fader, knob or button is held.
    strip_touch: HashSet<Option<Id>>,
    /// Each channel's parameters held in plug-in interfaces, as (slot, parameter).
    plugin_held: HashMap<Option<Id>, Vec<(Id, u32)>>,
    /// The automation parameter shown on each track in the tracks area.
    pub auto_view: HashMap<Id, AutoTarget>,
    peaks_pending: HashSet<String>,
    peaks_inflight: HashSet<String>,
    pub meters: HashMap<Option<Id>, Meter>,
    /// Selected tracks and buses; `selected_track` is the most recently clicked of them.
    pub selection: Vec<Id>,
    gesture: Option<Project>,
    /// A drag that began during a render, so all of it is ignored.
    gesture_refused: bool,
    /// "Copy Channel Strip": the channel plus each plug-in's state at the time of copying.
    pub strip_clipboard: Option<Channel>,
    pub browse_to: Option<PathBuf>,
    pub message: Option<String>,
    /// Measures the master output while the Loudness window is open, or while reference tracks
    /// are level matched to it.
    pub master_meter: Option<crate::master_meter::MasterMeter>,
    pub loudness_open: bool,
    /// The main outputs play the reference tracks instead of the mix.
    pub reference_on: bool,
    /// What each reference track's loudness was measured from, and the result once it is in.
    reference_measured: HashMap<Id, (Vec<crate::reference::Segment>, Option<f32>)>,
}

#[derive(Clone)]
pub struct Ctx {
    pub studio: Rc<RefCell<Studio>>,
    listeners: Rc<RefCell<Vec<Rc<dyn Fn(Change)>>>>,
}

impl Ctx {
    pub fn new(studio: Studio) -> Ctx {
        Ctx { studio: Rc::new(RefCell::new(studio)), listeners: Rc::default() }
    }

    pub fn clear_listeners(&self) {
        self.listeners.borrow_mut().clear();
    }

    /// Tells projects apart, for windows kept per project.
    pub fn key(&self) -> usize {
        Rc::as_ptr(&self.studio) as usize
    }

    pub fn on(&self, f: impl Fn(Change) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    pub fn emit(&self, c: Change) {
        let ls: Vec<_> = self.listeners.borrow().clone();
        for l in ls {
            l(c);
        }
    }

    pub fn s(&self) -> std::cell::Ref<'_, Studio> {
        self.studio.borrow()
    }

    pub fn m(&self) -> std::cell::RefMut<'_, Studio> {
        self.studio.borrow_mut()
    }

    /// While a render runs, the project it was compiled from must stay as it is.
    pub fn refuse_while_rendering(&self) -> bool {
        let busy = self.s().render.is_busy();
        if busy {
            self.message("Wait for the render to finish");
        }
        busy
    }

    /// An undoable edit. Rebuilds the engine graph and notifies views. Freezes the edit makes
    /// stale are dropped in the same step.
    pub fn edit(&self, label: &str, f: impl FnOnce(&mut Project)) {
        if self.refuse_while_rendering() {
            return;
        }
        {
            let mut s = self.m();
            let s = &mut *s;
            let mut before = s.project.clone();
            f(&mut s.project);
            drop_stale_freezes(&mut s.project);
            // Plug-ins this edit removes keep their current settings in the undo step, so undo
            // brings them back as they were rather than as last saved.
            let after = slot_ids(&s.project);
            let states: HashMap<Id, Vec<u8>> =
                s.plugins.iter().filter(|(id, _)| !after.contains(id)).map(|(id, l)| (*id, l.inst.state())).collect();
            apply_states(&mut before, &states);
            s.history.push_snapshot(label, before);
            s.sync();
        }
        self.emit(Change::Project);
    }

    /// A change that isn't an undo step, such as cycle or metronome toggles.
    pub fn tweak(&self, f: impl FnOnce(&mut Project)) {
        {
            let mut s = self.m();
            f(&mut s.project);
            s.history.mark_dirty();
            s.sync();
        }
        self.emit(Change::Project);
    }

    pub fn undo(&self) {
        if self.refuse_while_rendering() {
            return;
        }
        let done = {
            let mut s = self.m();
            let s = &mut *s;
            let r = s.history.undo(&mut s.project);
            s.fix_selection();
            s.sync();
            r.is_some()
        };
        if done {
            self.emit(Change::Project);
        }
    }

    /// Takes back the latest edit when it is the one called `label`, without leaving it to
    /// redo, as when a click turns out to be the first half of a double-click.
    pub fn retract(&self, label: &str) {
        let done = {
            let mut s = self.m();
            let s = &mut *s;
            if s.render.is_busy() || s.history.undo_label() != Some(label) {
                return;
            }
            s.history.retract(&mut s.project);
            s.fix_selection();
            s.sync();
            true
        };
        if done {
            self.emit(Change::Project);
        }
    }

    pub fn redo(&self) {
        if self.refuse_while_rendering() {
            return;
        }
        let done = {
            let mut s = self.m();
            let s = &mut *s;
            let r = s.history.redo(&mut s.project);
            s.fix_selection();
            s.sync();
            r.is_some()
        };
        if done {
            self.emit(Change::Project);
        }
    }

    pub fn select_track(&self, id: Option<Id>) {
        {
            let mut s = self.m();
            if s.selected_track == id && s.selection.len() <= 1 {
                return;
            }
            s.selected_track = id;
            s.selection = id.into_iter().collect();
            s.rebuild();
        }
        self.emit(Change::Selection);
    }

    /// Shift-click: adds or removes one channel without touching the rest.
    pub fn toggle_selected(&self, id: Id) {
        {
            let mut s = self.m();
            if let Some(i) = s.selection.iter().position(|x| *x == id) {
                s.selection.remove(i);
                if s.selected_track == Some(id) {
                    s.selected_track = s.selection.last().copied();
                }
            } else {
                s.selection.push(id);
                s.selected_track = Some(id);
            }
            s.rebuild();
        }
        self.emit(Change::Selection);
    }

    pub fn set_selection(&self, ids: Vec<Id>) {
        {
            let mut s = self.m();
            if s.selection == ids {
                return;
            }
            s.selected_track = ids.last().copied();
            s.selection = ids;
            s.rebuild();
        }
        self.emit(Change::Selection);
    }

    /// Shows a short message to the user as a toast.
    pub fn message(&self, text: &str) {
        self.m().message = Some(text.to_string());
        self.emit(Change::Message);
    }

    /// Starts a drag that changes the project live; `end_gesture` makes it one undo step.
    pub fn begin_gesture(&self) {
        if self.refuse_while_rendering() {
            // The rest of this drag is ignored too, even if the render ends during it.
            self.m().gesture_refused = true;
            return;
        }
        let mut s = self.m();
        if s.gesture.is_none() {
            s.gesture = Some(s.project.clone());
        }
    }

    /// Applies a change during a gesture: the engine follows, views are left alone so the
    /// widget being dragged survives.
    pub fn live(&self, rebuild: bool, f: impl FnOnce(&mut Project)) {
        let mut s = self.m();
        if s.render.is_busy() || s.gesture_refused {
            return;
        }
        f(&mut s.project);
        if rebuild {
            s.sync();
        } else {
            for id in s.channel_ids() {
                s.push_strip(id);
            }
        }
    }

    pub fn end_gesture(&self, label: &str) {
        let changed = {
            let mut s = self.m();
            s.gesture_refused = false;
            let Some(before) = s.gesture.take() else { return };
            drop_stale_freezes(&mut s.project);
            if before == s.project {
                false
            } else {
                s.history.push_snapshot(label, before);
                true
            }
        };
        if changed {
            self.emit(Change::Project);
        }
    }

    /// Selects one region. Clicking a region that is already part of a multiple selection
    /// keeps the selection, so the group can be dragged.
    pub fn select_region(&self, id: Option<Id>) {
        {
            let mut s = self.m();
            s.selected_region = id;
            match id {
                Some(r) if s.selected_regions.contains(&r) => {}
                _ => s.selected_regions = id.into_iter().collect(),
            }
            if let Some((ti, _)) = id.and_then(|r| s.project.find_region(r)) {
                s.selected_track = Some(s.project.tracks[ti].id);
            }
        }
        self.emit(Change::Selection);
    }

    pub fn select_regions(&self, ids: Vec<Id>) {
        {
            let mut s = self.m();
            s.selected_region = ids.last().copied();
            s.selected_regions = ids;
        }
        self.emit(Change::Selection);
    }

    /// Shift-click on a region.
    pub fn toggle_region(&self, id: Id) {
        let mut ids = self.s().selected_regions.clone();
        match ids.iter().position(|x| *x == id) {
            Some(i) => {
                ids.remove(i);
            }
            None => ids.push(id),
        }
        self.select_regions(ids);
    }

    /// The regions an edit command acts on: the selection, or else nothing.
    pub fn selected_region_ids(&self) -> Vec<Id> {
        self.s().selected_regions.clone()
    }

    pub fn copy_regions(&self, cut: bool) {
        let ids = self.selected_region_ids();
        if ids.is_empty() {
            return;
        }
        {
            let mut s = self.m();
            let picked: Vec<(usize, Region)> =
                ids.iter().filter_map(|id| s.project.find_region(*id).map(|(t, r)| (t, r.clone()))).collect();
            s.region_clipboard = picked;
        }
        if cut {
            self.edit("Cut", |p| {
                for id in &ids {
                    p.remove_region(*id);
                }
            });
        }
    }

    /// Pastes at the playhead; the earliest region lands there, on the selected track.
    pub fn paste_regions(&self) {
        let (clip, row) = {
            let s = self.s();
            let row = s.selected_track.and_then(|id| s.project.tracks.iter().position(|t| t.id == id));
            (s.region_clipboard.clone(), row)
        };
        if clip.is_empty() {
            return;
        }
        let first = clip.iter().map(|(_, r)| r.start).min().unwrap();
        let top = clip.iter().map(|(t, _)| *t).min().unwrap();
        let at = self.position();
        let rows = row.map_or(0, |r| r as i64 - top as i64);
        let mut new = Vec::new();
        self.edit("Paste", |p| {
            let shifted: Vec<(usize, Region)> =
                clip.iter().map(|(t, r)| (*t, Region { start: r.start - first, ..r.clone() })).collect();
            new = p.paste_regions(&shifted, at, rows);
        });
        self.select_regions(new);
    }

    pub fn send(&self, cmd: Command) {
        if let Some(io) = self.m().io.as_mut() {
            io.handle.send(cmd);
        }
    }

    pub fn position(&self) -> u64 {
        self.s().io.as_ref().map_or(0, |io| io.handle.shared.position.load(Relaxed))
    }

    pub fn playing(&self) -> bool {
        self.s().io.as_ref().is_some_and(|io| io.handle.shared.playing.load(Relaxed))
    }

    pub fn recording(&self) -> bool {
        self.s().io.as_ref().is_some_and(|io| io.handle.shared.recording.load(Relaxed))
    }

    pub fn play_pause(&self) {
        self.send(if self.playing() { Command::Stop } else { Command::Play });
    }

    /// Stop: a second press while stopped returns to the start.
    pub fn stop(&self) {
        if self.playing() {
            self.send(Command::Stop);
        } else {
            self.send(Command::Locate(0));
        }
    }

    pub fn record(&self) {
        let on = !self.recording();
        let bars = self.s().settings.count_in_bars as u64;
        if on && bars > 0 && !self.playing() {
            let frames = self.s().project.time().bar_len() * bars;
            self.send(Command::RecordCountIn(frames));
        } else {
            self.send(Command::Record(on));
        }
    }

    pub fn locate(&self, pos: u64) {
        self.send(Command::Locate(pos));
        self.m().prefetch(pos);
    }

    pub fn nudge_bars(&self, bars: i64) {
        let t = self.s().project.time();
        let bar = t.bar_len() as i64;
        let pos = self.position() as i64;
        let cur = (pos as f64 / bar as f64).round() as i64;
        self.locate(((cur + bars).max(0) * bar) as u64);
    }

    /// Marks a channel's control as held, for Touch and Latch automation.
    pub fn touch(&self, id: Option<Id>, on: bool) {
        let mut s = self.m();
        if on {
            s.strip_touch.insert(id);
        } else {
            s.strip_touch.remove(&id);
        }
        s.sync_touch(id);
    }

    /// Writes automation from the current control values on channels in Write, or in Touch and
    /// Latch while held. A whole pass becomes one undo step when the transport stops.
    fn record_automation(&self) {
        let (playing, pos) = (self.playing(), self.position());
        let finished = {
            let mut s = self.m();
            let s = &mut *s;
            if !playing {
                s.auto_latched.clear();
                s.auto_last.clear();
                let was = std::mem::replace(&mut s.was_playing, false);
                if was {
                    // A gesture whose end was lost would otherwise hold its channel for good.
                    for l in s.plugins.values() {
                        l.inst.clear_held();
                    }
                }
                match s.auto_snapshot.take() {
                    Some(before) if was => {
                        s.history.push_snapshot("Write Automation", before);
                        s.sync();
                        true
                    }
                    _ => false,
                }
            } else {
                s.was_playing = true;
                s.write_automation(pos, |s, slot, param| s.plugins.get(&slot).map(|l| l.inst.param(param) as f32));
                false
            }
        };
        if finished {
            self.emit(Change::Project);
        }
    }

    /// Main-thread housekeeping, run every frame: plug-in UIs, finished takes, MIDI recording.
    pub fn poll(&self) {
        self.poll_render();
        solstice_vst3::idle();
        let mut held: HashMap<Option<Id>, Vec<(Id, u32)>> = HashMap::new();
        let mut edited = false;
        {
            let s = self.s();
            for (sid, l) in &s.plugins {
                l.inst.idle();
                edited |= l.inst.take_dirty();
                let params = l.inst.held_params();
                if let Some(ch) = crate::plugins::slot_channel(&s.project, *sid).filter(|_| !params.is_empty()) {
                    held.entry(ch).or_default().extend(params.into_iter().map(|p| (*sid, p)));
                }
            }
        }
        let first_edit = {
            let mut s = self.m();
            s.set_plugin_held(held);
            s.follow_latencies();
            s.plugin_edited(edited)
        };
        if first_edit {
            self.emit(Change::Project);
        }
        self.record_automation();
        let mut changed = false;
        let live;
        {
            let mut s = self.m();
            let s = &mut *s;
            if s.io.as_ref().is_some_and(|io| io.backend.buffer_size() != io.period) {
                s.apply_record_offset();
            }
            let Some(io) = s.io.as_mut() else { return };
            io.handle.collect();
            let recording = io.handle.shared.recording.load(Relaxed);
            s.live_in.clear();
            while let Ok(e) = io.handle.live_notes.pop() {
                if e.recording {
                    s.midi_take.push((e.pass, e.pos, e.data));
                    continue;
                }
                if e.playing {
                    if std::mem::take(&mut s.capture_stale) {
                        s.capture.clear();
                    }
                    s.capture.push((e.pass, e.pos, e.data));
                }
                s.live_in.push(e.data);
            }
            if !io.handle.shared.playing.load(Relaxed) {
                s.capture_stale = true;
            }
            live = !s.live_in.is_empty();
            let takes: Vec<_> = io.recorder.takes.try_iter().collect();
            for take in takes {
                if s.project.track(take.track).is_none() {
                    continue;
                }
                if !changed {
                    s.history.push("Record", &s.project);
                    changed = true;
                }
                // Another cycle pass over a take from this recording joins it as a take folder.
                let pass = s.rec_regions.iter().copied().find(|id| {
                    s.project.track(take.track).is_some_and(|t| t.regions.iter().any(|r| r.id == *id && r.start == take.start))
                });
                if let Some(rid) = pass
                    && let Some(r) = s.project.region_mut(rid)
                    && let Content::Audio(a) = &mut r.content
                {
                    let n = a.takes.len().max(1) + 1;
                    a.add_take(Take { name: format!("Take {n}"), file: take.file, offset: 0 });
                    r.length = r.length.max(take.frames);
                    continue;
                }
                let id = s.project.alloc_id();
                s.rec_regions.push(id);
                let n = s.project.track(take.track).map_or(0, |t| t.regions.len()) + 1;
                let name = format!("{} #{n}", s.project.track(take.track).unwrap().name);
                s.project.track_mut(take.track).unwrap().regions.push(Region {
                    id,
                    name,
                    start: take.start,
                    length: take.frames,
                    muted: false,
                    loops: 0,
                    color: None,
                    content: Content::Audio(AudioContent {
                        file: take.file,
                        offset: 0,
                        gain_db: 0.0,
                        fade_in: 0,
                        fade_out: 0,
                        takes: Vec::new(),
                        comp: Vec::new(),
                    }),
                });
            }
            if s.was_recording && !recording && !s.midi_take.is_empty() {
                if !changed {
                    s.history.push("Record", &s.project);
                }
                changed |= s.finish_midi_take();
            }
            if !recording {
                s.midi_take.clear();
            } else if !s.was_recording {
                s.rec_regions.clear();
            }
            s.was_recording = recording;
            if changed {
                s.sync();
            }
        }
        if changed {
            self.emit(Change::Project);
        }
        if live {
            self.emit(Change::LiveNote);
        }
    }

    /// Capture as Recording: turns what was played in the last pass into a region.
    pub fn capture_recording(&self) -> bool {
        let made = {
            let mut s = self.m();
            let s = &mut *s;
            let events = s.capture.clone();
            let before = s.project.clone();
            let made = s.midi_region(&events, false);
            if made {
                s.history.push_snapshot("Capture as Recording", before);
                s.sync();
            }
            made
        };
        if made {
            self.emit(Change::Project);
        } else {
            self.message("Nothing played to capture");
        }
        made
    }
}

impl Studio {
    pub fn new(project: Project, bundle: Bundle, settings: Settings) -> Studio {
        Studio {
            project,
            bundle,
            history: {
                let mut h = History::default();
                h.set_limit(settings.undo_levels);
                h
            },
            selected_track: None,
            selected_region: None,
            selected_regions: Vec::new(),
            region_clipboard: Vec::new(),
            settings,
            io: None,
            io_error: None,
            cache: HashMap::new(),
            peaks: HashMap::new(),
            plugins: HashMap::new(),
            plugin_errors: HashMap::new(),
            strips: HashMap::new(),
            catalog: Vec::new(),
            rings: RefCell::default(),
            registered: HashSet::new(),
            render: RenderState::Idle,
            bounce_queue: Default::default(),
            batch: (0, 0),
            render_rate: 0,
            render_token: 0,
            render_errors: Vec::new(),
            render_notes: Vec::new(),
            restart_audio: false,
            midi_take: Vec::new(),
            rec_regions: Vec::new(),
            capture: Vec::new(),
            capture_stale: false,
            live_in: Vec::new(),
            was_recording: false,
            was_playing: false,
            auto_latched: HashMap::new(),
            auto_last: HashMap::new(),
            auto_snapshot: None,
            strip_touch: HashSet::new(),
            plugin_held: HashMap::new(),
            auto_view: HashMap::new(),
            peaks_pending: HashSet::new(),
            peaks_inflight: HashSet::new(),
            meters: HashMap::new(),
            selection: Vec::new(),
            gesture: None,
            gesture_refused: false,
            strip_clipboard: None,
            browse_to: None,
            message: None,
            master_meter: None,
            loudness_open: false,
            reference_on: false,
            reference_measured: HashMap::new(),
        }
    }

    pub fn start_audio(&mut self) {
        let mut parts = None;
        let result = Backend::start("Solstice", self.settings.workers(), |sr| {
            let (engine, handle, rec) = solstice_engine::new(sr);
            parts = Some((handle, rec));
            engine
        });
        match result {
            Ok(backend) => {
                let (handle, rec) = parts.unwrap();
                let recorder = recorder::spawn(rec, backend.sample_rate);
                let _ = recorder.control.send(Control::SetDir(self.bundle.audio_dir()));
                if let Some(frames) = self.settings.buffer_size {
                    let _ = backend.set_buffer_size(frames);
                }
                if self.project.tracks.is_empty() && self.history.undo_label().is_none() {
                    self.project.sample_rate = backend.sample_rate;
                }
                for src in backend.midi_sources() {
                    if self.settings.disabled_midi.contains(&src) {
                        backend.set_midi_connected(&src, false);
                    }
                }
                self.io = Some(AudioIo { backend, handle, recorder, period: 0 });
                self.io_error = None;
                self.apply_record_offset();
                self.registered.clear();
                self.sync();
            }
            Err(e) => self.io_error = Some(format!("Could not connect to the audio server: {e}")),
        }
    }

    /// Hands the audio server back, as when another project window comes to the front.
    pub fn stop_audio(&mut self) {
        self.restart_audio = false;
        if let Some(io) = self.io.as_mut() {
            io.handle.send(Command::Stop);
        }
        self.io = None;
    }

    /// How far takes are moved back for the interface, or `None` without a backend.
    pub fn record_offset(&self) -> Option<u32> {
        let io = self.io.as_ref()?;
        Some((io.backend.round_trip() as i64 + self.settings.record_adjust as i64).max(0) as u32)
    }

    pub fn apply_record_offset(&mut self) {
        let Some(frames) = self.record_offset() else { return };
        let io = self.io.as_mut().unwrap();
        io.handle.shared.record_offset.store(frames, Relaxed);
        io.period = io.backend.buffer_size();
    }

    pub fn sample_rate(&self) -> u32 {
        self.io.as_ref().map_or(self.project.sample_rate, |io| io.backend.sample_rate)
    }

    fn fix_selection(&mut self) {
        if self.selected_track.is_some_and(|id| self.project.track(id).is_none()) {
            self.selected_track = None;
        }
        if self.selected_region.is_some_and(|id| self.project.find_region(id).is_none()) {
            self.selected_region = None;
        }
        let p = &self.project;
        self.selected_regions.retain(|id| p.find_region(*id).is_some());
    }

    /// Brings plug-in instances, record rings and the engine graph in line with the project.
    pub fn sync(&mut self) {
        if self.project.settle_patterns() > 0 {
            self.message = Some("The pattern is now plain notes".into());
        }
        self.project.settle_midi_folders();
        self.fix_selection();
        self.sync_plugins();
        self.sync_rings();
        // A compile maps any file missing from the cache afresh, so freeze files are kept there.
        let frozen: Vec<String> = self.project.tracks.iter().filter_map(|t| Some(t.freeze.as_ref()?.file.clone())).collect();
        for file in frozen {
            self.audio(&file);
        }
        self.rebuild();
    }

    fn slots(&self) -> Vec<PluginSlot> {
        let p = &self.project;
        let mut v: Vec<PluginSlot> = p.master.inserts.clone();
        for t in &p.tracks {
            v.extend(t.channel.inserts.iter().cloned());
            v.extend(t.instrument.iter().cloned());
            v.extend(t.midi_fx.iter().cloned());
        }
        for b in &p.buses {
            v.extend(b.channel.inserts.iter().cloned());
        }
        v
    }

    fn sync_plugins(&mut self) {
        let slots = self.slots();
        let live: HashSet<Id> = slots.iter().map(|s| s.id).collect();
        self.plugins.retain(|id, _| live.contains(id));
        let sr = self.sample_rate() as f64;
        for slot in &slots {
            if let Some(l) = self.plugins.get_mut(&slot.id) {
                l.cell.bypass.store(slot.bypass, Relaxed);
                if !slot.state.is_empty() && slot.state != l.applied {
                    l.inst.set_state_in(&slot.state, &self.bundle.audio_dir());
                    l.applied = slot.state.clone();
                }
                continue;
            }
            if self.plugin_errors.contains_key(&slot.id) {
                continue;
            }
            let info = self.catalog.iter().find(|c| c.uid == slot.uid).cloned().unwrap_or_else(|| ClassInfo {
                uid: slot.uid.clone(),
                name: slot.name.clone(),
                vendor: String::new(),
                subcategories: String::new(),
                path: slot.path.clone(),
            });
            let opened = if slot.uid.starts_with(solstice_dsp::PREFIX) {
                solstice_dsp::create(&slot.uid, sr)
                    .map(|(native, plugin)| (crate::host::Host::Native(Arc::new(native)), plugin))
                    .ok_or_else(|| format!("unknown built-in plug-in {}", slot.uid))
            } else if slot.uid.starts_with(solstice_clap::PREFIX) {
                solstice_clap::Instance::open(&info, sr, MAX_BLOCK).map(|inst| {
                    let plugin = inst.processor(MAX_BLOCK);
                    (crate::host::Host::Clap(inst), plugin)
                })
            } else if slot.uid.starts_with(solstice_lv2::PREFIX) {
                solstice_lv2::Instance::open(&info, sr, MAX_BLOCK).map(|inst| {
                    let plugin = inst.processor();
                    (crate::host::Host::Lv2(inst), plugin)
                })
            } else {
                Instance::open(&info, sr, MAX_BLOCK).map(|inst| {
                    let plugin = inst.processor();
                    (crate::host::Host::Vst3(inst), plugin)
                })
            };
            match opened {
                Ok((inst, plugin)) => {
                    if !slot.state.is_empty() {
                        inst.set_state_in(&slot.state, &self.bundle.audio_dir());
                    }
                    let cell = PluginCell::new(plugin, inst.latency());
                    cell.bypass.store(slot.bypass, Relaxed);
                    self.plugins.insert(slot.id, Loaded { inst, cell, applied: slot.state.clone() });
                }
                Err(e) => {
                    eprintln!("solstice: {}: {e}", slot.name);
                    self.plugin_errors.insert(slot.id, e);
                }
            }
        }
    }

    fn sync_rings(&mut self) {
        let Some(io) = &self.io else { return };
        let sr = self.sample_rate() as usize;
        for t in self.project.tracks.iter().filter(|t| t.armed && t.kind == TrackKind::Audio) {
            if self.registered.insert(t.id) {
                let (p, c) = RingBuffer::new(sr * 2 * 4);
                let _ = io.recorder.control.send(Control::Register(t.id, c));
                self.rings.borrow_mut().insert(t.id, p);
            }
        }
    }

    pub fn rebuild(&mut self) {
        let opts = Options {
            max_block: MAX_BLOCK,
            inputs: INPUTS,
            auto_input: self.settings.auto_input,
            low_latency: self
                .settings
                .low_latency
                .then(|| (self.settings.low_latency_ms / 1000.0 * self.sample_rate() as f32) as u32),
            midi_track: self.selected_track,
            midi_thru: self.settings.midi_thru,
            click_gain: db_to_gain(self.settings.click_level_db),
            click_accent: self.settings.click_accent,
            click_groups_only: self.settings.click_groups_only,
            outputs: self.io.as_ref().map_or(2, |io| io.backend.outputs),
            isolate: None,
            freeze: None,
        };
        for id in self.channel_ids() {
            self.push_strip(id);
        }
        // A plug-in's side-chain input is only switched on while the engine feeds it a key:
        // some plug-ins behave differently with it on, even when it is silent.
        let keys = solstice_engine::side_chain_keys(&self.project, &|slot| self.plugins.contains_key(&slot));
        for (id, l) in &self.plugins {
            if l.inst.set_sidechain_active(keys.contains_key(id)) {
                l.cell.latency.store(l.inst.latency(), Relaxed);
            }
        }
        // The render owns the plug-ins until it is done; the engine gets a graph again then.
        if self.io.is_none() || !matches!(self.render, RenderState::Idle) {
            return;
        }
        let graph = solstice_engine::compile(&self.project, &*self, &opts);
        self.io.as_mut().unwrap().handle.set_graph(graph);
    }

    /// Every channel: the stereo output, then tracks, then buses.
    pub fn channel_ids(&self) -> Vec<Option<Id>> {
        let p = &self.project;
        std::iter::once(None)
            .chain(p.tracks.iter().map(|t| Some(t.id)))
            .chain(p.buses.iter().map(|b| Some(b.id)))
            .collect()
    }

    /// Pulls each strip's peak since the last frame: the bar falls back, the hold line waits
    /// about two seconds, and the clip light stays on until it is clicked.
    pub fn update_levels(&mut self) {
        for (id, strip) in &self.strips {
            let peak = strip.take_peaks();
            let m = self.meters.entry(*id).or_default();
            for c in 0..2 {
                m.level[c] = peak[c].max(m.level[c] * 0.88);
            }
            if peak[0].max(peak[1]) >= m.hold[0].max(m.hold[1]) {
                m.hold = peak;
                m.hold_frames = 120;
            } else if m.hold_frames > 0 {
                m.hold_frames -= 1;
            } else {
                m.hold = [m.hold[0] * 0.95, m.hold[1] * 0.95];
            }
            m.clip |= peak[0] > 1.0 || peak[1] > 1.0;
            m.max = m.max.max(peak[0]).max(peak[1]);
        }
    }

    pub fn run_master_meter(&mut self) {
        let matching = self.settings.reference_match && self.has_reference();
        if !self.loudness_open && !matching {
            if self.master_meter.is_some() {
                self.close_master_meter();
            }
            return;
        }
        let (rate, reset) = (self.sample_rate(), self.settings.loudness_reset_on_play);
        let m = self.master_meter.get_or_insert_with(|| crate::master_meter::MasterMeter::new(rate));
        if let Some(io) = self.io.as_mut() {
            m.run(&mut io.handle, rate, reset);
        }
    }

    pub fn has_reference(&self) -> bool {
        self.project.tracks.iter().any(|t| t.reference)
    }

    /// Passes the Reference switch to the engine and moves each reference track's level
    /// match a step towards the mix's loudness.
    pub fn run_reference(&mut self) {
        self.reference_on &= self.has_reference();
        if let Some(io) = &self.io {
            io.handle.shared.reference.store(self.reference_on, Relaxed);
        }
        let mix = self.master_meter.as_ref().and_then(|m| crate::reference::mix_loudness(&m.loudness));
        for t in &self.project.tracks {
            let Some(strip) = self.strips.get(&Some(t.id)) else { continue };
            if !t.reference {
                strip.match_db.set(0.0);
                continue;
            }
            let fader = crate::reference::fader_db(&self.project, t);
            let target = crate::reference::match_target(self.settings.reference_match, mix, t.reference_lufs, fader);
            let current = strip.match_db.get();
            if let Some(target) = target.filter(|v| *v != current) {
                strip.match_db.set(crate::reference::glide(current, target));
            }
        }
    }

    /// Reference tracks whose audio changed since their loudness was measured, with what to
    /// measure. A value saved with the project is trusted until the audio changes.
    pub fn take_reference_jobs(&mut self) -> Vec<(Id, Vec<crate::reference::Segment>, Vec<Arc<AudioData>>)> {
        let current: Vec<(Id, Vec<crate::reference::Segment>, Option<f32>)> =
            self.project.tracks.iter().filter(|t| t.reference).map(|t| (t.id, crate::reference::segments(t), t.reference_lufs)).collect();
        self.reference_measured.retain(|id, _| current.iter().any(|c| c.0 == *id));
        let mut jobs = Vec::new();
        for (id, segs, saved) in current {
            match self.reference_measured.get(&id) {
                Some((s, v)) if *s == segs => {
                    // Undo can bring back the track from before its measurement came in.
                    if saved.is_none() && v.is_some() {
                        let v = *v;
                        if let Some(t) = self.project.track_mut(id) {
                            t.reference_lufs = v;
                        }
                    }
                    continue;
                }
                None if saved.is_some() => {
                    self.reference_measured.insert(id, (segs, saved));
                    continue;
                }
                _ => {}
            }
            let data: Option<Vec<Arc<AudioData>>> = segs.iter().map(|s| self.audio(&s.file)).collect();
            self.reference_measured.insert(id, (segs.clone(), None));
            if let Some(data) = data {
                jobs.push((id, segs, data));
            }
        }
        jobs
    }

    pub fn set_reference_lufs(&mut self, id: Id, segs: &[crate::reference::Segment], lufs: Option<f32>) {
        let Some(entry) = self.reference_measured.get_mut(&id).filter(|e| e.0 == segs) else { return };
        entry.1 = lufs;
        if let Some(t) = self.project.track_mut(id).filter(|t| t.reference && t.reference_lufs != lufs) {
            t.reference_lufs = lufs;
            self.history.mark_dirty();
        }
    }

    pub fn close_master_meter(&mut self) {
        if let (Some(m), Some(io)) = (self.master_meter.as_mut(), self.io.as_mut()) {
            m.detach(&mut io.handle);
        }
        self.master_meter = None;
    }

    /// Channels a strip control acts on: the whole selection if the strip is part of it.
    pub fn gang(&self, id: Option<Id>) -> Vec<Option<Id>> {
        match id {
            Some(i) if self.selection.contains(&i) => self.selection.iter().map(|x| Some(*x)).collect(),
            _ => vec![id],
        }
    }

    pub fn push_strip(&mut self, id: Option<Id>) {
        if let Some(ch) = self.project.channel(id) {
            let dim = if id.is_none() && self.project.dim { 20.0 } else { 0.0 };
            let (v, p, m) = (ch.volume_db - dim, ch.pan, ch.mute);
            self.strip(id).set(v, p, m);
        }
    }

    pub fn copy_strip(&mut self, id: Option<Id>) {
        let Some(mut ch) = self.project.channel(id).cloned() else { return };
        for slot in &mut ch.inserts {
            if let Some(l) = self.plugins.get(&slot.id) {
                slot.state = l.inst.state();
            }
        }
        self.strip_clipboard = Some(ch);
    }

    /// Removes an untitled bundle that was never saved and holds no recordings.
    pub fn cleanup_unused_bundle(&mut self) {
        let dir = &self.bundle.dir;
        let audio_empty = std::fs::read_dir(self.bundle.audio_dir())
            .map_or(true, |d| d.flatten().all(|e| e.file_name().to_string_lossy().starts_with(crate::templates::KIT_PREFIX)));
        if !Bundle::is_bundle(dir) && audio_empty {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    pub fn strip(&mut self, id: Option<Id>) -> Arc<Strip> {
        self.strips.entry(id).or_default().clone()
    }

    /// Tells the engine whether anything on a channel is held: its own controls or a parameter
    /// in one of its plug-ins' interfaces.
    fn sync_touch(&mut self, id: Option<Id>) {
        let on = self.strip_touch.contains(&id) || self.plugin_held.contains_key(&id);
        self.strip(id).touched.store(on, Relaxed);
    }

    fn set_plugin_held(&mut self, held: HashMap<Option<Id>, Vec<(Id, u32)>>) {
        let channels: HashSet<Option<Id>> = self.plugin_held.keys().chain(held.keys()).copied().collect();
        self.plugin_held = held;
        for id in channels {
            self.sync_touch(id);
        }
    }

    /// An LV2 plug-in reports its latency on an output port, which can change while it runs, as
    /// when its lookahead is changed in its own interface. Delay compensation follows it.
    fn follow_latencies(&mut self) {
        let mut changed = false;
        for l in self.plugins.values() {
            if let crate::host::Host::Lv2(i) = &l.inst {
                let now = i.latency();
                changed |= l.cell.latency.swap(now, Relaxed) != now;
            }
        }
        if changed {
            self.rebuild();
        }
    }

    /// Marks the project changed when a plug-in changed its own settings. True for the edit
    /// that first makes it unsaved, when views need telling.
    fn plugin_edited(&mut self, edited: bool) -> bool {
        let first = edited && !self.history.is_dirty();
        if edited {
            self.history.mark_dirty();
        }
        first
    }

    /// Writes one moment of a pass at `pos`. Write records every lane the channel has; Touch
    /// and Latch only what is or was held, so other lanes keep playing back as they were.
    /// `param` reads a plug-in parameter, 0..1.
    fn write_automation(&mut self, pos: u64, param: impl Fn(&Studio, Id, u32) -> Option<f32>) {
        let tenth = self.project.sample_rate as u64 / 10;
        for id in self.channel_ids() {
            let strip = self.strip_touch.contains(&id);
            let held = self.plugin_held.get(&id).cloned().unwrap_or_default();
            let Some(ch) = self.project.channel(id) else { continue };
            if ch.auto_mode == AutoMode::Latch && (strip || !held.is_empty()) {
                let l = self.auto_latched.entry(id).or_default();
                l.strip |= strip;
                for h in &held {
                    if !l.params.contains(h) {
                        l.params.push(*h);
                    }
                }
            }
            let (strip, params) = match ch.auto_mode {
                AutoMode::Write => {
                    let mut params: Vec<(Id, u32)> = ch
                        .automation
                        .iter()
                        .filter_map(|l| match l.target {
                            AutoTarget::Plugin { slot, param } => Some((slot, param)),
                            _ => None,
                        })
                        .collect();
                    for h in held {
                        if !params.contains(&h) {
                            params.push(h);
                        }
                    }
                    (true, params)
                }
                AutoMode::Touch => (strip, held),
                AutoMode::Latch => self.auto_latched.get(&id).map_or((false, Vec::new()), |l| (l.strip, l.params.clone())),
                _ => (false, Vec::new()),
            };
            if !strip && params.is_empty() {
                self.auto_last.remove(&id);
                continue;
            }
            if self.auto_snapshot.is_none() {
                self.auto_snapshot = Some(self.project.clone());
            }
            let mut values = Vec::new();
            if strip {
                values.push((AutoTarget::Volume, ch.volume_db.max(-60.0)));
                values.push((AutoTarget::Pan, ch.pan));
                if ch.mute || ch.lane(AutoTarget::Mute).is_some() {
                    values.push((AutoTarget::Mute, if ch.mute { 1.0 } else { 0.0 }));
                }
                for (i, send) in ch.sends.iter().enumerate() {
                    if ch.lane(AutoTarget::Send(i)).is_some() {
                        values.push((AutoTarget::Send(i), send.level_db));
                    }
                }
            }
            for (slot, p) in params {
                if let Some(v) = param(self, slot, p) {
                    values.push((AutoTarget::Plugin { slot, param: p }, v));
                }
            }
            // A cycle wrap jumps back; start a fresh stretch there.
            let from = self.auto_last.get(&id).copied().filter(|f| *f <= pos).unwrap_or(pos);
            let ch = self.project.channel_mut(id).unwrap();
            for (target, v) in values {
                let lane = ch.lane_mut(target);
                let last = lane.points.iter().rev().find(|p| p.pos >= from && p.pos <= pos).copied();
                let changed = lane.value_at(pos).is_none_or(|x| (x - v).abs() > 0.01);
                let stale = last.is_none_or(|p| pos - p.pos >= tenth);
                let add = if changed || stale { vec![AutoPoint { pos, value: v }] } else { Vec::new() };
                // Keep the points written earlier in this stretch; overwrite only what's new.
                let start = last.map_or(from, |p| p.pos + 1);
                lane.write(start.min(pos), pos, &add);
            }
            self.auto_last.insert(id, pos);
        }
    }

    pub fn audio(&mut self, file: &str) -> Option<Arc<AudioData>> {
        if let Some(a) = self.cache.get(file) {
            return Some(a.clone());
        }
        let a = Arc::new(AudioData::open(&self.bundle.audio_path(file)).ok()?);
        self.cache.insert(file.to_string(), a.clone());
        Some(a)
    }

    /// Returns the peaks for a file, queueing a background build on first request.
    pub fn peaks_for(&mut self, file: &str) -> Option<Rc<Peaks>> {
        if let Some(p) = self.peaks.get(file) {
            return Some(p.clone());
        }
        if !self.peaks_inflight.contains(file) {
            self.peaks_pending.insert(file.to_string());
        }
        None
    }

    pub fn take_peak_requests(&mut self) -> Vec<(String, Arc<AudioData>)> {
        let mut out = Vec::new();
        for f in std::mem::take(&mut self.peaks_pending) {
            if let Some(a) = self.audio(&f) {
                self.peaks_inflight.insert(f.clone());
                out.push((f, a));
            }
        }
        out
    }

    pub fn add_peaks(&mut self, file: String, peaks: Peaks) {
        self.peaks_inflight.remove(&file);
        self.peaks.insert(file, Rc::new(peaks));
    }

    pub fn prefetch(&mut self, pos: u64) {
        for (f, frame, len) in self.prefetch_spans(pos) {
            if let Some(a) = self.audio(&f) {
                a.prefetch(frame, len);
            }
        }
    }

    /// The audio the engine reads in the next five seconds from `pos`, as (file, first frame,
    /// frames).
    pub(crate) fn prefetch_spans(&self, pos: u64) -> Vec<(String, u64, u64)> {
        let ahead = self.sample_rate() as u64 * 5;
        let p = &self.project;
        let mut spans = Vec::new();
        for t in &p.tracks {
            if let Some(f) = live_freeze(p, t) {
                if f.start + f.frames > pos && f.start < pos + ahead {
                    spans.push((f.file.clone(), pos.saturating_sub(f.start), ahead));
                }
                continue;
            }
            for r in t.regions.iter().filter(|r| r.end() > pos && r.start < pos + ahead) {
                if let Content::Audio(a) = &r.content {
                    spans.push((a.file.clone(), a.offset + pos.saturating_sub(r.start), ahead));
                }
            }
        }
        spans
    }

    fn finish_midi_take(&mut self) -> bool {
        let events = std::mem::take(&mut self.midi_take);
        let folders = self.settings.midi_cycle == MidiCycle::TakeFolder;
        self.midi_region(&events, folders)
    }

    /// Adds a MIDI region on the selected (or armed) instrument track from live events, as a
    /// take folder over the cycle when `folders` is set and the cycle is on.
    fn midi_region(&mut self, events: &[PassEvent], folders: bool) -> bool {
        let Some(track) = self
            .selected_track
            .filter(|id| self.project.track(*id).is_some_and(|t| t.kind == TrackKind::Instrument))
            .or_else(|| self.project.tracks.iter().find(|t| t.armed && t.kind == TrackKind::Instrument).map(|t| t.id))
        else {
            return false;
        };
        let p = &self.project;
        let cycle = (p.cycle_on && !p.cycle.is_empty()).then_some(p.cycle);
        let Some((start, end, content)) = recorded_midi(&p.time(), events, cycle, folders) else { return false };
        let id = self.project.alloc_id();
        let t = self.project.track_mut(track).unwrap();
        let n = t.regions.len() + 1;
        let name = format!("{} #{n}", t.name);
        t.regions.push(Region { id, name, start, length: end - start, muted: false, loops: 0, color: None, content: Content::Midi(content) });
        true
    }

    /// Copies each plug-in's current state into its slot, ready for saving.
    pub fn capture_plugin_states(&mut self) {
        let states: HashMap<Id, Vec<u8>> = self.plugins.iter().map(|(id, l)| (*id, l.inst.state())).collect();
        for (id, st) in &states {
            if let Some(l) = self.plugins.get_mut(id) {
                l.applied = st.clone();
            }
        }
        apply_states(&mut self.project, &states);
    }

    /// Makes a change made inside a plug-in, such as to its sample zones, an undo step:
    /// `before` is the plug-in's state from just before the change.
    pub fn plugin_state_edit(&mut self, sid: Id, before: Vec<u8>, label: &str) {
        let Some(after) = self.plugins.get(&sid).map(|l| l.inst.state()) else { return };
        if let Some(l) = self.plugins.get_mut(&sid) {
            l.applied = after.clone();
        }
        let mut snapshot = self.project.clone();
        apply_states(&mut snapshot, &HashMap::from([(sid, before)]));
        apply_states(&mut self.project, &HashMap::from([(sid, after)]));
        self.history.push_snapshot(label, snapshot);
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        self.capture_plugin_states();
        self.bundle.save(&self.project)?;
        self.history.mark_saved();
        self.remember_recent();
        Ok(())
    }

    pub fn remember_recent(&mut self) {
        let dir = self.bundle.dir.clone();
        self.settings.recent = Some(dir.clone());
        self.settings.recent_list.retain(|p| *p != dir);
        self.settings.recent_list.insert(0, dir);
        self.settings.recent_list.truncate(10);
        self.settings.save();
    }

    /// Writes the crash-recovery copy if there are unsaved changes.
    pub fn autosave(&mut self) {
        if !self.history.is_dirty() {
            return;
        }
        self.capture_plugin_states();
        if let Err(e) = self.bundle.autosave(&self.project) {
            eprintln!("solstice: autosave: {e}");
        }
    }

    /// Plug-ins offered in pickers: everything scanned except those switched off in Preferences.
    /// Enabled plug-ins. One installed in several formats is listed once, preferring VST3, then
    /// CLAP, then LV2.
    pub fn available_plugins(&self) -> Vec<solstice_vst3::ClassInfo> {
        let rank = |c: &solstice_vst3::ClassInfo| {
            if c.uid.starts_with(solstice_dsp::PREFIX) {
                None
            } else if c.uid.starts_with(solstice_lv2::PREFIX) {
                Some(2)
            } else if c.uid.starts_with(solstice_clap::PREFIX) {
                Some(1)
            } else {
                Some(0)
            }
        };
        let mut best: HashMap<(String, String), u8> = HashMap::new();
        for c in &self.catalog {
            if let Some(r) = rank(c) {
                let b = best.entry(same_plugin(c)).or_insert(r);
                *b = (*b).min(r);
            }
        }
        self.catalog
            .iter()
            .filter(|c| !self.settings.disabled_plugins.contains(&c.uid))
            .filter(|c| rank(c).is_none_or(|r| best.get(&same_plugin(c)).is_none_or(|b| r <= *b)))
            .cloned()
            .collect()
    }

    /// Saves to a new bundle, copying the audio files across.
    pub fn save_as(&mut self, dir: &Path) -> std::io::Result<()> {
        if !matches!(self.render, RenderState::Idle) {
            return Err(std::io::Error::other("a render is still running"));
        }
        let new = Bundle::new(dir);
        std::fs::create_dir_all(new.audio_dir())?;
        if let Ok(rd) = std::fs::read_dir(self.bundle.audio_dir()) {
            // Renders write to a `.part` file and rename it once complete.
            for e in rd.flatten().filter(|e| e.path().extension().is_none_or(|x| x != "part")) {
                std::fs::copy(e.path(), new.audio_dir().join(e.file_name()))?;
            }
        }
        self.project.name = dir.file_stem().map_or("Untitled".into(), |s| s.to_string_lossy().into_owned());
        self.bundle = new;
        self.cache.clear();
        if let Some(io) = &self.io {
            let _ = io.recorder.control.send(Control::SetDir(self.bundle.audio_dir()));
        }
        self.save()?;
        self.sync();
        Ok(())
    }

    pub fn open(&mut self, dir: &Path) -> std::io::Result<()> {
        let bundle = Bundle::new(dir);
        let project = bundle.load()?;
        self.replace_project(project, bundle);
        self.remember_recent();
        Ok(())
    }

    /// Switches to another project, removing the current one's folder if it was never used.
    pub fn replace_project(&mut self, project: Project, bundle: Bundle) {
        if bundle.dir != self.bundle.dir {
            self.cleanup_unused_bundle();
        }
        self.abort_render();
        self.project = project;
        self.bundle = bundle;
        self.reset_session();
        if std::mem::take(&mut self.restart_audio) && self.io.is_none() {
            self.start_audio();
        }
    }

    pub fn save_template(&mut self, name: &str) -> std::io::Result<()> {
        self.capture_plugin_states();
        crate::templates::save_user(&self.project, &self.bundle, name)
    }

    fn reset_session(&mut self) {
        self.history.clear();
        self.selected_track = None;
        self.selected_region = None;
        self.cache.clear();
        self.peaks.clear();
        self.peaks_pending.clear();
        self.peaks_inflight.clear();
        self.reference_measured.clear();
        self.reference_on = false;
        if let Some(m) = self.master_meter.as_mut() {
            m.reset();
        }
        for strip in self.strips.values() {
            strip.match_db.set(0.0);
        }
        self.plugins.clear();
        self.plugin_errors.clear();
        if let Some(io) = &self.io {
            let _ = io.recorder.control.send(Control::SetDir(self.bundle.audio_dir()));
            io.handle.shared.position.store(0, Relaxed);
        }
        self.sync();
        if let Some(io) = self.io.as_mut() {
            io.handle.send(Command::Stop);
            io.handle.send(Command::Locate(0));
        }
    }
}

impl Resources for Studio {
    fn audio(&self, file: &str) -> Option<Arc<AudioData>> {
        self.cache
            .get(file)
            .cloned()
            .or_else(|| AudioData::open(&self.bundle.audio_path(file)).ok().map(Arc::new))
    }

    fn plugin(&self, slot: Id) -> Option<Arc<PluginCell>> {
        self.plugins.get(&slot).map(|l| l.cell.clone())
    }

    fn strip(&self, id: Option<Id>) -> Arc<Strip> {
        self.strips.get(&id).cloned().unwrap_or_default()
    }

    fn record_ring(&self, track: Id) -> Option<Producer<f32>> {
        self.rings.borrow_mut().remove(&track)
    }

    fn class_is_stereo(&self, uid: &str) -> bool {
        crate::variants::slot_is_stereo(&self.catalog, uid)
    }
}

pub fn projects_dir() -> PathBuf {
    if let Some(d) = crate::settings::dev_root() {
        return d.join("projects");
    }
    glib::user_special_dir(glib::UserDirectory::Music).unwrap_or_else(glib::home_dir).join("Solstice")
}

/// The next free "Untitled n" bundle folder and its project name.
pub fn untitled_dir() -> (PathBuf, String) {
    let base = projects_dir();
    let mut n = 1;
    loop {
        let d = base.join(format!("Untitled {n}.solstice"));
        if !d.exists() {
            return (d, format!("Untitled {n}"));
        }
        n += 1;
    }
}

/// A fresh project in its own bundle, so recordings have somewhere to go before the first save.
pub fn untitled(sample_rate: u32) -> (Project, Bundle) {
    let (dir, name) = untitled_dir();
    let _ = std::fs::create_dir_all(dir.join("Audio Files"));
    let mut p = Project::new(&name, sample_rate);
    p.add_track(TrackKind::Audio);
    p.add_track(TrackKind::Instrument);
    (p, Bundle::new(dir))
}

/// What identifies a plug-in across formats: its maker's first word, and its name without that
/// word, which LV2 names tend to start with ("LSP Compressor Stereo").
fn same_plugin(c: &solstice_vst3::ClassInfo) -> (String, String) {
    let maker = c.vendor.split_whitespace().next().unwrap_or("").to_lowercase();
    let name = c.name.to_lowercase();
    let name = match name.strip_prefix(&maker).and_then(|n| n.strip_prefix(' ')) {
        Some(rest) if !maker.is_empty() => rest.to_string(),
        _ => name,
    };
    (name, maker)
}

/// Unfreezes tracks whose freeze no longer matches what they would render.
fn drop_stale_freezes(p: &mut Project) {
    for i in 0..p.tracks.len() {
        if p.tracks[i].freeze.is_some() && live_freeze(p, &p.tracks[i]).is_none() {
            p.tracks[i].freeze = None;
        }
    }
}

/// Writes saved plug-in states into the matching slots.
fn apply_states(p: &mut Project, states: &HashMap<Id, Vec<u8>>) {
    let mut apply = |slot: &mut PluginSlot| {
        if let Some(s) = states.get(&slot.id) {
            slot.state = s.clone();
        }
    };
    p.master.inserts.iter_mut().for_each(&mut apply);
    for t in &mut p.tracks {
        t.channel.inserts.iter_mut().for_each(&mut apply);
        t.instrument.iter_mut().for_each(&mut apply);
        t.midi_fx.iter_mut().for_each(&mut apply);
    }
    for b in &mut p.buses {
        b.channel.inserts.iter_mut().for_each(&mut apply);
    }
}

fn slot_ids(p: &Project) -> HashSet<Id> {
    let mut ids: HashSet<Id> = p.master.inserts.iter().map(|s| s.id).collect();
    for t in &p.tracks {
        ids.extend(t.channel.inserts.iter().chain(&t.instrument).chain(&t.midi_fx).map(|s| s.id));
    }
    for b in &p.buses {
        ids.extend(b.channel.inserts.iter().map(|s| s.id));
    }
    ids
}

/// A live MIDI message as (cycle pass, sample position, bytes).
pub type PassEvent = (u32, u64, [u8; 3]);

/// The events of each cycle pass, in pass order.
fn midi_passes(events: &[PassEvent]) -> Vec<Vec<(u64, [u8; 3])>> {
    let mut passes: Vec<u32> = events.iter().map(|e| e.0).collect();
    passes.sort_unstable();
    passes.dedup();
    passes.into_iter().map(|p| events.iter().filter(|e| e.0 == p).map(|e| (e.1, e.2)).collect()).collect()
}

/// The start, end and content of a region recorded from live events. With `folders` and a
/// cycle, two or more passes with notes make a take folder over it; otherwise the passes merge,
/// each one's held notes ending at the region end. `None` when nothing was played.
fn recorded_midi(time: &TimeMap, events: &[PassEvent], cycle: Option<Range>, folders: bool) -> Option<(u64, u64, MidiContent)> {
    let bar = time.bar_len();
    let first = events.iter().map(|e| e.1).min()?;
    let last = events.iter().map(|e| e.1).max()?;
    let (mut start, end) = (first / bar * bar, (last / bar + 1) * bar);
    // Notes played just ahead of the cycle start, as the plug-in delay can make the downbeat,
    // join the region there at its first tick rather than open another bar before it.
    if let Some(c) = cycle
        && first < c.start
        && c.start - first <= time.beat_len()
    {
        start = c.start;
    }
    let passes = midi_passes(events);
    if let Some(c) = cycle.filter(|_| folders) {
        // Notes played well before the cycle, on the way into it, stay in the first take.
        let (start, end) = (start.min(c.start), if last > c.end { end } else { c.end });
        let mut takes = passes.iter().map(|p| notes_from_events(time, start, end, p)).filter(|n| !n.is_empty());
        let mut m = MidiContent { notes: takes.next().unwrap_or_default(), ..Default::default() };
        for (i, notes) in takes.enumerate() {
            m.add_take(MidiTake { name: format!("Take {}", i + 2), notes });
        }
        if m.is_folder() {
            return Some((start, end, m));
        }
    }
    let mut notes: Vec<Note> = passes.iter().flat_map(|p| notes_from_events(time, start, end, p)).collect();
    notes.sort_by_key(|n| (n.start, n.pitch));
    (!notes.is_empty()).then(|| (start, end, MidiContent { notes, ..Default::default() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOT: Id = 900;

    fn studio(mode: AutoMode) -> (Studio, Id) {
        let mut p = Project::new("T", 48000);
        let track = p.add_track(TrackKind::Audio);
        let ch = p.channel_mut(Some(track)).unwrap();
        ch.auto_mode = mode;
        ch.volume_db = -3.0;
        ch.lane_mut(AutoTarget::Volume).points = vec![AutoPoint { pos: 0, value: -12.0 }, AutoPoint { pos: 96000, value: 0.0 }];
        ch.lane_mut(AutoTarget::Plugin { slot: SLOT, param: 1 }).points = vec![AutoPoint { pos: 0, value: 0.8 }];
        let dir = std::env::temp_dir().join("solstice-automation-test-unused");
        (Studio::new(p, Bundle::new(dir), Settings::default()), track)
    }

    /// Writes a pass from 0 to 1 s, with every plug-in parameter at 0.3.
    fn pass(s: &mut Studio) {
        for pos in (0..=48000).step_by(4800) {
            s.write_automation(pos, |_, _, _| Some(0.3));
        }
    }

    fn lane(s: &Studio, track: Id, target: AutoTarget) -> Option<Vec<AutoPoint>> {
        s.project.channel(Some(track)).unwrap().lane(target).map(|l| l.points.clone())
    }

    #[test]
    fn plugins_in_several_formats_are_listed_once() {
        let (mut s, _) = studio(AutoMode::Read);
        let class = |uid: &str, name: &str, vendor: &str| solstice_vst3::ClassInfo {
            uid: uid.into(),
            name: name.into(),
            vendor: vendor.into(),
            subcategories: "Fx".into(),
            path: PathBuf::new(),
        };
        s.catalog = vec![
            class("6473", "Compressor Stereo", "LSP VST3"),
            class("clap:in.lsp-plug.compressor_stereo", "Compressor Stereo", "LSP CLAP"),
            class("lv2:http://lsp-plug.in/plugins/lv2/compressor_stereo", "LSP Compressor Stereo", "LSP LV2"),
            class("clap:gate", "Gate", "LSP CLAP"),
            class("lv2:gate", "LSP Gate", "LSP LV2"),
            class("lv2:piano", "MDA Piano", "David Robillard"),
        ];
        let uids: Vec<String> = s.available_plugins().into_iter().map(|c| c.uid).collect();
        assert_eq!(uids, ["6473", "clap:gate", "lv2:piano"]);
    }

    #[test]
    fn a_held_plugin_parameter_writes_only_its_own_lane_in_touch() {
        let (mut s, track) = studio(AutoMode::Touch);
        let volume = lane(&s, track, AutoTarget::Volume);
        let held = AutoTarget::Plugin { slot: SLOT, param: 2 };
        s.set_plugin_held(HashMap::from([(Some(track), vec![(SLOT, 2)])]));
        assert!(s.strip(Some(track)).touched.load(Relaxed));
        pass(&mut s);
        assert_eq!(lane(&s, track, AutoTarget::Volume), volume);
        assert_eq!(lane(&s, track, AutoTarget::Pan), None);
        assert_eq!(lane(&s, track, AutoTarget::Plugin { slot: SLOT, param: 1 }), Some(vec![AutoPoint { pos: 0, value: 0.8 }]));
        let written = lane(&s, track, held).unwrap();
        assert!(!written.is_empty() && written.iter().all(|p| p.value == 0.3));
    }

    #[test]
    fn a_held_fader_leaves_plugin_lanes_alone_in_touch() {
        let (s, track) = studio(AutoMode::Touch);
        let ctx = Ctx::new(s);
        ctx.touch(Some(track), true);
        let mut s = Rc::try_unwrap(ctx.studio).ok().unwrap().into_inner();
        pass(&mut s);
        assert_eq!(lane(&s, track, AutoTarget::Plugin { slot: SLOT, param: 1 }), Some(vec![AutoPoint { pos: 0, value: 0.8 }]));
        assert!(lane(&s, track, AutoTarget::Volume).unwrap().iter().filter(|p| p.pos <= 48000).all(|p| p.value == -3.0));
        // Letting go of a plug-in parameter doesn't let go of the fader.
        s.set_plugin_held(HashMap::from([(Some(track), vec![(SLOT, 2)])]));
        s.set_plugin_held(HashMap::new());
        assert!(s.strip(Some(track)).touched.load(Relaxed));
    }

    #[test]
    fn write_records_every_lane() {
        let (mut s, track) = studio(AutoMode::Write);
        pass(&mut s);
        assert!(lane(&s, track, AutoTarget::Volume).unwrap().iter().filter(|p| p.pos <= 48000).all(|p| p.value == -3.0));
        assert!(lane(&s, track, AutoTarget::Pan).is_some());
        assert!(lane(&s, track, AutoTarget::Plugin { slot: SLOT, param: 1 }).unwrap().iter().all(|p| p.value == 0.3));
    }

    #[test]
    fn latch_goes_on_writing_only_what_was_held() {
        let (mut s, track) = studio(AutoMode::Latch);
        let volume = lane(&s, track, AutoTarget::Volume);
        s.set_plugin_held(HashMap::from([(Some(track), vec![(SLOT, 1)])]));
        s.write_automation(0, |_, _, _| Some(0.3));
        s.set_plugin_held(HashMap::new());
        assert!(!s.strip(Some(track)).touched.load(Relaxed));
        s.write_automation(24000, |_, _, _| Some(0.5));
        assert_eq!(lane(&s, track, AutoTarget::Volume), volume);
        let param = lane(&s, track, AutoTarget::Plugin { slot: SLOT, param: 1 }).unwrap();
        assert_eq!(param.last(), Some(&AutoPoint { pos: 24000, value: 0.5 }));
    }

    #[test]
    fn only_the_first_plugin_edit_announces_itself() {
        let (mut s, _) = studio(AutoMode::Read);
        assert!(!s.plugin_edited(false));
        assert!(s.plugin_edited(true));
        assert!(s.history.is_dirty());
        assert!(!s.plugin_edited(true));
    }

    const ON: [u8; 3] = [0x90, 60, 100];
    const OFF: [u8; 3] = [0x80, 60, 0];

    fn time() -> TimeMap {
        TimeMap::new(48000, 120.0, 4, 4)
    }

    #[test]
    fn midi_passes_group_by_pass_and_keep_order() {
        let events = [(1, 30, ON), (0, 50, ON), (1, 10, OFF), (0, 60, OFF), (3, 5, ON)];
        assert_eq!(
            midi_passes(&events),
            vec![vec![(50, ON), (60, OFF)], vec![(30, ON), (10, OFF)], vec![(5, ON)]]
        );
    }

    #[test]
    fn merge_of_a_note_held_across_the_wrap_does_not_panic() {
        let bar = time().bar_len();
        // Held from late in the first pass until after the wrap back to the cycle start.
        let events = [(0, bar * 2 - 100, ON), (1, 200, OFF), (1, 1000, ON), (1, 2000, OFF)];
        let (start, end, m) = recorded_midi(&time(), &events, None, false).unwrap();
        assert_eq!((start, end), (0, bar * 2));
        assert_eq!(m.notes.len(), 2);
        assert!(!m.is_folder());
        let held = m.notes.iter().find(|n| n.start > PPQ).unwrap();
        assert_eq!(held.start + held.length, time().ticks_between(0, bar * 2) as u32);
    }

    #[test]
    fn cycle_passes_make_a_take_folder_playing_the_last() {
        let bar = time().bar_len();
        let cycle = Range::new(bar, bar * 3);
        let events = [(0, bar + 10, ON), (0, bar + 500, OFF), (1, bar + 20, OFF), (2, bar * 2, ON), (2, bar * 2 + 900, OFF)];
        let (start, end, m) = recorded_midi(&time(), &events, Some(cycle), true).unwrap();
        assert_eq!((start, end), (bar, bar * 3));
        assert_eq!(m.takes.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Take 1", "Take 2"]);
        assert_eq!(m.notes, m.takes[1].notes);
        assert_eq!(m.notes[0].start, time().ticks_between(bar, bar * 2) as u32);
        // A single pass with notes stays a plain region.
        let (_, _, one) = recorded_midi(&time(), &events[..3], Some(cycle), true).unwrap();
        assert!(!one.is_folder() && one.notes.len() == 1);
    }

    #[test]
    fn an_early_downbeat_joins_the_take_folder_at_the_cycle_start() {
        let (bar, beat) = (time().bar_len(), time().beat_len());
        let cycle = Range::new(bar, bar * 3);
        let events = [(0, bar - 200, ON), (0, bar + 500, OFF), (1, bar + 1000, ON), (1, bar + 2000, OFF)];
        let (start, end, m) = recorded_midi(&time(), &events, Some(cycle), true).unwrap();
        assert_eq!((start, end), (bar, bar * 3));
        assert_eq!(m.takes[0].notes[0].start, 0);
        // A lead-in longer than a beat is played on the way into the cycle, so it is kept.
        let events = [(0, bar - beat * 2, ON), (0, bar - beat, OFF), (1, bar + 1000, ON), (1, bar + 2000, OFF)];
        let (start, _, m) = recorded_midi(&time(), &events, Some(cycle), true).unwrap();
        assert_eq!(start, 0);
        assert_eq!(m.takes[0].notes[0].start, time().ticks_between(0, bar - beat * 2) as u32);
    }

    #[test]
    fn capture_merges_every_pass_into_one_region() {
        let bar = time().bar_len();
        let cycle = Range::new(bar, bar * 3);
        let events = [(0, bar + 900, ON), (0, bar + 1000, OFF), (1, bar + 100, ON), (1, bar + 200, OFF), (2, bar * 2, ON)];
        let (start, end, m) = recorded_midi(&time(), &events, Some(cycle), false).unwrap();
        assert_eq!((start, end), (bar, bar * 3));
        assert!(!m.is_folder());
        let starts: Vec<u32> = m.notes.iter().map(|n| n.start).collect();
        let tick = |pos| time().ticks_between(bar, pos) as u32;
        assert_eq!(starts, [tick(bar + 100), tick(bar + 900), tick(bar * 2)]);
    }

    #[test]
    fn edits_write_back_into_midi_takes() {
        let mut p = Project::new("t", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        let note = |start, pitch| Note::new(start, 10, pitch, 100);
        let mut m = MidiContent { notes: vec![note(0, 60)], ..Default::default() };
        m.add_take(MidiTake { name: "Take 2".into(), notes: vec![note(0, 72)] });
        let content = Content::Midi(m);
        p.track_mut(t).unwrap().regions.push(Region { id, name: "m".into(), start: 0, length: 48000, muted: false, loops: 0, color: None, content });
        let dir = std::env::temp_dir().join(format!("solstice-studio-test-{}", std::process::id()));
        let ctx = Ctx::new(Studio::new(p, Bundle::new(dir), Settings::default()));
        ctx.edit("Add Note", |p| {
            if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
                m.notes.push(note(100, 74));
            }
        });
        let s = ctx.s();
        let Some((_, Region { content: Content::Midi(m), .. })) = s.project.find_region(id) else { panic!() };
        assert_eq!(m.takes[1].notes, [note(0, 72), note(100, 74)]);
        assert_eq!(m.takes[0].notes, [note(0, 60)]);
    }

    #[test]
    fn reference_tracks_are_measured_once_per_change() {
        let dir = std::env::temp_dir().join(format!("solstice-reference-jobs-{}", std::process::id()));
        let bundle = Bundle::new(&dir);
        std::fs::create_dir_all(bundle.audio_dir()).unwrap();
        solstice_engine::audio::write_wav(&bundle.audio_path("ref.wav"), 48000, &[vec![0.5; 4800]]).unwrap();
        let mut p = Project::new("T", 48000);
        let id = p.add_track(TrackKind::Audio);
        let t = p.track_mut(id).unwrap();
        t.reference = true;
        t.regions.push(Region { id: 50, name: "r".into(), start: 0, length: 4800, muted: false, loops: 0, color: None, content: Content::Audio(AudioContent::new("ref.wav")) });
        let saved = p.clone();
        let mut s = Studio::new(p, Bundle::new(&dir), Settings::default());

        let jobs = s.take_reference_jobs();
        assert_eq!(jobs.len(), 1);
        assert!(s.take_reference_jobs().is_empty(), "one measurement at a time");
        let segs = jobs[0].1.clone();
        s.set_reference_lufs(id, &segs, Some(-9.0));
        assert_eq!(s.project.track(id).unwrap().reference_lufs, Some(-9.0));

        s.project.track_mut(id).unwrap().reference_lufs = None;
        assert!(s.take_reference_jobs().is_empty());
        assert_eq!(s.project.track(id).unwrap().reference_lufs, Some(-9.0), "undo past the result keeps it");

        if let Content::Audio(a) = &mut s.project.track_mut(id).unwrap().regions[0].content {
            a.gain_db = -6.0;
        }
        assert_eq!(s.take_reference_jobs().len(), 1);
        s.set_reference_lufs(id, &segs, Some(-1.0));
        assert_eq!(s.project.track(id).unwrap().reference_lufs, Some(-9.0), "a result for old audio is dropped");

        let mut p = saved;
        p.track_mut(id).unwrap().reference_lufs = Some(-8.0);
        let mut s = Studio::new(p, bundle, Settings::default());
        assert!(s.take_reference_jobs().is_empty(), "a saved value is trusted");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn level_matching_makes_up_for_the_fader_and_ends_with_the_reference() {
        let (mut s, track) = studio(AutoMode::Read);
        let t = s.project.track_mut(track).unwrap();
        t.reference = true;
        t.reference_lufs = Some(-8.0);
        let mut meter = crate::master_meter::MasterMeter::new(48000);
        let a = 10f32.powf(-14.0 / 20.0);
        let sine: Vec<f32> = (0..96000).map(|n| a * (std::f32::consts::TAU * 1000.0 * n as f32 / 48000.0).sin()).collect();
        meter.feed(&sine, &sine);
        s.master_meter = Some(meter);
        let strip = s.strips.entry(Some(track)).or_default().clone();
        let settle = |s: &mut Studio| (0..200).for_each(|_| s.run_reference());
        settle(&mut s);
        // −14 LUFS mix, −8 LUFS reference behind a −3 dB fader.
        assert!((strip.match_db.get() + 3.0).abs() < 0.05, "{}", strip.match_db.get());

        s.project.track_mut(track).unwrap().reference = false;
        s.run_reference();
        assert_eq!(strip.match_db.get(), 0.0, "a track that stops being a reference rejoins the mix at once");

        s.project.track_mut(track).unwrap().reference = true;
        settle(&mut s);
        s.reference_on = true;
        let next = s.project.clone();
        s.replace_project(next, Bundle::new(std::env::temp_dir().join("solstice-reference-switch-unused")));
        assert!(!s.reference_on);
        assert_eq!(strip.match_db.get(), 0.0);
        assert!(!s.master_meter.as_ref().unwrap().loudness.integrated().is_finite(), "the last project's loudness is gone");
    }
}
