# First-run setup

Status: draft for approval, 2026-11-13.

## Intent

Someone who installs the Solstice RPM should be able to track guitar through bridged
Windows plug-ins at a small buffer without crackles, and without first debugging their
system. Today that depends on settings nobody is told about: the account's realtime
priority limit, which interfaces Solstice wires to, and whether yabridge works with the
installed Wine. Each of these broke on the developer's own machine on 2026-11-13:

- With `ulimit -r` at 0, the yabridge host processes could not take realtime priority, so
  Solstice's cycle sometimes ran to 7.1 ms of a 5.3 ms period and the graph xrunned. Joining
  the `pipewire` group and logging in again brought the worst cycle down to 0.83 ms.
- Wiring every physical port put every sound card into Solstice's clock group, so its
  128-frame buffer was forced onto a DALI IO that stalls at anything other than its own
  period. The Interface setting (commit 8668c3c) fixes this once one interface is chosen.
- yabridge 5.1.1, the latest release, ignores clicks in Windows plug-in pop-ups under
  Wine 9.22 and later. Fedora ships Wine 11.

The first-run setup walks a new user through these, plus the folders Solstice saves to and
the plug-ins it can find, once, before the project chooser.

## Goals

- A setup window shown on first launch, before the project chooser, with five pages in
  order: Realtime, Interface, Folders, Plug-ins, Windows Plug-ins. Back and Next move
  between pages; the last page has Done.
- A "Set Up Solstice…" menu item that opens the same window again at any time.
- Realtime page: reads the limit Solstice actually has. With a limit high enough for audio,
  a tick and the value. Otherwise, why it matters (plug-ins run late and crackle), the
  command that fixes it for this system, and that the fix takes effect after logging out
  and in again.
- Interface page: the interface and buffer size, the same settings as Preferences → Audio,
  with the latency of each buffer size shown.
- Folders page: the folder new projects are saved in and the folder mixes and stems are
  exported to, each with a folder picker.
- Plug-ins page: searches for installed plug-ins, beyond the standard folders, and lists
  what it found in two groups: Linux plug-ins Solstice can use now, and Windows plug-ins
  that need yabridge.
- Windows Plug-ins page: detects Wine and yabridge, says whether this yabridge works with
  this Wine, links to the right yabridge when it is missing or broken, and once a working
  yabridge is present, sets it up.
- On later launches, a banner when the realtime limit is too low, since a reinstall or a
  new account can lose it silently.

## Non-goals

- Running `sudo` in a terminal, or asking for a password in Solstice's own window. Changes
  that need root go through the desktop's own authentication prompt (see Elevation).
- Downloading or installing yabridge, which no distribution packages. Solstice links to it.
- Moving the library folders (captures, impulse responses, presets, templates, grooves) out
  of `~/.local/share/solstice`.
- Checking CPU governors, kernel preemption, IRQ threading or other tuning. Only the checks
  that broke real sessions are in scope.

## Anti-goals

- A setup that blocks the user. Every page can be passed with Next; nothing is required to
  reach the project chooser.
- Wrong reassurance. A tick appears only for something actually checked; a yabridge build
  Solstice cannot judge is not reported as working.

## Constraints

- Solstice connects through the JACK API and runs on PipeWire or JACK. The interface and
  buffer size pages need the backend connected; without one they say so and offer nothing.
- Group names for realtime differ by distribution: `pipewire` on Fedora, `audio` or
  `realtime` elsewhere. The command shown is built from the limits files on this system.
- yabridge builds made from git report the same version as the release they follow. The
  developer's working build of `new-wine10-embedding` (commit ba7022d) says `5.1.1`, like
  the broken release. A git build embeds its short commit ID in `libyabridge-vst3.so`; a
  release does not.
- Code style per the developer's global instructions: lean, no explanatory comments for UI
  choices, explanations go in the UI text and the chat, not the code.

## Approaches

1. **Step-by-step window** (chosen). One page per subject with Back and Next. Each page can
   explain itself, and the slow work (the plug-in search, `yabridgectl sync`) gets a page
   with its own progress. More UI code, and experienced users click through five pages.
2. **Single checklist.** One window with every check as a row and a Fix or Choose button.
   Fast for experienced users and doubles as a health check, but crowds new users and leaves
   the search and yabridge setup one row each.
3. **A Setup page in Preferences.** Least code, reusing the existing rows, but it reads as
   settings rather than a welcome and the realtime and yabridge warnings are easy to miss.

The developer picked 1, since the setup is for other users and the realtime and yabridge
pages each need explaining.

## Design decided

### When it shows

- First launch: settings record the setup version last completed. When it is below the
  current version, the window opens before the project chooser. Done records the version.
  Closing the window without Done shows it again next launch.
- Users upgrading from 0.7 have no recorded version, so they see the setup once too, as
  the developer decided. It shows their existing settings and changes none of them.
