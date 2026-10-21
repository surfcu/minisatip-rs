#!/usr/bin/env bash
# Type-check the workspace for the static musl targets built by the CI `cross`
# job (PLAN.md §2.5). Check only (lib/bin targets, like CI's build), so no
# cross linker or C toolchain is needed; link errors are not caught.
#
# Usage: scripts/check-cross.sh [target...]
# Stable targets need `rustup target add <target>`; mipsel (tier 3) needs a
# nightly toolchain with the rust-src component for -Zbuild-std.
set -euo pipefail

cd "$(dirname "$0")/.."

stable_targets=(
    x86_64-unknown-linux-musl
    armv7-unknown-linux-musleabihf
    aarch64-unknown-linux-musl
)
build_std_targets=(
    mipsel-unknown-linux-musl
)

if [ "$#" -gt 0 ]; then
    targets=("$@")
else
    targets=("${stable_targets[@]}" "${build_std_targets[@]}")
fi

failed=()
for target in "${targets[@]}"; do
    echo "==> cargo check --workspace --target $target"
    if [[ " ${build_std_targets[*]} " == *" $target "* ]]; then
        cmd=(cargo +nightly check -Zbuild-std=std,panic_abort --workspace --target "$target")
    else
        cmd=(cargo check --workspace --target "$target")
    fi
    if ! "${cmd[@]}"; then
        failed+=("$target")
    fi
done

if [ "${#failed[@]}" -gt 0 ]; then
    echo "check-cross: failed for: ${failed[*]}" >&2
    exit 1
fi
echo "check-cross: all targets passed"
