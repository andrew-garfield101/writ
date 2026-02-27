"""Shared fixtures for writ Python binding tests."""

import os
import tempfile

import pytest
import writ


@pytest.fixture
def tmp_repo(tmp_path):
    """Create a temporary writ repository and return the repo + path."""
    repo = writ.Repository.init(str(tmp_path))
    return repo, tmp_path


@pytest.fixture
def sealed_repo(tmp_path):
    """Repository with a baseline seal and tracked file."""
    repo = writ.Repository.init(str(tmp_path))
    (tmp_path / "base.py").write_text("# baseline\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )
    return repo, tmp_path


@pytest.fixture
def diverged_repo(tmp_path):
    """Repository with two truly diverged spec branches.

    Uses the "seal A, seal B, seal A again" pattern to fork the chain:
    1. Baseline seal
    2. Agent A seals under spec-a (HEAD=seal1, spec-a=seal1)
    3. Agent B seals under spec-b (HEAD=seal2, spec-b=seal2)
    4. Agent A seals under spec-a again — resolve_parent() uses spec-a's
       head (seal1) as parent, NOT global HEAD (seal2). This puts seal2
       off the HEAD chain, making spec-b genuinely diverged.
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline
    (tmp_path / "base.py").write_text("# baseline\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    # Create specs
    repo.add_spec(id="spec-a", title="Feature A")
    repo.add_spec(id="spec-b", title="Feature B")

    # Step 1: Agent A writes module_a.py, seals under spec-a
    (tmp_path / "module_a.py").write_text("def feature_a():\n    return 'A'\n")
    repo.seal(
        summary="agent-a work",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="spec-a",
        status="in-progress",
    )

    # Step 2: Agent B writes module_b.py, seals under spec-b
    (tmp_path / "module_b.py").write_text("def feature_b():\n    return 'B'\n")
    repo.seal(
        summary="agent-b work",
        agent_id="agent-b",
        agent_type="agent",
        spec_id="spec-b",
        status="in-progress",
    )

    # Step 3: Agent A seals under spec-a AGAIN — this forks the chain!
    # resolve_parent(spec-a) returns seal1, not seal2 (global HEAD).
    # New seal's parent = seal1, so HEAD chain skips seal2.
    # spec-b's tip (seal2) is now off the HEAD chain → diverged.
    (tmp_path / "module_a.py").write_text("def feature_a():\n    return 'A v2'\n")
    repo.seal(
        summary="agent-a continued",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="spec-a",
        status="in-progress",
    )

    return repo, tmp_path


@pytest.fixture
def conflicting_repo(tmp_path):
    """Repository with two truly diverged specs that modify the same file.

    Uses "seal A, seal B, seal A again" to create real divergence,
    with both specs modifying shared.py at the same line.
    """
    repo = writ.Repository.init(str(tmp_path))

    # Baseline with shared file
    (tmp_path / "shared.py").write_text("line1\noriginal\nline3\n")
    repo.seal(
        summary="baseline",
        agent_id="setup",
        agent_type="agent",
        status="in-progress",
    )

    # Create specs
    repo.add_spec(id="spec-a", title="Feature A")
    repo.add_spec(id="spec-b", title="Feature B")

    # Step 1: Agent A modifies shared.py, seals under spec-a
    (tmp_path / "shared.py").write_text("line1\nleft_change\nline3\n")
    repo.seal(
        summary="agent-a modifies shared",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="spec-a",
        status="in-progress",
    )

    # Step 2: Agent B modifies shared.py differently, seals under spec-b
    (tmp_path / "shared.py").write_text("line1\nright_change\nline3\n")
    repo.seal(
        summary="agent-b modifies shared",
        agent_id="agent-b",
        agent_type="agent",
        spec_id="spec-b",
        status="in-progress",
    )

    # Step 3: Agent A seals under spec-a AGAIN — forks the chain!
    # Restore spec-a's file state and add a marker file for a non-empty seal.
    (tmp_path / "shared.py").write_text("line1\nleft_change\nline3\n")
    (tmp_path / "spec_a_extra.txt").write_text("extra work\n")
    repo.seal(
        summary="agent-a continued",
        agent_id="agent-a",
        agent_type="agent",
        spec_id="spec-a",
        status="in-progress",
    )

    return repo, tmp_path
