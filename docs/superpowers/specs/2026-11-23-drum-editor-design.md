# Drum Editor: one page for placing, shaping and quantizing hits

Status: draft for approval, 2026-11-23.

## Intent

Editing drums in Solstice today means moving between three places: the step sequencer for
grid patterns, the piano roll's Hits view for hits off the grid, and the velocity lane or
step modes to shape dynamics. Each does part of the job, and the switching makes drum editing
feel awkward next to a dedicated drum editor such as Cubase's. Tom wants one page where hits
are placed, painted along a row, given velocity and quantized without changing view, with
quantizing that matches what he relies on in Cubase: a grid per row, humanising, iterative
quantize and quantize while recording.

The Drum Editor is an original Solstice editor in the spirit of those editors, not a copy of
any one of them.

## Goals

- A new Drum Editor page in the editor pane, and the page every drum region opens on: Drum
  Kit regions, regions on any track with a drum map, and step pattern regions.
- One row per drum sound from the track's drum map, with the step sequencer's full row
  header: name (click to audition, double-click to rename, drag to reorder), the row's grid,
  mute, solo and a row menu.
- Hits drawn as diamonds with no length, shaded by velocity, plus a velocity lane.
- Click to add a hit, drag along a row to paint hits on every grid step crossed, right-drag
  to erase.
- Select, move and copy hits across rows and in time, with the keyboard shortcuts the piano
  roll already has.
- A grid per row, saved in the drum map. Clicking, painting, dragging, nudging and Quantize
  snap each hit to its own row's grid.
- Humanise, Iterative Quantize and Auto Quantize alongside the existing Quantize, strength,
  swing and grooves.
- Every edit undoable, like the other editors.

## Non-goals

- Replacing the step sequencer. It stays for programming patterns with ratchets, chance and
  ties, one click away.
- Note lengths in the Drum Editor. Hits keep the length they have; new hits take the length
  of one step of their row's grid.
