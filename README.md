# Solstice

A digital audio workstation for GNOME, laid out and driven like Logic Pro: control bar
and LCD, library, inspector, tracks area, docked or windowed mixer, piano roll and file
browser. Audio runs through the JACK API, so it works on PipeWire and on JACK itself.

- Audio and software instrument tracks, cycle and autopunch recording, real-time bounce
- Input monitoring inside a single audio period, with Logic's auto input monitoring and
  low latency mode
- VST3 effects and instruments; plug-ins are scanned in a separate process
- Aux buses with pre-fader, post-fader and post-pan sends, bus-to-bus routing and
  feedback protection
- Mixer gestures from Logic: relative fader drags, Shift for fine control, Alt-click to
  reset, ganged controls across selected strips, drag across M/S/R/I buttons to paint
  their state, Ctrl-click solo for solo-safe, and plug-in drag and drop (Alt to copy)

## Building

```bash
sudo dnf install cargo cmake gcc-c++ gtk4-devel libadwaita-devel pipewire-jack-audio-connection-kit-devel libX11-devel
```

```bash
git submodule update --init vst3shim/vst3sdk
git -C vst3shim/vst3sdk submodule update --init base cmake pluginterfaces public.sdk
cargo build --release
```

`cargo test` runs the engine, model and VST3 host tests. The host tests use the SDK's sample
plug-ins, built with `vst3shim/build-samples.sh`.

## Packaging

`packaging/make-tarball.sh` builds a source tarball that includes the VST3 SDK, for
`packaging/solstice.spec`.

## Licence

GPL-3.0-or-later. The bundled VST3 SDK is MIT licensed.
