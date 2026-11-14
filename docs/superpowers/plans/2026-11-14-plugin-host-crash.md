# Plug-in Host Crash Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a yabridge host process dies, Solstice notices at once, stops calling into
the plug-ins it served, passes their slots' audio through, keeps their last state, and offers
Reload.

**Architecture:** A watcher thread in `solstice-vst3` holds a `pidfd` for each `yabridge-host`
child and sets a shared `dead` flag when one exits. The VST3 and CLAP `Instance`s check that flag
before every call into the plug-in, and return their kept state when it is set. The app's
per-frame `poll` copies the flag into `PluginCell::crashed`, which the engine treats as bypass,
and then shows the crash and offers Reload.

**Tech Stack:** Rust, libc (`pidfd_open`, `poll`, `waitid(P_PIDFD)`), GTK4 and libadwaita.

**Spec:** `docs/superpowers/specs/2026-11-14-plugin-host-crash-design.md`

## Global Constraints

- Code style follows `/home/tom/.claude/CLAUDE.md`:
  - lean code, with no banner comments and no docstrings that restate the name;
  - comments only for what the code can't show.
- The audio thread may do only atomic loads for this feature, with no locks and no allocation.
  `Processor::process` and `PluginCell::is_bypassed` run on it.
- A dead instance never calls the shim or the CLAP plug-in again, and that includes on drop.
- A crash never writes `PluginSlot.bypass`, and never adds an undo step.
- UI copy:
  - the toast says `<slot name> crashed`, with a `Reload` button;
  - the slot tooltip says `<slot name> crashed. Right-click to reload.`;
  - the slot menu item is `Reload`;
  - the export refusal says `<names> crashed. Reload them before exporting.`;
  - the mid-render message says `<name> crashed during the render, so nothing was written.`.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Tests: `cargo test -p <crate>`. Before each commit, build the whole workspace with
  `cargo build` and `cargo clippy --workspace --all-targets`, with no new warnings.

## Review Focus

1. **Another child starts while a plug-in opens.** `solstice-scan` runs in parallel. Only
   children recognised as yabridge hosts are ever matched. Task 1 has a test.
2. **The host dies before the watch is registered.** `pidfd_open` then fails with `ESRCH`, or
   returns a pidfd that is readable at once. Either way the instance is marked dead straight away.
   Task 1 has a test.
3. **A crashed plug-in is removed, replaced by undo, or the project is closed.** Dropping a dead
   instance must not reach the plug-in. Tasks 2 and 3 have tests.
4. **Group hosts.** Several plug-ins served by one host are all marked dead, and a host already
   being watched is not watched twice. Task 1 has a test.
5. **The autosave timer runs while nothing has changed.** The kept state still refreshes, but no
   autosave file is written. Task 5 has a test.

---

### Task 1: Host watcher

**Files:**
- Create: `crates/vst3/src/hostwatch.rs`
- Modify: `crates/vst3/src/lib.rs` (`pub mod hostwatch;`)
- Modify: `crates/vst3/Cargo.toml` (add `libc.workspace = true`)

**Interfaces:**
- Produces:
  - `pub struct Child { pub pid: i32, pub args: Vec<String> }`
  - `pub fn children() -> Vec<Child>`: Solstice's direct children, read from
    `/proc/self/task/*/children` and each child's `/proc/<pid>/cmdline`.
  - `pub fn is_bridge_host(c: &Child) -> bool`: true when the file name of `args[0]` starts with
    `yabridge-host` or `yabridge-group`.
  - `pub fn new_hosts(before: &[Child], after: &[Child]) -> Vec<i32>`: bridge hosts in `after`
    whose pid is not in `before`.
  - `pub fn watch(pid: i32, dead: Arc<AtomicBool>)`: registers the flag with the watcher thread,
    starting the thread on first use. A pid already watched gets the flag added to its list.
  - `pub fn opening<T>(open: impl FnOnce() -> T) -> (T, Vec<i32>)`: runs `open` between two
    `children()` snapshots and returns `new_hosts`.

