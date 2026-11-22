Name:           solstice
Version:        %{?solstice_version}%{!?solstice_version:0.7.11}
# build-rpm.sh sets this to the commit count and hash, so every build upgrades the last.
Release:        %{?solstice_release}%{!?solstice_release:1}%{?dist}
Summary:        Digital audio workstation for GNOME
License:        GPL-3.0-or-later AND GPL-3.0-only AND GPL-2.0-or-later AND MIT AND MPL-2.0 AND Apache-2.0 AND BSD-3-Clause
URL:            https://solarflareit.co.uk
Vendor:         SolarFlare Developments
# Made by packaging/make-tarball.sh, which includes the VST3 SDK and NAM core submodules.
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  cmake
BuildRequires:  gcc-c++
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(jack)
BuildRequires:  pkgconfig(x11)
BuildRequires:  pkgconfig(lilv-0)
BuildRequires:  /usr/bin/glib-compile-resources
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib
BuildRequires:  systemd-rpm-macros
Requires:       pipewire-jack-audio-connection-kit
# Steinberg's VST3 SDK is MIT licensed and not packaged for Fedora.
Provides:       bundled(vst3sdk) = 3.8.1
# NeuralAmpModelerCore is MIT licensed and includes nlohmann/json (MIT). Eigen is MPL-2.0, with
# a few headers under Apache-2.0 and BSD-3-Clause.
Provides:       bundled(NeuralAmpModelerCore) = 0.6.0
Provides:       bundled(nlohmann-json) = 3.12.0
Provides:       bundled(eigen3) = 5.0.1
# The starter captures (GPL-3.0-only) and cabinet impulse responses (GPL-2.0-or-later) are compiled
# into the binary; data/starter/CREDITS.md lists their sources.

%description
Solstice is a digital audio workstation for GNOME with a professional studio layout,
low-latency input monitoring through JACK or PipeWire, and VST3 and CLAP plug-in
hosting.

%prep
%autosetup
# Two upstream files are both called LICENSE.
cp namshim/NeuralAmpModelerCore/LICENSE LICENSE.NeuralAmpModelerCore

%build
cargo build --release --locked

%install
install -Dm755 ${CARGO_TARGET_DIR:-target}/release/solstice %{buildroot}%{_bindir}/solstice
install -Dm755 ${CARGO_TARGET_DIR:-target}/release/solstice-scan %{buildroot}%{_bindir}/solstice-scan
install -Dm644 data/uk.co.solarflareit.Solstice.desktop %{buildroot}%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
install -Dm644 data/uk.co.solarflareit.Solstice.metainfo.xml %{buildroot}%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml
install -Dm644 data/icons/uk.co.solarflareit.Solstice.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/uk.co.solarflareit.Solstice.svg
install -Dm644 data/udev/70-solstice-cpu-latency.rules %{buildroot}%{_udevrulesdir}/70-solstice-cpu-latency.rules

%post
udevadm control --reload >/dev/null 2>&1 || :
udevadm trigger --action=change --name-match=cpu_dma_latency >/dev/null 2>&1 || :

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml

%files
%license LICENSE vst3shim/vst3sdk/LICENSE.txt LICENSE.NeuralAmpModelerCore
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.README
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.MPL2
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.APACHE
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.BSD
%doc README.md data/starter/CREDITS.md
%{_bindir}/solstice
%{_bindir}/solstice-scan
%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/uk.co.solarflareit.Solstice.svg
%{_udevrulesdir}/70-solstice-cpu-latency.rules

%changelog
* Sun Nov 22 2026 SolarFlare Developments - 0.7.11-1
- Project bundles import in one step with their exported arrangement, exported tracks or recordings at their recorded positions, plus tempo maps, signature changes, markers and key; the cycle sits in the bar ruler and can be moved and resized, Ctrl-drag sets a range that playback skips, and the ruler always lines up with the tracks

* Sat Nov 14 2026 SolarFlare Developments - 0.7.10-1
- Fewer overruns with Windows plug-ins: while audio runs, Solstice keeps the CPU out of the deep sleep states that are slow to wake from, and Preferences shows whether the system allows it

* Sat Nov 14 2026 SolarFlare Developments - 0.7.9-1
- Stability: a take that fails to start or a full event queue no longer spoils later takes, disarming mid-take ends the take, autosave follows changes within seconds, audio is read into memory ahead of playback, very long takes continue in new files, closing a recording window asks first, and the project lock and newer-format checks no longer lock out read-only projects or let an older copy replace a newer one

