# Drum Editor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A Drum Editor page where drum hits are placed, painted, given velocity and quantized per row, replacing the piano roll's Hits view.

**Architecture:** Pure note functions and the new model fields live in `solstice_model` and are unit-tested. The step sequencer's row header moves into a shared `drum_rows.rs` used by both drum pages. The new page is `hit_editor.rs`, drawn on `Canvas` like the piano roll, editing through `edit_midi`/`Ctx::edit` so undo is unchanged. `editor.rs` routes drum regions to it and loses `Lanes::Hits`.

**Tech Stack:** Rust 2024, GTK4 + libadwaita (gtk-rs), serde.

**Spec:** `docs/superpowers/specs/2026-11-23-drum-editor-design.md`

## Global Constraints

- Work on branch `drum-editor` in a git worktree; never switch branches in the main checkout. Do not release.
- Formal UI copy. Lean code: no `// Section ---` banners, no docstrings restating names; comment only what the code can't show.
- New serialized fields use `#[serde(default, skip_serializing_if = ...)]`, so 0.7.12 projects open unchanged and untouched projects save byte-identical.
- Every edit goes through `Ctx::edit` (or `edit_midi`) with a label.
- `cargo test` and `cargo clippy --all-targets` pass with no new warnings.
- Headless runs use a private `XDG_RUNTIME_DIR` (`rt=$(mktemp -d) && chmod 700 "$rt"`, removed afterwards). Use `pgrep -x`, never `pkill -f`.
- Exact copy from the spec: banner text `This region is a step pattern. Editing it here turns it into hits.`; banner button `Open in Step Sequencer`; step sequencer banner button `Open in Drum Editor`; page buttons `Drum Editor`, `Step Sequencer`, `Piano Roll`, `Event List`; menu items `Select Hits in Row`, `Delete Hits in Row`; actions `Quantize`, `Iterative Quantize`, `Humanise`, `Auto Quantize`; grid value `Editor` for a row following the toolbar, `Off` for a free row.

## Review Focus

- A row whose grid is Off: Q, Shift+Q and Auto Quantize leave its hits exactly where they are; clicks and drags on it don't snap; painting uses the toolbar grid.
- Hits whose pitch has no drum map row (recorded from a pad not in the map): they still show (on the extra rows `all_rows` adds) and quantize with the toolbar grid.
- A paint stroke over steps that already have hits adds nothing there, and a stroke that runs past the region end stops at it.
- Undo straight after the first edit to a step pattern region brings the pattern back intact (one undo step, not two).
- Take folder regions: edits go to the comp notes, as in the piano roll, and Auto Quantize quantizes every take.

---

### Task 1: Model fields for row grids and drum quantize settings

**Files:**
- Modify: `crates/model/src/lib.rs` (`DrumRow`, `Track`, `freeze_key`, the `Track` literal near line 1213, tests)
- Modify: every other `DrumRow { .. }` literal (`grep -rn "DrumRow {" crates`)

**Interfaces:**
- Produces:
  - `DrumRow.grid: Option<u32>`: ticks; `None` follows the toolbar grid; `Some(0)` is Off.
  - `pub struct DrumQuantize { pub grid: u32, pub strength: f32, pub swing: f32, pub iterative: f32, pub humanise_ms: f32, pub humanise_velocity: f32, pub auto: bool }` with `Default` = `{ grid: PPQ / 4, strength: 1.0, swing: 0.0, iterative: 0.5, humanise_ms: 8.0, humanise_velocity: 0.1, auto: false }`, `Serialize/Deserialize/Clone/Copy/Debug/PartialEq`.
  - `Track.drum_quantize: DrumQuantize`, `#[serde(default, skip_serializing_if = "is_default_quantize")]`.
  - `impl Track { pub fn row_grid(&self, pitch: u8) -> u32 }`: the row's grid (0 for Off), else `drum_quantize.grid`.

- [ ] **Step 1: Write failing tests** in the `lib.rs` test module:

