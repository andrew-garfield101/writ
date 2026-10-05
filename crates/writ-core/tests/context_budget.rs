//! B.2 contract tests for "Context on a Budget" (A.2 `ctx-budget` group).
//!
//! Real repositories in tempdirs, through `Repository::context_limited`, the
//! same entry point the CLI and Python binding use. Numbering follows
//! `bench/context/TEST-PLAN.md`. Amis's unit tests in `context.rs` cover the
//! trimming primitives on synthetic `ContextOutput`s; these cover the
//! assembled output on disk-backed repos.

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tempfile::{tempdir, TempDir};

use writ_core::context::{
    ContextFilter, ContextLimits, ContextOutput, ContextScope, BUDGET_SEAL_FLOOR, DEFAULT_MAX_FILES,
};
use writ_core::format::formatter_for;
use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::{Repository, WritResult};

// ── Helpers ─────────────────────────────────────────────────────────

fn agent(id: &str) -> AgentIdentity {
    AgentIdentity {
        id: id.to_string(),
        agent_type: AgentType::Agent,
    }
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// Seal every pending file explicitly. The fixture builds history for
/// several agents in sequence, so it passes the paths the way a real agent
/// would under the S.1 default seal scope.
fn seal(repo: &Repository, who: &str, spec: Option<&str>, summary: &str) {
    let pending: Vec<String> = repo
        .state()
        .unwrap()
        .changes
        .into_iter()
        .map(|f| f.path)
        .collect();
    repo.seal_paths(
        agent(who),
        summary.to_string(),
        spec.map(String::from),
        TaskStatus::InProgress,
        Verification::default(),
        &pending,
        false,
    )
    .unwrap();
}

fn json_len(c: &ContextOutput) -> WritResult<usize> {
    Ok(serde_json::to_string(c)?.len())
}

fn ctx_with(repo: &Repository, scope: ContextScope, limits: ContextLimits) -> ContextOutput {
    repo.context_limited(scope, 10, &ContextFilter::default(), &limits, json_len)
        .unwrap()
}

fn limits(max_files: Option<usize>, budget: Option<usize>) -> ContextLimits {
    ContextLimits::from_user(max_files, budget)
}

fn to_json(ctx: &ContextOutput) -> Value {
    serde_json::to_value(ctx).unwrap()
}

fn pending_paths(ctx: &ContextOutput) -> Vec<String> {
    ctx.pending_changes
        .as_ref()
        .map(|pc| pc.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default()
}

fn ws_path_count(ctx: &ContextOutput) -> usize {
    let ws = &ctx.working_state;
    ws.new_files.len() + ws.modified_files.len() + ws.deleted_files.len()
}

const MODIFIED: usize = 80;
const ADDED: usize = 30;
const DELETED: usize = 20;
const CHANGED: usize = MODIFIED + ADDED + DELETED;

/// Mixed change set with independently known line totals:
/// 80 modified (+2/-0 each), 30 new (3 lines each), 20 deleted (2 lines each).
/// Expected totals: additions 80*2 + 30*3 = 250, deletions 20*2 = 40.
fn mixed_repo() -> (TempDir, Repository) {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let repo = Repository::init(root).unwrap();
    for i in 0..MODIFIED {
        write(root, &format!("mod/m{i:03}.txt"), "a\nb\n");
    }
    for i in 0..DELETED {
        write(root, &format!("del/d{i:03}.txt"), "x\ny\n");
    }
    seal(&repo, "setup", None, "baseline");

    for i in 0..MODIFIED {
        write(root, &format!("mod/m{i:03}.txt"), "a\nb\nc\nd\n");
    }
    for i in 0..ADDED {
        write(root, &format!("new/n{i:03}.txt"), "1\n2\n3\n");
    }
    for i in 0..DELETED {
        fs::remove_file(root.join(format!("del/d{i:03}.txt"))).unwrap();
    }
    (dir, repo)
}

const EXPECTED_ADDITIONS: usize = MODIFIED * 2 + ADDED * 3;
const EXPECTED_DELETIONS: usize = DELETED * 2;

// ── 13–17: caps, markers, exact totals ──────────────────────────────

#[test]
fn test_pending_changes_capped_at_default_50() {
    let (_dir, repo) = mixed_repo();
    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    let pc = ctx.pending_changes.as_ref().unwrap();
    assert_eq!(pc.files.len(), DEFAULT_MAX_FILES);
    assert!(pc.truncated);
    assert_eq!(pc.omitted, CHANGED - DEFAULT_MAX_FILES);
}

#[test]
fn test_pending_totals_exact_when_truncated() {
    let (_dir, repo) = mixed_repo();
    let capped = ctx_with(&repo, ContextScope::Full, limits(Some(5), None));
    let pc = capped.pending_changes.as_ref().unwrap();
    assert_eq!(pc.files.len(), 5);
    assert_eq!(pc.files_changed, CHANGED);
    assert_eq!(pc.total_additions, EXPECTED_ADDITIONS);
    assert_eq!(pc.total_deletions, EXPECTED_DELETIONS);
}

#[test]
fn test_working_state_lists_capped_with_markers() {
    let (_dir, repo) = mixed_repo();
    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    let ws = &ctx.working_state;
    assert!(!ws.clean);
    assert_eq!(
        ws_path_count(&ctx),
        DEFAULT_MAX_FILES,
        "cap is across all three lists"
    );
    assert!(ws.truncated);
    assert_eq!(ws.omitted, CHANGED - DEFAULT_MAX_FILES);
    let counts = ws.counts.expect("exact counts present when truncated");
    assert_eq!(
        (counts.new, counts.modified, counts.deleted),
        (ADDED, MODIFIED, DELETED)
    );
}

#[test]
fn test_no_truncation_markers_under_cap() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    write(dir.path(), "a.txt", "a\n");
    seal(&repo, "setup", None, "baseline");
    for i in 0..10 {
        write(dir.path(), &format!("f{i}.txt"), "x\n");
    }

    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    assert_eq!(pending_paths(&ctx).len(), 10);

    // Markers are absent from the wire format, so pre-sprint consumers see
    // the same shape for small repos.
    let v = to_json(&ctx);
    for section in ["pending_changes", "working_state"] {
        let obj = v[section].as_object().unwrap();
        for key in ["truncated", "omitted", "counts"] {
            assert!(!obj.contains_key(key), "{section}.{key} present under cap");
        }
    }
    let top = v.as_object().unwrap();
    for key in [
        "file_scope_truncated",
        "file_scope_omitted",
        "budget_exceeded",
    ] {
        assert!(!top.contains_key(key), "{key} present under cap");
    }
}

#[test]
fn test_max_files_zero_is_unlimited() {
    let (_dir, repo) = mixed_repo();
    let ctx = ctx_with(&repo, ContextScope::Full, limits(Some(0), None));
    let pc = ctx.pending_changes.as_ref().unwrap();
    assert_eq!(pc.files.len(), CHANGED);
    assert!(!pc.truncated);
    assert_eq!(ws_path_count(&ctx), CHANGED);
    assert!(!ctx.working_state.truncated);
    assert_eq!(ctx.file_scope.len(), ctx.tracked_files);
    assert!(!ctx.file_scope_truncated);
}

// ── 18: ordering ────────────────────────────────────────────────────

/// Pin a file's mtime and read it back, so ordering tests never depend on
/// write order or filesystem timestamp granularity (finding 22).
fn set_mtime(path: &Path, t: SystemTime) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(t).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let got = fs::metadata(path).unwrap().modified().unwrap();
    let delta = got.duration_since(t).unwrap_or_else(|e| e.duration());
    assert!(
        delta < Duration::from_millis(1),
        "mtime not pinned on {}: wanted {t:?}, got {got:?}",
        path.display()
    );
}

