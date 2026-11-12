use crate::peaks::Peaks;
use crate::settings::Settings;
use gtk::glib;
use rtrb::{Producer, RingBuffer};
use solstice_backend_jack::{Backend, INPUTS};
use solstice_engine::audio::AudioData;
use solstice_engine::recorder::{self, Control, Recorder};
use solstice_engine::{BOUNCE_TRACK, Command, Handle, Options, PluginCell, Resources, Strip};
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
}

pub struct Loaded {
    pub inst: crate::host::Host,
    pub cell: Arc<PluginCell>,
    /// The slot state this instance last took or gave. When undo puts a different one in the
    /// project, the instance is given it.
    pub applied: Vec<u8>,
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
    bounce_ring: RefCell<Option<Producer<f32>>>,
    /// The bounce running now, and those waiting after it.
    pub bounce_job: Option<BounceJob>,
    bounce_queue: std::collections::VecDeque<BounceJob>,
    pub midi_take: Vec<(u64, [u8; 3])>,
    /// Regions recorded since recording last started, so further cycle passes fold into them.
    rec_regions: Vec<Id>,
    /// Notes played during the last pass that was not recording, for Capture as Recording.
    capture: Vec<(u64, [u8; 3])>,
    capture_stale: bool,
    pub live_in: Vec<[u8; 3]>,
    was_recording: bool,
    was_playing: bool,
    /// Automation writing state for the current pass: channels latched on, where each last
    /// wrote, and the project before the pass for a single undo step.
    auto_latched: HashSet<Option<Id>>,
    auto_last: HashMap<Option<Id>, u64>,
    auto_snapshot: Option<Project>,
    /// Channels touched because a parameter is held in a plug-in's own interface.
    plugin_touch: HashSet<Option<Id>>,
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
    /// "Copy Channel Strip": the channel plus each plug-in's state at the time of copying.
    pub strip_clipboard: Option<Channel>,
    pub browse_to: Option<PathBuf>,
    pub message: Option<String>,
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

    /// An undoable edit. Rebuilds the engine graph and notifies views.
    pub fn edit(&self, label: &str, f: impl FnOnce(&mut Project)) {
        {
            let mut s = self.m();
            let s = &mut *s;
            let mut before = s.project.clone();
            f(&mut s.project);
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

    pub fn redo(&self) {
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
        let mut s = self.m();
        if s.gesture.is_none() {
            s.gesture = Some(s.project.clone());
        }
    }

    /// Applies a change during a gesture: the engine follows, views are left alone so the
    /// widget being dragged survives.
    pub fn live(&self, rebuild: bool, f: impl FnOnce(&mut Project)) {
        let mut s = self.m();
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
            let Some(before) = s.gesture.take() else { return };
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

    /// Logic's stop: a second press while stopped returns to the start.
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
        self.m().strip(id).touched.store(on, Relaxed);
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
                let tenth = s.project.sample_rate as u64 / 10;
                for id in s.channel_ids() {
                    let held = s.plugin_held.get(&id).cloned().unwrap_or_default();
                    let touched = s.strip(id).touched.load(Relaxed) || !held.is_empty();
                    let Some(ch) = s.project.channel(id) else { continue };
                    let mode = ch.auto_mode;
                    if mode == AutoMode::Latch && touched {
                        s.auto_latched.insert(id);
                    }
                    let writing = match mode {
                        AutoMode::Write => true,
                        AutoMode::Touch => touched,
                        AutoMode::Latch => touched || s.auto_latched.contains(&id),
                        _ => false,
                    };
                    if !writing {
                        s.auto_last.remove(&id);
                        continue;
                    }
                    if s.auto_snapshot.is_none() {
                        s.auto_snapshot = Some(s.project.clone());
                    }
                    // A cycle wrap jumps back; start a fresh stretch there.
                    let from = s.auto_last.get(&id).copied().filter(|f| *f <= pos).unwrap_or(pos);
                    let mut values = vec![(AutoTarget::Volume, ch.volume_db.max(-60.0)), (AutoTarget::Pan, ch.pan)];
                    if ch.mute || ch.lane(AutoTarget::Mute).is_some() {
                        values.push((AutoTarget::Mute, if ch.mute { 1.0 } else { 0.0 }));
                    }
                    for (i, send) in ch.sends.iter().enumerate() {
                        if ch.lane(AutoTarget::Send(i)).is_some() {
                            values.push((AutoTarget::Send(i), send.level_db));
                        }
                    }
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
                    for (slot, param) in params {
                        if let Some(l) = s.plugins.get(&slot) {
                            values.push((AutoTarget::Plugin { slot, param }, l.inst.param(param) as f32));
                        }
                    }
                    let ch = s.project.channel_mut(id).unwrap();
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
                    s.auto_last.insert(id, pos);
                }
                false
            }
        };
        if finished {
            self.emit(Change::Project);
        }
    }