```rust
#[test]
fn row_grids_fall_back_to_the_track_grid() {
    let mut p = Project::new("T", 48000);
    let id = p.add_track(TrackKind::Instrument);
    let t = p.track_mut(id).unwrap();
    t.drum_map = vec![
        DrumRow { pitch: 42, name: "Hat".into(), hidden: false, mute: false, solo: false, grid: Some(PPQ / 3) },
        DrumRow { pitch: 49, name: "Crash".into(), hidden: false, mute: false, solo: false, grid: Some(0) },
    ];
    assert_eq!(t.row_grid(42), PPQ / 3);
    assert_eq!(t.row_grid(49), 0);
    assert_eq!(t.row_grid(36), PPQ / 4);
}

#[test]
fn drum_quantize_is_left_out_of_files_until_changed() {
    // Same shape as the existing drum_map serialization test near line 3196.
    let mut p = Project::new("T", 48000);
    let id = p.add_track(TrackKind::Instrument);
    assert!(!serde_json::to_string(&p).unwrap().contains("drum_quantize"));
    p.track_mut(id).unwrap().drum_quantize.auto = true;
    let json = serde_json::to_string(&p).unwrap();
    assert!(json.contains("drum_quantize"));
    let back: Project = serde_json::from_str(&json).unwrap();
    assert!(back.track(id).unwrap().drum_quantize.auto);
}

#[test]
fn drum_quantize_does_not_change_the_freeze_key() {
    let mut p = Project::new("T", 48000);
    let id = p.add_track(TrackKind::Instrument);
    let before = freeze_key(&p, p.track(id).unwrap());
    p.track_mut(id).unwrap().drum_quantize.strength = 0.5;
    assert_eq!(freeze_key(&p, p.track(id).unwrap()), before);
}
```

- [ ] **Step 2: Run** `cargo test -p solstice-model row_grids drum_quantize`. Expected: compile failure (`grid`, `drum_quantize` missing).
- [ ] **Step 3: Implement** the fields, `Default`, `is_default_quantize`, `row_grid`. In `freeze_key` reset `t.drum_quantize = DrumQuantize::default()` and `r.grid = None` alongside the existing `r.name.clear()`. Add `grid: None` to every `DrumRow` literal (`drum_row` in `drum_editor.rs`, tests).
- [ ] **Step 4: Run** `cargo test -p solstice-model`. Expected: all pass.
- [ ] **Step 5: Commit** `git commit -am "Give drum rows their own grid and tracks drum quantize settings"`.

### Task 2: Pure hit functions in `midi_tools`

**Files:**
- Modify: `crates/model/src/midi_tools.rs`

**Interfaces:**
- Consumes: `Note`, `Track::row_grid` shape (the functions take a grid lookup closure, not a `Track`).
- Produces:
  - `pub fn quantize_rows(notes: &[Note], grid: impl Fn(u8) -> u32, strength: f32, swing: f32) -> Vec<Note>`: `quantize` per note with that pitch's grid; grid 0 leaves the note alone. Iterative quantize is this with `strength = iterative`.
  - `pub fn humanise(notes: &[Note], max_ticks: u32, velocity: f32, seed: u32) -> Vec<Note>`: each start moves by a uniform random amount in `-max_ticks..=max_ticks` (not below 0); velocity scales by a random factor in `1 ± velocity`, clamped 1–127. Uses `crate::pattern::xorshift`, seeded with `seed.max(1)`.
  - `pub fn paint(existing: &[Note], pitch: u8, from: u32, to: u32, grid: u32, length: u32, velocity: u8, end: u32) -> Vec<Note>`: new hits on `pitch` at every multiple of `grid` between `from` and `to` (either order, inclusive of the step each end lies in), below `end`, skipping steps where `existing` already has a hit on `pitch` starting inside that step. Grid 0 is treated as 1 tick (callers pass the toolbar grid for Off rows).
  - `pub fn erase(existing: &[Note], pitch: u8, from: u32, to: u32, tolerance: u32) -> Vec<Note>`: the hits on `pitch` whose start lies within `min(from,to) - tolerance ..= max(from,to) + tolerance`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn rows_quantize_to_their_own_grid_and_off_rows_stay() {
    let notes = [n(130, 10, 36), n(130, 10, 42), n(130, 10, 49)];
    let grid = |p: u8| match p { 36 => 240, 42 => 320, _ => 0 };
    let q = quantize_rows(&notes, grid, 1.0, 0.0);
    assert_eq!(q.iter().map(|n| n.start).collect::<Vec<_>>(), [240, 0, 130]);
}

