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
    pub bounce_target: Option<PathBuf>,
    pub midi_take: Vec<(u64, [u8; 3])>,
    was_recording: bool,
    was_playing: bool,
    /// Automation writing state for the current pass: channels latched on, where each last
    /// wrote, and the project before the pass for a single undo step.
    auto_latched: HashSet<Option<Id>>,
    auto_last: HashMap<Option<Id>, u64>,
    auto_snapshot: Option<Project>,
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
            s.history.push(label, &s.project);
            f(&mut s.project);
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
        self.send(Command::Record(on));
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
                    let touched = s.strip(id).touched.load(Relaxed);
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
        self.record_automation();
        solstice_vst3::idle();
        let mut changed = false;
        let mut bounce_done = None;
        {
            let mut s = self.m();
            let s = &mut *s;
            let Some(io) = s.io.as_mut() else { return };
            io.handle.collect();
            let recording = io.handle.shared.recording.load(Relaxed);
            while let Ok(e) = io.handle.midi_rec.pop() {
                s.midi_take.push(e);
            }
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
                let id = s.project.alloc_id();
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
    }

    fn finish_bounce(&self, take: recorder::Take) {
        let s = self.s();
        let Some(target) = s.bounce_target.clone() else { return };
        let src = s.bundle.audio_path(&take.file);
        drop(s);
        let result = std::fs::rename(&src, &target).or_else(|_| std::fs::copy(&src, &target).map(|_| ()));
        if let Err(e) = result {
            eprintln!("solstice: bounce: {e}");
        }
        self.m().bounce_target = None;
    }

    pub fn bounce(&self, target: PathBuf) {
        let end = self.s().project.length();
        if end == 0 {
            return;
        }
        {
            let mut s = self.m();
            s.bounce_target = Some(target);
            if s.registered.insert(BOUNCE_TRACK) {
                let sr = s.project.sample_rate as usize;
                let (p, c) = RingBuffer::new(sr * 2 * 4);
                if let Some(io) = &s.io {
                    let _ = io.recorder.control.send(Control::Register(BOUNCE_TRACK, c));
                }
                *s.bounce_ring.borrow_mut() = Some(p);
                s.rebuild();
            }
        }
        self.send(Command::Bounce { start: 0, end });
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
            bounce_ring: RefCell::default(),
            bounce_target: None,
            midi_take: Vec::new(),
            was_recording: false,
            was_playing: false,
            auto_latched: HashSet::new(),
            auto_last: HashMap::new(),
            auto_snapshot: None,
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
        let result = Backend::start("Solstice", |sr| {
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
            if let Some(l) = self.plugins.get(&slot.id) {
                l.cell.bypass.store(slot.bypass, Relaxed);
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
                    self.plugins.insert(slot.id, Loaded { inst, cell });
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
        let audio_empty = std::fs::read_dir(self.bundle.audio_dir()).map_or(true, |mut d| d.next().is_none());
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
        let Some(track) = self
            .selected_track
            .filter(|id| self.project.track(*id).is_some_and(|t| t.kind == TrackKind::Instrument))
            .or_else(|| self.project.tracks.iter().find(|t| t.armed && t.kind == TrackKind::Instrument).map(|t| t.id))
        else {
            return false;
        };
        let events = std::mem::take(&mut self.midi_take);
        let time = self.project.time();
        let bar = time.bar_len();
        let first = events.iter().map(|e| e.0).min().unwrap_or(0);
        let last = events.iter().map(|e| e.0).max().unwrap_or(0);
        let start = first / bar * bar;
        let end = (last / bar + 1) * bar;
        let mut open: HashMap<u8, (u64, u8)> = HashMap::new();
        let mut notes = Vec::new();
        let tick = |pos: u64| time.samples_to_ticks(pos - start) as u32;
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
        let p = &mut self.project;
        let mut apply = |slot: &mut PluginSlot| {
            if let Some(s) = states.get(&slot.id) {
                slot.state = s.clone();
            }
        };
        p.master.inserts.iter_mut().for_each(&mut apply);
        for t in &mut p.tracks {
            t.channel.inserts.iter_mut().for_each(&mut apply);
            t.instrument.iter_mut().for_each(&mut apply);
        }
        for b in &mut p.buses {
            b.channel.inserts.iter_mut().for_each(&mut apply);
        }
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        self.capture_plugin_states();
        self.bundle.save(&self.project)?;
        self.history.mark_saved();
        self.remember_recent();
        Ok(())
    }

    fn remember_recent(&mut self) {
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
    pub fn available_plugins(&self) -> Vec<solstice_vst3::ClassInfo> {
        self.catalog.iter().filter(|c| !self.settings.disabled_plugins.contains(&c.uid)).cloned().collect()
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
        self.project = project;
        self.bundle = bundle;
        self.reset_session();
        self.remember_recent();
        Ok(())
    }

    pub fn new_project(&mut self) {
        let (project, bundle) = untitled(self.sample_rate());
        self.project = project;
        self.bundle = bundle;
        self.reset_session();
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

/// A fresh project in its own bundle, so recordings have somewhere to go before the first save.
pub fn untitled(sample_rate: u32) -> (Project, Bundle) {
    let base = projects_dir();
    let mut n = 1;
    let dir = loop {
        let d = base.join(format!("Untitled {n}.solstice"));
        if !d.exists() {
            break d;
        }
        n += 1;
    };
    let _ = std::fs::create_dir_all(dir.join("Audio Files"));
    let mut p = Project::new(&format!("Untitled {n}"), sample_rate);
    p.add_track(TrackKind::Audio);
    p.add_track(TrackKind::Instrument);
    (p, Bundle::new(dir))
}