    /// Main-thread housekeeping, run every frame: plug-in UIs, finished takes, MIDI recording.
    pub fn poll(&self) {
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
            let s = &mut *s;
            let channels: HashSet<Option<Id>> = held.keys().copied().collect();
            for (ch, on) in plugin_touches(&channels, &mut s.plugin_touch) {
                s.strip(ch).touched.store(on, Relaxed);
            }
            s.plugin_held = held;
            let first = edited && !s.history.is_dirty();
            if edited {
                s.history.mark_dirty();
            }
            first
        };
        if first_edit {
            self.emit(Change::Project);
        }
        self.record_automation();
        let mut changed = false;
        let mut bounce_done = None;
        let live;
        {
            let mut s = self.m();
            let s = &mut *s;
            let Some(io) = s.io.as_mut() else { return };
            io.handle.collect();
            let recording = io.handle.shared.recording.load(Relaxed);
            s.live_in.clear();
            while let Ok(e) = io.handle.live_notes.pop() {
                if e.recording {
                    s.midi_take.push((e.pos, e.data));
                    continue;
                }
                if e.playing {
                    if std::mem::take(&mut s.capture_stale) {
                        s.capture.clear();
                    }
                    s.capture.push((e.pos, e.data));
                }
                s.live_in.push(e.data);
            }
            if !io.handle.shared.playing.load(Relaxed) {
                s.capture_stale = true;
            }
            live = !s.live_in.is_empty();
            let takes: Vec<_> = io.recorder.takes.try_iter().collect();
            for take in takes {
                if take.track == BOUNCE_TRACK {
                    bounce_done = Some(take);
                    continue;
                }
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
        if let Some(take) = bounce_done {
            self.finish_bounce(take);
        }
        if changed {
            self.emit(Change::Project);
        }
        if live {
            self.emit(Change::LiveNote);
        }
    }

    /// Logic's Capture as Recording: turns what was played in the last pass into a region.
    pub fn capture_recording(&self) -> bool {
        let made = {
            let mut s = self.m();
            let s = &mut *s;
            let events = s.capture.clone();
            let before = s.project.clone();
            let made = s.midi_region(events);
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

    fn finish_bounce(&self, take: recorder::Take) {
        let Some(job) = self.m().bounce_job.take() else { return };
        match job.target {
            BounceTarget::File(target) => {
                let src = self.s().bundle.audio_path(&take.file);
                let result = std::fs::rename(&src, &target).or_else(|_| std::fs::copy(&src, &target).map(|_| ()));
                if let Err(e) = result {
                    self.message(&format!("Could not write {}: {e}", target.display()));
                }
            }
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
                        length: take.frames,
                        muted: false,
                        loops: 0,
                        content: Content::Audio(AudioContent::new(take.file.clone())),
                    });
                    p.tracks.insert(i + 1, t);
                    for r in p.tracks[i].regions.iter_mut().filter(|r| regions.contains(&r.id)) {
                        r.muted = true;
                    }
                });
            }
        }
        if !self.start_next_bounce() {
            self.m().rebuild();
            self.message("Bounce finished");
        }
    }

    /// Bounces the whole project to a file, in real time.
    pub fn bounce(&self, target: PathBuf) {
        let end = self.s().project.length();
        if end > 0 {
            self.queue_bounce(BounceJob { track: None, start: 0, end, target: BounceTarget::File(target) });
        }
    }

    /// Logic's Export All Tracks as Audio Files: each track on its own, from the start.
    pub fn export_tracks(&self, dir: &Path) -> usize {
        let jobs: Vec<BounceJob> = {
            let p = &self.s().project;
            let end = p.length();
            p.tracks
                .iter()
                .filter(|t| !t.regions.is_empty())
                .map(|t| {
                    let file = format!("{}.wav", t.name.replace(['/', '\\'], "-"));
                    BounceJob { track: Some(t.id), start: 0, end, target: BounceTarget::File(dir.join(file)) }
                })
                .collect()
        };
        let n = jobs.len();
        for j in jobs {
            self.queue_bounce(j);
        }
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
            let end = chosen.iter().map(|r| r.span_end()).max().unwrap_or(0) + s.project.sample_rate as u64;
            BounceJob {
                track: Some(t.id),
                start,
                end,
                target: BounceTarget::InPlace { regions: chosen.iter().map(|r| r.id).collect() },
            }
        };
        self.queue_bounce(job);
        true
    }

    fn queue_bounce(&self, job: BounceJob) {
        self.m().bounce_queue.push_back(job);
        if self.s().bounce_job.is_none() {
            self.start_next_bounce();
        }
    }

    fn start_next_bounce(&self) -> bool {
        let Some(job) = self.m().bounce_queue.pop_front() else { return false };
        let (start, end) = (job.start, job.end);
        {
            let mut s = self.m();
            s.bounce_job = Some(job);
            if s.registered.insert(BOUNCE_TRACK) {
                let sr = s.project.sample_rate as usize;
                let (p, c) = RingBuffer::new(sr * 2 * 4);
                if let Some(io) = &s.io {
                    let _ = io.recorder.control.send(Control::Register(BOUNCE_TRACK, c));
                }
                *s.bounce_ring.borrow_mut() = Some(p);
            }
            s.rebuild();
        }
        self.send(Command::Bounce { start, end });
        true
    }
}