#[test]
fn iterative_quantize_moves_part_of_the_way() {
    let q = quantize_rows(&[n(200, 10, 36)], |_| 240, 0.5, 0.0);
    assert_eq!(q[0].start, 220);
}

#[test]
fn humanise_is_repeatable_and_bounded() {
    let notes: Vec<Note> = (0..50).map(|i| n(1000 + i * 240, 10, 38)).collect();
    let a = humanise(&notes, 20, 0.1, 7);
    assert_eq!(a, humanise(&notes, 20, 0.1, 7));
    for (o, h) in notes.iter().zip(&a) {
        assert!((h.start as i64 - o.start as i64).abs() <= 20);
        assert!((90..=110).contains(&h.velocity));
    }
    assert_ne!(a, notes);
}

#[test]
fn painting_fills_empty_steps_up_to_the_region_end() {
    let existing = [n(240, 10, 36)];
    let hits = paint(&existing, 36, 10, 1000, 240, 240, 90, 900);
    assert_eq!(hits.iter().map(|n| n.start).collect::<Vec<_>>(), [0, 480, 720]);
    assert!(hits.iter().all(|n| n.pitch == 36 && n.velocity == 90 && n.length == 240));
    assert_eq!(paint(&existing, 36, 1000, 10, 240, 240, 90, 900), hits);
}

#[test]
fn erasing_takes_hits_on_one_row_only() {
    let existing = [n(0, 10, 36), n(240, 10, 36), n(240, 10, 38), n(960, 10, 36)];
    assert_eq!(erase(&existing, 36, 0, 300, 5), [n(0, 10, 36), n(240, 10, 36)]);
}
```

- [ ] **Step 2: Run** `cargo test -p solstice-model midi_tools`. Expected: compile failure.
- [ ] **Step 3: Implement** the four functions as specified.
- [ ] **Step 4: Run** `cargo test -p solstice-model midi_tools`. Expected: pass.
- [ ] **Step 5: Commit** `"Add per-row quantize, humanise, paint and erase hit functions"`.

### Task 3: Turning a step pattern into hits, and Auto Quantize on record

**Files:**
- Modify: `crates/model/src/lib.rs` (new `Project` methods + tests)
- Modify: `crates/app/src/studio.rs` (`midi_region`, the record-finish path near line 722, `capture_recording`, tests)

**Interfaces:**
- Consumes: `quantize_rows`, `Track::row_grid`, `DrumQuantize`.
- Produces:
  - `impl Project { pub fn release_pattern(&mut self, region: Id, shown: Vec<DrumRow>) -> bool }`: when the region holds a pattern, sets `pattern = None` keeping `notes`. `shown` is the full row list the app displays (`all_rows`, hidden rows included); it becomes the drum map when the map is empty, as `edit_map` does. Then each map row whose pitch has steps in the pattern and whose `grid` is `None` gets `Some(row.rate.unwrap_or(pattern.rate))`. Returns whether there was a pattern.
  - `pub fn auto_quantize(track: &Track, m: &mut MidiContent)`: quantizes `m.notes` and every take's notes with `quantize_rows(.., |p| track.row_grid(p), dq.strength, dq.swing)`. No-op unless `track.drum_quantize.auto`.
  - In `Studio::midi_region`, after the region is pushed and when the track's `drum_quantize.auto` is on: `self.history.push("Auto Quantize", &self.project)` then `auto_quantize` the new region's content. The recording's own undo step is pushed by the callers before `midi_region`, so this makes the quantize a second, separate step.

- [ ] **Step 1: Write failing tests**

```rust
// lib.rs
#[test]
fn releasing_a_pattern_keeps_its_notes_and_seeds_row_grids() {
    // Build a project with one instrument track and a MIDI region holding a Pattern with
    // rate PPQ/4, one row on 36 with steps and one row on 42 with rate Some(PPQ/3).
    // After release_pattern(id, rows for 36 and 42): m.pattern is None, m.notes equal the baked notes before,
    // row_grid(36) == PPQ/4, row_grid(42) == PPQ/3, and a second call returns false.
}

