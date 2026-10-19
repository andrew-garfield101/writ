"""Tests for the stranger kit's scoring (the collector is itself under test,
finding 93). Transcript analysis is checked against the saved 0.4.1 live
run 1, whose findings and replies are quoted in the sprint 4 doc; the
records hurdle count against a synthetic log; score() end to end on a
synthetic OUT directory.

Run: python3 -m pytest tools/stranger_kit -q
The saved-run tests read STRANGER_EVIDENCE (default: a TEAM_WRIT_DOCS
checkout next to this repo) and skip without it. The end-to-end smoke test
runs setup, collect and score on a scratch repo for every binary in
STRANGER_BINS (colon-separated; default: target/release/writ, then the first
writ on PATH); it launches no agent.
"""
from __future__ import annotations

import json
import shutil
import sys
from pathlib import Path

import pytest

KIT = Path(__file__).resolve().parent
sys.path.insert(0, str(KIT))
import stranger  # noqa: E402

import os  # noqa: E402
import subprocess  # noqa: E402

REPO = KIT.parent.parent
EVIDENCE = Path(os.environ.get("STRANGER_EVIDENCE",
                               REPO.parent / "TEAM_WRIT_DOCS" / "evidence"))
RUN1 = EVIDENCE / "0.4.1-live-run1"
STALE_TEXT = ("[yellow] stale_claim: spec 'ghost-cleanup' is claimed by 'claude-code-bc85', "
              "but pid 76551 on host 'weatherbox' has exited | fix: writ spec release "
              "ghost-cleanup --stale")

needs_run1 = pytest.mark.skipif(not RUN1.exists(), reason="0.4.1 run 1 evidence not present")


@needs_run1
def test_writ_ids_come_from_writ_output_not_driver_names():
    assert stranger.analyze_agent(RUN1 / "auth-agent.jsonl")["writ_ids"] == ["claude-code-af15"]
    assert stranger.analyze_agent(RUN1 / "crud-agent.jsonl")["writ_ids"] == ["claude-code-3d9d"]


@needs_run1
def test_exact_finding_text_seen_and_reply_and_what_it_told_the_user():
    crud = stranger.analyze_agent(RUN1 / "crud-agent.jsonl")
    seen = {f["text"]: f for f in crud["findings_seen"]}
    assert STALE_TEXT in seen, list(seen)
    f = seen[STALE_TEXT]
    assert f["times"] == 2 and "writ" in f["seen_in"]
    assert f["reply"].startswith("Done. The Task CRUD API is implemented")
    assert any("I left it alone since it is not mine" in m for m in f["final_mentions"])


@needs_run1
def test_toon_block_is_one_unit_with_its_fields():
    auth = stranger.analyze_agent(RUN1 / "auth-agent.jsonl")
    toon = [f["text"] for f in auth["findings_seen"] if f["text"].startswith("- check: stale_claim")]
    assert len(toon) == 1
    assert "fix_command: \"writ spec release ghost-cleanup --stale\"" in toon[0]
    assert "needs_human: false" in toon[0]
    assert "paths[" not in toon[0]


@needs_run1
def test_run1_had_no_force_no_stale_release_no_git_fallback():
    for a in ("auth-agent", "crud-agent"):
        i = stranger.analyze_agent(RUN1 / f"{a}.jsonl")
        assert (i["forced"], i["stale_releases"], i["git_fallbacks"]) == ([], [], [])
        assert i["result"] == "success"


def test_finding_units_human_form_with_fix_line():
    text = "doctor: x\n  [red] store_integrity: blob missing\n    fix: writ repair\n  [info] survival: a.txt was sealed\n"
    assert stranger.finding_units(text) == [
        "[red] store_integrity: blob missing | fix: writ repair",
        "[info] survival: a.txt was sealed",
    ]


def test_refusal_line_kinds():
    assert stranger.refusal_line("error: spec 'auth' is already committed to git in abc")[0] == "committed spec"
    assert stranger.refusal_line("error: seal refused: 1 line(s) ...")[0] == "seal refused"
    assert stranger.refusal_line("all good") is None


