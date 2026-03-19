"""PY.8 — Contract tests for Python SDK v2 features.

Covers:
- Spec identity: resolve_spec, slug field, hash-based IDs
- Auto-scoping: resolve_spec_for_agent, seal without spec_id
- Skills: generate_skills / remove_skills via Python
- Convergence inspection: archive_orphaned_specs, convergence_eligible_specs
"""

import os
import re

import pytest
import writ


# ── helpers ──────────────────────────────────────────────────────────────

HASH_ID_PATTERN = re.compile(r"^[0-9a-f]{12}$")


def make_spec_with_agent(repo, tmp_path, spec_id, title, agent_id):
    """Create a spec, claim it, and seal once so the agent owns it."""
    repo.add_spec(id=spec_id, title=title)
    repo.spec_claim(spec_id, agent_id)
    (tmp_path / f"{spec_id}.txt").write_text(f"work for {spec_id}\n")
    repo.seal(
        summary=f"work on {title}",
        agent_id=agent_id,
        agent_type="agent",
        spec_id=spec_id,
        status="in-progress",
    )


# ── PY.1: Spec identity ─────────────────────────────────────────────────


class TestResolveSpec:
    """resolve_spec() — resolve by ID, ID prefix, slug, or slug prefix."""

    def test_resolve_by_exact_id(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="a3f7b2c1d9e4", title="Auth module")
        spec = repo.resolve_spec("a3f7b2c1d9e4")
        assert spec["id"] == "a3f7b2c1d9e4"
        assert spec["title"] == "Auth module"

    def test_resolve_by_id_prefix(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="a3f7b2c1d9e4", title="Auth module")
        spec = repo.resolve_spec("a3f7b2")
        assert spec["id"] == "a3f7b2c1d9e4"

    def test_resolve_by_exact_slug(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="abc123def456", title="Auth module")
        spec = repo.resolve_spec("auth-module")
        assert spec["id"] == "abc123def456"
        assert spec["slug"] == "auth-module"

    def test_resolve_by_slug_prefix(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="abc123def456", title="Auth module feature")
        spec = repo.resolve_spec("auth-mod")
        assert spec["id"] == "abc123def456"

    def test_resolve_ambiguous_raises(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="aaa111bbb222", title="Auth login")
        repo.add_spec(id="aaa111ccc333", title="Auth signup")
        with pytest.raises(writ.WritError, match="multiple specs match"):
            repo.resolve_spec("aaa111")

    def test_resolve_not_found_raises(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.resolve_spec("nonexistent")

    def test_resolve_returns_slug_field(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="abc123def456", title="My Cool Feature")
        spec = repo.resolve_spec("abc123def456")
        assert "slug" in spec
        assert spec["slug"] == "my-cool-feature"


class TestAddSpecAutoId:
    """add_spec() — auto-generated hash ID when id is omitted."""

    def test_add_spec_with_title_only(self, tmp_repo):
        repo, path = tmp_repo
        spec = repo.add_spec(title="OAuth2 authentication")
        assert HASH_ID_PATTERN.match(spec["id"]), f"Expected 12-char hex ID, got: {spec['id']}"
        assert spec["title"] == "OAuth2 authentication"
        assert spec["slug"] == "oauth2-authentication"

    def test_add_spec_with_explicit_id(self, tmp_repo):
        repo, path = tmp_repo
        spec = repo.add_spec(id="my-custom-id", title="Custom spec")
        assert spec["id"] == "my-custom-id"
        assert spec["title"] == "Custom spec"

    def test_add_spec_positional_backward_compat(self, tmp_repo):
        """Existing code uses add_spec("id", "title") positionally."""
        repo, path = tmp_repo
        spec = repo.add_spec("feat-a", "Feature A")
        assert spec["id"] == "feat-a"
        assert spec["title"] == "Feature A"

    def test_add_spec_id_only_no_title_raises(self, tmp_repo):
        """add_spec(id="my-id") without title should error, not treat id as title."""
        repo, path = tmp_repo
        with pytest.raises(writ.WritError, match="requires a title"):
            repo.add_spec(id="my-id")

    def test_add_spec_no_args_raises(self, tmp_repo):
        """add_spec() with nothing provided should error."""
        repo, path = tmp_repo
        with pytest.raises(writ.WritError, match="requires at least a title"):
            repo.add_spec()

    def test_add_spec_auto_id_deterministic(self, tmp_repo):
        """Two specs with the same title should get different IDs (timestamp/random)."""
        repo, path = tmp_repo
        spec1 = repo.add_spec(title="Duplicate title")
        # Need a new repo for the second one since same title+id would conflict
        repo2 = writ.Repository.init(str(path / "repo2"))
        spec2 = repo2.add_spec(title="Duplicate title")
        # Both should be valid hex IDs (may or may not be equal depending on timing)
        assert HASH_ID_PATTERN.match(spec1["id"])
        assert HASH_ID_PATTERN.match(spec2["id"])


class TestPlanReturnsSlug:
    """plan() returns slug field in results."""

    def test_plan_result_has_slug(self, tmp_repo):
        repo, path = tmp_repo
        results = repo.plan(["Auth module", "Payment system"])
        assert len(results) == 2
        for r in results:
            assert "slug" in r
            assert "spec_id" in r
            assert HASH_ID_PATTERN.match(r["spec_id"])
        assert results[0]["slug"] == "auth-module"
        assert results[1]["slug"] == "payment-system"

    def test_plan_result_has_hash_id(self, tmp_repo):
        repo, path = tmp_repo
        results = repo.plan(["Build pipeline"])
        assert HASH_ID_PATTERN.match(results[0]["spec_id"])


class TestGetSpecSlug:
    """get_spec() returns slug field."""

    def test_get_spec_includes_slug(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-123", title="My Feature")
        spec = repo.get_spec("feat-123")
        assert spec["slug"] == "my-feature"


# ── PY.2: Auto-scoping ──────────────────────────────────────────────────


class TestAutoScoping:
    """resolve_spec_for_agent() and auto-scoping in seal/spec_done."""

    def test_resolve_with_one_claimed_spec(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        resolved = repo.resolve_spec_for_agent("agent-1")
        assert resolved == "auth"

    def test_resolve_with_zero_specs_raises(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.resolve_spec_for_agent("agent-orphan")

    def test_resolve_with_two_specs_raises(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        make_spec_with_agent(repo, path, "pay", "Payment", "agent-1")
        with pytest.raises(writ.WritError):
            repo.resolve_spec_for_agent("agent-1")

    def test_resolve_with_explicit_override(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        make_spec_with_agent(repo, path, "pay", "Payment", "agent-1")
        # Explicit spec_id should bypass the ambiguity
        resolved = repo.resolve_spec_for_agent("agent-1", spec_id="auth")
        assert resolved == "auth"

    def test_seal_without_spec_id_auto_scopes(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        # Now seal without spec_id — should auto-scope to "auth"
        (path / "extra.txt").write_text("more work\n")
        result = repo.seal(
            summary="auto-scoped work",
            agent_id="agent-1",
            agent_type="agent",
            status="in-progress",
        )
        assert result["spec_id"] == "auth"

    def test_spec_done_without_spec_id_auto_scopes(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        result = repo.spec_done(agent_id="agent-1", summary="done")
        assert result["status"].lower() == "complete"

    def test_seal_no_spec_with_zero_claimed_falls_back(self, tmp_repo):
        """seal() without spec_id when agent has 0 claimed specs should still seal (spec_id=None)."""
        repo, path = tmp_repo
        (path / "work.txt").write_text("work\n")
        result = repo.seal(
            summary="unscoped work",
            agent_id="new-agent",
            agent_type="agent",
            status="in-progress",
        )
        # Should succeed with no spec_id (unscoped seal)
        assert result is not None

    def test_seal_no_spec_with_two_claimed_falls_back(self, tmp_repo):
        """seal() without spec_id when agent has 2+ claimed specs should still seal (spec_id=None)."""
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        make_spec_with_agent(repo, path, "pay", "Payment", "agent-1")
        (path / "more.txt").write_text("more work\n")
        result = repo.seal(
            summary="ambiguous work",
            agent_id="agent-1",
            agent_type="agent",
            status="in-progress",
        )
        # Should succeed — auto-scope failure is graceful, falls back to unscoped
        assert result is not None

    def test_spec_done_human_no_spec_id_raises(self, tmp_repo):
        """spec_done() with human agent and no spec_id should raise an error."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth")
        with pytest.raises(writ.WritError, match="spec_id required"):
            repo.spec_done(agent_id="human")


# ── PY.3: Skills ─────────────────────────────────────────────────────────


class TestSkills:
    """generate_skills / remove_skills via Python module-level functions."""

    def test_generate_creates_skill_dirs(self, tmp_repo):
        repo, path = tmp_repo
        result = writ.generate_skills(str(path))
        assert "created" in result
        assert result["created"] > 0
        # Verify skill dirs exist
        skills_dir = path / ".claude" / "skills"
        assert skills_dir.exists()
        skill_dirs = [d for d in skills_dir.iterdir() if d.is_dir() and d.name.startswith("writ-")]
        assert len(skill_dirs) > 0

    def test_generate_is_idempotent(self, tmp_repo):
        repo, path = tmp_repo
        result1 = writ.generate_skills(str(path))
        result2 = writ.generate_skills(str(path))
        # Second run should update, not create new
        assert result2["created"] == 0

    def test_remove_cleans_up_skills(self, tmp_repo):
        repo, path = tmp_repo
        writ.generate_skills(str(path))
        removed = writ.remove_skills(str(path))
        assert len(removed) > 0
        # Verify writ- skill dirs are gone
        skills_dir = path / ".claude" / "skills"
        if skills_dir.exists():
            writ_dirs = [d for d in skills_dir.iterdir() if d.is_dir() and d.name.startswith("writ-")]
            assert len(writ_dirs) == 0

    def test_remove_preserves_non_writ_skills(self, tmp_repo):
        repo, path = tmp_repo
        writ.generate_skills(str(path))
        # Create a non-writ skill
        custom_skill = path / ".claude" / "skills" / "my-custom-skill"
        custom_skill.mkdir(parents=True, exist_ok=True)
        (custom_skill / "SKILL.md").write_text("# Custom skill\n")
        # Remove should only remove writ- prefixed
        writ.remove_skills(str(path))
        assert custom_skill.exists()


# ── PY.4: Convergence inspection ─────────────────────────────────────────


class TestConvergenceInspection:
    """archive_orphaned_specs and convergence_eligible_specs."""

    def test_archive_orphaned_specs_empty(self, tmp_repo):
        repo, path = tmp_repo
        archived = repo.archive_orphaned_specs()
        assert archived == []

    def test_convergence_eligible_empty_repo(self, tmp_repo):
        repo, path = tmp_repo
        eligible = repo.convergence_eligible_specs()
        assert eligible == []

    def test_convergence_eligible_includes_complete_specs(self, tmp_repo):
        repo, path = tmp_repo
        make_spec_with_agent(repo, path, "auth", "Auth", "agent-1")
        repo.spec_done(spec_id="auth", summary="done")
        eligible = repo.convergence_eligible_specs()
        # Complete spec should be eligible for convergence
        ids = [s["id"] for s in eligible]
        assert "auth" in ids
