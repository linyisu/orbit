#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
profile=${1:-release}
case "$profile" in
    release) native_flags=(--release) ;;
    debug) native_flags=() ;;
    *) printf 'Usage: %s [release|debug]\n' "$0" >&2; exit 2 ;;
esac

host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
    x86_64-unknown-linux-*) architecture=x86_64 ;;
    aarch64-unknown-linux-*) architecture=aarch64 ;;
    *) printf 'Build the Linux bundle on x86-64 or AArch64 Linux.\n' >&2; exit 1 ;;
esac

target="$architecture-unknown-linux-musl"
cc_variable="CC_${target//-/_}"
linker_variable="CARGO_TARGET_${target^^}_LINKER"
linker_variable=${linker_variable//-/_}
flags_variable="CARGO_TARGET_${target^^}_RUSTFLAGS"
flags_variable=${flags_variable//-/_}
compiler=${!cc_variable:-musl-gcc}
command -v "$compiler" >/dev/null || {
    printf 'Install musl-tools (musl-gcc), then rustup target add %s.\n' "$target" >&2
    exit 1
}

env "$cc_variable=$compiler" "$linker_variable=${!linker_variable:-cc}" \
    "$flags_variable=${!flags_variable:-} -C target-feature=+crt-static" \
    cargo build --locked --release --target "$target" -p orbit-cli
cargo build --locked "${native_flags[@]}" -p orbit-cli

output=${CARGO_TARGET_DIR:-target}
server="$output/$target/release/orbit"
case "$(readelf -l "$server") $(readelf -d "$server")" in
    *INTERP*|*NEEDED*) printf 'The workspace server must be statically linked.\n' >&2; exit 1 ;;
esac

companion="orbit-server-$architecture-linux-musl"
install -m 755 "$server" "$output/$profile/$companion"
mkdir -p "$output/bundle"
install -m 755 "$output/$profile/orbit" "$output/bundle/orbit"
install -m 755 "$server" "$output/bundle/$companion"
printf 'Bundle: %s/bundle (keep both executables together)\n' "$output"
