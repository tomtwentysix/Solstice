# First-Run Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A five-page setup window, shown once on first launch and from the menu, that checks realtime priority, sets the interface and buffer size, the save folders, finds plug-ins, and checks and sets up Wine and yabridge.

**Architecture:** The checks are pure functions in two new modules (`system.rs` for realtime, Wine and yabridge; `plugin_search.rs` for folders), unit-tested without a desktop. `elevate.rs` runs the two root actions through `pkexec` and PackageKit. `setup.rs` builds the window from those, reusing the Interface and Buffer Size rows extracted from Preferences. `projects.rs` shows it before the chooser; `window.rs` adds the menu item and the realtime banner.

**Tech Stack:** Rust, GTK 4.20 (`gtk4` 0.11), libadwaita 1.8 (`libadwaita` 0.9), serde.

**Spec:** `docs/superpowers/specs/2026-11-13-first-run-setup-design.md`

## Global Constraints

- Code stays lean: no section banners, no doc comments restating names, comments only for what the code can't show (file formats, quirks, why a workaround exists).
- UI copy is formal and in the style of the existing Preferences text.
- Solstice never asks for a password itself. Root actions go through `pkexec` or PackageKit; on cancel, failure or no agent the page shows the command to run by hand.
- Solstice downloads nothing. yabridge is linked to, not installed.
- Every page can be passed with Next; nothing blocks reaching the project chooser.
- A tick appears only for something actually checked.
- Tests that touch settings must not write the real settings file (`Settings::save` asserts this in tests); use temp dirs.
- Known-good yabridge commits: `ba7022d`. Wine version from which yabridge 5.1.1 breaks pop-ups: 9.22. Branch link: `https://github.com/robbert-vdh/yabridge/tree/new-wine10-embedding`. Release link: `https://github.com/robbert-vdh/yabridge/releases`.
- `SETUP_VERSION = 1`.

## Review Focus

1. **A 0.7.1 settings file** with none of the new fields loads with every existing value intact and `setup_version` 0, so the setup shows once and changes nothing. Test in Task 1.
2. **`unlimited` in `/proc/self/limits`** (root, or a permissive PAM config) counts as allowed, not as a parse failure. Test in Task 2.
3. **Limits files with comments, `*` and per-user lines, and `soft`/`hard`/`-` types**: only `@group` lines with `-` or `soft` count, and the highest wins. Test in Task 2.
4. **yabridge present but its library unreadable or not where `yabridgectl` lives** (installed from a package to `/usr/lib`): the page reports it can't check, no tick and no crash. Test in Task 3.
5. **The authentication prompt cancelled** (`pkexec` exits 126) is reported as cancelled with the manual command, not as an error. Test in Task 4.

---

### Task 1: Save folder settings and setup version

**Files:**
- Modify: `crates/app/src/settings.rs` (struct, `Default`, tests)
- Modify: `crates/app/src/studio.rs:1713-1718` (`projects_dir`)
- Modify: `crates/app/src/export.rs:137,148` (initial folder for both dialogs)

**Interfaces:**
- Produces: `Settings { setup_version: u32, projects_folder: Option<PathBuf>, exports_folder: Option<PathBuf>, .. }`; `Settings::projects_dir(&self) -> PathBuf`; `Settings::exports_dir(&self) -> PathBuf`; `studio::projects_dir()` unchanged in signature, now `Settings::load().projects_dir()`.

- [ ] **Step 1: Write the failing tests** in `settings.rs`'s test module:

```rust
#[test]
fn old_settings_keep_their_values() {
    let old = r#"{"buffer_size": 128, "interface": "Midas UMC22", "vst3_paths": ["/opt/vst3"], "starter_installed": 1}"#;
    let s: Settings = serde_json::from_str(old).unwrap();
    assert_eq!(s.buffer_size, Some(128));
    assert_eq!(s.interface.as_deref(), Some("Midas UMC22"));
    assert_eq!(s.setup_version, 0);
    assert_eq!(s.projects_folder, None);
    assert_eq!(s.exports_folder, None);
}

#[test]
fn export_folder_follows_projects_folder() {
    let mut s = Settings::default();
    s.projects_folder = Some("/srv/audio/Projects".into());
    assert_eq!(s.projects_dir(), PathBuf::from("/srv/audio/Projects"));
    assert_eq!(s.exports_dir(), PathBuf::from("/srv/audio/Projects"));
    s.exports_folder = Some("/srv/audio/Mixes".into());
    assert_eq!(s.exports_dir(), PathBuf::from("/srv/audio/Mixes"));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p solstice settings::`
