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
ref=${MINISATIP_REF:-master}

if [ ! -d "$src/.git" ]; then
    git clone --depth 1 --branch "$ref" "$repo" "$src"
fi
cd "$src"

# The patch adds src/replay.cpp and a REPLAY CMake option (off by default).
if ! grep -q find_replay_adapter src/adapter.cpp; then
    git apply "$root/harness/cpp-replay.patch"
fi

# Minimal oracle by default: the optional backends are off, as in the Rust
# default build. --full keeps CA/CSA/DDCI for the CA milestones.
opts=(-DCMAKE_BUILD_TYPE=Release -DREPLAY=ON -DSRT=OFF -DNETCVCLIENT=OFF)
if [ "$full" = 1 ]; then
    opts+=(-DDVBCSA=ON -DDVBCA=ON -DDDCI=ON)
else
    opts+=(-DDVBCSA=OFF -DDVBCA=OFF -DDDCI=OFF)
fi
cmake -S . -B build-harness "${opts[@]}" ${CPP_CMAKE_ARGS:-}
cmake --build build-harness -j"$(nproc)"
echo "$src/build-harness/minisatip"