/// One real-time bounce: which track to play on its own (all of them for `None`), the span,
/// and where the result goes.
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
            bounce_ring: RefCell::default(),
            bounce_job: None,
            bounce_queue: Default::default(),
            midi_take: Vec::new(),
            rec_regions: Vec::new(),
            capture: Vec::new(),
            capture_stale: false,
            live_in: Vec::new(),
            was_recording: false,
            was_playing: false,
            auto_latched: HashSet::new(),
            auto_last: HashMap::new(),
            auto_snapshot: None,
            plugin_touch: HashSet::new(),
            plugin_held: HashMap::new(),
            auto_view: HashMap::new(),
            peaks_pending: HashSet::new(),
            peaks_inflight: HashSet::new(),
            meters: HashMap::new(),
            selection: Vec::new(),
            gesture: None,
            strip_clipboard: None,
            browse_to: None,
            message: None,
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
                self.io = Some(AudioIo { backend, handle, recorder });
                self.io_error = None;
                self.registered.clear();
                self.sync();
            }
            Err(e) => self.io_error = Some(format!("Could not connect to the audio server: {e}")),
        }
    }

    /// Hands the audio server back, as when another project window comes to the front.
    pub fn stop_audio(&mut self) {
        if let Some(io) = self.io.as_mut() {
            io.handle.send(Command::Stop);
        }
        self.io = None;
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
        self.fix_selection();
        self.sync_plugins();
        self.sync_rings();
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
            isolate: self.bounce_job.as_ref().and_then(|j| j.track),
        };
        for id in self.channel_ids() {
            self.push_strip(id);
        }
        if self.io.is_none() {
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
        let ahead = self.sample_rate() as u64 * 5;
        let files: Vec<(String, u64, u64)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.regions.iter())
            .filter(|r| r.end() > pos && r.start < pos + ahead)
            .filter_map(|r| match &r.content {
                Content::Audio(a) => Some((a.file.clone(), a.offset + pos.saturating_sub(r.start), ahead)),
                _ => None,
            })
            .collect();
        for (f, frame, len) in files {
            if let Some(a) = self.audio(&f) {
                a.prefetch(frame, len);
            }
        }
    }

    fn finish_midi_take(&mut self) -> bool {
        let events = std::mem::take(&mut self.midi_take);
        self.midi_region(events)
    }

    /// Adds a MIDI region on the selected (or armed) instrument track from live events.
    fn midi_region(&mut self, events: Vec<(u64, [u8; 3])>) -> bool {
        let Some(track) = self
            .selected_track
            .filter(|id| self.project.track(*id).is_some_and(|t| t.kind == TrackKind::Instrument))
            .or_else(|| self.project.tracks.iter().find(|t| t.armed && t.kind == TrackKind::Instrument).map(|t| t.id))
        else {
            return false;
        };
        let time = self.project.time();
        let bar = time.bar_len();
        let first = events.iter().map(|e| e.0).min().unwrap_or(0);
        let last = events.iter().map(|e| e.0).max().unwrap_or(0);
        let start = first / bar * bar;
        let end = (last / bar + 1) * bar;
        let mut open: HashMap<u8, (u64, u8)> = HashMap::new();
        let mut notes = Vec::new();
        let tick = |pos: u64| time.ticks_between(start, pos) as u32;
        for (pos, d) in events {
            let on = d[0] & 0xf0 == 0x90 && d[2] > 0;
            let off = d[0] & 0xf0 == 0x80 || (d[0] & 0xf0 == 0x90 && d[2] == 0);
            if on {
                open.insert(d[1], (pos, d[2]));
            } else if off && let Some((s, v)) = open.remove(&d[1]) {
                notes.push(Note { start: tick(s), length: (tick(pos) - tick(s)).max(1), pitch: d[1], velocity: v });
            }
        }
        for (pitch, (s, v)) in open {
            notes.push(Note { start: tick(s), length: tick(last.max(s + 1)) - tick(s), pitch, velocity: v });
        }
        if notes.is_empty() {
            return false;
        }
        notes.sort_by_key(|n| (n.start, n.pitch));
        let id = self.project.alloc_id();
        let t = self.project.track_mut(track).unwrap();
        let n = t.regions.len() + 1;
        let name = format!("{} #{n}", t.name);
        t.regions.push(Region {
            id,
            name,
            start,
            length: end - start,
            muted: false,
            loops: 0,
            content: Content::Midi(MidiContent { notes, ..Default::default() }),
        });
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
    /// Enabled plug-ins. A CLAP plug-in that is also installed as VST3 is listed once, as the
    /// VST3, which can show its own interface here.
    pub fn available_plugins(&self) -> Vec<solstice_vst3::ClassInfo> {
        let maker = |c: &solstice_vst3::ClassInfo| c.vendor.split_whitespace().next().unwrap_or("").to_lowercase();
        let vst3: HashSet<(String, String)> = self
            .catalog
            .iter()
            .filter(|c| !c.uid.starts_with(solstice_clap::PREFIX) && !c.uid.starts_with(solstice_dsp::PREFIX))
            .map(|c| (c.name.clone(), maker(c)))
            .collect();
        self.catalog
            .iter()
            .filter(|c| !self.settings.disabled_plugins.contains(&c.uid))
            .filter(|c| !c.uid.starts_with(solstice_clap::PREFIX) || !vst3.contains(&(c.name.clone(), maker(c))))
            .cloned()
            .collect()
    }

    /// Saves to a new bundle, copying the audio files across.
    pub fn save_as(&mut self, dir: &Path) -> std::io::Result<()> {
        let new = Bundle::new(dir);
        std::fs::create_dir_all(new.audio_dir())?;
        if let Ok(rd) = std::fs::read_dir(self.bundle.audio_dir()) {
            for e in rd.flatten() {
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
        self.project = project;
        self.bundle = bundle;
        self.reset_session();
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

    fn bounce_ring(&self) -> Option<Producer<f32>> {
        self.bounce_ring.borrow_mut().take()
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

/// Touches channels that newly have a parameter held in a plug-in's interface and releases
/// those that no longer do, as (channel, touched) changes. `touching` is kept between calls.
fn plugin_touches(held: &HashSet<Option<Id>>, touching: &mut HashSet<Option<Id>>) -> Vec<(Option<Id>, bool)> {
    let mut out: Vec<(Option<Id>, bool)> = held.iter().filter(|c| touching.insert(**c)).map(|c| (*c, true)).collect();
    let released: Vec<Option<Id>> = touching.iter().filter(|c| !held.contains(c)).copied().collect();
    for c in released {
        touching.remove(&c);
        out.push((c, false));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_plugin_parameters_touch_their_channel() {
        let strips: HashMap<Option<Id>, Strip> = [None, Some(1), Some(2)].into_iter().map(|c| (c, Strip::default())).collect();
        let mut touching = HashSet::new();
        let mut step = |held: &[Option<Id>]| {
            for (ch, on) in plugin_touches(&held.iter().copied().collect(), &mut touching) {
                strips[&ch].touched.store(on, Relaxed);
            }
            strips.iter().filter(|(_, s)| s.touched.load(Relaxed)).map(|(c, _)| *c).collect::<HashSet<_>>()
        };
        assert_eq!(step(&[Some(1)]), HashSet::from([Some(1)]));
        assert_eq!(step(&[Some(1), None]), HashSet::from([Some(1), None]));
        assert_eq!(step(&[None]), HashSet::from([None]));
        assert_eq!(step(&[]), HashSet::new());
        // A fader held on its own is left alone.
        strips[&Some(2)].touched.store(true, Relaxed);
        assert_eq!(step(&[]), HashSet::from([Some(2)]));
    }
}
