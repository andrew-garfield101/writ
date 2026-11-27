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


# Finding 225: the audit found an amended commit's receipt by (parent hash,
# tree). When two consecutive finish commits are both rewritten (new
# subjects, same trees), the second one's parent is the rewritten first, not
# the original, so no receipt matched and the audit printed "specs=[]", 0
# lines checked, which reads like a pass. It now keys on (parent tree, tree)
# and a commit with files that matches no receipt exits non zero.
UNMATCHED = "no receipt matched this commit: not audited"


def _audit_raw(script: Path, repo: Path, rev: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, WRIT_AUDIT_REPO=str(repo))
    return subprocess.run([sys.executable, str(script), rev], capture_output=True, text=True,
                          env=env, timeout=120)


@pytest.fixture
def two_finishes_rewritten(finished: Path) -> tuple[Path, str, str]:
    """s1's finish, then s2's finish (b.txt, two lines), then both commits
    recreated on the same base with new messages, trees unchanged."""
    writ = _writ_bin()
    _run(finished, writ, "spec", "add", "--id", "s2", "--title", "s2")
    _run(finished, writ, "spec", "claim", "s2", "--agent", AGENT)
    (finished / "b.txt").write_text("bee 1\nbee 2\n")
    _run(finished, writ, "seal", "-s", "two lines", "--agent", AGENT, "--spec", "s2",
         "--paths", "b.txt")
    _run(finished, writ, "spec", "done", "s2", "--agent", AGENT, "--no-seal")
    _run(finished, writ, "finish", "-y")
    base = _run(finished, "git", "rev-parse", "HEAD~2").strip()
    t1 = _run(finished, "git", "rev-parse", "HEAD~1^{tree}").strip()
    t2 = _run(finished, "git", "rev-parse", "HEAD^{tree}").strip()
    new1 = _run(finished, "git", "commit-tree", t1, "-p", base, "-m", "feat: s1 reworded").strip()
    new2 = _run(finished, "git", "commit-tree", t2, "-p", new1, "-m", "feat: s2 reworded").strip()
    _run(finished, "git", "update-ref", "refs/heads/main", new2)
    return finished, new1, new2


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_first_of_two_rewritten_finishes_finds_its_receipt(two_finishes_rewritten, script):
    repo, new1, _ = two_finishes_rewritten
    out, checked, lost, specs = _audit(script, repo, new1)
    assert specs == ["s1"], out
    assert checked == 3 and lost == 0, out


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_second_of_two_rewritten_finishes_finds_its_receipt_by_parent_tree(
        two_finishes_rewritten, script):
    """225: the 5f64d45 / ba69567 shape. The parent hash changed, the parent tree did not."""
    repo, _, new2 = two_finishes_rewritten
    out, checked, lost, specs = _audit(script, repo, new2)
    assert specs == ["s2"], f"the second rewritten commit's specs were not found:\n{out}"
    assert checked == 2 and lost == 0, out


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_commit_with_files_and_no_receipt_is_not_audited_and_fails(finished: Path, script):
    (finished / "c.txt").write_text("not a finish\n")
    _run(finished, "git", "add", "c.txt")
    _run(finished, "git", "commit", "-q", "--no-verify", "-m", "chore: hand commit")
    r = _audit_raw(script, finished, "HEAD")
    assert r.returncode != 0, f"an unmatched commit must not exit 0:\n{r.stdout}{r.stderr}"
    assert UNMATCHED in r.stdout, r.stdout
    assert "LOST: 0" not in r.stdout, f"must not print a count that reads as a pass:\n{r.stdout}"


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_unmatched_commit_fails_the_whole_run_but_others_are_still_audited(
        finished: Path, script):
    """Several commits in one call (the kit's collect does this): the matched one
    still prints its count, and the run exits non zero for the unmatched one."""
    finish = _run(finished, "git", "rev-parse", "HEAD").strip()
    (finished / "c.txt").write_text("not a finish\n")
    _run(finished, "git", "add", "c.txt")
    _run(finished, "git", "commit", "-q", "--no-verify", "-m", "chore: hand commit")
    env = dict(os.environ, WRIT_AUDIT_REPO=str(finished))
    r = subprocess.run([sys.executable, str(script), finish, "HEAD"], capture_output=True,
                       text=True, env=env, timeout=120)
    assert r.returncode != 0, r.stdout
    assert "added lines checked: 3; superseded by a later seal: 0; LOST: 0" in r.stdout, r.stdout
    assert r.stdout.count(UNMATCHED) == 1, r.stdout


@pytest.mark.parametrize("script", AUDITS, ids=["bench", "kit"])
def test_empty_commit_with_no_receipt_is_not_an_error(finished: Path, script):
    _run(finished, "git", "commit", "-q", "--no-verify", "--allow-empty", "-m", "chore: empty")
    r = _audit_raw(script, finished, "HEAD")
    assert r.returncode == 0, r.stdout + r.stderr
    assert UNMATCHED not in r.stdout, r.stdout
