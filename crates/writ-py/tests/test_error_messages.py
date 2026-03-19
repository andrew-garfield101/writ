"""Tests for PY.6: Error message polish — actionable, consistent error messages."""

import pytest
import writ


class TestSpecNotFoundError:
    """SpecNotFound should include an actionable suggestion."""

    def test_spec_not_found_suggests_context(self, tmp_path):
        """SpecNotFound error message suggests running writ context."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "base.py").write_text("# baseline\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        with pytest.raises(writ.WritError, match=r"spec not found.*writ context"):
            repo.get_spec("nonexistent-spec-id")


class TestAmbiguousSpecError:
    """AmbiguousSpec should format candidates clearly."""

    def test_ambiguous_spec_lists_candidates(self, tmp_path):
        """AmbiguousSpec error includes candidate IDs and a disambiguation hint."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "base.py").write_text("# baseline\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        # Create two specs with similar titles that will produce overlapping slugs.
        repo.add_spec(id="auth-feature-1", title="Auth feature one")
        repo.add_spec(id="auth-feature-2", title="Auth feature two")

        # "auth" prefix should match both — triggering AmbiguousSpec.
        with pytest.raises(writ.WritError, match=r"multiple specs match.*auth"):
            repo.resolve_spec("auth")


class TestInvalidInputError:
    """InvalidInput errors should be clear about what went wrong."""

    def test_spec_done_requires_spec_for_human(self, tmp_path):
        """spec_done without spec_id or agent_id gives a clear error."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "base.py").write_text("# baseline\n")
        repo.seal(
            summary="baseline",
            agent_id="setup",
            agent_type="agent",
            status="in-progress",
        )

        with pytest.raises(writ.WritError, match=r"spec_id required"):
            repo.spec_done()
