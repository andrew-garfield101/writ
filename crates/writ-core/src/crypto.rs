//! Cryptographic primitives for seal integrity and agent identity.
//!
//! Provides BLAKE3 content hashing, chain hashing, and Ed25519 digital
//! signatures. These are the building blocks for tamper-evident seal chains
//! and authenticated agent identities.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::seal::Seal;

// ---------------------------------------------------------------------------
// Canonical serialization
// ---------------------------------------------------------------------------

/// Fields included in the canonical representation for content hashing.
/// This is the subset of Seal fields that constitute the "content" —
/// everything except id, content_hash, chain_hash, and signature
/// (which are derived from the content).
#[derive(Serialize)]
struct CanonicalSeal<'a> {
    parent: &'a Option<String>,
    timestamp: &'a chrono::DateTime<chrono::Utc>,
    tree: &'a str,
    agent: &'a crate::seal::AgentIdentity,
    spec_id: &'a Option<String>,
    status: &'a crate::seal::TaskStatus,
    changes: &'a Vec<crate::seal::FileChange>,
    verification: &'a crate::seal::Verification,
    summary: &'a str,
    warnings: &'a Vec<String>,
    parent_seal_hash: &'a Option<String>,
}

/// Produce the canonical byte representation of a seal's content fields.
///
/// Uses deterministic JSON serialization with sorted keys. Only content fields
/// are included — derived fields (id, content_hash, chain_hash, signature)
/// are excluded. If the same content can produce two different canonical forms,
/// the entire hash chain breaks, so this function must be deterministic.
pub fn canonical_bytes(seal: &Seal) -> Vec<u8> {
    let canonical = CanonicalSeal {
        parent: &seal.parent,
        timestamp: &seal.timestamp,
        tree: &seal.tree,
        agent: &seal.agent,
        spec_id: &seal.spec_id,
        status: &seal.status,
        changes: &seal.changes,
        verification: &seal.verification,
        summary: &seal.summary,
        warnings: &seal.warnings,
        parent_seal_hash: &seal.parent_seal_hash,
    };
    // serde_json with sorted keys via BTreeMap isn't needed here because
    // struct field order is deterministic in serde — fields serialize in
    // declaration order. This is guaranteed by serde's derive macro.
    serde_json::to_vec(&canonical).expect("canonical serialization should not fail")
}

// ---------------------------------------------------------------------------
// BLAKE3 content hashing
// ---------------------------------------------------------------------------

/// Compute the BLAKE3 hash of a seal's canonical content.
///
/// Returns a 64-character lowercase hex string.
pub fn compute_content_hash(seal: &Seal) -> String {
    let bytes = canonical_bytes(seal);
    blake3_hex(&bytes)
}

/// Compute the BLAKE3 hash of arbitrary bytes, returned as hex.
pub fn blake3_hex(data: &[u8]) -> String {
    let hash = blake3::hash(data);
    hash.to_hex().to_string()
}

// ---------------------------------------------------------------------------
// BLAKE3 chain hashing
// ---------------------------------------------------------------------------

/// Compute the chain hash: `BLAKE3(parent_seal_hash || content_hash)`.
///
/// The chain hash links each seal to its predecessor, creating a tamper-evident
/// chain. If any seal in the chain is modified, all subsequent chain hashes
/// become invalid.
///
/// For the first seal in a chain (`parent_seal_hash` is None), the chain hash
/// is `BLAKE3("" || content_hash)` — an empty string prefix.
pub fn compute_chain_hash(parent_seal_hash: Option<&str>, content_hash: &str) -> String {
    let mut input = String::new();
    if let Some(parent) = parent_seal_hash {
        input.push_str(parent);
    }
    input.push_str(content_hash);
    blake3_hex(input.as_bytes())
}

// ---------------------------------------------------------------------------
// Ed25519 signatures
// ---------------------------------------------------------------------------

/// Generate a new Ed25519 keypair.
///
/// Returns `(signing_key, verifying_key)` — the signing key is private
/// and must be stored securely; the verifying key is public.
pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
    let mut rng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut rng);
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Sign a content hash with an Ed25519 signing key.
///
/// Returns the 64-byte signature as a hex string.
pub fn sign(content_hash: &str, signing_key: &SigningKey) -> String {
    let signature = signing_key.sign(content_hash.as_bytes());
    hex_encode(signature.to_bytes().as_ref())
}