- [ ] **Step 1: Write the failing tests** in `hostwatch.rs`:
  - `new_hosts_picks_only_new_bridge_hosts`:
    - `before` holds pid 10, a `yabridge-host.exe.so`;
    - `after` holds pid 10, pid 11 (a `/usr/bin/solstice-scan`) and pid 12 (a
      `/home/u/.local/share/yabridge/yabridge-host.exe.so`);
    - expect `vec![12]`.
  - `group_hosts_are_bridge_hosts`: `is_bridge_host` is true for
    `/x/yabridge-group.exe.so`, and false for `/usr/bin/wine`.
  - `watched_child_exit_sets_the_flag_and_is_reaped`:
    - spawn `sleep 30` with `std::process::Command`, then `watch` its pid with two flags,
      as a group;
    - kill it with `libc::kill(pid, SIGKILL)`;
    - within 2 s, both flags are true and `/proc/<pid>` no longer exists.
  - `watching_a_dead_pid_sets_the_flag_at_once`:
    - spawn `true`, wait for it to exit with `libc::waitpid`, then `watch` its pid;
    - the flag is true within 2 s.

- [ ] **Step 2:** Run `cargo test -p solstice-vst3 hostwatch`. Expected: it fails to compile,
  because the module doesn't exist yet.

- [ ] **Step 3: Implement.** One thread, named `solstice-hostwatch`:
  - it `poll`s every pidfd, plus a pipe that `watch` writes to after adding an entry under a
    `Mutex<Vec<(i32 pidfd, i32 pid, Vec<Arc<AtomicBool>>)>>`;
  - on `POLLIN` from a pidfd, it stores `true` in every flag, calls
    `waitid(P_PIDFD, fd, WEXITED)`, closes the fd and drops the entry;
  - if `pidfd_open` fails with `ESRCH`, `watch` stores `true` at once.

  `waitid` reaps only a child of this process. yabridge does not reap its hosts, which is why the
  host stayed a zombie.

- [ ] **Step 4:** Run `cargo test -p solstice-vst3 hostwatch`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message `Watch yabridge host processes for exit`.

### Task 2: VST3 instances survive a dead host

**Files:**
- Modify: `crates/vst3/src/lib.rs`

**Interfaces:**
- Consumes: `hostwatch::opening` and `hostwatch::watch` from Task 1.
- Produces, on `Instance`:
  - `pub fn crashed(&self) -> bool`;
  - `pub fn bridged(&self) -> bool`: true when at least one host was matched at open;
  - `#[cfg(test)] pub(crate) fn dead_for_test(kept: Vec<u8>) -> Arc<Instance>`: has a null
    `ptr`, `dead` already true, and `bridged` true.

- [ ] **Step 1: Write the failing tests:**
  - `dead_instance_returns_kept_state`:
    - `state()` equals the kept bytes;
    - `latency()` is 0, `has_editor()` is false and `params()` is empty;
    - `close_editor()` and `set_param` return without touching the null pointer.
  - `dead_processor_leaves_buffers_alone`: build the `processor()` of a dead instance and run
    `process` on buffers filled with 0.5. They are still 0.5.
  - `dropping_a_dead_instance_does_not_close_it`: drop the last `Arc`. A null `sv_close` would
    crash, so this test passes simply by not crashing.

- [ ] **Step 2:** Run `cargo test -p solstice-vst3`. Expected: it fails to compile.

- [ ] **Step 3: Implement.**
  - Add `dead: Arc<AtomicBool>`, `bridged: bool` and `kept: Mutex<Vec<u8>>` to `Instance`.
  - `open` wraps `sv_open` in `hostwatch::opening`, and then calls `watch` for each new host.
    A plug-in opened into a yabridge group host that was already running matches no new host
    and is not watched, which the spec allows. Leave `bridged` false for it.
  - Every method that calls the shim first returns its neutral value when `dead` is set:
    - 0 or false;
    - an empty `Vec` or `String`;
    - `(0, 0)` for `editor_size`;
    - nothing at all for the setters.
  - `state()`:
    - when the instance is live, it stores a non-empty result in `kept`;
    - when it is dead, it returns a clone of `kept`.
  - `set_state` stores the data in `kept` when it succeeds.
  - `Drop` skips `sv_close` when `dead` is set.
  - `Processor::process` returns at the top when `inst.dead` is set. Use a `Relaxed` load.

- [ ] **Step 4:** Run `cargo test -p solstice-vst3`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message `Stop calling into VST3 plug-ins whose host has died`.

### Task 3: CLAP instances survive a dead host

**Files:**
- Modify: `crates/clap/src/lib.rs`

**Interfaces:**
- Consumes: `solstice_vst3::hostwatch` from Task 1.
- Produces, on `solstice_clap::Instance`: `pub fn crashed(&self) -> bool` and
  `pub fn bridged(&self) -> bool`.

