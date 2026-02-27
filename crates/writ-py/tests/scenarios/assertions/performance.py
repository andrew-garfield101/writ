"""Performance assertion helpers for YAML scenario tests.

Checks timing thresholds against measured execution durations.
Timings are populated by the ScenarioRunner during execution.

Available timing keys:
  - agents_seconds: time spent executing agents (changes + seals)
  - convergence_seconds: time spent in converge_all
  - total_seconds: agents + convergence combined
"""


def check(assertion: dict, timings: dict) -> None:
    """Dispatch a performance assertion."""
    atype = assertion["type"]
    dispatch = {
        "max_duration": _max_duration,
        "max_convergence_duration": _max_convergence_duration,
        "max_agent_duration": _max_agent_duration,
    }
    handler = dispatch.get(atype)
    if handler is None:
        raise ValueError(f"Unknown performance assertion type: {atype}")
    handler(assertion, timings)


def _max_duration(assertion, timings):
    """Check that total scenario duration is under a threshold."""
    max_seconds = assertion["seconds"]
    actual = timings.get("total_seconds", 0)
    assert actual <= max_seconds, (
        f"Total duration {actual:.2f}s exceeded max {max_seconds}s "
        f"(agents: {timings.get('agents_seconds', 0):.2f}s, "
        f"convergence: {timings.get('convergence_seconds', 0):.2f}s)"
    )


def _max_convergence_duration(assertion, timings):
    """Check that convergence phase duration is under a threshold."""
    max_seconds = assertion["seconds"]
    actual = timings.get("convergence_seconds", 0)
    assert actual <= max_seconds, (
        f"Convergence duration {actual:.2f}s exceeded max {max_seconds}s"
    )


def _max_agent_duration(assertion, timings):
    """Check that agent execution phase duration is under a threshold."""
    max_seconds = assertion["seconds"]
    actual = timings.get("agents_seconds", 0)
    assert actual <= max_seconds, (
        f"Agent execution duration {actual:.2f}s exceeded max {max_seconds}s"
    )