Expected: compile error, no field `setup_version`.

- [ ] **Step 3: Add the fields and methods.** `projects_dir` keeps today's rule when unset: the dev root's `projects` folder, else `~/Music/Solstice` (falling back to home). `exports_dir` is `exports_folder` or `projects_dir()`. Move the body of `studio::projects_dir` into `Settings::projects_dir` and make the free function call `Settings::load().projects_dir()`.

- [ ] **Step 4: Point both export dialogs at `ctx.s().settings.exports_dir()`** with `set_initial_folder`, as `window.rs:678` does for projects.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p solstice`
Expected: all pass.

- [ ] **Step 6: Commit** — `Add save folder settings and the setup version`

---

### Task 2: Realtime check and group lookup

**Files:**
- Create: `crates/app/src/system.rs` (add `mod system;` to `main.rs`)

**Interfaces:**
- Produces:
  - `pub enum RtLimit { Unlimited, Limit(u32) }`
  - `pub fn parse_rtprio(proc_limits: &str) -> Option<RtLimit>` — reads the soft value of the `Max realtime priority` line of `/proc/self/limits`.
  - `pub fn realtime_ok(limit: Option<RtLimit>) -> bool` — `Unlimited` or `Limit(n)` with n > 0.
  - `pub fn realtime_limit() -> Option<RtLimit>` — reads `/proc/self/limits`.
  - `pub fn rt_group(files: &[String]) -> Option<(String, u32)>` — the `@group` with the highest rtprio across limits file contents.
  - `pub fn rt_group_on_system() -> Option<(String, u32)>` — reads `/etc/security/limits.conf` and `/etc/security/limits.d/*.conf` in name order.

- [ ] **Step 1: Write the failing tests** in `system.rs`:

```rust
const LIMITS: &str = "Limit                     Soft Limit           Hard Limit           Units     \n\
Max nice priority         0                    0                    \n\
Max realtime priority     70                   70                   \n";

#[test]
fn reads_rtprio() {
    assert!(matches!(parse_rtprio(LIMITS), Some(RtLimit::Limit(70))));
    assert!(matches!(parse_rtprio(&LIMITS.replace("70                   70", "unlimited            unlimited")), Some(RtLimit::Unlimited)));
    assert!(parse_rtprio("Max nice priority 0 0\n").is_none());
}

#[test]
fn realtime_needs_a_limit_above_zero() {
    assert!(!realtime_ok(Some(RtLimit::Limit(0))));
    assert!(realtime_ok(Some(RtLimit::Limit(1))));
    assert!(realtime_ok(Some(RtLimit::Unlimited)));
    assert!(!realtime_ok(None));
}

#[test]
fn picks_the_highest_group() {
    let fedora = "# comment\n@pipewire   - rtprio  70\n@pipewire   - memlock 4194304\n".to_string();
    let other = "*  - rtprio 99\ntom - rtprio 95\n@audio hard rtprio 98\n@realtime soft rtprio 80\n#@ghost - rtprio 99\n".to_string();
    assert_eq!(rt_group(&[fedora.clone()]), Some(("pipewire".into(), 70)));
    assert_eq!(rt_group(&[fedora, other]), Some(("realtime".into(), 80)));
    assert_eq!(rt_group(&["@audio - rtprio 0\n".into()]), None);
    assert_eq!(rt_group(&[]), None);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p solstice system::`
Expected: compile error, `parse_rtprio` not found.

- [ ] **Step 3: Implement the functions.** Limits lines are whitespace-separated `domain type item value`; skip blank lines and `#` comments; keep only domains starting `@`, type `-` or `soft`, item `rtprio`, value above 0.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p solstice system::`
Expected: PASS.

- [ ] **Step 5: Commit** — `Read the realtime limit and the group that grants it`

---

### Task 3: Wine and yabridge verdict

**Files:**
- Modify: `crates/app/src/system.rs`
- Modify: `crates/app/src/yabridge.rs` (add `pub fn lib_dir() -> Option<PathBuf>`: the directory holding `ctl()`)

**Interfaces:**
- Consumes: `yabridge::ctl()`.
- Produces:
  - `pub fn wine_version(output: &str) -> Option<(u32, u32)>` — from `wine --version` output.
  - `pub enum YabridgeBuild { Release(u32, u32, u32), Commit(String) }`
  - `pub fn yabridge_build(lib: &[u8]) -> Option<YabridgeBuild>`
  - `pub enum Verdict { NoWine, NoYabridge { branch: bool }, Unchecked, Broken, Unknown, Working }`
  - `pub fn verdict(wine: Option<(u32, u32)>, installed: bool, build: Option<&YabridgeBuild>) -> Verdict`
  - `pub fn yabridge_link(wine: (u32, u32)) -> &'static str`
  - `pub fn detect() -> (Option<(u32, u32)>, Verdict)` — runs `wine --version`, finds yabridge with `yabridge::ctl()`, reads `libyabridge-vst3.so` beside it.
- Rule table, one `const` each: `KNOWN_GOOD: &[&str] = &["ba7022d"]`, `POPUPS_BREAK_FROM: (u32, u32) = (9, 22)`, `FIXED_RELEASE: (u32, u32, u32) = (5, 2, 0)`.

The library's version string is the NUL-terminated string immediately before `yabridge-host-32.exe\0`: `5.1.1` in a release, a short commit ID (`ba7022d`) in a git build.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn reads_wine_versions() {
    assert_eq!(wine_version("wine-11.20 (Staging)\n"), Some((11, 20)));
    assert_eq!(wine_version("wine-9.0"), Some((9, 0)));
    assert_eq!(wine_version("bash: wine: command not found"), None);
}

#[test]
fn finds_the_yabridge_build() {
    let lib = |v: &str| [b"other_proxy\0".as_slice(), v.as_bytes(), b"\0yabridge-host-32.exe\0yabridge-host.exe\0"].concat();
    assert!(matches!(yabridge_build(&lib("5.1.1")), Some(YabridgeBuild::Release(5, 1, 1))));
    assert!(matches!(yabridge_build(&lib("ba7022d")), Some(YabridgeBuild::Commit(c)) if c == "ba7022d"));
    assert!(yabridge_build(b"no marker here").is_none());
}

#[test]
fn judges_yabridge_against_wine() {
    use YabridgeBuild::*;
    let new_wine = Some((11, 20));
    let old_wine = Some((9, 21));
    assert!(matches!(verdict(None, false, None), Verdict::NoWine));
    assert!(matches!(verdict(new_wine, false, None), Verdict::NoYabridge { branch: true }));
    assert!(matches!(verdict(old_wine, false, None), Verdict::NoYabridge { branch: false }));
    assert!(matches!(verdict(new_wine, true, None), Verdict::Unchecked));
    assert!(matches!(verdict(new_wine, true, Some(&Release(5, 1, 1))), Verdict::Broken));
    assert!(matches!(verdict(old_wine, true, Some(&Release(5, 1, 1))), Verdict::Working));
    assert!(matches!(verdict(new_wine, true, Some(&Release(5, 2, 0))), Verdict::Working));
    assert!(matches!(verdict(new_wine, true, Some(&Commit("ba7022d".into()))), Verdict::Working));
    assert!(matches!(verdict(new_wine, true, Some(&Commit("1234abc".into()))), Verdict::Unknown));
    assert!(matches!(verdict(old_wine, true, Some(&Commit("1234abc".into()))), Verdict::Working));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p solstice system::`
Expected: compile error.

- [ ] **Step 3: Implement.** `verdict` order: no Wine → `NoWine`; not installed → `NoYabridge { branch: wine >= POPUPS_BREAK_FROM }`; no build read → `Unchecked`; Wine below 9.22 → `Working`; release at or past `FIXED_RELEASE` or a known-good commit → `Working`; other release → `Broken`; other commit → `Unknown`. `yabridge_link` returns the branch link for Wine at or past 9.22, else the release link.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p solstice system::`
Expected: PASS.

- [ ] **Step 5: Check `detect()` on this machine** with a throwaway test marked `#[ignore]`, printing the result:

Run: `cargo test -p solstice system::detect_here -- --ignored --nocapture`
Expected: `(Some((11, 20)), Working)`. Delete the test after.

- [ ] **Step 6: Commit** — `Judge the installed yabridge against the Wine version`

---

### Task 4: Root actions through the desktop prompt

**Files:**
- Create: `crates/app/src/elevate.rs` (add `mod elevate;`)

**Interfaces:**
- Produces:
  - `pub enum Outcome { Done, Cancelled, Failed(String) }`
  - `pub fn group_argv(group: &str, user: &str) -> Option<Vec<String>>` — `["pkexec", "/usr/sbin/usermod", "-aG", group, user]`; `None` when either name isn't `[a-z_][a-z0-9_-]*` (a trailing `$` allowed on the user).
  - `pub fn group_command(group: &str) -> String` — `sudo usermod -aG <group> $USER`, shown on fallback.
  - `pub fn wine_argv() -> Vec<String>` — `["pkcon", "install", "--noninteractive", "wine"]`.
  - `pub fn wine_command(os_release: &str) -> String` — the command shown on fallback, chosen from `/etc/os-release` `ID` then `ID_LIKE`: `fedora` → `sudo dnf install wine`, `debian`/`ubuntu` → `sudo apt install wine`, `opensuse` → `sudo zypper install wine`, `arch` → `sudo pacman -S wine`; otherwise "Install the wine package with your distribution's package manager."
  - `pub fn outcome(status: Option<i32>, stderr: &str) -> Outcome` — 126 or 127 from `pkexec`, or "cancel" in PackageKit's stderr, is `Cancelled`; 0 is `Done`; anything else, including failing to start the program, is `Failed`.
  - `pub fn run(argv: Vec<String>, done: impl FnOnce(Outcome) + 'static)` — spawns with `gio::Subprocess`, never blocking the main loop.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn builds_safe_commands() {
    assert_eq!(group_argv("pipewire", "tom").unwrap(), ["pkexec", "/usr/sbin/usermod", "-aG", "pipewire", "tom"]);
    assert!(group_argv("pipe wire", "tom").is_none());
    assert!(group_argv("audio", "tom; rm -rf /").is_none());
    assert!(group_argv("audio", "-oops").is_none());
}

#[test]
fn reads_outcomes() {
    assert!(matches!(outcome(Some(0), ""), Outcome::Done));
    assert!(matches!(outcome(Some(126), ""), Outcome::Cancelled));
    assert!(matches!(outcome(Some(127), ""), Outcome::Cancelled));
    assert!(matches!(outcome(Some(1), "Fatal error: The transaction was cancelled"), Outcome::Cancelled));
    assert!(matches!(outcome(Some(1), "No package wine found"), Outcome::Failed(e) if e.contains("wine")));
    assert!(matches!(outcome(None, ""), Outcome::Failed(_)));
}

#[test]
fn wine_command_per_distribution() {
    assert_eq!(wine_command("ID=fedora\n"), "sudo dnf install wine");
    assert_eq!(wine_command("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"), "sudo apt install wine");
    assert!(wine_command("ID=gentoo\n").contains("package manager"));
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p solstice elevate::`
Expected: compile error.

- [ ] **Step 3: Implement the functions.** `run` passes the outcome of `communicate_utf8_async` to `outcome`; a spawn error (no `pkexec` or `pkcon`) is `Failed`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p solstice elevate::`
Expected: PASS.

- [ ] **Step 5: Commit** — `Run root actions through the desktop's authentication prompt`

---

### Task 5: Plug-in folder search

**Files:**
- Create: `crates/app/src/plugin_search.rs` (add `mod plugin_search;`)

**Interfaces:**
- Produces:
  - `pub fn wine_prefixes(home: &Path, wineprefix: Option<&Path>) -> Vec<PathBuf>` — existing prefixes, deduplicated, in order: `$WINEPREFIX`, `~/.wine`, each directory under `~/.local/share/bottles/bottles` and `~/.var/app/com.usebottles.bottles/data/bottles/bottles`, and under `~/Games` each `*/` holding a `drive_c` (Lutris). A prefix is a directory holding `drive_c`.
  - `pub fn windows_plugin_dirs(prefix: &Path) -> Vec<PathBuf>` — `drive_c/Program Files/Common Files/VST3` and `.../CLAP` that exist.
  - `pub fn linux_plugin_dirs(roots: &[PathBuf]) -> Vec<PathBuf>` — directories up to three levels below each root that directly contain a `*.vst3` or `*.clap` entry, excluding the standard folders `solstice_vst3::default_paths()` and `solstice_clap::default_paths()` already scan.
  - `pub fn extra_roots(home: &Path) -> Vec<PathBuf>` — `/opt`, `~/.local/lib`.

- [ ] **Step 1: Write the failing tests** using a temp dir as home:

```rust
#[test]
fn finds_prefixes_and_their_plugin_folders() {
    let home = tempdir();
    let vst3 = home.join(".wine/drive_c/Program Files/Common Files/VST3");
    std::fs::create_dir_all(&vst3).unwrap();
    std::fs::create_dir_all(home.join(".local/share/bottles/bottles/Audio/drive_c")).unwrap();
    std::fs::create_dir_all(home.join("Games/not-a-prefix")).unwrap();
    let prefixes = wine_prefixes(&home, Some(&home.join(".wine")));
    assert_eq!(prefixes, [home.join(".wine"), home.join(".local/share/bottles/bottles/Audio")]);
    assert_eq!(windows_plugin_dirs(&prefixes[0]), [vst3]);
    assert!(windows_plugin_dirs(&prefixes[1]).is_empty());
}