- [ ] **Step 1: Write the failing tests:**
  - a dead processor leaves its buffers unchanged;
  - a dead instance's `state()` returns its kept state.

  If building an `Instance` without a plug-in needs too much scaffolding, test `Handle` and
  `Processor` directly, and note that in the report.

- [ ] **Step 2:** Run `cargo test -p solstice-clap`. Expected: the tests fail.

- [ ] **Step 3: Implement**, as in Task 2:
  - the flag lives in `Handle`, so `Processor` and `Instance` share it;
  - `Handle::drop` skips destroying the plug-in when the flag is set;
  - `close_editor` on a dead instance only drops the X11 window, through
    `solstice_vst3::x11::destroy`;
  - `idle` does nothing when the instance is dead;
  - `open` wraps the factory's create call in `hostwatch::opening`.

- [ ] **Step 4:** Run `cargo test -p solstice-clap`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message `Stop calling into CLAP plug-ins whose host has died`.

### Task 4: The engine treats a crashed cell as bypassed

**Files:**
- Modify: `crates/engine/src/lib.rs` (`PluginCell`)
- Modify: `crates/app/src/host.rs`
- Test: `crates/engine/src/tests.rs`

**Interfaces:**
- Produces:
  - `PluginCell { pub crashed: AtomicBool, .. }`;
  - `is_bypassed()` returns `bypass || crashed`;
  - `Host::crashed(&self) -> bool` and `Host::bridged(&self) -> bool`, which are false for
    `Lv2` and `Native`.

- [ ] **Step 1: Write the failing test** `crashed_insert_passes_audio_and_drops_latency`:
  - a channel holds an insert that multiplies by 0 and reports latency 64;
  - set `crashed`, compile, and process;
  - the output equals the input, and the channel's latency is 0.

  Copy the setup from the existing bypass tests in `tests.rs`.

- [ ] **Step 2:** Run `cargo test -p solstice-engine crashed`. Expected: it fails.

- [ ] **Step 3: Implement** the field (initialised to false in `PluginCell::new`), the
  `is_bypassed` change, and the two `Host` methods.

- [ ] **Step 4:** Run `cargo test -p solstice-engine`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message `Treat a crashed plug-in's cell as bypassed`.

### Task 5: The studio notices crashes, keeps state and reloads

**Files:**
- Modify: `crates/app/src/studio.rs`
- Modify: `crates/app/src/plugins.rs` (the close request at the editor window, around line 781)

