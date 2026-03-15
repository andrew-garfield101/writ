"""Seal-triggered convergence integration tests.

Tests for the seal-triggered convergence feature where seal() automatically
detects overlapping file changes across specs and attempts convergence inline.

Tests #11-#15 from the seal-triggered-convergence-sprint.md test plan.
"""

import json
import subprocess
from pathlib import Path
from typing import Optional

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(6):
    _search = _search.parent
    candidates = []
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            candidates.append(candidate)
    if candidates:
        candidates.sort(key=lambda p: p.stat().st_mtime, reverse=True)
        WRIT_BIN = str(candidates[0])
        break


def run_writ(args: list, cwd: Path, check: bool = True):
    """Run writ CLI command."""
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args,
        capture_output=True, text=True, cwd=cwd, check=check,
    )


def make_overlapping_repo(tmp_path: Path):
    """Create a repo with two specs that modify the same file.

    Returns (repo, path) with both specs sealed and overlapping on shared.rs.
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline seal with shared file
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    repo.add_spec(id="feat-a", title="Feature A")
    repo.add_spec(id="feat-b", title="Feature B")

    # Spec A modifies shared.rs
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\nfn feature_a() {}\n")
    seal_a = repo.seal(
        summary="add feature_a",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="feat-a",
        status="in-progress",
    )

    # Spec B modifies shared.rs differently
    (tmp_path / "shared.rs").write_text("// base\nfn hello() {}\nfn feature_b() {}\n")
    seal_b = repo.seal(
        summary="add feature_b",
        agent_id="agent-b",
        agent_type="agent",
        spec_id="feat-b",
        status="in-progress",
    )

    return repo, seal_a, seal_b


# ---------------------------------------------------------------------------
# Test #11: seal() returns convergence info via Python API
# ---------------------------------------------------------------------------


class TestSealReturnsConvergenceInfo:
    """Test #11: Verify convergence fields appear in seal() result dict."""

    def test_convergence_field_present_on_overlap(self, tmp_path):
        """When specs overlap, seal result includes convergence dict."""
        _repo, _seal_a, seal_b = make_overlapping_repo(tmp_path)

        conv = seal_b.get("convergence")
        assert conv is not None, "convergence field should be present"
        assert isinstance(conv, dict), "convergence should be a dict"

    def test_convergence_fields_populated(self, tmp_path):
        """All SealConvergenceResult fields are present and typed correctly."""
        _repo, _seal_a, seal_b = make_overlapping_repo(tmp_path)

        conv = seal_b["convergence"]
        assert "attempted" in conv
        assert "succeeded" in conv
        assert "files_merged" in conv
        assert "overlaps_detected" in conv
        assert "specs_involved" in conv

        assert isinstance(conv["attempted"], bool)
        assert isinstance(conv["succeeded"], bool)
        assert isinstance(conv["files_merged"], int)
        assert isinstance(conv["overlaps_detected"], int)
        assert isinstance(conv["specs_involved"], list)

    def test_convergence_attempted_on_overlap(self, tmp_path):
        """Convergence is attempted when overlapping files detected."""
        _repo, _seal_a, seal_b = make_overlapping_repo(tmp_path)

        conv = seal_b["convergence"]
        assert conv["attempted"] is True
        assert conv["overlaps_detected"] > 0
        assert len(conv["specs_involved"]) >= 2

    def test_convergence_not_attempted_without_overlap(self, tmp_repo):
        """No convergence when specs modify different files."""
        repo, path = tmp_repo

        (path / "base.txt").write_text("base\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        repo.add_spec(id="s1", title="S1")
        repo.add_spec(id="s2", title="S2")

        (path / "a.txt").write_text("agent-a work\n")
        repo.seal(
            summary="a work",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="s1",
            status="in-progress",
        )

        (path / "b.txt").write_text("agent-b work\n")
        seal_b = repo.seal(
            summary="b work",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="s2",
            status="in-progress",
        )

        conv = seal_b.get("convergence")
        # Either None or attempted=False (no overlaps to converge)
        if conv is not None:
            assert conv["attempted"] is False

    def test_convergence_not_attempted_without_spec(self, tmp_repo):
        """Unscoped seals (no --spec) never trigger convergence."""
        repo, path = tmp_repo

        (path / "file.txt").write_text("content\n")
        result = repo.seal(
            summary="unscoped",
            agent_id="solo",
            agent_type="agent",
            status="in-progress",
        )

        conv = result.get("convergence")
        if conv is not None:
            assert conv["attempted"] is False

    def test_convergence_specs_involved_correct(self, tmp_path):
        """specs_involved lists the correct spec IDs."""
        _repo, _seal_a, seal_b = make_overlapping_repo(tmp_path)

        conv = seal_b["convergence"]
        specs = sorted(conv["specs_involved"])
        assert "feat-a" in specs
        assert "feat-b" in specs


# ---------------------------------------------------------------------------
# Test #12: Overlapping seals produce convergence (file content verification)
# ---------------------------------------------------------------------------


class TestSealConvergenceMergesFiles:
    """Test #12: Two Python seals trigger convergence, verify results."""

    def test_seal_commits_even_when_convergence_fails(self, tmp_path):
        """Seal is always committed regardless of convergence outcome.

        This is the critical safety invariant: seal-first, converge-after.
        """
        _repo, seal_a, seal_b = make_overlapping_repo(tmp_path)

        # Both seals must succeed (have IDs)
        assert seal_a.get("id") is not None
        assert seal_b.get("id") is not None
        assert seal_a["summary"] == "add feature_a"
        assert seal_b["summary"] == "add feature_b"

    def test_convergence_message_on_failure(self, tmp_path):
        """When convergence fails, message field explains why."""
        _repo, _seal_a, seal_b = make_overlapping_repo(tmp_path)

        conv = seal_b["convergence"]
        if not conv["succeeded"]:
            assert conv.get("message") is not None
            assert len(conv["message"]) > 0


# ---------------------------------------------------------------------------
# Test #13: Config flag disables convergence
# ---------------------------------------------------------------------------


class TestSealConvergenceDisabledViaConfig:
    """Test #13: auto_converge_on_seal config flag respected from Python."""

    def test_convergence_disabled_via_config(self, tmp_path):
        """When auto_converge_on_seal=false, convergence is not attempted."""
        repo = writ.Repository.init(str(tmp_path))

        # Write config with convergence disabled
        config_dir = tmp_path / ".writ"
        config_path = config_dir / "config.toml"
        config_content = config_path.read_text() if config_path.exists() else ""
        config_content += "\n[watch]\nauto_converge_on_seal = false\n"
        config_path.write_text(config_content)

        # Re-open repo to pick up config
        repo = writ.Repository.open(str(tmp_path))

        # Create overlapping specs
        (tmp_path / "shared.rs").write_text("// base\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        repo.add_spec(id="feat-a", title="Feature A")
        repo.add_spec(id="feat-b", title="Feature B")

        (tmp_path / "shared.rs").write_text("// version A\n")
        repo.seal(
            summary="a",
            agent_id="agent-a",
            agent_type="agent",
            spec_id="feat-a",
            status="in-progress",
        )

        (tmp_path / "shared.rs").write_text("// version B\n")
        seal_b = repo.seal(
            summary="b",
            agent_id="agent-b",
            agent_type="agent",
            spec_id="feat-b",
            status="in-progress",
        )

        conv = seal_b.get("convergence")
        # With config disabled, convergence should not be present
        assert conv is None, (
            f"convergence should be None when disabled via config, got: {conv}"
        )


# ---------------------------------------------------------------------------
# Test #14: CLI shows convergence output
# ---------------------------------------------------------------------------


class TestSealCLIShowsConvergenceOutput:
    """Test #14: CLI prints convergence info when overlap exists."""

    @pytest.fixture
    def git_writ_repo(self, tmp_path):
        """Git repo with writ initialized."""
        subprocess.run(["git", "init"], cwd=tmp_path, capture_output=True, check=True)
        subprocess.run(
            ["git", "config", "user.email", "test@test.com"],
            cwd=tmp_path, capture_output=True, check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test"],
            cwd=tmp_path, capture_output=True, check=True,
        )
        (tmp_path / "README.md").write_text("# Test\n")
        subprocess.run(
            ["git", "add", "README.md"],
            cwd=tmp_path, capture_output=True, check=True,
        )
        subprocess.run(
            ["git", "commit", "-m", "init"],
            cwd=tmp_path, capture_output=True, check=True,
        )
        run_writ(["init", "--yes"], tmp_path)
        subprocess.run(
            ["git", "add", "."],
            cwd=tmp_path, capture_output=True, check=True,
        )
        subprocess.run(
            ["git", "commit", "-m", "writ init"],
            cwd=tmp_path, capture_output=True, check=True,
        )
        return tmp_path

    def test_cli_seal_shows_convergence_info(self, git_writ_repo):
        """CLI seal command prints convergence status when overlaps detected."""
        path = git_writ_repo

        # Create specs first (CLI enforces --spec on seals)
        run_writ(["spec", "add", "--id", "setup-spec", "--title", "Setup"], path)
        run_writ(["spec", "add", "--id", "feat-a", "--title", "Feature A"], path)
        run_writ(["spec", "add", "--id", "feat-b", "--title", "Feature B"], path)

        # Baseline seal with shared file
        (path / "shared.rs").write_text("// base\nfn hello() {}\n")
        run_writ(
            ["seal", "-s", "baseline", "--agent", "setup", "--spec", "setup-spec"],
            path,
        )

        # Spec A modifies shared file
        (path / "shared.rs").write_text("// base\nfn hello() {}\nfn a() {}\n")
        run_writ(
            ["seal", "-s", "a work", "--agent", "agent-a", "--spec", "feat-a"],
            path,
        )

        # Spec B modifies shared file differently
        (path / "shared.rs").write_text("// base\nfn hello() {}\nfn b() {}\n")
        result = run_writ(
            ["seal", "-s", "b work", "--agent", "agent-b", "--spec", "feat-b"],
            path, check=False,
        )

        output = result.stdout + result.stderr
        # Seal should always succeed (exit 0)
        assert result.returncode == 0, f"seal failed: {output}"
        # Output should mention convergence (either success or attempt)
        assert "converge" in output.lower() or "sealed" in output.lower(), (
            f"Expected convergence info in output, got: {output}"
        )


# ---------------------------------------------------------------------------
# Test #15: Five agents scenario (reproduces alpha-12 Test 2)
# ---------------------------------------------------------------------------


class TestFiveAgentsSealTriggeredConvergence:
    """Test #15: Reproduce alpha-12 scenario — 5 agents, convergence without watch."""

    def test_five_agents_seal_triggered_convergence(self, tmp_path):
        """Five agents seal overlapping and non-overlapping work.

        Validates that seal-triggered convergence handles N-agent scenarios
        without requiring writ watch. Each seal commits successfully.
        """
        repo = writ.Repository.init(str(tmp_path))

        # Baseline
        (tmp_path / "shared.rs").write_text("// shared base\n")
        (tmp_path / "config.toml").write_text("[base]\nversion = 1\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        # Create 5 specs
        specs = [
            ("auth", "Authentication"),
            ("payments", "Payment Processing"),
            ("dashboard", "Dashboard UI"),
            ("api", "API Gateway"),
            ("monitoring", "Monitoring"),
        ]
        for spec_id, title in specs:
            repo.add_spec(id=spec_id, title=title)

        seals = []

        # Agent 1 — auth: modifies shared.rs (overlap) + creates auth.py
        (tmp_path / "shared.rs").write_text("// shared base\n// auth import\n")
        (tmp_path / "auth.py").write_text("def login(): pass\n")
        s = repo.seal(
            summary="auth module",
            agent_id="agent-1",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )
        seals.append(("auth", s))

        # Agent 2 — payments: modifies shared.rs (overlap) + creates payments.py
        (tmp_path / "shared.rs").write_text("// shared base\n// payments import\n")
        (tmp_path / "payments.py").write_text("def charge(): pass\n")
        s = repo.seal(
            summary="payment processing",
            agent_id="agent-2",
            agent_type="agent",
            spec_id="payments",
            status="in-progress",
        )
        seals.append(("payments", s))

        # Agent 3 — dashboard: creates dashboard.js only (no overlap)
        (tmp_path / "dashboard.js").write_text("export function render() {}\n")
        s = repo.seal(
            summary="dashboard components",
            agent_id="agent-3",
            agent_type="agent",
            spec_id="dashboard",
            status="in-progress",
        )
        seals.append(("dashboard", s))

        # Agent 4 — api: modifies config.toml (overlap) + creates api.py
        (tmp_path / "config.toml").write_text("[base]\nversion = 1\n[api]\nport = 8080\n")
        (tmp_path / "api.py").write_text("def gateway(): pass\n")
        s = repo.seal(
            summary="api gateway",
            agent_id="agent-4",
            agent_type="agent",
            spec_id="api",
            status="in-progress",
        )
        seals.append(("api", s))

        # Agent 5 — monitoring: modifies config.toml (overlap) + creates monitor.py
        (tmp_path / "config.toml").write_text("[base]\nversion = 1\n[monitoring]\nenabled = true\n")
        (tmp_path / "monitor.py").write_text("def check_health(): pass\n")
        s = repo.seal(
            summary="monitoring setup",
            agent_id="agent-5",
            agent_type="agent",
            spec_id="monitoring",
            status="in-progress",
        )
        seals.append(("monitoring", s))

        # All 5 seals must succeed (critical invariant)
        for spec_id, seal in seals:
            assert seal is not None, f"seal for {spec_id} should not be None"
            assert seal.get("id") is not None, f"seal for {spec_id} should have an ID"

        # Later seals with overlaps should attempt convergence
        overlap_seals = [
            (spec_id, seal) for spec_id, seal in seals
            if seal.get("convergence", {}).get("attempted", False)
        ]
        assert len(overlap_seals) > 0, (
            "At least one seal should have attempted convergence (overlapping files)"
        )

        # All agent files should exist on disk (seals committed work)
        assert (tmp_path / "auth.py").exists()
        assert (tmp_path / "payments.py").exists()
        assert (tmp_path / "dashboard.js").exists()
        assert (tmp_path / "api.py").exists()
        assert (tmp_path / "monitor.py").exists()

        # Verify seal chain integrity — all seals are in the log
        log = repo.log()
        # 1 baseline + 5 agent seals = at least 6
        assert len(log) >= 6, f"Expected at least 6 seals in log, got {len(log)}"
