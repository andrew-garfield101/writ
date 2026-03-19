"""Type stubs for the writ Python SDK (writ-vcs).

Provides IDE autocompletion and type checking for the writ native bindings.
"""

from typing import Any, Dict, List, Optional


class WritError(Exception):
    """Base exception for all writ errors."""
    ...


class AgentType:
    """Agent type enum: 'human' or 'agent'."""
    ...


class TaskStatus:
    """Task status enum: 'pending', 'in-progress', 'complete', 'blocked'."""
    ...


class SpecStatus:
    """Spec status enum: 'pending', 'in-progress', 'complete', 'blocked'."""
    ...


class Repository:
    """A writ repository handle. All operations are performed through this class."""

    @staticmethod
    def init(path: str) -> "Repository":
        """Initialize a new writ repository at the given path."""
        ...

    @staticmethod
    def open(path: str) -> "Repository":
        """Open an existing writ repository at the given path."""
        ...

    @staticmethod
    def install(path: str) -> Dict[str, Any]:
        """One-command setup: init + detect git + import baseline."""
        ...

    @staticmethod
    def remote_init(path: str) -> None:
        """Initialize a bare writ remote at the given path."""
        ...

    def state(self) -> Dict[str, Any]:
        """Get working directory state (tracked files, changes, etc.)."""
        ...

    def seal(
        self,
        summary: str,
        agent_id: str = "human",
        agent_type: str = "human",
        spec_id: Optional[str] = None,
        status: str = "complete",
        paths: Optional[List[str]] = None,
        tests_passed: Optional[int] = None,
        tests_failed: Optional[int] = None,
        linted: bool = False,
        allow_empty: bool = False,
    ) -> Dict[str, Any]:
        """Create a seal (checkpoint) from current changes.

        Args:
            summary: Description of the changes.
            agent_id: ID of the agent creating the seal.
            agent_type: 'human' or 'agent'.
            spec_id: Spec to scope the seal to. Auto-scopes if agent has 1 claimed spec.
            status: Task status ('in-progress', 'complete', etc.).
            paths: Selective seal — only include these file paths.
            tests_passed: Number of tests that passed.
            tests_failed: Number of tests that failed.
            linted: Whether the code was linted.
            allow_empty: Allow sealing with no changes.

        Returns:
            Dict with seal details (id, changes, hints, etc.).
        """
        ...

    def seal_with_check(
        self,
        summary: str,
        agent_id: str = "human",
        agent_type: str = "human",
        spec_id: Optional[str] = None,
        status: str = "complete",
        paths: Optional[List[str]] = None,
        tests_passed: Optional[int] = None,
        tests_failed: Optional[int] = None,
        linted: bool = False,
        allow_empty: bool = False,
    ) -> Dict[str, Any]:
        """Create a seal with conflict detection (checks if HEAD moved since last context).

        Same parameters as seal(). Returns dict with optional 'conflict_warning' field.
        """
        ...

    def log(
        self,
        limit: Optional[int] = None,
        format: str = "dict",
    ) -> Any:
        """List seals on the current branch.

        Args:
            limit: Max number of seals to return.
            format: Output format ('dict', 'toon', 'json').
        """
        ...

    def spec_log(
        self,
        spec_id: str,
        limit: Optional[int] = None,
        format: str = "dict",
    ) -> Any:
        """List seals for a specific spec.

        Args:
            spec_id: The spec to get seals for.
            limit: Max number of seals to return.
            format: Output format ('dict', 'toon', 'json').
        """
        ...

    def log_all(
        self,
        limit: Optional[int] = None,
        format: str = "dict",
    ) -> Any:
        """List all seals across all branches (including diverged)."""
        ...

    def diverged_branches(self) -> Dict[str, Any]:
        """Get diverged branch information (specs with seals off the HEAD chain)."""
        ...

    def spec_head(self, spec_id: str) -> Optional[str]:
        """Get the latest seal ID for a spec, or None if no seals exist."""
        ...

    def status(self, format: str = "dict") -> Any:
        """Get repository status (unsealed changes, tracked files, etc.)."""
        ...

    def diff(
        self,
        spec: Optional[str] = None,
        agent: Optional[str] = None,
        completed: bool = False,
        include_all: bool = False,
        file: Optional[str] = None,
        format: str = "dict",
    ) -> Any:
        """Show differences between working directory and last seal.

        Args:
            spec: Filter to changes for a specific spec.
            agent: Filter to changes by a specific agent.
            completed: Include completed specs.
            include_all: Include all changes across all specs.
            file: Show diff for a single file.
            format: Output format ('dict', 'toon', 'json').
        """
        ...

    def diff_seals(
        self,
        from_id: str,
        to_id: str,
        format: str = "dict",
    ) -> Any:
        """Show differences between two seals."""
        ...

    def diff_seal(
        self,
        seal_id: str,
        format: str = "dict",
    ) -> Any:
        """Show what changed in a specific seal."""
        ...

    def context(
        self,
        spec: Optional[str] = None,
        seal_limit: int = 10,
        status: Optional[str] = None,
        agent: Optional[str] = None,
        for_agent: Optional[str] = None,
        format: str = "dict",
    ) -> Any:
        """Get full project context (specs, seals, changes, integration risk).

        Args:
            spec: Scope context to a specific spec.
            seal_limit: Max seals to include in context.
            status: Filter specs by status.
            agent: Filter context for a specific agent.
            for_agent: Agent requesting context (records HEAD for conflict detection).
            format: Output format ('dict', 'toon', 'json').
        """
        ...

    def get_seal(self, seal_id: str) -> Dict[str, Any]:
        """Load a seal by ID. Returns the full seal dict."""
        ...

    def restore(self, seal_id: str) -> Dict[str, Any]:
        """Restore working directory to a previous seal's state."""
        ...

    def add_spec(
        self,
        id: Optional[str] = None,
        title: str = "",
        description: str = "",
        acceptance_criteria: Optional[List[str]] = None,
        design_notes: Optional[List[str]] = None,
        tech_stack: Optional[List[str]] = None,
    ) -> None:
        """Add a new spec (task definition).

        Args:
            id: Spec ID. If omitted, auto-generates a hash-based ID from the title.
            title: Human-readable title for the spec.
            description: Detailed description of the work.
            acceptance_criteria: List of acceptance criteria.
            design_notes: List of design notes.
            tech_stack: List of technologies used.
        """
        ...

    def resolve_spec(self, input: str) -> Dict[str, Any]:
        """Resolve a spec by ID, ID prefix, slug, or slug prefix.

        Raises WritError if no match or multiple matches (ambiguous).
        """
        ...

    def get_spec(self, id: str) -> Dict[str, Any]:
        """Get a spec by exact ID. Returns the full spec dict."""
        ...

    def update_spec(
        self,
        id: str,
        status: Optional[str] = None,
        depends_on: Optional[List[str]] = None,
        file_scope: Optional[List[str]] = None,
        acceptance_criteria: Optional[List[str]] = None,
        design_notes: Optional[List[str]] = None,
        tech_stack: Optional[List[str]] = None,
    ) -> Dict[str, Any]:
        """Update a spec's fields. Only provided fields are changed."""
        ...

    def list_specs(self, format: str = "dict") -> Any:
        """List all specs in the repository."""
        ...

    def converge(self, left_spec: str, right_spec: str) -> Dict[str, Any]:
        """Analyze convergence between two specs (dry run). Returns a report."""
        ...

    def apply_convergence(
        self,
        report: Dict[str, Any],
        resolutions: Optional[Dict[str, str]] = None,
    ) -> None:
        """Apply a convergence report to disk.

        Args:
            report: The convergence report dict from converge().
            resolutions: Manual resolutions for conflicted files.
        """
        ...

    def converge_all(
        self,
        strategy: str = "escalate",
        apply: bool = False,
    ) -> Dict[str, Any]:
        """Converge all diverged specs.

        Args:
            strategy: 'escalate', 'three-way-merge', or 'most-recent'.
            apply: If True, apply changes to disk. If False, dry run.
        """
        ...

    def converge_from_seal_trees(
        self,
        spec_ids: List[str],
        strategy: str = "escalate",
    ) -> Dict[str, Any]:
        """Converge specific specs using seal-tree convergence (v3)."""
        ...

    def finalize_convergence(self) -> Dict[str, Any]:
        """Finalize any pending convergence results (materialize shadow merges)."""
        ...

    def materialize_convergence(self, report: Dict[str, Any]) -> None:
        """Materialize a convergence report to disk."""
        ...

    def archive_orphaned_specs(self) -> Dict[str, Any]:
        """Archive specs that have no seals and are orphaned. Returns archived IDs."""
        ...

    def convergence_eligible_specs(self, format: str = "dict") -> Any:
        """List specs eligible for convergence (uncommitted, post-epoch)."""
        ...

    def bridge_import(
        self,
        git_ref: str = "HEAD",
        agent_id: str = "bridge",
        agent_type: str = "agent",
    ) -> Dict[str, Any]:
        """Import state from a git ref into writ."""
        ...

    def bridge_export(self, branch: str = "writ/export") -> Dict[str, Any]:
        """Export writ state to a git branch."""
        ...

    def bridge_status(self) -> Dict[str, Any]:
        """Get bridge sync status between writ and git."""
        ...

    def summary(self) -> Dict[str, Any]:
        """Generate a project summary (all specs, seals, completion status)."""
        ...

    def remote_add(self, name: str, path: str) -> None:
        """Add a remote to this repository."""
        ...

    def remote_remove(self, name: str) -> None:
        """Remove a remote from this repository."""
        ...

    def remote_list(self) -> Dict[str, Any]:
        """List all configured remotes."""
        ...

    def push(self, remote: str = "origin") -> Dict[str, Any]:
        """Push seals and specs to a remote."""
        ...

    def pull(self, remote: str = "origin") -> Dict[str, Any]:
        """Pull seals and specs from a remote."""
        ...

    def remote_status(self, remote: str = "origin") -> Dict[str, Any]:
        """Check sync status with a remote."""
        ...

    def verify_chain(self, use_convergence_key: bool = False) -> Dict[str, Any]:
        """Verify the cryptographic seal chain integrity."""
        ...

    def verify_seal(
        self,
        seal_id: str,
        use_convergence_key: bool = False,
    ) -> Dict[str, Any]:
        """Verify a single seal's cryptographic signature."""
        ...

    def storage_report(self) -> Dict[str, Any]:
        """Get storage usage report (object counts, sizes, compression)."""
        ...

    def gc_status(self) -> Dict[str, Any]:
        """Get garbage collection status and lifecycle states."""
        ...

    def gc_dry_run(self) -> Dict[str, Any]:
        """Preview what garbage collection would clean up."""
        ...

    def gc(self) -> Dict[str, Any]:
        """Run garbage collection on the repository."""
        ...

    def cancel_spec(self, spec_id: str) -> None:
        """Cancel a spec (marks it as cancelled, preserves seal history)."""
        ...

    def complete_spec(self, spec_id: str) -> None:
        """Complete a spec (marks commit state as ready for git commit)."""
        ...

    def finish(
        self,
        strategy: str = "single",
        message: Optional[str] = None,
        dry_run: bool = False,
        specs: Optional[List[str]] = None,
    ) -> Dict[str, Any]:
        """Finish completed work: converge + commit to git.

        Args:
            strategy: Commit strategy ('single', 'per-spec').
            message: Custom commit message (for single strategy).
            dry_run: Preview without committing.
            specs: Specific spec IDs to finish. If None, finishes all complete specs.
        """
        ...

    def resolve_spec_for_agent(
        self,
        agent_id: str,
        spec_id: Optional[str] = None,
    ) -> str:
        """Resolve the spec for an agent (auto-scope if agent has 1 claimed spec).

        Args:
            agent_id: The agent requesting resolution.
            spec_id: Explicit spec ID. If None, auto-detects.

        Returns:
            The resolved spec ID string.
        """
        ...

    def spec_done(
        self,
        spec_id: Optional[str] = None,
        summary: Optional[str] = None,
        agent_id: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Mark a spec as done (status -> Complete).

        Args:
            spec_id: Spec to complete. If None, auto-scopes via agent_id.
            summary: Optional completion summary.
            agent_id: Agent completing the spec (for auto-scoping).
        """
        ...

    def reopen_spec(self, spec_id: str) -> None:
        """Reopen a completed spec (returns it to in-progress status)."""
        ...

    def spec_claim(self, spec_id: str, agent_id: str) -> None:
        """Claim a spec for an agent (assigns ownership)."""
        ...

    def propose(
        self,
        spec_ids: List[str],
        message: str,
        proposed_by: str = "cli",
        strategy: str = "single",
    ) -> Dict[str, Any]:
        """Propose a set of specs for finishing (auto workflow mode)."""
        ...

    def list_proposals(self) -> Dict[str, Any]:
        """List all pending proposals."""
        ...

    def accept_proposal(self, proposal_id: str) -> Dict[str, Any]:
        """Accept a pending proposal (commits to git)."""
        ...

    def reject_proposal(self, proposal_id: str) -> Dict[str, Any]:
        """Reject a pending proposal."""
        ...

    def doctor(self) -> Dict[str, Any]:
        """Run diagnostics on the repository (integrity checks)."""
        ...

    def version_info(self) -> Dict[str, Any]:
        """Get version information for the writ library."""
        ...

    def create_task(
        self,
        title: str,
        id: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Create a task (spec + workspace + assignment in one command)."""
        ...

    def plan(self, tasks: List[str]) -> List[Dict[str, Any]]:
        """Create multiple specs from a list of task descriptions.

        Returns a list of dicts with spec_id and title for each created spec.
        """
        ...

    def active_workspace(self) -> str:
        """Get the name of the currently active workspace."""
        ...

    def create_workspace(
        self,
        name: str,
        path: Optional[str] = None,
        from_workspace: Optional[str] = None,
    ) -> Dict[str, Any]:
        """Create a new workspace directory."""
        ...

    def delete_workspace(self, name: str, keep_files: bool = False) -> None:
        """Delete a workspace.

        Args:
            name: Workspace name to delete.
            keep_files: If True, preserve workspace files on disk.
        """
        ...

    def list_workspaces(self) -> Dict[str, Any]:
        """List all workspaces in the repository."""
        ...

    def assign_spec_to_workspace(self, spec_id: str, workspace: str) -> None:
        """Assign a spec to a workspace."""
        ...

    def unassign_spec_from_workspace(self, spec_id: str) -> None:
        """Remove a spec's workspace assignment."""
        ...

    def converge_workspaces(
        self,
        workspace_names: List[str],
        strategy: str = "three-way-merge",
        dry_run: bool = False,
    ) -> Dict[str, Any]:
        """Converge changes across workspaces."""
        ...


def detect_frameworks(path: str) -> Dict[str, Any]:
    """Detect agent frameworks in a project directory."""
    ...


def install_hooks(path: str) -> Dict[str, Any]:
    """Install writ hooks for all detected agent frameworks."""
    ...


def generate_skills(path: str) -> Dict[str, Any]:
    """Generate writ skill directories in .claude/skills/.

    Returns:
        Dict with 'created', 'updated', 'skipped' counts.
    """
    ...


def remove_skills(path: str) -> List[str]:
    """Remove writ skill directories from .claude/skills/.

    Returns:
        List of removed skill directory names.
    """
    ...


__version__: str
"""The writ package version string."""
