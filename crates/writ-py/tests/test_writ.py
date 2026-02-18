"""Tests for the writ Python bindings."""

import json
import os

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


class TestConvergence:
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
