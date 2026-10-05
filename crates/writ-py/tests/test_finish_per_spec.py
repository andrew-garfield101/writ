"""S.2 finish-per-spec through the Python binding."""

import subprocess
from pathlib import Path

import pytest

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


def _git_writ_repo(tmp_path: Path) -> writ.Repository:
    """A git repo with one commit, `.writ/` ignored, then writ init."""

    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q")
    git("config", "user.name", "t")
    git("config", "user.email", "t@localhost")
    (tmp_path / ".gitignore").write_text(".writ/\n")
    (tmp_path / "README.md").write_text("base\n")
    git("add", "-A")
    git("commit", "-q", "-m", "base")
    return writ.Repository.init(str(tmp_path))


def test_finish_per_spec_commits_in_dependency_order(tmp_path):
    """S.2: per-spec finish orders by depends_on, not completion time."""
    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    repo = _git_writ_repo(tmp_path)
    repo.add_spec(id="sa", title="A")
    repo.add_spec(id="sb", title="B")
    repo.update_spec("sb", depends_on=["sa"])
    _seal(repo, "b", "sb", tmp_path, "b.txt")
    repo.spec_done("sb", summary="b", agent_id="b")
    _seal(repo, "a", "sa", tmp_path, "a.txt")
    repo.spec_done("sa", summary="a", agent_id="a")

    result = repo.finish(strategy="per-spec")

    assert [c["specs"] for c in result["commits"]] == [["sa"], ["sb"]]
    assert repo.get_spec("sb")["commit_state"] == "committed"


def test_seal_and_spec_done_on_committed_spec_are_refused(tmp_path):
    """Finding 65: the binding refuses like the CLI, naming the follow-up command."""
    repo = _git_writ_repo(tmp_path)
    repo.add_spec(id="sa", title="A")
    _seal(repo, "a", "sa", tmp_path, "a.txt")
    repo.spec_done("sa", summary="a", agent_id="a")
    commit = repo.finish()["commits"][0]["hash"]
    (tmp_path / "a.txt").write_text("late\n")
    before = len(repo.log_all())

    with pytest.raises(writ.WritError, match=r'writ spec add "A \(follow-up\)" --claim'):
        repo.seal(summary="late", agent_id="a", spec_id="sa", paths=["a.txt"])
    with pytest.raises(writ.WritError, match=commit[:12]):
        repo.spec_done("sa", summary="again", agent_id="a")

    assert len(repo.log_all()) == before


def test_comma_separated_file_scope_is_split(tmp_path):
    """Finding 66 through the binding."""
    repo = writ.Repository.init(str(tmp_path))
    spec = repo.add_spec(id="sa", title="A", file_scope=["app.py, tests/*"])

    assert spec["file_scope"] == ["app.py", "tests/*"]


def test_spec_done_without_summary_uses_the_title(tmp_path):
    """Finding 68 through the binding."""
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="Add login form")
    _seal(repo, "a", "sa", tmp_path, "a.txt")

    done = repo.spec_done("sa", agent_id="a")

    assert done["completion_summary"] == "Add login form"
