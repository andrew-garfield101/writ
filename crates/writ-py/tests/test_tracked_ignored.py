"""Finding 210: a path git tracks is tracked by writ whatever .gitignore says.

git applies .gitignore only to untracked files. A file force added before
its directory went into .gitignore (the writ repo's own
``scripts/bump-version.sh``) is still tracked by git, so ``writ seal
--paths`` must seal it. Untracked files in that directory stay ignored.
"""

import subprocess
from pathlib import Path

import pytest

import writ


def _git(path: Path, *args: str) -> str:
    out = subprocess.run(
        ["git", *args], cwd=path, capture_output=True, text=True, check=True
    )
    return out.stdout


def _changed(result: dict) -> list[str]:
    return sorted(c["path"] for c in result["changes"])


@pytest.fixture
def tracked_ignored_repo(tmp_path: Path):
    """git repo where tools/bump.sh is tracked and tools/ is gitignored."""
    _git(tmp_path, "init", "-q")
    _git(tmp_path, "config", "user.name", "writ-test")
    _git(tmp_path, "config", "user.email", "writ-test@localhost")
    (tmp_path / "README.md").write_text("base\n")
    (tmp_path / "tools").mkdir()
    (tmp_path / "tools" / "bump.sh").write_text("echo bump\n")
    (tmp_path / ".gitignore").write_text(".writ/\ntools/\n")
    _git(tmp_path, "add", "README.md", ".gitignore")
    _git(tmp_path, "add", "-f", "tools/bump.sh")
    _git(tmp_path, "commit", "-q", "-m", "base")
    assert _git(tmp_path, "ls-files", "-ci", "--exclude-standard").split() == [
        "tools/bump.sh"
    ]
    repo = writ.Repository.init(str(tmp_path))
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
        allow_empty=True,
    )
    repo.add_spec(id="release", title="Release script")
    return repo, tmp_path


def test_seal_paths_seals_a_git_tracked_file_under_an_ignored_directory(
    tracked_ignored_repo,
):
    repo, root = tracked_ignored_repo
    (root / "tools" / "bump.sh").write_text("echo bump\necho tag\n")

    result = repo.seal(
        summary="bump prints the tag step",
        agent_id="djo",
        agent_type="agent",
        status="in-progress",
        spec_id="release",
        paths=["tools/bump.sh"],
    )

    assert _changed(result) == ["tools/bump.sh"]


def test_untracked_file_under_the_ignored_directory_stays_ignored(
    tracked_ignored_repo,
):
    repo, root = tracked_ignored_repo
    (root / "tools" / "bump.sh").write_text("echo bump\necho tag\n")
    (root / "tools" / "scratch.sh").write_text("echo scratch\n")
    (root / "notes.md").write_text("notes\n")

    result = repo.seal(
        summary="everything writ sees",
        agent_id="djo",
        agent_type="agent",
        status="in-progress",
        spec_id="release",
    )

    assert _changed(result) == ["notes.md", "tools/bump.sh"]
