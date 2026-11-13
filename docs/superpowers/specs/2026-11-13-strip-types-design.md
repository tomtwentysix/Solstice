# Strip types: sectioned channel strips for guitar and bass

Status: draft for approval, 2026-11-13.

## Intent

A plain audio track looks the same whatever is recorded on it. A musician who is not an
audio engineer thinks of a guitar sound as pedals, an amp and a cabinet, not as a list of
insert slots. A strip type is an optional setting on a channel that redraws its inserts as
named sections matching that signal chain, and Solstice ships enough to fill those sections
without installing anything.

## Goals

- An optional strip type per channel. Two types to start: Guitar DI and Bass DI, each with
  the sections Pedals, Amp, Cabinet, Effects.
- The Amp section offers only plug-ins categorised as amps, and the Cabinet section only
  cabinets. Pedals and Effects accept any plug-in.
- Solstice keeps its own plug-in categories, which also drive a Group by control in the
  plug-in list (Type, Developer, Name).
- Neural Amp Modeler (NAM) built in as an effect, with a capture library, drag-and-drop
  import and a small starter set of captures.
- A built-in impulse response loader for the Cabinet section.
- Two built-in pedals: a tuner and an overdrive.
- Section labels above each section inside every strip; the label column at the side of the
  mixer is removed.
- Factory channel strip settings for buses and common sources.

## Non-goals

- Solstice's own amp modelling. NAM covers it.
- Searching TONE3000 from inside Solstice, and anything else that downloads. That belongs to
  the content store project, which follows this one. The sections gain a "Get more…" entry
  then.
- Bundling Ignite Amps or other closed freeware. They are recognised by the category table
  only.
- Sectioned strips for vocals, drums, keys or buses. Those get factory strip settings.
- A parallel clean/dirty split for bass. Bass DI is a straight chain.
- A user interface for defining new strip types.

## Anti-goals

- Any feature that only works on a typed strip. NAM, the IR loader, categories and Group by
  work everywhere.
- Setting or clearing a strip type changing the sound.
- An existing project looking different when opened, other than the labels moving from the
  side column into the strips.

## Constraints

- The engine tests fail on any allocation in the audio thread. NAM and the IR loader must
  pass them, including while a capture or impulse response is being swapped.
- Solstice is GPL-3.0-or-later. NeuralAmpModelerCore is reported as MIT and depends on Eigen
  (MPL-2.0). The builder must read both licence files in the upstream repositories before
  vendoring, and add both to the third-party credits.
- Only captures and impulse responses whose authors explicitly permit redistribution may
  ship. Captures on TONE3000 carry a "T3K" licence whose terms were not found, so they are
  not assumed redistributable.
- Software is credited to SolarFlare Developments.

## Approaches

**A. Sections are labels on the existing insert list (chosen by Tom).** The channel gains an
optional strip type and each insert an optional section. The engine still processes one
ordered list. Automation, freeze, delay compensation, drag and drop and saved strip settings
keep working because they already act on inserts. Clearing the type flattens the strip.

**B. A guitar rig effect holding the chain inside itself.** Any track gets a rig by inserting
one and a rig saves as one preset, but it is a plug-in host inside a plug-in: automation,
delay compensation and plug-in windows all need a second nested path, and the plug-ins
inside are hidden from the mixer.

**C. A separate insert list per section in the project file.** Dropped: every piece of code
that touches inserts changes, for nothing A does not already give.

Tom chose A because it meets the rules that a type is optional, only changes how the strip is
drawn, and gates nothing.

## Design

### Decided by Tom

- Approach A.
- Guitar DI and Bass DI only; anything without a very specific signal chain is a preset.
- Amp and Cabinet sections are strict about category.
- A built-in tuner and a built-in overdrive are part of this project.
- NAM is bundled; Solstice does not need its own amp. A starter set of captures ships with
  it. He has no amps of his own to capture.
- TONE3000 gets a link and an import flow now; search moves to the store project.
- Inserts on typed strips are taller and carry a monochrome, translucent picture of what
  they are, drawn as a large faint watermark that fills the insert and runs off its edge,
  with the strip colour showing through (mockup "B").
- Untyped strips keep today's short, name-only inserts, with a toggle to give them pictures
  too.
- Section labels move above each section on all strips and the side label column goes.
- The plug-in list gets Group by: Developer, Type, Name (first letter).

### My calls

**Naming.** "Strip Type" in the interface; `StripKind` in code. It is set from the strip's
Setting menu and the track's right-click menu, with "None" to clear it.

**Model.** `Channel` gains `kind: Option<StripKind>` and `PluginSlot` gains
`section: Option<Section>`, both omitted from the file when unset so old projects load and
save unchanged. A strip type is a list of sections; a section is a label and the categories
it accepts.

- Along `inserts`, section order never goes backwards. An insert with no section is a plain
  insert and stays where it sits between sections; this is how an EQ goes between amp and
  cabinet.
- Setting a type on a channel that already has inserts puts them all in the last section,
  Effects, in their existing order. Clearing a type removes the section from every insert.
  Neither reorders the list.
