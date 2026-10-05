"""Binding passthrough for repo.context(max_files=..., budget=...) (ctx-budget A.2).

Bri's B.2 suite (test_context_budget.py) owns the full contract; this file only
pins that the keyword arguments reach the core and change the output.
"""

import json
from pathlib import Path

import writ

N_PENDING = 80


def _repo(tmp_path: Path) -> "writ.Repository":
    repo = writ.Repository.init(str(tmp_path))
    (tmp_path / "seed.txt").write_text("seed\n")
    repo.seal(summary="baseline", agent_id="setup", agent_type="agent")
    for i in range(N_PENDING):
        (tmp_path / f"f{i:03}.txt").write_text(f"{i}\n")
    return repo


def test_default_caps_pending_changes_at_50(tmp_path):
    ctx = _repo(tmp_path).context()
    pending = ctx["pending_changes"]
    assert len(pending["files"]) == 50
    assert pending["truncated"] is True
    assert pending["omitted"] == N_PENDING - 50
    assert pending["files_changed"] == N_PENDING


def test_max_files_zero_is_unlimited(tmp_path):
    ctx = _repo(tmp_path).context(max_files=0)
    assert len(ctx["pending_changes"]["files"]) == N_PENDING
    assert "truncated" not in ctx["pending_changes"]


def test_max_files_custom_cap(tmp_path):
    ctx = _repo(tmp_path).context(max_files=5)
    assert len(ctx["working_state"]["new_files"]) == 5
    assert ctx["working_state"]["counts"]["new"] == N_PENDING


def test_budget_is_measured_in_requested_format(tmp_path):
    repo = _repo(tmp_path)
    for fmt in ("json", "json-compact", "toon"):
        out = repo.context(format=fmt, budget=4096)
        assert len(out) <= 4096, fmt


def test_budget_with_dict_measures_compact_json(tmp_path):
    ctx = _repo(tmp_path).context(budget=4096)
    assert len(json.dumps(ctx, separators=(",", ":"))) <= 4096
    assert ctx["pending_changes"]["files_changed"] == N_PENDING


def test_brief_format_is_small_toon_with_required_sections(tmp_path):
    out = _repo(tmp_path).context(format="brief")
    assert out.startswith("# writ context-brief")
    for key in ("specs", "seals[", "pending:", "risk:"):
        assert key in out, key
    assert "f000.txt" not in out
    assert len(out) <= 2048


def test_brief_dict_has_exact_pending_counts(tmp_path):
    brief = _repo(tmp_path).context(format="brief-dict")
    assert set(brief) >= {"scope", "specs", "seals", "pending", "risk"}
    assert brief["pending"]["files"] == N_PENDING
    assert brief["pending"]["new"] == N_PENDING
    assert brief["risk"]["level"] in ("low", "medium", "high")