def test_records_hurdles_count_only_writ_records_for_the_given_specs():
    seals = [{"spec_id": "a", "forced": True}, {"spec_id": "a", "allow_removals": ["x"]},
             {"spec_id": "a"}, {"spec_id": "b", "forced": True}]
    events = "\n".join(json.dumps(e) for e in [
        {"event_type": "seal_refused", "specs": ["a"]},
        {"event_type": "finish_refused", "specs": ["a", "b"]},
        {"event_type": "seal_refused", "specs": ["b"]},
        {"event_type": "claim_force_released", "specs": ["a"]}])
    h = stranger.records_hurdles(seals, events, ["a"])
    assert (h["seals"], h["forced"], h["allow_removals"], h["seal_refused"], h["finish_refused"]) == (3, 1, 1, 1, 1)
    assert h["hurdles"] == 4 and h["per_10_seals"] == 13.3


@needs_run1
def test_score_end_to_end_on_a_synthetic_out(tmp_path):
    out = tmp_path / "out"
    out.mkdir()
    shutil.copy(RUN1 / "auth-agent.jsonl", out / "agent-1.jsonl")
    shutil.copy(RUN1 / "crud-agent.jsonl", out / "agent-2.jsonl")
    specs = {"auth": {"claimed_by": None, "created_by": "claude-code-af15", "status": "complete"},
             "crud": {"claimed_by": None, "created_by": "claude-code-3d9d", "status": "complete"},
             "ghost-cleanup": {"claimed_by": "claude-code-bc85", "status": "pending"}}
    (out / "specs.json").write_text(json.dumps(specs))
    (out / "seals.json").write_text(json.dumps([
        {"spec_id": "auth", "agent": {"id": "claude-code-af15"}},
        {"spec_id": "crud", "agent": {"id": "claude-code-3d9d"}}]))
    (out / "events.jsonl").write_text("")
    (out / "audit.txt").write_text("added lines checked: 10; superseded by a later seal: 0; LOST: 0\n")
    ghost = {"check": "stale_claim", "severity": "yellow",
             "message": "spec 'ghost-cleanup' is claimed by 'claude-code-bc85', but pid 1 has exited"}
    stranger.save_kit(out, {
        "writ_version": "writ 0.4.1", "inject": "stale-claim", "finish_rc": 0,
        "injection": {"spec": "ghost-cleanup"},
        "prompts": {"agent-1": "auth", "agent-2": "crud"},
        "doctor_after": {"fast": {"headline": "h", "findings": [ghost]},
                         "survival": {"headline": "s", "findings": []}}})
    s = stranger.score(out)
    assert s["agents"]["agent-1"]["specs"] == ["auth"]
    assert s["agents"]["agent-2"]["specs"] == ["crud"]
    # The ghost claim is not an agent's own: strict 0/2, lenient 2/2 (run 1's result).
    assert (s["zero_touch_strict"], s["zero_touch_lenient"]) == ("0/2", "2/2")
    assert s["recovery"]["released_by"] == [] and not s["recovery"]["ghost_released"]
    md = (out / "report.md").read_text()
    assert STALE_TEXT[:60] in md and "Told its user" in md
    assert json.loads((out / "summary.json").read_text())["lost_lines"] == 0


def test_provenance_release_install_vs_dev_build(tmp_path):
    rel = tmp_path / "bin" / "writ"
    rel.parent.mkdir()
    rel.write_bytes(b"binary")
    p = stranger.build_provenance(str(rel), None)
    assert p["kind"].startswith("unknown build") and len(p["sha256"]) == 16  # finding 172
    dev = stranger.build_provenance(str(rel), "ea59f117fd8c")
    assert dev == {**p, "kind": "dev build", "built_from": "ea59f117fd8c"}
    real = shutil.which("writ")
    if real and stranger.build_provenance(real, None)["sha256"][:12] in stranger.KNOWN_RELEASES:
        assert stranger.build_provenance(real, None)["kind"] == "release install"


