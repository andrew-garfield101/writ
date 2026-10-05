"""Contract tests for bench/transcript_metrics.py (pure, synthetic transcripts)."""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from transcript_metrics import hint_metrics, metrics_for_file, parse_bash_calls  # noqa: E402

HINT = (
    "sealed 1a2b3c  spec: sa\n"
    "  left for other specs: src/b/lib.rs\n"
    "  hint: writ seal -s \"work\" --spec sa --paths src/a/lib.rs\n"
)


def transcript(*steps: tuple[str, str, bool]) -> list[str]:
    """Build stream-json lines: one tool_use + tool_result per (cmd, out, err)."""
    lines = [json.dumps({"type": "system", "subtype": "init"})]
    for n, (cmd, out, err) in enumerate(steps):
        tid = f"toolu_{n}"
        lines.append(json.dumps({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "running"},
            {"type": "tool_use", "id": tid, "name": "Bash", "input": {"command": cmd}},
        ]}}))
        lines.append(json.dumps({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": tid, "is_error": err,
             "content": [{"type": "text", "text": out}]},
        ]}}))
    return lines


def metrics(*steps):
    return hint_metrics(parse_bash_calls(transcript(*steps)))


def test_hint_then_paths_retry_counts_as_self_corrected_and_landed():
    m = metrics(
        ("writ seal -s work --agent a", HINT, False),
        ("writ seal -s work --spec sa --paths src/a/lib.rs", "sealed 4d5e6f", False),
    )
    assert (m.hints_shown, m.self_corrected, m.correction_landed, m.ignored) == (1, 1, 1, 0)


def test_failed_retry_is_corrected_but_not_landed():
    m = metrics(
        ("writ seal -s work", HINT, False),
        ("writ seal -s work --paths nope.rs", "error: no such path", True),
    )
    assert (m.self_corrected, m.correction_landed) == (1, 0)


def test_git_fallback_after_hint_is_counted_not_corrected():
    m = metrics(
        ("writ seal -s work", HINT, False),
        ("git add -A && git commit -m work", "[main abc] work", False),
    )
    assert (m.self_corrected, m.git_fallback_after_hint, m.ignored) == (0, 1, 0)


def test_hint_with_no_follow_up_is_ignored():
    m = metrics(("writ seal -s work", HINT, False), ("ls", "a b", False))
    assert (m.hints_shown, m.ignored) == (1, 1)


def test_seal_with_paths_mentioning_paths_is_not_a_hint():
    m = metrics(("writ seal -s w --paths a.rs", "sealed; --paths honored", False))
    assert m.hints_shown == 0


def test_spec_done_hint_and_spec_done_paths_retry():
    m = metrics(
        ("writ spec done sa --agent a", "nothing sealed; pass --paths", False),
        ("writ spec done sa --agent a --paths src/a/lib.rs", "done", False),
    )
    assert (m.hints_shown, m.self_corrected, m.correction_landed) == (1, 1, 1)


def test_second_hint_before_any_retry_closes_the_first_as_ignored():
    m = metrics(
        ("writ seal -s one", HINT, False),
        ("writ seal -s two", HINT, False),
        ("writ seal -s three --paths src/a/lib.rs", "sealed", False),
    )
    assert (m.hints_shown, m.ignored, m.self_corrected) == (2, 1, 1)


def test_non_json_noise_and_non_bash_tools_are_skipped(tmp_path):
    lines = ["not json"] + transcript(("writ seal -s w", HINT, False))
    lines.append(json.dumps({"type": "assistant", "message": {"content": [
        {"type": "tool_use", "id": "r1", "name": "Read", "input": {"file_path": "x"}}]}}))
    f = tmp_path / "t.jsonl"
    f.write_text("\n".join(lines))
    out = metrics_for_file(f)
    assert out["bash_calls"] == 1 and out["hints_shown"] == 1 and out["ignored"] == 1
