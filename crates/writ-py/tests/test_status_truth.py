"""S.4a status truth through the Python binding: per-spec seals come from the
spec record, and status names the claim holder."""

from pathlib import Path

import writ


def _seal(repo: writ.Repository, agent: str, spec: str, path: Path, name: str) -> dict:
    (path / name).write_text(f"{agent} {name}\n")
    return repo.seal(
        summary=f"{agent} {name}",
        agent_id=agent,
        agent_type="agent",
        spec_id=spec,
        paths=[name],
    )


def _setup(tmp_path: Path) -> writ.Repository:
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="A")
    repo.add_spec(id="sb", title="B")
    repo.spec_claim("sa", "a")
    repo.spec_claim("sb", "b")
    _seal(repo, "a", "sa", tmp_path, "a1.txt")
    _seal(repo, "a", "sa", tmp_path, "a2.txt")
    _seal(repo, "b", "sb", tmp_path, "b.txt")
    return repo


def test_spec_seals_returns_only_the_specs_own_seals(tmp_path):
    repo = _setup(tmp_path)

    own = repo.spec_seals("sb")

    assert [s["spec_id"] for s in own] == ["sb"]
    assert len(repo.spec_log("sb")) > len(own), "spec_log walks the chain"


def test_spec_seals_newest_first_and_limit(tmp_path):
    repo = _setup(tmp_path)

    own = repo.spec_seals("sa")

    assert [s["changes"][0]["path"] for s in own] == ["a2.txt", "a1.txt"]
    assert len(repo.spec_seals("sa", limit=1)) == 1


def test_spec_done_completes_with_own_seal_count(tmp_path):
    repo = _setup(tmp_path)

    done = repo.spec_done("sb", summary="done", agent_id="b")

    assert len(done["sealed_by"]) == 1


def test_non_holder_spec_done_warns_naming_holder(tmp_path):
    """Finding 52: closing another agent's claimed spec is never silent."""
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S")
    repo.spec_claim("s", "ada")

    done = repo.spec_done("s", summary="d", agent_id="bea")

    claim = [h for h in done["hints"] if h.startswith("CLAIM")]
    assert claim and "'ada'" in claim[0], done["hints"]
    assert done["final_seal"] is None