- Amp accepts a plug-in that has Amp among its categories, and Cabinet one that has Cabinet.
  Pedals and Effects accept anything, amps and cabinets included, so a NAM capture of a drive
  pedal can sit in Pedals and the IR loader can be a reverb in Effects. The strictness is
  about what Amp and Cabinet offer, not about where an amp may go.
- Adding to a section inserts after that section's last insert. Dragging into Amp or Cabinet
  is refused unless the category fits; dropping between sections makes a plain insert.
- Setting a type never adds or removes plug-ins.

**Categories.** A plug-in can have several. Sources, highest first: the user's own choice
(right-click a plug-in, "Category"), stored in settings; a table shipped in `data/` keyed by
plug-in ID with a vendor-and-name fallback; the plug-in's own subcategories as today.
The table adds Amp, Cabinet and Pedal to the existing groups. It ships with the built-ins,
the NAM family, Ignite Amps and the common open-source guitar plug-ins; the builder compiles
the list.

Built-in NAM carries Amp and Pedal; the built-in IR loader carries Cabinet and Reverb.

**Strip settings.** Saved strip settings, the factory ones, and copy and paste of a strip
carry the strip type and each insert's section. Loading one replaces the type along with the
inserts. `StripSetting` has no such fields today.

**Guitar DI and Bass DI** have the same sections and rules. They differ in the label, in the
picture set if the artwork warrants it, and in which folder of the capture list the NAM
panel opens on (captures are kept in Guitar and Bass folders).

**Plug-in list.** Group by Type (default, from the categories above), Developer or Name. The
choice is saved in settings.

**Strip drawing.** The one strip function serves the mixer and inspector. Every section gets
a small label above it. Strips no longer share row heights above the pan control; the pan,
fader, buttons and name stay aligned along the bottom and the part above scrolls when it does
not fit. Typed strips draw 40 px inserts with the watermark picture and a second line for a
capture, impulse response or preset name. Untyped strips draw today's 24 px inserts unless
"Pictures on all strips" is on in the mixer toolbar; that switch is saved in settings and
off by default. Pictures are one symbolic SVG per category.

**NAM effect.** A built-in effect, `solstice:nam`, category Amp, with a native panel: a list
of captures, input level and output level. The engine comes from NeuralAmpModelerCore,
vendored as a submodule and reached through a small C++ bridge in its own crate, as the VST3
host does.

- Captures load and are freed off the audio thread; the audio thread only swaps a pointer.
- The effect is mono: a stereo strip is summed in and the result goes to both channels.
- A capture made at a different sample rate from the session is resampled around the model.
- A used capture is copied into the project folder, as Sampler files are, so projects stay
  self-contained.

**Capture library.** A `captures` folder under Solstice's user data directory, respecting
`SOLSTICE_DEV`. Dropping a `.nam` file or a zip of them onto an Amp section or the NAM panel
copies it in and loads it. The panel also has Import… and an "Open TONE3000" link that opens
the browser.

**IR loader.** A built-in effect, `solstice:ir`, category Cabinet: convolution with a WAV
impulse response, resampled to the session rate on load, with level and mix. Same library,
import and project-copy behaviour as captures, in an `irs` folder.

**Tuner.** A built-in effect, `solstice:tuner`, category Pedal and Tuner: a chromatic tuner
with note name and cents on a native panel, passing audio through unchanged, with a mute
while tuning. This is the tuner already listed for 0.7 on the roadmap, not a second one.

**Overdrive.** A built-in effect, `solstice:overdrive`, category Pedal: drive, tone and
level, oversampled to keep aliasing down, with a native panel. The voicing is the builder's.

**Starter content.** A handful of captures (clean, crunch, high gain, one bass) and matching
cabinet impulse responses. The builder lists candidates with the evidence that each may be
redistributed; Tom approves the list before any ship. If none qualify, the sections open
empty with the import flow and the link.

**Factory strip settings.** Shipped read-only in `data/`, listed under "Factory" in the
Setting menu above the user's own, built from built-in effects only: Guitar Clean, Guitar
Crunch, Guitar High Gain and Bass (each sets its strip type and fills the sections from the
starter content), Lead Vocal, Kick, Snare, Overheads, Drum Bus, Guitar Bus, Mix Bus, Reverb
Send. The guitar and bass ones are named for the sound so they do not read as the strip types.

## Order of work

1. A throwaway test that NAM processes and swaps captures without allocating on the audio
   thread. The engine's check replaces the Rust global allocator and cannot see `malloc` or
   `operator new` in C++, so the test must intercept those as well. If the core allocates,
   a small patch kept in this repository and applied at build time is acceptable; if fixing
   it means rewriting the model code, stop and report before anything else is built.
2. Model changes, categories, Group by, section drawing and the label move.
3. NAM effect, capture library and import.
4. IR loader, tuner and overdrive.
5. Starter content, after approval.
6. Factory strip settings.

## Left to the builder

- The convolution method and its latency, the resampler, and the bridge's interface.
- Picture artwork, exact sizes and opacity, starting from the mockup in
  `.superpowers/brainstorm/650335-1794592243/content/strips-v3.html`.
- The contents of the category table and the values in the factory strip settings.
- Tests: section ordering rules in the model, both effects under the allocation check.

## Roadmap

Release 0.7, with cue mixes and pre-roll. It does not depend on LV2.
