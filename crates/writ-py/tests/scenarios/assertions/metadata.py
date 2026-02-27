"""Metadata and context assertion helpers for YAML scenario tests."""


def check(assertion: dict, repo) -> None:
    """Dispatch a metadata assertion."""
    atype = assertion["type"]
    dispatch = {
        "diverged_branches_count": _diverged_branches_count,
        "seal_count": _seal_count,
        "spec_exists": _spec_exists,
        "context_has_field": _context_has_field,
    }
    handler = dispatch.get(atype)
    if handler is None:
        raise ValueError(f"Unknown metadata assertion type: {atype}")
    handler(assertion, repo)


def _diverged_branches_count(assertion, repo):
    expected = assertion["expected"]
    branches = repo.diverged_branches()
    assert len(branches) == expected, (
        f"Expected {expected} diverged branches, got {len(branches)}"
    )


def _seal_count(assertion, repo):
    """Check total seal count.

    Note: log() returns the HEAD chain which includes convergence seals.
    If a scenario runs converge_all(apply=True), the convergence seal is
    counted. Use with awareness of post-convergence seal count.
    """
    expected = assertion["expected"]
    log = repo.log()
    assert len(log) == expected, (
        f"Expected {expected} seals, got {len(log)}"
    )


def _spec_exists(assertion, repo):
    spec_id = assertion["spec_id"]
    try:
        spec = repo.get_spec(spec_id)
    except Exception as e:
        raise AssertionError(
            f"Spec '{spec_id}' not found in repository: {e}"
        ) from e
    assert spec["id"] == spec_id, (
        f"Expected spec id '{spec_id}', got '{spec['id']}'"
    )


def _context_has_field(assertion, repo):
    field = assertion["field"]
    ctx = repo.context()
    assert field in ctx, (
        f"Context missing field '{field}'. "
        f"Available: {list(ctx.keys())}"
    )
