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

### 0.7.8: Project safety

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

### Later

- Stream audio from disk instead of memory-mapping it on the audio thread.
- Takes over 4 GiB, using RF64 or rolling to a new file.
- Unplugging and replugging the interface.
- A sample-rate change under Solstice.
- The cycle-boundary race in the recorder.
- Moving `reconnect_at` off the main thread.
- The LV2 worker lock.
- `conform` off the main thread.
- A per-instance LV2 URID map.

## Constraints

- Code style follows Tom's global instructions.
- Each fix starts with a failing test where the behaviour can be reached from a test. Where it can't, the release notes say how it was checked.
- The audio thread gains only atomics and preallocated buffers.
