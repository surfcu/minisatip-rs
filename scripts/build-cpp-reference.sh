#!/usr/bin/env bash
# Builds the C++ reference minisatip with the harness file-replay tuner.
# Usage: scripts/build-cpp-reference.sh [--full] [SRC_DIR]; prints the binary path last.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
full=0
if [ "${1:-}" = "--full" ]; then
    full=1
    shift
fi
src=${1:-$root/target/upstream/minisatip}
repo=${MINISATIP_REPO:-https://github.com/catalinii/minisatip.git}
# The port's baseline (README); upstream master moves on and stops taking the patch.
ref=${MINISATIP_REF:-28dfd7820159ef22333af51ea0d5ef5bc56014b7}
patch=$root/harness/cpp-replay.patch

if [ ! -d "$src/.git" ]; then
    # A fetch by name takes a branch, a tag or a full commit id.
    git init -q "$src"
    git -C "$src" remote add origin "$repo"
    git -C "$src" fetch -q --depth 1 origin "$ref"
    git -C "$src" checkout -q FETCH_HEAD
fi
cd "$src"

# The patch adds src/replay.cpp and a REPLAY CMake option (off by default).
# A tree carrying an older revision of the patch fails here; reset it first.
if ! git apply --reverse --check "$patch" 2>/dev/null; then
    git apply "$patch"
fi

# Default oracle = the Rust default build: CA and DDCI are always compiled in
# Rust (and on by default on Linux in C++); --full adds CSA (Rust `dvbcsa`).
opts=(-DCMAKE_BUILD_TYPE=Release -DREPLAY=ON -DSRT=OFF -DNETCVCLIENT=OFF -DDVBCA=ON -DDDCI=ON)
if [ "$full" = 1 ]; then
    opts+=(-DDVBCSA=ON)
else
    opts+=(-DDVBCSA=OFF)
fi
cmake -S . -B build-harness "${opts[@]}" ${CPP_CMAKE_ARGS:-}
cmake --build build-harness -j"$(nproc)"
echo "$src/build-harness/minisatip"
