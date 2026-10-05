"""Scorecard metric for live agent runs: did the agent self-correct after a
`--paths` hint?

S.1 makes a seal without `--paths` leave contested files out and print a hint
(an exact `writ seal ... --paths ...` command). This module reads a Claude Code
`claude -p --output-format stream-json --verbose` transcript and counts:

- hints_shown: seal / spec done calls made without `--paths` whose output
  mentions `--paths` (the exclusion hint);
- self_corrected: hints followed later in the same transcript by a seal or
  spec done call that passes `--paths`;
- correction_landed: of those, retries whose result was not an error;
- git_fallback_after_hint: hints followed by a raw `git add` / `git commit`;
- ignored: hints followed by neither.

It answers "is the message alone enough?" without a judge model. Pure: no
binary, no network.

Usage: python3 bench/transcript_metrics.py transcript.jsonl [...] (JSON out).
"""

from __future__ import annotations

import json
import re
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Iterable

SEAL_CMD = re.compile(r"\bwrit\s+(seal|spec\s+done)\b")
PATHS_FLAG = re.compile(r"(^|\s)--paths(\s|=|$)")
GIT_FALLBACK = re.compile(r"\bgit\s+(add|commit)\b")
ERROR_TEXT = re.compile(r"^\s*error:", re.M)


@dataclass
class ToolCall:
    """One Bash tool call and its result, in transcript order."""

    command: str
    output: str = ""
    is_error: bool = False


@dataclass
class HintMetrics:
    hints_shown: int = 0
    self_corrected: int = 0
    correction_landed: int = 0
    git_fallback_after_hint: int = 0
    ignored: int = 0
    examples: list[str] = field(default_factory=list)


def _result_text(content: object) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(
            part.get("text", "") for part in content if isinstance(part, dict)
        )
    return ""


def parse_bash_calls(lines: Iterable[str]) -> list[ToolCall]:
    """Pair Bash tool_use blocks with their tool_result, in order."""
    calls: dict[str, ToolCall] = {}
    order: list[str] = []
    for raw in lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            event = json.loads(raw)
        except json.JSONDecodeError:
            continue  # stream-json may interleave non-JSON noise; skip it
        message = event.get("message") or {}
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use" and block.get("name") == "Bash":
                command = (block.get("input") or {}).get("command", "")
                calls[block["id"]] = ToolCall(command=command)
                order.append(block["id"])
            elif block.get("type") == "tool_result":
                call = calls.get(block.get("tool_use_id", ""))
                if call is not None:
                    call.output = _result_text(block.get("content"))
                    call.is_error = bool(block.get("is_error"))
    return [calls[i] for i in order]


def is_seal(call: ToolCall) -> bool:
    return bool(SEAL_CMD.search(call.command))


def has_paths(call: ToolCall) -> bool:
    return bool(PATHS_FLAG.search(call.command))


def is_hint(call: ToolCall) -> bool:
    """A seal/spec done without --paths whose output points at --paths."""
    return is_seal(call) and not has_paths(call) and "--paths" in call.output


def landed(call: ToolCall) -> bool:
    return not call.is_error and not ERROR_TEXT.search(call.output)


def hint_metrics(calls: list[ToolCall]) -> HintMetrics:
    """Classify each hint by what the agent did next (first matching action)."""
    m = HintMetrics()
    for i, call in enumerate(calls):
        if not is_hint(call):
            continue
        m.hints_shown += 1
        outcome = "ignored"
        for later in calls[i + 1:]:
            if is_seal(later) and has_paths(later):
                outcome = "corrected"
                m.self_corrected += 1
                if landed(later):
                    m.correction_landed += 1
                if len(m.examples) < 3:
                    m.examples.append(later.command)
                break
            if GIT_FALLBACK.search(later.command):
                outcome = "git"
                m.git_fallback_after_hint += 1
                break
            if is_hint(later):
                break  # a fresh hint; the earlier one went unanswered
        if outcome == "ignored":
            m.ignored += 1
    return m


def metrics_for_file(path: Path) -> dict:
    with path.open(encoding="utf-8") as fh:
        calls = parse_bash_calls(fh)
    out = asdict(hint_metrics(calls))
    out["transcript"] = str(path)
    out["bash_calls"] = len(calls)
    return out


def main(argv: list[str]) -> int:
    if not argv:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    print(json.dumps([metrics_for_file(Path(p)) for p in argv], indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
