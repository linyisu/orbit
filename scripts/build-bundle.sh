#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
profile=${1:-release}
case "$profile" in
    release) native_flags=(--release) ;;
    debug) native_flags=() ;;
    *) printf 'Usage: %s [release|debug]\n' "$0" >&2; exit 2 ;;
esac

cargo build --locked "${native_flags[@]}" -p orbit-cli

output=${CARGO_TARGET_DIR:-target}
mkdir -p "$output/bundle"
rm -f "$output/bundle/orbit-server-x86_64-linux-musl" \
    "$output/bundle/orbit-server-aarch64-linux-musl"
install -m 755 "$output/$profile/orbit" "$output/bundle/orbit"
printf 'Bundle: %s/bundle/orbit (workspace server embedded)\n' "$output"
