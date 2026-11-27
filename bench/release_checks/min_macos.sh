#!/usr/bin/env bash
# Finding 224 follow up: moving the macOS release builds from macos-14 to
# macos-15 changes the SDK they link against, and must not change the oldest
# macOS the binaries run on. That floor comes from rustc's default deployment
# target, not from the runner image. v0.4.5 (built on macos-14, SDK 14.5):
#   writ-aarch64-apple-darwin  LC_BUILD_VERSION      minos   11.0
#   writ-x86_64-apple-darwin   LC_VERSION_MIN_MACOSX version 10.12
# This script asserts the downloaded release binary still says the same.
#
# Usage:
#   min_macos.sh <binary | writ-<arch>-apple-darwin.tar.gz> [<expected>]
#     <expected> defaults by architecture: arm64 11.0, x86_64 10.12.
#   At publish time, on the release assets:
#     gh release download vX.Y.Z -p 'writ-*-apple-darwin.tar.gz' -D "$d"
#     for t in "$d"/*.tar.gz; do bench/release_checks/min_macos.sh "$t"; done
#
# Needs otool, which ships with the Xcode command line tools
# (`xcode-select --install`); no other dependency. Runs on macOS only.
# Prints one PASS/FAIL line per check; exit 0 all PASS, 1 any FAIL,
# 2 bad arguments, missing otool, or a file that is not a thin Mach-O binary.
set -uo pipefail

usage() { echo "usage: $0 <binary | writ-<arch>-apple-darwin.tar.gz> [<expected minos>]" >&2; exit 2; }

[ $# -ge 1 ] && [ $# -le 2 ] || usage
input="$1"
expected="${2:-}"
[ -f "$input" ] || { echo "min_macos: no such file: $input" >&2; exit 2; }

if ! command -v otool >/dev/null 2>&1; then
    echo "min_macos: otool not found. It ships with the Xcode command line tools:" >&2
    echo "  xcode-select --install" >&2
    exit 2
fi

bin="$input"
case "$input" in
    *.tar.gz|*.tgz)
        work="$(mktemp -d)"
        trap 'rm -rf "$work"' EXIT
        tar -xzf "$input" -C "$work" || { echo "min_macos: cannot extract $input" >&2; exit 2; }
        bin="$(find "$work" -type f -name writ | head -1)"
        [ -n "$bin" ] || { echo "min_macos: no writ binary inside $input" >&2; exit 2; }
        ;;
esac

# Architecture from the Mach-O header. A fat binary or a non Mach-O file is
# not what the release ships, so it is a usage error, not a FAIL.
header="$(otool -hv "$bin" 2>/dev/null)" || { echo "min_macos: not a Mach-O file: $bin" >&2; exit 2; }
ncpu="$(printf '%s\n' "$header" | grep -cE '^ *MH_MAGIC')"
[ "$ncpu" -ge 1 ] || { echo "min_macos: not a Mach-O binary: $bin" >&2; exit 2; }
[ "$ncpu" -eq 1 ] || { echo "min_macos: expected one architecture, found $ncpu in $bin" >&2; exit 2; }
if printf '%s\n' "$header" | grep -qE 'MH_MAGIC_64 +ARM64'; then
    arch=arm64; default=11.0
elif printf '%s\n' "$header" | grep -qE 'MH_MAGIC_64 +X86_64'; then
    arch=x86_64; default=10.12
else
    echo "min_macos: unknown architecture in $bin" >&2; exit 2
fi
expected="${expected:-$default}"

# LC_BUILD_VERSION carries `minos X`; the older LC_VERSION_MIN_MACOSX carries
# `version X`. Take the first of either.
minos="$(otool -l "$bin" | awk '
    /cmd LC_BUILD_VERSION$/      { want="minos" }
    /cmd LC_VERSION_MIN_MACOSX$/ { want="version" }
    want != "" && $1 == want     { print $2; exit }
')"
sdk="$(otool -l "$bin" | awk '$1 == "sdk" { print $2; exit }')"

fail=0
if [ -n "$minos" ]; then
    echo "PASS load command with the minimum macOS present ($arch)"
else
    echo "FAIL load command with the minimum macOS present ($arch): neither LC_BUILD_VERSION nor LC_VERSION_MIN_MACOSX"
    exit 1
fi
if [ "$minos" = "$expected" ]; then
    echo "PASS minimum macOS is $expected ($arch, sdk ${sdk:-unknown})"
else
    echo "FAIL minimum macOS is $minos, expected $expected ($arch, sdk ${sdk:-unknown})"
    fail=1
fi
exit "$fail"