#[test]
fn test_cap_orders_spec_files_first_then_mtime() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let repo = Repository::init(root).unwrap();
    for i in 0..55 {
        write(root, &format!("other/o{i:02}.txt"), "o\n");
    }
    seal(&repo, "setup", None, "baseline");
    repo.add_spec(&Spec::new("s1".into(), "S1".into(), String::new()))
        .unwrap();
    for i in 0..5 {
        write(root, &format!("a/f{i}.txt"), "a\n");
    }
    seal(&repo, "agent-a", Some("s1"), "s1 work");

    // Modify 55 non-spec files with strictly increasing mtimes, and the 5
    // spec files with the OLDEST mtimes, so only spec priority lifts them.
    // Fixed epoch, not now(): nothing written during the test can land between.
    let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for i in 0..5 {
        let p = root.join(format!("a/f{i}.txt"));
        fs::write(&p, "a\nchanged\n").unwrap();
        set_mtime(&p, base + Duration::from_secs(i as u64));
    }
    for i in 0..55 {
        let p = root.join(format!("other/o{i:02}.txt"));
        fs::write(&p, "o\nchanged\n").unwrap();
        set_mtime(&p, base + Duration::from_secs(100 + i as u64));
    }

    // Spec scope: pending is narrowed to the spec's files (A.2 scoping).
    let scoped = ctx_with(
        &repo,
        ContextScope::Spec("s1".into()),
        limits(Some(0), None),
    );
    let expected_spec: Vec<String> = (0..5).rev().map(|i| format!("a/f{i}.txt")).collect();
    assert_eq!(
        pending_paths(&scoped),
        expected_spec,
        "spec scope, newest first"
    );

    // Full scope, cap 3: no spec priority, so the 3 newest files win and the
    // old spec files fall off the end.
    let capped = ctx_with(&repo, ContextScope::Full, limits(Some(3), None));
    assert_eq!(
        pending_paths(&capped),
        vec!["other/o54.txt", "other/o53.txt", "other/o52.txt"]
    );

    // Agent scope with cap 3: agent-a's spec files outrank newer files.
    // The agent's world here is exactly its spec, so the list is the
    // three newest spec files.
    let agent_ctx = ctx_with(
        &repo,
        ContextScope::Agent("agent-a".into()),
        limits(Some(3), None),
    );
    assert_eq!(
        pending_paths(&agent_ctx),
        vec!["a/f4.txt", "a/f3.txt", "a/f2.txt"]
    );
}

