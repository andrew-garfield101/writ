"""Contract tests for the handoff and why bindings (sprint 5, U.3).

Both are read only: nothing under .writ/objects, .writ/seals or .writ/specs
changes. Handoff's next action is doctor's, from the same code path.
"""

from pathlib import Path

import writ

HANDOFF_KEYS = {
    "window",
    "agents",
    "specs",
    "merges",
    "stale_rewrites",
    "unsealed",
    "doctor",
    "next_action",
}
SPEC_KEYS = {"id", "title", "seals", "state", "agents"}
MERGE_KEYS = {"path", "specs", "resolution", "detail"}
WHY_KEYS = {"path", "history", "merge"}
ENTRY_KEYS = {"seal", "spec", "agent", "timestamp", "change", "added", "removed", "override"}


def _snapshot(root: Path) -> dict[str, bytes]:
    out: dict[str, bytes] = {}
    for part in ("objects", "seals", "specs"):
        for p in sorted((root / ".writ" / part).rglob("*")):
            if p.is_file():
                out[str(p)] = p.read_bytes()
    return out


def _seal(repo: writ.Repository, agent: str, spec: str, paths: list[str]) -> dict:
    return repo.seal(
        summary=f"{agent} work",
        agent_id=agent,
        agent_type="agent",
        spec_id=spec,
        paths=paths,
    )


def _two_specs_one_file(tmp_path: Path) -> writ.Repository:
    """sa (alpha) and sb (bravo) both seal shared.txt; sb builds on sa."""
    repo = writ.Repository.init(str(tmp_path))
    (tmp_path / "shared.txt").write_text("".join(f"line {i}\n" for i in range(20)))
    repo.seal(summary="base", agent_id="setup", agent_type="agent", status="in-progress")
    for spec, agent in (("sa", "alpha"), ("sb", "bravo")):
        repo.add_spec(id=spec, title=f"spec {spec}")
        repo.spec_claim(spec, agent)
    text = (tmp_path / "shared.txt").read_text()
    (tmp_path / "shared.txt").write_text(text.replace("line 2\n", "line 2 A\n"))
    _seal(repo, "alpha", "sa", ["shared.txt"])
    text = (tmp_path / "shared.txt").read_text()
    (tmp_path / "shared.txt").write_text(text.replace("line 9\n", "line 9 B\n"))
    _seal(repo, "bravo", "sb", ["shared.txt"])
    return repo


class TestHandoff:
    def test_shape_and_names_both_specs_and_the_merge(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        h = repo.handoff(since="2000-01-01")
        assert HANDOFF_KEYS <= set(h)
        assert sorted(s["id"] for s in h["specs"]) == ["sa", "sb"]
        for s in h["specs"]:
            assert SPEC_KEYS <= set(s)
            assert s["seals"] == 1
            assert "in progress" in s["state"]
        assert {"alpha", "bravo"} <= set(h["agents"])
        assert [m["path"] for m in h["merges"]] == ["shared.txt"]
        m = h["merges"][0]
        assert MERGE_KEYS <= set(m)
        assert m["specs"] == ["sa", "sb"]
        assert m["resolution"] == "kept"

    def test_next_action_is_doctors(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        assert repo.handoff()["next_action"] == repo.doctor()["next_action"]

    def test_unsealed_work_names_the_last_sealer(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        (tmp_path / "shared.txt").write_text("rewritten\n")
        u = repo.handoff(since="2000-01-01")["unsealed"]
        assert u == [{"path": "shared.txt", "agent": "bravo"}]

    def test_text_formats_and_read_only(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        before = _snapshot(tmp_path)
        md = repo.handoff_text(since="2000-01-01")
        brief = repo.handoff_text(since="2000-01-01", format="brief")
        assert md.startswith("# Handoff since")
        assert "shared.txt" in md and "sa" in md and "sb" in md
        assert len(brief.encode()) <= 2048
        assert _snapshot(tmp_path) == before


class TestWhy:
    def test_history_in_order_with_added_lines(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        before = _snapshot(tmp_path)
        w = repo.why("shared.txt")
        assert _snapshot(tmp_path) == before
        assert WHY_KEYS <= set(w)
        entries = [e for e in w["history"] if e["spec"]]
        for e in entries:
            assert ENTRY_KEYS <= set(e)
            assert e["override"] is None
        assert [e["spec"] for e in entries] == ["sa", "sb"]
        assert "line 2 A" in entries[0]["added"]
        assert "line 9 B" in entries[1]["added"]
        assert w["merge"]["specs"] == ["sa", "sb"]
        assert w["merge"]["resolution"] == "kept"

    def test_unknown_file_has_empty_history(self, tmp_path):
        repo = _two_specs_one_file(tmp_path)
        w = repo.why("nope.txt")
        assert w["history"] == [] and w["merge"] is None
