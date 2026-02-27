"""Security-specific assertion helpers for YAML scenario tests."""

# Cached verify_chain result to avoid redundant calls within a scenario.
_chain_cache: dict = {}


def check(assertion: dict, report: dict, repo) -> None:
    """Dispatch a security assertion."""
    atype = assertion["type"]
    dispatch = {
        "chain_valid": _chain_valid,
        "chain_seal_count": _chain_seal_count,
        "seal_has_content_hash": _seal_has_content_hash,
        "seal_has_chain_hash": _seal_has_chain_hash,
        "chain_no_failures": _chain_no_failures,
    }
    handler = dispatch.get(atype)
    if handler is None:
        raise ValueError(f"Unknown security assertion type: {atype}")
    handler(assertion, report, repo)


def _get_chain_result(repo) -> dict:
    """Get cached verify_chain result (avoids redundant calls per scenario)."""
    repo_id = id(repo)
    if repo_id not in _chain_cache:
        _chain_cache[repo_id] = repo.verify_chain()
    return _chain_cache[repo_id]


def clear_cache() -> None:
    """Clear the chain verification cache between scenarios."""
    _chain_cache.clear()


def _chain_valid(assertion, report, repo):
    result = _get_chain_result(repo)
    assert result["valid"] is True, (
        f"Chain verification failed. "
        f"Total: {result['total_seals']}, "
        f"Verified: {result['verified']}, "
        f"Failures: {len(result['failures'])}"
    )


def _chain_seal_count(assertion, report, repo):
    expected = assertion["expected"]
    result = _get_chain_result(repo)
    actual = result["total_seals"]
    assert actual == expected, (
        f"Expected {expected} seals in chain, got {actual}"
    )


def _seal_has_content_hash(assertion, report, repo):
    """Verify that all seals have content_hash populated."""
    log = repo.log()
    for seal in log:
        content_hash = seal.get("content_hash")
        assert content_hash is not None and len(content_hash) > 0, (
            f"Seal {seal['id'][:8]} missing content_hash"
        )


def _seal_has_chain_hash(assertion, report, repo):
    """Verify that all seals have chain_hash populated."""
    log = repo.log()
    for seal in log:
        chain_hash = seal.get("chain_hash")
        assert chain_hash is not None and len(chain_hash) > 0, (
            f"Seal {seal['id'][:8]} missing chain_hash"
        )


def _chain_no_failures(assertion, report, repo):
    result = _get_chain_result(repo)
    assert len(result["failures"]) == 0, (
        f"Chain has {len(result['failures'])} failures: "
        f"{[f['seal_id'][:8] for f in result['failures']]}"
    )