@needs_run1
def test_report_header_and_evidence_lines_are_not_truncated(tmp_path):
    out = tmp_path / "out"
    out.mkdir()
    shutil.copy(RUN1 / "auth-agent.jsonl", out / "agent-1.jsonl")
    shutil.copy(RUN1 / "crud-agent.jsonl", out / "agent-2.jsonl")
    for name, body in (("specs.json", "{}"), ("seals.json", "[]"), ("events.jsonl", ""),
                       ("audit.txt", "LOST: 0\n")):
        (out / name).write_text(body)
    stranger.save_kit(out, {"writ_version": "writ 0.4.1", "finish_rc": 0,
                            "writ_build": {"path": "/x/target/release/writ", "sha256": "ab",
                                           "kind": "dev build", "built_from": "ea59f117fd8c"}})
    stranger.score(out)
    md = (out / "report.md").read_text()
    assert "Binary: `/x/target/release/writ` (sha256 ab), dev build, built from ea59f117fd8c." in md
    full = stranger.analyze_agent(RUN1 / "auth-agent.jsonl")["final_report_lines"]
    long_lines = [l for l in full if len(l) > 300]
    assert long_lines, "fixture has no line over 300 chars"
    for l in long_lines:
        assert f"> {l}" in md


# ---------------------------------------------------------------------------
# 0.4.3: auto seal metrics (synthetic stream-json, no saved run needed)
# ---------------------------------------------------------------------------

def _ev(kind, content, at, parent=None):
    e = {"type": kind, "timestamp": at, "message": {"content": content}}
    if parent:
        e["parent_tool_use_id"] = parent
    return e


def _transcript(tmp_path, evs):
    t = tmp_path / "agent-1.jsonl"
    t.write_text("\n".join(json.dumps(e) for e in evs) + "\n")
    return t


CTX = ("writ refused the auto seal of src/x.py: it removes 3 sealed lines of spec s-b by "
       "agent bravo; fix: writ seal --paths src/x.py --allow-removals src/x.py")


def test_edits_main_and_subagent_with_notebook_path(tmp_path):
    t = _transcript(tmp_path, [
        _ev("assistant", [{"type": "tool_use", "id": "1", "name": "Edit",
                           "input": {"file_path": "/r/src/a.py"}}], "2026-10-14T10:00:00Z"),
        _ev("assistant", [{"type": "tool_use", "id": "2", "name": "NotebookEdit",
                           "input": {"notebook_path": "/r/nb.ipynb"}}], "2026-10-14T10:00:01Z",
            parent="toolu_agent"),
        _ev("assistant", [{"type": "tool_use", "id": "3", "name": "Bash",
                           "input": {"command": "ls"}}], "2026-10-14T10:00:02Z")])
    e = stranger.analyze_agent(t)["edits"]
    assert [(x["tool"], x["path"], x["subagent"]) for x in e] == [
        ("Edit", "/r/src/a.py", False), ("NotebookEdit", "/r/nb.ipynb", True)]


def test_hook_context_line_seen_once_with_the_reply_after_it(tmp_path):
    t = _transcript(tmp_path, [
        _ev("user", [{"type": "tool_result", "tool_use_id": "1",
                      "content": f"ok\n<system-reminder>PostToolUse:Edit hook additional context: {CTX}</system-reminder>"}],
            "2026-10-14T10:00:01Z"),
        _ev("user", [{"type": "text", "text": f"PostToolUse:Edit hook additional context: {CTX}"}],
            "2026-10-14T10:00:02Z"),
        _ev("assistant", [{"type": "text", "text": "Bravo's lines are gone; I will restore them."}],
            "2026-10-14T10:00:03Z")])
    h = stranger.analyze_agent(t)["hook_context_seen"]
    assert len(h) == 1
    assert h[0]["line"] == CTX and h[0]["event"] == "PostToolUse:Edit" and h[0]["times"] == 2
    assert h[0]["reply"] == "Bravo's lines are gone; I will restore them."


def test_hand_writ_commands_counted_spec_done_summary_recognized():
    cmds = ["writ context", "cd /r && writ spec done -s 'parser fixed'", "ls writ/",
            "git status", "/opt/homebrew/bin/writ log", "echo writing"]
    hand = stranger.hand_writ_commands(cmds)
    assert hand == ["writ context", "cd /r && writ spec done -s 'parser fixed'",
                    "/opt/homebrew/bin/writ log"]


