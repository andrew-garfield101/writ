"""Contract tests for writ convergence Python API.

Validates that the Python bindings expose the correct return structures
for all convergence-related operations: converge_all, converge,
apply_convergence, and diverged_branches.

These are NOT duplicate tests of the Rust convergence logic — they verify
that the pyo3 serialization layer produces the expected dict shapes,
field types, and defaults that downstream Python consumers depend on.
"""

import pytest
import writ


# ── converge_all return structure ──────────────────────────────────────


class TestConvergeAllContract:
    """Verify converge_all returns the documented ConvergeAllReport shape."""

    def test_converge_all_returns_dict(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        assert isinstance(report, dict), "converge_all must return a dict"

    def test_converge_all_top_level_fields(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)

        # Required fields from ConvergeAllReport
        assert "base_spec" in report
        assert "merge_order" in report
        assert "merges" in report
        assert "strategy" in report
        assert "total_auto_merged" in report
        assert "total_conflicts" in report
        assert "total_resolutions" in report
        assert "is_clean" in report
        assert "applied" in report

    def test_converge_all_field_types(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)

        assert isinstance(report["base_spec"], str)
        assert isinstance(report["merge_order"], list)
        assert isinstance(report["merges"], list)
        assert isinstance(report["strategy"], str)
        assert isinstance(report["total_auto_merged"], int)
        assert isinstance(report["total_conflicts"], int)
        assert isinstance(report["total_resolutions"], int)
        assert isinstance(report["is_clean"], bool)
        assert isinstance(report["applied"], bool)

    def test_converge_all_strategy_field_matches_input(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["strategy"] == "escalate"

    def test_converge_all_applied_false_when_dry_run(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["applied"] is False

    def test_converge_all_applied_true_when_apply(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=True)
        assert report["applied"] is True

    def test_converge_all_default_strategy_is_escalate(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all()
        assert report["strategy"] == "escalate"

    def test_converge_all_clean_disjoint_files(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=True)
        assert report["is_clean"] is True
        assert report["total_conflicts"] == 0

    def test_converge_all_degraded_default_false(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        # degraded is serde(default) so may be absent or False
        assert report.get("degraded", False) is False


# ── MergeStepResult shape ─────────────────────────────────────────────


class TestMergeStepContract:
    """Verify each merge step has the documented MergeStepResult shape."""

    def test_merge_step_fields(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        assert len(report["merges"]) > 0, "Should have at least one merge step"

        step = report["merges"][0]
        assert "left_spec" in step
        assert "right_spec" in step
        assert "auto_merged" in step
        assert "conflicts" in step
        assert "left_only" in step
        assert "right_only" in step
        assert "clean" in step

    def test_merge_step_field_types(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        step = report["merges"][0]

        assert isinstance(step["left_spec"], str)
        assert isinstance(step["right_spec"], str)
        assert isinstance(step["auto_merged"], int)
        assert isinstance(step["conflicts"], int)
        assert isinstance(step["left_only"], int)
        assert isinstance(step["right_only"], int)
        assert isinstance(step["clean"], bool)

    def test_merge_step_spec_ids_from_setup(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        step = report["merges"][0]

        # Both spec IDs should be non-empty strings from our test setup
        assert len(step["left_spec"]) > 0
        assert len(step["right_spec"]) > 0
        all_specs = {step["left_spec"], step["right_spec"]}
        assert all_specs & {"spec-a", "spec-b"}, (
            f"Expected spec-a or spec-b in merge step, got {all_specs}"
        )


# ── Escalation contract ───────────────────────────────────────────────


class TestEscalationContract:
    """Verify escalation fields when conflicts are present."""

    def test_conflicting_repo_produces_conflicts(self, conflicting_repo):
        repo, path = conflicting_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        # The fixture must produce conflicts — fail loudly if it doesn't
        assert report["total_conflicts"] > 0, (
            "conflicting_repo fixture did not produce any conflicts. "
            f"Report: is_clean={report['is_clean']}, "
            f"escalations={report.get('escalations', [])}"
        )

    def test_escalations_present_on_conflict(self, conflicting_repo):
        repo, path = conflicting_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        assert report["total_conflicts"] > 0, "Precondition: fixture must produce conflicts"
        assert "escalations" in report
        assert isinstance(report["escalations"], list)
        assert len(report["escalations"]) > 0, "Escalate strategy should produce escalations"

    def test_escalation_field_shape(self, conflicting_repo):
        repo, path = conflicting_repo
        report = repo.converge_all(strategy="escalate", apply=False)
        escalations = report.get("escalations", [])
        assert len(escalations) > 0, "Precondition: fixture must produce escalations"

        for esc in escalations:
            assert "file_path" in esc
            assert "reason" in esc
            assert "conflict_class" in esc
            assert "left_spec" in esc
            assert "right_spec" in esc
            assert "recommended_action" in esc

            assert isinstance(esc["file_path"], str)
            assert isinstance(esc["reason"], str)
            assert isinstance(esc["recommended_action"], str)


# ── Two-spec converge contract ────────────────────────────────────────


class TestConvergeContract:
    """Verify the two-spec converge() API returns the expected shape."""

    def test_converge_returns_dict(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge("spec-a", "spec-b")
        assert isinstance(report, dict)

    def test_converge_has_is_clean(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge("spec-a", "spec-b")
        assert "is_clean" in report
        assert isinstance(report["is_clean"], bool)

    def test_converge_has_auto_merged(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge("spec-a", "spec-b")
        assert "auto_merged" in report
        assert isinstance(report["auto_merged"], list)

    def test_converge_has_conflicts(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge("spec-a", "spec-b")
        assert "conflicts" in report
        assert isinstance(report["conflicts"], list)

    def test_converge_nonexistent_spec_raises(self, diverged_repo):
        repo, path = diverged_repo
        with pytest.raises(writ.WritError):
            repo.converge("spec-a", "nonexistent")


# ── diverged_branches contract ────────────────────────────────────────


class TestDivergedBranchesContract:
    """Verify diverged_branches() returns the expected shape."""

    def test_empty_for_no_specs(self, sealed_repo):
        repo, path = sealed_repo
        branches = repo.diverged_branches()
        assert isinstance(branches, list)
        assert len(branches) == 0

    def test_detects_diverged_branches(self, diverged_repo):
        repo, path = diverged_repo
        branches = repo.diverged_branches()
        assert isinstance(branches, list)
        assert len(branches) > 0, (
            "diverged_repo fixture should produce at least one diverged branch"
        )

    def test_diverged_branch_field_shape(self, diverged_repo):
        repo, path = diverged_repo
        branches = repo.diverged_branches()
        assert len(branches) > 0, "Precondition: must have diverged branches"

        branch = branches[0]
        assert "spec_id" in branch
        assert "tip_seal" in branch
        assert "seal_count" in branch
        assert "agents" in branch

        assert isinstance(branch["spec_id"], str)
        assert isinstance(branch["tip_seal"], str)
        assert isinstance(branch["seal_count"], int)
        assert isinstance(branch["agents"], list)
        assert branch["seal_count"] > 0

    def test_diverged_spec_b_detected(self, diverged_repo):
        repo, path = diverged_repo
        branches = repo.diverged_branches()
        spec_ids = [b["spec_id"] for b in branches]
        assert "spec-b" in spec_ids, (
            f"Expected spec-b to be diverged, got: {spec_ids}"
        )


# ── files_changed field ───────────────────────────────────────────────


class TestFilesChangedContract:
    """Verify the files_changed field in ConvergeAllReport."""

    def test_files_changed_present_after_apply(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=True)
        # files_changed may be empty list or populated list
        files = report.get("files_changed", [])
        assert isinstance(files, list)

    def test_files_changed_contains_strings(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="escalate", apply=True)
        files = report.get("files_changed", [])
        for f in files:
            assert isinstance(f, str)


# ── Strategy variations ───────────────────────────────────────────────


class TestStrategyContract:
    """Verify different strategy values produce valid reports."""

    def test_manual_strategy(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="manual", apply=False)
        assert isinstance(report, dict)
        assert report["strategy"] == "manual"

    def test_orchestrator_strategy(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="orchestrator", apply=False)
        assert isinstance(report, dict)
        assert report["strategy"] == "orchestrator"

    def test_unknown_strategy_defaults_to_escalate(self, diverged_repo):
        repo, path = diverged_repo
        report = repo.converge_all(strategy="unknown_value", apply=False)
        # Per lib.rs: unknown strategies default to Escalate
        assert report["strategy"] == "escalate"
