# Production stability

Status: approved for overnight work, 2026-11-14 (Tom: "stable enough for production
environments where data loss, or interrupting a take must absolutely be avoided"; work
autonomously and release as needed).

## Intent

Five read-only audits on 2026-11-14 (recording, persistence, audio thread, main thread, audio
server) found ways a take or a project can be lost, or a session interrupted, beyond the yabridge
host crash fixed in 0.7.5. This document ranks them and sets the order of work. Each release fixes
one group, test first, and nothing in a group ships half done.

## Ranking

The first question is whether a take or saved work can be lost without the user knowing. The second is whether a session stops or freezes. The third is whether an edge case misbehaves.

### Reported by Tom during the night

- **U1. Clicks in Windows plug-in editors land in the wrong place, sometimes.** Found: when the
  plug-in window moves, yabridge loses the editor's offset inside the window (48 pixels under the
  header). Fixed in 0.7.7: Solstice announces the editor's screen position the way a window
  manager does.
- **U2. Ozone causes overruns.** Measured: Ozone itself takes about 0.1 ms per 128-frame block
  (worst about 1 ms). The cause found was Solstice's worker threads running at priority 10, below
  interrupt threads, and yabridge giving the plug-ins they call the same priority. Fixed in 0.7.7.

### 0.7.6: Recording safety (released)

- **R1. Ring overruns.** When the record ring overflows, the audio thread drops the block but still counts it. The take then never finalises, and the next take starts late.
  - Count only the frames that were pushed, and have the disk side write silence for the dropped frames.
  - Enlarge the ring to 30 s.
  - Show overruns.
- **R2. Write errors.**
  - On a full disk or a write error, stop that writer at its first error.
  - Finalise what reached the disk, deliver it as a take, and tell the user.
  - Open take files with `create_new`, so an earlier take is never overwritten.
- **R3. Mid-take actions.** These must not end a take silently:
  - stopping audio, changing interface or rate, or switching the project window;
  - closing a window or opening a project;
  - Save As;
  - a render or freeze;
  - undo or redo.

  Refuse them while recording, with a message. `stop_audio` also stops, drains and delivers takes in progress before the recorder goes.
- **R4. Crash mid-take.** Flush each take's WAV header about once a second, so a killed process leaves a playable file. When a project opens, offer audio files that no region uses and that are newer than the last save, as recovered takes.
- **R5. Channel count.** Changing a track's input between mono and stereo during a take restarts the take rather than scrambling the file.
- **S1. Audio server shutdown.**
  - Add a JACK `shutdown` handler that sets a flag.
  - Poll the flag: finalise the takes, show the banner, and reconnect with backoff.
  - When the server isn't running at launch, retry automatically.

### 0.7.7: Audio engine safety (released)

- **E1. Worker pool race.** A worker can run an item of the next job. Fix it with a job base index.
- **E2. Panics.** Catch panics around each period and each strip. A panic silences the period and flags the plug-in, and the process never aborts.
- **E3. Bad samples.** Replace non-finite samples on strip outputs, and hard-clip the hardware outputs. Meters show NaN as an alarm.
- **E4. Denormals.** Set FTZ and DAZ in each period, on the audio thread and the workers.
- **E5. Large periods.** Process periods longer than `MAX_BLOCK` in chunks.
- **E6. Hung bridged plug-ins.** Add a watchdog for a bridged plug-in whose process call hangs while its host stays alive: past a limit, kill the host, so the existing crash handling takes over.

### 0.7.8: Project safety (released)

- **P1. Saves.**
  - fsync the temp file and the folder, and keep `project.json.bak`.
  - Loading falls back to the backup or autosave, and the startup error is shown rather than opening Untitled silently.
- **P2. VST3 dirty tracking.** VST3 edits mark the project dirty, through `performEdit` and `restartComponent` in the shim's component handler. Today a tone dialled in a plug-in window is lost on close without a prompt.
- **P3. Settings.** Never save over a settings file that failed to parse, and write it atomically.
- **P4. Recovery prompt.**
  - Escape on the recovery prompt keeps the autosave.
  - Discard renames the autosave rather than deleting it.
  - "Don't Save" clears the autosave.
- **P5. Autosave failures.** Show a lasting notice when autosave fails.
- **P6. Lock file.** Hold a lock file in the bundle, so the same project cannot be open twice.
- **M1. Panics on the main thread.** A panic hook writes an emergency autosave.
- **M2. Damaged WAV headers.** Bounds-check WAV headers.
- **M3. X11 errors.** Install non-fatal X11 error handlers.
- **M4. NAM captures.** Add `catch (...)` to every NAM shim entry, and check weight counts before building a model.
- **M5. Crash loops.** Write a marker around in-process plug-in loads, so a plug-in that crashed while loading is skipped at the next open instead of crashing again.

### Done after 0.7.8

- The VST3 shim no longer allocates on the audio thread for dense parameter changes.
- Graph swaps exchange delay buffers instead of copying them on the audio thread.
- Takes past 4 GB continue in a new file, landing as consecutive regions.
- Housekeeping and read-ahead keep running while the window is hidden.
- X11 errors from plug-in windows no longer end the process.

### Later

- Stream audio from disk instead of memory-mapping it on the audio thread. Read-ahead now runs
  on a timer and from where playback starts, but a cold cache can still fault.
- Converting files at another sample rate, and reconnecting at a new rate, on a worker thread
  rather than the main thread. Neither can happen during a take.
- Restoring LV2 state without waiting for that plug-in's own worker job; it only matters for
  heavy LV2 sample loaders.
- A lock-free URID map for LV2 plug-ins that map URIs from `run`.
- A per-take sequence number in the record ring, to make the cycle-boundary handling
  independent of event timing. The queue of takes per track already covers the case the
  audit found.

## Constraints

- Code style follows Tom's global instructions.
- Each fix starts with a failing test where the behaviour can be reached from a test. Where it can't, the release notes say how it was checked.
- The audio thread gains only atomics and preallocated buffers.