def test_unsealed_window_p50_max_and_never_sealed():
    edits = [{"path": "/r/a.py", "at": "2026-10-14T10:00:00Z"},
             {"path": "/r/a.py", "at": "2026-10-14T10:00:10Z"},
             {"path": "/r/b.py", "at": "2026-10-14T10:00:00Z"},
             {"path": "/r/c.py", "at": "2026-10-14T10:00:00Z"}]
    seals = [{"timestamp": "2026-10-14T10:00:00.200000Z", "changes": [{"path": "a.py"}]},
             {"timestamp": "2026-10-14T10:00:10.400000Z", "changes": [{"path": "a.py"}]},
             {"timestamp": "2026-10-14T10:00:03Z", "changes": [{"path": "b.py"}]}]
    w = stranger.unsealed_window(edits, seals, "/r")
    assert (w["edits"], w["sealed"], w["never_sealed"]) == (4, 3, 1)
    assert w["never_sealed_paths"] == ["c.py"]
    assert w["p50_s"] == 0.4 and w["max_s"] == 3.0


def test_hooks_log_summary_counts():
    h = stranger.hooks_log_summary("ok a\nerror: store\nseal refused x\n\n")
    assert h == {"lines": 3, "parsed": 0, "unparseable": 3, "errors": 1, "refused": 1}


def _bins() -> list[str]:
    env = os.environ.get("STRANGER_BINS")
    if env:
        return [b for b in env.split(":") if b]
    rel = REPO / "target" / "release" / "writ"
    found = shutil.which("writ")
    return [str(rel)] if rel.exists() else ([found] if found else [])


@pytest.mark.parametrize("writ_bin", _bins() or [None])
def test_kit_runs_setup_collect_score_on_the_binary(tmp_path, writ_bin):
    """No agent: setup, collect (finish of nothing), score. The kit must not
    crash on the shipped binary or the dev build, and must name the binary."""
    if writ_bin is None:
        pytest.skip("no writ binary")
    repo, out = tmp_path / "repo", tmp_path / "out"
    repo.mkdir()
    g = ["git", "-C", str(repo), "-c", "user.name=f", "-c", "user.email=f@example.invalid"]
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run([*g, "add", "-A"], check=True)
    subprocess.run([*g, "commit", "-qm", "init"], check=True)
    env = {k: v for k, v in os.environ.items() if k != "WRIT_AGENT_ID"}
    env.update(GIT_AUTHOR_NAME="f", GIT_AUTHOR_EMAIL="f@example.invalid",
               GIT_COMMITTER_NAME="f", GIT_COMMITTER_EMAIL="f@example.invalid")
    kit = str(KIT / "stranger.py")
    for args in (["setup", str(repo), str(out), "--writ", writ_bin],
                 ["collect", str(repo), str(out)],
                 ["score", str(out)]):
        r = subprocess.run([sys.executable, kit, *args], capture_output=True, text=True,
                           env=env, timeout=300)
        assert r.returncode == 0, f"{args[0]}: {r.stdout}{r.stderr}"
    s = json.loads((out / "summary.json").read_text())
    ver = subprocess.run([writ_bin, "--version"], capture_output=True, text=True).stdout.strip()
    assert s["writ_version"] == ver
    assert s["lost_lines"] == 0 and s["integrity_rc"] == 0
    assert s["unsealed_window"]["edits"] == 0
    md = (out / "report.md").read_text()
    assert ver in md and "Unsealed window" in md


def test_without_claude_setup_works_and_launch_says_what_is_missing(tmp_path):
    """Finding 143: a machine without Claude Code. setup, collect and score
    run; launch exits non-zero with one line naming what is missing."""
    bins = _bins()
    if not bins:
        pytest.skip("no writ binary")
    writ_bin = bins[0]
    repo, out = tmp_path / "repo", tmp_path / "out"
    repo.mkdir()
    g = ["git", "-C", str(repo), "-c", "user.name=f", "-c", "user.email=f@example.invalid"]
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run([*g, "add", "-A"], check=True)
    subprocess.run([*g, "commit", "-qm", "init"], check=True)
    git = shutil.which("git")
    pathdir = tmp_path / "path"
    pathdir.mkdir()
    (pathdir / "git").symlink_to(git)
    env = {"PATH": f"{pathdir}:/usr/bin:/bin", "HOME": str(tmp_path),
           "GIT_AUTHOR_NAME": "f", "GIT_AUTHOR_EMAIL": "f@example.invalid",
           "GIT_COMMITTER_NAME": "f", "GIT_COMMITTER_EMAIL": "f@example.invalid"}
    assert shutil.which("claude", path=env["PATH"]) is None
    kit = str(KIT / "stranger.py")
    r = subprocess.run([sys.executable, kit, "setup", str(repo), str(out), "--writ", writ_bin],
                       capture_output=True, text=True, env=env, timeout=300)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "Traceback" not in r.stderr
    p1 = tmp_path / "p1.txt"
    p1.write_text("task one")
    r = subprocess.run([sys.executable, kit, "launch", str(repo), str(out),
                        "--prompt", str(p1), "--prompt", str(p1)],
                       capture_output=True, text=True, env=env, timeout=60)
    assert r.returncode != 0
    assert "Traceback" not in r.stderr
    msg = r.stderr.strip()
    assert len(msg.splitlines()) == 1 and "claude" in msg, msg


