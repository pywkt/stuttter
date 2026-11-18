# stuttter

A stutter plugin for [Bitwig Studio](https://www.bitwig.com/): it chops,
repeats, reverses and tape-stops whatever a track feeds it, in time with the
song. You build the effect out of **step blocks** on a grid — click a step,
choose what it does — instead of drawing curves.

It works by recording everything that comes in and, for every sample it
sends out, choosing how far back in that recording to read. It never reads
ahead, so it adds no latency to the track.

- **Sixteen patterns**, and one parameter that picks between them. **Pattern**
  is `Off` or 1–16, with seventeen fixed positions, so a controller or an
  automation lane steps between whole patterns instead of sliding across
  them. A change waits for the end of the step that is playing, so it never
  cuts a repeat half way through a slice.
- **Or play them from notes.** Switch on **Notes** and sixteen keys play
  patterns 1 to 16, starting right on the note: for as long as each note is
  held, or with **Latch** on until the next note. Put the keys in a range
  your instrument does not use — above the pads of a drum machine, say — and
  stutters can be drawn as notes in the arranger instead of as automation.
  Between notes nothing plays.
- **A grid, not a curve.** Each pattern is a row of 1 to 64 steps, each one a
  note value long (1/8, 1/8T, 1/16, 1/16T or 1/32). Over them sit a **Time
  row** of blocks, stretched across as many steps as you like, a **Volume
  row** holding one level per step, and a **filter** (low-, high- or
  band-pass) with its cutoff and resonance drawn step by step on rows of
  their own. Like the volume, the filter only acts while the pattern plays.
- **Blocks you can combine.** Every block has a **loop** (off, or 1/1 down
  to 1/64 plus triplets), a **source** (its own start, a fixed spot anywhere
  earlier in the song, or *continue* from where the previous block left off)
  and a **speed**
  ramped from start to end between −2 and +2, with pitch following speed the
  way it does on tape. One-click presets cover **Repeat**, **Reverse**,
  **Half speed**, **Double speed**, **Tape stop** and **Tape start**; from
  there a repeat can rise in pitch, and three repeats of a beat can end in a
  tape stop of that same beat.
- **Squared up with the bar.** Steps are counted from the start of the song,
  not from the moment you switched the pattern on, so a pattern always lands
  where you expect however you bring it in. Loops and jumps in the arranger
  are followed; with the transport stopped it plays live.
- **Nothing clicks.** Wherever the read position jumps — a block starting, a
  repeat wrapping round, a pattern change, the transport moving — the old
  read fades out while the new one fades in, over a crossfade you set with
  **Smoothing** (0.5 to 20 ms). Volume and filter steps are ramped over the
  same time.
- **Steps that play live.** A step with no block over it passes the input
  through, and a pattern with nothing on it is a finished pattern that does
  nothing — there is no empty state to get out of.
- **A waveform view** showing one pass of the pattern, with what came in
  drawn dim grey and what went out drawn over it in the colour of the block
  that played it, and where every block reads from marked in its colour — so
  you can see what a block is actually doing to the sound, and where it is
  taking it from. Drag the selected
  block's box along the waveform to choose what it repeats.
- **A block panel** beside the grid with every setting of the selected block
  on screen at once, and a sentence saying what it does. The window is
  resizable.
- **Built for performing.** Blocks move and stretch by dragging, levels are
  drawn by sweeping across the row, and every change is heard straight away
  — a block dragged while the song plays moves under the playhead.
- **Starter patterns** to begin from — a stutter build, a tape stop ending,
  reversed snares, half-time, a gate and a filter sweep — and **copy a whole
  pattern** onto another, or clear one, all from the right-click menu on its
  number.
- **Undo and redo** for everything you edit in the window (Ctrl+Z /
  Ctrl+Shift+Z / Ctrl+Y), a hundred steps deep, one step per gesture.
- **An On top switch**, so the window can stay in front of Bitwig or stack
  with it like any other pair of windows.
- **Projects carry their patterns.** All sixteen are saved with the project,
  along with the window's own preferences, and they come back whether or not
  you ever open the window.
- **Mix** (0 to 100 %) to blend the effect against the untouched input, and a
  remote-controls page holding Pattern, Mix and Smoothing for a controller.
- **The same look as bitstep and chopdrop**, the step sequencer and sample
  player by the same author, so the three read as a set side by side.

stuttter is an audio effect: one stereo input and one stereo output, plus a
note input for playing the patterns (it sends no notes). It keeps the last 64
seconds of input to read from.

## Status

- **CLAP** plugin, **Linux** (x86_64) only for now.
- Version 0.1.0. No prebuilt downloads yet: you build it from source, which
  [takes a few minutes](docs/install.md).

## Getting started

1. [Install stuttter](docs/install.md): build it, copy it into `~/.clap`,
   and load it in Bitwig.
2. Read the [user guide](docs/user-guide.md): the three parameters, what each
   action does, building a pattern block by block, drawing the Volume row,
   the waveform view, and the keyboard shortcuts.

Quick version, if you already have [Rust](https://rustup.rs) and the build
dependencies listed in the install guide:

```sh
cargo xtask bundle stuttter --release
mkdir -p ~/.clap && cp target/bundled/stuttter.clap ~/.clap/
```

Using the **Flatpak** version of Bitwig, or a rolling-release distro? Build
it in a container instead; [here's why, and how](docs/install.md#1-build).

## Building and testing

```sh
cargo build --workspace                  # build everything
cargo test --workspace                   # run the test suite
cargo xtask bundle stuttter --release  # make target/bundled/stuttter.clap
```

The Rust version is pinned in `rust-toolchain.toml`; rustup installs it
automatically. The plugin is built with
[nice-plug](https://crates.io/crates/nice-plug) (a fork of NIH-plug) and
[egui](https://github.com/emilk/egui), and needs no patched dependencies.

## More for Bitwig

Other Bitwig projects by the same author:

- [bitstep](https://github.com/pywkt/bitstep): a step sequencer that sends notes, with per-lane lengths and rates, 64 patterns and a song mode.
- [chopdrop](https://github.com/pywkt/chopdrop): a sample player laid out as lanes, with a chop editor, 16 outputs and time-stretching.
- [Concord](https://github.com/pywkt/concord): play one key, hear a full chord, voiced and strummed.
- [knobbox](https://github.com/pywkt/knobbox): a DIY USB MIDI controller whose knobs follow the selected device.

## License

[ISC](LICENSE)
