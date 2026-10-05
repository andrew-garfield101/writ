"""Contract tests for the "Context on a Budget" sprint (B.2).

Groups match bench/context/TEST-PLAN.md. A.1 (ignore rules) lands first;
A.2 caps/budget, A.3 brief, and A.4 hooks are added as they report.
"""

import json
from pathlib import Path

import pytest
import writ

# Ignored trees: (dir, gitignore rule, written before init?)
IGNORED_TREES = [
    (".venv311", ".venv*/", True),  # only the glob matches it
    ("web/app/node_modules", "**/node_modules/", True),
    ("build", "build/**", False),  # rule appended after init
]
SOURCE_FILES = [f"src/mod_{i}.py" for i in range(8)]
FILES_PER_TREE = 12


def _write(root: Path, rel: str, body: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)


def _all_strings(value) -> list[str]:
    """Every string anywhere in a nested dict/list structure."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [s for v in value.values() for s in _all_strings(v)]
    if isinstance(value, list):
        return [s for v in value for s in _all_strings(v)]
    return []


def _leaked(ctx: dict) -> list[str]:
    prefixes = tuple(f"{tree}/" for tree, _, _ in IGNORED_TREES)
    return [s for s in _all_strings(ctx) if s.startswith(prefixes)]


@pytest.fixture
def messy_repo(tmp_path):
    """Repo with sources plus three ignored trees, one ruled only after init."""
    rules_at_init = [rule for _, rule, at_init in IGNORED_TREES if at_init]
    (tmp_path / ".gitignore").write_text("\n".join(rules_at_init) + "\n")
    for rel in SOURCE_FILES:
        _write(tmp_path, rel, f"# {rel}\n")
    for tree, _, at_init in IGNORED_TREES:
        if at_init:
            for i in range(FILES_PER_TREE):
                _write(tmp_path, f"{tree}/pkg/gen_{i}.py", f"# {i}\n")

    repo = writ.Repository.init(str(tmp_path))
    repo.seal(summary="baseline", agent_id="bri-test", agent_type="agent",
              status="in-progress")

    # Post-init: new ignore rule and its artifacts, then edit every source file.
    with open(tmp_path / ".gitignore", "a") as fh:
        for _, rule, at_init in IGNORED_TREES:
            if not at_init:
                fh.write(rule + "\n")
    for tree, _, at_init in IGNORED_TREES:
        if not at_init:
            for i in range(FILES_PER_TREE):
                _write(tmp_path, f"{tree}/out/obj_{i}.o", f"{i}\n")
    for rel in SOURCE_FILES:
        _write(tmp_path, rel, f"# {rel} edited\n")
    return repo, tmp_path


# ── A.1 ctx-ignore ──────────────────────────────────────────────


def test_context_has_no_gitignored_paths(messy_repo):
    repo, _ = messy_repo

    ctx = repo.context()

    leaked = _leaked(ctx)
    assert leaked == [], f"gitignored paths leaked into context: {leaked[:5]}"
    assert "src/mod_0.py" in _all_strings(ctx)


def test_context_json_format_has_no_gitignored_paths(messy_repo):
    repo, _ = messy_repo

    raw = repo.context(format="json")
    ctx = json.loads(raw) if isinstance(raw, str) else raw

    assert _leaked(ctx) == []


def test_tracked_count_excludes_ignored_trees(messy_repo):
    repo, _ = messy_repo

    ctx = repo.context()

    # 8 sources + .gitignore; none of the 36 ignored files.
    assert ctx["working_state"]["tracked_count"] == len(SOURCE_FILES) + 1


def test_seal_does_not_capture_gitignored_paths(messy_repo):
    repo, _ = messy_repo

    seal = repo.seal(summary="edit sources", agent_id="bri-test", agent_type="agent",
                     status="in-progress")

    paths = [c["path"] for c in seal["changes"]]
    assert sorted(paths) == sorted(SOURCE_FILES + [".gitignore"])


# ── A.2 ctx-budget ──────────────────────────────────────────────

N_PENDING = 70
PROTECTED_KEYS = ("all_specs", "recommended_action", "integration_risk", "chain_integrity")


@pytest.fixture
def pending_repo(tmp_path):
    """Baseline of 3 files, then 70 unsealed new files across 2 dirs."""
    repo = writ.Repository.init(str(tmp_path))
    for i in range(3):
        _write(tmp_path, f"base/b{i}.txt", "b\n")
    repo.seal(summary="baseline", agent_id="bri-test", agent_type="agent",
              status="in-progress")
    for i in range(N_PENDING):
        _write(tmp_path, f"pending/p{i:03d}.txt", "1\n2\n")
    return repo, tmp_path


def test_context_max_files_kwarg(pending_repo):
    repo, _ = pending_repo

    ctx = repo.context(max_files=5)

    pc = ctx["pending_changes"]
    assert len(pc["files"]) == 5
    assert pc["truncated"] is True
    assert pc["omitted"] == N_PENDING - 5
    assert pc["files_changed"] == N_PENDING
    assert pc["total_additions"] == N_PENDING * 2
    assert ctx["working_state"]["counts"] == {"new": N_PENDING, "modified": 0, "deleted": 0}


def test_context_budget_kwarg_fits(pending_repo):
    repo, _ = pending_repo
    full = repo.context(max_files=0)

    ctx = repo.context(budget=2048)

    assert len(json.dumps(ctx, separators=(",", ":"))) <= 2048
    for key in PROTECTED_KEYS:
        assert ctx.get(key) == full.get(key), key


def test_context_budget_string_formats_fit(pending_repo):
    repo, _ = pending_repo

    for fmt in ("json", "json-compact", "toon"):
        out = repo.context(format=fmt, budget=3072)
        assert isinstance(out, str), fmt
        assert len(out) <= 3072, f"{fmt}: {len(out)} B"


def test_context_defaults_unchanged_for_small_repo(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    _write(tmp_path, "a.txt", "a\n")
    repo.seal(summary="baseline", agent_id="bri-test", agent_type="agent",
              status="in-progress")
    for i in range(4):
        _write(tmp_path, f"f{i}.txt", "x\n")

    ctx = repo.context()

    for section in ("pending_changes", "working_state"):
        for key in ("truncated", "omitted", "counts"):
            assert key not in ctx[section], f"{section}.{key}"
    for key in ("file_scope_truncated", "file_scope_omitted", "budget_exceeded"):
        assert key not in ctx, key
    assert len(ctx["pending_changes"]["files"]) == 4


def test_context_budget_below_floor_flags_exceeded(pending_repo):
    repo, _ = pending_repo

    ctx = repo.context(budget=64)

    assert ctx["budget_exceeded"] is True
    assert ctx["pending_changes"]["files_changed"] == N_PENDING


@pytest.mark.parametrize("kwargs", [{"budget": -1}, {"max_files": -1}])
def test_context_rejects_negative_limits(pending_repo, kwargs):
    repo, _ = pending_repo

    with pytest.raises((OverflowError, ValueError, TypeError)):
        repo.context(**kwargs)