@pytest.mark.parametrize("writ_bin", _bins() or [None])
def test_writ_id_for_session_matches_the_binary(tmp_path, writ_bin):
    """The kit's id for a session equals the id the writ binary resolves from
    CLAUDE_CODE_SESSION_ID (fake session ids only)."""
    if writ_bin is None:
        pytest.skip("no writ binary")
    repo = tmp_path / "r"
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    env = {k: v for k, v in os.environ.items()
           if k not in ("WRIT_AGENT_ID", "CLAUDECODE") and not k.startswith("CLAUDE_")}
    subprocess.run([writ_bin, "init", "-y"], cwd=repo, env=env, capture_output=True)
    for i, sess in enumerate(["00000000-1111-2222-3333-444444444444", "fake-session", "x" * 37]):
        r = subprocess.run([writ_bin, "spec", "add", "--id", f"s{i}", "--title", "t"], cwd=repo,
                           env=dict(env, CLAUDE_CODE_SESSION_ID=sess), capture_output=True, text=True)
        assert r.returncode == 0, r.stderr
        spec = json.loads((repo / ".writ" / "specs" / f"s{i}.json").read_text())
        assert spec["created_by"] == stranger.writ_id_for_session(sess), (sess, spec["created_by"])


def test_hooks_log_jsonl_summary():
    lines = [
        {"event": "PostToolUse", "tool": "Edit", "path": "a", "agent": "x", "outcome": "sealed",
         "edit_to_seal_ms": 130, "ms": 128},
        {"event": "PostToolUse", "tool": "Edit", "path": "a", "agent": "x-a1b2c3d4", "outcome": "refused",
         "ms": 40},
        {"event": "Stop", "agent": "x", "outcome": "nothing", "ms": 10},
    ]
    h = stranger.hooks_log_summary("\n".join(json.dumps(l) for l in lines) + "\n")
    assert h["lines"] == 3 and h["refused"] == 1 and h["unparseable"] == 0
    assert h["by_event"] == {"PostToolUse": 2, "Stop": 1}
    assert h["by_outcome"] == {"sealed": 1, "refused": 1, "nothing": 1}
    assert h["agents"] == ["x", "x-a1b2c3d4"]
    assert h["hook_ms"] == {"p50": 40.0, "max": 128.0}
    assert h["edit_to_seal_ms"] == {"p50": 130.0, "max": 130.0}


def test_setup_exposes_a_renamed_binary_as_writ_on_the_agents_path(tmp_path):
    """A binary named writ-<build> must still be `writ` to the agents and the
    hooks; otherwise every hook is a silent no-op (Bri's own fleet run 1)."""
    bins = _bins()
    if not bins:
        pytest.skip("no writ binary")
    renamed = tmp_path / "bin" / "writ-renamed"
    renamed.parent.mkdir()
    shutil.copy(bins[0], renamed)
    repo, out = tmp_path / "repo", tmp_path / "out"
    repo.mkdir()
    g = ["git", "-C", str(repo), "-c", "user.name=f", "-c", "user.email=f@example.invalid"]
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    (repo / "a.txt").write_text("a\n")
    subprocess.run([*g, "add", "-A"], check=True)
    subprocess.run([*g, "commit", "-qm", "init"], check=True)
    env = {k: v for k, v in os.environ.items() if k != "WRIT_AGENT_ID"}
    r = subprocess.run([sys.executable, str(KIT / "stranger.py"), "setup", str(repo), str(out),
                        "--writ", str(renamed)], capture_output=True, text=True, env=env, timeout=300)
    assert r.returncode == 0, r.stdout + r.stderr
    kit = json.loads((out / "kit.json").read_text())
    path = stranger.agent_env(kit, out)["PATH"]
    found = shutil.which("writ", path=path)
    assert found and Path(found).resolve() == renamed.resolve()


