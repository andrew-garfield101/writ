"""
writ SDK — higher-level Python toolkit for agent workflows.

Wraps the low-level writ bindings into ergonomic abstractions:
  - Agent: context manager for a single agent session
  - Phase: context manager for a pipeline phase boundary
  - Pipeline: decorator-based autonomous pipeline runner

Zero external dependencies beyond the writ package itself.
"""

from __future__ import annotations

import json
from contextlib import contextmanager
from typing import Any, Callable, Dict, List, Optional

import writ


class Agent:
    """
    High-level wrapper for an agent working within a writ repository.

    Usage::

        with Agent("implementer", spec_id="auth") as agent:
            ctx = agent.context
            # ... do work ...
            agent.seal("implemented token refresh")
    """

    def __init__(
        self,
        agent_id: str,
        *,
        path: str = ".",
        agent_type: str = "agent",
        spec_id: Optional[str] = None,
        seal_limit: int = 10,
    ):
        self.agent_id = agent_id
        self.agent_type = agent_type
        self.spec_id = spec_id
        self.seal_limit = seal_limit
        self._path = path
        self._repo: Optional[writ.Repository] = None
        self._context: Optional[Dict[str, Any]] = None

    def __enter__(self) -> "Agent":
        self.open()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        return False

    def open(self) -> "Agent":
        """Open the repository and load initial context."""
        self._repo = writ.Repository.open(self._path)
        self._context = None
        return self

    @property
    def repo(self) -> writ.Repository:
        if self._repo is None:
            raise RuntimeError("Agent not opened — use 'with Agent(...) as a:' or call agent.open()")
        return self._repo

    @property
    def context(self) -> Dict[str, Any]:
        """Lazily loaded, cached context. Call refresh() to reload."""
        if self._context is None:
            self._context = self.repo.context(
                spec=self.spec_id,
                seal_limit=self.seal_limit,
                agent=self.agent_id,
            )
        return self._context

    def refresh(self) -> Dict[str, Any]:
        """Force-reload context from the repository."""
        self._context = None
        return self.context

    @property
    def seal_nudge(self) -> Optional[Dict[str, Any]]:
        """Returns the seal nudge if unsealed changes exist, else None."""
        return self.context.get("seal_nudge")

    def should_seal(self) -> bool:
        """True if the working directory has unsealed changes."""
        ctx = self.repo.context(spec=self.spec_id, seal_limit=1)
        return "seal_nudge" in ctx

    def seal(
        self,
        summary: str,
        *,
        status: str = "in-progress",
        tests_passed: Optional[int] = None,
        tests_failed: Optional[int] = None,
        linted: bool = False,
        allow_empty: bool = False,
    ) -> Dict[str, Any]:
        """Create a seal with this agent's identity pre-filled."""
        kwargs: Dict[str, Any] = dict(
            summary=summary,
            agent_id=self.agent_id,
            agent_type=self.agent_type,
            status=status,
            allow_empty=allow_empty,
        )
        if self.spec_id is not None:
            kwargs["spec_id"] = self.spec_id
        if tests_passed is not None:
            kwargs["tests_passed"] = tests_passed
        if tests_failed is not None:
            kwargs["tests_failed"] = tests_failed
        if linted:
            kwargs["linted"] = True
        result = self.repo.seal(**kwargs)
        self._context = None
        return result

    def checkpoint(self, summary: str) -> Dict[str, Any]:
        """Quick seal with sensible defaults (in-progress, no verification)."""
        return self.seal(summary, status="in-progress")

    def history(
        self,
        *,
        status: Optional[str] = None,
        agent: Optional[str] = None,
        seal_limit: int = 20,
    ) -> List[Dict[str, Any]]:
        """Query seal history with optional filters."""
        ctx = self.repo.context(
            spec=self.spec_id,
            seal_limit=seal_limit,
            status=status,
            agent=agent,
        )
        return ctx.get("recent_seals", [])

    def restore_last_good(self) -> Optional[Dict[str, Any]]:
        """Restore to the most recent seal where tests_failed == 0."""
        seals = self.repo.log()
        for s in seals:
            v = s.get("verification", {})
            if v.get("tests_failed") == 0:
                result = self.repo.restore(s["id"])
                self._context = None
                return result
        return None

    def handoff_summary(self) -> Dict[str, Any]:
        """Generate a structured summary for passing to another agent."""
        ctx = self.refresh()
        summary: Dict[str, Any] = {
            "agent_id": self.agent_id,
            "spec_id": self.spec_id,
            "working_state": ctx.get("working_state"),
            "recent_seals": ctx.get("recent_seals", [])[:5],
        }
        if ctx.get("seal_nudge"):
            summary["seal_nudge"] = ctx["seal_nudge"]
        if ctx.get("spec_progress"):
            summary["spec_progress"] = ctx["spec_progress"]
        if ctx.get("dependency_status"):
            summary["dependency_status"] = ctx["dependency_status"]
        return summary


