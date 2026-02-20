"""Tests for the writ SDK (Agent, Phase, Pipeline)."""

import json
import os

import pytest

import writ
from writ.sdk import Agent, Phase, Pipeline, estimate_complexity, FULL_PHASES, COMPACT_PHASES


@pytest.fixture
def repo_dir(tmp_path):
    """Create a writ repo and return its path as a string."""
    repo = writ.Repository.init(str(tmp_path))
    return str(tmp_path)


@pytest.fixture
def repo_with_file(repo_dir):
    """Repo with one tracked file."""
    filepath = os.path.join(repo_dir, "hello.txt")
    with open(filepath, "w") as f:
        f.write("hello world")
    repo = writ.Repository.open(repo_dir)
    repo.seal(
        summary="initial",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )
    return repo_dir


class TestAgent:
    def test_agent_context_manager(self, repo_with_file):
        with Agent("test-agent", path=repo_with_file) as agent:
            ctx = agent.context
            assert ctx["writ_version"] == "0.1.0"
            assert "recent_seals" in ctx

    def test_agent_open_explicit(self, repo_with_file):
        agent = Agent("test-agent", path=repo_with_file)
        agent.open()
        ctx = agent.context
        assert "working_state" in ctx

    def test_agent_repo_not_opened_raises(self, repo_dir):
        agent = Agent("test-agent", path=repo_dir)
        with pytest.raises(RuntimeError, match="not opened"):
            _ = agent.repo

    def test_agent_seal(self, repo_dir):
        path = os.path.join(repo_dir, "work.txt")
        with open(path, "w") as f:
            f.write("some work")
        with Agent("coder", path=repo_dir) as agent:
            result = agent.seal("did some work")
            assert "id" in result
            assert result["agent"]["id"] == "coder"

    def test_agent_checkpoint(self, repo_dir):
        path = os.path.join(repo_dir, "work.txt")
        with open(path, "w") as f:
            f.write("quick save")
        with Agent("coder", path=repo_dir) as agent:
            result = agent.checkpoint("quick progress save")
            assert "id" in result

    def test_agent_should_seal_true_when_dirty(self, repo_with_file):
        path = os.path.join(repo_with_file, "hello.txt")
        with open(path, "w") as f:
            f.write("modified content")
        with Agent("checker", path=repo_with_file) as agent:
            assert agent.should_seal() is True

    def test_agent_should_seal_false_when_clean(self, repo_with_file):
        with Agent("checker", path=repo_with_file) as agent:
            assert agent.should_seal() is False

    def test_agent_refresh(self, repo_with_file):
        with Agent("checker", path=repo_with_file) as agent:
            ctx1 = agent.context
            ctx2 = agent.refresh()
            assert ctx1["writ_version"] == ctx2["writ_version"]

    def test_agent_seal_with_spec(self, repo_dir):
        repo = writ.Repository.open(repo_dir)
        repo.add_spec(id="test-spec", title="Test", description="A test spec")
        path = os.path.join(repo_dir, "impl.txt")
        with open(path, "w") as f:
            f.write("implementation")
        with Agent("impl-agent", path=repo_dir, spec_id="test-spec") as agent:
            result = agent.seal("implemented feature", status="in-progress")
            assert result.get("spec_id") == "test-spec"

    def test_agent_history(self, repo_with_file):
        with Agent("viewer", path=repo_with_file) as agent:
            history = agent.history()
            assert isinstance(history, list)
            assert len(history) >= 1

    def test_agent_handoff_summary(self, repo_with_file):
        with Agent("worker", path=repo_with_file) as agent:
            summary = agent.handoff_summary()
            assert summary["agent_id"] == "worker"
            assert "working_state" in summary
            assert "recent_seals" in summary


