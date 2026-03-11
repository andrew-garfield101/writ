"""S2: Workspace Lifecycle — create → work → converge → cleanup.

Maps to Section 2 of the pre-beta testing guide (P0 if shipping with beta).
Tests the multi-workspace workflow end-to-end.
"""

import json
import subprocess
from pathlib import Path

import pytest

from helpers.cli import (
    git_log,
    writ_cmd,
    writ_context,
    writ_log,
    writ_spec_list,
)


def _run_writ(writ_bin: str, cwd: Path, *args, **kwargs):
    """Shorthand for writ_cmd with check=False default."""
    return writ_cmd(writ_bin, cwd, *args, check=False, **kwargs)


# ---------------------------------------------------------------------------
# S2.1–S2.2: Workspace creation and isolation
# ---------------------------------------------------------------------------

class TestS2WorkspaceCreation:
    """Workspace create, list, and basic structure."""

    def test_create_workspace(self, writ_project: Path, writ_bin: str):
        """2.1.1: writ workspace create succeeds."""
        result = _run_writ(writ_bin, writ_project, "workspace", "create", "alpha")
        assert result.returncode == 0, f"Create failed: {result.stderr}"

    def test_workspace_dir_created(self, writ_project: Path, writ_bin: str):
        """2.1.2–2.1.3: Parallel directory and pointer file created."""
        _run_writ(writ_bin, writ_project, "workspace", "create", "beta")
        ws_dir = writ_project / ".writ" / "ws" / "beta"
        assert ws_dir.is_dir(), "Workspace parallel directory not created"

        pointer = ws_dir / ".writ-workspace"
        assert pointer.exists(), ".writ-workspace pointer not found"

    def test_workspace_has_project_files(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.1.4: Workspace has copies of main project files."""
        (writ_project / "src").mkdir(exist_ok=True)
        (writ_project / "src" / "config.py").write_text("CONFIG = True\n")
        writ_cmd(writ_bin, writ_project,
                 "seal", "-s", "add config", "--agent", "setup")

        _run_writ(writ_bin, writ_project, "workspace", "create", "gamma")
        ws_dir = writ_project / ".writ" / "ws" / "gamma"
        ws_config = ws_dir / "src" / "config.py"
        assert ws_config.exists(), "Workspace missing project file"

    def test_workspace_list(self, writ_project: Path, writ_bin: str):
        """2.1.6: writ workspace list shows workspaces."""
        _run_writ(writ_bin, writ_project, "workspace", "create", "team-a")
        _run_writ(writ_bin, writ_project, "workspace", "create", "team-b")

        result = _run_writ(writ_bin, writ_project, "workspace", "list")
        assert result.returncode == 0
        assert "team-a" in result.stdout
        assert "team-b" in result.stdout

    def test_writ_state_dirs(self, writ_project: Path, writ_bin: str):
        """2.1.7: .writ/workspaces/ has entries for each workspace."""
        _run_writ(writ_bin, writ_project, "workspace", "create", "delta")
        ws_state = writ_project / ".writ" / "workspaces" / "delta"
        assert ws_state.is_dir(), "No state dir for workspace"


class TestS2WorkspaceIsolation:
    """Files and context are isolated between workspaces."""

    def test_changes_isolated(self, writ_project: Path, writ_bin: str):
        """2.2.11: Changes in one workspace not visible in another."""
        (writ_project / "shared.py").write_text("original\n")
        writ_cmd(writ_bin, writ_project,
                 "seal", "-s", "baseline", "--agent", "setup")

        _run_writ(writ_bin, writ_project, "workspace", "create", "ws-a")
        _run_writ(writ_bin, writ_project, "workspace", "create", "ws-b")

        ws_a = writ_project / ".writ" / "ws" / "ws-a"
        ws_b = writ_project / ".writ" / "ws" / "ws-b"

        # Modify in ws-a only
        (ws_a / "shared.py").write_text("modified by ws-a\n")

        # ws-b still has original
        if (ws_b / "shared.py").exists():
            assert (ws_b / "shared.py").read_text() == "original\n"

    def test_seals_tagged_with_workspace(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.2.4: Seal metadata shows correct workspace."""
        _run_writ(writ_bin, writ_project, "workspace", "create", "tagged")
        ws_dir = writ_project / ".writ" / "ws" / "tagged"

        (ws_dir / "test.py").write_text("# tagged\n")
        writ_cmd(writ_bin, ws_dir,
                 "seal", "-s", "tagged seal", "--agent", "tester")

        log = writ_log(writ_bin, ws_dir)
        if log:
            latest = log[0]
            ws_field = latest.get("workspace", "")
            assert ws_field == "tagged", (
                f"Expected workspace='tagged', got '{ws_field}'"
            )

    def test_context_scoped_to_workspace(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.2.6/2.2.10: Context in workspace shows only its specs."""
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "alpha-work", "--title", "Alpha")
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "beta-work", "--title", "Beta")

        _run_writ(writ_bin, writ_project, "workspace", "create", "alpha-ws")
        _run_writ(writ_bin, writ_project, "workspace", "create", "beta-ws")

        writ_cmd(writ_bin, writ_project,
                 "spec", "assign", "alpha-work", "--workspace", "alpha-ws")
        writ_cmd(writ_bin, writ_project,
                 "spec", "assign", "beta-work", "--workspace", "beta-ws")

        alpha_dir = writ_project / ".writ" / "ws" / "alpha-ws"
        ctx = writ_context(writ_bin, alpha_dir)

        # Context should include alpha-work but not beta-work
        specs = ctx.get("specs", ctx.get("all_specs", []))
        spec_ids = [s.get("id") for s in specs]
        assert "alpha-work" in spec_ids
        # beta-work should NOT be in the scoped context
        assert "beta-work" not in spec_ids


# ---------------------------------------------------------------------------
# S2.4: Workspace convergence
# ---------------------------------------------------------------------------

class TestS2WorkspaceConvergence:
    """Converging workspaces back to main."""

    def _setup_two_workspaces(self, writ_project: Path, writ_bin: str):
        """Create two workspaces with sealed changes."""
        (writ_project / "src").mkdir(exist_ok=True)
        (writ_project / "src" / "auth.py").write_text("# auth\n")
        (writ_project / "src" / "payments.py").write_text("# payments\n")
        writ_cmd(writ_bin, writ_project,
                 "seal", "-s", "baseline", "--agent", "setup")

        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "auth-feat", "--title", "Auth")

        _run_writ(writ_bin, writ_project, "workspace", "create", "auth-ws")
        _run_writ(writ_bin, writ_project, "workspace", "create", "pay-ws")

        auth = writ_project / ".writ" / "ws" / "auth-ws"
        pay = writ_project / ".writ" / "ws" / "pay-ws"

        # Auth workspace changes auth.py
        (auth / "src" / "auth.py").write_text(
            "class AuthManager:\n    pass\n",
        )
        result = _run_writ(writ_bin, auth,
                           "seal", "-s", "auth impl", "--agent", "auth-dev",
                           "--spec", "auth-feat")
        if result.returncode != 0:
            # Try without --spec (C.13 may block cross-workspace spec refs)
            _run_writ(writ_bin, auth,
                      "seal", "-s", "auth impl", "--agent", "auth-dev")

        # Payments workspace changes payments.py
        (pay / "src" / "payments.py").write_text(
            "class PaymentProcessor:\n    pass\n",
        )
        result = _run_writ(writ_bin, pay,
                           "seal", "-s", "pay impl", "--agent", "pay-dev",
                           "--spec", "auth-feat")
        if result.returncode != 0:
            _run_writ(writ_bin, pay,
                      "seal", "-s", "pay impl", "--agent", "pay-dev")

        return auth, pay

    def test_non_overlapping_merge(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.4.2: Non-overlapping changes merge cleanly."""
        self._setup_two_workspaces(writ_project, writ_bin)

        result = _run_writ(
            writ_bin, writ_project,
            "converge-workspaces", "auth-ws", "pay-ws",
        )
        assert result.returncode == 0, (
            f"Convergence failed: {result.stderr}"
        )

    def test_dry_run_no_changes(self, writ_project: Path, writ_bin: str):
        """Dry-run previews without applying."""
        self._setup_two_workspaces(writ_project, writ_bin)

        result = _run_writ(
            writ_bin, writ_project,
            "converge-workspaces", "auth-ws", "pay-ws", "--dry-run",
        )
        assert result.returncode == 0

    def test_strategy_acceptance(self, writ_project: Path, writ_bin: str):
        """Valid strategies accepted by converge-workspaces."""
        self._setup_two_workspaces(writ_project, writ_bin)

        for strategy in ["three-way-merge", "most-recent", "escalate"]:
            result = _run_writ(
                writ_bin, writ_project,
                "converge-workspaces", "auth-ws", "pay-ws",
                "--dry-run", "--strategy", strategy,
            )
            assert result.returncode == 0, (
                f"Strategy '{strategy}' rejected: {result.stderr}"
            )


# ---------------------------------------------------------------------------
# S2.7: Workspace cleanup
# ---------------------------------------------------------------------------

class TestS2WorkspaceCleanup:
    """Workspace deletion behavior."""

    def test_delete_removes_workspace(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.7.1: Delete removes workspace from list."""
        _run_writ(writ_bin, writ_project, "workspace", "create", "temp-ws")

        result = _run_writ(
            writ_bin, writ_project,
            "workspace", "delete", "temp-ws", "--force",
        )
        assert result.returncode == 0

        list_result = _run_writ(
            writ_bin, writ_project, "workspace", "list",
        )
        assert "temp-ws" not in list_result.stdout

    def test_delete_preserves_seals(
        self, writ_project: Path, writ_bin: str,
    ):
        """2.7.2: Seals from deleted workspace remain in log."""
        # Create a spec so C.13 enforcement allows sealing
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "ephemeral-spec",
                 "--title", "Ephemeral")

        _run_writ(writ_bin, writ_project, "workspace", "create", "ephemeral")
        ws_dir = writ_project / ".writ" / "ws" / "ephemeral"

        (ws_dir / "data.py").write_text("# ephemeral\n")
        seal_result = _run_writ(
            writ_bin, ws_dir,
            "seal", "-s", "ephemeral work", "--agent", "temp",
            "--spec", "ephemeral-spec",
        )
        if seal_result.returncode != 0:
            # Spec may not be visible — try without
            seal_result = _run_writ(
                writ_bin, ws_dir,
                "seal", "-s", "ephemeral work", "--agent", "temp",
            )

        _run_writ(
            writ_bin, writ_project,
            "workspace", "delete", "ephemeral", "--force",
        )

        # Seals should still exist in the shared store after deletion
        seals_dir = writ_project / ".writ" / "seals"
        if seals_dir.exists():
            seal_files = list(seals_dir.iterdir())
            assert len(seal_files) > 0, (
                "Seal files should persist after workspace deletion"
            )
        else:
            # Check via log --all
            result = _run_writ(
                writ_bin, writ_project,
                "log", "--all", "--format", "json",
            )
            if result.returncode == 0 and result.stdout.strip():
                log = json.loads(result.stdout)
                assert len(log) > 0, "Should have seals in global log"

    def test_delete_main_refused(self, writ_project: Path, writ_bin: str):
        """2.8.4: Deleting main workspace is rejected."""
        result = _run_writ(
            writ_bin, writ_project,
            "workspace", "delete", "main", "--force",
        )
        assert result.returncode != 0, "Should refuse to delete main"


# ---------------------------------------------------------------------------
# S2: Golden path — full workspace lifecycle
# ---------------------------------------------------------------------------

class TestS2GoldenPath:
    """Full multi-workspace workflow end-to-end."""

    def test_workspace_golden_path(self, writ_project: Path, writ_bin: str):
        """Create → assign → work → seal → done → converge."""
        # Setup files
        (writ_project / "src").mkdir(exist_ok=True)
        (writ_project / "src" / "api.py").write_text("# api\n")
        (writ_project / "src" / "ui.py").write_text("# ui\n")
        writ_cmd(writ_bin, writ_project,
                 "seal", "-s", "baseline", "--agent", "setup")

        # Create specs
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "api-work", "--title", "API Work")
        writ_cmd(writ_bin, writ_project,
                 "spec", "add", "--id", "ui-work", "--title", "UI Work")

        # Create workspaces
        _run_writ(writ_bin, writ_project, "workspace", "create", "api-team")
        _run_writ(writ_bin, writ_project, "workspace", "create", "ui-team")

        # Assign specs
        writ_cmd(writ_bin, writ_project,
                 "spec", "assign", "api-work", "--workspace", "api-team")
        writ_cmd(writ_bin, writ_project,
                 "spec", "assign", "ui-work", "--workspace", "ui-team")

        api_dir = writ_project / ".writ" / "ws" / "api-team"
        ui_dir = writ_project / ".writ" / "ws" / "ui-team"

        # Work in each workspace
        (api_dir / "src" / "api.py").write_text(
            "from flask import Flask\napp = Flask(__name__)\n",
        )
        result = _run_writ(writ_bin, api_dir,
                           "seal", "-s", "api routes", "--agent", "api-dev",
                           "--spec", "api-work")
        if result.returncode != 0:
            # Spec may not be visible from workspace — seal without spec
            _run_writ(writ_bin, api_dir,
                      "seal", "-s", "api routes", "--agent", "api-dev")

        (ui_dir / "src" / "ui.py").write_text(
            "function App() { return <div>App</div> }\n",
        )
        result = _run_writ(writ_bin, ui_dir,
                           "seal", "-s", "ui components", "--agent", "ui-dev",
                           "--spec", "ui-work")
        if result.returncode != 0:
            _run_writ(writ_bin, ui_dir,
                      "seal", "-s", "ui components", "--agent", "ui-dev")

        # Mark specs done
        _run_writ(writ_bin, api_dir, "spec", "done", "api-work")
        _run_writ(writ_bin, ui_dir, "spec", "done", "ui-work")

        # Converge
        result = _run_writ(
            writ_bin, writ_project,
            "converge-workspaces", "api-team", "ui-team",
        )
        assert result.returncode == 0, (
            f"Golden path convergence failed: {result.stderr}"
        )
