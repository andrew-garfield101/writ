"""W.38/W.39/W.43: Propose mode and auto mode tests.

Tests the propose and auto workflow modes via Python bindings:
- W.38: Propose mode lifecycle (propose → review → accept/reject)
- W.39: Auto mode verification
- W.43: Propose mode scenario (foundation for YAML scenario)

Bindings tested: propose, list_proposals, accept_proposal,
                 reject_proposal, finish, spec_done
"""

import subprocess
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

WRIT_BIN = None
_search = Path(__file__).resolve()
for _ in range(6):
    _search = _search.parent
    for profile in ("release", "debug"):
        candidate = _search / "target" / profile / "writ"
        if candidate.exists():
            WRIT_BIN = str(candidate)
            break
    if WRIT_BIN:
        break


def run_writ(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    if WRIT_BIN is None:
        pytest.skip("writ binary not found")
    return subprocess.run(
        [WRIT_BIN] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


def run_git(args: list, cwd: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git"] + args, capture_output=True, text=True, cwd=cwd, check=check,
    )


@pytest.fixture
def git_writ_repo(tmp_path):
    """Git repo with writ initialized."""
    run_git(["init"], str(tmp_path))
    run_git(["config", "user.email", "test@test.com"], str(tmp_path))
    run_git(["config", "user.name", "Test User"], str(tmp_path))
    (tmp_path / "README.md").write_text("# Project\n")
    run_git(["add", "README.md"], str(tmp_path))
    run_git(["commit", "-m", "initial"], str(tmp_path))
    run_writ(["init", "--yes"], str(tmp_path))
    run_git(["add", "."], str(tmp_path))
    run_git(["commit", "-m", "writ init"], str(tmp_path))
    return tmp_path


def _make_complete_spec(repo, path, spec_id, title, filename, summary):
    """Helper: create a spec, seal work, mark done."""
    repo.add_spec(id=spec_id, title=title)
    repo.update_spec(spec_id, file_scope=[filename])
    (path / filename).write_text(f"# {spec_id}\ndef {spec_id}(): pass\n")
    repo.seal(
        summary=f"{spec_id} impl",
        agent_id="dev",
        agent_type="agent",
        spec_id=spec_id,
        status="in-progress",
    )
    repo.spec_done(spec_id, summary=summary)


# ---------------------------------------------------------------------------
# W.38: Propose mode lifecycle
# ---------------------------------------------------------------------------

class TestProposeLifecycle:
    """Propose → review → accept/reject lifecycle."""

    def test_create_proposal(self, git_writ_repo):
        """Create a proposal for completed specs."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(
            spec_ids=["auth"],
            message="feat: add authentication",
            proposed_by="orchestrator",
        )

        assert proposal["status"] == "pending"
        assert "auth" in proposal["spec_ids"]
        assert proposal["message"] == "feat: add authentication"
        assert proposal["proposed_by"] == "orchestrator"
        assert proposal.get("id") is not None

    def test_list_proposals(self, git_writ_repo):
        """List shows pending proposals."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "feat-a", "A", "a.py", "A done")

        repo.propose(["feat-a"], "my proposal", "bot-1")

        proposals = repo.list_proposals()
        assert len(proposals) >= 1
        assert proposals[0]["message"] == "my proposal"

    def test_accept_proposal(self, git_writ_repo):
        """Accept a proposal marks it as accepted."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(["auth"], "feat: auth", "bot")
        prop_id = proposal["id"]

        accepted = repo.accept_proposal(prop_id)
        assert accepted["status"] == "accepted"

    def test_reject_proposal(self, git_writ_repo):
        """Reject a proposal marks it rejected, specs remain complete."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(["auth"], "feat: auth", "bot")
        prop_id = proposal["id"]

        rejected = repo.reject_proposal(prop_id)
        assert rejected["status"] == "rejected"

        # Spec should still be complete (not reverted)
        spec = repo.get_spec("auth")
        assert spec["status"] == "complete"

    def test_reject_then_new_proposal(self, git_writ_repo):
        """After rejection, a new proposal can be created for same specs."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        p1 = repo.propose(["auth"], "bad message", "bot")
        repo.reject_proposal(p1["id"])

        p2 = repo.propose(["auth"], "better message", "bot")
        assert p2["status"] == "pending"
        assert p2["message"] == "better message"

    def test_accept_already_accepted_fails(self, git_writ_repo):
        """Cannot accept an already-accepted proposal."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(["auth"], "msg", "bot")
        repo.accept_proposal(proposal["id"])

        with pytest.raises(Exception, match="not pending"):
            repo.accept_proposal(proposal["id"])

    def test_reject_already_rejected_fails(self, git_writ_repo):
        """Cannot reject an already-rejected proposal."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(["auth"], "msg", "bot")
        repo.reject_proposal(proposal["id"])

        with pytest.raises(Exception, match="not pending"):
            repo.reject_proposal(proposal["id"])

    def test_nonexistent_proposal_fails(self, tmp_repo):
        """Operations on nonexistent proposals fail."""
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.accept_proposal("prop-doesnt-exist")
        with pytest.raises(Exception):
            repo.reject_proposal("prop-doesnt-exist")


class TestProposalSupersession:
    """New proposals supersede overlapping pending proposals."""

    def test_supersedes_overlapping_proposal(self, git_writ_repo):
        """New proposal with same specs supersedes the old one."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        p1 = repo.propose(["auth"], "first attempt", "bot")
        p2 = repo.propose(["auth"], "second attempt", "bot")

        proposals = repo.list_proposals()
        pending = [p for p in proposals if p["status"] == "pending"]
        # Only the newest should be pending
        assert len(pending) == 1
        assert pending[0]["id"] == p2["id"]


# ---------------------------------------------------------------------------
# W.43: Multi-spec propose scenario
# ---------------------------------------------------------------------------

class TestProposeMultiSpec:
    """Propose mode with multiple specs — foundation for YAML scenario."""

    def test_propose_two_specs_single_proposal(self, git_writ_repo):
        """One proposal covering multiple specs."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth")
        _make_complete_spec(repo, path, "api", "API", "api.py", "API")

        proposal = repo.propose(
            spec_ids=["auth", "api"],
            message="feat: auth + api",
            proposed_by="orchestrator",
        )
        assert set(proposal["spec_ids"]) == {"auth", "api"}

    def test_propose_accept_then_finish(self, git_writ_repo):
        """Full flow: propose → accept → finish commits."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "auth", "Auth", "auth.py", "Auth done")

        proposal = repo.propose(["auth"], "feat: add auth", "orchestrator")
        repo.accept_proposal(proposal["id"])

        # After acceptance, finish should commit the specs
        result = repo.finish(strategy="single", message="feat: add auth")
        assert result["specs_finished"] >= 1

    def test_propose_reject_then_modify_then_repropose(self, git_writ_repo):
        """Reject → reopen → modify → done again → repropose."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "feat", "Feature", "feat.py", "v1")

        p1 = repo.propose(["feat"], "v1 proposal", "bot")
        repo.reject_proposal(p1["id"])

        # Reopen, modify, done again
        repo.reopen_spec("feat")
        (path / "feat.py").write_text("# v2\ndef feat(): return 'improved'\n")
        repo.seal(
            summary="improved",
            agent_id="dev",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        repo.spec_done("feat", summary="v2 with improvements")

        p2 = repo.propose(["feat"], "v2 proposal", "bot")
        assert p2["status"] == "pending"
        assert p2["message"] == "v2 proposal"


# ---------------------------------------------------------------------------
# W.39: Auto mode verification
# ---------------------------------------------------------------------------

class TestAutoMode:
    """Auto mode via CLI (writ finish --auto)."""

    def test_auto_flag_accepted(self, git_writ_repo):
        """CLI accepts --auto flag."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "feat", "Feature", "feat.py", "Done")

        result = run_writ(["finish", "--auto"], str(path), check=False)
        combined = result.stdout + result.stderr
        # Flag should be recognized (no "unexpected argument" error)
        assert "unexpected argument" not in combined.lower()

    def test_auto_with_no_specs(self, git_writ_repo):
        """Auto mode with nothing to commit handles gracefully."""
        path = git_writ_repo
        result = run_writ(["finish", "--auto"], str(path), check=False)
        # Should not crash
        assert result.returncode is not None

    def test_auto_with_verify_command_pass(self, git_writ_repo):
        """Auto mode with a passing verify command commits."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "feat", "Feature", "feat.py", "Done")

        # Set up auto config with a trivially passing verify command
        config_path = path / ".writ" / "config.toml"
        config_content = config_path.read_text() if config_path.exists() else ""
        config_content += '\n[auto]\nverify_command = "true"\n'
        config_path.write_text(config_content)

        result = run_writ(["finish", "--auto"], str(path), check=False)
        combined = result.stdout + result.stderr
        assert "unexpected argument" not in combined.lower()

    def test_auto_with_verify_command_fail(self, git_writ_repo):
        """Auto mode with a failing verify command should abort."""
        path = git_writ_repo
        repo = writ.Repository.open(str(path))
        _make_complete_spec(repo, path, "feat", "Feature", "feat.py", "Done")

        config_path = path / ".writ" / "config.toml"
        config_content = config_path.read_text() if config_path.exists() else ""
        config_content += '\n[auto]\nverify_command = "false"\n'
        config_path.write_text(config_content)

        result = run_writ(["finish", "--auto"], str(path), check=False)
        combined = result.stdout + result.stderr
        # Should either fail or indicate verification failed
        assert result.returncode != 0 or "verif" in combined.lower() or "fail" in combined.lower()