class TestPhase:
    def test_phase_basic(self, repo_dir):
        path = os.path.join(repo_dir, "phase_file.txt")
        with open(path, "w") as f:
            f.write("phase output")
        agent = Agent("orchestrator", path=repo_dir)
        agent.open()
        with Phase(agent, "implementation", agent_id="implementer") as p:
            ctx = p.context
            assert "working_state" in ctx
            result = p.seal("built feature")
            assert "id" in result
        assert agent.agent_id == "orchestrator"

    def test_phase_restores_agent_id(self, repo_dir):
        agent = Agent("original-id", path=repo_dir)
        agent.open()
        with Phase(agent, "test-phase", agent_id="temporary-id") as p:
            assert agent.agent_id == "temporary-id"
        assert agent.agent_id == "original-id"

    def test_phase_constructor_has_no_side_effects(self, repo_dir):
        agent = Agent("original-id", path=repo_dir)
        agent.open()
        _ = Phase(agent, "test-phase", agent_id="temporary-id")
        assert agent.agent_id == "original-id"

    def test_phase_complete_spec(self, repo_dir):
        repo = writ.Repository.open(repo_dir)
        repo.add_spec(id="finish-me", title="Finish", description="needs completion")
        agent = Agent("orchestrator", path=repo_dir, spec_id="finish-me")
        agent.open()
        with Phase(agent, "finalize", agent_id="integrator") as p:
            result = p.complete_spec()
            assert "id" in result

    def test_phase_complete_spec_no_spec_raises(self, repo_dir):
        agent = Agent("orchestrator", path=repo_dir)
        agent.open()
        with Phase(agent, "finalize") as p:
            with pytest.raises(ValueError, match="no spec_id"):
                p.complete_spec()


class TestPipeline:
    def test_pipeline_basic(self, repo_dir):
        pipeline = Pipeline(
            spec_id="test-pipeline",
            spec_title="Test Pipeline",
            spec_description="Testing the pipeline runner",
        )

        @pipeline.phase("specification", agent_id="spec-writer")
        def spec_fn(ctx):
            filepath = os.path.join(repo_dir, "spec.md")
            with open(filepath, "w") as f:
                f.write("# Spec\n\nDo the thing.")
            return {"summary": "wrote spec"}

        @pipeline.phase("implementation", agent_id="implementer")
        def impl_fn(ctx):
            filepath = os.path.join(repo_dir, "impl.py")
            with open(filepath, "w") as f:
                f.write("def do_thing(): pass\n")
            return {"summary": "implemented", "tests_passed": 1}

        results = pipeline.run(path=repo_dir)

        assert results["spec_id"] == "test-pipeline"
        assert len(results["phases"]) == 3  # spec + impl + finalize
        assert results["phases"][0]["name"] == "specification"
        assert results["phases"][1]["name"] == "implementation"
        assert results["phases"][2]["name"] == "finalize"
        assert len(results["seal_history"]) >= 3

    def test_pipeline_results_are_json_serializable(self, repo_dir):
        pipeline = Pipeline(
            spec_id="json-test",
            spec_title="JSON Test",
            spec_description="Verify JSON",
        )

        @pipeline.phase("work", agent_id="worker")
        def work(ctx):
            with open(os.path.join(repo_dir, "output.txt"), "w") as f:
                f.write("done")
            return {"summary": "did work"}

        results = pipeline.run(path=repo_dir)
        serialized = json.dumps(results)
        assert "json-test" in serialized


class TestSecurityIntegration:
    """Verify security hardening surfaces through the Python bindings."""

    def test_invalid_agent_id_rejected(self, repo_dir):
        path = os.path.join(repo_dir, "test.txt")
        with open(path, "w") as f:
            f.write("data")
        repo = writ.Repository.open(repo_dir)
        with pytest.raises(writ.WritError):
            repo.seal(
                summary="test",
                agent_id="evil agent; rm -rf /",
                agent_type="agent",
                status="in-progress",
            )

    def test_valid_agent_id_accepted(self, repo_dir):
        path = os.path.join(repo_dir, "test.txt")
        with open(path, "w") as f:
            f.write("data")
        repo = writ.Repository.open(repo_dir)
        result = repo.seal(
            summary="test",
            agent_id="my-agent_v2.0",
            agent_type="agent",
            status="in-progress",
        )
        assert "id" in result

    def test_sdk_agent_rejects_invalid_id(self, repo_dir):
        path = os.path.join(repo_dir, "test.txt")
        with open(path, "w") as f:
            f.write("data")
        with Agent("bad agent!", path=repo_dir) as agent:
            with pytest.raises(writ.WritError):
                agent.seal("should fail")