/// Verify an Ed25519 signature against a content hash and public key.
///
/// Returns `true` if the signature is valid, `false` otherwise.
/// Returns `false` for malformed signatures rather than panicking.
pub fn verify_signature(
    content_hash: &str,
    signature_hex: &str,
    verifying_key: &VerifyingKey,
) -> bool {
    let sig_bytes = match hex_decode(signature_hex) {
        Some(b) => b,
        None => return false,
    };
    if sig_bytes.len() != 64 {
        return false;
    }
    let mut sig_array = [0u8; 64];
    sig_array.copy_from_slice(&sig_bytes);
    let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
    verifying_key
        .verify(content_hash.as_bytes(), &signature)
        .is_ok()
}

// ---------------------------------------------------------------------------
// Key serialization
// ---------------------------------------------------------------------------

/// Serializable form of an Ed25519 verifying (public) key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyRecord {
    /// Hex-encoded 32-byte Ed25519 verifying key.
    pub key_hex: String,
    /// Which agent this key belongs to.
    pub agent_id: String,
}

/// Serialize a verifying key to hex.
pub fn verifying_key_to_hex(key: &VerifyingKey) -> String {
    hex_encode(key.as_bytes())
}

/// Deserialize a verifying key from hex.
pub fn verifying_key_from_hex(hex: &str) -> Option<VerifyingKey> {
    let bytes = hex_decode(hex)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    VerifyingKey::from_bytes(&arr).ok()
}

/// Serialize a signing key to hex.
pub fn signing_key_to_hex(key: &SigningKey) -> String {
    hex_encode(key.to_bytes().as_ref())
}

/// Deserialize a signing key from hex.
pub fn signing_key_from_hex(hex: &str) -> Option<SigningKey> {
    let bytes = hex_decode(hex)?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(SigningKey::from_bytes(&arr))
}