* Sat Nov 14 2026 SolarFlare Developments - 0.7.8-1
- Project safety: saves survive a crash or power cut with a backup kept, damaged project and settings files are recovered instead of replaced, edits in VST3 plug-in windows count as unsaved changes, autosave failures are reported, a plug-in that crashes while loading no longer crashes its project every time, and a project can't be open twice

* Sat Nov 14 2026 SolarFlare Developments - 0.7.7-1
- Clicks in Windows plug-in editors land where they are aimed; plug-ins on tracks run at real-time priority, which removes the overruns heavy plug-ins such as Ozone caused; a hung or crashing plug-in can no longer stall or stop the engine

* Sat Nov 14 2026 SolarFlare Developments - 0.7.6-1
- Recording safety: takes survive a slow or full disk, a crash, an audio server restart and a switch between mono and stereo, and actions that would end a take wait until recording stops

* Sat Nov 14 2026 SolarFlare Developments - 0.7.5-1
- Solstice survives a Windows plug-in's host crashing: the slot passes audio through, keeps its settings and offers Reload, and exports and freezes refuse to leave the plug-in out

* Fri Nov 13 2026 SolarFlare Developments - 0.7.4-1
- First-run setup that checks realtime priority, the interface and buffer size, save folders, installed plug-ins, and Wine and yabridge, with root actions through the desktop's authentication prompt; Solstice connects to one chosen interface; the xrun count shows only after more than 10 in the last hour; plug-in scans run one at a time.

* Fri Nov 13 2026 SolarFlare Developments - 0.7.3-1
- Empty sections on typed strips use the normal empty-slot colour; Guitar DI and Bass DI tracks show guitar and bass icons.

* Fri Nov 13 2026 SolarFlare Developments - 0.7.2-1
- Empty sections on Guitar DI and Bass DI strips show their picture on a full-height add slot.

* Fri Nov 13 2026 SolarFlare Developments - 0.7.1-1
- Starter captures and cabinet impulse responses installed into the libraries; Guitar Clean, Guitar Crunch, Guitar High Gain and Bass factory strip settings.

* Fri Nov 13 2026 SolarFlare Developments - 0.7.0-1
- Strip types for guitar and bass DI with Pedals, Amp, Cabinet and Effects sections; built-in Neural Amp Modeler with a capture library; impulse response loader; plug-in categories and Group by; factory channel strip settings; cue mixes on hardware outputs; tuner; pre-roll; choosing the sample rate, with audio at other rates converted; region end edges resize except at the loop corner.

* Fri Nov 13 2026 SolarFlare Developments - 0.6.1-1
- Saving a new project into its own folder no longer empties its audio files.

* Fri Nov 13 2026 SolarFlare Developments - 0.6.0-1
- Loudness window with LUFS, loudness range, true peak, correlation and spectrum; reference tracks with A/B and level matching; Export Mix and Export Stems in WAV and FLAC with dither and loudness normalisation; Saturation and De-esser built-ins; LV2 latency changes followed live.

* Fri Nov 13 2026 SolarFlare Developments - 0.5.1-1
- Faders icon for the mixer and snowflake icon for Freeze; old and never-saved projects open, with a versioned project format; larger mixer strip text and buttons; buses centred between tracks and the output; the plug-in On/Off button always first; the audio file editor ruler starts in minutes and seconds.

* Fri Nov 13 2026 SolarFlare Developments - 0.5.0-1
- Recording delay compensation with loopback measurement; polarity invert; metronome click grouping for odd and compound meters; hardware outputs for re-amping; LV2 plug-in hosting.

* Fri Nov 13 2026 SolarFlare Developments - 0.4.0-1
- Groove templates for Quantize, made from any MIDI or drum region, with built-in swing grooves; track alternatives; region colours.

* Fri Nov 13 2026 SolarFlare Developments - 0.3.0-1
- A full drum editor: step patterns with velocity, gate, ratchets, chance, offset and ties per step, per-row rates and lengths, row tools and starter patterns; a Hits view for drum parts; and pad naming, ordering, hiding, mute and solo.

* Thu Nov 12 2026 SolarFlare Developments - 0.2.0-1
- Track Freeze and faster-than-real-time bounce and export; side-chain inputs for VST3 plug-ins; CLAP plug-in interfaces shown inside the plug-in window; MIDI take folders for cycle recording.

* Thu Nov 12 2026 SolarFlare Developments - 0.1.1-1
- Describes Solstice's features on their own terms, without naming other products.

* Thu Nov 12 2026 SolarFlare Developments - 0.1.0-1
- First release