// studio.rs tests
#[test]
fn auto_quantize_is_its_own_undo_step() {
    // Studio with an instrument track, drum_quantize.auto = true, events with a note at a
    // tick 30 past a 1/16 line. Push "Record" then midi_region(&events, false).
    // The region's note sits on the line; history.undo restores the played tick with the
    // region still present; a second undo removes the region.
}
```

Write them out fully using the helpers already in each test module (`studio(...)`, `recorded_midi` tests near line 2484 show how events are built).

- [ ] **Step 2: Run** `cargo test -p solstice-model releasing_a_pattern` and `cargo test -p solstice-app auto_quantize` (check the crate name in `crates/app/Cargo.toml`). Expected: fail.
- [ ] **Step 3: Implement** `release_pattern`, `auto_quantize` and the `midi_region` hook.
- [ ] **Step 4: Run** both crates' tests. Expected: pass.
- [ ] **Step 5: Commit** `"Release step patterns to hits and quantize drum recordings"`.

### Task 4: Shared row header in `drum_rows.rs`

**Files:**
- Create: `crates/app/src/drum_rows.rs`
- Modify: `crates/app/src/drum_editor.rs` (remove what moves), `crates/app/src/main.rs` (`mod drum_rows;`)

**Interfaces:**
- Produces (all `pub(crate)`):
  - Moved unchanged: `drum_row`, `is_drum_track`, `is_drum_kit`, `hit_rows`, `defaults`, `all_rows`, `note_name`, `pad_of`, `kit`, `load_sample`, `show_pad`, `place_row`, `move_row`, `edit_map`, `audition`, the `Zone` enum, layout constants `HEAD_W`, `BADGE_X`, `BADGE_W`, `MUTE_X`, `SOLO_X`, `MENU_X`, `BOX_W`, and their tests.
  - `pub(crate) struct Header { pub focus: Cell<usize>, pub moving: Cell<Option<(usize, usize)>> }`.
  - `pub(crate) trait RowSource { fn rows(&self, ctx: &Ctx) -> Vec<DrumRow>; fn used(&self, ctx: &Ctx) -> Vec<u8>; fn row_h(&self) -> f32; fn badge(&self, ctx: &Ctx, row: &DrumRow) -> (String, bool); fn badge_clicked(&self, ctx: &Ctx, anchor: &Canvas, x: f64, y: f64, row: &DrumRow); fn extend_menu(&self, ctx: &Ctx, m: &Menu, row: &DrumRow); }`. `badge` returns its text and whether it is highlighted (the row has its own setting).
  - `pub(crate) fn draw_header(ctx: &Ctx, src: &dyn RowSource, h: &Header, focused: bool, c: &Canvas, s: &gtk::Snapshot, w: f32)`.
  - `pub(crate) fn install_header_input(ctx: &Ctx, src: Rc<dyn RowSource>, h: Rc<Header>, header: &Canvas, grid: &Canvas)`: name click auditions and focuses, double-click renames, drag reorders, mute, solo, badge via `badge_clicked`, menu and right-click build the row menu: `extend_menu` items first, then Rename…, Move Up, Move Down, the Drum Kit items, Hide Row. Tooltips as today, with the Badge tooltip supplied by a `fn badge_tip(&self) -> &'static str` on the trait.
- The step sequencer implements `RowSource` with its pattern view: badge = `"{rate} {len}"`, badge click = its existing `badge_popover`, `extend_menu` = Copy Row … Euclidean (its current pattern items).

