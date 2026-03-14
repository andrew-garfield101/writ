"""MS.21: Phase 3 — writ plan command tests.

Tests for batch spec creation via `writ plan`, unclaimed spec discovery
in context, and claim state changes. These are Python contract tests
using the writ Python bindings.
"""

import pytest
import writ


class TestPlanSpecCreation:
    """MS.21: Batch spec creation via writ plan."""

    def test_plan_creates_specs_from_list(self, tmp_repo):
        """Plan with a list of titles creates one spec per title."""
        repo, path = tmp_repo
        titles = ["Implement OAuth2 auth", "Add Stripe payments", "Build admin dashboard"]
        result = repo.plan(titles)

        assert len(result) == 3
        for spec_info in result:
            assert "spec_id" in spec_info
            assert "title" in spec_info

    def test_plan_generates_slugs(self, tmp_repo):
        """Plan generates URL-safe slugs from titles."""
        repo, path = tmp_repo
        result = repo.plan(["Backend API work"])

        assert result[0]["spec_id"] == "backend-api-work"

    def test_plan_duplicate_title_errors(self, tmp_repo):
        """Plan with duplicate titles returns an error."""
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.plan(["Same title", "Same title"])

    def test_plan_titles_match(self, tmp_repo):
        """Plan preserves the original titles."""
        repo, path = tmp_repo
        titles = ["Auth module", "Payment system"]
        result = repo.plan(titles)

        assert result[0]["title"] == "Auth module"
        assert result[1]["title"] == "Payment system"

    def test_plan_specs_appear_in_context(self, tmp_repo):
        """Plan-created specs are visible in context."""
        repo, path = tmp_repo
        repo.plan(["Feature A", "Feature B"])

        ctx = repo.context()
        # Plan-created specs should show up as unclaimed
        unclaimed_ids = [s["id"] for s in ctx.get("unclaimed_specs", [])]
        assert "feature-a" in unclaimed_ids
        assert "feature-b" in unclaimed_ids


class TestUnclaimedSpecsInContext:
    """MS.21: Unclaimed specs appear in context output."""

    def test_context_shows_unclaimed_specs(self, tmp_repo):
        """Unclaimed specs visible in context for agent discovery."""
        repo, path = tmp_repo
        repo.plan(["Auth", "Payments", "Dashboard"])

        ctx = repo.context()
        unclaimed = ctx.get("unclaimed_specs", [])
        assert len(unclaimed) == 3

    def test_claimed_spec_removed_from_unclaimed(self, tmp_repo):
        """After claiming, spec no longer appears as unclaimed."""
        repo, path = tmp_repo
        repo.plan(["Auth", "Payments"])

        repo.spec_claim("auth", "agent-1")

        ctx = repo.context()
        unclaimed = ctx.get("unclaimed_specs", [])
        unclaimed_ids = [s["id"] for s in unclaimed]
        assert "auth" not in unclaimed_ids
        assert "payments" in unclaimed_ids

    def test_completed_spec_not_in_unclaimed(self, tmp_repo):
        """Completed specs don't appear as unclaimed."""
        repo, path = tmp_repo
        repo.plan(["Done task"])

        # Mark it done
        repo.spec_done("done-task")

        ctx = repo.context()
        unclaimed = ctx.get("unclaimed_specs", [])
        unclaimed_ids = [s["id"] for s in unclaimed]
        assert "done-task" not in unclaimed_ids


class TestSpecClaiming:
    """MS.16/MS.4: Spec claiming via Python bindings."""

    def test_claim_spec(self, tmp_repo):
        """Claiming a spec succeeds."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth")
        repo.spec_claim("auth", "agent-1")
        # No error = success

    def test_claim_idempotent_same_agent(self, tmp_repo):
        """Same agent re-claiming is idempotent."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth")
        repo.spec_claim("auth", "agent-1")
        repo.spec_claim("auth", "agent-1")  # should not raise

    def test_claim_different_agent_errors(self, tmp_repo):
        """Different agent claiming raises error."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth")
        repo.spec_claim("auth", "agent-1")

        with pytest.raises(Exception, match="already claimed"):
            repo.spec_claim("auth", "agent-2")

    def test_claim_nonexistent_spec_errors(self, tmp_repo):
        """Claiming a non-existent spec raises error."""
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.spec_claim("nonexistent", "agent-1")

    def test_auto_claim_on_first_seal(self, tmp_repo):
        """First seal on a spec auto-claims it."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth")

        (path / "auth.py").write_text("# auth\n")
        repo.seal(
            summary="auth work",
            agent_id="agent-1",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )

        # Now a different agent should fail to claim
        with pytest.raises(Exception, match="already claimed"):
            repo.spec_claim("auth", "agent-2")