// ── 19: file_scope cap ──────────────────────────────────────────────

#[test]
fn test_file_scope_capped_with_exact_tracked_count() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    for i in 0..300 {
        write(dir.path(), &format!("src/f{i:03}.rs"), "x\n");
    }
    seal(&repo, "setup", None, "baseline");

    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    assert!(ctx.working_state.clean);
    assert_eq!(ctx.file_scope.len(), DEFAULT_MAX_FILES);
    assert!(ctx.file_scope_truncated);
    assert_eq!(ctx.file_scope_omitted, 300 - DEFAULT_MAX_FILES);
    assert_eq!(ctx.tracked_files, 300);
}

// ── 20–21: scoping ──────────────────────────────────────────────────

/// Two claimed specs with disjoint SEALED files (no declared file_scope),
/// both with pending edits, plus one unowned pending file.
fn two_spec_repo() -> (TempDir, Repository) {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let repo = Repository::init(root).unwrap();
    write(root, "shared/readme.txt", "r\n");
    seal(&repo, "setup", None, "baseline");
    for (spec, who, prefix) in [("s1", "agent-a", "one"), ("s2", "agent-b", "two")] {
        repo.add_spec(&Spec::new(spec.into(), spec.into(), String::new()))
            .unwrap();
        for i in 0..3 {
            write(root, &format!("{prefix}/f{i}.txt"), "v1\n");
        }
        seal(&repo, who, Some(spec), &format!("{spec} work"));
    }
    for prefix in ["one", "two"] {
        for i in 0..3 {
            write(root, &format!("{prefix}/f{i}.txt"), "v1\nv2\n");
        }
    }
    write(root, "shared/readme.txt", "r\nedit\n");
    (dir, repo)
}