// ---------------------------------------------------------------------------
// Hex utilities
// ---------------------------------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Public hex encoding for use by other modules (keystore).
pub fn hex_encode_pub(bytes: &[u8]) -> String {
    hex_encode(bytes)
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{AgentIdentity, AgentType, FileChange, Seal, TaskStatus, Verification};

    fn test_seal(parent: Option<String>, parent_seal_hash: Option<String>) -> Seal {
        Seal::new(
            parent,
            "tree-hash-abc".to_string(),
            AgentIdentity {
                id: "test-agent".to_string(),
                agent_type: AgentType::Agent,
            },
            Some("test-spec".to_string()),
            TaskStatus::InProgress,
            vec![FileChange {
                path: "file.txt".to_string(),
                change_type: crate::seal::ChangeType::Modified,
                old_hash: Some("old".to_string()),
                new_hash: Some("new".to_string()),
            }],
            Verification {
                tests_passed: Some(10),
                tests_failed: Some(0),
                linted: true,
            },
            "test seal summary".to_string(),
            vec![],
            parent_seal_hash,
        )
    }

    // --- Canonical serialization ---

    #[test]
    fn test_canonical_bytes_deterministic() {
        let seal = test_seal(None, None);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "canonical bytes must be deterministic");
    }

    #[test]
    fn test_canonical_bytes_excludes_derived_fields() {
        let seal = test_seal(None, None);
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();
        // Parse as a JSON object and check that derived keys are absent.
        let obj: serde_json::Value = serde_json::from_str(&json).unwrap();
        let map = obj.as_object().unwrap();
        assert!(
            !map.contains_key("id"),
            "canonical should not contain 'id' key"
        );
        assert!(
            !map.contains_key("content_hash"),
            "canonical should not contain 'content_hash' key"
        );
        assert!(
            !map.contains_key("chain_hash"),
            "canonical should not contain 'chain_hash' key"
        );
        assert!(
            !map.contains_key("signature"),
            "canonical should not contain 'signature' key"
        );
        // But content fields ARE present
        assert!(map.contains_key("summary"));
        assert!(map.contains_key("tree"));
        assert!(map.contains_key("agent"));
        assert!(map.contains_key("parent_seal_hash"));
    }

    #[test]
    fn test_canonical_bytes_includes_parent_seal_hash() {
        let seal = test_seal(None, Some("parent-hash-123".to_string()));
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("parent-hash-123"),
            "canonical should include parent_seal_hash"
        );
    }

    // --- BLAKE3 content hashing ---

    #[test]
    fn test_content_hash_deterministic() {
        let seal = test_seal(None, None);
        let h1 = compute_content_hash(&seal);
        let h2 = compute_content_hash(&seal);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_content_hash_is_blake3() {
        let seal = test_seal(None, None);
        let hash = compute_content_hash(&seal);
        // BLAKE3 produces 64 hex chars (32 bytes)
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_content_hash_changes_on_different_content() {
        let seal_a = test_seal(None, None);
        let mut seal_b = test_seal(None, None);
        seal_b.summary = "different summary".to_string();
        // Force same timestamp for comparison
        seal_b.timestamp = seal_a.timestamp;
        seal_b.id = seal_a.id.clone();

        let h_a = compute_content_hash(&seal_a);
        let h_b = compute_content_hash(&seal_b);
        assert_ne!(
            h_a, h_b,
            "different content should produce different hashes"
        );
    }

    // --- BLAKE3 chain hashing ---

    #[test]
    fn test_chain_hash_first_seal() {
        let content_hash = "abc123";
        let chain = compute_chain_hash(None, content_hash);
        // Should be BLAKE3("" + "abc123") = BLAKE3("abc123")
        let expected = blake3_hex(b"abc123");
        assert_eq!(chain, expected);
    }

    #[test]
    fn test_chain_hash_with_parent() {
        let parent_hash = "parenthash999";
        let content_hash = "contenthash111";
        let chain = compute_chain_hash(Some(parent_hash), content_hash);
        let expected = blake3_hex(b"parenthash999contenthash111");
        assert_eq!(chain, expected);
    }

    #[test]
    fn test_chain_hash_changes_with_parent() {
        let content_hash = "same-content";
        let chain_a = compute_chain_hash(None, content_hash);
        let chain_b = compute_chain_hash(Some("parent-x"), content_hash);
        assert_ne!(
            chain_a, chain_b,
            "different parent hashes must produce different chain hashes"
        );
    }

    #[test]
    fn test_chain_hash_changes_with_content() {
        let parent = Some("same-parent");
        let chain_a = compute_chain_hash(parent, "content-a");
        let chain_b = compute_chain_hash(parent, "content-b");
        assert_ne!(
            chain_a, chain_b,
            "different content hashes must produce different chain hashes"
        );
    }

    #[test]
    fn test_chain_hash_length() {
        let chain = compute_chain_hash(Some("p"), "c");
        assert_eq!(chain.len(), 64, "BLAKE3 chain hash should be 64 hex chars");
    }

    // --- Ed25519 signatures ---

    #[test]
    fn test_generate_keypair() {
        let (signing, verifying) = generate_keypair();
        // Signing key is 32 bytes
        assert_eq!(signing.to_bytes().len(), 32);
        // Verifying key is 32 bytes
        assert_eq!(verifying.as_bytes().len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let (signing_key, verifying_key) = generate_keypair();
        let content_hash = "abc123def456";
        let signature = sign(content_hash, &signing_key);
        assert!(
            verify_signature(content_hash, &signature, &verifying_key),
            "valid signature should verify"
        );
    }

    #[test]
    fn test_verify_rejects_tampered_content() {
        let (signing_key, verifying_key) = generate_keypair();
        let signature = sign("original-hash", &signing_key);
        assert!(
            !verify_signature("tampered-hash", &signature, &verifying_key),
            "signature should fail for tampered content"
        );
    }

    #[test]
    fn test_verify_rejects_wrong_key() {
        let (signing_key, _) = generate_keypair();
        let (_, wrong_verifying_key) = generate_keypair();
        let signature = sign("content-hash", &signing_key);
        assert!(
            !verify_signature("content-hash", &signature, &wrong_verifying_key),
            "signature should fail with wrong key"
        );
    }

    #[test]
    fn test_verify_rejects_malformed_signature() {
        let (_, verifying_key) = generate_keypair();
        assert!(
            !verify_signature("content", "not-hex!", &verifying_key),
            "malformed hex should return false"
        );
        assert!(
            !verify_signature("content", "aabb", &verifying_key),
            "too-short signature should return false"
        );
        assert!(
            !verify_signature("content", "", &verifying_key),
            "empty signature should return false"
        );
    }

    #[test]
    fn test_verify_rejects_truncated_signature() {
        let (signing_key, verifying_key) = generate_keypair();
        let signature = sign("content-hash", &signing_key);
        // Truncate to half
        let truncated = &signature[..signature.len() / 2];
        assert!(
            !verify_signature("content-hash", truncated, &verifying_key),
            "truncated signature should fail"
        );
    }

    #[test]
    fn test_signature_hex_roundtrip() {
        let (signing_key, verifying_key) = generate_keypair();
        let signature = sign("test-content", &signing_key);
        // Signature is 128 hex chars (64 bytes)
        assert_eq!(signature.len(), 128);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(verify_signature("test-content", &signature, &verifying_key));
    }

    // --- Key serialization ---

    #[test]
    fn test_verifying_key_hex_roundtrip() {
        let (_, verifying_key) = generate_keypair();
        let hex = verifying_key_to_hex(&verifying_key);
        assert_eq!(hex.len(), 64);
        let recovered = verifying_key_from_hex(&hex).unwrap();
        assert_eq!(recovered.as_bytes(), verifying_key.as_bytes());
    }

    #[test]
    fn test_signing_key_hex_roundtrip() {
        let (signing_key, _) = generate_keypair();
        let hex = signing_key_to_hex(&signing_key);
        assert_eq!(hex.len(), 64);
        let recovered = signing_key_from_hex(&hex).unwrap();
        assert_eq!(recovered.to_bytes(), signing_key.to_bytes());
    }

    #[test]
    fn test_invalid_key_hex() {
        assert!(verifying_key_from_hex("not-valid-hex").is_none());
        assert!(verifying_key_from_hex("aabb").is_none()); // too short
        assert!(signing_key_from_hex("").is_none());
    }

    // --- Full chain scenario ---

    #[test]
    fn test_three_seal_chain_integrity() {
        let (signing_key, verifying_key) = generate_keypair();

        // Seal 1 (genesis)
        let seal1 = test_seal(None, None);
        let content_hash1 = compute_content_hash(&seal1);
        let chain_hash1 = compute_chain_hash(None, &content_hash1);
        let sig1 = sign(&content_hash1, &signing_key);

        // Seal 2 (child of 1)
        let seal2 = test_seal(Some(seal1.id.clone()), Some(chain_hash1.clone()));
        let content_hash2 = compute_content_hash(&seal2);
        let chain_hash2 = compute_chain_hash(Some(&chain_hash1), &content_hash2);
        let sig2 = sign(&content_hash2, &signing_key);

        // Seal 3 (child of 2)
        let seal3 = test_seal(Some(seal2.id.clone()), Some(chain_hash2.clone()));
        let content_hash3 = compute_content_hash(&seal3);
        let chain_hash3 = compute_chain_hash(Some(&chain_hash2), &content_hash3);
        let sig3 = sign(&content_hash3, &signing_key);

        // All signatures valid
        assert!(verify_signature(&content_hash1, &sig1, &verifying_key));
        assert!(verify_signature(&content_hash2, &sig2, &verifying_key));
        assert!(verify_signature(&content_hash3, &sig3, &verifying_key));

        // Chain hashes are all distinct
        assert_ne!(chain_hash1, chain_hash2);
        assert_ne!(chain_hash2, chain_hash3);
        assert_ne!(chain_hash1, chain_hash3);

        // Chain hashes are reproducible
        assert_eq!(compute_chain_hash(None, &content_hash1), chain_hash1);
        assert_eq!(
            compute_chain_hash(Some(&chain_hash1), &content_hash2),
            chain_hash2
        );
        assert_eq!(
            compute_chain_hash(Some(&chain_hash2), &content_hash3),
            chain_hash3
        );
    }

    #[test]
    fn test_tampered_seal_breaks_chain() {
        let seal1 = test_seal(None, None);
        let content_hash1 = compute_content_hash(&seal1);
        let chain_hash1 = compute_chain_hash(None, &content_hash1);

        let seal2 = test_seal(Some(seal1.id.clone()), Some(chain_hash1.clone()));
        let content_hash2 = compute_content_hash(&seal2);
        let chain_hash2 = compute_chain_hash(Some(&chain_hash1), &content_hash2);

        // Tamper with seal1's content
        let mut tampered = seal1.clone();
        tampered.summary = "tampered summary".to_string();
        let tampered_hash = compute_content_hash(&tampered);
        let tampered_chain = compute_chain_hash(None, &tampered_hash);

        // Tampered chain hash doesn't match original
        assert_ne!(tampered_chain, chain_hash1);

        // Seal 2's chain hash was built on original chain_hash1,
        // so recomputing with tampered parent gives wrong result
        let recomputed = compute_chain_hash(Some(&tampered_chain), &content_hash2);
        assert_ne!(recomputed, chain_hash2, "tampered parent breaks chain");
    }

    #[test]
    fn test_chain_with_100_seals_performance() {
        let (signing_key, verifying_key) = generate_keypair();
        let start = std::time::Instant::now();

        let mut prev_chain_hash: Option<String> = None;
        let mut prev_id: Option<String> = None;

        for i in 0..100 {
            let mut seal = test_seal(prev_id.clone(), prev_chain_hash.clone());
            seal.summary = format!("seal #{i}");

            let content_hash = compute_content_hash(&seal);
            let chain_hash = compute_chain_hash(prev_chain_hash.as_deref(), &content_hash);
            let sig = sign(&content_hash, &signing_key);

            // Verify each seal as we go
            assert!(
                verify_signature(&content_hash, &sig, &verifying_key),
                "signature failed for seal #{i}"
            );

            prev_chain_hash = Some(chain_hash);
            prev_id = Some(seal.id.clone());
        }

        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs() < 5,
            "100-seal chain creation + verification took {elapsed:?}"
        );
    }

    // --- A.1.3: Canonical serialization fuzz / edge-case testing ---
    //
    // The serializer is the foundation of the hash chain. If the same
    // content can produce two different canonical forms, the entire
    // chain is broken. These tests probe edge cases aggressively.

    /// Helper: build a seal with custom summary & changes for fuzz testing.
    fn fuzz_seal(summary: &str, changes: Vec<FileChange>, warnings: Vec<String>) -> Seal {
        Seal::new(
            None,
            "tree-fuzz".to_string(),
            AgentIdentity {
                id: "fuzz-agent".to_string(),
                agent_type: AgentType::Agent,
            },
            Some("fuzz-spec".to_string()),
            TaskStatus::InProgress,
            changes,
            Verification::default(),
            summary.to_string(),
            warnings,
            None,
        )
    }

    #[test]
    fn test_canonical_unicode_cjk() {
        let seal = fuzz_seal("中文摘要 日本語テスト 한국어", vec![], vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "CJK text must serialize deterministically");
        let json = String::from_utf8(b1).unwrap();
        assert!(json.contains("中文摘要"), "CJK preserved in output");
    }

    #[test]
    fn test_canonical_unicode_emoji() {
        let seal = fuzz_seal("🦀 Rust seal 🔒🔑", vec![], vec!["⚠️ warning".to_string()]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "emoji must serialize deterministically");
    }

    #[test]
    fn test_canonical_unicode_combining_chars() {
        // é as e + combining acute (U+0301) vs precomposed é (U+00E9)
        let seal_combining = fuzz_seal("cafe\u{0301}", vec![], vec![]);
        let seal_precomposed = fuzz_seal("caf\u{00E9}", vec![], vec![]);
        // These are DIFFERENT unicode strings — canonical_bytes should produce
        // different output (we don't normalize unicode, that's intentional).
        let b1 = canonical_bytes(&seal_combining);
        let b2 = canonical_bytes(&seal_precomposed);
        assert_ne!(
            b1, b2,
            "different unicode representations must produce different canonical bytes"
        );
    }

    #[test]
    fn test_canonical_unicode_rtl() {
        let seal = fuzz_seal("مرحبا بالعالم", vec![], vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "RTL text must serialize deterministically");
    }

    #[test]
    fn test_canonical_empty_summary() {
        let seal = fuzz_seal("", vec![], vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "empty summary must be deterministic");
        let json = String::from_utf8(b1).unwrap();
        assert!(
            json.contains("\"summary\":\"\""),
            "empty summary present in output"
        );
    }

    #[test]
    fn test_canonical_empty_changes() {
        let seal = fuzz_seal("no changes", vec![], vec![]);
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();
        assert!(
            json.contains("\"changes\":[]"),
            "empty changes array present"
        );
    }

    #[test]
    fn test_canonical_no_spec_id() {
        let mut seal = fuzz_seal("no spec", vec![], vec![]);
        seal.spec_id = None;
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2);
        let json = String::from_utf8(b1).unwrap();
        assert!(json.contains("\"spec_id\":null"), "null spec_id in output");
    }

    #[test]
    fn test_canonical_no_parent() {
        let seal = fuzz_seal("genesis", vec![], vec![]);
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();
        assert!(json.contains("\"parent\":null"), "null parent in output");
    }

    #[test]
    fn test_canonical_max_length_summary() {
        let long_summary = "A".repeat(100_000);
        let seal = fuzz_seal(&long_summary, vec![], vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "100K summary must be deterministic");
    }

    #[test]
    fn test_canonical_many_changes() {
        let changes: Vec<FileChange> = (0..500)
            .map(|i| FileChange {
                path: format!("src/module_{i}/file_{i}.rs"),
                change_type: crate::seal::ChangeType::Modified,
                old_hash: Some(format!("old-{i}")),
                new_hash: Some(format!("new-{i}")),
            })
            .collect();
        let seal = fuzz_seal("500 file changes", changes, vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "500 changes must be deterministic");
    }

    #[test]
    fn test_canonical_many_warnings() {
        let warnings: Vec<String> = (0..100)
            .map(|i| format!("WARNING_{i}: something happened at line {}", i * 10))
            .collect();
        let seal = fuzz_seal("many warnings", vec![], warnings);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "100 warnings must be deterministic");
    }

    #[test]
    fn test_canonical_special_chars_in_summary() {
        // Newlines, tabs, quotes, backslashes — JSON escape sequences
        let seal = fuzz_seal(
            "line1\nline2\ttab\r\nwindows\t\"quoted\"\\backslash",
            vec![],
            vec![],
        );
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "special chars must be deterministic");
        let json = String::from_utf8(b1).unwrap();
        // Verify JSON escaping happened
        assert!(json.contains("\\n"), "newline escaped");
        assert!(json.contains("\\t"), "tab escaped");
        assert!(json.contains("\\\""), "quote escaped");
        assert!(json.contains("\\\\"), "backslash escaped");
    }

    #[test]
    fn test_canonical_null_bytes_in_path() {
        let changes = vec![FileChange {
            path: "file\0name.txt".to_string(),
            change_type: crate::seal::ChangeType::Added,
            old_hash: None,
            new_hash: Some("hash".to_string()),
        }];
        let seal = fuzz_seal("null byte path", changes, vec![]);
        let b1 = canonical_bytes(&seal);
        let b2 = canonical_bytes(&seal);
        assert_eq!(b1, b2, "null bytes must be deterministic");
    }

    #[test]
    fn test_canonical_derived_fields_do_not_affect_output() {
        // Two seals with same content but different derived fields must
        // produce identical canonical bytes.
        let mut seal_a = fuzz_seal("same content", vec![], vec![]);
        let mut seal_b = fuzz_seal("same content", vec![], vec![]);

        // Force identical timestamps
        seal_b.timestamp = seal_a.timestamp;

        // Set different derived fields
        seal_a.content_hash = Some("hash-aaa".to_string());
        seal_a.chain_hash = Some("chain-aaa".to_string());
        seal_a.signature = Some("sig-aaa".to_string());

        seal_b.content_hash = Some("hash-bbb".to_string());
        seal_b.chain_hash = Some("chain-bbb".to_string());
        seal_b.signature = Some("sig-bbb".to_string());

        let bytes_a = canonical_bytes(&seal_a);
        let bytes_b = canonical_bytes(&seal_b);
        assert_eq!(
            bytes_a, bytes_b,
            "derived fields (content_hash, chain_hash, signature) must not affect canonical output"
        );
    }

    #[test]
    fn test_canonical_id_does_not_affect_output() {
        let mut seal_a = fuzz_seal("same", vec![], vec![]);
        let mut seal_b = fuzz_seal("same", vec![], vec![]);
        seal_b.timestamp = seal_a.timestamp;

        seal_a.id = "id-aaa".to_string();
        seal_b.id = "id-bbb".to_string();

        assert_eq!(
            canonical_bytes(&seal_a),
            canonical_bytes(&seal_b),
            "seal ID must not affect canonical output"
        );
    }

    #[test]
    fn test_canonical_every_content_field_matters() {
        // Changing any single content field must change the canonical output.
        let baseline = fuzz_seal("baseline", vec![], vec![]);
        let baseline_bytes = canonical_bytes(&baseline);

        // Change summary
        let mut s = baseline.clone();
        s.summary = "different".to_string();
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "summary change detected"
        );

        // Change tree
        let mut s = baseline.clone();
        s.tree = "different-tree".to_string();
        assert_ne!(canonical_bytes(&s), baseline_bytes, "tree change detected");

        // Change agent id
        let mut s = baseline.clone();
        s.agent.id = "other-agent".to_string();
        assert_ne!(canonical_bytes(&s), baseline_bytes, "agent change detected");

        // Change status
        let mut s = baseline.clone();
        s.status = TaskStatus::Complete;
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "status change detected"
        );

        // Change spec_id from Some to None
        let mut s = baseline.clone();
        s.spec_id = None;
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "spec_id change detected"
        );

        // Change timestamp
        let mut s = baseline.clone();
        s.timestamp = s.timestamp + chrono::Duration::seconds(1);
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "timestamp change detected"
        );

        // Change parent
        let mut s = baseline.clone();
        s.parent = Some("parent-id".to_string());
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "parent change detected"
        );

        // Change parent_seal_hash
        let mut s = baseline.clone();
        s.parent_seal_hash = Some("hash-123".to_string());
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "parent_seal_hash change detected"
        );

        // Change verification
        let mut s = baseline.clone();
        s.verification.tests_passed = Some(42);
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "verification change detected"
        );

        // Add warnings
        let mut s = baseline.clone();
        s.warnings = vec!["warn".to_string()];
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "warnings change detected"
        );

        // Add a file change
        let mut s = baseline.clone();
        s.changes = vec![FileChange {
            path: "new.txt".to_string(),
            change_type: crate::seal::ChangeType::Added,
            old_hash: None,
            new_hash: Some("hash".to_string()),
        }];
        assert_ne!(
            canonical_bytes(&s),
            baseline_bytes,
            "changes change detected"
        );
    }

    #[test]
    fn test_canonical_output_is_valid_json() {
        let seal = fuzz_seal(
            "json validity test 🦀\n\t\"quotes\"",
            vec![FileChange {
                path: "src/test.rs".to_string(),
                change_type: crate::seal::ChangeType::Modified,
                old_hash: Some("old".to_string()),
                new_hash: Some("new".to_string()),
            }],
            vec!["warning 1".to_string(), "warning 2".to_string()],
        );
        let bytes = canonical_bytes(&seal);
        // Must be valid UTF-8
        let json_str =
            String::from_utf8(bytes.clone()).expect("canonical bytes must be valid UTF-8");
        // Must parse as valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("canonical bytes must be valid JSON");
        // Must be a JSON object
        assert!(parsed.is_object(), "canonical output must be a JSON object");
    }

    #[test]
    fn test_canonical_roundtrip_hash_stability() {
        // Hash the canonical bytes, then verify re-hashing produces the same result.
        // This simulates what happens when verifying a stored content_hash.
        let seal = test_seal(Some("parent".to_string()), Some("parent-hash".to_string()));
        let hash1 = compute_content_hash(&seal);
        let hash2 = compute_content_hash(&seal);
        let hash3 = compute_content_hash(&seal);
        assert_eq!(hash1, hash2);
        assert_eq!(hash2, hash3);
    }

    #[test]
    fn test_canonical_field_order_is_deterministic() {
        // Verify the JSON keys appear in a fixed order (struct declaration order).
        let seal = fuzz_seal("order test", vec![], vec![]);
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();

        // Find positions of each key in the JSON string
        let pos_parent = json.find("\"parent\"").expect("parent key");
        let pos_timestamp = json.find("\"timestamp\"").expect("timestamp key");
        let pos_tree = json.find("\"tree\"").expect("tree key");
        let pos_agent = json.find("\"agent\"").expect("agent key");
        let pos_spec_id = json.find("\"spec_id\"").expect("spec_id key");
        let pos_status = json.find("\"status\"").expect("status key");
        let pos_changes = json.find("\"changes\"").expect("changes key");
        let pos_verification = json.find("\"verification\"").expect("verification key");
        let pos_summary = json.find("\"summary\"").expect("summary key");
        let pos_warnings = json.find("\"warnings\"").expect("warnings key");
        let pos_psh = json
            .find("\"parent_seal_hash\"")
            .expect("parent_seal_hash key");

        // Assert the order matches CanonicalSeal struct declaration
        assert!(pos_parent < pos_timestamp, "parent before timestamp");
        assert!(pos_timestamp < pos_tree, "timestamp before tree");
        assert!(pos_tree < pos_agent, "tree before agent");
        assert!(pos_agent < pos_spec_id, "agent before spec_id");
        assert!(pos_spec_id < pos_status, "spec_id before status");
        assert!(pos_status < pos_changes, "status before changes");
        assert!(
            pos_changes < pos_verification,
            "changes before verification"
        );
        assert!(
            pos_verification < pos_summary,
            "verification before summary"
        );
        assert!(pos_summary < pos_warnings, "summary before warnings");
        assert!(pos_warnings < pos_psh, "warnings before parent_seal_hash");
    }

    #[test]
    fn test_canonical_verification_edge_cases() {
        // Test all combinations of verification fields
        let cases = vec![
            (None, None, false),                    // all defaults
            (Some(0), Some(0), true),               // zeros
            (Some(u32::MAX), Some(u32::MAX), true), // max u32
            (Some(1), None, false),                 // partial
            (None, Some(5), false),                 // only failures
        ];

        for (tp, tf, linted) in &cases {
            let mut seal = fuzz_seal("verify test", vec![], vec![]);
            seal.verification = Verification {
                tests_passed: *tp,
                tests_failed: *tf,
                linted: *linted,
            };
            let b1 = canonical_bytes(&seal);
            let b2 = canonical_bytes(&seal);
            assert_eq!(
                b1, b2,
                "verification ({tp:?}, {tf:?}, {linted}) must be deterministic"
            );
        }
    }

    #[test]
    fn test_canonical_stress_repeated_serialization() {
        // Serialize the same seal 1000 times — every output must be identical.
        let seal = test_seal(Some("parent".to_string()), Some("hash".to_string()));
        let reference = canonical_bytes(&seal);
        for i in 0..1000 {
            let bytes = canonical_bytes(&seal);
            assert_eq!(bytes, reference, "iteration {i}: canonical output diverged");
        }
    }

    /// Golden-value regression test for canonical serialization.
    ///
    /// This test builds a seal with fully deterministic fields (including a
    /// fixed timestamp) and asserts the exact content hash. If CanonicalSeal's
    /// field set, field order, or serialization format ever changes, this test
    /// will break — which is exactly the point. Any such change would silently
    /// invalidate every existing hash chain.
    #[test]
    fn test_canonical_golden_value_regression() {
        use chrono::TimeZone;

        let mut seal = Seal::new(
            Some("parent-seal-id-abc".to_string()),
            "tree-hash-golden".to_string(),
            AgentIdentity {
                id: "agent-golden".to_string(),
                agent_type: AgentType::Agent,
            },
            Some("spec-golden".to_string()),
            TaskStatus::Complete,
            vec![
                FileChange {
                    path: "src/main.rs".to_string(),
                    change_type: crate::seal::ChangeType::Modified,
                    old_hash: Some("aaa111".to_string()),
                    new_hash: Some("bbb222".to_string()),
                },
                FileChange {
                    path: "README.md".to_string(),
                    change_type: crate::seal::ChangeType::Added,
                    old_hash: None,
                    new_hash: Some("ccc333".to_string()),
                },
            ],
            Verification {
                tests_passed: Some(42),
                tests_failed: Some(0),
                linted: true,
            },
            "golden value test seal".to_string(),
            vec!["SCOPE_WARNING: test".to_string()],
            Some("parent-chain-hash-xyz".to_string()),
        );

        // Pin the timestamp to a fixed value so the hash is fully deterministic
        seal.timestamp = chrono::Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();

        // Compute the content hash once and record it as the golden value.
        // If this hash ever changes, it means the canonical format changed
        // and all existing seal chains would be broken.
        let content_hash = compute_content_hash(&seal);

        // Snapshot the canonical bytes too for structural verification
        let bytes = canonical_bytes(&seal);
        let json = String::from_utf8(bytes).unwrap();

        // Structural assertions — the JSON must contain exactly these keys
        // in this order (CanonicalSeal struct declaration order)
        assert!(json.contains("\"parent\":\"parent-seal-id-abc\""));
        assert!(json.contains("\"tree\":\"tree-hash-golden\""));
        assert!(json.contains("\"summary\":\"golden value test seal\""));
        assert!(json.contains("\"parent_seal_hash\":\"parent-chain-hash-xyz\""));

        // The golden hash — pinned from the first run. If this changes, the
        // canonical format has drifted and all existing seal chains are broken.
        // Only update this value if the format change is intentional AND a
        // chain migration plan is in place.
        let golden_hash = "b2237ff86abac5d0c4aef5a921f5c7179a94704f604b9ca6c078f909a41f111f";

        assert_eq!(
            content_hash, golden_hash,
            "canonical format regression: content_hash changed for fixed seal content"
        );
    }
}
