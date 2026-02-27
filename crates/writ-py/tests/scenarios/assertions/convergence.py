"""Convergence-specific assertion helpers for YAML scenario tests."""

import os


def check(assertion: dict, report: dict, repo, tmp_dir: str) -> None:
    """Dispatch a convergence assertion."""
    atype = assertion["type"]
    dispatch = {
        "no_escalations": _no_escalations,
        "has_escalations": _has_escalations,
        "escalation_count": _escalation_count,
        "is_clean": _is_clean,
        "not_degraded": _not_degraded,
        "all_definitions_preserved": _all_definitions_preserved,
        "file_contains": _file_contains,
        "file_not_contains": _file_not_contains,
        "file_deleted": _file_deleted,
        "file_exists": _file_exists,
        "files_changed_count": _files_changed_count,
        "confidence_above": _confidence_above,
        "total_conflicts": _total_conflicts,
        "total_auto_merged_gte": _total_auto_merged_gte,
        "escalated_file": _escalated_file,
    }
    handler = dispatch.get(atype)
    if handler is None:
        raise ValueError(f"Unknown convergence assertion type: {atype}")
    handler(assertion, report, repo, tmp_dir)


def check_verification(
    assertion: dict, report: dict, repo, tmp_dir: str
) -> None:
    """Dispatch a verification assertion (post-convergence checks)."""
    atype = assertion["type"]
    dispatch = {
        "syntax_valid": _syntax_valid,
        "no_silent_additions": _no_silent_additions,
    }
    handler = dispatch.get(atype)
    if handler is None:
        raise ValueError(f"Unknown verification assertion type: {atype}")
    handler(assertion, report, repo, tmp_dir)


# ── Convergence assertions ────────────────────────────────────────────


def _no_escalations(assertion, report, repo, tmp_dir):
    escalations = report.get("escalations", [])
    assert len(escalations) == 0, (
        f"Expected no escalations, got {len(escalations)}: "
        f"{[e['file_path'] for e in escalations]}"
    )


def _has_escalations(assertion, report, repo, tmp_dir):
    escalations = report.get("escalations", [])
    assert len(escalations) > 0, "Expected escalations but got none"


def _escalation_count(assertion, report, repo, tmp_dir):
    expected = assertion["expected"]
    escalations = report.get("escalations", [])
    assert len(escalations) == expected, (
        f"Expected {expected} escalations, got {len(escalations)}"
    )


def _is_clean(assertion, report, repo, tmp_dir):
    expected = assertion.get("expected", True)
    assert report["is_clean"] == expected, (
        f"Expected is_clean={expected}, got {report['is_clean']}"
    )


def _not_degraded(assertion, report, repo, tmp_dir):
    assert not report.get("degraded", False), "Convergence was degraded"


def _all_definitions_preserved(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    definitions = assertion["definitions"]
    full_path = os.path.join(tmp_dir, filepath)
    with open(full_path) as f:
        content = f.read()
    for defn in definitions:
        assert defn in content, (
            f"Definition '{defn}' not found in {filepath}.\n"
            f"Content:\n{content}"
        )


def _file_contains(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    expected = assertion["content"]
    full_path = os.path.join(tmp_dir, filepath)
    with open(full_path) as f:
        content = f.read()
    assert expected in content, (
        f"File {filepath} does not contain: {expected!r}\n"
        f"Content:\n{content}"
    )


def _file_not_contains(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    unexpected = assertion["content"]
    full_path = os.path.join(tmp_dir, filepath)
    with open(full_path) as f:
        content = f.read()
    assert unexpected not in content, (
        f"File {filepath} should NOT contain: {unexpected!r}\n"
        f"Content:\n{content}"
    )


def _file_deleted(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    full_path = os.path.join(tmp_dir, filepath)
    assert not os.path.exists(full_path), (
        f"Expected {filepath} to be deleted but it still exists"
    )


def _file_exists(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    full_path = os.path.join(tmp_dir, filepath)
    assert os.path.exists(full_path), (
        f"Expected {filepath} to exist but it was not found"
    )


def _files_changed_count(assertion, report, repo, tmp_dir):
    expected = assertion["expected"]
    files = report.get("files_changed", [])
    assert len(files) == expected, (
        f"Expected {expected} files changed, got {len(files)}: {files}"
    )


def _confidence_above(assertion, report, repo, tmp_dir):
    """Check that the quality report shows confidence above threshold for a file.

    If require_report is set (default: false), fails when no quality report
    or file decision is present, instead of silently passing.
    """
    threshold = assertion["threshold"]
    filepath = assertion["file"]
    require = assertion.get("require_report", False)
    quality = report.get("quality_report")
    if quality is None:
        if require:
            raise AssertionError(
                f"confidence_above: no quality_report in convergence result "
                f"(required for {filepath})"
            )
        return
    for decision in quality.get("file_decisions", []):
        if decision.get("path") == filepath:
            conf = decision.get("confidence", 0.0)
            assert conf >= threshold, (
                f"Confidence for {filepath} is {conf}, "
                f"expected >= {threshold}"
            )
            return
    if require:
        raise AssertionError(
            f"confidence_above: {filepath} not found in quality_report file_decisions"
        )


def _total_conflicts(assertion, report, repo, tmp_dir):
    expected = assertion["expected"]
    actual = report.get("total_conflicts", 0)
    assert actual == expected, (
        f"Expected {expected} total conflicts, got {actual}"
    )


def _total_auto_merged_gte(assertion, report, repo, tmp_dir):
    minimum = assertion["minimum"]
    actual = report.get("total_auto_merged", 0)
    assert actual >= minimum, (
        f"Expected at least {minimum} auto-merged, got {actual}"
    )


def _escalated_file(assertion, report, repo, tmp_dir):
    filepath = assertion["file"]
    escalations = report.get("escalations", [])
    escalated_paths = [e["file_path"] for e in escalations]
    assert filepath in escalated_paths, (
        f"Expected {filepath} to be escalated. "
        f"Escalated files: {escalated_paths}"
    )


# ── Verification assertions ──────────────────────────────────────────


def _syntax_valid(assertion, report, repo, tmp_dir):
    """Check that a file is syntactically valid.

    Currently supports Python (.py) via compile(). Non-Python files
    are verified to exist and be non-empty (basic sanity check) since
    we don't have parsers for other languages in the test framework.
    """
    filepath = assertion["file"]
    full_path = os.path.join(tmp_dir, filepath)
    assert os.path.exists(full_path), f"syntax_valid: {filepath} does not exist"

    with open(full_path) as f:
        source = f.read()

    assert len(source.strip()) > 0, f"syntax_valid: {filepath} is empty"

    if filepath.endswith(".py"):
        try:
            compile(source, filepath, "exec")
        except SyntaxError as e:
            raise AssertionError(
                f"Syntax error in {filepath}: {e}\nContent:\n{source}"
            )


def _no_silent_additions(assertion, report, repo, tmp_dir):
    """Check that the traceability report (if present) has no additions."""
    # This checks the convergence report's quality/traceability data
    quality = report.get("quality_report")
    if quality is None:
        return
    # If traceability data is present, check it
    for check_item in quality.get("consistency_checks", []):
        if check_item.get("type") == "silent_addition":
            assert check_item.get("passed", True), (
                f"Silent addition detected: {check_item}"
            )
