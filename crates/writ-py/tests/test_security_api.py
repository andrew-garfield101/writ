"""Contract tests for writ security Python API.

Validates that the Python bindings expose the correct return structures
for security-related operations: verify_chain, verify_seal, and
cryptographic integrity fields on seals.

These tests verify the pyo3 serialization contract — that the Python dict
shapes match what downstream consumers (agents, orchestrators) depend on.
"""

import pytest
import writ


# ── verify_chain contract ─────────────────────────────────────────────


class TestVerifyChainContract:
    """Verify verify_chain returns the documented ChainVerification shape."""

    def test_verify_chain_returns_dict(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()
        assert isinstance(result, dict)

    def test_verify_chain_required_fields(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()

        assert "total_seals" in result
        assert "verified" in result
        assert "unsecured" in result
        assert "failures" in result
        assert "valid" in result

    def test_verify_chain_field_types(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()

        assert isinstance(result["total_seals"], int)
        assert isinstance(result["verified"], int)
        assert isinstance(result["unsecured"], int)
        assert isinstance(result["failures"], list)
        assert isinstance(result["valid"], bool)

    def test_verify_chain_counts_seals(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()
        assert result["total_seals"] >= 1

    def test_verify_chain_valid_for_fresh_repo(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()
        # Fresh repo with proper seals should be valid
        assert result["valid"] is True

    def test_verify_chain_no_failures_on_valid_chain(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()
        assert len(result["failures"]) == 0

    def test_verify_chain_math_consistent(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain()
        # verified + unsecured should equal total_seals
        assert result["verified"] + result["unsecured"] == result["total_seals"]

    def test_verify_chain_with_multiple_seals(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        for i in range(5):
            (tmp_path / f"file_{i}.txt").write_text(f"content {i}")
            repo.seal(
                summary=f"seal {i}",
                agent_id="test-agent",
                agent_type="agent",
                status="in-progress",
            )
        result = repo.verify_chain()
        assert result["total_seals"] == 5
        assert result["valid"] is True

    def test_verify_chain_with_convergence_key(self, sealed_repo):
        repo, path = sealed_repo
        result = repo.verify_chain(use_convergence_key=True)
        assert isinstance(result, dict)
        assert "valid" in result

    def test_verify_chain_default_no_convergence_key(self, sealed_repo):
        repo, path = sealed_repo
        # Default should work without convergence key
        result = repo.verify_chain()
        assert isinstance(result, dict)


# ── verify_seal contract ──────────────────────────────────────────────


class TestVerifySealContract:
    """Verify verify_seal returns the documented SealVerification shape."""

    def test_verify_seal_returns_dict(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)
        assert isinstance(result, dict)

    def test_verify_seal_required_fields(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)

        assert "seal_id" in result
        assert "content_hash_valid" in result
        assert "chain_hash_valid" in result
        assert "signature_present" in result

    def test_verify_seal_field_types(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)

        assert isinstance(result["seal_id"], str)
        assert isinstance(result["content_hash_valid"], bool)
        assert isinstance(result["chain_hash_valid"], bool)
        assert isinstance(result["signature_present"], bool)

    def test_verify_seal_id_matches_input(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)
        assert result["seal_id"] == seal_id

    def test_verify_seal_signature_valid_optional(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)
        # signature_valid is Option<bool> — may be None or bool
        sig_valid = result.get("signature_valid")
        assert sig_valid is None or isinstance(sig_valid, bool)

    def test_verify_seal_error_optional(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id)
        # error is Option<String> — may be absent or string
        error = result.get("error")
        assert error is None or isinstance(error, str)

    def test_verify_seal_nonexistent_raises(self, sealed_repo):
        repo, path = sealed_repo
        with pytest.raises(writ.WritError):
            repo.verify_seal("nonexistent-id-00000000")

    def test_verify_seal_with_convergence_key(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal_id = log[0]["id"]
        result = repo.verify_seal(seal_id, use_convergence_key=True)
        assert isinstance(result, dict)
        assert "seal_id" in result


# ── Seal cryptographic fields contract ────────────────────────────────


class TestSealCryptoFieldsContract:
    """Verify seals contain the expected cryptographic integrity fields."""

    def test_seal_has_id(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        assert "id" in seal
        assert isinstance(seal["id"], str)
        assert len(seal["id"]) > 0

    def test_seal_has_parent(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("first")
        seal1 = repo.seal(summary="first")
        (tmp_path / "b.txt").write_text("second")
        seal2 = repo.seal(summary="second")

        # Second seal should reference first as parent
        assert seal2.get("parent") == seal1["id"]

    def test_first_seal_has_no_parent(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        first_seal = log[-1]  # Oldest (log is newest-first)
        assert first_seal.get("parent") is None

    def test_seal_has_content_hash(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        # Sprint A guarantees secure() populates content_hash on every seal
        content_hash = seal.get("content_hash")
        assert content_hash is not None, "content_hash must be populated (Sprint A)"
        assert isinstance(content_hash, str)
        assert len(content_hash) > 0

    def test_seal_has_chain_hash(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        # Sprint A guarantees secure() populates chain_hash on every seal
        chain_hash = seal.get("chain_hash")
        assert chain_hash is not None, "chain_hash must be populated (Sprint A)"
        assert isinstance(chain_hash, str)
        assert len(chain_hash) > 0

    def test_seal_has_parent_seal_hash(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("first")
        repo.seal(summary="first")
        (tmp_path / "b.txt").write_text("second")
        seal2 = repo.seal(summary="second")

        # Second seal must have parent_seal_hash linking to first
        psh = seal2.get("parent_seal_hash")
        assert psh is not None, "parent_seal_hash must be populated for non-genesis seal"
        assert isinstance(psh, str)
        assert len(psh) > 0

    def test_seal_tree_hash_present(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        assert "tree" in seal
        assert isinstance(seal["tree"], str)
        assert len(seal["tree"]) > 0

    def test_seal_timestamp_present(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        assert "timestamp" in seal
        assert isinstance(seal["timestamp"], str)

    def test_seal_agent_identity_shape(self, sealed_repo):
        repo, path = sealed_repo
        log = repo.log()
        seal = log[0]
        assert "agent" in seal
        assert "id" in seal["agent"]
        assert "agent_type" in seal["agent"]


# ── Chain integrity contract ──────────────────────────────────────────


class TestChainIntegrityContract:
    """Verify chain hashes link seals correctly."""

    def test_chain_hashes_differ_between_seals(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("first")
        seal1 = repo.seal(summary="first")
        (tmp_path / "b.txt").write_text("second")
        seal2 = repo.seal(summary="second")

        h1 = seal1.get("chain_hash")
        h2 = seal2.get("chain_hash")
        if h1 is not None and h2 is not None:
            assert h1 != h2, "Chain hashes should differ between seals"

    def test_content_hashes_differ_for_different_content(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("content A")
        seal1 = repo.seal(summary="first")
        (tmp_path / "a.txt").write_text("content B")
        seal2 = repo.seal(summary="second")

        h1 = seal1.get("content_hash")
        h2 = seal2.get("content_hash")
        if h1 is not None and h2 is not None:
            assert h1 != h2, "Content hashes should differ for different content"

    def test_full_chain_verification_after_multiple_seals(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        for i in range(10):
            (tmp_path / f"file_{i}.txt").write_text(f"content {i}")
            repo.seal(
                summary=f"seal {i}",
                agent_id="test",
                agent_type="agent",
                status="in-progress",
            )

        result = repo.verify_chain()
        assert result["total_seals"] == 10
        assert result["valid"] is True
        assert len(result["failures"]) == 0


# ── Graceful chain degradation (Amis fix: walk_chain_graceful) ────────


class TestChainGracefulDegradation:
    """Verify verify_chain handles missing/corrupted seals gracefully.

    When a seal file is deleted from .writ/seals/, verify_chain should
    return {valid: false, failures: [...]} instead of throwing RuntimeError.
    Covers Amis's fix for the "object not found" bug.
    """

    def test_missing_mid_chain_seal_returns_failure_not_error(self, tmp_path):
        """Delete a middle seal from disk — verify_chain should not throw."""
        repo = writ.Repository.init(str(tmp_path))
        seal_ids = []
        for i in range(3):
            (tmp_path / f"file_{i}.txt").write_text(f"content {i}")
            seal = repo.seal(
                summary=f"seal {i}",
                agent_id="test",
                agent_type="agent",
                status="in-progress",
            )
            seal_ids.append(seal["id"])

        # Delete the middle seal file from disk
        mid_seal_path = tmp_path / ".writ" / "seals" / f"{seal_ids[1]}.json"
        assert mid_seal_path.exists(), f"Expected seal file at {mid_seal_path}"
        mid_seal_path.unlink()

        # Should NOT throw — should return graceful failure
        result = repo.verify_chain()
        assert isinstance(result, dict)
        assert result["valid"] is False
        assert len(result["failures"]) > 0

    def test_missing_head_seal_returns_failure(self, tmp_path):
        """Delete the HEAD seal — verify_chain should degrade gracefully."""
        repo = writ.Repository.init(str(tmp_path))
        (tmp_path / "a.txt").write_text("first")
        seal1 = repo.seal(summary="first")
        (tmp_path / "b.txt").write_text("second")
        seal2 = repo.seal(summary="second")

        # Delete the latest seal
        head_path = tmp_path / ".writ" / "seals" / f"{seal2['id']}.json"
        assert head_path.exists()
        head_path.unlink()

        result = repo.verify_chain()
        assert isinstance(result, dict)
        assert result["valid"] is False
        assert len(result["failures"]) > 0

    def test_missing_genesis_seal_still_verifies_loaded(self, tmp_path):
        """Delete genesis seal — loaded seals should still be verified."""
        repo = writ.Repository.init(str(tmp_path))
        seal_ids = []
        for i in range(3):
            (tmp_path / f"file_{i}.txt").write_text(f"content {i}")
            seal = repo.seal(
                summary=f"seal {i}",
                agent_id="test",
                agent_type="agent",
                status="in-progress",
            )
            seal_ids.append(seal["id"])

        # Delete the genesis (first) seal
        genesis_path = tmp_path / ".writ" / "seals" / f"{seal_ids[0]}.json"
        assert genesis_path.exists()
        genesis_path.unlink()

        result = repo.verify_chain()
        assert isinstance(result, dict)
        assert result["valid"] is False
        # Should have at least 1 failure for the chain break
        assert len(result["failures"]) >= 1
        # The other seals that could be loaded should still count
        assert result["total_seals"] >= 1

    def test_intact_chain_passes_after_verify(self, tmp_path):
        """Sanity check: intact chain still passes (no false positives)."""
        repo = writ.Repository.init(str(tmp_path))
        for i in range(5):
            (tmp_path / f"file_{i}.txt").write_text(f"content {i}")
            repo.seal(
                summary=f"seal {i}",
                agent_id="test",
                agent_type="agent",
                status="in-progress",
            )

        result = repo.verify_chain()
        assert result["valid"] is True
        assert len(result["failures"]) == 0
        assert result["total_seals"] == 5


# ── Error handling contract ───────────────────────────────────────────


class TestSecurityErrorContract:
    """Verify security operations raise WritError on invalid input."""

    def test_verify_seal_bad_id_raises(self, sealed_repo):
        repo, path = sealed_repo
        with pytest.raises(writ.WritError):
            repo.verify_seal("00000000-0000-0000-0000-000000000000")

    def test_verify_seal_empty_id_matches_via_prefix(self, sealed_repo):
        repo, path = sealed_repo
        # Empty string prefix-matches the first seal — this is expected
        # behavior from the short-ID lookup. Verify it returns a valid result.
        result = repo.verify_seal("")
        assert isinstance(result, dict)
        assert "seal_id" in result

    def test_verify_chain_on_empty_repo(self, tmp_path):
        repo = writ.Repository.init(str(tmp_path))
        result = repo.verify_chain()
        # Empty repo: 0 seals, still valid (vacuously)
        assert result["total_seals"] == 0
        assert result["valid"] is True
