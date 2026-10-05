"""S.1 seal isolation through the Python binding: spec_done scope, claim
enforcement, and the SHARED case.

The Rust isolation group (crates/writ-cli/tests/isolation.rs) covers these
through the CLI; this file pins the same contract on ``writ.Repository`` so a
binding that drops a warning or ignores the config is caught.
"""

import json
import subprocess
from pathlib import Path

import pytest

import writ

STRICT_CLAIMS = '[security]\nclaim_enforcement = "strict"\n'


def _changed(seal: dict) -> list[str]:
    return sorted(c["path"] for c in seal["changes"])


def _spec_paths(repo: writ.Repository, spec_id: str) -> set[str]:
    """Every path any seal on ``spec_id`` captured (all branches)."""
    return {
        c["path"]
        for seal in repo.log_all()
        if seal.get("spec_id") == spec_id
        for c in seal["changes"]
    }


def _seal(repo: writ.Repository, agent: str, spec: str, **kw) -> dict:
    return repo.seal(
        summary=f"{agent} work",
        agent_id=agent,
        agent_type="agent",
        spec_id=spec,
        **kw,
    )


@pytest.fixture
def two_agents(tmp_path: Path):
    """sa held by a owning a.txt, sb held by b owning b.txt; both modified."""
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="A")
    repo.add_spec(id="sb", title="B")
    repo.spec_claim("sa", "a")
    repo.spec_claim("sb", "b")
    (tmp_path / "a.txt").write_text("a1\n")
    (tmp_path / "b.txt").write_text("b1\n")
    _seal(repo, "a", "sa", paths=["a.txt"])
    _seal(repo, "b", "sb", paths=["b.txt"])
    (tmp_path / "a.txt").write_text("a2\n")
    (tmp_path / "b.txt").write_text("b2\n")
    return repo, tmp_path


# --- spec_done -------------------------------------------------------------


def test_spec_done_final_seal_takes_own_file_never_another_agents(two_agents):
    repo, path = two_agents
    sealed_a = (path / "a.txt").read_text()

    repo.spec_done("sa", summary="done", agent_id="a")

    sa_seals = [s for s in repo.log_all() if s.get("spec_id") == "sa"]
    final = max(sa_seals, key=lambda s: s["timestamp"])
    assert _changed(final) == ["a.txt"], "final seal missed a's own pending edit"
    assert sealed_a == "a2\n"
    assert "b.txt" not in _spec_paths(repo, "sa")
    assert repo.get_spec("sa")["status"] == "complete"
    later = _seal(repo, "b", "sb")
    assert _changed(later) == ["b.txt"], "b's pending file was swept or lost"


def test_spec_done_never_sweeps_another_agents_pending_file(two_agents):
    repo, _ = two_agents

    repo.spec_done("sa", summary="done", agent_id="a")

    assert "b.txt" not in _spec_paths(repo, "sa")
    later = _seal(repo, "b", "sb", paths=["b.txt"])
    assert _changed(later) == ["b.txt"], "b's pending edit was lost"


def test_spec_done_with_nothing_pending_closes_without_a_seal(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="solo", title="Solo")
    (tmp_path / "a.txt").write_text("a\n")
    _seal(repo, "solo-agent", "solo")
    before = len(repo.log_all())

    repo.spec_done("solo", summary="done", agent_id="solo-agent")

    assert len(repo.log_all()) == before, "spec_done created an empty seal"
    assert repo.get_spec("solo")["status"] == "complete"


def test_spec_done_accepts_paths(two_agents):
    repo, path = two_agents
    (path / "extra.txt").write_text("a made this\n")

    repo.spec_done("sa", summary="done", agent_id="a", paths=["a.txt", "extra.txt"])

    assert {"a.txt", "extra.txt"} <= _spec_paths(repo, "sa")
    assert "b.txt" not in _spec_paths(repo, "sa")


def test_spec_done_reports_final_seal_and_paste_hint(two_agents):
    repo, path = two_agents
    (path / "loose.txt").write_text("nobody owns this\n")

    done = repo.spec_done("sa", summary="done", agent_id="a")

    assert done["status"] == "complete"
    assert _changed(done["final_seal"]) == ["a.txt"]
    assert any(h.startswith("LEFT_OUT: loose.txt") for h in done["hints"]), done["hints"]
    retry = [h for h in done["hints"] if "writ spec done sa" in h]
    assert retry and "--agent a" in retry[0], done["hints"]


def test_spec_done_nothing_in_scope_closes_without_seal_with_hint(tmp_path):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="sa", title="A")
    (tmp_path / "a.txt").write_text("a\n")
    _seal(repo, "a", "sa", paths=["a.txt"])
    (tmp_path / "loose.txt").write_text("unowned\n")
    before = len(repo.log_all())

    done = repo.spec_done("sa", summary="done", agent_id="a")

    assert len(repo.log_all()) == before
    assert done["final_seal"] is None
    assert done["status"] == "complete"
    assert done["hints"][0].startswith("NOT_SEALED"), done["hints"]
    assert any("--paths loose.txt" in h for h in done["hints"]), done["hints"]


