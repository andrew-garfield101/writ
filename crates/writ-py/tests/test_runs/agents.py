"""Agent runners for test runs.

Two modes:
  - ScriptedAgentRunner: Python functions that make deterministic changes + seal.
    Fast (~1s per agent), reproducible, for framework development and CI.

  - LiveAgentRunner: Real Claude Code sessions via `claude -p`.
    Non-deterministic (~5-10 min per agent), exploratory, for per-sprint TRs.
"""

import importlib.util
import os
import subprocess
import time
from abc import ABC, abstractmethod
from pathlib import Path
from typing import Any

from .report import AgentResult


class AgentRunner(ABC):
    """Base class for agent runners."""

    @abstractmethod
    def run(
        self, agent_def: dict, workspace: Path, repo: Any
    ) -> AgentResult:
        """Execute an agent's work and return the result."""
        ...


class ScriptedAgentRunner(AgentRunner):
    """Run agents as Python functions from a TR's agents_scripted.py module.

    Each agent function receives (workspace, repo) and should:
    1. Make file changes in the workspace
    2. Seal the work via repo.seal()
    3. Return a list of changed file paths
    """

    def __init__(self, tr_dir: Path):
        self.tr_dir = tr_dir
        self._module = None

    def _load_module(self):
        """Lazy-load the agents_scripted.py module."""
        if self._module is not None:
            return

        script_path = self.tr_dir / "agents_scripted.py"
        if not script_path.exists():
            raise FileNotFoundError(
                f"Scripted agents not found: {script_path}. "
                f"Create agents_scripted.py with a function per agent."
            )

        spec = importlib.util.spec_from_file_location("agents_scripted", script_path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self._module = module

    def run(
        self, agent_def: dict, workspace: Path, repo: Any
    ) -> AgentResult:
        self._load_module()

        agent_name = agent_def["name"]
        spec_id = agent_def.get("spec", agent_name)

        # Convert agent name to function name (e.g., "backend-dev" -> "backend_dev")
        fn_name = agent_name.replace("-", "_")
        fn = getattr(self._module, fn_name, None)
        if fn is None:
            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=False,
                error=f"No function '{fn_name}' in agents_scripted.py",
            )

        try:
            changed_files = fn(workspace, repo)
            if changed_files is None:
                changed_files = []

            # Get the latest seal ID
            log = repo.log(limit=1)
            seal_id = log[0]["id"] if log else ""

            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=True,
                seal_id=seal_id,
                files_changed=changed_files,
            )
        except Exception as e:
            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=False,
                error=str(e),
            )


class LiveAgentRunner(AgentRunner):
    """Run agents as real Claude Code sessions via `claude -p`.

    Reads prompts from tr_dir/prompts/{agent_name}.md and spawns
    a Claude Code subprocess scoped to the workspace directory.
    Captures output to logs/ for post-run analysis.

    By default, agents run with Claude Code's normal permission model —
    you'll see permission prompts in the terminal. Use trust_agents=True
    (--trust-live-agents CLI flag) to bypass permissions, but only in
    isolated/disposable environments.
    """

    def __init__(self, tr_dir: Path, trust_agents: bool = False):
        self.tr_dir = tr_dir
        self.trust_agents = trust_agents
        self.logs_dir = tr_dir / "logs"
        self.logs_dir.mkdir(exist_ok=True)

    def run(
        self, agent_def: dict, workspace: Path, repo: Any
    ) -> AgentResult:
        agent_name = agent_def["name"]
        spec_id = agent_def.get("spec", agent_name)

        # Load prompt
        prompt_path = self.tr_dir / "prompts" / f"{agent_name}.md"
        if not prompt_path.exists():
            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=False,
                error=f"Prompt not found: {prompt_path}",
            )

        prompt = prompt_path.read_text()

        # Substitute workspace path into prompt
        prompt = prompt.replace("__WORKSPACE__", str(workspace))

        # Log file for this agent
        timestamp = time.strftime("%Y%m%d_%H%M%S")
        log_path = self.logs_dir / f"agent-{agent_name}-{timestamp}.log"

        # Build command — always scope to workspace directory
        cmd = [
            "claude",
            "-p", prompt,
            "--output-format", "text",
            "--directory", str(workspace),
        ]

        # Only bypass permissions when explicitly requested
        if self.trust_agents:
            cmd.extend([
                "--dangerously-skip-permissions",
                "--permission-mode", "bypassPermissions",
            ])

        try:
            # When not trusting agents, don't capture output — let the
            # user see and approve permission prompts in real-time.
            if self.trust_agents:
                result = subprocess.run(
                    cmd,
                    cwd=str(workspace),
                    capture_output=True,
                    text=True,
                    timeout=600,  # 10 minute timeout per agent
                )
                stdout = result.stdout
                stderr = result.stderr
                returncode = result.returncode
            else:
                # Stream output to terminal so user can interact with
                # permission prompts. Also tee to log file.
                result = subprocess.run(
                    cmd,
                    cwd=str(workspace),
                    timeout=600,
                )
                stdout = "(streamed to terminal)"
                stderr = ""
                returncode = result.returncode

            # Write log
            with open(log_path, "w") as f:
                f.write(f"=== Agent: {agent_name} ===\n")
                f.write(f"Spec: {spec_id}\n")
                f.write(f"Exit code: {returncode}\n")
                f.write(f"Trust mode: {self.trust_agents}\n")
                f.write(f"{'=' * 50}\n\n")
                f.write("=== STDOUT ===\n")
                f.write(stdout)
                f.write("\n\n=== STDERR ===\n")
                f.write(stderr)

            if returncode != 0:
                return AgentResult(
                    agent_id=agent_name,
                    spec_id=spec_id,
                    success=False,
                    error=f"claude exited with code {returncode}. Log: {log_path}",
                )

            # Determine changed files by checking seal
            log = repo.log(limit=1)
            seal_id = log[0]["id"] if log else ""

            # Detect files the agent touched
            changed = []
            for root, _dirs, files in os.walk(workspace):
                rel_root = os.path.relpath(root, workspace)
                if rel_root.startswith((".writ", ".git")):
                    continue
                for fname in files:
                    changed.append(os.path.relpath(os.path.join(root, fname), workspace))

            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=True,
                seal_id=seal_id,
                files_changed=changed,
            )

        except subprocess.TimeoutExpired:
            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=False,
                error=f"Agent timed out after 600s. Log: {log_path}",
            )
        except FileNotFoundError:
            return AgentResult(
                agent_id=agent_name,
                spec_id=spec_id,
                success=False,
                error="'claude' command not found. Is Claude Code installed?",
            )
