use crate::canvas::{self, Canvas, accent, rect, rgba, rounded, track_color};
use crate::context_menu::Menu;
use crate::drum_patterns::{STARTERS, Starter, genres};
use crate::drum_rows::*;
use crate::studio::{Change, Ctx};
use gtk::prelude::*;
use gtk::{gdk, glib};
use solstice_model::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

const CELL_W: f32 = 24.0;
const RATES: [(u32, &str); 5] = [(PPQ / 2, "1/8"), (PPQ / 3, "1/8T"), (PPQ / 4, "1/16"), (PPQ / 6, "1/16T"), (PPQ / 8, "1/32")];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    On,
    Velocity,
    Gate,
    Repeat,
    Chance,
    Offset,
    Tie,
}

const MODES: [(Mode, &str, &str); 7] = [
    (Mode::On, "Steps", "Switch steps on and off (1)"),
    (Mode::Velocity, "Velocity", "Drag a step up or down to set how hard it plays (2)"),
    (Mode::Gate, "Gate", "Drag a step up or down to set how long it lasts (3)"),
    (Mode::Repeat, "Repeat", "Drag a step up or down to play it several times within the step (4)"),
    (Mode::Chance, "Chance", "Drag a step up or down to set how likely it is to play; each repeat is rolled on its own (5)"),
    (Mode::Offset, "Offset", "Drag a step up or down to move it off the grid (6)"),
    (Mode::Tie, "Tie", "Click a step to hold the note before it through it (7)"),
];

impl Mode {
    /// The lowest and highest value, and how far a key press nudges it.
    fn range(self) -> Option<(i32, i32, i32)> {
        match self {
            Mode::Velocity => Some((1, 127, 8)),
            Mode::Gate | Mode::Chance => Some((1, 100, 10)),
            Mode::Repeat => Some((1, 8, 1)),
            Mode::Offset => Some((-50, 50, 5)),
            Mode::On | Mode::Tie => None,
        }
    }

    /// The mode whose value nudges and scrolls change: modes without one change velocity.
    fn valued(self) -> Mode {
        if self.range().is_some() { self } else { Mode::Velocity }
    }

    fn get(self, s: &Step) -> i32 {
        match self {
            Mode::Gate => s.gate as i32,
            Mode::Repeat => s.repeat as i32,
            Mode::Chance => s.chance as i32,
            Mode::Offset => s.offset as i32,
            Mode::Tie => s.tie as i32,
            Mode::On | Mode::Velocity => s.velocity as i32,
        }
    }