def test_spec_done_no_seal_skips_final_seal(two_agents):
    repo, _ = two_agents
    before = len(repo.log_all())

    done = repo.spec_done("sa", summary="done", agent_id="a", no_seal=True)

    assert len(repo.log_all()) == before
    assert done["final_seal"] is None


# --- claims ----------------------------------------------------------------


def test_seal_onto_spec_held_by_another_agent_warns_naming_owner(two_agents):
    repo, _ = two_agents

    seal = _seal(repo, "b", "sa", paths=["b.txt"])

    claim_warnings = [w for w in seal["warnings"] if w.startswith("CLAIM")]
    assert claim_warnings, seal["warnings"]
    assert "'a'" in claim_warnings[0] and "'sa'" in claim_warnings[0]
    assert repo.get_spec("sa")["claimed_by"] == "a", "warned seal moved the claim"


def test_seal_onto_spec_held_by_another_agent_rejected_under_strict(two_agents):
    repo, path = two_agents
    (path / ".writ" / "config.toml").write_text(STRICT_CLAIMS)
    repo = writ.Repository.open(str(path))  # config is read at open
    before = len(repo.log_all())

    with pytest.raises(writ.WritError, match="sa"):
        _seal(repo, "b", "sa", paths=["b.txt"])

    assert len(repo.log_all()) == before, "rejected seal was written"
    assert _changed(_seal(repo, "b", "sb")) == ["b.txt"], "rejected seal ate b.txt"


def test_holder_seals_normally_under_strict(two_agents):
    repo, path = two_agents
    (path / ".writ" / "config.toml").write_text(STRICT_CLAIMS)
    repo = writ.Repository.open(str(path))  # config is read at open

    seal = _seal(repo, "a", "sa", paths=["a.txt"])

    assert _changed(seal) == ["a.txt"]
    assert not [w for w in seal["warnings"] if w.startswith("CLAIM")]


# --- SHARED ----------------------------------------------------------------


def test_file_owned_by_two_open_specs_is_included_with_shared_warning(two_agents):
    repo, path = two_agents
    (path / "shared.txt").write_text("v1\n")
    _seal(repo, "a", "sa", paths=["shared.txt"])
    (path / "shared.txt").write_text("v2\n")
    _seal(repo, "b", "sb", paths=["shared.txt"])
    (path / "shared.txt").write_text("v3\n")

    seal = _seal(repo, "a", "sa")

    assert _changed(seal) == ["a.txt", "shared.txt"], "b.txt swept or shared left out"
    shared = [w for w in seal["warnings"] if w.startswith("SHARED")]
    assert shared and "shared.txt" in shared[0] and "sb" in shared[0], seal["warnings"]


# --- S.3 agent identity through the binding ---------------------------------
#
# The binding resolves identity with the core resolver minus framework
# detection: explicit agent_id > WRIT_AGENT_ID > default_agent > "human".
# Each test controls every variable the resolver could read.

_IDENTITY_VARS = (
    "WRIT_AGENT_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "ANTHROPIC_SESSION_ID",
    "CLAUDECODE",
    "CODEX_SESSION",
    "CODEX_SESSION_ID",
)


@pytest.fixture
def clean_identity(monkeypatch):
    for var in _IDENTITY_VARS:
        monkeypatch.delenv(var, raising=False)
    return monkeypatch


def _with_default_agent(path: Path, agent: str) -> writ.Repository:
    settings = path / ".writ" / "settings.json"
    data = json.loads(settings.read_text()) if settings.exists() else {}
    data["default_agent"] = agent
    settings.write_text(json.dumps(data))
    return writ.Repository.open(str(path))  # settings are read at open