class Phase:
    """
    Context manager for a single pipeline phase.

    Usage::

        agent = Agent("orchestrator", spec_id="auth")
        agent.open()

        with Phase(agent, "implementation", agent_id="implementer") as p:
            # ... do work ...
            p.seal("built auth module", tests_passed=12)
    """

    def __init__(
        self,
        agent: Agent,
        name: str,
        *,
        agent_id: Optional[str] = None,
    ):
        self._agent = agent
        self.name = name
        self._override_agent_id = agent_id

    def __enter__(self) -> "Phase":
        self._original_agent_id = self._agent.agent_id
        if self._override_agent_id is not None:
            self._agent.agent_id = self._override_agent_id
        self._agent.refresh()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self._agent.agent_id = self._original_agent_id
        return False

    @property
    def context(self) -> Dict[str, Any]:
        return self._agent.context

    def seal(
        self,
        summary: str,
        *,
        status: str = "in-progress",
        tests_passed: Optional[int] = None,
        tests_failed: Optional[int] = None,
        linted: bool = False,
    ) -> Dict[str, Any]:
        """Seal with the phase's agent identity."""
        return self._agent.seal(
            summary,
            status=status,
            tests_passed=tests_passed,
            tests_failed=tests_failed,
            linted=linted,
        )

    def complete_spec(self, summary: Optional[str] = None) -> Dict[str, Any]:
        """Mark the spec as complete and seal the transition."""
        spec_id = self._agent.spec_id
        if spec_id is None:
            raise ValueError("no spec_id set on agent — cannot complete spec")
        self._agent.repo.update_spec(spec_id, status="complete")
        msg = summary or f"finalized spec {spec_id}"
        return self._agent.seal(msg, status="complete", allow_empty=True)


FULL_PHASES = ("specification", "design", "implementation", "testing", "review", "finalize")
COMPACT_PHASES = ("spec-and-design", "implementation", "test-and-review", "finalize")


def estimate_complexity(description: str, file_scope: Optional[List[str]] = None) -> str:
    """Estimate feature complexity from its description and scope.

    Returns "full" for complex features (6 phases) or "compact" for
    simple ones (4 collapsed phases).

    Heuristics:
    - More than 5 scoped files -> full
    - Description mentions architecture, migration, refactor -> full
    - Description under 200 chars with no complexity signals -> compact
    """
    complex_signals = [
        "architect", "migration", "refactor", "redesign",
        "infrastructure", "database", "schema", "security audit",
        "authentication", "authorization", "multi-",
    ]

    desc_lower = description.lower()
    if any(s in desc_lower for s in complex_signals):
        return "full"

    if file_scope and len(file_scope) > 5:
        return "full"

    if len(description) > 500:
        return "full"

    return "compact"


