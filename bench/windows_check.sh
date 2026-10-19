#!/usr/bin/env bash
# Windows cross-check for the gate (finding 185, sprint 6 retro rule 1).
#
# 0.4.3's tag failed on Windows (std::os::unix in auto_seal.rs) because nothing
# before the push compiled the tree for Windows: CI has no Windows job, only the
# Release workflow builds it, after the tag. This type checks writ-cli and every
# crate it pulls in for x86_64-pc-windows-msvc on this machine.
#
# What it checks: all Rust code under cfg(windows), the class of 185. What it does
# not: C dependencies (zstd-sys) and linking. A macOS host has no MSVC headers, so
# the C compiler and archiver for that target are stubs that write empty outputs;
# `cargo check` never links, so the stubbed objects are never used. A C level
# Windows break still shows only in the Release workflow.
#
# Verified 2026-10-18: on the e353935 tree (the failed 0.4.3 tag) it fails with
# E0433 "cannot find `unix` in `os`" at crates/writ-core/src/auto_seal.rs:487;
# on 5d30ff7 and later it passes.
#
# Usage: bench/windows_check.sh [LOG]   exit 0 pass, 1 compile error, 2 cannot run
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO="${CARGO:-$(command -v cargo || echo "$HOME/.cargo/bin/cargo")}"
RUSTUP="${RUSTUP:-$(command -v rustup || echo "$HOME/.cargo/bin/rustup")}"
TARGET=x86_64-pc-windows-msvc
LOG="${1:-/dev/stdout}"
cd "$ROOT"
[ -x "$RUSTUP" ] || { echo "windows check: rustup not found, cannot cross-check" >"$LOG"; exit 2; }
# A target (the standard library for Windows), not a toolchain; reads rust-toolchain.toml.
if ! "$RUSTUP" target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    "$RUSTUP" target add "$TARGET" >"$LOG" 2>&1 || { echo "windows check: rustup target add $TARGET failed" >>"$LOG"; exit 2; }
fi
STUB="$(mktemp -d "${TMPDIR:-/tmp}/writ-wincheck.XXXXXX")"
trap 'rm -rf "$STUB"' EXIT
cat >"$STUB/cc" <<'SH'
#!/bin/sh
out=""; while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift;; -o*) out="${1#-o}";; esac; shift; done
[ -n "$out" ] && : >"$out"; exit 0
SH
cat >"$STUB/ar" <<'SH'
#!/bin/sh
for a in "$@"; do case "$a" in -out:*|/OUT:*) : >"${a#*:}"; exit 0;; *.a|*.lib) : >"$a"; exit 0;; esac; done; exit 0
SH
chmod +x "$STUB/cc" "$STUB/ar"
# Own target dir so the gate's host build cache is untouched.
CC_x86_64_pc_windows_msvc="$STUB/cc" AR_x86_64_pc_windows_msvc="$STUB/ar" \
    "$CARGO" check -p writ-cli --target "$TARGET" --target-dir "$ROOT/target/windows-check" >>"$LOG" 2>&1
rc=$?
[ $rc -eq 0 ] || exit 1
exit 0