@pytest.mark.parametrize(
    ("explicit", "env", "default_agent", "framework", "expected"),
    [
        ("flagged", "from-env", "from-setting", "hub", "flagged"),
        (None, "from-env", "from-setting", "hub", "from-env"),
        (None, None, "from-setting", "hub", "from-setting"),
        (None, "  ", "from-setting", None, "from-setting"),
        # The binding skips framework detection: a library call inside a
        # Claude Code session is not necessarily that session's agent.
        (None, None, None, "hub", "human"),
        (None, None, None, None, "human"),
    ],
    ids=["explicit", "env", "setting", "blank-env", "framework-ignored", "human"],
)
def test_resolver_ladder_is_the_same_for_add_spec_seal_and_release(
    tmp_path, clean_identity, explicit, env, default_agent, framework, expected
):
    if env is not None:
        clean_identity.setenv("WRIT_AGENT_ID", env)
    if framework is not None:
        clean_identity.setenv("CLAUDE_CODE_SESSION_ID", framework)
    repo = writ.Repository.init(str(tmp_path))
    if default_agent:
        repo = _with_default_agent(tmp_path, default_agent)

    spec = repo.add_spec(id="s", title="S", agent_id=explicit, claim=True)
    (tmp_path / "a.txt").write_text("a\n")
    seal = repo.seal(summary="w", agent_id=explicit, spec_id="s", paths=["a.txt"])
    released = repo.spec_release("s", agent_id=explicit)

    assert spec["created_by"] == expected
    assert spec.get("claimed_by") == expected
    assert seal["agent"]["id"] == expected
    assert not [w for w in seal["warnings"] if w.startswith("CLAIM")], seal["warnings"]
    assert released == expected, "release did not resolve the holder's id"
    assert repo.get_spec("s").get("claimed_by") is None


def test_add_spec_does_not_claim_without_claim_true(tmp_path, clean_identity):
    clean_identity.setenv("WRIT_AGENT_ID", "ada")
    repo = writ.Repository.init(str(tmp_path))

    spec = repo.add_spec(id="s", title="S")

    assert spec["created_by"] == "ada"
    assert spec.get("claimed_by") is None
    assert repo.get_spec("s").get("claimed_by") is None
    repo.spec_claim("s", "bea")
    assert repo.get_spec("s")["claimed_by"] == "bea"


def test_spec_release_by_non_holder_raises_naming_holder(tmp_path, clean_identity):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S", agent_id="ada", claim=True)

    with pytest.raises(writ.WritError, match="ada"):
        repo.spec_release("s", agent_id="bea")

    assert repo.get_spec("s")["claimed_by"] == "ada"


def test_forced_release_returns_previous_holder_and_is_logged(tmp_path, clean_identity):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S", agent_id="ada", claim=True)

    previous = repo.spec_release("s", agent_id="bea", force=True)

    assert previous == "ada"
    assert repo.get_spec("s").get("claimed_by") is None
    events = (tmp_path / ".writ" / "security" / "events.jsonl").read_text()
    forced = [json.loads(e) for e in events.splitlines() if "claim_force_released" in e]
    assert forced and forced[0]["agent_id"] == "bea", events
    assert "'ada'" in forced[0]["details"]


def test_release_of_unclaimed_spec_returns_none(tmp_path, clean_identity):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S")
    assert repo.spec_release("s", agent_id="bea") is None


def test_finish_never_cancels_zero_seal_specs(tmp_path, clean_identity):
    """Finding 46 through the binding: finish only commits."""

    def git(*args):
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q")
    git("config", "user.name", "writ-test")
    git("config", "user.email", "writ-test@localhost")
    (tmp_path / ".gitignore").write_text(".writ/\n")
    (tmp_path / "README.md").write_text("base\n")
    git("add", "-A")
    git("commit", "-q", "-m", "base")
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="ahead", title="Planned", agent_id="cc")
    repo.add_spec(id="held", title="Held", agent_id="bea", claim=True)
    repo.add_spec(id="work", title="Work", agent_id="ada", claim=True)
    (tmp_path / "w.txt").write_text("w\n")
    _seal(repo, "ada", "work", paths=["w.txt"])
    repo.spec_done("work", summary="done", agent_id="ada")

    result = repo.finish()

    assert result["commits"], result
    for spec_id in ("ahead", "held"):
        spec = repo.get_spec(spec_id)
        assert spec["status"] == "pending", spec
        # lifecycle_state is omitted while Active (serde default)
        assert spec.get("lifecycle_state", "active") == "active", spec


def test_spec_done_auto_scope_honours_writ_agent_id(tmp_path, clean_identity):
    clean_identity.setenv("WRIT_AGENT_ID", "ada")
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S", claim=True)

    repo.spec_done(summary="done")

    assert repo.get_spec("s")["status"] == "complete"


def test_non_holder_spec_done_seals_under_caller_with_claim_warning(
    tmp_path, clean_identity
):
    repo = writ.Repository.init(str(tmp_path))
    repo.add_spec(id="s", title="S", agent_id="ada", claim=True)
    (tmp_path / "a.txt").write_text("a1\n")
    _seal(repo, "ada", "s", paths=["a.txt"])
    (tmp_path / "a.txt").write_text("a2\n")

    repo.spec_done("s", summary="closing", agent_id="bea")

    final = max(
        (s for s in repo.log_all() if s.get("spec_id") == "s"),
        key=lambda s: s["timestamp"],
    )
    assert final["agent"]["id"] == "bea"
    assert any(
        w.startswith("CLAIM") and "'ada'" in w and "'bea'" in w for w in final["warnings"]
    ), final["warnings"]