    fn set(self, s: &mut Step, v: i32) {
        let (lo, hi, _) = self.valued().range().unwrap_or((1, 127, 1));
        let v = v.clamp(lo, hi);
        match self.valued() {
            Mode::Gate => s.gate = v as u8,
            Mode::Repeat => s.repeat = v as u8,
            Mode::Chance => s.chance = v as u8,
            Mode::Offset => s.offset = v as i8,
            _ => s.velocity = v as u8,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Mode::On => "Edit Steps",
            Mode::Velocity => "Step Velocity",
            Mode::Gate => "Step Gate",
            Mode::Repeat => "Step Repeat",
            Mode::Chance => "Step Chance",
            Mode::Offset => "Step Offset",
            Mode::Tie => "Tie Steps",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Paint {
    /// Switching steps on or off, or ties on or off, across the cells dragged over.
    Steps(bool),
    Tie(bool),
    /// Dragging one step's value: where the drag started and the value last set.
    Value { pitch: u8, index: u16, from: i32, last: i32 },
}

/// The most steps a pattern or row can loop over.
const MAX_STEPS: f64 = u16::MAX as f64 + 1.0;

struct Steps {
    mode: Cell<Mode>,
    /// The step rate shown for a region that has no pattern yet.
    rate: Cell<u32>,
    /// The focused row (an index into the shown rows) and step.
    focus: Cell<(usize, u32)>,
    paint: Cell<Option<Paint>>,
    visited: RefCell<HashSet<(u8, u16)>>,
    /// The last press on the grid: event time, button, cell, and whether it removed a step.
    last_press: Cell<Option<(u32, u32, u8, u16, bool)>>,
    follow: Cell<bool>,
    used_only: Cell<bool>,
    row_clip: RefCell<Option<PatternRow>>,
    clip: RefCell<Option<Pattern>>,
    preview: RefCell<Option<Preview>>,
}

impl Steps {
    fn new() -> Steps {
        Steps {
            mode: Cell::default(),
            rate: Cell::new(PPQ / 4),
            focus: Cell::default(),
            paint: Cell::new(None),
            visited: RefCell::default(),
            last_press: Cell::new(None),
            follow: Cell::new(false),
            used_only: Cell::new(false),
            row_clip: RefCell::default(),
            clip: RefCell::default(),
            preview: RefCell::default(),
        }
    }
}

/// A plain region read as a pattern, kept while its notes stay the same.
struct Preview {
    id: Id,
    notes: Vec<Note>,
    rate: u32,
    span: u32,
    pattern: Pattern,
    moved: usize,
}

/// What the editor shows for the selected region.
struct View {
    id: Id,
    start: u64,
    length: u64,
    end: u64,
    color: u8,
    pattern: Pattern,
    /// None when the region holds a pattern, else how many notes converting would move.
    moved: Option<usize>,
    folder: bool,
    rows: Vec<DrumRow>,
}

impl View {
    /// Whether `change` would take an edit.
    fn editable(&self) -> bool {
        !self.folder && self.moved.is_none_or(|n| n == 0)
    }

    fn row(&self, pitch: u8) -> Option<&PatternRow> {
        self.pattern.rows.iter().find(|r| r.pitch == pitch)
    }

    fn step(&self, pitch: u8, index: u16) -> Option<Step> {
        self.row(pitch)?.step(index).copied()
    }

    fn used(&self) -> Vec<u8> {
        used(&self.pattern)
    }
}

fn rate_name(rate: u32) -> String {
    RATES.iter().find(|r| r.0 == rate).map_or_else(|| format!("{rate} ticks"), |r| r.1.to_string())
}

fn used(p: &Pattern) -> Vec<u8> {
    p.rows.iter().filter(|r| !r.steps.is_empty()).map(|r| r.pitch).collect()
}

fn span_of(time: &TimeMap, start: u64, end: u64) -> u32 {
    time.ticks_between(start, end).min(u32::MAX as u64) as u32
}

fn timing(p: &Pattern, pitch: u8) -> (u32, u32) {
    let row = p.rows.iter().find(|r| r.pitch == pitch);
    (row.and_then(|r| r.rate).unwrap_or(p.rate).max(1), row.and_then(|r| r.length).unwrap_or(p.length).max(1))
}

/// Ticks the grid spans: the longest loop of any row.
fn grid_ticks(p: &Pattern) -> u64 {
    p.rows
        .iter()
        .map(|r| timing(p, r.pitch))
        .chain([(p.rate.max(1), p.length.max(1))])
        .map(|(rate, len)| rate as u64 * len as u64)
        .max()
        .unwrap_or(1)
}

/// The x and width of step `i` in the row on `pitch`. One pattern step is `CELL_W` wide.
fn step_x(p: &Pattern, pitch: u8, i: u32) -> (f32, f32) {
    let w = CELL_W * timing(p, pitch).0 as f32 / p.rate.max(1) as f32;
    (i as f32 * w, w)
}

/// The step at `x` in the row on `pitch`, if within the row's length.
fn step_at(p: &Pattern, pitch: u8, x: f64) -> Option<u32> {
    let (rate, len) = timing(p, pitch);
    if x < 0.0 {
        return None;
    }
    let i = (x * p.rate.max(1) as f64 / (CELL_W as f64 * rate as f64)) as u32;
    (i < len && i <= u16::MAX as u32).then_some(i)
}

/// The step of the row on `pitch` that plays `tick` ticks into the region.
fn row_step(p: &Pattern, pitch: u8, tick: u64) -> u32 {
    let (rate, len) = timing(p, pitch);
    (tick / rate as u64 % len as u64) as u32
}

fn row_mut(p: &mut Pattern, pitch: u8) -> &mut PatternRow {
    let i = match p.rows.iter().position(|r| r.pitch == pitch) {
        Some(i) => i,
        None => {
            p.rows.push(PatternRow::new(pitch));
            p.rows.len() - 1
        }
    };
    &mut p.rows[i]
}

/// Switches a step, or in Tie mode its tie, on or off.
fn paint(p: &mut Pattern, pitch: u8, index: u16, mode: Mode, on: bool) {
    let row = row_mut(p, pitch);
    match (mode, row.step(index).copied()) {
        (Mode::Tie, Some(s)) => row.set(Step { tie: on, ..s }),
        (Mode::Tie, None) if on => row.set(Step { tie: true, ..Step::new(index, 100) }),
        (_, None) if on => row.set(Step::new(index, 100)),
        (_, Some(_)) if !on => row.remove(index),
        _ => {}
    }
}

fn set_value(p: &mut Pattern, pitch: u8, index: u16, mode: Mode, v: i32) {
    let row = row_mut(p, pitch);
    if let Some(mut s) = row.step(index).copied() {
        mode.set(&mut s, v);
        row.set(s);
    }
}

/// Runs `f` on region `id`'s pattern and bakes the result. A plain region is converted first,
/// at `rate`, when that moves no notes; otherwise nothing changes and this returns false. `f`
/// can return a new region length in ticks.
fn change(p: &mut Project, id: Id, rate: u32, f: impl FnOnce(&mut Pattern) -> Option<u32>) -> bool {
    let time = p.time();
    let Some(r) = p.region_mut(id) else { return false };
    let Content::Midi(m) = &r.content else { return false };
    if m.is_folder() {
        return false;
    }
    let mut pat = match &m.pattern {
        Some(x) => x.clone(),
        None => match Pattern::from_notes(&m.notes, rate, span_of(&time, r.start, r.end())) {
            (x, 0) => x,
            _ => return false,
        },
    };
    let resize = f(&mut pat);
    pat.rows.retain(|r| !r.steps.is_empty() || r.rate.is_some() || r.length.is_some() || r.swing.is_some());
    if let Some(ticks) = resize {
        r.length = time.at(r.start, ticks as u64).saturating_sub(r.start).max(1);
    }
    let span = span_of(&time, r.start, r.end());
    if let Content::Midi(m) = &mut r.content {
        m.set_pattern(pat, span);
    }
    true
}

/// Makes region `id`'s notes a pattern of `rate`-tick steps, moving any that sit off the grid.
fn convert(p: &mut Project, id: Id, rate: u32) -> bool {
    let time = p.time();
    if !holds_pattern(p, id) {
        return false;
    }
    let Some(r) = p.region_mut(id) else { return false };
    let span = span_of(&time, r.start, r.end());
    let Content::Midi(m) = &mut r.content else { return false };
    let (pattern, _) = Pattern::from_notes(&m.notes, rate, span);
    m.set_pattern(pattern, span);
    true
}

fn view(ctx: &Ctx, st: &Steps) -> Option<View> {
    let s = ctx.s();
    let id = s.selected_region?;
    let (ti, r) = s.project.find_region(id)?;
    let Content::Midi(m) = &r.content else { return None };
    let span = span_of(&s.project.time(), r.start, r.end());
    let (pattern, moved) = match &m.pattern {
        Some(p) => (p.clone(), None),
        None => {
            let rate = st.rate.get();
            let mut cache = st.preview.borrow_mut();
            let fresh = cache.as_ref().is_some_and(|c| c.id == id && c.rate == rate && c.span == span && c.notes == m.notes);
            if !fresh {
                let (pattern, moved) = Pattern::from_notes(&m.notes, rate, span);
                *cache = Some(Preview { id, notes: m.notes.clone(), rate, span, pattern, moved });
            }
            let c = cache.as_ref()?;
            (c.pattern.clone(), Some(c.moved))
        }
    };
    let t = &s.project.tracks[ti];
    let used = used(&pattern);
    let mut rows = all_rows(&t.drum_map, &defaults(t), &used);
    rows.retain(|r| !r.hidden);
    if st.used_only.get() && rows.iter().any(|r| used.contains(&r.pitch)) {
        rows.retain(|r| used.contains(&r.pitch));
    }
    Some(View { id, start: r.start, length: r.length, end: r.span_end(), color: t.color, pattern, moved, folder: m.is_folder(), rows })
}

fn edit_pattern(ctx: &Ctx, st: &Steps, label: &str, f: impl FnOnce(&mut Pattern) -> Option<u32>) {
    let Some(v) = view(ctx, st) else { return };
    if !v.editable() {
        return;
    }
    let rate = st.rate.get();
    ctx.edit(label, |p| {
        change(p, v.id, rate, f);
    });
}

/// Changes the pattern during a gesture; `end_gesture` makes the undo step.
fn edit_pattern_live(ctx: &Ctx, st: &Steps, f: impl FnOnce(&mut Pattern) -> Option<u32>) {
    let Some(id) = ctx.s().selected_region else { return };
    let rate = st.rate.get();
    ctx.live(true, |p| {
        change(p, id, rate, f);
    });
}

struct Bar {
    rate: gtk::DropDown,
    length: gtk::SpinButton,
    swing: gtk::SpinButton,
    banner: gtk::Box,
    banner_text: gtk::Label,
    convert: gtk::Button,
    syncing: Cell<bool>,
}

/// Whether region `id` is one a pattern can go in: MIDI, and not a take folder.
fn holds_pattern(p: &Project, id: Id) -> bool {
    matches!(p.find_region(id), Some((_, Region { content: Content::Midi(m), .. })) if !m.is_folder())
}

fn apply_starter(ctx: &Ctx, starter: &Starter) {
    let Some(id) = ctx.s().selected_region.filter(|id| holds_pattern(&ctx.s().project, *id)) else { return };
    let Ok(pat) = starter.pattern() else { return };
    ctx.edit("Starter Pattern", |p| replace_pattern(p, id, pat));
}

/// Makes `pat` region `id`'s pattern whatever its notes were, lengthening the region to hold
/// one loop and showing any hidden rows it plays.
fn replace_pattern(p: &mut Project, id: Id, pat: Pattern) {
    let time = p.time();
    let Some((ti, _)) = p.find_region(id).filter(|_| holds_pattern(p, id)) else { return };
    let used = used(&pat);
    for r in p.tracks[ti].drum_map.iter_mut().filter(|r| used.contains(&r.pitch)) {
        r.hidden = false;
    }
    let Some(r) = p.region_mut(id) else { return };
    let need = pat.rate * pat.length;
    if span_of(&time, r.start, r.end()) < need {
        r.length = time.at(r.start, need as u64).saturating_sub(r.start).max(1);
    }
    let span = span_of(&time, r.start, r.end());
    if let Content::Midi(m) = &mut r.content {
        m.set_pattern(pat, span);
    }
}

fn starter_menu(ctx: &Ctx, anchor: &gtk::MenuButton) {
    let m = Menu::at(anchor, 0.0, anchor.height() as f64);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    for genre in genres() {
        let heading = gtk::Label::new(Some(genre));
        heading.add_css_class("heading");
        heading.set_xalign(0.0);
        heading.set_margin_start(8);
        heading.set_margin_top(6);
        list.append(&heading);
        for s in STARTERS.iter().filter(|s| s.genre == genre) {
            let b = gtk::Button::with_label(s.name);
            b.add_css_class("flat");
            if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let (ctx, pop) = (ctx.clone(), m.pop.clone());
            b.connect_clicked(move |_| {
                pop.popdown();
                apply_starter(&ctx, s);
            });
            list.append(&b);
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(480)
        .child(&list)
        .build();
    m.list.append(&scroll);
    m.show();
}

/// The step sequencer, and the buttons in it that only apply on drum tracks.
pub fn build(ctx: &Ctx, go: Rc<dyn Fn(&'static str)>) -> (gtk::Widget, Vec<gtk::Widget>) {
    let st = Rc::new(Steps::new());

    let grid = {
        let (ctx, st) = (ctx.clone(), st.clone());
        Canvas::new(move |c, s, w, h| draw(&ctx, &st, c, s, w, h))
    };
    grid.set_widget_name("step-grid");
    grid.set_focusable(true);
    grid.set_focus_on_click(true);
    let rows: Rc<dyn RowSource> = Rc::new(StepRows { st: st.clone() });
    let head = Rc::new(Header::default());
    let header = {
        let (ctx, rows, head, grid) = (ctx.clone(), rows.clone(), head.clone(), grid.clone());
        Canvas::new(move |c, s, w, _| draw_header(&ctx, &*rows, &head, &grid, c, s, w))
    };

    let hscroll = gtk::ScrolledWindow::builder().vscrollbar_policy(gtk::PolicyType::Never).hexpand(true).child(&grid).build();
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&header);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&hscroll);
    let vscroll = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&body).build();

    let resize: Rc<dyn Fn()> = {
        let (ctx, st, grid, header) = (ctx.clone(), st.clone(), grid.clone(), header.clone());
        Rc::new(move || {
            let (w, rows) = view(&ctx, &st).map_or((16.0 * CELL_W, 16), |v| {
                (grid_ticks(&v.pattern) as f32 / v.pattern.rate.max(1) as f32 * CELL_W, v.rows.len())
            });
            let h = (rows as f32 * ROW_H) as i32;
            grid.set_size_request(w.ceil() as i32, h);
            header.set_size_request(HEAD_W as i32, h);
            grid.queue_draw();
            header.queue_draw();
        })
    };
    let mut drum_only = Vec::new();
    let (top, bar, modes, to_drums) = top_bar(ctx, &st, &grid, resize.clone(), go.clone());
    let banner_box = bar.banner.clone();
    {
        let (ctx, st) = (ctx.clone(), st.clone());
        bar.convert.connect_clicked(move |_| {
            let Some(id) = view(&ctx, &st).filter(|v| !v.folder && v.moved.is_some()).map(|v| v.id) else { return };
            let rate = st.rate.get();
            ctx.edit("Convert to Pattern", |p| {
                convert(p, id, rate);
            });
        });
        let hits = gtk::Button::with_label("Open in Drum Editor");
        hits.set_tooltip_text(Some("Edit the notes where they are"));
        let go = go.clone();
        hits.connect_clicked(move |_| go("drums"));
        banner_box.append(&bar.convert);
        banner_box.append(&hits);
        drum_only.push(hits.upcast());
    }

    install_grid_input(ctx, &st, &grid, &header, &hscroll, &vscroll, modes);
    install_header_input(ctx, rows, head, &header, &grid);

    {
        let (ctx2, st, resize) = (ctx.clone(), st.clone(), resize.clone());
        ctx.on(move |c| {
            if matches!(c, Change::LiveNote) {
                return;
            }
            sync_bar(&ctx2, &st, &bar);
            resize();
        });
    }
    {
        let (ctx, st, hscroll) = (ctx.clone(), st.clone(), hscroll.clone());
        grid.add_tick_callback(move |g, _| {
            if ctx.playing() {
                if st.follow.get()
                    && let Some(v) = view(&ctx, &st)
                {
                    let pos = ctx.position();
                    if pos >= v.start && pos < v.end {
                        let time = ctx.s().project.time();
                        let tick = time.ticks_between(v.start, v.start + (pos - v.start) % v.length.max(1));
                        let x = (tick % grid_ticks(&v.pattern)) as f64 / v.pattern.rate.max(1) as f64 * CELL_W as f64;
                        let a = hscroll.hadjustment();
                        if x < a.value() || x >= a.value() + a.page_size() {
                            a.set_value((x / a.page_size().max(1.0)).floor() * a.page_size());
                        }
                    }
                }
                g.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }
    resize();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&top);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&banner_box);
    root.append(&vscroll);
    drum_only.push(to_drums);
    (root.upcast(), drum_only)
}

fn top_bar(ctx: &Ctx, st: &Rc<Steps>, grid: &Canvas, resize: Rc<dyn Fn()>, go: Rc<dyn Fn(&'static str)>) -> (gtk::Box, Rc<Bar>, Rc<Vec<gtk::ToggleButton>>, gtk::Widget) {
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.set_margin_start(6);
    top.set_margin_end(6);
    top.set_margin_top(4);
    top.set_margin_bottom(4);
    let title = gtk::Label::new(Some("Step Sequencer"));
    title.add_css_class("heading");
    top.append(&title);

    let pattern = gtk::MenuButton::builder().label("Pattern").tooltip_text("Pattern actions").build();
    {
        let (ctx, st, resize) = (ctx.clone(), st.clone(), resize.clone());
        pattern.set_create_popup_func(move |mb| pattern_menu(&ctx, &st, mb, resize.clone()));
    }
    top.append(&pattern);

    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);

    let mode_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    mode_box.add_css_class("linked");
    let mut modes: Vec<gtk::ToggleButton> = Vec::new();
    for (mode, label, tip) in MODES {
        let b = gtk::ToggleButton::with_label(label);
        b.set_tooltip_text(Some(tip));
        b.set_group(modes.first());
        b.set_active(mode == Mode::On);
        let (st, grid) = (st.clone(), grid.clone());
        b.connect_toggled(move |b| {
            if b.is_active() {
                st.mode.set(mode);
                grid.queue_draw();
            }
        });
        mode_box.append(&b);
        modes.push(b);
    }
    top.append(&mode_box);

    let rate = gtk::DropDown::from_strings(&RATES.map(|r| r.1));
    rate.set_selected(2);
    rate.set_tooltip_text(Some("Step length"));
    let length = gtk::SpinButton::with_range(1.0, MAX_STEPS, 1.0);
    length.set_tooltip_text(Some("Steps in the pattern; the region is resized to fit them"));
    let swing = gtk::SpinButton::with_range(0.0, 100.0, 5.0);
    swing.set_tooltip_text(Some("Delays every second step"));
    top.append(&rate);
    top.append(&gtk::Label::new(Some("Length")));
    top.append(&length);
    top.append(&gtk::Label::new(Some("Swing")));
    top.append(&swing);

    let follow = gtk::ToggleButton::with_label("Follow");
    follow.set_tooltip_text(Some("Keep the playhead in view while playing"));
    {
        let st = st.clone();
        follow.connect_toggled(move |b| st.follow.set(b.is_active()));
    }
    top.append(&follow);
    let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    views.add_css_class("linked");
    let mut to_drums = None;
    for (label, tip, page) in [("Drum Editor", "Edit the notes in a row per drum sound", "drums"), ("Piano Roll", "Edit the notes on a keyboard", "midi")] {
        let b = gtk::Button::with_label(label);
        b.set_tooltip_text(Some(tip));
        let go = go.clone();
        b.connect_clicked(move |_| go(page));
        views.append(&b);
        to_drums.get_or_insert(b.upcast::<gtk::Widget>());
    }
    top.append(&views);

    let banner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    banner.set_margin_start(6);
    banner.set_margin_end(6);
    banner.set_margin_top(4);
    banner.set_margin_bottom(4);
    let banner_text = gtk::Label::new(None);
    banner_text.set_hexpand(true);
    banner_text.set_xalign(0.0);
    banner_text.set_wrap(true);
    banner.append(&banner_text);
    banner.set_visible(false);

    let convert = gtk::Button::with_label("Convert to Pattern");
    convert.add_css_class("suggested-action");
    let bar = Bar { rate, length, swing, banner, banner_text, convert, syncing: Cell::new(false) };
    let bar = Rc::new(bar);
    {
        let (ctx, st, b) = (ctx.clone(), st.clone(), bar.clone());
        bar.rate.connect_selected_notify(move |d| {
            if b.syncing.get() {
                return;
            }
            let Some(&(rate, _)) = RATES.get(d.selected() as usize) else { return };
            let has_pattern = view(&ctx, &st).is_some_and(|v| v.moved.is_none());
            if has_pattern {
                edit_pattern(&ctx, &st, "Step Rate", |p| {
                    p.rate = rate;
                    None
                });
            }
            st.rate.set(rate);
            sync_bar(&ctx, &st, &b);
            resize();
        });
    }
    {
        let (ctx, st, b) = (ctx.clone(), st.clone(), bar.clone());
        bar.length.connect_value_changed(move |s| {
            if b.syncing.get() {
                return;
            }
            let n = s.value() as u32;
            edit_pattern(&ctx, &st, "Pattern Length", |p| {
                p.length = n;
                Some(n * p.rate)
            });
        });
    }
    {
        let (ctx, st, b) = (ctx.clone(), st.clone(), bar.clone());
        bar.swing.connect_value_changed(move |s| {
            if b.syncing.get() {
                return;
            }
            let swing = s.value() as f32 / 100.0;
            edit_pattern(&ctx, &st, "Swing", |p| {
                p.swing = swing;
                None
            });
        });
    }
    (top, bar, Rc::new(modes), to_drums.unwrap())
}

/// Brings the top bar and banner in line with the selected region.
fn sync_bar(ctx: &Ctx, st: &Steps, bar: &Bar) {
    let Some(v) = view(ctx, st) else { return };
    bar.syncing.set(true);
    if v.moved.is_none() {
        st.rate.set(v.pattern.rate);
    }
    if let Some(i) = RATES.iter().position(|r| r.0 == v.pattern.rate) {
        bar.rate.set_selected(i as u32);
    }
    bar.length.set_value(v.pattern.length as f64);
    bar.swing.set_value((v.pattern.swing * 100.0).round() as f64);
    bar.syncing.set(false);
    let editable = v.editable();
    for w in [bar.length.upcast_ref::<gtk::Widget>(), bar.swing.upcast_ref()] {
        w.set_sensitive(editable);
    }
    bar.convert.set_visible(!v.folder);
    match v.moved {
        _ if v.folder => {
            bar.banner_text.set_text("A take folder cannot hold a pattern. Flatten it to edit its notes as steps.");
            bar.banner.set_visible(true);
        }
        Some(n) if n > 0 => {
            let notes = if n == 1 { "1 note".to_string() } else { format!("{n} notes") };
            bar.banner_text.set_text(&format!("These notes are not all on the step grid. Converting to a pattern moves {notes}."));
            bar.banner.set_visible(true);
        }
        _ => bar.banner.set_visible(false),
    }
}

fn pattern_menu(ctx: &Ctx, st: &Rc<Steps>, mb: &gtk::MenuButton, resize: Rc<dyn Fn()>) {
    let pop = gtk::Popover::new();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
    pop.set_child(Some(&list));
    let hidden = {
        let s = ctx.s();
        s.selected_region.and_then(|id| s.project.find_region(id)).is_some_and(|(ti, _)| s.project.tracks[ti].drum_map.iter().any(|r| r.hidden))
    };
    let editable = view(ctx, st).is_some_and(|v| v.editable());
    let has_clip = st.clip.borrow().is_some();
    let item = |label: &str, enabled: bool, f: Rc<dyn Fn()>| {
        let b = gtk::Button::with_label(label);
        b.add_css_class("flat");
        b.set_sensitive(enabled);
        if let Some(l) = b.child().and_downcast::<gtk::Label>() {
            l.set_xalign(0.0);
        }
        let pop = pop.clone();
        b.connect_clicked(move |_| {
            pop.popdown();
            f();
        });
        list.append(&b);
    };
    let (c, s) = (ctx.clone(), st.clone());
    item(
        "Copy Pattern",
        true,
        Rc::new(move || {
            *s.clip.borrow_mut() = view(&c, &s).map(|v| v.pattern);
        }),
    );
    let (c, s) = (ctx.clone(), st.clone());
    item(
        "Paste Pattern",
        editable && has_clip,
        Rc::new(move || {
            let Some(clip) = s.clip.borrow().clone() else { return };
            edit_pattern(&c, &s, "Paste Pattern", |p| {
                *p = clip;
                Some(p.rate * p.length)
            });
        }),
    );
    let (c, s) = (ctx.clone(), st.clone());
    item(
        "Clear Pattern",
        editable,
        Rc::new(move || {
            edit_pattern(&c, &s, "Clear Pattern", |p| {
                p.rows.clear();
                None
            })
        }),
    );
    let (c, s) = (ctx.clone(), st.clone());
    item(
        "Double Pattern",
        editable,
        Rc::new(move || {
            let span = view(&c, &s).map_or(0, |v| span_of(&c.s().project.time(), v.start, v.start + v.length));
            edit_pattern(&c, &s, "Double Pattern", |p| {
                p.double();
                Some((p.rate * p.length).max(span))
            });
        }),
    );
    let (c, anchor) = (ctx.clone(), mb.clone());
    item("Starter Patterns…", view(ctx, st).is_some_and(|v| !v.folder), Rc::new(move || starter_menu(&c, &anchor)));
    let s = st.clone();
    item(
        if st.used_only.get() { "Show All Rows" } else { "Show Used Rows Only" },
        true,
        Rc::new(move || {
            s.used_only.set(!s.used_only.get());
            resize();
        }),
    );
    let (c, s) = (ctx.clone(), st.clone());
    item(
        "Show Hidden Rows",
        hidden,
        Rc::new(move || {
            let used = view(&c, &s).map(|v| v.used()).unwrap_or_default();
            edit_map(&c, "Show Hidden Rows", used, |map| {
                for r in map {
                    r.hidden = false;
                }
            });
        }),
    );
    mb.set_popover(Some(&pop));
}

fn install_grid_input(
    ctx: &Ctx,
    st: &Rc<Steps>,
    grid: &Canvas,
    header: &Canvas,
    hscroll: &gtk::ScrolledWindow,
    vscroll: &gtk::ScrolledWindow,
    modes: Rc<Vec<gtk::ToggleButton>>,
) {
    let cell_at = |v: &View, x: f64, y: f64| -> Option<(usize, u8, u16)> {
        let ri = (y / ROW_H as f64).floor();
        let row = v.rows.get(ri as usize).filter(|_| ri >= 0.0)?;
        let i = step_at(&v.pattern, row.pitch, x)?;
        Some((ri as usize, row.pitch, i as u16))
    };
    let redraw = {
        let (grid, header) = (grid.clone(), header.clone());
        move || {
            grid.queue_draw();
            header.queue_draw();
        }
    };

    let g = gtk::GestureDrag::new();
    g.set_button(0);
    {
        let (ctx, st, grid, redraw) = (ctx.clone(), st.clone(), grid.clone(), redraw.clone());
        g.connect_drag_begin(move |g, x, y| {
            grid.grab_focus();
            let Some(v) = view(&ctx, &st) else { return };
            let Some((ri, pitch, index)) = cell_at(&v, x, y) else { return };
            st.focus.set((ri, index as u32));
            redraw();
            if !v.editable() {
                return;
            }
            let time = g.current_event_time();
            let button = g.current_button();
            let double = gtk::Settings::default().map_or(400, |s| s.gtk_double_click_time()) as u32;
            if let Some((t, b, p, i, removed)) = st.last_press.take()
                && (b, p, i) == (button, pitch, index)
                && button == gdk::BUTTON_PRIMARY
                && time.wrapping_sub(t) <= double
            {
                // The second press of a double-click: the first one was not meant as a toggle.
                if removed {
                    ctx.retract(Mode::On.label());
                }
                step_popover(&ctx, &st, &grid, pitch, index, x, y);
                return;
            }
            let step = v.step(pitch, index);
            let mode = st.mode.get();
            let p = if button == gdk::BUTTON_SECONDARY {
                Paint::Steps(false)
            } else {
                match mode {
                    Mode::On => Paint::Steps(step.is_none()),
                    Mode::Tie => Paint::Tie(!step.is_some_and(|s| s.tie)),
                    m => {
                        let from = m.get(&step.unwrap_or(Step::new(index, 100)));
                        Paint::Value { pitch, index, from, last: from }
                    }
                }
            };
            st.last_press.set(Some((time, button, pitch, index, step.is_some() && p == Paint::Steps(false))));
            st.paint.set(Some(p));
            st.visited.borrow_mut().clear();
            st.visited.borrow_mut().insert((pitch, index));
            ctx.begin_gesture();
            match p {
                Paint::Steps(on) => edit_pattern_live(&ctx, &st, |pat| {
                    paint(pat, pitch, index, Mode::On, on);
                    None
                }),
                Paint::Tie(on) => edit_pattern_live(&ctx, &st, |pat| {
                    paint(pat, pitch, index, Mode::Tie, on);
                    None
                }),
                Paint::Value { .. } if step.is_none() => edit_pattern_live(&ctx, &st, |pat| {
                    paint(pat, pitch, index, Mode::On, true);
                    None
                }),
                Paint::Value { .. } => {}
            }
            if step.is_none() && p != Paint::Steps(false) {
                audition(&ctx, pitch, 100);
            }
            grid.queue_draw();
        });
    }
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        g.connect_drag_update(move |g, dx, dy| {
            let Some(p) = st.paint.get() else { return };
            let mode = st.mode.get();
            match p {
                Paint::Value { pitch, index, from, last } => {
                    let Some((lo, hi, _)) = mode.range() else { return };
                    let fine = g.current_event_state().contains(gdk::ModifierType::SHIFT_MASK);
                    let per = if fine { 400.0 } else { 100.0 };
                    let value = (from + (-dy * (hi - lo) as f64 / per).round() as i32).clamp(lo, hi);
                    if value != last {
                        st.paint.set(Some(Paint::Value { pitch, index, from, last: value }));
                        edit_pattern_live(&ctx, &st, |pat| {
                            set_value(pat, pitch, index, mode, value);
                            None
                        });
                        grid.queue_draw();
                    }
                }
                Paint::Steps(on) | Paint::Tie(on) => {
                    let Some((sx, sy)) = g.start_point() else { return };
                    let Some(v) = view(&ctx, &st) else { return };
                    let Some((_, pitch, index)) = cell_at(&v, sx + dx, sy + dy) else { return };
                    if st.visited.borrow_mut().insert((pitch, index)) {
                        let mode = if matches!(p, Paint::Tie(_)) { Mode::Tie } else { Mode::On };
                        edit_pattern_live(&ctx, &st, |pat| {
                            paint(pat, pitch, index, mode, on);
                            None
                        });
                        grid.queue_draw();
                    }
                }
            }
        });
    }
    {
        let (ctx, st) = (ctx.clone(), st.clone());
        g.connect_drag_end(move |_, _, _| {
            if let Some(p) = st.paint.take() {
                let label = match p {
                    Paint::Steps(_) => Mode::On.label(),
                    Paint::Tie(_) => Mode::Tie.label(),
                    Paint::Value { .. } => st.mode.get().label(),
                };
                ctx.end_gesture(label);
            }
        });
    }
    grid.add_controller(g);

    let pointer = Rc::new(Cell::new((0.0, 0.0)));
    let motion = gtk::EventControllerMotion::new();
    {
        let pointer = pointer.clone();
        motion.connect_motion(move |_, x, y| pointer.set((x, y)));
    }
    grid.add_controller(motion);
    let sc = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let (ctx, st, grid) = (ctx.clone(), st.clone(), grid.clone());
        sc.connect_scroll(move |c, _, dy| {
            let (x, y) = pointer.get();
            let Some(v) = view(&ctx, &st).filter(|v| v.editable()) else { return glib::Propagation::Proceed };
            let Some((_, pitch, index)) = cell_at(&v, x, y) else { return glib::Propagation::Proceed };
            let Some(step) = v.step(pitch, index) else { return glib::Propagation::Proceed };
            let ctrl = c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK);
            let (mode, by) = if ctrl { (Mode::Chance, 5) } else { (Mode::Velocity, 8) };
            let value = mode.get(&step) + if dy < 0.0 { by } else { -by };
            edit_pattern(&ctx, &st, mode.label(), |p| {
                set_value(p, pitch, index, mode, value);
                None
            });
            grid.queue_draw();
            glib::Propagation::Stop
        });
    }
    grid.add_controller(sc);

    let keys = gtk::EventControllerKey::new();
    {
        let (ctx, st, grid, hscroll, vscroll) = (ctx.clone(), st.clone(), grid.clone(), hscroll.clone(), vscroll.clone());
        keys.connect_key_pressed(move |_, key, _, mods| {
            let v = view(&ctx, &st).filter(|v| !v.rows.is_empty());
            // The window lets Return through to here, so it still goes to the start when
            // there is no step to toggle.
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && !v.as_ref().is_some_and(|v| v.editable()) {
                let _ = WidgetExt::activate_action(&grid, "win.goto-start", None);
                return glib::Propagation::Stop;
            }
            let Some(v) = v else { return glib::Propagation::Proceed };
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let (ri, i) = st.focus.get();
            let ri = ri.min(v.rows.len() - 1);
            let pitch = v.rows[ri].pitch;
            let len = timing(&v.pattern, pitch).1;
            let i = i.min(len - 1);
            let index = i as u16;
            let mode = st.mode.get();
            let digit = |k: gdk::Key| (1..=7u32).find(|d| k.to_unicode() == char::from_digit(*d, 10));
            match key {
                gdk::Key::Left | gdk::Key::Right | gdk::Key::Up | gdk::Key::Down if !ctrl => {
                    let (ri, i) = match key {
                        gdk::Key::Left => (ri, i.saturating_sub(1)),
                        gdk::Key::Right => (ri, (i + 1).min(len - 1)),
                        gdk::Key::Up => (ri.saturating_sub(1), i),
                        _ => ((ri + 1).min(v.rows.len() - 1), i),
                    };
                    let p = v.rows[ri].pitch;
                    let i = i.min(timing(&v.pattern, p).1 - 1);
                    st.focus.set((ri, i));
                    let (x, w) = step_x(&v.pattern, p, i);
                    reveal(&hscroll.hadjustment(), x as f64, w as f64);
                    reveal(&vscroll.vadjustment(), (ri as f32 * ROW_H) as f64, ROW_H as f64);
                }
                gdk::Key::Return | gdk::Key::KP_Enter if v.editable() => {
                    let step = v.step(pitch, index);
                    let (m, on) = match mode {
                        Mode::Tie => (Mode::Tie, !step.is_some_and(|s| s.tie)),
                        _ => (Mode::On, step.is_none()),
                    };
                    edit_pattern(&ctx, &st, m.label(), |p| {
                        paint(p, pitch, index, m, on);
                        None
                    });
                    if on && step.is_none() {
                        audition(&ctx, pitch, 100);
                    }
                }
                gdk::Key::Delete | gdk::Key::BackSpace if v.editable() => {
                    if v.step(pitch, index).is_some() {
                        edit_pattern(&ctx, &st, "Clear Step", |p| {
                            paint(p, pitch, index, Mode::On, false);
                            None
                        });
                    }
                }
                gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add | gdk::Key::minus | gdk::Key::KP_Subtract if !ctrl && v.editable() => {
                    let Some(step) = v.step(pitch, index) else { return glib::Propagation::Stop };
                    let m = mode.valued();
                    let by = m.range().map_or(1, |r| r.2);
                    let up = matches!(key, gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add);
                    let value = m.get(&step) + if up { by } else { -by };
                    edit_pattern(&ctx, &st, m.label(), |p| {
                        set_value(p, pitch, index, m, value);
                        None
                    });
                }
                gdk::Key::c if ctrl => {
                    *st.row_clip.borrow_mut() = Some(v.row(pitch).cloned().unwrap_or_else(|| PatternRow::new(pitch)));
                }
                gdk::Key::v if ctrl && v.editable() => paste_row(&ctx, &st, pitch),
                k if !ctrl && digit(k).is_some() => {
                    if let Some(b) = digit(k).and_then(|d| modes.get(d as usize - 1)) {
                        b.set_active(true);
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            grid.queue_draw();
            glib::Propagation::Stop
        });
    }
    grid.add_controller(keys);
    {
        let redraw = redraw.clone();
        grid.connect_has_focus_notify(move |_| redraw());
    }
}

/// Scrolls just far enough to show `from..from + len`.
fn reveal(a: &gtk::Adjustment, from: f64, len: f64) {
    if from < a.value() {
        a.set_value(from);
    } else if from + len > a.value() + a.page_size() {
        a.set_value(from + len - a.page_size());
    }
}

fn paste_row(ctx: &Ctx, st: &Steps, pitch: u8) {
    let Some(clip) = st.row_clip.borrow().clone() else { return };
    edit_pattern(ctx, st, "Paste Row", |p| {
        *row_mut(p, pitch) = PatternRow { pitch, ..clip };
        None
    });
}

fn draw(ctx: &Ctx, st: &Steps, c: &Canvas, s: &gtk::Snapshot, w: f32, h: f32) {
    let Some(v) = view(ctx, st) else { return };
    let fg = canvas::fg(c);
    let shade = |a: f32| gdk::RGBA::new(fg.red(), fg.green(), fg.blue(), a);
    let base = track_color(v.color);
    let color = |a: f32| gdk::RGBA::new(base.red(), base.green(), base.blue(), a);
    let p = &v.pattern;
    let rate = p.rate.max(1);
    let time = ctx.s().project.time();
    let cols = grid_ticks(p).div_ceil(rate as u64) as usize;
    let per_beat = (time.ticks_per_beat() / rate as u64).max(1) as usize;
    let per_bar = per_beat * time.sig_at(v.start).0.max(1) as usize;
    for col in 0..cols {
        let x = col as f32 * CELL_W;
        rect(s, x, 0.0, CELL_W, h, &shade(if (col / per_beat).is_multiple_of(2) { 0.05 } else { 0.025 }));
        if col % per_bar == 0 {
            rect(s, x, 0.0, 1.0, h, &shade(0.3));
        }
    }
    let dim = if v.editable() { 1.0 } else { 0.4 };
    let mode = st.mode.get();
    let tick = ctx.playing().then(|| ctx.position()).filter(|pos| *pos >= v.start && *pos < v.end).map(|pos| {
        time.ticks_between(v.start, v.start + (pos - v.start) % v.length.max(1))
    });
    let (focus_row, focus_step) = st.focus.get();
    for (ri, row) in v.rows.iter().enumerate() {
        let y = ri as f32 * ROW_H;
        rect(s, 0.0, y + ROW_H - 1.0, w, 1.0, &shade(0.08));
        let (row_rate, len) = timing(p, row.pitch);
        let cells = grid_ticks(p).div_ceil(row_rate as u64) as u32;
        let silent = row.mute || v.rows.iter().any(|r| r.solo) && !row.solo;
        let dim = if silent { dim * 0.45 } else { dim };
        for i in 0..cells {
            let (x, cw) = step_x(p, row.pitch, i);
            rounded(s, x + 2.0, y + 3.0, cw - 4.0, ROW_H - 6.0, 3.0, &shade(if i < len { 0.07 } else { 0.025 }));
        }
        if let Some(t) = tick {
            let (x, cw) = step_x(p, row.pitch, row_step(p, row.pitch, t));
            rect(s, x, y, cw, ROW_H, &shade(0.14));
        }
        let Some(pr) = v.row(row.pitch) else { continue };
        for step in pr.steps.iter().filter(|s| (s.index as u32) < cells) {
            let i = step.index as u32;
            let (x, cw) = step_x(p, row.pitch, i);
            let past = if i < len { 1.0 } else { 0.3 };
            let a = dim * past;
            let tied = i < len && pr.joins(step.index, len);
            if let Some((lo, hi, _)) = mode.range() {
                rounded(s, x + 2.0, y + 3.0, cw - 4.0, ROW_H - 6.0, 3.0, &color(0.25 * a));
                let inner = ROW_H - 8.0;
                let value = mode.get(step);
                if mode == Mode::Offset {
                    let mid = y + 4.0 + inner / 2.0;
                    let off = value as f32 / hi as f32 * inner / 2.0;
                    rect(s, x + 4.0, mid - off.max(0.0), cw - 8.0, off.abs().max(1.0), &color(a));
                } else {
                    let f = (value - lo + 1) as f32 / (hi - lo + 1) as f32;
                    rect(s, x + 4.0, y + 4.0 + inner * (1.0 - f), cw - 8.0, inner * f, &color(a));
                }
                continue;
            }
            let vel = step.velocity as f32 / 127.0;
            let chance = step.chance as f32 / 100.0;
            if tied {
                rect(s, x - 3.0, y + ROW_H / 2.0 - 3.0, cw, 6.0, &color((0.35 + 0.65 * vel) * a));
                rounded(s, x + 2.0, y + 3.0, cw - 4.0, ROW_H - 6.0, 3.0, &color(0.3 * a));
                continue;
            }
            let shift = step.offset as f32 / 100.0 * cw;
            let (cx, cy, ww, hh) = (x + 2.0 + shift, y + 3.0, cw - 4.0, ROW_H - 6.0);
            rounded(s, cx, cy, ww, hh, 3.0, &color((0.35 + 0.65 * vel) * (0.3 + 0.7 * chance) * a));
            if step.chance < 100 {
                let o = color(a);
                rect(s, cx, cy, ww, 1.0, &o);
                rect(s, cx, cy + hh - 1.0, ww, 1.0, &o);
                rect(s, cx, cy, 1.0, hh, &o);
                rect(s, cx + ww - 1.0, cy, 1.0, hh, &o);
            }
            if step.repeat > 1 {
                let n = step.repeat as f32;
                for j in 0..step.repeat {
                    rect(s, cx + 2.0 + j as f32 * (ww - 4.0) / n, cy + 2.0, 1.5, 4.0, &rgba(0xffffff, 0.85 * a));
                }
            }
            if step.gate < 100 {
                rect(s, cx + 3.0, cy + hh - 4.0, (ww - 6.0) * step.gate as f32 / 100.0, 2.0, &rgba(0xffffff, 0.7 * a));
            }
        }
    }
    if c.has_focus()
        && let Some(row) = v.rows.get(focus_row)
    {
        let i = focus_step.min(timing(p, row.pitch).1 - 1);
        let (x, cw) = step_x(p, row.pitch, i);
        let y = focus_row as f32 * ROW_H;
        let a = accent(c);
        rect(s, x + 1.0, y + 1.0, cw - 2.0, 2.0, &a);
        rect(s, x + 1.0, y + ROW_H - 3.0, cw - 2.0, 2.0, &a);
        rect(s, x + 1.0, y + 1.0, 2.0, ROW_H - 2.0, &a);
        rect(s, x + cw - 3.0, y + 1.0, 2.0, ROW_H - 2.0, &a);
    }
}

struct StepRows {
    st: Rc<Steps>,
}

impl RowSource for StepRows {
    fn rows(&self, ctx: &Ctx) -> Option<Vec<DrumRow>> {
        view(ctx, &self.st).map(|v| v.rows)
    }

    fn used(&self, ctx: &Ctx) -> Vec<u8> {
        view(ctx, &self.st).map(|v| v.used()).unwrap_or_default()
    }

    fn badge(&self, ctx: &Ctx, row: &DrumRow) -> (String, bool) {
        let Some(v) = view(ctx, &self.st) else { return (String::new(), false) };
        let own = v.row(row.pitch).is_some_and(|r| r.rate.is_some() || r.length.is_some());
        let (rate, len) = timing(&v.pattern, row.pitch);
        (format!("{} {len}", rate_name(rate)), own)
    }

    fn badge_tip(&self) -> &'static str {
        "Step length and loop length of this row"
    }

    fn badge_clicked(&self, ctx: &Ctx, anchor: &Canvas, x: f64, y: f64, row: &DrumRow) {
        if let Some(v) = view(ctx, &self.st) {
            badge_popover(ctx, &self.st, anchor, x, y, row.pitch, &v);
        }
    }

    fn extend_menu(&self, ctx: &Ctx, m: &Menu, anchor: &Canvas, x: f64, y: f64, row: &DrumRow) {
        let Some(v) = view(ctx, &self.st) else { return };
        let st = &self.st;
        let header = anchor;
    let pitch = row.pitch;
    let editable = v.editable();
    let len = timing(&v.pattern, pitch).1;
    let has_steps = v.row(pitch).is_some_and(|r| !r.steps.is_empty());
    let op = |label: &'static str, enabled: bool, f: Rc<dyn Fn(&mut PatternRow)>| {
        let (ctx, st) = (ctx.clone(), st.clone());
        m.item(label, editable && enabled, move || {
            let f = f.clone();
            edit_pattern(&ctx, &st, label, move |p| {
                f(row_mut(p, pitch));
                None
            });
        });
    };
    {
        let (st, r) = (st.clone(), v.row(pitch).cloned());
        m.item("Copy Row", true, move || *st.row_clip.borrow_mut() = Some(r.clone().unwrap_or_else(|| PatternRow::new(pitch))));
    }
    {
        let (ctx, st2) = (ctx.clone(), st.clone());
        m.item("Paste Row", editable && st.row_clip.borrow().is_some(), move || paste_row(&ctx, &st2, pitch));
    }
    op("Clear Row", has_steps, Rc::new(|r| r.clear()));
    m.separator();
    op("Shift Left", has_steps, Rc::new(move |r| r.shift(-1, len)));
    op("Shift Right", has_steps, Rc::new(move |r| r.shift(1, len)));
    op("Reverse", has_steps, Rc::new(move |r| r.reverse(len)));
    let anchor = header.clone();
    let ask = |label: &'static str, fields: Vec<(&'static str, f64, f64, f64)>, f: Rc<dyn Fn(&mut PatternRow, &[u32])>| {
        let (ctx, st, anchor) = (ctx.clone(), st.clone(), anchor.clone());
        m.item(&format!("{label}…"), editable, move || {
            let (ctx, st, f) = (ctx.clone(), st.clone(), f.clone());
            ask_numbers(&anchor, x, y, &fields, move |values| {
                let f = f.clone();
                edit_pattern(&ctx, &st, label, move |p| {
                    f(row_mut(p, pitch), &values);
                    None
                });
            });
        });
    };
    ask(
        "Randomise",
        vec![("Density (%)", 1.0, 100.0, 40.0)],
        Rc::new(move |r, v| r.randomise(glib::random_int(), v[0] as u8, len)),
    );
    let l = len as f64;
    ask(
        "Fill Every",
        vec![("Every", 1.0, l, 4.0f64.min(l)), ("From Step", 1.0, l, 1.0)],
        Rc::new(move |r, v| r.fill_every(v[0], v[1] - 1, len)),
    );
    ask("Euclidean", vec![("Hits", 1.0, l, 3.0f64.min(l)), ("Steps", 1.0, l, 8.0f64.min(l))], Rc::new(move |r, v| r.euclid(v[0], v[1], len)));
    }

    fn focus_row(&self) -> usize {
        self.st.focus.get().0
    }

    fn set_focus_row(&self, i: usize) {
        self.st.focus.set((i, self.st.focus.get().1));
    }
}

/// A popover of labelled number fields with an Apply button.
fn ask_numbers(anchor: &Canvas, x: f64, y: f64, fields: &[(&str, f64, f64, f64)], done: impl Fn(Vec<u32>) + 'static) {
    let m = Menu::at(anchor, x, y);
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    let spins: Vec<gtk::SpinButton> = fields
        .iter()
        .enumerate()
        .map(|(i, (label, lo, hi, value))| {
            let l = gtk::Label::new(Some(label));
            l.set_xalign(0.0);
            let spin = gtk::SpinButton::with_range(*lo, hi.max(*lo), 1.0);
            spin.set_value(*value);
            grid.attach(&l, 0, i as i32, 1, 1);
            grid.attach(&spin, 1, i as i32, 1, 1);
            spin
        })
        .collect();
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    grid.attach(&apply, 1, fields.len() as i32, 1, 1);
    m.list.append(&grid);
    let pop = m.pop.clone();
    apply.connect_clicked(move |_| {
        pop.popdown();
        done(spins.iter().map(|s| s.value() as u32).collect());
    });
    m.show();
}

/// Rate and loop length for one row, either the pattern's or its own.
fn badge_popover(ctx: &Ctx, st: &Rc<Steps>, anchor: &Canvas, x: f64, y: f64, pitch: u8, v: &View) {
    let m = Menu::at(anchor, x, y);
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    let row = v.row(pitch);
    let mut names = vec!["Pattern"];
    names.extend(RATES.iter().map(|r| r.1));
    let rate = gtk::DropDown::from_strings(&names);
    rate.set_selected(row.and_then(|r| r.rate).and_then(|r| RATES.iter().position(|x| x.0 == r)).map_or(0, |i| i as u32 + 1));
    let own = gtk::CheckButton::with_label("Own Length");
    own.set_active(row.is_some_and(|r| r.length.is_some()));
    let length = gtk::SpinButton::with_range(1.0, MAX_STEPS, 1.0);
    length.set_value(timing(&v.pattern, pitch).1 as f64);
    length.set_sensitive(own.is_active());
    let l = gtk::Label::new(Some("Step Length"));
    l.set_xalign(0.0);
    grid.attach(&l, 0, 0, 1, 1);
    grid.attach(&rate, 1, 0, 1, 1);
    grid.attach(&own, 0, 1, 1, 1);
    grid.attach(&length, 1, 1, 1, 1);
    m.list.append(&grid);
    let enabled = v.editable();
    grid.set_sensitive(enabled);
    let apply = {
        let (ctx, st, rate, own, length) = (ctx.clone(), st.clone(), rate.clone(), own.clone(), length.clone());
        Rc::new(move || {
            let r = rate.selected().checked_sub(1).and_then(|i| RATES.get(i as usize)).map(|r| r.0);
            let len = own.is_active().then(|| length.value() as u32);
            length.set_sensitive(own.is_active());
            edit_pattern(&ctx, &st, "Row Timing", |p| {
                let row = row_mut(p, pitch);
                row.rate = r;
                row.length = len;
                None
            });
        })
    };
    {
        let apply = apply.clone();
        rate.connect_selected_notify(move |_| apply());
    }
    {
        let apply = apply.clone();
        own.connect_toggled(move |_| apply());
    }
    length.connect_value_changed(move |_| apply());
    m.show();
}

/// Every setting of one step. Changing one switches the step on if it was off.
fn step_popover(ctx: &Ctx, st: &Rc<Steps>, anchor: &Canvas, pitch: u8, index: u16, x: f64, y: f64) {
    let Some(v) = view(ctx, st) else { return };
    let step = v.step(pitch, index).unwrap_or(Step::new(index, 100));
    let m = Menu::at(anchor, x, y);
    let grid = gtk::Grid::builder().row_spacing(6).column_spacing(12).margin_top(6).margin_bottom(6).margin_start(6).margin_end(6).build();
    let fields = [(Mode::Velocity, "Velocity", ""), (Mode::Gate, "Gate (%)", ""), (Mode::Repeat, "Repeat", ""), (Mode::Chance, "Chance (%)", "Each repeat is rolled on its own"), (Mode::Offset, "Offset (%)", "")];
    let set = {
        let (ctx, st) = (ctx.clone(), st.clone());
        Rc::new(move |f: &dyn Fn(&mut Step)| {
            edit_pattern(&ctx, &st, "Edit Step", |p| {
                let row = row_mut(p, pitch);
                let mut s = row.step(index).copied().unwrap_or(Step::new(index, 100));
                f(&mut s);
                row.set(s);
                None
            });
        })
    };
    for (i, (mode, label, tip)) in fields.into_iter().enumerate() {
        let (lo, hi, _) = mode.range().unwrap_or((0, 1, 1));
        let l = gtk::Label::new(Some(label));
        l.set_xalign(0.0);
        let spin = gtk::SpinButton::with_range(lo as f64, hi as f64, 1.0);
        spin.set_value(mode.get(&step) as f64);
        if !tip.is_empty() {
            spin.set_tooltip_text(Some(tip));
        }
        let set = set.clone();
        spin.connect_value_changed(move |s| {
            let value = s.value() as i32;
            set(&|step| mode.set(step, value));
        });
        grid.attach(&l, 0, i as i32, 1, 1);
        grid.attach(&spin, 1, i as i32, 1, 1);
    }
    let tie = gtk::CheckButton::with_label("Tie to the step before");
    tie.set_active(step.tie);
    tie.connect_toggled(move |b| {
        let on = b.is_active();
        set(&|step| step.tie = on);
    });
    grid.attach(&tie, 0, fields.len() as i32, 2, 1);
    m.list.append(&grid);
    m.show();
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u32 = PPQ / 4;

    fn on(p: &mut Pattern, pitch: u8, steps: &[u16]) {
        for &i in steps {
            paint(p, pitch, i, Mode::On, true);
        }
    }

    fn project_with(content: MidiContent, bars: u64) -> (Project, Id) {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let id = p.alloc_id();
        let length = p.time().bar_len() * bars;
        p.track_mut(t).unwrap().regions.push(Region {
            id,
            name: String::new(),
            start: 0,
            length,
            muted: false,
            loops: 0,
            color: None,
            content: Content::Midi(content),
        });
        (p, id)
    }

    fn midi_of(p: &Project, id: Id) -> &MidiContent {
        match &p.find_region(id).unwrap().1.content {
            Content::Midi(m) => m,
            _ => panic!(),
        }
    }

    #[test]
    fn mode_values_clamp_to_their_range() {
        let mut s = Step::new(0, 100);
        Mode::Velocity.set(&mut s, 200);
        Mode::Gate.set(&mut s, 0);
        Mode::Repeat.set(&mut s, 12);
        Mode::Chance.set(&mut s, -5);
        Mode::Offset.set(&mut s, -80);
        assert_eq!((s.velocity, s.gate, s.repeat, s.chance, s.offset), (127, 1, 8, 1, -50));
        // Steps and Tie nudge velocity.
        Mode::On.set(&mut s, 64);
        assert_eq!((s.velocity, Mode::Tie.valued()), (64, Mode::Velocity));
    }

    #[test]
    fn painting_steps_and_ties() {
        let mut p = Pattern::default();
        on(&mut p, 36, &[0, 4]);
        paint(&mut p, 36, 4, Mode::On, true);
        assert_eq!(p.rows[0].steps.len(), 2);
        paint(&mut p, 36, 5, Mode::Tie, true);
        paint(&mut p, 36, 0, Mode::Tie, true);
        assert!(p.rows[0].step(5).unwrap().tie && p.rows[0].step(0).unwrap().tie);
        paint(&mut p, 36, 0, Mode::Tie, false);
        assert!(!p.rows[0].step(0).unwrap().tie);
        paint(&mut p, 36, 7, Mode::Tie, false);
        assert!(p.rows[0].step(7).is_none());
        paint(&mut p, 36, 4, Mode::On, false);
        assert_eq!(p.rows[0].steps.iter().map(|s| s.index).collect::<Vec<_>>(), vec![0, 5]);
        set_value(&mut p, 36, 0, Mode::Chance, 30);
        set_value(&mut p, 36, 9, Mode::Chance, 30);
        assert_eq!(p.rows[0].step(0).unwrap().chance, 30);
        assert!(p.rows[0].step(9).is_none());
    }

    #[test]
    fn rows_with_their_own_rate_are_wider_cells() {
        let mut p = Pattern::default();
        on(&mut p, 36, &[0]);
        let r = row_mut(&mut p, 38);
        r.rate = Some(2 * S);
        r.length = Some(12);
        assert_eq!(step_x(&p, 36, 3), (3.0 * CELL_W, CELL_W));
        assert_eq!(step_x(&p, 38, 3), (6.0 * CELL_W, 2.0 * CELL_W));
        assert_eq!(step_at(&p, 36, 3.5 * CELL_W as f64), Some(3));
        assert_eq!(step_at(&p, 38, 3.5 * CELL_W as f64), Some(1));
        assert_eq!(step_at(&p, 36, 16.5 * CELL_W as f64), None);
        assert_eq!(step_at(&p, 38, 23.5 * CELL_W as f64), Some(11));
        assert_eq!(step_at(&p, 38, -1.0), None);
        assert_eq!(grid_ticks(&p), 24 * S as u64);
        // Each row's playhead loops on its own length.
        assert_eq!(row_step(&p, 36, 17 * S as u64 + 5), 1);
        assert_eq!(row_step(&p, 38, 25 * S as u64), 0);
        assert_eq!(row_step(&p, 40, 5 * S as u64), 5);
    }

    #[test]
    fn rows_come_from_the_map_then_the_pattern() {
        let defaults = vec![drum_row(36, "Kick".into()), drum_row(38, "Snare".into())];
        let rows = all_rows(&[], &defaults, &[50, 36, 41, 41]);
        assert_eq!(rows.iter().map(|r| (r.pitch, r.name.as_str())).collect::<Vec<_>>(), vec![(36, "Kick"), (38, "Snare"), (41, "F2"), (50, "D3")]);
        let map = vec![DrumRow { hidden: true, ..drum_row(38, String::new()) }, drum_row(36, "Bass Drum".into())];
        let rows = all_rows(&map, &defaults, &[38]);
        assert_eq!(rows.iter().map(|r| (r.pitch, r.name.as_str(), r.hidden)).collect::<Vec<_>>(), vec![(38, "Snare", true), (36, "Bass Drum", false)]);
    }

    #[test]
    fn placing_a_row_shifts_the_rows_between() {
        let pitches = |m: &[DrumRow]| m.iter().map(|r| r.pitch).collect::<Vec<_>>();
        let mut map: Vec<DrumRow> = [36, 38, 42, 46].iter().map(|p| drum_row(*p, String::new())).collect();
        place_row(&mut map, 36, 42);
        assert_eq!(pitches(&map), vec![38, 42, 36, 46]);
        place_row(&mut map, 46, 38);
        assert_eq!(pitches(&map), vec![46, 38, 42, 36]);
        place_row(&mut map, 46, 50);
        assert_eq!(pitches(&map), vec![46, 38, 42, 36]);
    }

    #[test]
    fn hits_rows_cover_the_map_and_every_note() {
        let mut p = Project::new("Test", 48000);
        let t = p.add_track(TrackKind::Instrument);
        let t = p.track_mut(t).unwrap();
        t.drum_map = vec![drum_row(38, "Snare".into()), DrumRow { hidden: true, ..drum_row(36, "Kick".into()) }];
        let rows = hit_rows(t, &[Note::new(0, 10, 36, 100), Note::new(0, 10, 99, 100)]);
        assert_eq!(rows.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![38, 99]);
    }

    #[test]
    fn a_starter_replaces_the_pattern_and_lengthens_the_region() {
        // Off-grid notes that could not be converted are replaced all the same.
        let notes = vec![Note::new(7, 10, 36, 100), Note::new(9, 10, 36, 100)];
        let (mut p, id) = project_with(MidiContent { notes, ..Default::default() }, 1);
        p.tracks[0].drum_map = vec![DrumRow { hidden: true, ..drum_row(39, String::new()) }];
        let trap = STARTERS.iter().find(|s| s.name == "Trap").unwrap();
        replace_pattern(&mut p, id, trap.pattern().unwrap());
        let time = p.time();
        let r = p.find_region(id).unwrap().1;
        assert_eq!(span_of(&time, r.start, r.end()), 32 * S);
        let m = midi_of(&p, id);
        assert_eq!(m.pattern.as_ref().unwrap().length, 32);
        assert!(!m.notes.is_empty() && m.notes.iter().all(|n| n.start != 7 && n.start != 9));
        assert!(!p.tracks[0].drum_map[0].hidden);
        assert_eq!(p.settle_patterns(), 0);
        // A longer region keeps its length and loops the pattern.
        let (mut p, id) = project_with(MidiContent::default(), 4);
        replace_pattern(&mut p, id, STARTERS[0].pattern().unwrap());
        assert_eq!(midi_of(&p, id).pattern.as_ref().unwrap().span, 64 * S);
    }

    #[test]
    fn moving_a_row_skips_hidden_ones() {
        let mut map = vec![drum_row(36, "A".into()), DrumRow { hidden: true, ..drum_row(38, "B".into()) }, drum_row(42, "C".into())];
        move_row(&mut map, 42, true);
        assert_eq!(map.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![42, 38, 36]);
        move_row(&mut map, 42, true);
        assert_eq!(map[0].pitch, 42);
        move_row(&mut map, 36, false);
        assert_eq!(map.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![42, 38, 36]);
        move_row(&mut map, 42, false);
        assert_eq!(map.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![36, 38, 42]);
    }

    #[test]
    fn first_edit_converts_notes_that_sit_on_steps() {
        let notes = vec![Note::new(0, S, 36, 100), Note::new(4 * S, S, 38, 90)];
        let (mut p, id) = project_with(MidiContent { notes, ..Default::default() }, 1);
        assert!(change(&mut p, id, S, |pat| {
            on(pat, 42, &[2]);
            None
        }));
        let m = midi_of(&p, id);
        let pat = m.pattern.as_ref().unwrap();
        assert_eq!((pat.rate, pat.length, pat.span), (S, 16, 16 * S));
        assert_eq!(m.notes.iter().map(|n| (n.start, n.pitch)).collect::<Vec<_>>(), vec![(0, 36), (2 * S, 42), (4 * S, 38)]);
        assert_eq!(p.settle_patterns(), 0);
    }

    #[test]
    fn notes_off_the_grid_are_left_alone() {
        // Two hits too close to share a step.
        let notes = vec![Note::new(0, 10, 36, 100), Note::new(30, 10, 36, 100)];
        let (mut p, id) = project_with(MidiContent { notes: notes.clone(), ..Default::default() }, 1);
        let before = p.clone();
        assert!(!change(&mut p, id, S, |pat| {
            on(pat, 36, &[1]);
            None
        }));
        assert_eq!(p, before);
        assert_eq!(midi_of(&p, id).notes, notes);
    }

    #[test]
    fn length_and_double_resize_the_region() {
        let (mut p, id) = project_with(MidiContent::default(), 1);
        change(&mut p, id, S, |pat| {
            on(pat, 36, &[0, 8]);
            pat.length = 8;
            Some(8 * S)
        });
        let time = p.time();
        let r = p.find_region(id).unwrap().1.clone();
        assert_eq!(time.ticks_between(r.start, r.end()), 8 * S as u64);
        assert_eq!(midi_of(&p, id).notes.len(), 1);
        change(&mut p, id, S, |pat| {
            pat.double();
            Some(pat.rate * pat.length)
        });
        let r = p.find_region(id).unwrap().1.clone();
        assert_eq!(time.ticks_between(r.start, r.end()), 16 * S as u64);
        let m = midi_of(&p, id);
        assert_eq!(m.notes.iter().map(|n| n.start).collect::<Vec<_>>(), vec![0, 8 * S]);
        assert_eq!(m.pattern.as_ref().unwrap().span, 16 * S);
        assert_eq!(p.settle_patterns(), 0);
    }

    fn studio(p: Project, id: Id) -> Ctx {
        let dir = std::env::temp_dir().join("solstice-drum-editor-test-unused");
        let ctx = Ctx::new(crate::studio::Studio::new(p, Bundle::new(dir), crate::settings::Settings::default()));
        ctx.m().selected_region = Some(id);
        ctx
    }

    fn folder() -> MidiContent {
        let take = |pitch| MidiTake { name: "Take".into(), notes: vec![Note::new(0, S, pitch, 100)] };
        MidiContent { notes: vec![Note::new(0, S, 36, 100)], takes: vec![take(36), take(38)], ..Default::default() }
    }

    #[test]
    fn take_folders_take_no_pattern_edits() {
        let (mut p, id) = project_with(folder(), 1);
        p.tracks[0].drum_map = vec![DrumRow { hidden: true, ..drum_row(36, "Kick".into()) }];
        let ctx = studio(p, id);
        let before = ctx.s().project.clone();
        let st = Steps::new();
        assert!(view(&ctx, &st).is_some_and(|v| v.folder && !v.editable()));
        edit_pattern(&ctx, &st, "Edit Steps", |pat| {
            on(pat, 36, &[4]);
            None
        });
        apply_starter(&ctx, &STARTERS[0]);
        assert!(ctx.s().history.undo_label().is_none());
        let p = &mut ctx.m().project;
        assert!(!convert(p, id, S));
        replace_pattern(p, id, STARTERS[0].pattern().unwrap());
        assert_eq!(*p, before);
    }

    #[test]
    fn a_converting_first_edit_undoes_and_redoes_as_one() {
        let notes = vec![Note::new(0, S, 36, 100), Note::new(8 * S, S, 36, 100)];
        let (p, id) = project_with(MidiContent { notes: notes.clone(), ..Default::default() }, 1);
        let ctx = studio(p, id);
        let st = Steps::new();
        edit_pattern(&ctx, &st, "Edit Steps", |pat| {
            on(pat, 38, &[4]);
            None
        });
        let after = ctx.s().project.clone();
        assert_eq!(midi_of(&after, id).notes.len(), 3);
        assert!(midi_of(&after, id).pattern.is_some());
        ctx.undo();
        assert!(ctx.s().history.undo_label().is_none());
        let m = midi_of(&ctx.s().project, id).clone();
        assert_eq!((m.notes, m.pattern), (notes, None));
        ctx.redo();
        assert_eq!(ctx.s().project, after);
    }

    #[test]
    fn retracting_a_first_click_leaves_nothing_to_redo() {
        let (p, id) = project_with(MidiContent::default(), 1);
        let ctx = studio(p, id);
        let st = Steps::new();
        edit_pattern(&ctx, &st, Mode::On.label(), |pat| {
            on(pat, 36, &[0]);
            None
        });
        let one = ctx.s().project.clone();
        edit_pattern(&ctx, &st, Mode::On.label(), |pat| {
            paint(pat, 36, 0, Mode::On, false);
            None
        });
        ctx.retract("Tie Steps");
        assert!(midi_of(&ctx.s().project, id).notes.is_empty());
        ctx.retract(Mode::On.label());
        assert_eq!(ctx.s().project, one);
        assert!(ctx.s().history.redo_label().is_none());
    }

    #[test]
    fn a_note_edit_from_elsewhere_drops_the_pattern() {
        let (p, id) = project_with(MidiContent::default(), 1);
        let ctx = studio(p, id);
        let st = Steps::new();
        edit_pattern(&ctx, &st, "Edit Steps", |pat| {
            on(pat, 36, &[0, 4]);
            None
        });
        ctx.m().message = None;
        ctx.edit("Transpose", |p| {
            if let Some(Region { content: Content::Midi(m), .. }) = p.region_mut(id) {
                m.notes.iter_mut().for_each(|n| n.pitch += 2);
            }
        });
        let s = ctx.s();
        assert_eq!(s.message.as_deref(), Some("The pattern is now plain notes"));
        let m = midi_of(&s.project, id);
        assert!(m.pattern.is_none());
        assert!(m.notes.iter().all(|n| n.pitch == 38));
    }

    #[test]
    fn emptied_rows_are_dropped_from_the_pattern() {
        let (mut p, id) = project_with(MidiContent::default(), 1);
        change(&mut p, id, S, |pat| {
            on(pat, 36, &[0]);
            row_mut(pat, 38).length = Some(3);
            None
        });
        change(&mut p, id, S, |pat| {
            paint(pat, 36, 0, Mode::On, false);
            None
        });
        let pat = midi_of(&p, id).pattern.clone().unwrap();
        assert_eq!(pat.rows.iter().map(|r| r.pitch).collect::<Vec<_>>(), vec![38]);
    }
}
