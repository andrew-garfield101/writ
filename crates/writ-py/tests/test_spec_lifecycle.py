"""W.34 + W.41: Spec lifecycle transition and stale detection tests.

Tests the round-trip spec lifecycle via Python bindings:
- LifecycleState transitions via complete_spec() and cancel_spec()
- CommitState and related fields in spec dicts
- Stale spec detection in context() and gc_status()
- New round-trip fields: commit_state, completion_summary,
  completed_at, commit_hash, committed_at

Bindings tested: add_spec, get_spec, update_spec, complete_spec,
                 cancel_spec, context, gc_status
"""

import json
from datetime import datetime, timezone, timedelta
from pathlib import Path

import pytest
import writ


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _spec_json_path(tmp_path: Path, spec_id: str) -> Path:
    """Return the on-disk path for a spec JSON file."""
    return tmp_path / ".writ" / "specs" / f"{spec_id}.json"


def _read_spec_json(tmp_path: Path, spec_id: str) -> dict:
    """Read a spec directly from disk (bypassing bindings)."""
    return json.loads(_spec_json_path(tmp_path, spec_id).read_text())


def _write_spec_json(tmp_path: Path, spec_id: str, data: dict) -> None:
    """Write a spec directly to disk (bypassing bindings)."""
    _spec_json_path(tmp_path, spec_id).write_text(json.dumps(data))


def _make_spec_stale(tmp_path: Path, spec_id: str) -> None:
    """Modify a spec on disk to have lifecycle_state='stale'."""
    data = _read_spec_json(tmp_path, spec_id)
    data["lifecycle_state"] = "stale"
    _write_spec_json(tmp_path, spec_id, data)


def _set_last_activity_hours_ago(tmp_path: Path, spec_id: str, hours: int) -> None:
    """Set a spec's last_activity to N hours in the past."""
    data = _read_spec_json(tmp_path, spec_id)
    old_time = datetime.now(timezone.utc) - timedelta(hours=hours)
    data["last_activity"] = old_time.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
    _write_spec_json(tmp_path, spec_id, data)


# ---------------------------------------------------------------------------
# W.34: Spec lifecycle defaults
# ---------------------------------------------------------------------------

