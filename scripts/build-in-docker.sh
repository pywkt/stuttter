#!/bin/sh
# Builds target/bundled/stuttter.clap inside an Ubuntu 24.04 container, so
# it loads in the Flatpak version of Bitwig and on systems with an older
# glibc.
#
#   ./scripts/build-in-docker.sh          (or with sudo if your user is not
#                                           in the docker group)
set -eu

cd "$(dirname "$0")/.."

# The files the build writes are handed back to you, even when run with sudo.
owner="${SUDO_UID:-$(id -u)}:${SUDO_GID:-$(id -g)}"

docker build -t stuttter-build docker

# The bundle is built first and its exit status kept, so the files are handed
# back even when the build fails.
bundle='cargo xtask bundle stuttter --release; status=$?'

# Downloaded crates are kept in a named volume, so later builds are faster.
docker run --rm \
    -v "$PWD":/src \
    -v stuttter-cargo-registry:/usr/local/cargo/registry \
    stuttter-build \
    sh -c "$bundle; chown -R $owner target; exit \$status"

echo
echo "Built target/bundled/stuttter.clap"
