Name:           solstice
Version:        %{?solstice_version}%{!?solstice_version:0.7.0}
# build-rpm.sh sets this to the commit count and hash, so every build upgrades the last.
Release:        %{?solstice_release}%{!?solstice_release:1}%{?dist}
Summary:        Digital audio workstation for GNOME
License:        GPL-3.0-or-later AND MIT AND MPL-2.0 AND Apache-2.0 AND BSD-3-Clause
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
Requires:       pipewire-jack-audio-connection-kit
# Steinberg's VST3 SDK is MIT licensed and not packaged for Fedora.
Provides:       bundled(vst3sdk) = 3.8.1
# NeuralAmpModelerCore is MIT licensed and includes nlohmann/json (MIT). Eigen is MPL-2.0, with
# a few headers under Apache-2.0 and BSD-3-Clause.
Provides:       bundled(NeuralAmpModelerCore) = 0.6.0
Provides:       bundled(nlohmann-json) = 3.12.0
Provides:       bundled(eigen3) = 5.0.1

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

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml

%files
%license LICENSE vst3shim/vst3sdk/LICENSE.txt LICENSE.NeuralAmpModelerCore
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.README
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.MPL2
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.APACHE
%license namshim/NeuralAmpModelerCore/Dependencies/eigen/COPYING.BSD
%doc README.md
%{_bindir}/solstice
%{_bindir}/solstice-scan
%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/uk.co.solarflareit.Solstice.svg

%changelog
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