class TestSpecLifecycleDefaults:
    """New specs start with correct default values for round-trip fields."""

    def test_new_spec_lifecycle_state_active(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec["lifecycle_state"] == "active"

    def test_new_spec_commit_state_uncommitted(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec["commit_state"] == "uncommitted"

    def test_new_spec_no_completion_summary(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec.get("completion_summary") is None

    def test_new_spec_no_completed_at(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec.get("completed_at") is None

    def test_new_spec_no_commit_hash(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec.get("commit_hash") is None

    def test_new_spec_no_committed_at(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec.get("committed_at") is None

    def test_new_spec_status_pending(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert spec["status"] == "pending"


# ---------------------------------------------------------------------------
# W.34: complete_spec()
# ---------------------------------------------------------------------------

class TestCompleteSpec:
    """complete_spec() transitions lifecycle Active -> Completed."""

    def test_complete_spec_succeeds(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        # Must set status to complete first
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        spec = repo.get_spec("feat-1")
        assert spec["lifecycle_state"] == "completed"

    def test_complete_spec_requires_complete_status(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        # Status is pending (default) — should fail
        with pytest.raises(Exception, match="status must be 'complete'"):
            repo.complete_spec("feat-1")

    def test_complete_spec_in_progress_fails(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="in-progress")
        with pytest.raises(Exception, match="status must be 'complete'"):
            repo.complete_spec("feat-1")

    def test_complete_spec_nonexistent_fails(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.complete_spec("no-such-spec")

    def test_complete_spec_from_stale_rejected(self, tmp_repo):
        """Stale specs cannot be lifecycle-completed directly.

        The transition table does not include Stale -> Completed.
        Stale specs must first reactivate (Stale -> Active) before
        completing (Active -> Completed). Note: complete_spec() accepts
        Active|Stale in its match arm but transition_spec_lifecycle()
        enforces the stricter rule — this is a known inconsistency
        (BRI-RT1) for CC to review.
        """
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        _make_spec_stale(path, "feat-1")
        with pytest.raises(Exception, match="not a legal transition"):
            repo.complete_spec("feat-1")

    def test_complete_spec_from_cancelled_fails(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.cancel_spec("feat-1")
        with pytest.raises(Exception, match="cannot complete"):
            repo.complete_spec("feat-1")

    def test_complete_spec_idempotent_fails(self, tmp_repo):
        """Completing an already-completed spec should fail."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        with pytest.raises(Exception, match="cannot complete"):
            repo.complete_spec("feat-1")

    def test_complete_spec_updates_timestamp(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec_before = repo.get_spec("feat-1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        spec_after = repo.get_spec("feat-1")
        assert spec_after["updated_at"] >= spec_before["updated_at"]


# ---------------------------------------------------------------------------
# W.34: cancel_spec()
# ---------------------------------------------------------------------------

class TestCancelSpec:
    """cancel_spec() transitions Active/Stale -> Cancelled."""

    def test_cancel_from_active(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.cancel_spec("feat-1")
        spec = repo.get_spec("feat-1")
        assert spec["lifecycle_state"] == "cancelled"

    def test_cancel_from_stale(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        _make_spec_stale(path, "feat-1")
        repo.cancel_spec("feat-1")
        spec = repo.get_spec("feat-1")
        assert spec["lifecycle_state"] == "cancelled"

    def test_cancel_already_cancelled_fails(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.cancel_spec("feat-1")
        with pytest.raises(Exception, match="already terminal"):
            repo.cancel_spec("feat-1")

    def test_cancel_completed_fails(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        with pytest.raises(Exception, match="already terminal"):
            repo.cancel_spec("feat-1")

    def test_cancel_nonexistent_fails(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(Exception):
            repo.cancel_spec("no-such-spec")


# ---------------------------------------------------------------------------
# W.34: CommitState fields accessible via get_spec
# ---------------------------------------------------------------------------

class TestCommitStateFields:
    """Round-trip commit fields are present in spec dicts."""

    def test_commit_state_in_spec_dict(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert "commit_state" in spec
        assert spec["commit_state"] == "uncommitted"

    def test_lifecycle_state_in_spec_dict(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        assert "lifecycle_state" in spec
        assert spec["lifecycle_state"] == "active"

    def test_commit_fields_absent_for_new_spec(self, tmp_repo):
        """New spec should not have commit_hash or committed_at."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        spec = repo.get_spec("feat-1")
        # These optional fields are skip_serializing_if = None, so they
        # may be absent from the dict entirely or present as None
        assert spec.get("commit_hash") is None
        assert spec.get("committed_at") is None

    def test_commit_state_preserved_after_cancel(self, tmp_repo):
        """Cancelling doesn't affect commit_state."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.cancel_spec("feat-1")
        spec = repo.get_spec("feat-1")
        assert spec["commit_state"] == "uncommitted"

    def test_commit_state_preserved_after_complete(self, tmp_repo):
        """Completing lifecycle doesn't change commit_state."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        spec = repo.get_spec("feat-1")
        # complete_spec only changes lifecycle_state, not commit_state
        assert spec["commit_state"] == "uncommitted"


# ---------------------------------------------------------------------------
# W.34: Lifecycle reflected in context()
# ---------------------------------------------------------------------------

class TestLifecycleInContext:
    """Lifecycle state changes appear in context output."""

    def test_active_spec_in_context(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        ctx = repo.context()
        specs = ctx.get("all_specs", [])
        feat = next((s for s in specs if s["id"] == "feat-1"), None)
        assert feat is not None
        assert feat["lifecycle_state"] == "active"
        assert feat["commit_state"] == "uncommitted"

    def test_completed_spec_in_context(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        ctx = repo.context()
        specs = ctx.get("all_specs", [])
        feat = next((s for s in specs if s["id"] == "feat-1"), None)
        assert feat is not None
        assert feat["lifecycle_state"] == "completed"

    def test_cancelled_spec_in_context(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.cancel_spec("feat-1")
        ctx = repo.context()
        specs = ctx.get("all_specs", [])
        feat = next((s for s in specs if s["id"] == "feat-1"), None)
        assert feat is not None
        assert feat["lifecycle_state"] == "cancelled"

    def test_multiple_specs_different_states(self, tmp_repo):
        """Context correctly shows specs in different lifecycle states."""
        repo, path = tmp_repo
        repo.add_spec(id="active-spec", title="Active")
        repo.add_spec(id="done-spec", title="Done")
        repo.add_spec(id="killed-spec", title="Killed")

        repo.update_spec("done-spec", status="complete")
        repo.complete_spec("done-spec")
        repo.cancel_spec("killed-spec")

        ctx = repo.context()
        specs = {s["id"]: s for s in ctx.get("all_specs", [])}
        assert specs["active-spec"]["lifecycle_state"] == "active"
        assert specs["done-spec"]["lifecycle_state"] == "completed"
        assert specs["killed-spec"]["lifecycle_state"] == "cancelled"


# ---------------------------------------------------------------------------
# W.34: Full flow — seal to complete lifecycle
# ---------------------------------------------------------------------------

class TestFullLifecycleFlow:
    """End-to-end: create spec, seal work, complete lifecycle."""

    def test_seal_then_complete(self, tmp_repo):
        """Standard flow: add spec, seal as complete, lifecycle complete."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth Feature")

        # Simulate agent work
        (path / "auth.py").write_text("def login(): pass\n")
        repo.seal(
            summary="implemented auth",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="complete",
        )

        # Verify seal updated spec status
        spec = repo.get_spec("auth")
        assert spec["status"] == "complete"
        assert len(spec["sealed_by"]) >= 1

        # Now complete the lifecycle
        repo.complete_spec("auth")
        spec = repo.get_spec("auth")
        assert spec["lifecycle_state"] == "completed"
        assert spec["commit_state"] == "uncommitted"

    def test_multiple_seals_then_complete(self, tmp_repo):
        """Multiple intermediate seals, then final seal, then complete."""
        repo, path = tmp_repo
        repo.add_spec(id="auth", title="Auth Feature")

        # First seal — in progress
        (path / "auth.py").write_text("def login(): pass\n")
        repo.seal(
            summary="started auth",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )

        # Second seal — still in progress
        (path / "auth.py").write_text("def login(): return True\n")
        repo.seal(
            summary="auth logic done",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="in-progress",
        )

        # Final seal — complete
        (path / "auth_test.py").write_text("def test_login(): assert True\n")
        repo.seal(
            summary="auth with tests",
            agent_id="dev-agent",
            agent_type="agent",
            spec_id="auth",
            status="complete",
        )

        spec = repo.get_spec("auth")
        assert spec["status"] == "complete"
        assert len(spec["sealed_by"]) == 3

        repo.complete_spec("auth")
        assert repo.get_spec("auth")["lifecycle_state"] == "completed"


# ---------------------------------------------------------------------------
# W.34: Backward compatibility
# ---------------------------------------------------------------------------

class TestBackwardCompat:
    """Specs from older writ versions (missing new fields) still work."""

    def test_old_spec_without_commit_fields(self, tmp_repo):
        """A spec JSON without round-trip fields deserializes correctly."""
        repo, path = tmp_repo
        # Write a minimal spec (pre-round-trip era)
        old_spec = {
            "id": "legacy-spec",
            "title": "Legacy Feature",
            "description": "from before round-trip sprint",
            "status": "in-progress",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-02-20T00:00:00Z",
            "updated_at": "2026-02-20T00:00:00Z",
            "sealed_by": [],
        }
        specs_dir = path / ".writ" / "specs"
        specs_dir.mkdir(parents=True, exist_ok=True)
        (specs_dir / "legacy-spec.json").write_text(json.dumps(old_spec))

        spec = repo.get_spec("legacy-spec")
        assert spec["lifecycle_state"] == "active"
        assert spec["commit_state"] == "uncommitted"
        assert spec.get("completion_summary") is None
        assert spec.get("commit_hash") is None

    def test_old_spec_in_context(self, tmp_repo):
        """Context renders old specs with default round-trip values."""
        repo, path = tmp_repo
        old_spec = {
            "id": "legacy-spec",
            "title": "Legacy",
            "description": "",
            "status": "complete",
            "depends_on": [],
            "file_scope": [],
            "created_at": "2026-02-20T00:00:00Z",
            "updated_at": "2026-02-20T00:00:00Z",
            "sealed_by": [],
        }
        specs_dir = path / ".writ" / "specs"
        specs_dir.mkdir(parents=True, exist_ok=True)
        (specs_dir / "legacy-spec.json").write_text(json.dumps(old_spec))

        ctx = repo.context()
        specs = ctx.get("all_specs", [])
        legacy = next((s for s in specs if s["id"] == "legacy-spec"), None)
        assert legacy is not None
        assert legacy["lifecycle_state"] == "active"
        assert legacy["commit_state"] == "uncommitted"


# ---------------------------------------------------------------------------
# W.41: Stale spec detection
# ---------------------------------------------------------------------------

class TestStaleDetection:
    """Stale spec warnings in context() and gc_status()."""

    def test_fresh_specs_not_stale(self, tmp_repo):
        """Newly created specs should not appear in stale_specs."""
        repo, path = tmp_repo
        repo.add_spec(id="fresh", title="Fresh Spec")
        ctx = repo.context()
        assert ctx.get("stale_specs", []) == []

    def test_stale_spec_appears_in_context(self, tmp_repo):
        """A spec with old last_activity should appear in stale_specs."""
        repo, path = tmp_repo
        repo.add_spec(id="old-feat", title="Old Feature")
        # Set last_activity to 5 hours ago (default stale timeout is 2h)
        _set_last_activity_hours_ago(path, "old-feat", hours=5)
        ctx = repo.context()
        stale = ctx.get("stale_specs", [])
        assert len(stale) >= 1
        assert any("old-feat" in s for s in stale)

    def test_stale_warning_mentions_inactivity(self, tmp_repo):
        """Stale warning should mention the spec ID and inactivity."""
        repo, path = tmp_repo
        repo.add_spec(id="idle-spec", title="Idle Spec")
        _set_last_activity_hours_ago(path, "idle-spec", hours=5)
        ctx = repo.context()
        stale = ctx.get("stale_specs", [])
        warning = next((s for s in stale if "idle-spec" in s), None)
        assert warning is not None
        assert "inactive" in warning

    def test_stale_detection_ignores_completed(self, tmp_repo):
        """Completed specs should not be flagged as stale."""
        repo, path = tmp_repo
        repo.add_spec(id="done-spec", title="Done Spec")
        repo.update_spec("done-spec", status="complete")
        repo.complete_spec("done-spec")
        # Set old activity — should not matter since lifecycle is completed
        _set_last_activity_hours_ago(path, "done-spec", hours=10)
        ctx = repo.context()
        stale = ctx.get("stale_specs", [])
        assert not any("done-spec" in s for s in stale)

    def test_stale_detection_ignores_cancelled(self, tmp_repo):
        """Cancelled specs should not be flagged as stale."""
        repo, path = tmp_repo
        repo.add_spec(id="dead-spec", title="Dead Spec")
        repo.cancel_spec("dead-spec")
        _set_last_activity_hours_ago(path, "dead-spec", hours=10)
        ctx = repo.context()
        stale = ctx.get("stale_specs", [])
        assert not any("dead-spec" in s for s in stale)

    def test_mixed_stale_and_fresh(self, tmp_repo):
        """Only old Active specs flagged, not fresh ones."""
        repo, path = tmp_repo
        repo.add_spec(id="old-one", title="Old")
        repo.add_spec(id="new-one", title="New")
        _set_last_activity_hours_ago(path, "old-one", hours=5)
        # new-one stays fresh
        ctx = repo.context()
        stale = ctx.get("stale_specs", [])
        assert any("old-one" in s for s in stale)
        assert not any("new-one" in s for s in stale)

    def test_stale_in_gc_status(self, tmp_repo):
        """gc_status() should include stale spec candidates."""
        repo, path = tmp_repo
        repo.add_spec(id="stale-feat", title="Stale Feature")
        _set_last_activity_hours_ago(path, "stale-feat", hours=5)
        status = repo.gc_status()
        # gc_status returns a dict with lifecycle_counts and stale_candidates
        assert "stale_candidates" in status or "lifecycle_counts" in status

    def test_no_stale_without_specs(self, tmp_repo):
        """Empty repo has no stale warnings."""
        repo, path = tmp_repo
        ctx = repo.context()
        assert ctx.get("stale_specs", []) == []


# ---------------------------------------------------------------------------
# W.34: Round-trip field persistence
# ---------------------------------------------------------------------------

class TestRoundTripFieldPersistence:
    """Verify round-trip fields survive write/read cycles."""

    def test_commit_state_persisted_on_disk(self, tmp_repo):
        """commit_state written to spec JSON on disk."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        data = _read_spec_json(path, "feat-1")
        assert data["commit_state"] == "uncommitted"

    def test_lifecycle_state_persisted_on_disk(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.update_spec("feat-1", status="complete")
        repo.complete_spec("feat-1")
        data = _read_spec_json(path, "feat-1")
        assert data["lifecycle_state"] == "completed"

    def test_cancel_persisted_on_disk(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        repo.cancel_spec("feat-1")
        data = _read_spec_json(path, "feat-1")
        assert data["lifecycle_state"] == "cancelled"

    def test_list_specs_includes_lifecycle_fields(self, tmp_repo):
        """list_specs returns specs with round-trip fields."""
        repo, path = tmp_repo
        repo.add_spec(id="feat-1", title="Feature 1")
        specs = repo.list_specs()
        assert len(specs) >= 1
        feat = next((s for s in specs if s["id"] == "feat-1"), None)
        assert feat is not None
        assert "lifecycle_state" in feat
        assert "commit_state" in feat