**Interfaces:**
- Consumes: `Host::crashed`, `Host::bridged`, `PluginCell::crashed`, and Task 2's
  `dead_for_test` (expose it to the app's tests through a `test-util` feature on `solstice-vst3`,
  enabled in the app's `[dev-dependencies]`).
- Produces:
  - `Change::PluginCrashed`;
  - `Studio::crashes: Vec<Id>`, the slots newly crashed and not yet shown, which the window
    drains;
  - `pub fn crashed_slots(&self) -> Vec<Id>`;
  - `pub fn reload_plugin(&mut self, sid: Id)`.

- [ ] **Step 1: Write the failing tests** in the `studio.rs` tests:
  - `poll_marks_a_crashed_plugin`:
    - insert a `Loaded` built on `Host::Vst3(Instance::dead_for_test(b"kept".to_vec()))` for a
      slot whose state is `b"old"`;
    - after the crash check runs, the cell's `crashed` is true;
    - the slot's state is `b"kept"`, the slot's `bypass` is unchanged, and the undo history is
      unchanged;
    - `crashes == vec![sid]`.
  - `reload_drops_the_dead_instance`:
    - after `reload_plugin(sid)`, the old `Loaded` is gone;
    - a reopen was attempted from the slot's state: the slot is either loaded again or recorded
      in `plugin_errors`, and is no longer marked crashed.
  - `autosave_refreshes_kept_state_when_clean`:
    - with a clean history, `autosave()` calls `state()` on bridged instances, which is visible
      through the kept state of a live test double, or through a counter if one is easier;
    - no autosave file is written.

- [ ] **Step 2:** Run `cargo test -p solstice studio`. Expected: the tests fail.

- [ ] **Step 3: Implement.**
  - Split the crash check out of `poll` into `fn check_crashes(&mut self) -> bool`. For each
    `Loaded` whose `inst.crashed()` is true and whose `cell.crashed` is still false, it:
    - stores `true` in `cell.crashed`;
    - writes `inst.state()` into the slot with `apply_states` and sets `applied`;
    - pushes the slot to `crashes`.

    When anything changed, `poll` calls `rebuild()` and emits `Change::PluginCrashed`.
  - `autosave()`: before the dirty check, call `state()` on every `Loaded` whose `inst.bridged()`
    is true and that has not crashed. This only refreshes the kept copies.
  - In the editor window's close request, call `inst.state()` before `inst.close_editor()` when
    `inst.bridged()` is true.
  - `reload_plugin`:
    - removes the `Loaded` (dropping a dead instance is safe after Task 2);
    - removes any `plugin_errors` entry;
    - re-runs the plug-in sync and `rebuild()`, which opens the slot again from its state;
    - emits `Change::Project`.

- [ ] **Step 4:** Run `cargo test -p solstice`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message
  `Notice crashed plug-in hosts and keep their state`.

### Task 6: Show the crash and offer Reload

**Files:**
- Modify: `crates/app/src/mixer.rs` (`insert_slot` around line 1636, and `slot_menu`)
- Modify: `crates/app/src/plugins.rs` (the editor window's `ctx.on` handler, around line 786)
- Modify: `crates/app/src/window.rs` (the toast handler, around line 272, and a new
  `reload-plugin` action that takes a `u64`)
- Also update any other place that lists slots, such as the instrument slot and the MIDI FX
  slots. Find them with `grep -n "plugin_errors" crates/app/src`.

**Interfaces:**
- Consumes: `Studio::crashes`, `crashed_slots`, `reload_plugin` and `Change::PluginCrashed`.

- [ ] **Step 1: Implement:**
  - A crashed slot gets the `error` CSS class and the crash tooltip (both in Global
    Constraints), whatever its `bypass` is.
  - `slot_menu` puts `Reload` first for a crashed slot. It activates `win.reload-plugin` with
    the slot's id.
  - On `Change::PluginCrashed`, the window drains `crashes`. For each, it adds a toast with the
    crash message and a `Reload` button bound to `win.reload-plugin`.
  - The editor window's change handler also closes the window when its instance has crashed.
    The close request then calls only no-op methods, because of Task 2.
  - Mixer and track views redraw on `Change::PluginCrashed`, as they do on `Change::Project`.

- [ ] **Step 2:** Run `cargo build` and `cargo clippy --workspace --all-targets`. Expected:
  both are clean.

- [ ] **Step 3: Check by hand** in a dev instance. Use a private `XDG_RUNTIME_DIR` if it runs
  under a nested session.
  - Load a Windows plug-in and open its editor, then `kill -9` its `yabridge-host`.
  - Expected:
    - the editor closes;
    - the toast appears;
    - the slot turns red and the audio passes through;
    - Reload brings the plug-in back with its settings;
    - the window never freezes.

- [ ] **Step 4: Commit**, with the message `Show crashed plug-ins and offer Reload`.

### Task 7: Exports and freezes respect crashes

**Files:**
- Modify: `crates/app/src/render.rs` (`queue_render`, `poll_render` and `finish_job`)

**Interfaces:**
- Consumes: `crashed_slots`, `PluginCell::crashed`, and `Running.plugins`.

- [ ] **Step 1: Write the failing tests** in `render.rs`:
  - `export_refuses_with_a_crashed_slot`:
    - with a `dead_for_test` plug-in loaded and crashed, queuing an export queues nothing;
    - `render_errors` holds the refusal message;
    - a freeze is refused the same way.
  - `crash_during_render_discards_the_output`:
    - mark a cell in `Running.plugins` crashed while a job runs, then `poll_render`;
    - the job ends cancelled, and no file exists at its path;
    - no frozen audio is recorded;
    - `render_errors` holds the mid-render message.

- [ ] **Step 2:** Run `cargo test -p solstice render`. Expected: the tests fail.

- [ ] **Step 3: Implement.**
  - `queue_render` checks `crashed_slots()` first. If any exist, it pushes the refusal message,
    emits `Change::Render`, and returns.
  - `poll_render`: while `Running`, if any cell in `r.plugins` has `crashed`, it sets
    `progress.cancel`, and records the crashed slot's name for `finish_job`. To get the name,
    match the cell against `self.plugins` by `Arc::ptr_eq`.
  - On a cancel caused by a crash, `finish_job` pushes the mid-render message and removes the
    partial file, if the cancel path does not already do so.

- [ ] **Step 4:** Run `cargo test -p solstice`. Expected: all pass.

- [ ] **Step 5: Commit**, with the message `Refuse or stop renders when a plug-in has crashed`.
