"""Tests for the writ Python bindings."""

import json
import os
import subprocess

import pytest
import writ


class TestInitAndOpen:
    def test_init_creates_repo(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        assert (tmp_path / ".writ").exists()

    def test_open_existing(self, tmp_path):
        writ.Repository.init(str(tmp_path))
        repo = writ.Repository.open(str(tmp_path))
        state = repo.state()
        assert state["changes"] == []

    def test_open_nonexistent_raises(self, tmp_path):
        with pytest.raises(writ.WritError):
            writ.Repository.open(str(tmp_path / "nonexistent"))


class TestState:
    def test_clean_after_init(self, tmp_repo):
        repo, path = tmp_repo
        state = repo.state()
        assert state["changes"] == []
        assert state["tracked_count"] == 0

    def test_new_file_detected(self, tmp_repo):
        repo, path = tmp_repo
        (path / "hello.txt").write_text("hello world")
        state = repo.state()
        assert len(state["changes"]) == 1
        assert state["changes"][0]["path"] == "hello.txt"
        assert state["changes"][0]["status"] == "new"


class TestSealAndLog:
    def test_seal_returns_dict(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(summary="added file")
        assert isinstance(seal, dict)
        assert seal["summary"] == "added file"
        assert len(seal["changes"]) == 1
        assert seal["changes"][0]["path"] == "file.txt"

    def test_seal_with_agent(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(
            summary="agent work",
            agent_id="worker-1",
            agent_type="agent",
            status="in-progress",
        )
        assert seal["agent"]["id"] == "worker-1"
        assert seal["agent"]["agent_type"] == "agent"
        assert seal["status"] == "in-progress"

    def test_log_returns_list(self, tmp_repo):
        repo, path = tmp_repo
        (path / "a.txt").write_text("aaa")
        repo.seal(summary="first")
        (path / "b.txt").write_text("bbb")
        repo.seal(summary="second")

        log = repo.log()
        assert isinstance(log, list)
        assert len(log) == 2
        assert log[0]["summary"] == "second"  # newest first
        assert log[1]["summary"] == "first"

    def test_log_with_limit(self, tmp_repo):
        repo, path = tmp_repo
        for i in range(5):
            (path / f"f{i}.txt").write_text(f"content{i}")
            repo.seal(summary=f"seal {i}")

        log = repo.log(limit=2)
        assert len(log) == 2

    def test_seal_nothing_raises(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.seal(summary="empty")


class TestSelectiveSeal:
    def test_paths_filters_changes(self, tmp_repo):
        repo, path = tmp_repo
        (path / "a.txt").write_text("aaa")
        (path / "b.txt").write_text("bbb")

        seal = repo.seal(summary="only a", paths=["a.txt"])
        assert len(seal["changes"]) == 1
        assert seal["changes"][0]["path"] == "a.txt"

        # b.txt still pending
        state = repo.state()
        assert len(state["changes"]) == 1
        assert state["changes"][0]["path"] == "b.txt"

    def test_directory_prefix(self, tmp_repo):
        repo, path = tmp_repo
        (path / "src").mkdir()
        (path / "src" / "main.rs").write_text("fn main() {}")
        (path / "readme.txt").write_text("hello")

        seal = repo.seal(summary="only src", paths=["src"])
        assert len(seal["changes"]) == 1
        assert seal["changes"][0]["path"] == "src/main.rs"


class TestDiff:
    def test_diff_working_tree(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("line1\nline2\n")
        repo.seal(summary="initial")
        (path / "file.txt").write_text("line1\nchanged\n")

        diff = repo.diff()
        assert isinstance(diff, dict)
        assert diff["files_changed"] == 1
        assert diff["total_additions"] > 0
        assert diff["total_deletions"] > 0

    def test_diff_seals(self, tmp_repo):
        repo, path = tmp_repo
        (path / "a.txt").write_text("original")
        seal1 = repo.seal(summary="first")
        (path / "a.txt").write_text("modified")
        seal2 = repo.seal(summary="second")

        diff = repo.diff_seals(seal1["id"], seal2["id"])
        assert diff["files_changed"] == 1

    def test_diff_seal_vs_parent(self, tmp_repo):
        repo, path = tmp_repo
        (path / "a.txt").write_text("content")
        seal = repo.seal(summary="first")

        diff = repo.diff_seal(seal["id"])
        assert diff["files_changed"] == 1
        assert "vs empty" in diff["description"]


class TestSpecs:
    def test_spec_lifecycle(self, tmp_repo):
        repo, path = tmp_repo
        created = repo.add_spec(id="auth", title="Auth Migration", description="Move to OAuth")
        assert isinstance(created, dict)
        assert created["id"] == "auth"
        assert "created_at" in created

        spec = repo.get_spec("auth")
        assert spec["id"] == "auth"
        assert spec["title"] == "Auth Migration"
        assert spec["status"] == "pending"

        updated = repo.update_spec("auth", status="in-progress")
        assert updated["status"] == "in-progress"

        specs = repo.list_specs()
        assert isinstance(specs, list)
        assert len(specs) == 1

    def test_spec_not_found(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.get_spec("nonexistent")

    def test_spec_update_fields(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")

        updated = repo.update_spec(
            "feat",
            depends_on=["dep-1", "dep-2"],
            file_scope=["src/main.rs"],
        )
        assert updated["depends_on"] == ["dep-1", "dep-2"]
        assert updated["file_scope"] == ["src/main.rs"]


class TestContext:
    def test_context_full(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        repo.seal(summary="initial")

        ctx = repo.context()
        assert isinstance(ctx, dict)
        assert "writ_version" in ctx
        assert ctx["tracked_files"] == 1
        assert len(ctx["recent_seals"]) == 1
        assert ctx["working_state"]["clean"] is True

    def test_context_spec_scoped(self, tmp_repo):
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        (path / "a.txt").write_text("aaa")
        repo.seal(summary="for feat", spec_id="feat")

        ctx = repo.context(spec="feat")
        assert ctx["active_spec"]["id"] == "feat"
        assert len(ctx["recent_seals"]) == 1


class TestGetSeal:
    def test_short_id(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(summary="test")

        loaded = repo.get_seal(seal["id"][:8])
        assert loaded["id"] == seal["id"]
        assert loaded["summary"] == "test"


class TestRestore:
    def test_restore_to_previous(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("original")
        seal1 = repo.seal(summary="first")
        (path / "file.txt").write_text("modified")
        repo.seal(summary="second")

        result = repo.restore(seal1["id"])
        assert isinstance(result, dict)
        assert "file.txt" in result["modified"]
        assert (path / "file.txt").read_text() == "original"


class TestErrorHandling:
    def test_writ_error_is_catchable(self, tmp_repo):
        repo, path = tmp_repo
        with pytest.raises(writ.WritError, match="seal not found"):
            repo.get_seal("nonexistent")

    def test_init_twice_raises(self, tmp_path):
        writ.Repository.init(str(tmp_path))
        with pytest.raises(writ.WritError):
            writ.Repository.init(str(tmp_path))


class TestEnums:
    def test_agent_type_values(self):
        assert writ.AgentType.Human != writ.AgentType.Agent

    def test_task_status_values(self):
        assert writ.TaskStatus.InProgress != writ.TaskStatus.Complete
        assert writ.TaskStatus.Complete != writ.TaskStatus.Blocked

    def test_spec_status_values(self):
        assert writ.SpecStatus.Pending != writ.SpecStatus.InProgress
        assert writ.SpecStatus.Complete != writ.SpecStatus.Blocked


class TestWritignore:
    def test_writignore_hides_files(self, tmp_repo):
        repo, path = tmp_repo
        (path / ".writignore").write_text("*.log\n")
        (path / "app.log").write_text("log data")
        (path / "main.py").write_text("print('hello')")

        state = repo.state()
        paths = [c["path"] for c in state["changes"]]
        assert "main.py" in paths
        assert "app.log" not in paths


class TestJsonRoundTrip:
    """Verify return dicts are JSON-serializable (key for LLM agents)."""

    def test_state_is_json_serializable(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        state = repo.state()
        result = json.dumps(state)
        assert isinstance(result, str)

    def test_seal_is_json_serializable(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(summary="test")
        result = json.dumps(seal)
        assert isinstance(result, str)

    def test_context_is_json_serializable(self, tmp_repo):
        repo, path = tmp_repo
        ctx = repo.context()
        result = json.dumps(ctx)
        assert isinstance(result, str)


class TestVerification:
    def test_seal_with_verification(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(
            summary="verified work",
            agent_id="worker-1",
            agent_type="agent",
            tests_passed=42,
            tests_failed=0,
            linted=True,
        )
        assert seal["verification"]["tests_passed"] == 42
        assert seal["verification"]["tests_failed"] == 0
        assert seal["verification"]["linted"] is True

    def test_seal_verification_default(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(summary="default verification")
        assert seal["verification"]["tests_passed"] is None
        assert seal["verification"]["tests_failed"] is None
        assert seal["verification"]["linted"] is False

    def test_seal_verification_partial(self, tmp_repo):
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        seal = repo.seal(summary="partial", tests_passed=10)
        assert seal["verification"]["tests_passed"] == 10
        assert seal["verification"]["tests_failed"] is None
        assert seal["verification"]["linted"] is False


class TestConvergenceBasic:
    """Basic convergence tests — clean merges, conflicts, apply, error handling."""

    def _setup_base_and_specs(self, repo, path):
        """Helper: create a base file and two specs."""
        (path / "shared.py").write_text("line1\nline2\nline3\nline4\nline5\n")
        repo.seal(summary="base state")
        repo.add_spec(id="feat-a", title="Feature A")
        repo.add_spec(id="feat-b", title="Feature B")

    def test_converge_clean(self, tmp_repo):
        """Two specs modify disjoint files — clean convergence."""
        repo, path = tmp_repo
        self._setup_base_and_specs(repo, path)

        # Spec A adds a new file.
        (path / "module_a.py").write_text("# module A\n")
        repo.seal(summary="add module a", spec_id="feat-a")

        # Spec B adds a different file.
        (path / "module_b.py").write_text("# module B\n")
        repo.seal(summary="add module b", spec_id="feat-b")

        report = repo.converge("feat-a", "feat-b")
        assert isinstance(report, dict)
        assert report["is_clean"] is True
        assert len(report["conflicts"]) == 0
        assert "module_a.py" in report["left_only"]
        assert "module_b.py" in report["right_only"]

    def test_converge_with_conflicts(self, tmp_repo):
        """Two specs modify the same line differently — conflict detected."""
        repo, path = tmp_repo
        self._setup_base_and_specs(repo, path)

        # Both specs change line 2 of shared.py.
        (path / "shared.py").write_text("line1\nFEATURE_A\nline3\nline4\nline5\n")
        repo.seal(summary="feat a in shared", spec_id="feat-a")

        (path / "shared.py").write_text("line1\nFEATURE_B\nline3\nline4\nline5\n")
        repo.seal(summary="feat b in shared", spec_id="feat-b")

        report = repo.converge("feat-a", "feat-b")
        assert report["is_clean"] is False
        assert len(report["conflicts"]) == 1
        assert report["conflicts"][0]["path"] == "shared.py"
        # Verify structured conflict data.
        regions = report["conflicts"][0]["regions"]
        assert len(regions) >= 1
        assert regions[0]["left_lines"] == ["FEATURE_A"]
        assert regions[0]["right_lines"] == ["FEATURE_B"]

    def test_apply_convergence(self, tmp_repo):
        """Apply a clean convergence and verify files on disk."""
        repo, path = tmp_repo
        self._setup_base_and_specs(repo, path)

        # Non-overlapping changes to shared.py.
        (path / "shared.py").write_text("CHANGED_A\nline2\nline3\nline4\nline5\n")
        repo.seal(summary="change top", spec_id="feat-a")

        (path / "shared.py").write_text("line1\nline2\nline3\nline4\nCHANGED_B\n")
        repo.seal(summary="change bottom", spec_id="feat-b")

        report = repo.converge("feat-a", "feat-b")
        assert report["is_clean"] is True

        repo.apply_convergence(report)

        # Verify merged content on disk.
        content = (path / "shared.py").read_text()
        assert "CHANGED_A" in content
        assert "CHANGED_B" in content

    def test_converge_unknown_spec(self, tmp_repo):
        """Converging with a nonexistent spec raises WritError."""
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.converge("nonexistent-a", "nonexistent-b")

    def test_converge_report_is_json_serializable(self, tmp_repo):
        """Convergence report should be JSON-serializable for agents."""
        repo, path = tmp_repo
        self._setup_base_and_specs(repo, path)

        (path / "module_a.py").write_text("# A\n")
        repo.seal(summary="a work", spec_id="feat-a")

        (path / "module_b.py").write_text("# B\n")
        repo.seal(summary="b work", spec_id="feat-b")

        report = repo.converge("feat-a", "feat-b")
        result = json.dumps(report)
        assert isinstance(result, str)


class TestConvergeAll:
    def _setup_diverged(self, tmp_path):
        """Create a repo with alpha on HEAD, beta and gamma diverged."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "shared.txt").write_text("line1\nline2\nline3\n")
        repo.seal(summary="baseline", agent_id="setup", agent_type="agent")

        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")
        repo.add_spec(id="gamma", title="Gamma")

        # Alpha seals (stays on HEAD chain).
        (tmp_path / "alpha.txt").write_text("alpha content\n")
        repo.seal(summary="alpha work", agent_id="alpha-dev", spec_id="alpha")

        # Beta seals — diverges from HEAD because alpha advanced it.
        (tmp_path / "beta.txt").write_text("beta content\n")
        repo.seal(summary="beta work", agent_id="beta-dev", spec_id="beta")

        # Gamma seals — also diverges.
        (tmp_path / "gamma.txt").write_text("gamma content\n")
        repo.seal(summary="gamma work", agent_id="gamma-dev", spec_id="gamma")

        # Alpha seals again to advance HEAD past beta/gamma.
        (tmp_path / "alpha2.txt").write_text("alpha part 2\n")
        repo.seal(summary="alpha complete", agent_id="alpha-dev", spec_id="alpha")

        # Mark alpha Complete so it's eligible as base spec.
        repo.update_spec("alpha", status="complete")

        return repo

    def test_converge_all_no_diverged(self, tmp_repo):
        """converge_all on a repo with no diverged branches returns empty."""
        repo, path = tmp_repo
        (path / "a.txt").write_text("content\n")
        repo.seal(summary="work", agent_id="dev")
        report = repo.converge_all()
        assert isinstance(report, dict)
        assert report["is_clean"] is True
        assert len(report["merges"]) == 0

    def test_converge_all_clean_multi_branch(self, tmp_path):
        """converge_all merges disjoint diverged branches cleanly."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge_all()
        assert report["is_clean"] is True
        assert report["base_spec"] == "alpha"
        assert len(report["merge_order"]) > 0
        assert report["total_conflicts"] == 0

    def test_converge_all_with_apply(self, tmp_path):
        """converge_all with apply=True writes files to disk."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge_all(apply=True)
        assert report["applied"] is True
        assert (tmp_path / "beta.txt").exists()
        assert (tmp_path / "gamma.txt").exists()

    def test_converge_all_most_recent_resolves_conflicts(self, tmp_path):
        """MostRecent strategy auto-resolves conflicts."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "shared.txt").write_text("original\n")
        repo.seal(summary="baseline", agent_id="setup")

        repo.add_spec(id="left", title="Left")
        repo.add_spec(id="right", title="Right")

        (tmp_path / "shared.txt").write_text("LEFT_VERSION\n")
        repo.seal(summary="left change", agent_id="left-dev", spec_id="left")

        (tmp_path / "shared.txt").write_text("RIGHT_VERSION\n")
        repo.seal(summary="right change", agent_id="right-dev", spec_id="right")

        # Restore LEFT on disk before left's second seal.
        (tmp_path / "shared.txt").write_text("LEFT_VERSION\n")
        (tmp_path / "left-only.txt").write_text("extra\n")
        repo.seal(summary="left extra", agent_id="left-dev", spec_id="left")
        repo.update_spec("left", status="complete")

        report = repo.converge_all(strategy="most-recent", apply=True)
        assert report["is_clean"] is True
        assert report["total_resolutions"] > 0
        assert len(report["warnings"]) > 0

    def test_converge_all_report_serializable(self, tmp_path):
        """converge_all report is JSON-serializable."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge_all()
        result = json.dumps(report)
        assert isinstance(result, str)
        assert "base_spec" in result
        assert "strategy" in result

    def test_converge_all_step_results(self, tmp_path):
        """Each merge step has expected fields."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge_all()
        assert len(report["merges"]) == len(report["merge_order"])
        for step in report["merges"]:
            assert "left_spec" in step
            assert "right_spec" in step
            assert "auto_merged" in step
            assert "conflicts" in step
            assert "clean" in step

    def test_converge_all_default_strategy(self, tmp_path):
        """Default strategy is three-way-merge."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge_all()
        assert report["strategy"] == "three-way-merge"


class TestAllowEmpty:
    def test_seal_allow_empty(self, tmp_repo):
        """allow_empty=True on a clean repo succeeds."""
        repo, path = tmp_repo
        seal = repo.seal(summary="metadata only", allow_empty=True)
        assert isinstance(seal, dict)
        assert seal["summary"] == "metadata only"
        assert len(seal["changes"]) == 0

    def test_seal_allow_empty_default_false(self, tmp_repo):
        """Default allow_empty (False) still raises on clean repo."""
        repo, path = tmp_repo
        with pytest.raises(writ.WritError):
            repo.seal(summary="should fail")


class TestEnrichedContext:
    def test_context_seal_summary_enriched(self, tmp_repo):
        """Verification data appears in context seal summaries."""
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        repo.seal(
            summary="verified",
            agent_id="worker-1",
            agent_type="agent",
            tests_passed=42,
            tests_failed=0,
            linted=True,
        )

        ctx = repo.context()
        seal_summary = ctx["recent_seals"][0]
        assert seal_summary["status"] == "complete"
        assert seal_summary["verification"]["tests_passed"] == 42
        assert seal_summary["verification"]["linted"] is True

    def test_context_available_operations(self, tmp_repo):
        """available_operations is present and non-empty."""
        repo, path = tmp_repo
        ctx = repo.context()
        assert "available_operations" in ctx
        assert isinstance(ctx["available_operations"], list)
        assert len(ctx["available_operations"]) > 0

    def test_context_seal_summary_omits_empty_verification(self, tmp_repo):
        """Empty verification is omitted from context seal summaries."""
        repo, path = tmp_repo
        (path / "file.txt").write_text("content")
        repo.seal(summary="no verification")

        ctx = repo.context()
        seal_summary = ctx["recent_seals"][0]
        assert seal_summary["status"] == "complete"
        assert "verification" not in seal_summary


class TestContextFiltering:
    def test_filter_by_status(self, tmp_repo):
        """Filter seals by task status."""
        repo, path = tmp_repo
        (path / "a.txt").write_text("alpha")
        repo.seal(summary="wip", status="in-progress")
        (path / "b.txt").write_text("beta")
        repo.seal(summary="done", status="complete")

        ctx = repo.context(status="complete")
        assert len(ctx["recent_seals"]) == 1
        assert ctx["recent_seals"][0]["status"] == "complete"

        ctx = repo.context(status="in-progress")
        assert len(ctx["recent_seals"]) == 1
        assert ctx["recent_seals"][0]["status"] == "in-progress"

    def test_filter_by_agent(self, tmp_repo):
        """Filter seals by agent ID."""
        repo, path = tmp_repo
        (path / "a.txt").write_text("alpha")
        repo.seal(summary="by alpha", agent_id="agent-alpha", agent_type="agent")
        (path / "b.txt").write_text("beta")
        repo.seal(summary="by beta", agent_id="agent-beta", agent_type="agent")

        ctx = repo.context(agent="agent-alpha")
        assert len(ctx["recent_seals"]) == 1
        assert ctx["recent_seals"][0]["agent"] == "agent-alpha"

    def test_filter_combined(self, tmp_repo):
        """Combined status + agent filter."""
        repo, path = tmp_repo
        (path / "a.txt").write_text("a")
        repo.seal(summary="w1 wip", agent_id="w1", agent_type="agent", status="in-progress")
        (path / "b.txt").write_text("b")
        repo.seal(summary="w2 done", agent_id="w2", agent_type="agent", status="complete")
        (path / "c.txt").write_text("c")
        repo.seal(summary="w1 done", agent_id="w1", agent_type="agent", status="complete")

        ctx = repo.context(status="complete", agent="w1")
        assert len(ctx["recent_seals"]) == 1
        assert ctx["recent_seals"][0]["summary"] == "w1 done"

    def test_filter_invalid_status(self, tmp_repo):
        """Invalid status filter raises ValueError."""
        repo, _ = tmp_repo
        with pytest.raises(ValueError, match="unknown status"):
            repo.context(status="banana")

    def test_no_filter_returns_all(self, tmp_repo):
        """Default (no filter) returns all seals."""
        repo, path = tmp_repo
        (path / "a.txt").write_text("a")
        repo.seal(summary="first")
        (path / "b.txt").write_text("b")
        repo.seal(summary="second")

        ctx = repo.context()
        assert len(ctx["recent_seals"]) == 2


class TestSealNudge:
    def test_nudge_present_when_dirty(self, tmp_repo):
        """seal_nudge appears when working directory has unsealed changes."""
        repo, path = tmp_repo
        (path / "tracked.txt").write_text("initial")
        repo.seal(summary="base")

        (path / "tracked.txt").write_text("modified")
        (path / "new.txt").write_text("brand new")

        ctx = repo.context()
        assert "seal_nudge" in ctx
        nudge = ctx["seal_nudge"]
        assert nudge["unsealed_file_count"] == 2
        assert "2 file(s) changed" in nudge["message"]

    def test_nudge_absent_when_clean(self, tmp_repo):
        """seal_nudge is omitted when working directory is clean."""
        repo, path = tmp_repo
        (path / "file.txt").write_text("clean")
        repo.seal(summary="sealed")

        ctx = repo.context()
        assert "seal_nudge" not in ctx


class TestChangedPaths:
    def test_seal_summary_has_paths(self, tmp_repo):
        """Seal summaries include changed_paths for file relevance."""
        repo, path = tmp_repo
        (path / "auth.py").write_text("pass")
        (path / "main.py").write_text("run")
        repo.seal(summary="initial")

        ctx = repo.context()
        paths = ctx["recent_seals"][0]["changed_paths"]
        assert len(paths) == 2
        assert "auth.py" in paths
        assert "main.py" in paths

    def test_changed_paths_omitted_when_empty(self, tmp_repo):
        """changed_paths is omitted from JSON when empty (allow_empty seal)."""
        repo, path = tmp_repo
        (path / "file.txt").write_text("x")
        repo.seal(summary="has files")

        repo.seal(summary="metadata only", allow_empty=True)
        ctx = repo.context()
        empty_seal = ctx["recent_seals"][0]
        assert "changed_paths" not in empty_seal


class TestBridge:
    def _setup_git_repo(self, path):
        """Initialize a git repo with some files and an initial commit."""
        import subprocess

        subprocess.run(["git", "init"], cwd=str(path), check=True, capture_output=True)
        subprocess.run(
            ["git", "config", "user.email", "test@test.com"],
            cwd=str(path),
            check=True,
            capture_output=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test"],
            cwd=str(path),
            check=True,
            capture_output=True,
        )
        (path / "main.py").write_text("print('hello')")
        (path / "README.md").write_text("# Project")
        subprocess.run(
            ["git", "add", "."], cwd=str(path), check=True, capture_output=True
        )
        subprocess.run(
            ["git", "commit", "-m", "initial"],
            cwd=str(path),
            check=True,
            capture_output=True,
        )

    def test_bridge_import(self, tmp_path):
        """bridge_import creates a baseline seal from git state."""
        self._setup_git_repo(tmp_path)
        repo = writ.Repository.init(str(tmp_path))
        result = repo.bridge_import()
        assert isinstance(result, dict)
        assert "seal_id" in result
        assert result["files_imported"] > 0
        assert result["git_ref"] == "HEAD"

    def test_bridge_export(self, tmp_path):
        """bridge_export creates git commits from writ seals."""
        self._setup_git_repo(tmp_path)
        repo = writ.Repository.init(str(tmp_path))
        repo.bridge_import()

        (tmp_path / "new_file.py").write_text("# new")
        repo.seal(summary="agent work")

        result = repo.bridge_export()
        assert isinstance(result, dict)
        assert result["seals_exported"] == 1
        assert result["branch"] == "writ/export"

    def test_bridge_status(self, tmp_path):
        """bridge_status reports sync state."""
        self._setup_git_repo(tmp_path)
        repo = writ.Repository.init(str(tmp_path))

        status = repo.bridge_status()
        assert status["initialized"] is False

        repo.bridge_import()
        status = repo.bridge_status()
        assert status["initialized"] is True
        assert status["pending_export_count"] == 0

    def test_bridge_roundtrip_json_serializable(self, tmp_path):
        """All bridge results are JSON-serializable."""
        import json

        self._setup_git_repo(tmp_path)
        repo = writ.Repository.init(str(tmp_path))
        import_result = repo.bridge_import()
        assert isinstance(json.dumps(import_result), str)

        (tmp_path / "x.txt").write_text("x")
        repo.seal(summary="change")
        export_result = repo.bridge_export()
        assert isinstance(json.dumps(export_result), str)

        status = repo.bridge_status()
        assert isinstance(json.dumps(status), str)

    def test_bridge_no_git_repo_raises(self, tmp_repo):
        """bridge_import on a non-git directory raises WritError."""
        repo, path = tmp_repo
        with pytest.raises(writ.WritError, match="git"):
            repo.bridge_import()


class TestLocking:
    def test_lock_timeout_error(self, tmp_repo):
        """Hold a lock via the filesystem and verify seal raises WritError."""
        import fcntl

        repo, path = tmp_repo
        (path / "file.txt").write_text("content")

        lock_path = path / ".writ" / "writ.lock"
        lock_file = open(lock_path, "w")
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)

        try:
            with pytest.raises(writ.WritError, match="lock"):
                repo.seal(summary="should timeout")
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            lock_file.close()


class TestRichContext:
    def test_spec_with_acceptance_criteria(self, tmp_repo):
        """add_spec with acceptance_criteria returns them in the dict."""
        repo, path = tmp_repo
        spec = repo.add_spec(
            id="auth",
            title="Auth Migration",
            acceptance_criteria=["OAuth flow works", "Tests pass"],
        )
        assert spec["acceptance_criteria"] == ["OAuth flow works", "Tests pass"]

    def test_spec_with_design_notes(self, tmp_repo):
        """add_spec with design_notes returns them in the dict."""
        repo, path = tmp_repo
        spec = repo.add_spec(
            id="auth",
            title="Auth Migration",
            design_notes=["Use JWT for stateless auth"],
        )
        assert spec["design_notes"] == ["Use JWT for stateless auth"]

    def test_spec_with_tech_stack(self, tmp_repo):
        """add_spec with tech_stack returns them in the dict."""
        repo, path = tmp_repo
        spec = repo.add_spec(
            id="auth",
            title="Auth Migration",
            tech_stack=["rust", "pyo3", "tokio"],
        )
        assert spec["tech_stack"] == ["rust", "pyo3", "tokio"]

    def test_update_spec_enrichment(self, tmp_repo):
        """update_spec can set enrichment fields."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")
        updated = repo.update_spec(
            "feat",
            acceptance_criteria=["Compiles", "Tests green"],
            design_notes=["Prefer composition over inheritance"],
            tech_stack=["python", "fastapi"],
        )
        assert updated["acceptance_criteria"] == ["Compiles", "Tests green"]
        assert updated["design_notes"] == ["Prefer composition over inheritance"]
        assert updated["tech_stack"] == ["python", "fastapi"]

    def test_backwards_compat_empty_fields(self, tmp_repo):
        """Specs created without enrichment fields still work."""
        repo, path = tmp_repo
        spec = repo.add_spec(id="plain", title="Plain Spec")
        # Empty enrichment fields are omitted from serialization
        assert "acceptance_criteria" not in spec
        assert "design_notes" not in spec
        assert "tech_stack" not in spec

    def test_context_dependency_status(self, tmp_repo):
        """Spec-scoped context includes dependency_status."""
        repo, path = tmp_repo
        repo.add_spec(id="dep-1", title="Dependency")
        repo.update_spec("dep-1", status="complete")
        repo.add_spec(id="main", title="Main")
        repo.update_spec("main", depends_on=["dep-1"])

        (path / "file.txt").write_text("content")
        repo.seal(summary="work", spec_id="main", status="in-progress")

        ctx = repo.context(spec="main")
        assert "dependency_status" in ctx
        deps = ctx["dependency_status"]
        assert len(deps) == 1
        assert deps[0]["spec_id"] == "dep-1"
        assert deps[0]["status"] == "complete"
        assert deps[0]["resolved"] is True

    def test_context_spec_progress(self, tmp_repo):
        """Spec-scoped context includes spec_progress."""
        repo, path = tmp_repo
        repo.add_spec(id="feat", title="Feature")

        (path / "a.txt").write_text("alpha")
        repo.seal(
            summary="design",
            agent_id="designer",
            agent_type="agent",
            spec_id="feat",
            status="in-progress",
        )
        (path / "b.txt").write_text("beta")
        repo.seal(
            summary="impl",
            agent_id="coder",
            agent_type="agent",
            spec_id="feat",
            status="complete",
        )

        ctx = repo.context(spec="feat")
        assert "spec_progress" in ctx
        progress = ctx["spec_progress"]
        assert progress["total_seals"] == 2
        assert len(progress["agents_involved"]) == 2
        assert "designer" in progress["agents_involved"]
        assert "coder" in progress["agents_involved"]

    def test_enrichment_json_serializable(self, tmp_repo):
        """All enrichment fields survive json.dumps()."""
        repo, path = tmp_repo
        spec = repo.add_spec(
            id="rich",
            title="Rich Spec",
            acceptance_criteria=["criterion"],
            design_notes=["note"],
            tech_stack=["rust"],
        )
        result = json.dumps(spec)
        assert isinstance(result, str)
        assert "criterion" in result


class TestRemote:
    def test_remote_init(self, tmp_path):
        """remote_init creates the bare directory structure."""
        remote_dir = tmp_path / "remote"
        remote_dir.mkdir()
        writ.Repository.remote_init(str(remote_dir))

        assert (remote_dir / "objects").is_dir()
        assert (remote_dir / "seals").is_dir()
        assert (remote_dir / "specs").is_dir()
        assert (remote_dir / "HEAD").exists()

    def test_push_pull_roundtrip(self, tmp_path):
        """Push from one repo, pull into another, verify state matches."""
        work1 = tmp_path / "work1"
        work2 = tmp_path / "work2"
        remote_dir = tmp_path / "remote"
        work1.mkdir()
        work2.mkdir()
        remote_dir.mkdir()

        writ.Repository.remote_init(str(remote_dir))

        # Repo 1: create content and push
        repo1 = writ.Repository.init(str(work1))
        (work1 / "hello.txt").write_text("hello world")
        repo1.seal(summary="Initial seal", agent_id="test-agent", agent_type="agent")
        repo1.remote_add("origin", str(remote_dir))
        result = repo1.push("origin")

        assert result["remote"] == "origin"
        assert result["objects_pushed"] > 0
        assert result["seals_pushed"] > 0
        assert result["head_updated"] is True

        # Repo 2: pull
        repo2 = writ.Repository.init(str(work2))
        repo2.remote_add("origin", str(remote_dir))
        pull_result = repo2.pull("origin")

        assert pull_result["objects_pulled"] > 0
        assert pull_result["seals_pulled"] > 0
        assert pull_result["head_updated"] is True

        # Verify logs match
        log1 = repo1.log()
        log2 = repo2.log()
        assert len(log1) == len(log2)
        assert log1[0]["id"] == log2[0]["id"]

    def test_push_diverged_raises(self, tmp_path):
        """Diverged histories should raise WritError on push."""
        work1 = tmp_path / "work1"
        work2 = tmp_path / "work2"
        remote_dir = tmp_path / "remote"
        work1.mkdir()
        work2.mkdir()
        remote_dir.mkdir()

        writ.Repository.remote_init(str(remote_dir))

        # Repo 1
        repo1 = writ.Repository.init(str(work1))
        (work1 / "a.txt").write_text("alpha")
        repo1.seal(summary="R1 seal", agent_id="agent-1", agent_type="agent")
        repo1.remote_add("origin", str(remote_dir))
        repo1.push("origin")

        # Repo 2 (independent history)
        repo2 = writ.Repository.init(str(work2))
        (work2 / "b.txt").write_text("beta")
        repo2.seal(summary="R2 seal", agent_id="agent-2", agent_type="agent")
        repo2.remote_add("origin", str(remote_dir))

        with pytest.raises(writ.WritError, match="diverged"):
            repo2.push("origin")

    def test_remote_status(self, tmp_path):
        """remote_status shows ahead/behind counts."""
        work1 = tmp_path / "work1"
        work2 = tmp_path / "work2"
        remote_dir = tmp_path / "remote"
        work1.mkdir()
        work2.mkdir()
        remote_dir.mkdir()

        writ.Repository.remote_init(str(remote_dir))

        # Repo 1: push initial
        repo1 = writ.Repository.init(str(work1))
        (work1 / "a.txt").write_text("alpha")
        repo1.seal(summary="initial", agent_id="agent-1", agent_type="agent")
        repo1.remote_add("origin", str(remote_dir))
        repo1.push("origin")

        # Repo 2: pull, then repo1 pushes more
        repo2 = writ.Repository.init(str(work2))
        repo2.remote_add("origin", str(remote_dir))
        repo2.pull("origin")

        (work1 / "b.txt").write_text("beta")
        repo1.seal(summary="more work", agent_id="agent-1", agent_type="agent")
        repo1.push("origin")

        status = repo2.remote_status("origin")
        assert status["name"] == "origin"
        assert status["behind"] > 0

    def test_push_pull_json_serializable(self, tmp_path):
        """Push and pull results are JSON-safe."""
        work = tmp_path / "work"
        remote_dir = tmp_path / "remote"
        work.mkdir()
        remote_dir.mkdir()

        writ.Repository.remote_init(str(remote_dir))

        repo = writ.Repository.init(str(work))
        (work / "file.txt").write_text("content")
        repo.seal(summary="test", agent_id="agent", agent_type="agent")
        repo.remote_add("origin", str(remote_dir))

        push_result = repo.push("origin")
        assert isinstance(json.dumps(push_result), str)

        pull_result = repo.pull("origin")
        assert isinstance(json.dumps(pull_result), str)

        status = repo.remote_status("origin")
        assert isinstance(json.dumps(status), str)


class TestInstall:
    """Tests for the enhanced writ install command."""

    def test_install_returns_dict(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        assert isinstance(result, dict)

    def test_install_has_new_fields(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        assert "repo_root" in result
        assert "writignore_created" in result
        assert "already_imported" in result
        assert "reimported" in result
        assert "tracked_files" in result
        assert "available_operations" in result

    def test_install_initializes(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        assert result["initialized"] is True
        assert result["git_detected"] is False
        assert (tmp_path / ".writ").exists()

    def test_install_idempotent(self, tmp_path):
        first = writ.Repository.install(str(tmp_path))
        assert first["initialized"] is True
        assert first["writignore_created"] is True

        second = writ.Repository.install(str(tmp_path))
        assert second["initialized"] is False
        assert second["writignore_created"] is False

    def test_install_creates_writignore(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        assert result["writignore_created"] is True
        assert (tmp_path / ".writignore").exists()

    def test_install_preserves_writignore(self, tmp_path):
        (tmp_path / ".writignore").write_text("my_custom\n")
        result = writ.Repository.install(str(tmp_path))
        assert result["writignore_created"] is False
        assert (tmp_path / ".writignore").read_text() == "my_custom\n"

    def test_install_json_serializable(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        serialized = json.dumps(result)
        assert isinstance(serialized, str)

    def test_install_detects_claude_code(self, tmp_path):
        (tmp_path / "CLAUDE.md").write_text("# Project")
        result = writ.Repository.install(str(tmp_path))
        detected = [f for f in result.get("frameworks_detected", []) if f["detected"]]
        assert len(detected) > 0

    def test_install_available_operations(self, tmp_path):
        result = writ.Repository.install(str(tmp_path))
        ops = result["available_operations"]
        assert isinstance(ops, list)
        assert len(ops) > 0
        assert any("context" in op for op in ops)


class TestAutoConflictDetection:
    """P0: seal() auto-detects conflicts when context() was called first."""

    def test_seal_without_context_no_warning(self, tmp_path):
        """Standard seal without context() has no conflict_warning."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("hello")
        result = repo.seal(summary="first", agent_id="a1", agent_type="agent")
        # Without context() call, no automatic detection.
        assert result.get("conflict_warning") is None

    def test_seal_after_context_no_conflict(self, tmp_path):
        """context() + seal() with no HEAD movement: no warning."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("v1")
        repo.seal(summary="base", agent_id="a1", agent_type="agent")

        repo.context()  # Records HEAD.
        (tmp_path / "a.txt").write_text("v2")
        result = repo.seal(summary="update", agent_id="a1", agent_type="agent")
        assert result.get("conflict_warning") is None

    def test_seal_after_context_detects_conflict(self, tmp_path):
        """context() + another agent seals + seal(): conflict_warning present."""
        repo_a = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("v1")
        repo_a.seal(summary="base", agent_id="a1", agent_type="agent")

        # Agent A calls context, records HEAD.
        repo_a.context()

        # Agent B (separate repo instance) seals, moving HEAD on disk.
        repo_b = writ.Repository.open(str(tmp_path))
        (tmp_path / "b.txt").write_text("agent-b-work")
        repo_b.seal(summary="agent-b", agent_id="a2", agent_type="agent")

        # Agent A seals — should detect HEAD moved.
        (tmp_path / "a.txt").write_text("v2")
        result = repo_a.seal(summary="agent-a", agent_id="a1", agent_type="agent")
        assert result.get("conflict_warning") is not None
        warning = result["conflict_warning"]
        assert warning["is_clean"] is True  # Different files, no overlap.
        assert len(warning["intervening_seals"]) == 1

    def test_conflict_warning_shows_overlapping_files(self, tmp_path):
        """Conflict warning reports overlapping files."""
        repo_a = writ.Repository.init(str(tmp_path))
        (tmp_path / "shared.txt").write_text("v1")
        repo_a.seal(summary="base", agent_id="a1", agent_type="agent")

        repo_a.context()

        # Agent B modifies the same file.
        repo_b = writ.Repository.open(str(tmp_path))
        (tmp_path / "shared.txt").write_text("agent-b-edit")
        repo_b.seal(summary="agent-b", agent_id="a2", agent_type="agent")

        (tmp_path / "shared.txt").write_text("agent-a-edit")
        result = repo_a.seal(summary="agent-a", agent_id="a1", agent_type="agent")
        warning = result["conflict_warning"]
        assert warning["is_clean"] is False
        assert "shared.txt" in warning["overlapping_files"]

    def test_context_head_clears_after_seal(self, tmp_path):
        """After seal(), the tracked HEAD is cleared."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("v1")
        repo.seal(summary="base", agent_id="a1", agent_type="agent")

        repo.context()
        (tmp_path / "a.txt").write_text("v2")
        repo.seal(summary="update", agent_id="a1", agent_type="agent")

        # Second seal without new context() call: no warning.
        (tmp_path / "a.txt").write_text("v3")
        result = repo.seal(summary="third", agent_id="a1", agent_type="agent")
        assert result.get("conflict_warning") is None


class TestSpecScopedContextFiltering:
    """P0: context(spec=X) filters working_state, pending_changes, seal_nudge."""

    def test_filters_by_file_scope(self, tmp_path):
        """Only spec-scoped files appear in working_state."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="auth", title="Auth")
        repo.update_spec("auth", file_scope=["src/auth.py"])

        os.makedirs(str(tmp_path / "src"))
        (tmp_path / "src" / "auth.py").write_text("auth code")
        (tmp_path / "readme.md").write_text("docs")
        repo.seal(summary="base", agent_id="a1", agent_type="agent", spec_id="auth")

        (tmp_path / "src" / "auth.py").write_text("auth v2")
        (tmp_path / "readme.md").write_text("docs v2")

        ctx = repo.context(spec="auth")
        assert "src/auth.py" in ctx["working_state"]["modified_files"]
        assert "readme.md" not in ctx["working_state"]["modified_files"]

    def test_filters_pending_changes(self, tmp_path):
        """Only spec-scoped files in pending_changes."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="ui", title="UI")
        repo.update_spec("ui", file_scope=["style.css"])

        (tmp_path / "style.css").write_text("body {}")
        (tmp_path / "app.js").write_text("console.log()")
        repo.seal(summary="base", agent_id="a1", agent_type="agent", spec_id="ui")

        (tmp_path / "style.css").write_text("body { color: red }")
        (tmp_path / "app.js").write_text("changed")

        ctx = repo.context(spec="ui")
        pc = ctx["pending_changes"]
        assert pc["files_changed"] == 1
        assert pc["files"][0]["path"] == "style.css"

    def test_no_nudge_for_unrelated_changes(self, tmp_path):
        """Seal nudge is absent when only non-spec files changed."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="api", title="API")
        repo.update_spec("api", file_scope=["api.py"])

        (tmp_path / "api.py").write_text("v1")
        (tmp_path / "other.py").write_text("v1")
        repo.seal(summary="base", agent_id="a1", agent_type="agent", spec_id="api")

        (tmp_path / "other.py").write_text("v2")

        ctx = repo.context(spec="api")
        assert ctx.get("seal_nudge") is None

    def test_infers_scope_from_seals(self, tmp_path):
        """When no file_scope set, infer from files in spec's seals."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "feature.py").write_text("v1")
        repo.seal(summary="impl", agent_id="a1", agent_type="agent", spec_id="feat")

        (tmp_path / "unrelated.py").write_text("v1")
        repo.seal(summary="other", agent_id="a1", agent_type="agent")

        (tmp_path / "feature.py").write_text("v2")
        (tmp_path / "unrelated.py").write_text("v2")

        ctx = repo.context(spec="feat")
        assert "feature.py" in ctx["working_state"]["modified_files"]
        assert "unrelated.py" not in ctx["working_state"]["modified_files"]


class TestAgentActivity:
    """P1: agent_activity in context shows per-agent file ownership and provenance."""

    def test_empty_when_no_seals(self, tmp_path):
        """No agent_activity when repo has no seals."""
        repo = writ.Repository.init(str(tmp_path))
        ctx = repo.context()
        assert ctx.get("agent_activity", []) == []

    def test_single_agent_owns_files(self, tmp_path):
        """Single agent appears with all files they sealed."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("aaa")
        (tmp_path / "b.txt").write_text("bbb")
        repo.seal(summary="init", agent_id="worker-1", agent_type="agent")

        ctx = repo.context()
        activity = ctx["agent_activity"]
        assert len(activity) == 1
        assert activity[0]["agent_id"] == "worker-1"
        assert "a.txt" in activity[0]["files_owned"]
        assert "b.txt" in activity[0]["files_owned"]
        assert activity[0]["seal_count"] == 1

    def test_multi_agent_provenance(self, tmp_path):
        """File ownership reflects the most recent sealer."""
        repo = writ.Repository.init(str(tmp_path))

        # Agent A creates shared.txt and a.txt.
        (tmp_path / "shared.txt").write_text("v1")
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="a work", agent_id="agent-a", agent_type="agent")

        # Agent B modifies shared.txt and creates b.txt.
        (tmp_path / "shared.txt").write_text("v2")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="b work", agent_id="agent-b", agent_type="agent")

        ctx = repo.context()
        activity = {a["agent_id"]: a for a in ctx["agent_activity"]}

        # Agent A still owns a.txt.
        assert "a.txt" in activity["agent-a"]["files_owned"]
        # Agent B owns shared.txt (more recent) and b.txt.
        assert "shared.txt" in activity["agent-b"]["files_owned"]
        assert "b.txt" in activity["agent-b"]["files_owned"]
        # Agent A lost ownership of shared.txt.
        assert "shared.txt" not in activity["agent-a"]["files_owned"]

    def test_has_latest_summary(self, tmp_path):
        """Agent activity includes the most recent seal summary."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("v1")
        repo.seal(summary="first", agent_id="dev", agent_type="agent")
        (tmp_path / "a.txt").write_text("v2")
        repo.seal(summary="second", agent_id="dev", agent_type="agent")

        ctx = repo.context()
        assert ctx["agent_activity"][0]["latest_summary"] == "second"
        assert ctx["agent_activity"][0]["seal_count"] == 2

    def test_tracks_specs(self, tmp_path):
        """Agent activity includes specs touched."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="auth", title="Auth")
        repo.add_spec(id="ui", title="UI")

        (tmp_path / "auth.py").write_text("pass")
        repo.seal(summary="auth", agent_id="dev", agent_type="agent", spec_id="auth")
        (tmp_path / "ui.py").write_text("pass")
        repo.seal(summary="ui", agent_id="dev", agent_type="agent", spec_id="ui")

        ctx = repo.context()
        specs = ctx["agent_activity"][0]["specs_touched"]
        assert "auth" in specs
        assert "ui" in specs

    def test_spec_scoped_filters_files(self, tmp_path):
        """Spec-scoped context filters agent_activity to spec-relevant files."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="api", title="API")
        repo.update_spec("api", file_scope=["api.py"])

        (tmp_path / "api.py").write_text("v1")
        (tmp_path / "config.py").write_text("v1")
        repo.seal(summary="work", agent_id="dev", agent_type="agent", spec_id="api")

        ctx = repo.context(spec="api")
        activity = ctx["agent_activity"][0]
        assert "api.py" in activity["files_owned"]
        assert "config.py" not in activity["files_owned"]

    def test_json_serializable(self, tmp_path):
        """Agent activity survives JSON round-trip."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("hello")
        repo.seal(summary="work", agent_id="worker", agent_type="agent")

        ctx = repo.context()
        serialized = json.dumps(ctx)
        parsed = json.loads(serialized)
        assert len(parsed["agent_activity"]) == 1
        assert parsed["agent_activity"][0]["agent_id"] == "worker"


class TestLogAll:
    """Sprint 2: Unified multi-head log shows seals from all branches."""

    def test_log_all_empty(self, tmp_path):
        """Empty repo returns empty list."""
        repo = writ.Repository.init(str(tmp_path))
        assert repo.log_all() == []

    def test_log_all_matches_log_when_linear(self, tmp_path):
        """Without diverged branches, log_all matches log."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="first", agent_id="dev", agent_type="agent")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="second", agent_id="dev", agent_type="agent")

        regular = repo.log()
        all_seals = repo.log_all()
        assert len(regular) == len(all_seals)
        assert [s["id"] for s in regular] == [s["id"] for s in all_seals]

    def test_log_all_includes_diverged_seals(self, tmp_path):
        """Seals on diverged branches appear in log_all but not log."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        # Agent A seals on alpha.
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        # Agent B seals on beta (will be diverged after next seal).
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")

        # Agent A seals on alpha again — creates divergence.
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        regular = repo.log()
        all_seals = repo.log_all()

        # Regular log has 2 seals (alpha chain), log_all has 3 (includes beta).
        assert len(regular) == 2
        assert len(all_seals) == 3

        # Agent B should appear in log_all but not regular log.
        regular_agents = {s["agent"]["id"] for s in regular}
        all_agents = {s["agent"]["id"] for s in all_seals}
        assert "agent-b" not in regular_agents
        assert "agent-b" in all_agents

    def test_log_all_with_limit(self, tmp_path):
        """Limit parameter works with log_all."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="first", agent_id="dev", agent_type="agent")
        (tmp_path / "a.txt").write_text("b")
        repo.seal(summary="second", agent_id="dev", agent_type="agent")

        limited = repo.log_all(limit=1)
        assert len(limited) == 1

    def test_log_all_json_serializable(self, tmp_path):
        """log_all output survives JSON round-trip."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        all_seals = repo.log_all()
        serialized = json.dumps(all_seals)
        parsed = json.loads(serialized)
        assert len(parsed) == 1
        assert parsed[0]["summary"] == "work"


class TestDeletesExcludedFromOwnership:
    """Sprint 1: Deleting a file shouldn't make you its owner."""

    def test_deleter_does_not_own_removed_file(self, tmp_path):
        """Deleting a file should not grant ownership of that file."""
        repo = writ.Repository.init(str(tmp_path))

        # Create two files.
        (tmp_path / "keep.txt").write_text("keep")
        (tmp_path / "remove.txt").write_text("gone")
        repo.seal(summary="add files", agent_id="creator", agent_type="agent")

        # Delete one file.
        (tmp_path / "remove.txt").unlink()
        repo.seal(summary="cleanup", agent_id="deleter", agent_type="agent")

        ctx = repo.context()
        activity = {a["agent_id"]: a for a in ctx["agent_activity"]}

        # Deleter should NOT own remove.txt.
        assert "remove.txt" not in activity["deleter"]["files_owned"]
        # Creator still owns keep.txt.
        assert "keep.txt" in activity["creator"]["files_owned"]

    def test_delete_then_recreate_ownership(self, tmp_path):
        """If file is deleted then recreated, the re-creator owns it."""
        repo = writ.Repository.init(str(tmp_path))

        (tmp_path / "file.txt").write_text("v1")
        repo.seal(summary="create", agent_id="author", agent_type="agent")

        (tmp_path / "file.txt").unlink()
        repo.seal(summary="delete", agent_id="cleaner", agent_type="agent")

        (tmp_path / "file.txt").write_text("v2")
        repo.seal(summary="recreate", agent_id="rebuilder", agent_type="agent")

        ctx = repo.context()
        activity = {a["agent_id"]: a for a in ctx["agent_activity"]}

        # Rebuilder owns the file now (most recent non-delete seal).
        assert "file.txt" in activity["rebuilder"]["files_owned"]


class TestDivergedBranchDetection:
    """Sprint 1: Context surfaces diverged spec branches (ghost agent detection)."""

    def test_no_diverged_branches_when_linear(self, tmp_path):
        """Linear chain has no diverged branches."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")

        (tmp_path / "a.txt").write_text("v1")
        repo.seal(summary="first", agent_id="dev", agent_type="agent", spec_id="alpha")
        (tmp_path / "a.txt").write_text("v2")
        repo.seal(summary="second", agent_id="dev", agent_type="agent", spec_id="alpha")

        ctx = repo.context()
        assert ctx.get("diverged_branches", []) == []

    def test_detects_diverged_branch(self, tmp_path):
        """Diverged spec branch is surfaced in context."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        # Agent A seals on alpha.
        (tmp_path / "a.txt").write_text("a1")
        repo.seal(summary="alpha first", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        # Agent B seals on beta — parent from HEAD.
        (tmp_path / "b.txt").write_text("b1")
        repo.seal(summary="beta work", agent_id="agent-b", agent_type="agent", spec_id="beta")

        # Agent A seals on alpha again — parent from heads/alpha, not global HEAD.
        # This makes beta's seal orphaned from the HEAD chain.
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha second", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context()
        diverged = ctx.get("diverged_branches", [])
        assert len(diverged) == 1
        assert diverged[0]["spec_id"] == "beta"
        assert diverged[0]["seal_count"] == 1
        assert "agent-b" in diverged[0]["agents"]

    def test_diverged_branch_has_recommendation(self, tmp_path):
        """Each diverged branch warning includes a recommendation to converge."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="main-spec", title="Main")
        repo.add_spec(id="feature", title="Feature")

        (tmp_path / "x.txt").write_text("x")
        repo.seal(summary="main", agent_id="main-dev", agent_type="agent", spec_id="main-spec")

        (tmp_path / "y.txt").write_text("y")
        repo.seal(summary="feature", agent_id="feat-dev", agent_type="agent", spec_id="feature")

        (tmp_path / "x.txt").write_text("x2")
        repo.seal(summary="main done", agent_id="main-dev", agent_type="agent", spec_id="main-spec")

        ctx = repo.context()
        diverged = ctx["diverged_branches"]
        assert len(diverged) >= 1
        feat_branch = [d for d in diverged if d["spec_id"] == "feature"][0]
        assert "converge" in feat_branch["recommendation"].lower()

    def test_ghost_agent_visible_in_activity(self, tmp_path):
        """Agent on diverged branch still appears in agent_activity."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")

        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context()
        agent_ids = [a["agent_id"] for a in ctx["agent_activity"]]
        assert "agent-b" in agent_ids, "ghost agent should be visible in agent_activity"

    def test_diverged_branches_empty_in_spec_scoped(self, tmp_path):
        """Spec-scoped context omits diverged_branches."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context(spec="alpha")
        assert ctx.get("diverged_branches", []) == []

    def test_diverged_branch_json_serializable(self, tmp_path):
        """Context with diverged branches survives JSON round-trip."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context()
        serialized = json.dumps(ctx)
        parsed = json.loads(serialized)
        assert len(parsed["diverged_branches"]) == 1
        assert parsed["diverged_branches"][0]["spec_id"] == "beta"


class TestConvergenceNudge:
    """Sprint 2: Context includes convergence_recommended flag when branches diverge."""

    def test_convergence_not_recommended_when_linear(self, tmp_path):
        """No convergence needed for a clean linear chain."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        ctx = repo.context()
        # Field should be absent (skipped when false) or explicitly False.
        assert ctx.get("convergence_recommended", False) is False

    def test_convergence_recommended_when_diverged(self, tmp_path):
        """convergence_recommended is True when diverged branches exist."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context()
        assert ctx["convergence_recommended"] is True

    def test_convergence_not_recommended_in_spec_scoped(self, tmp_path):
        """Spec-scoped context doesn't include convergence nudge."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context(spec="alpha")
        assert ctx.get("convergence_recommended", False) is False

    def test_convergence_omitted_from_json_when_false(self, tmp_path):
        """Token-efficient: convergence_recommended is omitted from JSON when false."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        ctx = repo.context()
        serialized = json.dumps(ctx)
        assert "convergence_recommended" not in serialized


class TestFileScopeWarning:
    def test_no_warning_when_no_scope(self, tmp_path):
        """No warning when spec has no file_scope set."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        (tmp_path / "anything.py").write_text("hello")
        result = repo.seal(
            summary="work", agent_id="a1", agent_type="agent", spec_id="feat"
        )
        assert result.get("file_scope_warning") is None

    def test_no_warning_when_in_scope(self, tmp_path):
        """No warning when all files are within scope."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        repo.update_spec("feat", file_scope=["src/"])
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "main.py").write_text("hello")
        result = repo.seal(
            summary="work", agent_id="a1", agent_type="agent", spec_id="feat"
        )
        assert result.get("file_scope_warning") is None

    def test_warning_when_out_of_scope(self, tmp_path):
        """Warning returned when files are outside declared scope."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        repo.update_spec("feat", file_scope=["src/"])
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "main.py").write_text("in scope")
        (tmp_path / "README.md").write_text("out of scope")
        result = repo.seal(
            summary="work", agent_id="a1", agent_type="agent", spec_id="feat"
        )
        w = result.get("file_scope_warning")
        assert w is not None
        assert "README.md" in w["out_of_scope_files"]
        assert "src/main.py" in w["in_scope_files"]
        assert w["spec_id"] == "feat"
        assert "src/" in w["declared_scope"]

    def test_warning_with_glob_scope(self, tmp_path):
        """Glob patterns in file_scope work correctly."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        repo.update_spec("feat", file_scope=["*.py"])
        (tmp_path / "main.py").write_text("python")
        (tmp_path / "styles.css").write_text("css")
        result = repo.seal(
            summary="work", agent_id="a1", agent_type="agent", spec_id="feat"
        )
        w = result.get("file_scope_warning")
        assert w is not None
        assert "styles.css" in w["out_of_scope_files"]
        assert "main.py" in w["in_scope_files"]

    def test_no_warning_without_spec(self, tmp_path):
        """No file scope check when sealing without a spec."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "file.py").write_text("hello")
        result = repo.seal(summary="work", agent_id="a1", agent_type="agent")
        assert result.get("file_scope_warning") is None


def _init_git_repo(path):
    """Helper: create a git repo with an initial commit."""
    subprocess.run(["git", "init", str(path)], check=True, capture_output=True)
    subprocess.run(["git", "-C", str(path), "config", "user.email", "test@test.com"],
                   check=True, capture_output=True)
    subprocess.run(["git", "-C", str(path), "config", "user.name", "Test"],
                   check=True, capture_output=True)


def _git_commit(path, message="commit"):
    """Helper: stage all and commit."""
    subprocess.run(["git", "-C", str(path), "add", "-A"],
                   check=True, capture_output=True)
    subprocess.run(["git", "-C", str(path), "commit", "-m", message],
                   check=True, capture_output=True)


class TestBridgeImportAttribution:
    """Sprint 3: Bridge import should not pollute first agent's seal."""

    def test_first_seal_after_bridge_only_has_agent_changes(self, tmp_path):
        """First seal after bridge_import should only contain the agent's actual changes."""
        _init_git_repo(tmp_path)
        (tmp_path / "readme.md").write_text("# Hello")
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "main.py").write_text("print('hello')")
        _git_commit(tmp_path, "initial")

        repo = writ.Repository.init(str(tmp_path))
        result = repo.bridge_import(agent_id="bridge", agent_type="agent")
        assert result["files_imported"] == 2  # readme.md + src/main.py

        # Agent creates 1 new file.
        (tmp_path / "agent.txt").write_text("my work")
        seal = repo.seal(summary="agent work", agent_id="dev", agent_type="agent")
        changes = seal["changes"]

        # Only the agent's file should appear, not the bridge-imported files.
        paths = [c["path"] for c in changes]
        assert paths == ["agent.txt"], f"expected only agent.txt; got {paths}"

    def test_dirty_tree_not_attributed_to_first_agent(self, tmp_path):
        """Uncommitted files at bridge_import time shouldn't appear in first seal."""
        _init_git_repo(tmp_path)
        (tmp_path / "committed.txt").write_text("in git")
        _git_commit(tmp_path, "initial")

        # Dirty file exists before bridge_import.
        (tmp_path / "dirty.txt").write_text("not committed")

        repo = writ.Repository.init(str(tmp_path))
        repo.bridge_import(agent_id="bridge", agent_type="agent")

        # Agent creates their own file.
        (tmp_path / "agent.txt").write_text("agent work")
        seal = repo.seal(summary="work", agent_id="dev", agent_type="agent")
        paths = [c["path"] for c in seal["changes"]]

        assert "dirty.txt" not in paths, "dirty file should not pollute agent's seal"
        assert "agent.txt" in paths

    def test_bridge_seal_still_records_git_snapshot(self, tmp_path):
        """Bridge seal should record the git tree for historical accuracy."""
        _init_git_repo(tmp_path)
        (tmp_path / "file.txt").write_text("content")
        _git_commit(tmp_path, "initial")

        repo = writ.Repository.init(str(tmp_path))
        result = repo.bridge_import(agent_id="bridge", agent_type="agent")

        # The import result should show the git file was captured.
        assert result["files_imported"] == 1
        assert len(result["seal_id"]) > 0


class TestSpecScopedDivergedSeals:
    """Sprint 3: Spec-scoped context shows seals from diverged branches."""

    def test_diverged_spec_seals_visible_in_context(self, tmp_path):
        """Seals on a diverged spec branch should appear in spec-scoped context."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        # Agent A seals on alpha.
        (tmp_path / "a.txt").write_text("a1")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        # Agent B seals twice on beta.
        (tmp_path / "b.txt").write_text("b1")
        repo.seal(summary="beta 1", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "b.txt").write_text("b2")
        repo.seal(summary="beta 2", agent_id="agent-b", agent_type="agent", spec_id="beta")

        # Agent A seals on alpha again — diverges beta.
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        # Spec-scoped context for beta should show its seals.
        ctx = repo.context(spec="beta")
        assert len(ctx["recent_seals"]) == 2, (
            f"expected 2 beta seals; got {len(ctx['recent_seals'])}"
        )
        assert all(s["agent"] == "agent-b" for s in ctx["recent_seals"])

    def test_diverged_spec_still_shows_progress(self, tmp_path):
        """spec_progress should be populated for diverged specs."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context(spec="beta")
        assert ctx.get("spec_progress") is not None
        assert ctx["spec_progress"]["total_seals"] == 1
        assert "agent-b" in ctx["spec_progress"]["agents_involved"]

    def test_non_diverged_spec_still_works(self, tmp_path):
        """Spec-scoped context still works for non-diverged specs."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "f.txt").write_text("work")
        repo.seal(summary="feature", agent_id="dev", agent_type="agent", spec_id="feat")

        ctx = repo.context(spec="feat")
        assert len(ctx["recent_seals"]) == 1
        assert ctx["recent_seals"][0]["agent"] == "dev"

    def test_diverged_spec_context_json_serializable(self, tmp_path):
        """Diverged spec context survives JSON round-trip."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        ctx = repo.context(spec="beta")
        serialized = json.dumps(ctx)
        parsed = json.loads(serialized)
        assert len(parsed["recent_seals"]) == 1
        assert parsed["recent_seals"][0]["agent"] == "agent-b"


class TestSummary:
    """Sprint 3: Human-facing session summary for round-trip workflow."""

    def test_summary_empty_repo(self, tmp_path):
        """Empty repo returns a summary with zero seals."""
        repo = writ.Repository.init(str(tmp_path))
        summary = repo.summary()
        assert summary["total_seals"] == 0
        assert summary["files_changed"] == []
        assert summary["agents"] == []
        assert summary["specs_summary"] == []

    def test_summary_has_all_fields(self, tmp_path):
        """Summary dict has all expected fields."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        s = repo.summary()
        assert "headline" in s
        assert "body" in s
        assert "commit_message" in s
        assert "specs_summary" in s
        assert "agents" in s
        assert "total_seals" in s
        assert "files_changed" in s
        assert "files_to_stage" in s
        assert "convergence_recommended" in s
        assert "diverged_branch_count" in s

    def test_summary_single_spec_complete(self, tmp_path):
        """Complete spec shows title in headline."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="auth", title="Add authentication")

        (tmp_path / "auth.py").write_text("class Auth: pass")
        repo.seal(summary="added auth", agent_id="dev", agent_type="agent", spec_id="auth")
        repo.update_spec("auth", status="complete")

        s = repo.summary()
        assert "Add authentication" in s["headline"]
        assert s["total_seals"] == 1
        assert len(s["specs_summary"]) == 1
        assert s["specs_summary"][0]["status"] == "complete"

    def test_summary_multi_agent(self, tmp_path):
        """Multiple agents appear in summary."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="agent-1 work", agent_id="agent-1", agent_type="agent", spec_id="feat")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="agent-2 work", agent_id="agent-2", agent_type="agent", spec_id="feat")

        s = repo.summary()
        assert s["total_seals"] == 2
        agent_ids = [a["id"] for a in s["agents"]]
        assert "agent-1" in agent_ids
        assert "agent-2" in agent_ids

    def test_summary_commit_message(self, tmp_path):
        """commit_message combines headline and body."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "f.txt").write_text("content")
        repo.seal(summary="did stuff", agent_id="dev", agent_type="agent")

        s = repo.summary()
        assert s["headline"] in s["commit_message"]
        assert "Files changed:" in s["body"]
        assert "Total seals:" in s["body"]

    def test_summary_files_to_stage(self, tmp_path):
        """Working tree changes appear in files_to_stage."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        # Modify after seal.
        (tmp_path / "a.txt").write_text("modified")

        s = repo.summary()
        assert "a.txt" in s["files_to_stage"]

    def test_summary_excludes_bridge_seals(self, tmp_path):
        """Bridge import seals are excluded from summary counts."""
        repo = writ.Repository.init(str(tmp_path))

        # Simulate bridge seal.
        (tmp_path / "imported.txt").write_text("from git")
        repo.seal(summary="bridge import", agent_id="writ-bridge", agent_type="agent")

        # Real work.
        (tmp_path / "new.txt").write_text("agent work")
        repo.seal(summary="actual work", agent_id="dev", agent_type="agent")

        s = repo.summary()
        assert s["total_seals"] == 1
        assert len(s["agents"]) == 1
        assert s["agents"][0]["id"] == "dev"

    def test_summary_json_serializable(self, tmp_path):
        """Summary survives JSON round-trip."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "f.txt").write_text("content")
        repo.seal(summary="work", agent_id="dev", agent_type="agent")

        s = repo.summary()
        serialized = json.dumps(s)
        parsed = json.loads(serialized)
        assert parsed["total_seals"] == 1
        assert parsed["headline"] == s["headline"]

    def test_summary_convergence_warning(self, tmp_path):
        """Diverged branches show convergence warning in summary."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="alpha", title="Alpha")
        repo.add_spec(id="beta", title="Beta")

        (tmp_path / "a.txt").write_text("a")
        repo.seal(summary="alpha", agent_id="agent-a", agent_type="agent", spec_id="alpha")
        (tmp_path / "b.txt").write_text("b")
        repo.seal(summary="beta", agent_id="agent-b", agent_type="agent", spec_id="beta")
        (tmp_path / "a.txt").write_text("a2")
        repo.seal(summary="alpha 2", agent_id="agent-a", agent_type="agent", spec_id="alpha")

        s = repo.summary()
        assert s["convergence_recommended"] is True
        assert s["diverged_branch_count"] > 0
        assert "diverged" in s["body"]


class TestFileContention:
    """Tests for the file contention map in context output."""

    def test_no_contention_single_agent(self, tmp_path):
        """Single agent → no contention."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        (tmp_path / "a.txt").write_text("content")
        repo.seal(summary="work", agent_id="alice", agent_type="agent", spec_id="feat")

        ctx = repo.context()
        assert ctx.get("file_contention", []) == []

    def test_contention_two_agents_same_file(self, tmp_path):
        """Two agents touching the same file → contention entry."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "shared.txt").write_text("alice v1")
        repo.seal(summary="alice", agent_id="alice", agent_type="agent", spec_id="feat")

        (tmp_path / "shared.txt").write_text("bob v1")
        repo.seal(summary="bob", agent_id="bob", agent_type="agent", spec_id="feat")

        ctx = repo.context()
        contention = ctx.get("file_contention", [])
        assert len(contention) == 1
        assert contention[0]["path"] == "shared.txt"
        assert len(contention[0]["agents"]) == 2
        assert "alice" in contention[0]["agents"]
        assert "bob" in contention[0]["agents"]

    def test_contention_excludes_bridge(self, tmp_path):
        """Bridge seals don't count for contention."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "a.txt").write_text("bridge")
        repo.seal(summary="import", agent_id="writ-bridge", agent_type="agent")

        (tmp_path / "a.txt").write_text("alice")
        repo.seal(summary="work", agent_id="alice", agent_type="agent", spec_id="feat")

        ctx = repo.context()
        assert ctx.get("file_contention", []) == []

    def test_contention_sorted_by_agent_count(self, tmp_path):
        """Files with more agents appear first."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        # hot.txt: 3 agents, warm.txt: 2 agents
        (tmp_path / "hot.txt").write_text("a")
        (tmp_path / "warm.txt").write_text("a")
        repo.seal(summary="a", agent_id="alice", agent_type="agent", spec_id="feat")

        (tmp_path / "hot.txt").write_text("b")
        (tmp_path / "warm.txt").write_text("b")
        repo.seal(summary="b", agent_id="bob", agent_type="agent", spec_id="feat")

        (tmp_path / "hot.txt").write_text("c")
        repo.seal(summary="c", agent_id="charlie", agent_type="agent", spec_id="feat")

        ctx = repo.context()
        contention = ctx.get("file_contention", [])
        assert len(contention) == 2
        assert contention[0]["path"] == "hot.txt"
        assert len(contention[0]["agents"]) == 3
        assert contention[1]["path"] == "warm.txt"
        assert len(contention[1]["agents"]) == 2

    def test_contention_json_serializable(self, tmp_path):
        """Contention data survives JSON roundtrip."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "a.txt").write_text("alice")
        repo.seal(summary="a", agent_id="alice", agent_type="agent", spec_id="feat")

        (tmp_path / "a.txt").write_text("bob")
        repo.seal(summary="b", agent_id="bob", agent_type="agent", spec_id="feat")

        ctx = repo.context()
        serialized = json.dumps(ctx)
        parsed = json.loads(serialized)
        assert len(parsed.get("file_contention", [])) == 1

    def test_contention_not_in_spec_context(self, tmp_path):
        """Spec-scoped context doesn't include contention."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")

        (tmp_path / "a.txt").write_text("alice")
        repo.seal(summary="a", agent_id="alice", agent_type="agent", spec_id="feat")

        (tmp_path / "a.txt").write_text("bob")
        repo.seal(summary="b", agent_id="bob", agent_type="agent", spec_id="feat")

        ctx = repo.context(spec="feat")
        assert ctx.get("file_contention", []) == []


class TestIntegrationRisk:
    """Tests for the integration_risk field in context output."""

    def test_no_risk_clean_repo(self, tmp_path):
        """A clean repo with no divergence has no integration_risk."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("hello")
        repo.seal(summary="init", agent_id="dev", agent_type="agent")
        ctx = repo.context()
        assert ctx.get("integration_risk") is None

    def test_risk_present_when_diverged(self, tmp_path):
        """Diverged branches trigger integration_risk in context."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("base")
        repo.seal(summary="base", agent_id="setup", agent_type="agent")

        repo.add_spec(id="spec-a", title="A")
        repo.add_spec(id="spec-b", title="B")

        (tmp_path / "a.txt").write_text("left")
        repo.seal(summary="left", agent_id="dev-a", agent_type="agent", spec_id="spec-a")

        (tmp_path / "a.txt").write_text("right")
        repo.seal(summary="right", agent_id="dev-b", agent_type="agent", spec_id="spec-b")

        # Seal again on spec-a to create divergence
        (tmp_path / "a.txt").write_text("left2")
        repo.seal(summary="left2", agent_id="dev-a", agent_type="agent", spec_id="spec-a")

        ctx = repo.context()
        risk = ctx.get("integration_risk")
        assert risk is not None
        assert risk["level"] in ("low", "medium", "high")
        assert isinstance(risk["score"], int)
        assert isinstance(risk["factors"], list)
        assert len(risk["factors"]) > 0

    def test_risk_has_correct_structure(self, tmp_path):
        """integration_risk has level, score, and factors fields."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("base")
        repo.seal(summary="base", agent_id="setup", agent_type="agent")

        repo.add_spec(id="s1", title="S1")
        repo.add_spec(id="s2", title="S2")

        (tmp_path / "a.txt").write_text("v1")
        repo.seal(summary="v1", agent_id="a1", agent_type="agent", spec_id="s1")
        (tmp_path / "a.txt").write_text("v2")
        repo.seal(summary="v2", agent_id="a2", agent_type="agent", spec_id="s2")
        (tmp_path / "a.txt").write_text("v3")
        repo.seal(summary="v3", agent_id="a1", agent_type="agent", spec_id="s1")

        ctx = repo.context()
        risk = ctx.get("integration_risk")
        assert risk is not None
        serialized = json.dumps(risk)
        parsed = json.loads(serialized)
        assert "level" in parsed
        assert "score" in parsed
        assert "factors" in parsed

    def test_risk_not_in_spec_scoped_context(self, tmp_path):
        """Spec-scoped context does not include integration_risk."""
        repo = writ.Repository.init(str(tmp_path))
        repo.add_spec(id="feat", title="Feature")
        (tmp_path / "a.txt").write_text("hello")
        repo.seal(summary="init", agent_id="dev", agent_type="agent", spec_id="feat")
        ctx = repo.context(spec="feat")
        assert ctx.get("integration_risk") is None


class TestConvergence:
    """Tests for the convergence workflow via Python SDK."""

    def _setup_diverged(self, tmp_path):
        """Create a repo with two diverged spec branches.

        Returns (repo, tmp_path) with:
        - nav-update: modified index.html nav + added nav.js
        - footer-update: modified index.html footer + added footer.css
        - Branches are properly diverged (footer-update not on HEAD chain).
        """
        repo = writ.Repository.init(str(tmp_path))
        base = "<html>\n<nav>Home</nav>\n<main>Content</main>\n<footer>2026</footer>\n</html>\n"
        (tmp_path / "index.html").write_text(base)
        (tmp_path / "style.css").write_text("body{}")
        repo.seal(summary="baseline", agent_id="setup", agent_type="agent")

        repo.add_spec(id="nav-update", title="Update nav")
        repo.add_spec(id="footer-update", title="Update footer")

        # Agent A: nav changes
        nav = "<html>\n<nav>Home | Blog</nav>\n<main>Content</main>\n<footer>2026</footer>\n</html>\n"
        (tmp_path / "index.html").write_text(nav)
        repo.seal(summary="nav links", agent_id="nav-dev", agent_type="agent", spec_id="nav-update")

        # Agent B: footer changes (from baseline, not nav version)
        footer = "<html>\n<nav>Home</nav>\n<main>Content</main>\n<footer>2026 All rights reserved</footer>\n</html>\n"
        (tmp_path / "index.html").write_text(footer)
        (tmp_path / "footer.css").write_text(".footer{}")
        repo.seal(summary="footer", agent_id="footer-dev", agent_type="agent", spec_id="footer-update")

        # Agent A seals again → creates divergence (HEAD forks past B's seal)
        (tmp_path / "index.html").write_text(nav)
        (tmp_path / "nav.js").write_text("// nav")
        repo.seal(summary="nav script", agent_id="nav-dev", agent_type="agent", spec_id="nav-update")

        return repo

    def test_converge_returns_report(self, tmp_path):
        """converge() returns a structured report dict."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")

        assert isinstance(report, dict)
        assert report["left_spec"] == "nav-update"
        assert report["right_spec"] == "footer-update"
        assert "is_clean" in report
        assert "auto_merged" in report
        assert "conflicts" in report
        assert "left_only" in report
        assert "right_only" in report

    def test_converge_clean_merge(self, tmp_path):
        """Non-overlapping changes merge cleanly."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")

        assert report["is_clean"] is True
        assert len(report["conflicts"]) == 0

        # index.html should be in auto_merged with both changes
        merged = [m for m in report["auto_merged"] if m["path"] == "index.html"]
        assert len(merged) == 1
        assert "Blog" in merged[0]["content"]
        assert "All rights reserved" in merged[0]["content"]

    def test_apply_convergence_writes_to_disk(self, tmp_path):
        """apply_convergence writes merged files to the working directory."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")
        repo.apply_convergence(report)

        html = (tmp_path / "index.html").read_text()
        assert "Blog" in html
        assert "All rights reserved" in html

    def test_converge_then_seal(self, tmp_path):
        """After convergence, seal captures the merged state."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")
        repo.apply_convergence(report)

        seal = repo.seal(
            summary="Merged nav + footer",
            agent_id="orchestrator",
            agent_type="agent",
        )
        paths = [c["path"] for c in seal["changes"]]
        assert "index.html" in paths

    def test_converge_with_conflict(self, tmp_path):
        """Conflicting changes produce conflicts in the report."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("base\n")
        repo.seal(summary="base", agent_id="setup", agent_type="agent")

        repo.add_spec(id="spec-a", title="A")
        repo.add_spec(id="spec-b", title="B")

        (tmp_path / "a.txt").write_text("LEFT\n")
        repo.seal(summary="left", agent_id="a", agent_type="agent", spec_id="spec-a")

        (tmp_path / "a.txt").write_text("RIGHT\n")
        repo.seal(summary="right", agent_id="b", agent_type="agent", spec_id="spec-b")

        report = repo.converge("spec-a", "spec-b")
        assert report["is_clean"] is False
        assert len(report["conflicts"]) > 0
        assert report["conflicts"][0]["path"] == "a.txt"

    def test_apply_with_resolution(self, tmp_path):
        """Conflicts can be resolved by providing resolutions."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("base\n")
        repo.seal(summary="base", agent_id="setup", agent_type="agent")

        repo.add_spec(id="spec-a", title="A")
        repo.add_spec(id="spec-b", title="B")

        (tmp_path / "a.txt").write_text("LEFT\n")
        repo.seal(summary="left", agent_id="a", agent_type="agent", spec_id="spec-a")

        (tmp_path / "a.txt").write_text("RIGHT\n")
        repo.seal(summary="right", agent_id="b", agent_type="agent", spec_id="spec-b")

        report = repo.converge("spec-a", "spec-b")
        resolutions = [{"path": "a.txt", "content": "RESOLVED\n"}]
        repo.apply_convergence(report, resolutions)

        assert (tmp_path / "a.txt").read_text() == "RESOLVED\n"

    def test_apply_unresolved_conflict_errors(self, tmp_path):
        """Applying without resolving conflicts raises an error."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("base\n")
        repo.seal(summary="base", agent_id="setup", agent_type="agent")

        repo.add_spec(id="spec-a", title="A")
        repo.add_spec(id="spec-b", title="B")

        (tmp_path / "a.txt").write_text("LEFT\n")
        repo.seal(summary="left", agent_id="a", agent_type="agent", spec_id="spec-a")

        (tmp_path / "a.txt").write_text("RIGHT\n")
        repo.seal(summary="right", agent_id="b", agent_type="agent", spec_id="spec-b")

        report = repo.converge("spec-a", "spec-b")
        with pytest.raises(writ.WritError):
            repo.apply_convergence(report)

    def test_converge_report_json_serializable(self, tmp_path):
        """Convergence report survives JSON roundtrip."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")

        serialized = json.dumps(report)
        parsed = json.loads(serialized)
        assert parsed["left_spec"] == "nav-update"
        assert parsed["is_clean"] == report["is_clean"]

    def test_converge_left_right_only(self, tmp_path):
        """Left-only and right-only files are reported correctly."""
        repo = self._setup_diverged(tmp_path)
        report = repo.converge("nav-update", "footer-update")

        assert "nav.js" in report["left_only"]
        assert "footer.css" in report["right_only"]
