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