#[test]
fn test_spec_scope_limits_pending_to_spec_files() {
    let (_dir, repo) = two_spec_repo();
    let ctx = ctx_with(
        &repo,
        ContextScope::Spec("s1".into()),
        ContextLimits::default(),
    );
    let paths = pending_paths(&ctx);
    assert!(
        !paths.is_empty() && paths.iter().all(|p| p.starts_with("one/")),
        "--spec s1 pending not scoped to s1's sealed files: {paths:?}"
    );
}

#[test]
fn test_for_agent_scope_limits_pending_and_file_scope() {
    let (_dir, repo) = two_spec_repo();
    let ctx = ctx_with(
        &repo,
        ContextScope::Agent("agent-a".into()),
        ContextLimits::default(),
    );
    let paths = pending_paths(&ctx);
    assert!(
        !paths.is_empty() && paths.iter().all(|p| p.starts_with("one/")),
        "--for-agent agent-a pending not scoped: {paths:?}"
    );
    assert!(
        ctx.file_scope.iter().all(|p| p.starts_with("one/")),
        "--for-agent agent-a file_scope not scoped: {:?}",
        ctx.file_scope
    );
}

// ── files_owned cap ─────────────────────────────────────────────────

#[test]
fn test_agent_files_owned_capped_with_omitted() {
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    for i in 0..120 {
        write(dir.path(), &format!("src/f{i:03}.rs"), "x\n");
    }
    seal(&repo, "owner", None, "own 120 files");

    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    let owner = ctx
        .agent_activity
        .iter()
        .find(|a| a.agent_id == "owner")
        .expect("owner in agent_activity");
    assert_eq!(owner.files_owned.len(), DEFAULT_MAX_FILES);
    assert_eq!(owner.files_owned_omitted, 120 - DEFAULT_MAX_FILES);

    let full = ctx_with(&repo, ContextScope::Full, limits(Some(0), None));
    let owner = full
        .agent_activity
        .iter()
        .find(|a| a.agent_id == "owner")
        .unwrap();
    assert_eq!(owner.files_owned.len(), 120);
    assert_eq!(owner.files_owned_omitted, 0);
}

// ── 22–26: budget ───────────────────────────────────────────────────

/// 12 seals, 200 tracked files, 120 pending: every trimmable section is big.
fn budget_repo() -> (TempDir, Repository) {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let repo = Repository::init(root).unwrap();
    for i in 0..200 {
        write(root, &format!("src/f{i:03}.rs"), "x\n");
    }
    seal(&repo, "setup", None, "baseline");
    for n in 0..11 {
        write(root, &format!("src/f{n:03}.rs"), &format!("x\nseal {n}\n"));
        seal(&repo, "worker", None, &format!("seal {n}"));
    }
    for i in 0..120 {
        write(root, &format!("pending/p{i:03}.txt"), "p\n");
    }
    (dir, repo)
}

#[test]
fn test_budget_output_fits_requested_bytes() {
    let (_dir, repo) = budget_repo();
    for budget in [4096, 8192, 16384, 32768] {
        let ctx = ctx_with(&repo, ContextScope::Full, limits(None, Some(budget)));
        let size = json_len(&ctx).unwrap();
        assert!(size <= budget, "budget {budget}: {size} B");
        assert!(!ctx.budget_exceeded, "budget {budget} flagged exceeded");
    }
}

