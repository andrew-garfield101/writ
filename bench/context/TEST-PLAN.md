# B.2 test plan: Context on a Budget (spec `tests-context`)

Owner: Bri. Status: **A.1, A.2, A.3, A.4 groups landed** (2026-10-04). Finding 25 fixed by Amis (34af3d741ea8); test green.

## Placement and conventions

- **Rust, A.1 group (landed):** `ignore::tests::repo_level` in `crates/writ-core/src/ignore.rs` (CC's call), real `Repository` in a tempdir. Context checks walk the serialized JSON, so A.2 struct changes do not break them.
- **Rust L2 (repo level, A.2+):** new `crates/writ-core/tests/context_budget.rs`, real I/O in a `tempfile` repo through `Repository::init` / `seal` / `context`. Written only after Amis reports each task (Amis owns `crates/` while in flight).
- **Rust L1:** Amis already landed 26 matcher unit tests in `ignore.rs` (`test_root_dir_glob_matches_venv_variants`, `test_writignore_overrides_gitignore`, `test_load_reads_live_gitignore_every_time`, ...). B.2 does not duplicate them. B.2 tests the behavior *through context and seal*, where the leaks actually showed up.
- **Python L3:** `crates/writ-py/tests/test_context_budget.py`, `tmp_repo` fixture from `conftest.py`, Python 3.10+.
- **Bench gate:** `bench/bench-context.sh target/release/writ` must exit 0. It is the acceptance test for the byte and latency numbers; unit tests assert structure, not exact sizes.
- Shared helper (Rust): `fn messy_repo(n_tracked, n_modified, ignored: &[(&str, usize)]) -> (TempDir, Repository)` mirroring `bench/context/fixture.py` at small scale (e.g. 200 tracked, 120 modified, 3 ignored trees x 50).
- Every test name states the scenario. Arrange, act, assert.

## A.1 `ctx-ignore`: live `.gitignore`, path globs

| # | Layer | Test | Assertion |
|---|---|---|---|
| 1 | Rust | `test_context_excludes_root_dir_glob_venv_variants` | `.gitignore` has `.venv*/`; files in `.venv/`, `.venv311/`, `.venv-x/` exist; no path in `working_state.*`, `pending_changes.files`, or `file_scope` starts with any of them. |
| 2 | Rust | `test_context_excludes_nested_double_star_node_modules` | `**/node_modules/` ignores `node_modules/` and `pkg/a/node_modules/`; zero matching paths in context. |
| 3 | Rust | `test_context_excludes_trailing_double_star_build` | `build/**` ignores everything under `build/`, keeps `src/build.rs`. |
| 4 | Rust | `test_gitignore_rule_added_after_init_applies_without_reinit` | Init, then append `dist/` to `.gitignore`, create `dist/x.js`; next `context()` lists no `dist/` path and `tracked_count` excludes it. Regression for root cause 1. |
| 5 | Rust | `test_writignore_rule_wins_over_gitignore` | `.gitignore` ignores `gen/`, `.writignore` has `!gen/keep.rs` (or the documented override form); `gen/keep.rs` appears, `gen/other.rs` does not. Skipped with reason if negation is out of scope. |
| 6 | Rust | `test_writ_dir_always_ignored_even_if_unignored` | `.writignore` contains `!.writ/`; no `.writ/` path anywhere in context or seal changes. |
| 7 | Rust | `test_seal_does_not_capture_gitignored_files` | Create files in `.venv311/` and `src/`; `seal()` changes contain only `src/` paths. |
| 8 | Rust | `test_previously_tracked_file_now_ignored_is_hidden_not_deleted` | File tracked at init, later covered by a new `.gitignore` rule: not reported as `deleted` in `working_state` and not in `file_scope`; seal records no deletion. Pins Amis's "hidden from state" choice. |
| 9 | Rust | `test_no_gitignore_falls_back_to_writignore_defaults` | Repo with no `.gitignore`: `target/`, `node_modules/` still excluded. |
| 10 | Rust | `test_nested_gitignore_file_behavior_documented` | Subdir `.gitignore`: asserts whatever A.1 documents (root only today). Fails loudly if behavior changes silently. |
| 11 | Python | `test_context_has_no_gitignored_paths` | Fixture with `.venv311/`, `build/` (added post-init); `json.dumps(repo.context())` contains neither prefix at a path boundary. |
| 12 | Python | `test_tracked_count_excludes_ignored_trees` | `context()["working_state"]["tracked_count"]` equals the count of non-ignored files written. |
| 12a | Python | `test_context_json_format_has_no_gitignored_paths` | Same as 11 through `format="json"`. |
| 12b | Python | `test_seal_does_not_capture_gitignored_paths` | Seal after post-init edits contains exactly the 8 sources and `.gitignore`. |
| 51 | Rust | `test_rejected_scope_violation_seal_leaves_object_count_unchanged` | Strict scope rejection writes zero objects. **Fails today: 26 objects (25 blobs + 1 tree) orphaned.** `#[ignore]` until sprint 2 (finding 17). |
| 52 | Rust | `test_rejected_revoked_agent_seal_leaves_object_count_unchanged` | Revoked-agent rejection writes zero objects. Passes (check precedes the store). |

A.1 group results: Rust 12 new in `repo_level` (11 pass, 1 ignored = #51); Python 4 pass. #8 note: `recent_seals` and `agent_activity.files_owned` still name now-ignored files from history; live views (`working_state`, `pending_changes`, `file_scope`) do not. #10 pins root-only `.gitignore`: nested `.gitignore` files are not honored, unlike git. Documented known gap (scorecard #16); sprint 2 item via the `ignore` crate, at which point #10 flips to asserting the nested rule applies.

## A.2 `ctx-budget`: caps, budget trimming, scoping

| # | Layer | Test | Assertion |
|---|---|---|---|
| 13 | Rust | `test_pending_changes_capped_at_default_50` | 120 modified files; `pending_changes.files.len() == 50`, `truncated == true`, `omitted == 70`. |
| 14 | Rust | `test_pending_totals_exact_when_truncated` | Same repo; `files_changed == 120`, `total_additions`/`total_deletions` equal the sum computed independently over all 120 files. |
| 15 | Rust | `test_working_state_lists_capped_with_markers` | 80 new, 80 modified, 80 deleted; each list `len() <= 50`, per-list (or documented aggregate) `omitted` correct, `clean == false`. |
| 16 | Rust | `test_no_truncation_markers_under_cap` | 10 modified files: `truncated` false or absent, `omitted` 0 or absent; all 10 listed. |
| 17 | Rust | `test_max_files_zero_is_unlimited` | `max_files = 0`: all 120 listed, `truncated` false, `file_scope` lists all tracked files. |
| 18 | Rust | `test_cap_orders_spec_files_first_then_mtime` | Spec `s1` sealed `a/*`; modify 60 files incl. 5 in `a/`; with `--spec s1` the first 5 entries are the `a/` files; the remainder is sorted by mtime descending. |
| 19 | Rust | `test_file_scope_capped_with_exact_tracked_count` | 300 tracked, zero changes; `file_scope.len() == 50`, `truncated`, `tracked_files == 300`. Regression for finding 6. |
| 20 | Rust | `test_spec_scope_limits_pending_to_spec_files` | Two specs with disjoint sealed files, both modified; `context(Spec(s1))` pending lists only `s1` files, totals reflect the scoped set (or document that totals stay global). |
| 21 | Rust | `test_for_agent_scope_limits_pending_and_file_scope` | Same, via `ForAgent`; observed today: `--for-agent bri` lists Amis's `crates/` files (see seal 22d02ab3aabb context). |
| 22 | Rust | `test_budget_output_fits_requested_bytes` | For budgets 2048, 4096, 8192, 32768: serialized output (default format) `len() <= budget`. |
| 23 | Rust | `test_budget_trims_file_lists_before_file_scope_before_seals` | Budget chosen so only lists trim: `file_scope` and 10 seals intact. Smaller: `file_scope` trimmed, seals intact. Smaller: `recent_seals.len() == 3`, never below 3. |
| 24 | Rust | `test_budget_never_drops_protected_fields` | At the smallest achievable budget: `all_specs`, `recommended_action`, `integration_risk`, `chain_integrity` present and identical to the unbudgeted output. |
| 25 | Rust | `test_budget_below_floor_reports_not_fits` | Budget 256: output is the floor, and a flag or stderr warning says the budget could not be met (no panic, no empty output). |
| 26 | Rust | `test_budget_applies_per_format` | Budget 8192 with `json`, `json-compact`, TOON: each `<= 8192`. |
| 27 | Python | `test_context_max_files_kwarg` | `repo.context(max_files=5)` lists 5, `truncated` True, `omitted == n-5`. |
| 28 | Python | `test_context_budget_kwarg_fits` | `len(json.dumps(repo.context(budget=8192)))` within budget for the format the binding serializes; protected keys present. |
| 29 | Python | `test_context_defaults_unchanged_for_small_repo` | Under-cap repo: output equals pre-sprint shape (no markers), so existing callers are unaffected. |
| 30 | Python | `test_context_rejects_negative_budget` | `budget=-1` raises `ValueError` (or `OverflowError` from PyO3), never silently ignored. |

A.2 group results (against seal 15cc026158d5): Rust 15/15 in `crates/writ-core/tests/context_budget.rs` (#13-26 plus `test_agent_files_owned_capped_with_omitted`); Python 7 new tests (#27-30 plus `test_context_budget_string_formats_fit`, `test_context_budget_below_floor_flags_exceeded`, negative `max_files`), 11/11 in the file. Deviations from plan: #18 rewritten because `--spec`/`--for-agent` now *filter* pending to the spec's files, so spec-first ordering inside a mixed list is unreachable through a scoped call; it now asserts scoped lists are newest-first and that Full scope with cap 3 keeps the 3 newest. #23 is a budget sweep (90% steps from full size) asserting stage order at every step rather than three hand-picked budgets. #25 asserts `budget_exceeded` plus seals at floor 3.

## A.3 `ctx-brief`: useful brief

| # | Layer | Test | Assertion |
|---|---|---|---|
| 31 | Rust | `test_brief_contains_all_specs_fields` | Each spec has `id`, `slug`, `status`, `agent`, `seal_count`, matching `spec status`. |
| 32 | Rust | `test_brief_contains_last_three_seals` | 6 seals: exactly 3, newest first, each with `id`, `agent`, `summary`, `timestamp`. |
| 33 | Rust | `test_brief_pending_counts_match_full_context` | `changed/added/modified/deleted/additions/deletions` equal the uncapped totals from full context. |
| 34 | Rust | `test_brief_has_risk_action_chain` | `integration_risk` level and score, `recommended_action`, `chain_integrity` present. |
| 35 | Rust | `test_brief_lists_no_file_paths` | Brief output contains no tracked file path. |
| 36 | Rust | `test_brief_under_2kb_on_messy_repo` | `messy_repo` with 3 specs, 6 seals, 1,000 changes: `len() <= 2048`. |
| 37 | Rust | `test_brief_is_valid_toon` | Round-trips through the TOON decoder used by `format.rs`. |
| 38 | Python | `test_brief_keys_present` | `repo.context(format="brief")` decodes and has all keys from 31 to 34. |

## A.4 `init-hooks`: SessionStart only

| # | Layer | Test | Assertion |
|---|---|---|---|
| 39 | Python | `test_init_writes_single_sessionstart_hook_brief` (in `test_init_hooks_integration.py`) | Fresh `writ init -y`: `.claude/settings.json` has exactly one `SessionStart` writ entry, command contains `--format brief`, no `UserPromptSubmit` key. |
| 40 | Python | `test_init_replaces_legacy_two_hook_block` | Seed settings with the 0.2.0 two-hook block; after init, same as 39, no duplicates. |
| 41 | Python | `test_init_preserves_user_hooks` | A user's unrelated `UserPromptSubmit` hook survives init. |
| 42 | Python | `test_uninit_removes_exactly_what_init_wrote` | Init then uninit: settings equal the pre-init file byte for byte (modulo formatting). |
| 43 | Rust | update existing `hooks.rs` tests | Any assertion on `UserPromptSubmit` flips to its absence. |

## Isolation (reserved for sprint 2 `seal-isolation`; findings 13 and 14)

Written as `xfail(strict=True)` / `#[ignore = "seal-isolation"]` now so they flip to failures-as-success when the fix lands.

| # | Layer | Test | Assertion |
|---|---|---|---|
| 44 | Rust | `test_seal_without_paths_does_not_capture_other_agents_pending` | Agents A and B each claim a spec with `file_scope`; both have pending files; A seals without `--paths`; A's seal contains only A's files, B's stay pending. |
| 45 | Rust | `test_seal_without_paths_no_file_scope_two_agents` | Same, `file_scope` unset: documents the chosen behavior (attribute by claim? reject?). |
| 46 | Rust | `test_spec_done_does_not_sweep_other_agents_pending` | B has pending files; A runs `spec done`; final seal contains none of B's files. Live repro: seal 22d02ab3aabb (`ctx-ignore`) swept 6 `bench/` files. |
| 47 | Rust | `test_enforce_scope_filters_instead_of_warning` | With scope enforcement on, out-of-scope files are left pending, not sealed with a warning. |
| 48 | Python | `test_seal_result_exposes_hints_and_file_scope_warning` | `repo.seal(...)` result has `hints` and `file_scope_warning`; CLI `writ seal --format json` emits them (finding 14). |

## Bench and L4

| # | Layer | Check | Assertion |
|---|---|---|---|
| 49 | Bench | `bench/bench-context.sh target/release/writ` | Exit 0: default <= 32 KB, brief <= 2 KB, `--budget 8192` <= 8 KB, every case median <= 300 ms, binary <= 13,000,000 B, zero leaked ignored paths. |
| 50 | L4 | `testing/scenarios/context/messy_repo_budget.yaml` | Same fixture shape at 100-agent scale, asserting caps hold with 100 specs (brief growth is linear in specs; record bytes per spec). |

Known limitations: latency medians on a shared laptop vary about 20% run to run (baseline fixture default: 212 ms and 262 ms in two runs), which is close to the 300 ms budget. CI should use `--runs 7` and the median. Seal ids and timestamps vary, so fixture byte counts jitter by a few bytes per run.

## Finding 20 triage: 18 pre-existing Python failures (no test changes yet)

17 in `test_workspaces.py`, 1 in `test_roundtrip_basic.py`; all fail identically on 0.2.0 (`/opt/homebrew/bin/writ`) and the dev build: `no changes to seal`.

- **The tests were passing vacuously.** Rebuilt `e7c0d98` (the tests' last edit, 2026-03-14): the workspace seal "passes" by sweeping 25 init-generated root files (`.claude/commands/*`, `CLAUDE.md`, `.mcp.json`, ...). The edit the test makes in `.writ/ws/auth/src/app.py` is **not** in the seal.
- **What exposed it:** `880af38` (2026-03-20, "add clean genesis tree snapshot updates", shipped in v0.1.0) added the post-init `writ-bridge` seal that captures those generated files. With nothing left to sweep, the seal correctly finds nothing.
- **Underlying product bug (latent since the tests were written):** a seal run from a `writ workspace create` dir opens the project root via `.writ-workspace` and computes state there; the workspace tree lives under `.writ/ws/<name>/`, which is always ignored, so workspace edits are never sealable.
- Bisect was not possible commit by commit: 41 of 45 commits between `e7c0d98` and `v0.1.0` do not build in isolation (core changes committed separately from CLI changes).
- **Recommendation:** fix the product for the 16 tests that edit inside a workspace dir (or retire `writ workspace create` in favor of `writ task` workspaces at `workspaces/<id>/` and port the tests). Fix the test for `test_seal_has_workspace_field` and `test_finish_dry_run_no_changes` (seal with no change; add a real edit or `allow_empty`). Then strengthen every workspace test to assert the edited path appears in the seal's changes, so it cannot pass by sweeping.

### Finding 20 resolution (sprint 1, cleared by CC)

- 16 workspace-dir tests marked `@WORKSPACE_SEAL_XFAIL` (`xfail(strict=True)`, reason cites finding 20 and spec `workspace-seal`).
- Pure test bugs fixed with real edits: `test_seal_has_workspace_field` (edits `src/app.py`), `test_finish_dry_run_no_changes` (writes `notes.md`).
- Hardening: all 43 CLI seals and 3 binding seals in `test_workspaces.py` now assert the edited path is in the seal (`assert_sealed` / `assert_api_sealed`). `assert_sealed` also checks the seal's diff adds the file's on-disk first line from the directory the seal ran in.
- The content check exposed 2 more vacuous passes: `test_seals_from_different_workspaces_have_different_tags` and `test_context_shows_only_workspace_seals`. Their workspace seal re-captured **main's** copy of `src/app.py` through the spec baseline (`writ show --diff`: `+print('main')`), not the workspace edit. Both marked with the same xfail: 18 in `test_workspaces.py`, plus `e2e/tests/test_s2_workspaces.py::test_seals_tagged_with_workspace` (same root cause, found by the full-suite run): **19 total**.
- Permanent gate: `bench/test-gate.sh` (0 failures both languages, Rust `#[ignore]` must carry a reason, Python 0 XPASS); scorecard prints it.
- Stale xfail removed: `test_init_hooks_integration.py::test_init_yes_no_claude_flag` (BRI-B1) XPASSed after A.4; marker replaced with a comment.
- Flake observed once: `test_sdk.py::TestAgent::test_agent_context_manager` failed in the full run, passed alone and with its file (77/77). Tracked as finding 21 candidate; watch in the gate.
- Finding 22: `test_cap_orders_spec_files_first_then_mtime` now pins mtimes to a fixed epoch (not `now()`), `sync_all`s, and reads each mtime back with an assertion. 45/45 consecutive runs green.

## A.3 / A.4 group results

- A.3 (against a33e52d31ab7), Rust in `tests/context_budget.rs`: #31-37 plus `test_brief_lists_open_specs_and_counts_completed` (open = pending, in-progress, blocked; completed only counted; CC decision), `test_brief_caps_active_specs_at_20_with_omitted` (a blocked spec counts toward the cap), `test_brief_omits_specs_omitted_when_under_cap`. 10/10 green. #37 decodes the TOON body with `toon_format::decode_default` and compares `pending` to the struct.
- A.4 (against 1d5cb31ae196): Amis's 4 tests cover #39, #40, reinit, uninit-keeps-user-hooks. Added in `test_init_hooks_integration.py::TestSessionStartHookEndToEnd`:
  - `test_session_start_hook_runs_and_emits_brief_within_budget`: runs the generated hook with bash; exit 0, `context-brief` header, total output <= 2,048 B.
  - `test_init_replaces_pathless_legacy_hooks`: legacy `writ context` without a path in both events is replaced.
  - `test_uninit_restores_preexisting_settings_exactly`: finding 25 (init then uninit left `"instructions": []`). Was red; green after Amis's 34af3d741ea8.
  - `test_uninit_after_fresh_init_leaves_no_writ_hooks`.
  - `test_hook_uses_the_binary_that_ran_init`: **strict xfail, finding 24**. The hook embeds `which writ` at init time, not `current_exe`. A dev init on this laptop pins `/opt/homebrew/bin/writ` (0.2.0), and `2>/dev/null || true` hides any error. Sprint 2.

## Finding 23 repro: diff quality and cost (sprint 2 diff-quality spec, do not fix in sprint 1)

`crates/writ-core/src/diff.rs::compute_line_diff` has two paths split at `LCS_LINE_LIMIT = 10_000` lines (either side):

1. **Over the limit: wrong output.** `compute_linear_diff` walks greedily with an 8-line resync window (`find_resync(.., 8)`). An insert or delete of 8+ contiguous lines never resyncs, and the rest of the file is reported as removed and re-added.
2. **Under the limit: very expensive.** A full O(m*n) `usize` LCS table. A 9,000-line file with a one-line append costs **748 MB RSS and 380 ms** for both `writ diff --stat` and `writ context`. This is the likely source of the remaining writ-repo context latency (`repo.rs` is 33,683 lines; any 10k-line source file in the tree pays the table cost).

Deterministic repro (unique lines `let v{i} = {i};`, insert k lines after line 10, writ dev build vs `git diff --no-index --numstat`):

| base lines | inserted | writ | git | writ diff wall |
|---:|---:|---|---|---:|
| 10,001 | 8 | +9999 -9991 | +8 -0 | 41 ms |
| 10,001 | 9 | +10000 -9991 | +9 -0 | 42 ms |
| 10,001 | 20 | +10011 -9991 | +20 -0 | 41 ms |
| 10,001 (every 3rd line `}`) | 20 | +6686 -6666 | +20 -0 | 41 ms |
| 9,999 | 20 (crosses limit) | +10009 -9989 | +20 -0 | 41 ms |
| 9,000 | 20 | +20 -0 (correct) | +20 -0 | 389 ms |
| real `repo.rs` HEAD vs working tree | n/a | +10327 -9943 | +395 -11 | 40 ms |

Pinned as Rust tests in `crates/writ-core/tests/diff_quality.rs`: two `#[ignore = "finding 23: ..."]` tests (`test_large_file_block_insert_reports_only_inserted_lines`, `test_insert_that_crosses_line_limit_is_still_minimal`) that fail with exactly the numbers above under `--include-ignored`, and one green control under the limit. Suggested direction for sprint 2: Myers or histogram diff (e.g. the `similar` or `imara-diff` crates), linear space, no line limit.

## Sprint 1 final gate (2026-10-05T00:48Z, `bench/test-gate.sh`)

PASS. Rust 2,137 passed, 0 failed, 3 ignored (all with reasons: finding 17 x1, finding 23 x2). Python 872 passed, 0 failed, 0 errors, 0 XPASS, 21 xfailed, 10 skipped.
Python xfails: 19 finding 20 (`workspace-seal`, strict), 1 finding 24 (strict), 1 pre-existing non-strict `test_writ.py::test_binary_file_merge_does_not_corrupt`.
Gate fix: an earlier run reported 1 Rust failure and undercounted, because cargo stops at the first failing test binary. The gate now uses `--no-fail-fast` and keeps both logs in `bench/context/results/`. That failure did not reproduce in two full runs since (finding 26 candidate: an unidentified flaky Rust test).

## Finding 28: gc prunes live data (sprint 2 gc-integrity)

`crates/writ-core/tests/store_integrity.rs`, all in temp repos:

| test | state | asserts |
|---|---|---|
| `test_carried_forward_blob_is_not_orphaned` | ignored, f28 | Seal 1 adds `carried.txt` + `edited.txt`; seal 2 edits only `edited.txt`. Scanning against seal 2, `carried.txt`'s blob (in seal 2's tree, not its changes) is not orphaned. Fails today: the tree is never walked. |
| `test_index_only_blob_is_not_orphaned_after_bridge_import` | ignored, f28 (bridge feature) | git repo + untracked file, `init_project`, all seals loaded as `gc run` does: the untracked file's blob, referenced only by the workspace index, is not orphaned. Fails today. |
| `test_verify_all_chains_fails_when_referenced_blob_missing` | ignored, f28 | Delete a tree-referenced blob; `verify_all_chains().all_valid` must be false. Fails today (still true). |
| `test_orphan_scan_with_full_history_keeps_carried_blob` | green (control) | With full history the first seal's change list still names the blob, which is why unit tests never caught it. |
| `test_context_fails_loudly_when_referenced_blob_missing` | green (pins symptom) | `diff()` errors on a missing blob rather than guessing. |

**Root cause is wider than first reported.** There are two missing live roots, not one: (1) seal trees are not walked; (2) workspace indexes are not read. The second explains the bridge-import case: the import seal's tree holds only git-committed files, while untracked files and `.writignore` are stored and referenced only by the index.

**End-to-end repro, 6 commands, throwaway repo:** git repo with `a.txt` committed and `untracked.txt` untracked; `writ init -y --bare`; `writ gc run --yes`; append to `untracked.txt`; `writ diff --stat` gives `error: object not found: 0263829989b6...`. `bench/store_integrity.py` reports 0 missing before the gc run and 2 after.

**Correction to my earlier findings:** finding 12 ("91% orphaned") and finding 19 ("init stores gitignored files no seal references") were misdiagnosed. Those objects were live: referenced by seal trees and the index. The audit's orphan count was the bug. I said a prune would be free; it was not.

**Gate:** `bench/test-gate.sh` now carries an explicit expected-ignored allowlist (findings 17, 23 x2, 28 x3). A new ignore, or an expected one that starts running, fails the gate.
**Scorecard:** new Store integrity section from `bench/store_integrity.py`: referenced-but-missing objects across every seal tree, change hash and workspace index; must be 0. It is read-only and decodes zstd trees with the `zstd` CLI. It was not run against the writ repo while CC repairs it.

## Store integrity after repair (2026-10-05)

`bench/store_integrity.py` on the writ repo: 20 seals, 7 spec genesis trees, 1,588 index entries, 1,574 distinct referenced objects. Result: 3 missing, exactly the 3 permanently lost junk blobs (`ae01d21c123e`, `50ff4a66d3af`, `80dbd0cf0eac`). The scorecard excuses those 3 for this repo only (finding 28 reference; other repos get no allowances, and an allowance with no match is reported). With them excused, 0 missing: **PASS**. The metric now walks spec `genesis_tree`s too, matching the sprint 2 live-set definition.

Store composition: `.writ` is 144 MB, not 8.7 MB. 1,294 regenerated blobs were written raw (magic `0x00`, 138.8 MB); the other 279 objects are zstd (7.8 MB). `writ gc audit` still reports 1,296 orphaned objects (138.8 MB). Those are the regenerated **live** blobs, misclassified by finding 28. **A `writ gc run` today would delete them again.** Do not run gc until sprint 2 gc-integrity lands.

## Latency profile: `writ context` on the writ repo (dev build, 2026-10-05)

Wall time, median of 7 after 1 warm-up, release build on PATH:

| command | bytes | median |
|---|---:|---:|
| `context` (TOON) | 15,292 | 216 ms |
| `context --format json` | 18,859 | 210 ms |
| `context --format brief` | 997 | 211 ms |
| `context --max-files 0` | 21,339 | 217 ms |
| `context --spec tests-context` | 10,489 | 215 ms |
| `context --for-agent bri` | 11,063 | 213 ms |
| `context --budget 8192` | 9,494 | 220 ms |
| `status` | 1,380 | 112 ms |
| `diff --stat` | 176 | 108 ms |
| `log --limit 10` / `spec status` / `--version` | | 16 / 10 / 4 ms |

Trend on the writ repo: 1,167 ms (0.2.0) to 816 ms (A.1 build, mid-sprint) to **216 ms** now, under the 300 ms budget. Brief costs the same as full context, because size is not where the time goes.

**In-process breakdown** (`Repository` calls against writ-core, best of 5): open 5.3 ms, `IgnoreRules::load` 0.3, **`state()` 89.0**, **`diff()` 90.7**, `log()` 0.2, `log_all()` 1.3, `verify_all_chains` 1.7, `context(Full)` 185.1, `context_limited` 185.3 (caps and budget add nothing).

Where the time goes:
1. **Full-content SHA-256 of every tracked file on every call.** `sample` of a `state()` loop: 2,968 of 3,227 samples (92%) are in `sha2::sha256::compress256` under `state::compute_state`. Each call hashes the whole working tree: 208 files, 9.3 MB, about 113 MB/s (software SHA-256). There is no stat/mtime cache, so unchanged files are re-read and re-hashed every time. File reads are 4%, ignore matching 2%, the directory walk 1%.
2. **`context` computes the working state twice.** It calls `state()`, then `diff()`, and `diff()` calls `state()` again. That is 2 x 89 ms of hashing for one context call, about 85% of the 216 ms wall time.
3. **Finding 23 is the conditional third cost, and it explains the 816 ms.** Once a modified file is under 10,000 lines, the O(m*n) LCS table is built for it on every context call. Repro, copies of real files in a temp repo: clean 29 ms. A one-line append to `main.rs` (9,394 lines) gives **328 ms and 780 MB RSS**. Adding `context.rs` (1,963 lines) gives 339 ms. Mid-sprint, `main.rs` was in Amis's working set, which accounts for most of the 816 ms. Today's pending files are all under 200 lines, so it is not showing in the 216 ms. Files over 10,000 lines (`repo.rs`, 34,067) take the cheap but wrong linear path instead.

Recommendations for sprint 2, ordered by ms saved per call on this repo:
- Compute working state once per context call and pass it to the diff: about -90 ms.
- Stat cache (size + mtime + inode in the index; re-hash only on mismatch, as git does): about -85 ms of the remaining state cost.
- diff-quality (finding 23): linear-space Myers/histogram diff removes the 300+ ms / 780 MB spike whenever a large file is modified.
- Hardware SHA-256 (`sha2` `asm` feature, or BLAKE3, already a dependency, for content addressing) is a smaller win once the cache exists.