class Pipeline:
    """
    Decorator-based pipeline for autonomous spec-driven workflows.

    Supports two modes:
    - **full** (6 phases): spec -> design -> implement -> test -> review -> finalize
    - **compact** (4 phases): spec+design -> implement -> test+review -> finalize

    The mode is chosen automatically based on feature complexity, or can
    be set explicitly via the ``mode`` parameter.

    Usage::

        pipeline = Pipeline(
            spec_id="auth-migration",
            spec_title="OAuth2 Migration",
            spec_description="Migrate authentication to OAuth2 flow",
        )

        @pipeline.phase("specification", agent_id="spec-writer")
        def spec(ctx):
            return {"summary": "defined auth spec"}

        @pipeline.phase("implementation", agent_id="implementer")
        def implement(ctx):
            return {"summary": "built auth module", "tests_passed": 12}

        results = pipeline.run(path=".")

    Adaptive (auto-detect complexity)::

        pipeline = Pipeline(
            spec_id="add-footer-link",
            spec_title="Add Footer Link",
            spec_description="Add a privacy policy link to the footer",
            mode="auto",  # default: auto-detects compact vs full
        )
    """

    def __init__(
        self,
        spec_id: str,
        spec_title: str,
        spec_description: str = "",
        *,
        mode: str = "auto",
        file_scope: Optional[List[str]] = None,
    ):
        self.spec_id = spec_id
        self.spec_title = spec_title
        self.spec_description = spec_description
        self.file_scope = file_scope
        self._phases: List[_PhaseRegistration] = []

        if mode == "auto":
            self.mode = estimate_complexity(spec_description, file_scope)
        elif mode in ("full", "compact"):
            self.mode = mode
        else:
            raise ValueError(f"mode must be 'auto', 'full', or 'compact', got '{mode}'")

    @property
    def phase_names(self) -> tuple:
        """The phase sequence for this pipeline's mode."""
        return FULL_PHASES if self.mode == "full" else COMPACT_PHASES

    def phase(
        self,
        name: str,
        *,
        agent_id: Optional[str] = None,
    ) -> Callable:
        """Decorator to register a phase function."""
        def decorator(fn: Callable) -> Callable:
            self._phases.append(_PhaseRegistration(
                name=name,
                agent_id=agent_id or name,
                fn=fn,
            ))
            return fn
        return decorator

    def run(self, *, path: str = ".") -> Dict[str, Any]:
        """Execute all registered phases in order."""
        agent = Agent("pipeline", path=path, spec_id=self.spec_id)
        agent.open()

        try:
            spec_kwargs: Dict[str, Any] = dict(
                id=self.spec_id,
                title=self.spec_title,
                description=self.spec_description,
            )
            agent.repo.add_spec(**spec_kwargs)
            if self.file_scope:
                agent.repo.update_spec(self.spec_id, file_scope=self.file_scope)
        except writ.WritError as e:
            if "already exists" not in str(e).lower():
                raise

        results: Dict[str, Any] = {
            "phases": [],
            "spec_id": self.spec_id,
            "mode": self.mode,
        }

        for registration in self._phases:
            with Phase(agent, registration.name, agent_id=registration.agent_id) as p:
                phase_result = registration.fn(p.context)

                if not isinstance(phase_result, dict):
                    phase_result = {"summary": str(phase_result)}

                summary = phase_result.get("summary", f"completed {registration.name}")
                seal_kwargs: Dict[str, Any] = {}
                if "tests_passed" in phase_result:
                    seal_kwargs["tests_passed"] = phase_result["tests_passed"]
                if "tests_failed" in phase_result:
                    seal_kwargs["tests_failed"] = phase_result["tests_failed"]
                if "linted" in phase_result:
                    seal_kwargs["linted"] = phase_result["linted"]

                seal = p.seal(summary, **seal_kwargs)
                results["phases"].append({
                    "name": registration.name,
                    "agent_id": registration.agent_id,
                    "seal_id": seal.get("id", ""),
                    "summary": summary,
                })

        with Phase(agent, "finalize", agent_id="integrator") as p:
            seal = p.complete_spec()
            results["phases"].append({
                "name": "finalize",
                "agent_id": "integrator",
                "seal_id": seal.get("id", ""),
                "summary": f"finalized spec {self.spec_id}",
            })

        results["seal_history"] = agent.repo.log()
        return results


class _PhaseRegistration:
    """Internal: a registered phase function."""

    __slots__ = ("name", "agent_id", "fn")

    def __init__(self, name: str, agent_id: str, fn: Callable):
        self.name = name
        self.agent_id = agent_id
        self.fn = fn
