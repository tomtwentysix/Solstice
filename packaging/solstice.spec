Name:           solstice
Version:        %{?solstice_version}%{!?solstice_version:0.1.0}
# build-rpm.sh sets this to the commit count and hash, so every build upgrades the last.
Release:        %{?solstice_release}%{!?solstice_release:1}%{?dist}
Summary:        Digital audio workstation for GNOME
License:        GPL-3.0-or-later AND MIT
URL:            https://solarflareit.co.uk
Vendor:         SolarFlare Developments
# Made by packaging/make-tarball.sh, which includes the VST3 SDK submodule.
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  cmake
BuildRequires:  gcc-c++
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(jack)
BuildRequires:  pkgconfig(x11)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib
Requires:       pipewire-jack-audio-connection-kit
# Steinberg's VST3 SDK is MIT licensed and not packaged for Fedora.
Provides:       bundled(vst3sdk) = 3.8.1

%description
Solstice is a digital audio workstation for GNOME with a professional studio layout,
low-latency input monitoring through JACK or PipeWire, and VST3 and CLAP plug-in
hosting.

%prep
%autosetup

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
%license LICENSE vst3shim/vst3sdk/LICENSE.txt
%doc README.md
%{_bindir}/solstice
%{_bindir}/solstice-scan
%{_datadir}/applications/uk.co.solarflareit.Solstice.desktop
%{_metainfodir}/uk.co.solarflareit.Solstice.metainfo.xml
%{_datadir}/icons/hicolor/scalable/apps/uk.co.solarflareit.Solstice.svg

%changelog
* Thu Nov 12 2026 SolarFlare Developments - 0.1.0-1
- First release