- (Builder's call, from the review.) The main window and audio connection open as today;
  the setup is a dialog over the main window, shown before the chooser whatever the Startup
  preference is. Closing it, with or without Done, carries on to the chooser or project as
  the Startup preference says.
- The menu item opens it at any time; Done there records the version too.
- Later launches: if the realtime check fails, the main window shows a banner with the same
  explanation and a button that opens the setup at the Realtime page.

### Realtime page

- Reads `RLIMIT_RTPRIO` for the Solstice process.
- (Builder's call, from the review.) Passes when the limit is above 0. Any limit above 0
  lets the bridged plug-in threads take SCHED_FIFO, which is what fixed the xruns; the check
  is a pure function of the limit and needs no backend, so the startup banner can use it
  before audio connects.
- On failure, reads `/etc/security/limits.conf` and `/etc/security/limits.d/*.conf` for
  `@group - rtprio N` lines and picks the group with the highest limit. A "Fix" button adds
  the user to that group through the desktop's authentication prompt, then says to log out
  and back in. With no such line, says the system grants no realtime limit to any group and
  points to the distribution's docs.

### Interface page

- The same Interface and Buffer Size choices as Preferences → Audio, writing the same
  settings (`interface`, `buffer_size`), applied as Preferences applies them.
- (Builder's call, from the review.) No guessed default: port counts tie too often, and a
  wrong guess could pick a card like the DALI. The page shows the current setting, with
  text asking for the interface the user records and monitors through. Next and Done keep
  whatever is selected; an existing choice is never overwritten.

### Folders page

- Projects folder: defaults to `~/Music/Solstice`, as `projects_dir()` does today. Becomes a
  setting; `projects_dir()` and the new-project and open dialogs use it.
- Exports folder: where mixes and stems are offered to be saved. Defaults to the projects
  folder.

### Plug-ins page

- Runs the existing scan (standard VST3, CLAP and LV2 folders plus `vst3_paths`) and, in
  addition, looks for Windows VST3 and CLAP folders in Wine prefixes: `~/.wine`,
  `$WINEPREFIX`, and prefixes under the usual Bottles and Lutris locations.
- (Builder's call, from the review.) It also looks for Linux VST3 and CLAP plug-ins in
  `/opt` and `~/.local/lib`, and offers to add any folder found there to `vst3_paths`.
- Lists Linux plug-ins by count and format, and Windows plug-ins found in each prefix with
  whether yabridge already bridges that folder.

### Windows Plug-ins page

| Found | Shown |
|---|---|
| No Wine | Windows plug-ins need Wine, with an "Install Wine" button that installs the distribution's package through PackageKit. Stops there until it is installed. |
| Wine, no yabridge | Where to get yabridge: the release for Wine below 9.22, the `new-wine10-embedding` branch for 9.22 and later, with why. |
| yabridge release 5.1.1 or older, Wine 9.22 or later | A warning that pop-up windows will not take clicks with this Wine, with the same link. |
| yabridge built from a known-good commit (ba7022d), or 5.2 or later | Treated as working. |
| yabridge built from any other commit, Wine 9.22 or later | No tick: says Solstice can't tell whether this build has the pop-up fix, and if pop-ups ignore clicks, to use the `new-wine10-embedding` branch. Setup still offered. |
| Working yabridge | Offers to add the Windows VST3 and CLAP folders the search found (`yabridgectl add`), runs `yabridgectl sync`, then rescans the plug-in list. |

(Builder's call, from the review: a short commit ID can't show which branch a build came
from, so only listed commits get a tick, in keeping with the anti-goal.)

- Wine version from `wine --version`. yabridge version from `yabridgectl --version`, and a
  git build from a short commit ID in `libyabridge-vst3.so`.
- The Wine and yabridge version rules live in one table so a new yabridge release is a
  one-line change.

### Elevation

- Anything that needs root asks through the desktop's own authentication prompt: the
  full-screen dialog on GNOME, the equivalent on KDE and other desktops. Solstice never
  sees the password.
- Adding the user to a group runs `pkexec /usr/sbin/usermod -aG <group> <user>`, with fixed
  arguments built by Solstice; nothing typed by the user or read from a file other than the
  group name from the limits files is passed.
- Installing Wine goes through PackageKit, which asks through the same prompt and knows the
  distribution's package manager. The package is `wine` unless the builder finds a
  distribution where it differs.
- When the action is cancelled or fails, or no polkit agent or PackageKit is running (some
  minimal window managers), the page falls back to showing the command to run by hand.

## Left to the builder

- Widget structure for the pages (navigation view, carousel or stack) and the window's
  size, following the existing libadwaita dialogs.
- How the Wine version is parsed (`wine-11.20 (Staging)` and similar).
- How the commit ID is found in the library, without loading it.
- Exact paths for Bottles and Lutris prefixes.
- Wording of each page, in the style of the existing UI copy.
- Tests for the parts that are pure functions: the limits file parser, the realtime check,
  and the yabridge and Wine rule table. New settings fields (setup version, projects
  folder, exports folder) default so that an existing settings file loads unchanged.
