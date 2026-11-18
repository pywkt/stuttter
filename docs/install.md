# Installing stuttter

stuttter is a CLAP audio effect. Right now it is **Linux only** (x86_64)
and there are no prebuilt downloads, so you build it from source. It takes a
few minutes the first time.

This is an early version, but the effect itself is finished: the device
loads, its three parameters work, and its window is where you build the
sixteen patterns — the Time row of blocks, the Volume row of levels, and the
waveform view of the result. See the [user guide](user-guide.md) for what
each part does.

## What you need

- **Bitwig Studio** with CLAP support (Bitwig added CLAP in version 4.3; any
  current version works).
- **Rust**, installed through [rustup](https://rustup.rs). You don't need to
  pick a version: the repository's `rust-toolchain.toml` pins one, and rustup
  downloads it automatically the first time you build.
- **A C toolchain and the X11/OpenGL development libraries** for the plugin
  window:

  ```sh
  # Arch / Manjaro
  sudo pacman -S --needed base-devel libx11 libxcb xcb-util-wm libxcursor libxkbcommon libglvnd

  # Debian / Ubuntu
  sudo apt install build-essential pkg-config libgl-dev libx11-xcb-dev libxcb1-dev \
    libxcb-dri2-0-dev libxcb-icccm4-dev libxcursor-dev libxkbcommon-dev \
    libxcb-shape0-dev libxcb-xfixes0-dev
  ```

No audio libraries are needed: the plugin only ever talks to the host.

## 1. Build

Get the source code (on GitHub, **Code → Download ZIP**, or `git clone` the
repository), then from its root folder run:

```sh
cargo xtask bundle stuttter --release
```

The finished plugin is `target/bundled/stuttter.clap` (a single file on
Linux).

> **Using the Flatpak version of Bitwig, or a rolling-release distro like
> Arch?** A plugin only loads if it was built against a C library (glibc) no
> newer than the one Bitwig runs with. The Flatpak brings its own, older
> glibc, so a plugin built on an up-to-date system can fail to load. Build it
> inside an Ubuntu 24.04 container instead — one command, and you don't need
> Rust or the libraries above installed on your own system, only Docker:
>
> ```sh
> ./scripts/build-in-docker.sh
> ```
>
> (Run it with `sudo` if your user isn't in the `docker` group.) The script
> builds the container image from `docker/Dockerfile` — Ubuntu 24.04, the
> same pinned Rust version, the libraries listed above — then runs the same
> bundle command inside it and hands the finished files back to you. The
> plugin lands in the same place, `target/bundled/stuttter.clap`, and the
> result works with the Flatpak and with most Linux systems.
>
> The first run takes a few minutes to set up the container. After that the
> image is reused and the downloaded crates are kept in a Docker volume
> (`stuttter-cargo-registry`), so later runs are much quicker.

## 2. Install

Copy it into your user CLAP folder:

```sh
mkdir -p ~/.clap
cp target/bundled/stuttter.clap ~/.clap/
```

To install it for every user on the machine instead, copy it to
`/usr/lib/clap/` (needs `sudo`).

> **Developing stuttter?** Symlink instead of copying, so every rebuild is
> picked up without reinstalling:
> `ln -s "$PWD/target/bundled/stuttter.clap" ~/.clap/`

Copy or replace the file with Bitwig closed. Bitwig only scans `~/.clap` when
a project is open, and it remembers a plugin that failed to load: that verdict
is cached in `~/.BitwigStudio/cache/clap-metadata-13`, so if a first attempt
failed (the wrong glibc, say), a corrected build can still be ignored. The fix
is to move that folder aside, which also only works with Bitwig closed — see
[Troubleshooting](#troubleshooting).

## 3. Load it in Bitwig

1. Open **Settings → Locations → Plug-in Locations** and check that `~/.clap`
   is scanned (it is by default).
2. Click **Rescan**, or restart Bitwig.
3. In the device browser, search for **stuttter**. It is an **audio
   effect**, so it goes anywhere in a track's device chain, after whatever
   makes the sound.
4. Click the device's plugin-window button to open its window.

If it installed correctly:

- stuttter is listed with the audio FX, not with the instruments or the
  note FX.
- On the track it shows a stereo audio input and a stereo audio output, and a
  note input (for playing the patterns from notes) but no note output.
- The device has three parameters — **Pattern**, **Mix** and **Smoothing** —
  and they appear on a remote-controls page called "Main".
- Its window opens at a fixed size, with a bar of pattern buttons across the
  top and the Time row, the Volume row and the waveform view under it, all in
  readable text.

Continue with the [user guide](user-guide.md).

## Updating

Pull the new source, rebuild (step 1), copy the file again (step 2), then
rescan in Bitwig or restart it. Bitwig caches plugin information, so a rescan
is the safest way to make sure it loads the new build.

## Uninstalling

```sh
rm ~/.clap/stuttter.clap
```

Projects that used stuttter will open with the device marked as missing.

## Troubleshooting

| Problem | What to try |
|---|---|
| stuttter doesn't appear in the browser | Check the file is at `~/.clap/stuttter.clap`, then rescan. Then look for `stuttter` in Bitwig's log: `grep -i stuttter ~/.BitwigStudio/log/*.log` |
| The log says ``version `GLIBC_2.xx' not found`` | The plugin was built on a system with a newer C library than Bitwig uses. Build it in the container (see the note under [step 1](#1-build)), copy it again and rescan. |
| It still doesn't load after a fixed build | Bitwig remembers the failure. With Bitwig closed, move `~/.BitwigStudio/cache/clap-metadata-13` aside, then start Bitwig and rescan. |
| The build fails with a missing `xcb`, `X11` or `GL` library | Install the system packages listed under [What you need](#what-you-need). |
| The window opens but shows no text | The plugin was built without egui's bundled fonts. Build with the command in step 1 from an unmodified checkout. |
| Bitwig still runs the old version after an update | Rescan plug-ins or restart Bitwig. |
| The effect doesn't change the sound | A new instance starts with all sixteen patterns empty, and an empty pattern passes audio through untouched: open the window and click a step onto the Time row. Check too that **Pattern** is not on `Off` and **Mix** is not at 0 %. See the [user guide](user-guide.md). |

## Checking a build (developers)

To see the newest glibc a build needs (it must be no newer than the one
Bitwig runs with; the Flatpak's runtime has 2.42):

```sh
objdump -T target/bundled/stuttter.clap | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1
```

A build from `./scripts/build-in-docker.sh` can never ask for more than
Ubuntu 24.04's glibc 2.39, which is what makes it safe for the Flatpak.

To run the CLAP validator against a build:

```sh
clap-validator validate target/bundled/stuttter.clap
```

It should exit 0. Use `validate <path>` rather than `list plugins`, because
clap-validator 0.4.1's scanner doesn't see flat `.clap` files on Linux.
