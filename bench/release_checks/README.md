# Release checks

**Rule:** every claim in a release note gets a check script here before the tag.
If the note says writ does something, a script in this directory does it on a
throwaway project and exits non zero when the claim is false. A claim without a
script is cut from the note or the tag waits.

Why: the 0.3.0 note told users to re-run `writ init -y` after upgrading. Testing
that sentence as written (13 of 19 checks on the build of the day) found two
bugs the test suite missed (findings 71 and 72), both fixed before the tag.

## Conventions

- One script per claim, named after the claim: `init_rerun.sh`.
- Arguments: `[WRIT_BIN] [SCRATCH_DIR]`. Defaults: `target/release/writ` from
  this repo and a fresh `mktemp -d`. Run from any directory.
- Never touch this repo's `.writ` or the caller's identity: work in the scratch
  dir, `unset WRIT_AGENT_ID`.
- Print one `PASS <check>` or `FAIL <check>` line per check; exit 0 only when
  every check passed, 1 on any failure, 2 on bad arguments.
- The header comment names the release note sentence the script backs.

## Running

```bash
cargo build --release -p writ-cli
for s in bench/release_checks/*.sh; do "$s" || echo "FAILED: $s"; done
```

## Index

| Script | Claim | Checks |
|---|---|---|
| `init_rerun.sh` | Re-running `writ init -y` refreshes generated files, keeps user text and settings, and leaves the store, specs and `.writ/config.toml` alone (0.3.0; config checks and the codex opt-out check added for finding 82, green from 297f927ac566) | 26 |