#[test]
fn test_budget_trims_file_lists_before_file_scope_before_seals() {
    let (_dir, repo) = budget_repo();
    let full = ctx_with(&repo, ContextScope::Full, limits(Some(0), None));
    let full_size = json_len(&full).unwrap();
    let full_seals = full.recent_seals.len();
    assert!(full_seals > BUDGET_SEAL_FLOOR);

    let mut saw_scope_trim = false;
    let mut saw_seal_trim = false;
    let mut budget = full_size;
    while budget > 512 {
        budget = budget * 9 / 10;
        let ctx = ctx_with(&repo, ContextScope::Full, limits(Some(0), Some(budget)));
        let lists = pending_paths(&ctx).len()
            + ws_path_count(&ctx)
            + ctx
                .agent_activity
                .iter()
                .map(|a| a.files_owned.len())
                .sum::<usize>();
        if ctx.file_scope_truncated {
            saw_scope_trim = true;
            assert_eq!(
                lists, 0,
                "budget {budget}: file_scope trimmed while lists remain"
            );
        }
        if ctx.recent_seals.len() < full_seals {
            saw_seal_trim = true;
            assert!(
                ctx.file_scope.is_empty(),
                "budget {budget}: seals trimmed before file_scope"
            );
            assert_eq!(lists, 0);
        }
        assert!(
            ctx.recent_seals.len() >= BUDGET_SEAL_FLOOR,
            "budget {budget}"
        );
    }
    assert!(
        saw_scope_trim && saw_seal_trim,
        "sweep never reached later stages"
    );
}

#[test]
fn test_budget_never_drops_protected_fields() {
    let (_dir, repo) = budget_repo();
    let full = to_json(&ctx_with(&repo, ContextScope::Full, limits(Some(0), None)));
    let tiny = to_json(&ctx_with(
        &repo,
        ContextScope::Full,
        limits(Some(0), Some(1)),
    ));
    for key in [
        "all_specs",
        "recommended_action",
        "integration_risk",
        "chain_integrity",
    ] {
        assert_eq!(full.get(key), tiny.get(key), "{key} changed under budget");
    }
}

#[test]
fn test_budget_below_floor_reports_exceeded() {
    let (_dir, repo) = budget_repo();
    let ctx = ctx_with(&repo, ContextScope::Full, limits(None, Some(256)));
    assert!(ctx.budget_exceeded);
    assert_eq!(ctx.recent_seals.len(), BUDGET_SEAL_FLOOR);
    assert!(json_len(&ctx).unwrap() > 256, "floor output is non-empty");
}

#[test]
fn test_budget_applies_per_format() {
    let (_dir, repo) = budget_repo();
    for name in ["json", "json-compact", "toon"] {
        let f = formatter_for(name).unwrap();
        let measure = |c: &ContextOutput| -> WritResult<usize> { Ok(f.format_context(c)?.len()) };
        let ctx = repo
            .context_limited(
                ContextScope::Full,
                10,
                &ContextFilter::default(),
                &limits(None, Some(8192)),
                measure,
            )
            .unwrap();
        let out = f.format_context(&ctx).unwrap();
        assert!(out.len() <= 8192, "{name}: {} B", out.len());
    }
}

// ── A.3 ctx-brief (31–37 plus active-spec rules) ────────────────────

use writ_core::context::{BriefContext, BRIEF_SPEC_CAP};
use writ_core::format::format_brief_context;
use writ_core::spec::{SpecStatus, SpecUpdate};

fn brief(repo: &Repository) -> BriefContext {
    BriefContext::from_context(&ctx_with(
        repo,
        ContextScope::Full,
        ContextLimits::default(),
    ))
}

fn set_status(repo: &Repository, id: &str, status: SpecStatus) {
    repo.update_spec(
        id,
        SpecUpdate {
            status: Some(status),
            ..Default::default()
        },
    )
    .unwrap();
}

