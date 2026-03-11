"""Live agent tests — launch real Claude Code sessions and score adoption.

Maps to S1.6 and S5.5 of the pre-beta testing guide.
These tests are expensive and require the claude CLI.
Run with: pytest crates/writ-py/tests/e2e/tests/ --live

All tests in this file are marked @pytest.mark.live and will be
skipped unless --live is passed.
"""

import shutil
import subprocess
import time
from pathlib import Path

import pytest

from helpers.cli import (
    AgentScorecard,
    git_log,
    writ_cmd,
)

pytestmark = [pytest.mark.live, pytest.mark.slow]


# Zero-writ prompt — no mention of writ at all. Agent should discover it
# from CLAUDE.md and use context/spec/seal/done organically.
PORTFOLIO_BLOG_PROMPT = (
    "You are working on an Astro 5 portfolio site in this directory. "
    "Add a blog section: create a content collection for blog posts with "
    "title, date, tags, and description fields. Write 3 sample posts about "
    "software testing topics. Create a blog listing page at /blog that shows "
    "posts sorted by date, and a dynamic route for individual post pages at "
    "/blog/[slug]. Use the existing BaseLayout and match the site's design "
    "patterns."
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _run_claude_agent(
    claude_bin: str,
    cwd: Path,
    prompt: str,
    timeout: int = 300,
) -> subprocess.CompletedProcess:
    """Launch a claude -p session with the given prompt."""
    return subprocess.run(
        [claude_bin, "-p", prompt, "--dangerously-skip-permissions"],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


@pytest.fixture
def claude_bin_required(claude_bin):
    """Skip test if claude CLI is not available."""
    if claude_bin is None:
        pytest.skip("claude CLI not found on PATH")
    return claude_bin


# ---------------------------------------------------------------------------
# S1.6: Single agent adoption
# ---------------------------------------------------------------------------

class TestSingleAgentAdoption:
    """Does a single Claude agent discover and use writ?"""

    def test_agent_discovers_writ(
        self,
        portfolio_project: Path,
        writ_bin: str,
        claude_bin_required: str,
    ):
        """S1.6: Agent reads CLAUDE.md and uses writ without prompting.

        This is the most important live test. A zero-writ prompt is given
        (no mention of writ). The agent should discover writ from CLAUDE.md
        and use context/spec/seal/done organically.
        """
        commits_before = len(git_log(portfolio_project))

        try:
            result = _run_claude_agent(
                claude_bin_required,
                portfolio_project,
                PORTFOLIO_BLOG_PROMPT,
                timeout=300,
            )
        except subprocess.TimeoutExpired:
            pytest.fail("Agent timed out after 300s")

        # Score adoption
        scorecard = AgentScorecard("claude-agent")
        score = scorecard.evaluate(
            writ_bin,
            portfolio_project,
            git_commits_before=commits_before,
        )

        assert score["total"] > 0, "Agent produced no writ activity at all"
        assert score["pass"], (
            f"Agent adoption score {score['total']}/100 below 70 threshold"
        )


# ---------------------------------------------------------------------------
# S5.5: Multi-agent adoption
# ---------------------------------------------------------------------------

class TestMultiAgentAdoption:
    """Multiple agents in the same project all discover writ."""

    def test_multi_agent_scenario(
        self,
        writ_bin: str,
        claude_bin_required: str,
        tmp_path: Path,
    ):
        """S5.5: 3-agent scenario — all should adopt writ.

        Requires testing/roundtrip/live/runner.py (internal tooling).
        Skips if runner is not available.
        """
        try:
            from live.runner import run_scenario
        except ImportError:
            pytest.skip(
                "Multi-agent runner not available "
                "(requires internal testing/roundtrip/)"
            )

        result = run_scenario(
            "api-3",
            timeout_per_agent=300,
        )

        assert result.chain_valid, "Seal chain should be valid"
        assert result.total_seals > 0, "Should have at least 1 seal"
        assert result.overall_score >= 50, (
            f"Overall score {result.overall_score}/100 too low"
        )
