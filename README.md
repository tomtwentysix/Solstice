# Solstice

A digital audio workstation for GNOME, laid out and driven like Logic Pro: control bar
and LCD, library, inspector, tracks area, docked or windowed mixer, piano roll and file
browser. Audio runs through the JACK API, so it works on PipeWire and on JACK itself.

- Audio and software instrument tracks, cycle recording into take folders with swipe
  comping, autopunch, count-in, Capture as Recording and Musical Typing
- Input monitoring inside a single audio period, with Logic's auto input monitoring and
  low latency mode
- VST3 and CLAP effects and instruments, scanned in a separate process; Windows VST3s
  through yabridge
- Built-in Channel EQ, Compressor, Limiter, Noise Gate, Stereo Delay and Reverb; Solstice
  Synth, Sampler and Drum Kit; and MIDI FX (Arpeggiator, Chord Trigger, Note Repeater,
  Scale Quantize, Transposer, Velocity Processor)
- Aux buses with pre-fader, post-fader and post-pan sends, VCA faders, side-chain keying,
  plug-in delay compensation and track processing spread over real-time worker threads
- Mixer gestures from Logic: relative fader drags, Shift for fine control, Alt-click to
  reset, ganged controls across selected strips, drag across M/S/R/I buttons to paint
  their state, Ctrl-click solo for solo-safe, and plug-in drag and drop (Alt to copy)
- Piano roll, step sequencer and Event List; automation in Read, Touch, Latch and Write
- Global tracks for arrangement sections, markers, signature, key and tempo changes
- Real-time bounce, track export and bounce in place; import of Logic projects through
  Final Cut Pro XML, MIDI files and `.logicx` bundles; a loop browser for your own loops

## Building

```bash
sudo dnf install cargo cmake gcc-c++ gtk4-devel libadwaita-devel pipewire-jack-audio-connection-kit-devel libX11-devel
```

```bash
git submodule update --init vst3shim/vst3sdk
git -C vst3shim/vst3sdk submodule update --init base cmake pluginterfaces public.sdk
cargo build --release
```

`cargo test` runs the engine, model, plug-in and host tests. The VST3 host tests use the
SDK's sample plug-ins, built with `vst3shim/build-samples.sh`; the CLAP host test runs when
LSP's CLAP bundle is installed.

## Packaging

`packaging/make-tarball.sh` builds a source tarball that includes the VST3 SDK, for
`packaging/solstice.spec`.

## Licence

GPL-3.0-or-later. The bundled VST3 SDK is MIT licensed.
