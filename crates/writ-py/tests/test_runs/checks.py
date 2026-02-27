"""Validation check library for test runs.

Ported from TR19 converge.sh patterns to composable Python checks.
Each check function returns a CheckResult with pass/fail + details.
Checks are dispatched by category from the charter's check definitions.
"""

import ast
import os
from pathlib import Path
from typing import Any

from .report import CheckResult


# ── Dispatch ─────────────────────────────────────────────────────────


def run_checks(
    check_defs: dict[str, list],
    convergence_report: dict,
    repo,
    workspace: Path,
) -> list[CheckResult]:
    """Run all checks defined in the charter and return results."""
    results: list[CheckResult] = []

    for check_def in check_defs.get("convergence", []):
        results.append(_dispatch_convergence(check_def, convergence_report, workspace))

    for check_def in check_defs.get("security", []):
        results.append(_dispatch_security(check_def, repo))

    for check_def in check_defs.get("metadata", []):
        results.append(_dispatch_metadata(check_def, repo))

    for check_def in check_defs.get("quality", []):
        results.append(_dispatch_quality(check_def, workspace))

    return results


def _dispatch_convergence(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    """Dispatch a convergence check."""
    ctype = check_def["type"]
    handlers = {
        "not_degraded": _check_not_degraded,
        "is_clean": _check_is_clean,
        "no_escalations": _check_no_escalations,
        "has_escalations": _check_has_escalations,
        "definitions_preserved": _check_definitions_preserved,
        "file_contains": _check_file_contains,
        "file_exists": _check_file_exists,
        "file_not_contains": _check_file_not_contains,
    }
    handler = handlers.get(ctype)
    if handler is None:
        return CheckResult(
            name=ctype, category="convergence", passed=False,
            details=f"Unknown check type: {ctype}"
        )
    return handler(check_def, report, workspace)


def _dispatch_security(check_def: dict, repo) -> CheckResult:
    """Dispatch a security check."""
    ctype = check_def["type"]
    handlers = {
        "chain_valid": _check_chain_valid,
        "chain_no_failures": _check_chain_no_failures,
        "seals_have_hashes": _check_seals_have_hashes,
    }
    handler = handlers.get(ctype)
    if handler is None:
        return CheckResult(
            name=ctype, category="security", passed=False,
            details=f"Unknown check type: {ctype}"
        )
    return handler(check_def, repo)


def _dispatch_metadata(check_def: dict, repo) -> CheckResult:
    """Dispatch a metadata check."""
    ctype = check_def["type"]
    handlers = {
        "post_convergence_clean": _check_post_convergence_clean,
        "context_has_field": _check_context_has_field,
        "spec_exists": _check_spec_exists,
    }
    handler = handlers.get(ctype)
    if handler is None:
        return CheckResult(
            name=ctype, category="metadata", passed=False,
            details=f"Unknown check type: {ctype}"
        )
    return handler(check_def, repo)


def _dispatch_quality(check_def: dict, workspace: Path) -> CheckResult:
    """Dispatch a code quality check."""
    ctype = check_def["type"]
    handlers = {
        "python_syntax": _check_python_syntax,
        "python_import_order": _check_python_import_order,
        "no_duplicate_imports": _check_no_duplicate_imports,
        "bracket_balance": _check_bracket_balance,
    }
    handler = handlers.get(ctype)
    if handler is None:
        return CheckResult(
            name=ctype, category="quality", passed=False,
            details=f"Unknown check type: {ctype}"
        )
    return handler(check_def, workspace)


# ── Convergence Checks ───────────────────────────────────────────────


def _check_not_degraded(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    degraded = report.get("degraded", False)
    return CheckResult(
        name="not_degraded",
        category="convergence",
        passed=not degraded,
        details="Convergence was degraded" if degraded else "",
    )


def _check_is_clean(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    expected = check_def.get("expected", True)
    is_clean = report.get("is_clean", False)
    return CheckResult(
        name="is_clean",
        category="convergence",
        passed=is_clean == expected,
        details=f"is_clean={is_clean}, expected={expected}" if is_clean != expected else "",
    )


def _check_no_escalations(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    escalations = report.get("escalations", [])
    return CheckResult(
        name="no_escalations",
        category="convergence",
        passed=len(escalations) == 0,
        details=(
            f"{len(escalations)} escalation(s): "
            f"{[e.get('file_path', '?') for e in escalations]}"
            if escalations else ""
        ),
    )


def _check_has_escalations(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    escalations = report.get("escalations", [])
    return CheckResult(
        name="has_escalations",
        category="convergence",
        passed=len(escalations) > 0,
        details="Expected escalations but got none" if not escalations else "",
    )


def _check_definitions_preserved(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    filepath = check_def["file"]
    definitions = check_def["definitions"]
    full_path = workspace / filepath
    name = f"definitions_preserved:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="convergence", passed=False,
                          details=f"{filepath} does not exist")

    content = full_path.read_text()
    missing = [d for d in definitions if d not in content]

    return CheckResult(
        name=name,
        category="convergence",
        passed=len(missing) == 0,
        details=f"Missing: {missing}" if missing else "",
    )


def _check_file_contains(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    filepath = check_def["file"]
    expected = check_def["content"]
    full_path = workspace / filepath
    name = f"file_contains:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="convergence", passed=False,
                          details=f"{filepath} does not exist")

    content = full_path.read_text()
    return CheckResult(
        name=name,
        category="convergence",
        passed=expected in content,
        details=f"'{expected}' not found" if expected not in content else "",
    )


def _check_file_not_contains(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    filepath = check_def["file"]
    unexpected = check_def["content"]
    full_path = workspace / filepath
    name = f"file_not_contains:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="convergence", passed=True)

    content = full_path.read_text()
    return CheckResult(
        name=name,
        category="convergence",
        passed=unexpected not in content,
        details=f"'{unexpected}' was found but should not be present" if unexpected in content else "",
    )


def _check_file_exists(
    check_def: dict, report: dict, workspace: Path
) -> CheckResult:
    filepath = check_def["file"]
    full_path = workspace / filepath
    return CheckResult(
        name=f"file_exists:{filepath}",
        category="convergence",
        passed=full_path.exists(),
        details=f"{filepath} not found" if not full_path.exists() else "",
    )


# ── Security Checks ─────────────────────────────────────────────────


def _check_chain_valid(check_def: dict, repo) -> CheckResult:
    result = repo.verify_chain()
    return CheckResult(
        name="chain_valid",
        category="security",
        passed=result["valid"] is True,
        details=(
            f"Total: {result['total_seals']}, "
            f"Verified: {result['verified']}, "
            f"Failures: {len(result['failures'])}"
            if not result["valid"] else ""
        ),
    )


def _check_chain_no_failures(check_def: dict, repo) -> CheckResult:
    result = repo.verify_chain()
    failures = result.get("failures", [])
    return CheckResult(
        name="chain_no_failures",
        category="security",
        passed=len(failures) == 0,
        details=(
            f"{len(failures)} failure(s): "
            f"{[f.get('seal_id', '?')[:8] for f in failures]}"
            if failures else ""
        ),
    )


def _check_seals_have_hashes(check_def: dict, repo) -> CheckResult:
    log = repo.log()
    missing = []
    for seal in log:
        seal_id = seal.get("id", "?")[:8]
        if not seal.get("content_hash"):
            missing.append(f"{seal_id}:content_hash")
        if not seal.get("chain_hash"):
            missing.append(f"{seal_id}:chain_hash")

    return CheckResult(
        name="seals_have_hashes",
        category="security",
        passed=len(missing) == 0,
        details=f"Missing: {missing}" if missing else "",
    )


# ── Metadata Checks ─────────────────────────────────────────────────


def _check_post_convergence_clean(check_def: dict, repo) -> CheckResult:
    """Check that post-convergence state is clean (from TR19 converge.sh)."""
    ctx = repo.context()
    problems = []

    diverged = ctx.get("diverged_branches", [])
    if len(diverged) > 0:
        problems.append(f"diverged_branches={len(diverged)}")

    conv_rec = ctx.get("convergence_recommended", False)
    if conv_rec:
        problems.append("convergence_recommended=true")

    return CheckResult(
        name="post_convergence_clean",
        category="metadata",
        passed=len(problems) == 0,
        details="; ".join(problems) if problems else "",
    )


def _check_context_has_field(check_def: dict, repo) -> CheckResult:
    field = check_def["field"]
    ctx = repo.context()
    return CheckResult(
        name=f"context_has_field:{field}",
        category="metadata",
        passed=field in ctx,
        details=f"Field '{field}' not in context. Available: {list(ctx.keys())}" if field not in ctx else "",
    )


def _check_spec_exists(check_def: dict, repo) -> CheckResult:
    spec_id = check_def["spec_id"]
    try:
        spec = repo.get_spec(spec_id)
        return CheckResult(
            name=f"spec_exists:{spec_id}",
            category="metadata",
            passed=spec["id"] == spec_id,
        )
    except Exception as e:
        return CheckResult(
            name=f"spec_exists:{spec_id}",
            category="metadata",
            passed=False,
            details=f"Spec '{spec_id}' not found: {e}",
        )


# ── Code Quality Checks ─────────────────────────────────────────────


def _check_python_syntax(check_def: dict, workspace: Path) -> CheckResult:
    filepath = check_def["file"]
    full_path = workspace / filepath
    name = f"python_syntax:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="quality", passed=False,
                          details=f"{filepath} does not exist")

    source = full_path.read_text()
    try:
        compile(source, filepath, "exec")
        return CheckResult(name=name, category="quality", passed=True)
    except SyntaxError as e:
        return CheckResult(name=name, category="quality", passed=False,
                          details=f"Syntax error: {e}")


def _check_python_import_order(check_def: dict, workspace: Path) -> CheckResult:
    """Check that imports follow stdlib -> third-party -> local order."""
    filepath = check_def["file"]
    full_path = workspace / filepath
    name = f"import_order:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="quality", passed=False,
                          details=f"{filepath} does not exist")

    STDLIB = {
        "os", "sys", "datetime", "logging", "hashlib", "secrets", "functools",
        "json", "re", "typing", "enum", "collections", "pathlib", "io",
        "abc", "dataclasses", "time", "uuid", "copy", "math",
    }
    THIRDPARTY = {
        "flask", "flask_cors", "pydantic", "sqlalchemy", "redis", "celery",
        "fastapi", "uvicorn", "httpx", "pytest", "requests",
    }

    lines = full_path.read_text().splitlines()
    last_type = 0  # 0=none, 1=stdlib, 2=thirdparty, 3=local

    for i, line in enumerate(lines[:40]):  # Only check first 40 lines
        stripped = line.strip()
        if not stripped.startswith(("from ", "import ")):
            continue

        if stripped.startswith("from "):
            module = stripped.split()[1].split(".")[0]
        else:
            module = stripped.split()[1].split(".")[0]

        if module in STDLIB:
            cur_type = 1
        elif module in THIRDPARTY:
            cur_type = 2
        else:
            cur_type = 3

        if cur_type < last_type:
            return CheckResult(
                name=name, category="quality", passed=False,
                details=f"Line {i+1}: '{stripped}' (type {cur_type}) after type {last_type}"
            )
        last_type = cur_type

    return CheckResult(name=name, category="quality", passed=True)


def _check_no_duplicate_imports(check_def: dict, workspace: Path) -> CheckResult:
    filepath = check_def["file"]
    full_path = workspace / filepath
    name = f"no_duplicate_imports:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="quality", passed=False,
                          details=f"{filepath} does not exist")

    lines = full_path.read_text().splitlines()
    import_lines = [l.strip() for l in lines if l.strip().startswith(("from ", "import "))]

    seen = set()
    dupes = []
    for il in import_lines:
        if il in seen:
            dupes.append(il)
        seen.add(il)

    return CheckResult(
        name=name,
        category="quality",
        passed=len(dupes) == 0,
        details=f"{len(dupes)} duplicate(s): {dupes[:3]}" if dupes else "",
    )


def _check_bracket_balance(check_def: dict, workspace: Path) -> CheckResult:
    filepath = check_def["file"]
    full_path = workspace / filepath
    name = f"bracket_balance:{filepath}"

    if not full_path.exists():
        return CheckResult(name=name, category="quality", passed=False,
                          details=f"{filepath} does not exist")

    content = full_path.read_text()
    pairs = [("(", ")"), ("[", "]"), ("{", "}")]
    problems = []
    for open_c, close_c in pairs:
        open_count = content.count(open_c)
        close_count = content.count(close_c)
        if open_count != close_count:
            problems.append(f"'{open_c}{close_c}': {open_count} open, {close_count} close")

    return CheckResult(
        name=name,
        category="quality",
        passed=len(problems) == 0,
        details="; ".join(problems) if problems else "",
    )