- [ ] **Step 1:** Move the functions and their tests; build. Run `cargo test` and confirm the moved tests still pass under their new module.
- [ ] **Step 2:** Introduce `RowSource` and `Header`, port the step sequencer to them. `Steps.focus` keeps its step half; the row half reads from `Header.focus`.
- [ ] **Step 3: Verify** `cargo test && cargo clippy --all-targets` pass, then check on screen (see Task 8's recipe) that the step sequencer header looks and behaves as before: audition, rename, reorder, mute, solo, badge popover, row menu.
- [ ] **Step 4: Commit** `"Share the drum row header between drum pages"`.

### Task 5: Drum Editor page: layout, drawing and routing

**Files:**
- Create: `crates/app/src/hit_editor.rs`
- Modify: `crates/app/src/editor.rs` (routing, remove `Lanes`, Hits toggle, `HitsPref`, `hit_rows` use, `diamond` moves to `hit_editor.rs`), `crates/app/src/drum_editor.rs` (buttons), `crates/app/src/main.rs`

**Interfaces:**
- Consumes: `drum_rows::*`, `editor::edit_midi`, `editor::midi`, `Track::row_grid`, `Project::release_pattern`.
- Produces:
  - `pub fn build(ctx: &Ctx, go: Rc<dyn Fn(&'static str)>) -> gtk::Widget` (go switches editor page: `"steps"`, `"midi"`, `"events"`).
  - `struct Hits` (state, like `Roll`): `ppt`, `t0`, `hadj`, `selected: RefCell<Vec<Note>>`, `drag: Cell<Drag>`, `offset`, `clipboard`, `insert_velocity: Cell<u8>` (100), `header: Rc<Header>`, `syncing`, `just_added`, `seed: Cell<u32>`.
  - `fn edit_hits(ctx: &Ctx, label: &str, f: impl FnOnce(&mut Vec<Note>))`: releases a step pattern first, inside the same `ctx.edit`, then edits notes; every Drum Editor edit goes through it.
  - `editor::midi_page(prefer, folder, drums: bool, steps: bool) -> &'static str`: folder → `"takes"`, drums → `"drums"`, steps → `"steps"`, else `"midi"`; `prefer` overrides as today.
- Layout per the spec's sketch: toolbar (region name, Grid drop-down of note values 1/4, 1/8, 1/8T, 1/16, 1/16T, 1/32, 1/32T, 1/64, Quantize and Humanise menu buttons as placeholders until Task 7, Velocity spin, Step Sequencer / Piano Roll / Event List buttons), the pattern banner (hidden unless the region holds a pattern), header + grid in a vertical `ScrolledWindow`, velocity lane with label `Velocity · {row name}`, horizontal scrollbar. Row height 22 px (the Hits view's `HIT_ROW`).
- Drawing: per row, grid lines at that row's grid (toolbar grid for Off rows) faint, beats and bars stronger (reuse `draw_beats` style); diamonds 12 px across filled with the track colour at alpha `0.25 + 0.75 * velocity / 127`; selected hits with the white outline; playhead; marquee box.
- Routing: drum tracks open on `"drums"`; the piano roll gets a Drum Editor button visible on drum tracks; the step sequencer's Hits/Keys buttons become Drum Editor/Piano Roll, its Edit as Hits button becomes Open in Drum Editor, both shown only on drum tracks. The `auto` remembered choice now only decides `steps` for non-drum tracks.

- [ ] **Step 1: Update tests** in `editor.rs`: delete `hits()`, `rows_map_to_pitches_and_back`, `moving_notes_between_rows`; rewrite `midi_regions_open_on_the_picked_page_else_by_content`:

```rust
assert_eq!(midi_page(None, false, false, false), "midi");
assert_eq!(midi_page(None, false, false, true), "steps");
assert_eq!(midi_page(None, false, true, true), "drums");
assert_eq!(midi_page(None, true, true, true), "takes");
assert_eq!(midi_page(Some("steps"), false, true, true), "steps");
assert_eq!(midi_page(Some("takes"), false, true, false), "drums");
```

- [ ] **Step 2: Run** `cargo test midi_regions_open`. Expected: fail.
- [ ] **Step 3: Implement** routing, page skeleton, drawing, banner, velocity lane drawing, read-only (input comes in Task 6). Remove `Lanes` from the piano roll, leaving keys only.
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** `"Add the Drum Editor page and open drum regions in it"`.

### Task 6: Drum Editor input

**Files:**
- Modify: `crates/app/src/hit_editor.rs`

**Interfaces:**
- Consumes: `midi_tools::{paint, erase, shift, replace}`, `edit_hits`, `Track::row_grid`.
- Produces: `fn snap(tick: f64, grid: u32) -> u32` (floor to grid, grid 0 = unchanged); `fn hit_at(ctx, hits, x, y) -> Option<Note>` (diamond hit test, ±7 px).

