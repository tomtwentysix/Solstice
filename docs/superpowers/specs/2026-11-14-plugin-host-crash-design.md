# Surviving a crashed plug-in host

Status: approved 2026-11-14.

## Intent

Windows plug-ins run through yabridge. Their Windows half runs in a `yabridge-host` process,
but yabridge's Linux half runs inside Solstice and waits for each reply with no time limit.
On 2026-11-14 the host for Ozone 12 Maximizer died. Closing its editor then called
`IPlugView::removed()` through yabridge, which waited forever for a reply, and the window froze
until Solstice was killed. The audio thread kept running. Solstice never noticed that the
host had exited: it was left as a zombie child of Solstice.

When a plug-in host dies, Solstice should notice straight away, stop calling into that plug-in,
and carry on, so the session can continue and be saved.

## Goals

- Notice a `yabridge-host` exit within moments, without polling.
- From then on, make no call into that plug-in. This covers audio processing, parameters,
  state, editors and closing the instance.
- The slot passes its signal through untouched, as if bypassed, until it is reloaded.
- The slot shows that the plug-in crashed and offers Reload. Reload opens a fresh instance from
  the last state Solstice had for it.
- Saving and autosaving keep that last state, so the project does not lose the plug-in's
  settings.

## Non-goals

- Linux VST3, CLAP and LV2 plug-ins that crash inside Solstice's own process. Containing those
  needs plug-ins run out of process, which is a separate and much larger project.
- A host that hangs without exiting. Solstice cannot tell this apart from a slow reply.
- Calls that are already inside yabridge when the host dies (see Known gap).
- Reloading crashed plug-ins automatically.

## Anti-goals

- A crash that changes the mix without the user knowing. A crashed slot must be visible
  wherever the slot is shown, and an export must not quietly leave it out.
- Losing the plug-in's settings. A crashed slot's state is kept and saved, never replaced by an
  empty one.

## Constraints

- Every call from the UI goes through `Host` in `crates/app/src/host.rs`. Every VST3 call goes
  through `solstice_vst3::Instance`, and every CLAP call through `solstice_clap::Instance`.
  yabridge bridges both formats.
- The audio thread calls `Processor::process`. Any check it makes must be realtime-safe: an
  atomic load, with no locks and no allocation.
- yabridge starts `yabridge-host` as a child of Solstice during `sv_open` (or the CLAP
  equivalent), and connects to it before the open returns. In yabridge's groups mode, several
  plug-ins share one host.
- Linux 5.3 or later, for `pidfd_open`. Fedora 45 runs 7.x.
- Code style follows the developer's global instructions.

## Approaches

1. **Watch the host process** (chosen). Compare Solstice's child processes before and after
   each open to see which host belongs to which plug-in. A watcher thread waits on a `pidfd`
   for each host, marks its plug-ins dead the moment it exits, reaps it, and tells the main
   loop. It costs nothing while plug-ins run, and it does not depend on the main loop, so the
   flag is set even while the window is busy.
2. **Poll `/proc` every half second.** This uses the same matching. It is simpler, but leaves
   up to half a second in which a call can still go into a dead host, and it does a little
   work all the time.
3. **Probe yabridge's sockets** for a closed connection. This relies on yabridge's internals:
   which descriptors belong to which plug-in, and how its sockets are named. Those change
   between versions.

The developer chose 1.

## Design decided

### Detection

- Before an instance opens, Solstice lists its child processes. Afterwards it lists them again,
  and any new `yabridge-host` child belongs to that instance.
- If no new child appears but the plug-in is bridged, a group host already running may serve
  it. In that case it attaches to the running host whose group matches. (Builder's call:
  matching by the host's command line; if no match can be made, the instance is not watched,
  which is the behaviour today.)
- One watcher thread holds a `pidfd` for each watched host and waits on all of them. When one
  becomes readable, the thread:
  - sets a `dead` atomic on every instance served by that host;
  - reaps the host with `waitid(P_PIDFD, …)`;
  - wakes the main loop to update the UI.
- Instances with no host (Linux plug-ins and built-ins) are never marked dead.

### A dead instance

- (My call, from the review.) A crashed slot goes through the engine's existing bypass path.
  `PluginCell::is_bypassed()` reports true when the user switched the slot off or its plug-in
  has crashed, so latency, skipping the insert and stale-audio handling behave as they do for a
  switched-off slot. An instrument slot therefore goes silent, as a switched-off instrument does
  today. The main loop rebuilds the graph when it is told of the crash, as it does when bypass is
  toggled.
- `PluginSlot.bypass` is never written by a crash, so the saved project and Reload keep the
  user's own On/Off choice.
- `Processor::process` also checks the atomic, as a backstop for the blocks before the graph is
  rebuilt. When it is set, the processor returns without calling the shim and leaves the buffers
  untouched.
- Every `Instance` method checks the atomic before calling the shim. When it is set, the method
  returns a neutral value instead:
  - latency 0;
  - no editor;
  - an empty parameter list.
  `close_editor` does nothing.
- Dropping a dead instance does not call `sv_close`. The instance is leaked on purpose, because
  closing it would wait for the dead host.
- Reported latency drops to 0, so delay compensation updates as it does for a bypassed slot.
- `state()` on a dead instance returns the last state Solstice had for it. That is the state
  read at the last capture, or the state it was loaded with, whichever is newer.
  (Builder's call: where that copy lives, in the `Instance` or the slot.)
- (My call, from the review.) Changes made in a plug-in's own editor do not always mark the
  project dirty, so autosave alone could leave the kept state a whole session old. Every watched
  instance therefore has its state captured into memory:
  - when its editor closes;
  - on the autosave timer, whether or not the project is dirty.
  These captures are not written to disk. At most one autosave interval of changes is lost to a
  crash.

### What the user sees

- The slot's name shows that the plug-in crashed, wherever slots are listed. Its On/Off button
  shows it as off.
- An open editor window for that plug-in closes itself, without calling into the plug-in.
- A toast or banner names the plug-in that crashed and offers Reload. The slot's own menu offers
  Reload as well.
- Reload opens a new instance of the same class, restores the kept state, and swaps it in, in
  the same way as replacing a plug-in. The swap is not an undo step.

### Exports and freezes

- (My call.) An export or freeze refuses to start while any slot in the project has crashed. It
  names the crashed slots and offers Reload. A render with a slot passing dry audio would ship
  a mix different from the one the user set up.
- (My call, from the review.) If a plug-in the render uses crashes while an export or freeze is
  running, the job stops. Its output is discarded, and no frozen audio is recorded. The message
  names the plug-in and offers Reload.

### Known gap

- If the host dies while a call is already inside yabridge, that call still waits forever,
  because Solstice cannot interrupt it. This window is milliseconds, rather than the rest of the
  session as it is today.

## Left to the builder

- Which crate the watcher lives in. It is shared by the VST3 and CLAP instances, so a small
  module both can use, or the app passing the dead flag in.
- How a yabridge host is recognised among Solstice's children: by its executable name
  `yabridge-host.exe` (or its `.so` / `-32` forms) and the Solstice process ID that yabridge
  passes as its last argument.
- Tests:
  - the matching of children before and after an open, using fake process lists;
  - that a dead `Processor` leaves its buffers unchanged;
  - that a dead instance's `state()` returns the kept state and its drop does not reach the shim;
  - the watcher, against a child process the test spawns and kills.