class TestAdaptivePipeline:
    def test_estimate_compact_for_simple(self):
        assert estimate_complexity("Add a link to the footer") == "compact"
        assert estimate_complexity("Fix typo in README") == "compact"
        assert estimate_complexity("Update CSS colors") == "compact"

    def test_estimate_full_for_complex(self):
        assert estimate_complexity("Migrate database schema to PostgreSQL") == "full"
        assert estimate_complexity("Refactor the authentication module") == "full"
        assert estimate_complexity("Build multi-tenant infrastructure") == "full"

    def test_estimate_full_for_large_scope(self):
        scope = ["a.py", "b.py", "c.py", "d.py", "e.py", "f.py"]
        assert estimate_complexity("small change", file_scope=scope) == "full"

    def test_estimate_compact_for_small_scope(self):
        scope = ["footer.astro"]
        assert estimate_complexity("add link", file_scope=scope) == "compact"

    def test_estimate_full_for_long_description(self):
        desc = "x" * 501
        assert estimate_complexity(desc) == "full"

    def test_pipeline_auto_mode_compact(self, repo_dir):
        p = Pipeline(
            spec_id="small-feat",
            spec_title="Small Feature",
            spec_description="Add a link to the footer",
        )
        assert p.mode == "compact"
        assert p.phase_names == COMPACT_PHASES

    def test_pipeline_auto_mode_full(self, repo_dir):
        p = Pipeline(
            spec_id="big-feat",
            spec_title="Big Feature",
            spec_description="Refactor the authentication system",
        )
        assert p.mode == "full"
        assert p.phase_names == FULL_PHASES

    def test_pipeline_explicit_mode(self):
        p = Pipeline(
            spec_id="feat",
            spec_title="Feature",
            spec_description="anything",
            mode="full",
        )
        assert p.mode == "full"

    def test_pipeline_invalid_mode_raises(self):
        with pytest.raises(ValueError, match="mode must be"):
            Pipeline(
                spec_id="feat",
                spec_title="Feature",
                mode="invalid",
            )

    def test_pipeline_compact_run(self, repo_dir):
        p = Pipeline(
            spec_id="small-feat",
            spec_title="Small Feature",
            spec_description="Add a footer link",
            mode="compact",
        )

        @p.phase("spec-and-design", agent_id="planner")
        def plan(ctx):
            with open(os.path.join(repo_dir, "footer.txt"), "w") as f:
                f.write("link added")
            return {"summary": "planned and designed footer link"}

        @p.phase("implementation", agent_id="implementer")
        def impl(ctx):
            with open(os.path.join(repo_dir, "footer.txt"), "w") as f:
                f.write("link implemented")
            return {"summary": "implemented footer link"}

        @p.phase("test-and-review", agent_id="verifier")
        def verify(ctx):
            with open(os.path.join(repo_dir, "test_results.txt"), "w") as f:
                f.write("1 passed, 0 failed")
            return {"summary": "tested and reviewed", "tests_passed": 1}

        results = p.run(path=repo_dir)
        assert results["mode"] == "compact"
        phase_names = [ph["name"] for ph in results["phases"]]
        assert "spec-and-design" in phase_names
        assert "test-and-review" in phase_names
        assert "finalize" in phase_names
        assert len(results["phases"]) == 4

    def test_pipeline_sets_file_scope(self, repo_dir):
        p = Pipeline(
            spec_id="scoped-feat",
            spec_title="Scoped Feature",
            spec_description="Small change",
            file_scope=["src/components/"],
            mode="compact",
        )

        @p.phase("implementation", agent_id="impl")
        def impl(ctx):
            with open(os.path.join(repo_dir, "f.txt"), "w") as f:
                f.write("done")
            return {"summary": "implemented"}

        results = p.run(path=repo_dir)

        repo = writ.Repository.open(repo_dir)
        spec = repo.get_spec("scoped-feat")
        assert spec["file_scope"] == ["src/components/"]
