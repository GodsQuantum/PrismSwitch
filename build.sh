#!/bin/sh
# SPDX-License-Identifier: GPL-2.0-only
set -eu
cd "$(dirname "$0")"
[ "$(id -u)" != 0 ] || { echo "Build as an unprivileged user." >&2; exit 1; }
for tool in cargo rustc gcc sha256sum; do
    command -v "$tool" >/dev/null || { echo "Missing dependency: $tool" >&2; exit 1; }
done
# Stable baseline: do not specialize to the build host's CPU.
export CARGO_TARGET_DIR="$PWD/target"
cargo build --release --locked
gcc -O2 -Wall -o target/release/SmcDumpKey SmcDumpKey.c
ref=source-archive
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    ref=$(git rev-parse HEAD)
    if [ -n "$(git status --porcelain)" ]; then ref="$ref-dirty"; fi
fi
{
    printf 'git=%s\n' "$ref"
    target/release/prismswitch --version
    rustc --version
    printf 'target=%s\n' "$(rustc -vV | sed -n 's/^host: //p')"
} > target/release/BUILDINFO
(cd target/release && sha256sum prismswitch SmcDumpKey BUILDINFO > SHA256SUMS)
echo "Built target/release/prismswitch"