Behaviour (from the spec; the row grid is `row_grid(pitch)`, with the toolbar grid used where that is 0 for painting):
- Click on empty space: add a hit (length = one row-grid step, toolbar grid if Off) at the insert velocity, audition it; a click that only clears a selection adds nothing.
- Drag from empty space: paint along the press row; each motion event calls `paint` from the last painted tick to the current one and applies it with `ctx.live`, ending with one undo step `"Paint Hits"` on release (follow `edit_pattern_live` / `end_gesture` in the step sequencer).
- Right-drag: `erase` along the press row, same live-then-one-step pattern, label `"Erase Hits"`; a right-click on a hit deletes it.
- Double-click on a hit deletes it (`just_added` guard as in the piano roll).
- Drag a hit: move the selection (Alt copies); time snaps per landing row's grid; rows move by whole rows across the shown rows.
- Ctrl-drag from empty: marquee; Ctrl+Shift adds; Shift-click toggles.
- Keys: Up/Down move a row, Left/Right nudge by each hit's row grid, Delete/BackSpace, Ctrl+A/C/X/V (paste at the playhead, same rows), Q, Shift+Q, Escape.
- Velocity: lane drag sets bars of the focused row under the pointer (`"Change Velocity"`); the toolbar spin sets the selection's velocity, or `insert_velocity` when nothing is selected. Selecting hits focuses their row.

- [ ] **Step 1: Write failing tests** for the pure helpers:

```rust
#[test]
fn snapping_floors_to_the_row_grid_and_off_is_free() {
    assert_eq!(snap(250.0, 240), 240);
    assert_eq!(snap(479.9, 240), 240);
    assert_eq!(snap(250.0, 0), 250);
}
```

- [ ] **Step 2: Run** `cargo test snapping_floors`. Expected: fail.
- [ ] **Step 3: Implement** `snap`, `hit_at` and the gestures above.
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets`. Expected: pass.
- [ ] **Step 5: Commit** `"Place, paint, erase, move and shape hits in the Drum Editor"`.

### Task 7: Grid badge, Quantize and Humanise menus

**Files:**
- Modify: `crates/app/src/hit_editor.rs`

**Interfaces:**
- Consumes: `quantize_rows`, `humanise`, `groove_quantize`, `crate::grooves`, `DrumQuantize`, `drum_rows::edit_map`.
- Produces: the Drum Editor's `RowSource` impl: badge text = the row grid's note name, `Editor` when `None`, `Off` for `Some(0)`; highlighted when `Some`; `badge_clicked` opens a list (Editor, Off, then the toolbar's note values) that sets the row's `grid` via `edit_map` (`"Set Row Grid"`); `extend_menu` adds Select Hits in Row and Delete Hits in Row.
- Toolbar grid drop-down reads and writes `track.drum_quantize.grid` (`"Set Grid"`).
- Quantize menu: Quantize button (Q) applying `quantize_rows` with the track's strength and swing, or `groove_quantize` when a groove is picked (groove is per session, as in the piano roll); Strength %, Swing %, Iterative % spin rows; Groove list as in the piano roll; Iterative Quantize button (Shift+Q); an `Auto Quantize` switch. Spin and switch changes write `drum_quantize` (`"Change Quantize Settings"`).
- Humanise menu: Timing (ms) and Velocity % spins written to `drum_quantize`; a Humanise button applying `humanise` to the selection (or all hits) with `max_ticks` from the tempo at the region start (`TimeMap::tempo_at`) and a fresh seed from `glib::random_int()`, label `"Humanise"`.

- [ ] **Step 1:** Implement the badge, menus and wiring.
- [ ] **Step 2: Run** `cargo test && cargo clippy --all-targets`. Expected: pass.
- [ ] **Step 3: Commit** `"Add row grids, quantize and humanise controls to the Drum Editor"`.

### Task 8: Check on screen and finish

**Files:** none new.

- [ ] **Step 1:** `cargo build`, then run the debug binary headless in the Fedora 45 Podman container with Xvfb, xdotool and xwd, `target/debug` mounted read-only, and a private `XDG_RUNTIME_DIR`. Open a project with a Drum Kit track: check that a starter pattern region opens in the Drum Editor with the banner; one click converts it and Ctrl+Z restores it; painting a hat row on 1/8T; setting a row to Off; Q leaves the Off row; Humanise; velocity lane on the focused row; a plain MIDI track with a drum map opens in the Drum Editor; a synth region opens in the piano roll with no Hits toggle.
- [ ] **Step 2:** Fix anything found, re-run `cargo test && cargo clippy --all-targets`.
- [ ] **Step 3:** Commit fixes; leave the branch unpushed and unreleased.