- Drum maps that remap input or output notes (Cubase's I-note/O-note). A row is one pitch.
- Per-row MIDI channel or output port.
- A controller lane other than velocity. The piano roll keeps the controller lane.
- Editing several regions at once.

## Anti-goals

- Anything that sends the user to another view to finish an ordinary drum edit: placing,
  removing, moving, velocity or quantizing.
- Branches for drums inside the piano roll. The Hits view is removed, not extended.
- Changing a step pattern without saying so. Editing one in the Drum Editor turns it into
  hits, and the banner says so before the first edit.

## Constraints

- Rust, GTK4 and libadwaita, drawn on the existing `Canvas` like the piano roll and step
  sequencer.
- Edits go through `Ctx::edit` with a label, as `edit_midi` does, so undo works unchanged.
- Projects saved by 0.7.12 open unchanged; new fields use `serde(default)` and are skipped
  when at their defaults, as the drum map's fields are.
- Formal UI copy, lean code with no banner comments, per CLAUDE.md.
- `cargo test` and `cargo clippy` pass.

## Approaches

Three were considered:

- **A. A new Drum Editor page that replaces the Hits view.** A page of its own beside the
  piano roll, step sequencer and Event List, sharing the step sequencer's row header and the
  existing note-editing helpers (`edit_midi`, `midi_tools`, groove quantize). The piano roll
  goes back to keys only. Most new code, but each drum feature is built once without fitting
  around the piano roll's 128-key layout.
- **B. Grow the piano roll's Hits view.** Least new code, but every feature becomes another
  Hits-or-keys branch in a 1,500-line piano roll, which is how the current editing became
  clunky.
- **C. A free-timing mode in the step sequencer.** Ruled out: its model stores steps on a
  grid, so flams, recorded e-kit performances and humanised parts do not fit.

Tom picked **A**.

## Design

### Opening and moving between views

- A new `drums` page in the editor stack. A MIDI region on a drum track (`is_drum_track`)
  opens on it, whether or not its notes fit a step grid and whether or not it holds a step
  pattern. Take folders still open on their takes first.
- Other MIDI regions open as now: step pattern regions on tracks that are not drum tracks
  stay in the step sequencer, and the rest in the piano roll. The Drum Editor buttons and the
  "Open in Drum Editor" banner button show only on drum tracks, so a synth track never
  becomes a drum track by accident.
- The Drum Editor's toolbar has buttons for Step Sequencer, Piano Roll and Event List. The
  step sequencer's Hits and Keys buttons become Drum Editor and Piano Roll, and its "Edit as
  Hits" banner button becomes "Open in Drum Editor". The piano roll shows a Drum Editor button
  on drum tracks.
- The piano roll's Hits toggle, the `Lanes::Hits` variant and the per-region Hits preference
  are removed.

### Step pattern regions

When the region holds a step pattern, a banner sits above the grid: "This region is a step
pattern. Editing it here turns it into hits." with an "Open in Step Sequencer" button. The
first edit drops the pattern and keeps its current notes as hits, in the same undo step as
the edit, so one undo restores the pattern. The banner then goes. The step sequencer's
existing Convert to Pattern brings it back when the hits sit on a grid.

### Layout

```
 Region name   [Grid 1/16 ▾] [Quantize ▾] [Humanise ▾]  Velocity [100]   Step Sequencer · Piano Roll · Event List
 ┌───────────────────────────────┬─────────────────────────────────────────────┐
 │ Kick          1/16    M  S  ⋮ │ ◆       ◆   ◆       ◆       ◆   ◆          │
 │ Snare         1/16    M  S  ⋮ │     ◇       ◆       ◇       ◆       ◇      │
 │ Closed Hat    1/8T    M  S  ⋮ │ ◆  ◆  ◆  ◆  ◆  ◆  ◆  ◆  ◆  ◆  ◆  ◆         │
 ├───────────────────────────────┼─────────────────────────────────────────────┤
 │ Velocity · Snare              │ ▮   ▯   ▮   ▮   ▯   ▮                       │
 └───────────────────────────────┴─────────────────────────────────────────────┘
```

- Row header on the left, the hit grid on the right, a velocity lane at the bottom and a
  horizontal scrollbar, as in the piano roll. Rows scroll vertically with the header.
- The grid shades each row's own grid lines lightly, with beats and bars stronger, so a 1/8T
  hat row and a 1/16 kick row each show where their hits will land.
- Playhead, horizontal zoom and sideways scrolling behave as in the piano roll.

### Row header

The step sequencer's header, moved into code both pages share: name, grid badge, mute, solo,
row menu, row reordering by drag, sample drop on Drum Kit rows, and the same tooltips. The two
differ only in the badge: the step sequencer's shows its step rate and loop length; the Drum
Editor's shows the row's grid, or "Editor" when the row follows the toolbar grid, and opens a
list of note values to pick from.

Clicking a row's name auditions it at the insert velocity and focuses the row. The row menu
gains Select Hits in Row and Delete Hits in Row; it keeps Rename, Move Up, Move Down, Load
Sample, Show in Drum Kit and Hide Row.

### Hits

- Diamonds centred on each hit's start, filled with the track colour at an opacity that
  rises with velocity, so soft ghost notes read lighter than accents. Selected hits get the
  white outline the Hits view uses now.
- Hit testing is by diamond, as in the Hits view.

### Mouse

- Click on empty space: adds a hit there, snapped to the row's grid, at the insert velocity,
  and auditions it. A click that only clears a selection adds nothing, as in the piano roll.
- Drag from empty space: paints along that row only, adding a hit on every step of the row's
  grid the pointer crosses that has no hit yet. One undo step per stroke.
- Right-drag: erases every hit on that row the pointer crosses. Right-click on a hit deletes
  it.
- Double-click on a hit deletes it, as in the piano roll.
- Drag a hit: moves the selection in time and across rows; Alt-drag copies. Time snaps to
  the grid of the row each hit lands on.
- Ctrl-drag from empty space draws a selection box; Ctrl+Shift adds to the selection.
  Shift-click toggles a hit in the selection.

### Keyboard

As the piano roll, with drum meanings: Up and Down move the selection a row, Left and Right
nudge it by a step of each hit's row grid, Delete removes, Ctrl+A, Ctrl+C, Ctrl+X and Ctrl+V
work on hits (pasting at the playhead on the same rows), Q quantizes, Shift+Q quantizes
iteratively, Escape clears the selection.

### Velocity

- The toolbar's Velocity field sets the selected hits' velocity, and when nothing is
  selected it is the insert velocity for new hits.
- The velocity lane shows a bar per hit of the focused row, named in the lane's label, so
  hits on other rows at the same tick do not stack on top of each other. Dragging across the
  lane sets the bars under the pointer, as the piano roll's lane does. Selecting hits focuses
  their row.

### Quantizing

- **Per-row grid.** `DrumRow` gains `grid: Option<u32>` in ticks; `None` follows the
  toolbar grid and `Some(0)` is Off. The lists offer note values only (no Bar or Beat), plus
  Off for rows; the toolbar grid has no Off. An Off row is left alone by Quantize, Iterative
  Quantize and Auto Quantize, clicks and drags on it are free, and painting on it uses the
  toolbar grid. The row grid is a separate setting from the step sequencer's row rate; when a
  step pattern is turned into hits, each row without a grid takes its pattern rate. Quantize, the click and paint snap, drag snap and nudging use the grid of the
  row the hit is on.
- **Quantize (Q).** The existing quantize with strength, swing and grooves, applied with
  each hit's row grid. Grooves keep their own timing and ignore row grids.
- **Iterative Quantize (Shift+Q).** Moves hits a set share of the way to their grid, 50% by
  default, set in the Quantize menu.
- **Humanise.** Moves the selected hits (or all hits) by a random amount up to a set number
  of milliseconds either way, and varies velocity by up to a set percentage. Both are set in
  its menu; one undo step.
- **Auto Quantize.** A switch in the Quantize menu, saved per track. When on, notes recorded
  on that track are quantized to their row grids, with the track's saved strength and swing,
  as the take is stored. Grooves are not used. Every take of a cycle recording is quantized.
  The quantize is its own undo step after the recording, so one undo gets the played timing
  back. The switch only affects new recordings.

### Model changes

- `DrumRow.grid: Option<u32>`.
- `Track.drum_quantize`: the toolbar grid, strength, swing, iterative share, humanise
  amounts and the Auto Quantize switch, saved per track and skipped when at their defaults.
  `freeze_key` clears it, since none of it changes the sound of existing notes.
- Pure functions in `midi_tools` for per-row quantize, iterative quantize, humanise (with a
  seed so tests are repeatable) and the paint and erase strokes, each unit-tested.

## Calls made in this design, not agreed with Tom

- Painting is a plain drag from empty space; the selection box moved to Ctrl-drag.
- Right-drag erases along a row.
- The velocity lane follows the focused row.
- Strength, swing, the iterative share and humanise amounts are saved per track, so Auto
  Quantize has settings to use.
- New hits are one step of their row's grid long.
- Grooves ignore row grids, stay a per-session choice as in the piano roll, and are not used
  by Auto Quantize.
- A row grid can be Off; Bar and Beat are not offered.
- Row grids are separate from the step sequencer's row rates, seeded from them when a pattern
  is turned into hits.
- Step pattern regions on tracks that are not drum tracks stay in the step sequencer.
- Auto Quantize applies to every take and is a separate undo step from the recording.

## Left to the builder

- Module names and how the shared row header is split out of `drum_editor.rs`.
- Which note values the grid lists offer, and the default humanise amounts.
- How the toolbar's menus are laid out, within the existing piano roll style.
- Test coverage beyond the pure functions above, and how the editor is checked on screen.
