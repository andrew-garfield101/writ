"""`Repository.doctor()` bad-state contract (sprint 3, doctor-harness).

Mirrors crates/writ-cli/tests/doctor_states.rs through the Python binding:
one scenario per fast-tier check, four assertions each (named under the
right check id; never clean while the state exists, checked twice so a cache
keyed by newest seal id cannot hide it; the printed ``fix_command`` run
verbatim through ``sh`` succeeds; clean after), plus doctor clean before the
state exists and a healthy two-agent repo with zero findings.

The binding's report must match ``writ doctor --format json``: headline,
tier, survival_last_green, checks_run, findings[{check, severity, message,
fix_command, paths}], clean, red, yellow, elapsed_ms.

Strict xfails until each check lands in the binding; an XPASS fails the gate,
which is the signal to remove the marker.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import time
from pathlib import Path

import pytest

import writ

CLEAN = "fast checks clean; survival check not available until 0.4.1"
REPORT_KEYS = {
    "headline", "tier", "survival_last_green", "checks_run", "findings",
    "clean", "red", "yellow", "elapsed_ms",
}
FINDING_KEYS = {"check", "severity", "message", "fix_command", "paths"}


def _find_writ_bin() -> Path | None:
    search = Path(__file__).resolve()
    for _ in range(6):
        search = search.parent
        for profile in ("release", "debug"):
            candidate = search / "target" / profile / "writ"
            if candidate.exists():
                return candidate
    return None


WRIT_BIN = _find_writ_bin()


BANNED = {"rm", "rmdir", "mv", "unlink", "shred", "truncate", "dd", "sudo", "ln"}
BANNED_GIT = ("reset --hard", "clean", "checkout --", "restore", "push --force")


def assert_no_destructive_fixes(report: dict) -> None:
    """Finding 79: no fix_command deletes or moves files writ does not own."""
    for f in report["findings"]:
        fix = f.get("fix_command") or ""
        for seg in re.split(r"[\n;|&]", fix):
            words = seg.split()
            if not words:
                continue
            assert words[0] not in BANNED, f"finding 79: {f['check']}: {fix}"
            if words[0] == "git":
                rest = " ".join(words[1:])
                assert not rest.startswith(BANNED_GIT), f"finding 79: {f['check']}: {fix}"


def test_destructive_fix_guard_catches_rm_and_allows_brew_unlink():
    assert_no_destructive_fixes({"findings": [
        {"check": "version_skew", "fix_command": "brew unlink writ"},
        {"check": "version_skew", "fix_command": ""},
    ]})
    for bad in ("rm /opt/homebrew/bin/writ", "writ repair && rm -rf .writ", "git reset --hard"):
        with pytest.raises(AssertionError):
            assert_no_destructive_fixes({"findings": [{"check": "x", "fix_command": bad}]})


def _pending(check: str):
    return pytest.mark.xfail(
        strict=True, reason=f"doctor-core: {check} not in Repository.doctor() yet"
    )


class Project:
    """Hermetic git + writ project; fix commands resolve `writ` to this build."""

    def __init__(self, tmp_path: Path) -> None:
        if WRIT_BIN is None:
            pytest.skip("writ binary not built (target/release or target/debug)")
        self.root = tmp_path / "repo"
        self.home = tmp_path / "home"
        self.bin = tmp_path / "bin"
        for d in (self.root, self.home, self.bin):
            d.mkdir()
        (self.bin / "writ").symlink_to(WRIT_BIN)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.write("README.md", "# fixture\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "init")
        self.cli("human", "init", "-y")
        self.git("add", "-A")
        self.git("commit", "-q", "--no-verify", "-m", "writ init")
        self.repo = writ.Repository.open(str(self.root))

    def env(self, agent: str) -> dict[str, str]:
        return {
            "PATH": f"{self.bin}:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(self.home),
            "TMPDIR": os.environ.get("TMPDIR", "/tmp"),
            "WRIT_AGENT_ID": agent,
            "WRIT_EXCLUDES_FILE": str(self.home / "no-global-excludes"),
            "GIT_CONFIG_NOSYSTEM": "1",
        }

    def write(self, rel: str, content: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)

    def git(self, *args: str) -> str:
        out = subprocess.run(
            ["git", *args], cwd=self.root, env=self.env("human"),
            capture_output=True, text=True,
        )
        assert out.returncode == 0, f"git {args}: {out.stderr}"
        return out.stdout

    def cli(self, agent: str, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        out = subprocess.run(
            [str(self.bin / "writ"), *args], cwd=self.root, env=self.env(agent),
            capture_output=True, text=True, stdin=subprocess.DEVNULL,
        )
        if check:
            assert out.returncode == 0, f"writ {args} as {agent}: {out.stdout}{out.stderr}"
        return out

    def spec(self, spec_id: str, agent: str) -> None:
        self.repo.add_spec(id=spec_id, title=spec_id)
        self.repo.spec_claim(spec_id, agent)

    def seal(self, agent: str, spec: str, paths: list[str]) -> dict:
        return self.repo.seal(
            summary="work", agent_id=agent, agent_type="agent", spec_id=spec, paths=paths
        )

    # -- doctor ------------------------------------------------------------

    def doctor(self) -> dict:
        report = self.repo.doctor()
        assert REPORT_KEYS <= set(report), f"report shape: {sorted(report)}"
        for f in report["findings"]:
            assert FINDING_KEYS <= set(f), f"finding shape: {sorted(f)}"
        assert_no_destructive_fixes(report)
        return report

    def assert_clean(self, when: str) -> None:
        r = self.doctor()
        assert r["clean"] is True and r["findings"] == [], f"{when}: {r}"
        assert r["headline"] == CLEAN, when

    def assert_names(self, check: str) -> dict:
        first = None
        for run in (1, 2):
            r = self.doctor()
            assert r["clean"] is False, f"run {run}: clean while {check} exists: {r}"
            assert r["headline"] != CLEAN
            hits = [f for f in r["findings"] if f["check"] == check]
            assert hits, f"run {run}: no {check} finding: {r['findings']}"
            assert hits[0]["fix_command"].strip()
            first = first or hits[0]
        # The binding and the CLI must agree on what is wrong.
        cli = json.loads(self.cli("human", "doctor", "--format", "json", check=False).stdout)
        assert check in {f["check"] for f in cli["findings"]}, cli
        return first

    def run_fix(self, agent: str, finding: dict) -> None:
        out = subprocess.run(
            ["sh", "-c", finding["fix_command"]], cwd=self.root, env=self.env(agent),
            capture_output=True, text=True, stdin=subprocess.DEVNULL,
        )
        assert out.returncode == 0, (
            f"fix failed verbatim: {finding['fix_command']}\n{out.stdout}{out.stderr}"
        )

    def full_cycle(self, agent: str, check: str) -> dict:
        finding = self.assert_names(check)
        self.run_fix(agent, finding)
        self.assert_clean(f"after {finding['fix_command']!r}")
        return finding


def _age_spec(p: Project, spec_id: str, seconds: int) -> None:
    path = p.root / ".writ" / "specs" / f"{spec_id}.json"
    doc = json.loads(path.read_text())
    when = time.strftime("%Y-%m-%dT%H:%M:%S.000000Z", time.gmtime(time.time() - seconds))
    for key in ("updated_at", "last_activity", "claimed_at"):
        if key in doc:
            doc[key] = when
    path.write_text(json.dumps(doc, indent=2))


@pytest.fixture
def project(tmp_path: Path) -> Project:
    return Project(tmp_path)


# --- the six bad states ------------------------------------------------------


def test_store_integrity_missing_blob(project: Project):
    project.spec("sa", "a")
    project.write("kept.txt", "kept\n")
    sealed = project.seal("a", "sa", ["kept.txt"])
    project.assert_clean("before")
    h = next(c["new_hash"] for c in sealed["changes"] if c["path"] == "kept.txt")
    blob = project.root / ".writ" / "objects" / h[:2] / h[2:]
    blob.unlink()

    f = project.full_cycle("a", "store_integrity")

    assert f["severity"] == "red"
    assert "kept.txt" in f["paths"]
    assert blob.exists()


def test_stale_claim_idle_holder(project: Project):
    project.spec("sa", "ghost")
    project.write("g.txt", "ghost\n")
    project.seal("ghost", "sa", ["g.txt"])
    project.assert_clean("before (fresh claim)")
    _age_spec(project, "sa", 3 * 3600)
    spec_path = project.root / ".writ" / "specs" / "sa.json"
    doc = json.loads(spec_path.read_text())
    doc["claimed_host"] = "other-host.invalid"  # holder not checkable: idle rule
    spec_path.write_text(json.dumps(doc, indent=2))

    f = project.full_cycle("other", "stale_claim")

    assert "spec release" in f["fix_command"] and "sa" in f["fix_command"]
    assert project.repo.get_spec("sa").get("claimed_by") is None


def test_stale_claim_holder_session_exited(project: Project):
    project.repo.add_spec(id="ghost", title="ghost")
    project.assert_clean("before (unclaimed spec)")
    (project.bin / "claude").symlink_to("/bin/sh")
    out = subprocess.run(
        [str(project.bin / "claude"), "-c", "writ spec claim ghost --agent ghost-session && :"],
        cwd=project.root, env=project.env("ghost-session"), capture_output=True, text=True,
    )
    assert out.returncode == 0, out.stderr
    assert project.repo.get_spec("ghost").get("claimed_by") == "ghost-session"

    f = project.full_cycle("other", "stale_claim")

    assert "spec release" in f["fix_command"] and "ghost" in f["fix_command"]


def test_committed_spec_seal_stuck_file(project: Project):
    project.spec("sa", "a")
    project.write("s.txt", "v1\n")
    project.seal("a", "sa", ["s.txt"])
    project.repo.spec_done("sa", agent_id="a", no_seal=True)
    project.cli("human", "finish", "-y", "--no-check")
    assert project.git("show", "HEAD:s.txt") == "v1\n"
    project.assert_clean("after finish")
    project.write("s.txt", "v2\n")
    project.git("commit", "-q", "--no-verify", "-am", "human edit outside writ")

    f = project.full_cycle("a", "committed_spec_seal")

    assert "s.txt" in f["paths"]
    assert all(x in f["fix_command"] for x in ("spec add", "--claim", "--spec "))
    assert (project.root / "s.txt").read_text() == "v2\n"


def test_unsealed_at_risk_old_pending_while_other_active(project: Project):
    project.spec("sa", "a")
    project.spec("sb", "b")
    project.write("a.txt", "a1\n")
    project.seal("a", "sa", ["a.txt"])
    project.assert_clean("before")
    project.write("a.txt", "a1\na2\n")
    old = time.time() - 45 * 60
    os.utime(project.root / "a.txt", (old, old))
    project.write("b.txt", "b1\n")
    project.seal("b", "sb", ["b.txt"])

    f = project.full_cycle("a", "unsealed_at_risk")

    assert "a.txt" in f["paths"] and "a.txt" in f["fix_command"]
    assert (project.root / "a.txt").read_text() == "a1\na2\n"


def test_version_skew_stale_managed_block(project: Project):
    project.assert_clean("before")
    text = (project.root / "CLAUDE.md").read_text()
    begin = text.index("<!-- BEGIN WRIT")
    header_end = text.index("\n", begin) + 1
    end = text.index("<!-- END WRIT CONFIGURATION -->")
    stale = (
        "Project notes kept by the user.\n\n" + text[:header_end]
        + "## Writ (0.2.0 text)\nRun `writ install` first.\n" + text[end:]
    )
    (project.root / "CLAUDE.md").write_text(stale)

    f = project.full_cycle("a", "version_skew")

    after = (project.root / "CLAUDE.md").read_text()
    assert any("CLAUDE.md" in p for p in f["paths"])
    assert after.startswith("Project notes kept by the user.")
    assert "0.2.0 text" not in after


def test_left_out_uncommitted_unsealed_file(project: Project):
    project.spec("sa", "a")
    project.write("a.txt", "a1\n")
    project.seal("a", "sa", ["a.txt"])
    project.repo.spec_done("sa", agent_id="a", no_seal=True)
    project.assert_clean("before")
    project.write("stray.txt", "nobody sealed me\n")
    project.write("README.md", (project.root / "README.md").read_text())

    f = project.assert_names("left_out")
    assert "stray.txt" in f["paths"]
    assert "README.md" not in f["paths"], "finding 75: identical-to-HEAD file listed"
    project.run_fix("a", f)
    project.assert_clean("after fix")


# --- healthy -----------------------------------------------------------------


def test_healthy_two_active_agents_with_pending_work_has_zero_findings(project: Project):
    project.spec("sa", "a")
    project.spec("sb", "b")
    project.write("src/a.rs", "pub fn a() {}\n")
    project.write("src/b.rs", "pub fn b() {}\n")
    project.seal("a", "sa", ["src/a.rs"])
    project.seal("b", "sb", ["src/b.rs"])
    project.write("src/a.rs", "pub fn a() {}\npub fn a2() {}\n")
    project.write("src/b.rs", "pub fn b() {}\npub fn b2() {}\n")
    project.write("src/a_new.rs", "// new\n")
    project.write("README.md", (project.root / "README.md").read_text())

    project.assert_clean("healthy")
    cli = project.cli("human", "doctor", "--format", "json")
    assert json.loads(cli.stdout)["findings"] == []


def test_harness_fix_commands_resolve_this_build(project: Project):
    out = subprocess.run(
        ["sh", "-c", "command -v writ"], cwd=project.root, env=project.env("a"),
        capture_output=True, text=True,
    )
    assert out.stdout.strip() == str(project.bin / "writ")
    assert project.git("status", "--porcelain").strip() == ""
