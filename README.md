<p align="center">
  <img src="data/icons/uk.co.solarflareit.Solstice.svg" width="128" alt="">
</p>

<h1 align="center">Solstice</h1>

<p align="center">A digital audio workstation for GNOME, built for a professional studio workflow.</p>

Solstice has a control bar and LCD, library, inspector, tracks area, docked or
windowed mixer, editors and file browser, built with GTK 4 and libadwaita and drawn on the
GPU. Audio runs through the JACK API, so it works on PipeWire and on JACK itself.

## Features

- **Recording:** audio and software instrument tracks, cycle recording into take folders
  with swipe comping, autopunch, count-in, Capture as Recording and Musical Typing
- **Low latency:** input monitoring within a single audio period, with auto input
  monitoring and low latency mode
- **Plug-ins:** VST3 and CLAP effects and instruments, scanned in a separate process so a
  crashing plug-in cannot take Solstice down; Windows VST3s through yabridge
- **Built in:** Channel EQ, Compressor, Limiter, Noise Gate, Stereo Delay and Reverb;
  Solstice Synth, Sampler and Drum Kit (with its own synthesised kit); MIDI FX (Arpeggiator,
  Chord Trigger, Note Repeater, Scale Quantize, Transposer, Velocity Processor). Each has
  factory presets, and any plug-in's settings can be saved as your own
- **Project Chooser:** templates for songwriting, electronic, hip hop, band recording and
  podcasts, your own templates, recent projects, and the tempo, signature and key to start
  with
- **Mixing:** aux buses with pre-fader, post-fader and post-pan sends, VCA faders,
  side-chain keying, plug-in delay compensation and track processing spread over real-time
  worker threads
- **Mixer gestures:** relative fader drags, Shift for fine control, Alt-click to
  reset, ganged controls across selected strips, dragging across M/S/R/I buttons to paint
  them, Ctrl-click solo for solo-safe, and plug-in drag and drop (Alt to copy)
- **Editing:** piano roll, step sequencer, Event List, audio file editor and take folder
  comping; automation in Read, Touch, Latch and Write
- **Arrangement:** global tracks for sections, markers, signature, key and tempo changes;
  folder and summing Track Stacks; right-click menus for regions and tracks; track colours
  and duplicating; channel strip settings saved and loaded from the Setting menu
- **Getting audio in and out:** faster-than-real-time bounce, track export, bounce in place
  and Track Freeze; import of projects through Final Cut Pro XML, MIDI files and project
  bundles; a loop
  browser for your own loop folders

## Installing

Solstice is built for Fedora. Build an RPM of the current commit and install it:

```bash
packaging/build-rpm.sh
```

```bash
sudo dnf install dist/RPMS/x86_64/solstice-0.*.x86_64.rpm
```

The release number is the commit count and hash, such as `0.1.0-34.git1a2b3c4`, so each
build upgrades the one before it.

## Building

```bash
sudo dnf install cargo cmake gcc-c++ gtk4-devel libadwaita-devel pipewire-jack-audio-connection-kit-devel libX11-devel rpm-build desktop-file-utils libappstream-glib
```

```bash
git clone https://github.com/tomtwentysix/Solstice.git
cd Solstice
git submodule update --init vst3shim/vst3sdk
git -C vst3shim/vst3sdk submodule update --init base cmake pluginterfaces public.sdk
cargo build --release
```

`cargo test` runs the engine, model, plug-in and host tests. The engine tests fail on any
allocation in the audio thread. The VST3 host tests use the SDK's sample plug-ins, built
with `vst3shim/build-samples.sh`; the CLAP host test runs when LSP's CLAP bundle is
installed.

Setting `SOLSTICE_DEV=1` runs a separate instance with its own settings and projects, which
leaves an installed Solstice alone.

## Layout

| Crate | Contents |
| --- | --- |
| `app` | The GTK interface |
| `model` | Projects, tracks, regions, undo and the tempo map |
| `engine` | The real-time graph, recording, offline rendering and the worker pool |
| `backend-jack` | The JACK and PipeWire client |
| `dsp` | Built-in effects, instruments and MIDI FX |
| `vst3`, `clap` | Plug-in hosting; `vst3shim` is the C++ side of VST3 |
| `scan` | `solstice-scan`, the out-of-process plug-in scanner |
| `interchange` | Project bundle, Final Cut Pro XML and MIDI file import |

## Licence

GPL-3.0-or-later. The bundled VST3 SDK is MIT licensed.

Made by SolarFlare Developments.
