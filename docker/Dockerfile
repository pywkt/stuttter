# Build environment for stuttter: Ubuntu 24.04, so the plugin links against
# an older C library (glibc 2.39) and loads in the Flatpak version of Bitwig
# and on most Linux systems. Used by scripts/build-in-docker.sh.
FROM ubuntu:24.04

# The X11/OpenGL headers the plugin window needs. No audio libraries: the
# plugin only ever talks to the host, it opens no audio device of its own and
# decodes no files.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    build-essential \
    pkg-config \
    libgl-dev \
    libx11-xcb-dev \
    libxcb1-dev \
    libxcb-dri2-0-dev \
    libxcb-icccm4-dev \
    libxcursor-dev \
    libxkbcommon-dev \
    libxcb-shape0-dev \
    libxcb-xfixes0-dev \
    && rm -rf /var/lib/apt/lists/*

# Keep in step with rust-toolchain.toml: the same channel and the same
# components, so building in /src downloads no second toolchain.
ARG RUST_VERSION=1.99.1
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    --profile minimal \
    --default-toolchain "$RUST_VERSION" \
    --component clippy,rustfmt

WORKDIR /src