#[test]
fn finds_linux_plugins_outside_the_standard_folders() {
    let root = tempdir();
    std::fs::create_dir_all(root.join("Vendor/lib/vst3/Synth.vst3")).unwrap();
    std::fs::create_dir_all(root.join("Other/nothing")).unwrap();
    assert_eq!(linux_plugin_dirs(&[root.clone()]), [root.join("Vendor/lib/vst3")]);
    assert!(linux_plugin_dirs(&[root.join("missing")]).is_empty());
}
```

`tempdir()` is a small test helper making a unique directory under `std::env::temp_dir()`.

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p solstice plugin_search::`
Expected: compile error.

- [ ] **Step 3: Implement the functions.** Unreadable directories are skipped, not errors.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p solstice plugin_search::`
Expected: PASS.

- [ ] **Step 5: Commit** — `Find Wine prefixes and plug-in folders outside the standard paths`

---

### Task 6: Shared Interface and Buffer Size rows

**Files:**
- Modify: `crates/app/src/prefs.rs` (`audio_page`)

**Interfaces:**
- Produces:
  - `pub fn interface_row(ctx: &Ctx, changed: impl Fn() + 'static) -> adw::ComboRow` — the Interface combo as committed in 8668c3c; `changed` runs after `set_interface`.
  - `pub fn buffer_row(ctx: &Ctx, changed: impl Fn(Option<u32>) + 'static) -> adw::ComboRow` — the Buffer Size combo; `changed` gets the new round trip.

Moves code out of `audio_page` without changing behaviour; `audio_page` passes closures that refresh its Inputs, Outputs and latency rows.

- [ ] **Step 1: Extract the two functions** and use them in `audio_page`.
- [ ] **Step 2: Build and run the tests**

Run: `cargo build -p solstice && cargo test -p solstice`
Expected: builds without warnings, all pass.

- [ ] **Step 3: Check Preferences → Audio by hand**: changing Interface updates the Inputs and Outputs rows; changing Buffer Size updates Round-Trip Latency.
- [ ] **Step 4: Commit** — `Share the Interface and Buffer Size rows`

---

### Task 7: The setup window

**Files:**
- Create: `crates/app/src/setup.rs` (add `mod setup;`)

**Interfaces:**
- Consumes: Tasks 1–6.
- Produces:
  - `pub const SETUP_VERSION: u32 = 1;`
  - `pub enum Page { Realtime, Interface, Folders, Plugins, Windows }`
  - `pub fn present(ctx: &Ctx, win: &adw::ApplicationWindow, start: Page, closed: impl FnOnce() + 'static)` — an `adw::Dialog` with Back, Next and, on the last page, Done. Done sets `setup_version = SETUP_VERSION` and saves. `closed` runs when the dialog closes either way.

Pages (copy wording is the builder's, formal):

- **Realtime:** tick and "Allowed (priority N)" when `realtime_ok`; otherwise why it matters, and a Fix button running `group_argv(rt_group_on_system().0, user)`. On `Done`: "Log out and back in to finish." On `Cancelled` or `Failed`, or with no group found: the `group_command` text, selectable, or the no-group note from the spec.
- **Interface:** `interface_row` and `buffer_row`; without a backend, the existing "Audio engine not running" text and no rows.
- **Folders:** two rows with folder pickers (`gtk::FileDialog::select_folder`) for `projects_folder` and `exports_folder`, showing `projects_dir()` and `exports_dir()`.
- **Plug-ins:** on entering, runs `prefs::rescan_plugins` and the Task 5 search on `gio::spawn_blocking`, with a spinner. Then: Linux plug-ins by format (`catalog` count per format); extra Linux folders each with an Add button that appends to `vst3_paths` and rescans; Windows folders per prefix, each marked bridged or not (`yabridge::folders()` contains it).
- **Windows Plug-ins:** from `system::detect()`, per the spec's table. `NoWine`: Install Wine button running `wine_argv()`, falling back to `wine_command`; on `Done` re-runs detection. `NoYabridge`/`Broken`/`Unknown`: the text and a link button to `yabridge_link`. `Unchecked`: says Solstice couldn't read the yabridge build, no tick. `Working`: tick, and a Set Up button that runs `yabridge::add` for each unbridged Windows folder from the Plug-ins page, then `yabridge::sync`, then `prefs::rescan_plugins`.

- [ ] **Step 1: Build the dialog and pages.**
- [ ] **Step 2: Build**

Run: `cargo build -p solstice`
Expected: no warnings.

- [ ] **Step 3: Commit** — `Add the setup window`

---

### Task 8: Show it, the menu item and the realtime banner

**Files:**
- Modify: `crates/app/src/projects.rs:93-103` (`open_window`)
- Modify: `crates/app/src/window.rs` (menu at :137, action near :767, banner near :176)

**Interfaces:**
- Consumes: `setup::present`, `setup::SETUP_VERSION`, `setup::Page`, `system::realtime_ok`, `system::realtime_limit`.

- [ ] **Step 1: In `open_window`,** when this is the first window and `settings.setup_version < SETUP_VERSION`, call `setup::present(.., Page::Realtime, ..)` and run today's chooser-or-recovery branch from its `closed` callback; otherwise run that branch directly.
- [ ] **Step 2: Add "Set _Up Solstice…"** above "_Preferences" in the app section, action `win.setup`, opening `Page::Realtime` with an empty `closed`.
- [ ] **Step 3: Add a second `adw::Banner`** under the audio banner, revealed when `!realtime_ok(realtime_limit())`, titled with a one-line explanation, button "Fix…" opening `Page::Realtime`. Checked once when the window is built.
- [ ] **Step 4: Build and run all tests**

Run: `cargo build --workspace --all-targets && cargo test --workspace`
Expected: no warnings, all pass.

- [ ] **Step 5: Check by hand with a fresh dev root** (settings under it, so the real ones are untouched):

```bash
dev=$(mktemp -d) && SOLSTICE_DEV="$dev" cargo run -p solstice
```

Expected: the setup opens before the chooser; Done leads to the chooser; quitting and relaunching with the same `SOLSTICE_DEV` goes straight to the chooser; the menu item reopens it; Realtime shows a tick (limit 70 here); Windows Plug-ins shows yabridge as working. Then with the real settings (`cargo run -p solstice`): the setup appears once and the existing Interface and buffer size are shown unchanged.

- [ ] **Step 6: Commit** — `Show the setup on first launch and from the menu`
