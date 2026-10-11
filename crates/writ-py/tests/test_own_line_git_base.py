"""Finding 106: the seal-time own-line check holds when the file's base came
from a plain git commit, not a writ seal (the usual case: files that predate
`writ init`). Same steps as the writ-sealed base; only the base differs."""

import subprocess
from pathlib import Path

import pytest

import writ

FILE = "shared.txt"


def _numbered(edits: dict[int, str]) -> str:
    return "".join(
        f"line {i}" + (f" {edits[i]}" if i in edits else "") + "\n" for i in range(40)
    )


def _git(root: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@localhost", *args],
        cwd=root, check=True, capture_output=True,
    )


@pytest.fixture
def git_base_repo(tmp_path: Path):
    """writ init, then shared.txt committed to git only, then spec sa."""
    root = tmp_path / "repo"
    root.mkdir()
    _git(root, "init", "-q", "-b", "main")
    repo = writ.Repository.init(str(root))
    (root / FILE).write_text(_numbered({}))
    _git(root, "add", FILE)
    _git(root, "commit", "-q", "--no-verify", "-m", "shared")
    repo.add_spec(id="sa", title="sa")
    return root, repo


def _seal(repo, root: Path, text: str) -> dict:
    (root / FILE).write_text(text)
    return repo.seal(summary="a work", agent_id="agent-a", agent_type="agent",
                     spec_id="sa", paths=[FILE])


def test_own_line_removed_over_a_git_only_base_is_refused(git_base_repo):
    root, repo = git_base_repo
    _seal(repo, root, _numbered({2: "A"}))
    before = len(repo.get_spec("sa")["sealed_by"])

    with pytest.raises(writ.WritError) as err:
        _seal(repo, root, _numbered({30: "B"}))

    msg = str(err.value)
    assert "line 2 A" in msg and "no longer in shared.txt on disk" in msg, msg
    assert "--allow-removals" in msg and "--force" not in msg, msg
    assert len(repo.get_spec("sa")["sealed_by"]) == before, "a refused seal was recorded"


def test_own_edit_over_a_git_only_base_still_seals(git_base_repo):
    """Control: keeping the own line while editing elsewhere is not a loss."""
    root, repo = git_base_repo
    _seal(repo, root, _numbered({2: "A"}))
    _seal(repo, root, _numbered({2: "A", 30: "B"}))