/// 3 specs, 6 seals across 2 agents, then `n_pending` unsealed new files.
fn brief_repo(n_pending: usize) -> (TempDir, Repository) {
    let dir = tempdir().unwrap();
    let root = dir.path();
    let repo = Repository::init(root).unwrap();
    write(root, "base.txt", "b\n");
    seal(&repo, "setup", None, "baseline");
    for id in ["s-alpha", "s-beta", "s-gamma"] {
        repo.add_spec(&Spec::new(id.into(), id.into(), String::new()))
            .unwrap();
    }
    let plan = [
        ("agent-a", "s-alpha"),
        ("agent-a", "s-alpha"),
        ("agent-a", "s-beta"),
        ("agent-b", "s-beta"),
        ("agent-b", "s-gamma"),
        ("agent-b", "s-gamma"),
    ];
    for (n, (who, spec)) in plan.iter().enumerate() {
        write(root, &format!("{spec}/f{n}.txt"), &format!("seal {n}\n"));
        seal(&repo, who, Some(spec), &format!("seal {n} on {spec}"));
    }
    for i in 0..n_pending {
        write(root, &format!("pending/p{i:04}.txt"), "1\n2\n");
    }
    (dir, repo)
}

#[test]
fn test_brief_contains_all_specs_fields() {
    let (_dir, repo) = brief_repo(0);
    let b = brief(&repo);
    let ids: Vec<&str> = b.specs.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    for spec in &b.specs {
        let full = repo.load_spec(&spec.id).unwrap();
        assert_eq!(spec.slug, full.slug);
        assert_eq!(spec.seals, full.sealed_by.len());
        assert_eq!(spec.seals, 2, "{} seal count", spec.id);
        assert_eq!(spec.agent, full.claimed_by.clone().unwrap_or_default());
        assert!(
            !spec.agent.is_empty(),
            "{} auto-claimed on first seal",
            spec.id
        );
        assert!(!spec.status.is_empty());
    }
}

#[test]
fn test_brief_contains_last_three_seals() {
    let (_dir, repo) = brief_repo(0);
    let b = brief(&repo);
    let summaries: Vec<&str> = b.seals.iter().map(|s| s.summary.as_str()).collect();
    assert_eq!(
        summaries,
        vec!["seal 5 on s-gamma", "seal 4 on s-gamma", "seal 3 on s-beta"]
    );
    for s in &b.seals {
        assert!(!s.id.is_empty() && !s.at.is_empty());
        assert_eq!(s.agent, "agent-b");
    }
}

#[test]
fn test_brief_pending_counts_match_full_context() {
    let (_dir, repo) = mixed_repo();
    let b = brief(&repo);
    assert_eq!(b.pending.files, CHANGED);
    assert_eq!(
        (b.pending.new, b.pending.modified, b.pending.deleted),
        (ADDED, MODIFIED, DELETED)
    );
    assert_eq!(b.pending.additions, EXPECTED_ADDITIONS);
    assert_eq!(b.pending.deletions, EXPECTED_DELETIONS);
}

#[test]
fn test_brief_has_risk_action_chain() {
    let (_dir, repo) = brief_repo(5);
    let full = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    let b = BriefContext::from_context(&full);
    assert_eq!(b.risk.level, full.integration_risk.level);
    assert_eq!(b.risk.score, full.integration_risk.score);
    assert_eq!(b.next, full.recommended_action);
    assert!(b.next.is_some(), "pending work should yield a next action");
    assert_eq!(
        b.chain_ok,
        full.chain_integrity.as_ref().map(|c| c.valid),
        "chain_ok mirrors chain_integrity"
    );
}

#[test]
fn test_brief_lists_no_file_paths() {
    let (_dir, repo) = brief_repo(40);
    let out = format_brief_context(
        &ctx_with(&repo, ContextScope::Full, ContextLimits::default()),
        None,
    )
    .unwrap();
    for needle in ["pending/p", "s-alpha/f", "base.txt"] {
        assert!(
            !out.contains(needle),
            "brief leaked a path: {needle}\n{out}"
        );
    }
}