def test_hooks_log_unparseable_lines_are_counted_never_dropped():
    good = json.dumps({"event": "UserPromptSubmit", "outcome": "stashed", "ms": 3})
    text = '{"{event""event:""UserPromptSubmit"\n,"ms":30}\n' + good + "\n"
    h = stranger.hooks_log_summary(text)
    assert (h["lines"], h["parsed"], h["unparseable"]) == (3, 1, 2)


def test_subagent_calls_in_the_stream_are_not_counted_twice(tmp_path):
    t = _transcript(tmp_path, [
        _ev("assistant", [{"type": "tool_use", "id": "1", "name": "Bash", "input": {"command": "ls"}}],
            "2026-10-14T10:00:00Z", parent=None),
        _ev("assistant", [{"type": "tool_use", "id": "2", "name": "Bash", "input": {"command": "pwd"}}],
            "2026-10-14T10:00:01Z", parent="toolu_agent")])
    d = tmp_path / "agent-1.subagents"
    d.mkdir()
    (d / "agent-a1b2c3d4e5f6a7b8c.jsonl").write_text(json.dumps(
        {"agentId": "a1b2c3d4e5f6a7b8c", "timestamp": "2026-10-14T10:00:01Z",
         "message": {"content": [{"type": "tool_use", "id": "2", "name": "Bash",
                                  "input": {"command": "pwd"}}]}}) + "\n")
    i = stranger.analyze_agent(t)
    assert i["tools_total_main_and_subagents"] == {"Bash": 2} and i["tools_main_own"] == {"Bash": 1}
    assert i["tools_stream_incl_subagents"] == {"Bash": 2}


def test_seal_sources_by_identity_and_source():
    seals = [{"agent": {"id": "writ-bridge"}, "summary": "bridge import"},
             {"agent": {"id": "cc-1"}, "summary": "auto: Bash a.py, b.py"},
             {"agent": {"id": "cc-1"}, "summary": "auto: Bash c.py"},
             {"agent": {"id": "cc-1-a1b2c3d4"}, "summary": "auto: Edit x.py"},
             {"agent": {"id": "cc-1"}, "summary": "auto: retry 2 paths"},
             {"agent": {"id": "cc-2"}, "summary": "my own words"}]
    assert stranger.seal_sources(seals) == {
        "cc-1": {"auto: Bash": 2, "auto: retry": 1}, "cc-1-a1b2c3d4": {"auto: Edit": 1},
        "cc-2": {"hand": 1}}


def test_hooks_log_errors_by_outcome_not_by_path_words():
    lines = [{"event": "PostToolUse", "path": "tests/test_errors.py", "outcome": "sealed", "ms": 3},
             {"event": "PostToolUse", "path": "a.py", "outcome": "error", "detail": "x", "ms": 3},
             {"event": "PostToolUse", "path": "b.py", "outcome": "refused", "ms": 3}]
    h = stranger.hooks_log_summary("\n".join(json.dumps(l) for l in lines) + "\n")
    assert (h["errors"], h["refused"]) == (1, 1)


