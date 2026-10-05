"""S.2 finish-per-spec through the Python binding."""

import subprocess
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


def test_finish_per_spec_commits_in_dependency_order(tmp_path):
    """S.2: per-spec finish orders by depends_on, not completion time."""
    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q")
    git("config", "user.name", "t")
    git("config", "user.email", "t@localhost")
    (tmp_path / ".gitignore").write_text(".writ/\n")
    (tmp_path / "README.md").write_text("base\n")
    git("add", "-A")
    git("commit", "-q", "-m", "base")
    repo = writ.Repository.init(str(tmp_path))
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