#[test]
fn test_brief_under_2kb_on_messy_repo() {
    let (_dir, repo) = brief_repo(1_000);
    let out = format_brief_context(
        &ctx_with(&repo, ContextScope::Full, ContextLimits::default()),
        Some("bench-fixture"),
    )
    .unwrap();
    assert!(out.len() <= 2048, "brief is {} B:\n{out}", out.len());
}

#[test]
fn test_brief_is_valid_toon() {
    let (_dir, repo) = brief_repo(10);
    let ctx = ctx_with(&repo, ContextScope::Full, ContextLimits::default());
    let out = format_brief_context(&ctx, None).unwrap();
    // Drop the writ header comment line(s), then decode the TOON body.
    let body: String = out
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let decoded: Value = toon_format::decode_default(&body).unwrap();
    let expected = serde_json::to_value(BriefContext::from_context(&ctx)).unwrap();
    assert_eq!(decoded["pending"], expected["pending"]);
    assert_eq!(decoded["specs"].as_array().map(Vec::len), Some(3));
    assert_eq!(decoded["seals"].as_array().map(Vec::len), Some(3));
}

#[test]
fn test_brief_lists_open_specs_and_counts_completed() {
    let (_dir, repo) = brief_repo(0);
    for id in ["s-delta", "s-eps"] {
        repo.add_spec(&Spec::new(id.into(), id.into(), String::new()))
            .unwrap();
    }
    set_status(&repo, "s-alpha", SpecStatus::Complete);
    set_status(&repo, "s-beta", SpecStatus::InProgress);
    set_status(&repo, "s-delta", SpecStatus::Complete);
    set_status(&repo, "s-eps", SpecStatus::Blocked);
    // s-gamma is in-progress via its seals; s-delta and s-eps never sealed.
    repo.add_spec(&Spec::new("s-zeta".into(), "s-zeta".into(), String::new()))
        .unwrap();

    let b = brief(&repo);
    let mut listed: Vec<(&str, &str)> = b
        .specs
        .iter()
        .map(|s| (s.id.as_str(), s.status.as_str()))
        .collect();
    listed.sort();
    // Open = pending, in-progress, blocked (CC decision, sprint 1). A blocked
    // spec is what an agent most needs to see at task start.
    assert_eq!(
        listed,
        vec![
            ("s-beta", "in-progress"),
            ("s-eps", "blocked"),
            ("s-gamma", "in-progress"),
            ("s-zeta", "pending"),
        ],
    );
    // Completed specs are counted, never listed.
    assert!(b
        .specs
        .iter()
        .all(|s| s.id != "s-alpha" && s.id != "s-delta"));
    assert_eq!(b.specs_complete, 2);
}

#[test]
fn test_brief_caps_active_specs_at_20_with_omitted() {
    let (_dir, repo) = brief_repo(0);
    for i in 0..25 {
        let id = format!("bulk-{i:02}");
        repo.add_spec(&Spec::new(id.clone(), id, String::new()))
            .unwrap();
    }
    set_status(&repo, "s-alpha", SpecStatus::Complete);
    set_status(&repo, "bulk-00", SpecStatus::Blocked);

    let b = brief(&repo);
    let active = 2 + 25; // s-beta, s-gamma, 25 bulk (one blocked, still open)
    assert_eq!(b.specs.len(), BRIEF_SPEC_CAP);
    assert_eq!(b.specs_omitted, active - BRIEF_SPEC_CAP);
    assert_eq!(b.specs_complete, 1);
    let v = serde_json::to_value(&b).unwrap();
    assert_eq!(v["specs_omitted"], active - BRIEF_SPEC_CAP);
}

#[test]
fn test_brief_omits_specs_omitted_when_under_cap() {
    let (_dir, repo) = brief_repo(0);
    let v = serde_json::to_value(brief(&repo)).unwrap();
    assert!(v.get("specs_omitted").is_none());
    assert_eq!(v["specs_complete"], 0);
}
