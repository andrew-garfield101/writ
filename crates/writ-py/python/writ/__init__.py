"""writ — AI-native version control for agentic workflows.

Install: pip install writ-vcs
Docs:    https://github.com/agarfield/writ

Basic usage::

    import writ

    repo = writ.Repository.init(".")
    repo.seal(summary="initial work", agent_id="my-agent", agent_type="agent")
    ctx = repo.context()

One-command setup for existing projects::

    result = writ.Repository.install(".")

Agent SDK for higher-level workflows::

    from writ.sdk import Agent, Phase, Pipeline
"""

from writ._native import (
    Repository,
    AgentType,
    TaskStatus,
    SpecStatus,
    WritError,
    detect_frameworks,
    install_hooks,
)

__version__ = "0.1.0"

__all__ = [
    "Repository",
    "AgentType",
    "TaskStatus",
    "SpecStatus",
    "WritError",
    "detect_frameworks",
    "install_hooks",
    "__version__",
    "sdk",
]
