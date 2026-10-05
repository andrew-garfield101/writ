"""S.1 seal isolation: Python contract tests.

A spec's own files are its declared file_scope plus every path its seals
captured. A seal without ``paths`` takes only own files, plus files no spec
owns when no other agent holds an open claim. ``finish`` commits only what
completed specs sealed.
"""

import subprocess
from pathlib import Path

import pytest

import writ


def _changed(result: dict) -> list:
    return sorted(c["path"] for c in result["changes"])


def _git(path: Path, *args: str) -> str:
    out = subprocess.run(
        ["git", *args], cwd=path, capture_output=True, text=True, check=True
    )
    return out.stdout


@pytest.fixture
def git_repo(tmp_path: Path):
    """Git repo with one commit, then a writ repo on top."""
    _git(tmp_path, "init", "-q")
    _git(tmp_path, "config", "user.name", "writ-test")
    _git(tmp_path, "config", "user.email", "writ-test@localhost")
    (tmp_path / ".gitignore").write_text(".writ/\n")
    (tmp_path / "README.md").write_text("base\n")
    _git(tmp_path, "add", "-A")
    _git(tmp_path, "commit", "-q", "-m", "base")
    repo = writ.Repository.init(str(tmp_path))
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
        allow_empty=True,
    )
    return repo, tmp_path


def test_solo_later_seal_still_captures_new_files(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="feat", title="Feature")
    (tmp_path / "a.py").write_text("a\n")
    repo.seal(
        summary="one",
        agent_id="solo",
        agent_type="agent",
        status="in-progress",
        spec_id="feat",
    )

    (tmp_path / "a.py").write_text("a2\n")
    (tmp_path / "new.py").write_text("new\n")
    result = repo.seal(
        summary="two",
        agent_id="solo",
        agent_type="agent",
        status="in-progress",
        spec_id="feat",
    )

    assert _changed(result) == ["a.py", "new.py"]
    assert "left_out" not in result


def test_two_agents_default_seal_raises_with_paths_hint(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="A")
    repo.add_spec(id="sb", title="B")
    repo.spec_claim("sa", "agent-a")
    repo.spec_claim("sb", "agent-b")
    (tmp_path / "a.py").write_text("a\n")
    (tmp_path / "b.py").write_text("b\n")

    with pytest.raises(writ.WritError, match="--paths"):
        repo.seal(
            summary="a work",
            agent_id="agent-a",
            agent_type="agent",
            status="in-progress",
            spec_id="sa",
        )

    result = repo.seal(
        summary="a work",
        agent_id="agent-a",
        agent_type="agent",
        status="in-progress",
        spec_id="sa",
        paths=["a.py"],
    )
    assert _changed(result) == ["a.py"]


def test_left_out_reports_other_specs_files_and_a_seal_hint(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="A")
    repo.add_spec(id="sb", title="B")
    repo.spec_claim("sa", "agent-a")
    repo.spec_claim("sb", "agent-b")
    (tmp_path / "a.py").write_text("a\n")
    (tmp_path / "b.py").write_text("b\n")
    repo.seal(
        summary="a",
        agent_id="agent-a",
        agent_type="agent",
        status="in-progress",
        spec_id="sa",
        paths=["a.py"],
    )
    repo.seal(
        summary="b",
        agent_id="agent-b",
        agent_type="agent",
        status="in-progress",
        spec_id="sb",
        paths=["b.py"],
    )

    (tmp_path / "a.py").write_text("a2\n")
    (tmp_path / "b.py").write_text("b2\n")
    (tmp_path / "stray.py").write_text("whose?\n")
    result = repo.seal(
        summary="a again",
        agent_id="agent-a",
        agent_type="agent",
        status="in-progress",
        spec_id="sa",
    )

    assert _changed(result) == ["a.py"]
    left = result["left_out"]
    assert left["other_specs"] == [{"path": "b.py", "spec_id": "sb"}]
    assert left["unowned"] == ["stray.py"]
    assert any("writ seal" in h for h in result["hints"]), result["hints"]


def test_add_spec_file_scope_bounds_default_seal(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    spec = repo.add_spec(id="web", title="Web", file_scope=["web/"])
    assert spec["file_scope"] == ["web/"]
    (tmp_path / "web").mkdir()
    (tmp_path / "web" / "app.ts").write_text("x\n")
    (tmp_path / "notes.md").write_text("n\n")

    result = repo.seal(
        summary="web",
        agent_id="solo",
        agent_type="agent",
        status="in-progress",
        spec_id="web",
    )

    assert _changed(result) == ["web/app.ts"]
    assert result["left_out"]["unowned"] == ["notes.md"]


def test_finish_commits_only_completed_specs_sealed_paths(git_repo):
    repo, path = git_repo
    repo.add_spec(id="done", title="Done")
    repo.add_spec(id="wip", title="WIP")
    (path / "done.py").write_text("done\n")
    repo.seal(
        summary="done",
        agent_id="agent-a",
        agent_type="agent",
        status="in-progress",
        spec_id="done",
        paths=["done.py"],
    )
    (path / "wip.py").write_text("wip\n")
    repo.seal(
        summary="wip",
        agent_id="agent-b",
        agent_type="agent",
        status="in-progress",
        spec_id="wip",
        paths=["wip.py"],
    )
    repo.spec_done("done")
    (path / "stray.py").write_text("stray\n")

    result = repo.finish(strategy="single")

    committed = set(
        _git(path, "show", "--name-only", "--pretty=format:", "HEAD").split()
    )
    assert committed == {"done.py"}
    assert result["specs_finished"] == 1
    left = result["left_out"]
    assert "stray.py" in left["unsealed"]
    assert ("wip.py", "wip") in [tuple(x) for x in left["in_progress"]]


def test_seal_default_status_is_in_progress(tmp_path):
    """S.1 (e): seal() without status leaves the spec open, matching the CLI."""
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="feat", title="Feature")
    (tmp_path / "a.py").write_text("a\n")
    result = repo.seal(summary="work", agent_id="a1", agent_type="agent", spec_id="feat")
    assert result["status"] in ("in-progress", "InProgress", "in_progress")
    assert repo.get_spec("feat")["status"] != "complete"
