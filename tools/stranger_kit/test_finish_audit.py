"""Finding 207: `finish_audit.py <commit>` after `git commit --amend`.

The audit finds a finish commit's specs by the spec's recorded commit hash,
so after an amend (new hash, same parent, same tree) it reads 0 lines
checked and says nothing about it. The fix (sprint 8 item 6): the audit
accepts the finish record whose commit has the amended commit's parent and
tree. Both copies of the script (bench/ and the stranger kit) are checked;
they must stay identical.

Run: python3 -m pytest tools/stranger_kit/test_finish_audit.py -q
Binary: STRANGER_BINS (first entry), else target/release/writ, else writ on PATH.
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

KIT = Path(__file__).resolve().parent
REPO = KIT.parent.parent
AUDITS = [REPO / "bench" / "finish_audit.py", KIT / "finish_audit.py"]
AGENT = "alpha"


def _writ_bin() -> str | None:
    env = os.environ.get("STRANGER_BINS")
    if env:
        return env.split(":")[0]
    rel = REPO / "target" / "release" / "writ"
    return str(rel) if rel.exists() else shutil.which("writ")


def _env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith(("WRIT_", "CLAUDE"))}
    env.update(WRIT_AGENT_ID=AGENT, GIT_AUTHOR_NAME="f", GIT_AUTHOR_EMAIL="f@example.invalid",
               GIT_COMMITTER_NAME="f", GIT_COMMITTER_EMAIL="f@example.invalid",
               GIT_CONFIG_NOSYSTEM="1")
    return env


def _run(repo: Path, *args: str) -> str:
    r = subprocess.run(list(args), cwd=repo, capture_output=True, text=True, env=_env(),
                       timeout=120)
    assert r.returncode == 0, f"{args}: {r.stdout}{r.stderr}"
    return r.stdout


@pytest.fixture
def finished(tmp_path: Path) -> Path:
    """A repo with one writ finish commit: spec s1 added three lines to a.txt."""
    writ = _writ_bin()
    if writ is None:
        pytest.skip("no writ binary")
    repo = tmp_path / "repo"
    repo.mkdir()
    _run(repo, "git", "init", "-q", "-b", "main")
    (repo / "a.txt").write_text("one\ntwo\n")
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-qm", "init")
    _run(repo, writ, "init", "-y")
    _run(repo, "git", "add", "-A")
    _run(repo, "git", "commit", "-q", "--no-verify", "-m", "writ init")
    _run(repo, writ, "spec", "add", "--id", "s1", "--title", "s1")
    _run(repo, writ, "spec", "claim", "s1", "--agent", AGENT)
    (repo / "a.txt").write_text("one\ntwo\nadded 1\nadded 2\nadded 3\n")
    _run(repo, writ, "seal", "-s", "three lines", "--agent", AGENT, "--spec", "s1",
         "--paths", "a.txt")
    _run(repo, writ, "spec", "done", "s1", "--agent", AGENT, "--no-seal")
    _run(repo, writ, "finish", "-y")
    return repo


def _audit(script: Path, repo: Path, rev: str) -> tuple[str, int, int, list[str]]:
    env = dict(os.environ, WRIT_AUDIT_REPO=str(repo))
    r = subprocess.run([sys.executable, str(script), rev], capture_output=True, text=True,
                       env=env, timeout=120)
    assert r.returncode == 0, r.stdout + r.stderr
    m = re.search(r"added lines checked: (\d+);.*LOST: (\d+)", r.stdout)
    assert m, r.stdout
    specs = re.search(r"specs=\[([^\]]*)\]", r.stdout)
    names = [s.strip(" '\"") for s in specs.group(1).split(",") if s.strip()] if specs else []
    return r.stdout, int(m.group(1)), int(m.group(2)), names


def test_both_audit_copies_are_identical():
    assert AUDITS[0].read_bytes() == AUDITS[1].read_bytes(), "bench/ and kit copies differ"


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_audit_of_the_finish_commit_checks_its_lines(finished: Path, script: Path):
    """Live guard (passes on 0.4.4): the unamended finish commit."""
    out, checked, lost, specs = _audit(script, finished, "HEAD")
    assert specs == ["s1"], out
    assert checked == 3 and lost == 0, out


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_audit_of_an_amended_commit_finds_the_receipt_by_parent_and_tree(
        finished: Path, script: Path):
    before = _run(finished, "git", "rev-parse", "HEAD").strip()
    _run(finished, "git", "commit", "--amend", "-q", "--no-verify", "-m",
         "feat: the same finish, reworded")
    after = _run(finished, "git", "rev-parse", "HEAD").strip()
    assert after != before
    assert _run(finished, "git", "rev-parse", "HEAD^{tree}") == \
        _run(finished, "git", "rev-parse", f"{before}^{{tree}}"), "fixture: same tree"
    out, checked, lost, specs = _audit(script, finished, "HEAD")
    assert specs == ["s1"], f"the amended commit's specs were not found:\n{out}"
    assert checked == 3 and lost == 0, out