def test_window_ambiguity_and_credit_moved_from_hooks_log():
    recs = [
        {"event": "PostToolUse", "tool": "Bash", "agent": "a", "outcome": "attributed", "paths": ["x.py"]},
        {"event": "PostToolUse", "path": "x.py", "agent": "a", "outcome": "sealed", "edit_to_seal_ms": 100},
        {"event": "PostToolUse", "path": "y.py", "agent": "a", "outcome": "sealed", "edit_to_seal_ms": 300},
        {"event": "PostToolUse", "path": "y.py", "agent": "a", "outcome": "sealed", "edit_to_seal_ms": 900,
         "coarse_mtime": True},
        {"event": "PostToolUse", "path": "z.py", "agent": "a", "agents": ["a", "b"], "outcome": "ambiguous"},
    ]
    w = stranger.hook_window(recs)
    assert (w["sealed_records"], w["files"]) == (3, 2)
    assert w["exact_ms"] == {"p50": 300.0, "max": 300.0, "n": 2} or w["exact_ms"]["n"] == 2
    assert w["at_most_ms_coarse"]["n"] == 1
    a = stranger.ambiguity_rate(recs)
    assert (a["ambiguous_records"], a["ambiguous_files"], a["sealed_records"]) == (1, 1, 3)
    seals = [{"id": "s1" * 6, "agent": {"id": "c"}, "spec_id": "f", "changes": [{"path": "z.py"}]},
             {"id": "s2" * 6, "agent": {"id": "a"}, "spec_id": "g", "changes": [{"path": "x.py"}]}]
    moved = stranger.credit_moved(recs, seals)
    assert len(moved) == 1 and moved[0].startswith("z.py sealed by c")


def test_worst_window_names_path_hook_and_cause():
    recs = [
        {"ts": "T1", "event": "PostToolUse", "tool": "Write", "path": "app.py", "agent": "d",
         "outcome": "sealed", "edit_to_seal_ms": 100, "seal": "s1"},
        {"ts": "T2", "event": "PostToolUse", "tool": "Bash", "path": "app.py", "agent": "e",
         "outcome": "ambiguous"},
        {"ts": "T3", "event": "PostToolUse", "tool": "retry", "path": "app.py", "agent": "d",
         "outcome": "sealed", "edit_to_seal_ms": 17951, "seal": "s2"},
    ]
    w = stranger.worst_window(recs)
    assert (w["ms"], w["path"], w["hook"], w["seal"]) == (17951, "app.py", "retry", "s2")
    assert "drained" in w["cause"] and "ambiguous by e" in w["cause"]


# Finding 187: records writ could not attribute are not asks.
RUNA_F4 = EVIDENCE / "0.4.3-live-runA-freeze4"
needs_runa_f4 = pytest.mark.skipif(not (RUNA_F4 / "summary.json").exists(),
                                   reason="0.4.3 run A freeze 4 evidence not present")


@needs_runa_f4
def test_run_a_freeze4_has_28_records_not_attributed_and_5_ask_lines_shown():
    summary = json.loads((RUNA_F4 / "summary.json").read_text())
    recs = stranger._records(RUNA_F4)
    a = stranger.ask_counts(summary, recs)
    assert a["records_not_attributed"] == 28
    assert a["ambiguous_records"] == 28  # 0.4.3 field name, kept for old evidence
    assert a["ask_lines_shown"] == {"agent-1": 2, "agent-2": 3}
    assert a["ask_lines_shown_total"] == 5
    assert a["records_told"] is None  # 0.4.3 hooks.log has no told flag


def test_ask_lines_count_only_ambiguity_lines_and_repeats():
    agents = {
        "agent-1": {"hook_context_seen": [
            {"line": f"a.py changed while 2 agents ran commands (x, y); {stranger.ASK_MARK}; ...",
             "times": 2},
            {"line": "a.py: writ did not seal it: removes lines sealed by y", "times": 1},
        ]},
        "agent-2": {"hook_context_seen": []},
    }
    recs = [
        {"event": "PostToolUse", "path": "a.py", "agent": "agent-1", "outcome": "ambiguous", "told": True},
        {"event": "Stop", "path": "a.py", "agent": "agent-2", "outcome": "ambiguous", "told": False},
    ]
    a = stranger.ambiguity_rate(recs, agents)
    assert (a["records_not_attributed"], a["records_told"]) == (2, 1)
    assert a["ask_lines_shown"] == {"agent-1": 2, "agent-2": 0}
    assert a["ask_lines_shown_total"] == 2


C409 = EVIDENCE / "0.4.3-fleet-live-c409"


@pytest.mark.skipif(not (C409 / "summary.json").exists(), reason="c409 evidence not present")
def test_c409_ask_lines_include_the_subagents_own_transcripts():
    summary = json.loads((C409 / "summary.json").read_text())
    a = stranger.ask_counts(summary, stranger._records(C409))
    assert a["records_not_attributed"] == 17
    assert a["ask_lines_shown"] == {"agent-1": 3, "agent-2": 1}
    assert a["ask_lines_shown_total"] == 4
