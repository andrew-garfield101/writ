//! Key storage for agent Ed25519 keypairs.
//!
//! Private keys are encrypted at rest using AES-256-GCM with a
//! randomly-generated master key. Public keys are stored in plaintext
//! for easy retrieval during signature verification.
//!
//! Layout:
//!   .writ/keys/.master              — 32-byte master encryption key (hex)
//!   .writ/keys/{agent_id}.pub       — Ed25519 verifying key (hex)
//!   .writ/keys/{agent_id}.enc       — AES-256-GCM encrypted signing key

use std::fs;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::crypto;
use crate::error::{WritError, WritResult};

/// Manages agent keypairs in `.writ/keys/`.
pub struct KeyStore {
    keys_dir: PathBuf,
}

/// AES-256-GCM nonce size (96 bits / 12 bytes).
const NONCE_SIZE: usize = 12;

impl KeyStore {
    /// Open an existing key store.
    pub fn open(writ_dir: &Path) -> Self {
        KeyStore {
            keys_dir: writ_dir.join("keys"),
        }
    }

    /// Ensure the master encryption key exists, creating one if needed.
    ///
    /// Called during `writ init`. The master key is 32 random bytes stored
    /// as hex in `.writ/keys/.master`.
    pub fn ensure_master_key(&self) -> WritResult<()> {
        let path = self.master_key_path();
        if path.exists() {
            return Ok(());
        }
        let mut key_bytes = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut key_bytes);
        let hex = crypto::hex_encode_pub(&key_bytes);
        fs::write(&path, hex)?;
        Self::restrict_permissions(&path)?;
        Ok(())
    }

    /// Store an agent's keypair. The signing key is encrypted; the
    /// verifying key is stored in plaintext.
    pub fn store_agent_key(
        &self,
        agent_id: &str,
        signing_key: &ed25519_dalek::SigningKey,
        verifying_key: &ed25519_dalek::VerifyingKey,
    ) -> WritResult<()> {
        Self::validate_agent_id(agent_id)?;

        // Check for duplicate
        if self.has_agent(agent_id) {
            return Err(WritError::InvalidInput(format!(
                "key for agent '{agent_id}' already exists"
            )));
        }

        // Store public key in plaintext
        let pub_hex = crypto::verifying_key_to_hex(verifying_key);
        fs::write(self.pub_key_path(agent_id), pub_hex)?;

        // Encrypt and store private key
        let master_key = self.load_master_key()?;
        let plaintext = signing_key.to_bytes();
        let ciphertext = self.encrypt(&master_key, &plaintext)?;
        let enc_hex: String = ciphertext.iter().map(|b| format!("{b:02x}")).collect();
        let enc_path = self.enc_key_path(agent_id);
        fs::write(&enc_path, enc_hex)?;
        Self::restrict_permissions(&enc_path)?;

        Ok(())
    }

    /// Load an agent's signing (private) key.
    pub fn load_agent_signing_key(&self, agent_id: &str) -> WritResult<ed25519_dalek::SigningKey> {
        let enc_hex = fs::read_to_string(self.enc_key_path(agent_id))
            .map_err(|_| WritError::InvalidInput(format!("no key found for agent '{agent_id}'")))?;
        let ciphertext = hex_decode_bytes(&enc_hex)?;
        let master_key = self.load_master_key()?;
        let plaintext = self.decrypt(&master_key, &ciphertext)?;
        if plaintext.len() != 32 {
            return Err(WritError::InvalidInput(
                "decrypted key has wrong length".into(),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&plaintext);
        Ok(ed25519_dalek::SigningKey::from_bytes(&arr))
    }

    /// Load an agent's verifying (public) key.
    pub fn load_agent_verifying_key(
        &self,
        agent_id: &str,
    ) -> WritResult<ed25519_dalek::VerifyingKey> {
        let hex = fs::read_to_string(self.pub_key_path(agent_id)).map_err(|_| {
            WritError::InvalidInput(format!("no public key found for agent '{agent_id}'"))
        })?;
        crypto::verifying_key_from_hex(hex.trim()).ok_or_else(|| {
            WritError::InvalidInput(format!("invalid public key for agent '{agent_id}'"))
        })
    }

    /// Check if an agent has stored keys.
    pub fn has_agent(&self, agent_id: &str) -> bool {
        self.pub_key_path(agent_id).exists()
    }

    /// List all agents that have stored keys.
    pub fn list_agents(&self) -> WritResult<Vec<String>> {
        let mut agents = Vec::new();
        if !self.keys_dir.exists() {
            return Ok(agents);
        }
        for entry in fs::read_dir(&self.keys_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(agent_id) = name.strip_suffix(".pub") {
                agents.push(agent_id.to_string());
            }
        }
        agents.sort();
        Ok(agents)
    }

    /// Remove an agent's keys (for revocation).
    ///
    /// Returns true if keys existed and were removed.
    pub fn remove_agent_keys(&self, agent_id: &str) -> WritResult<bool> {
        let pub_path = self.pub_key_path(agent_id);
        let enc_path = self.enc_key_path(agent_id);
        let existed = pub_path.exists() || enc_path.exists();
        if pub_path.exists() {
            fs::remove_file(&pub_path)?;
        }
        if enc_path.exists() {
            fs::remove_file(&enc_path)?;
        }
        Ok(existed)
    }

    // --- Internal helpers ---

    fn master_key_path(&self) -> PathBuf {
        self.keys_dir.join(".master")
    }

    fn pub_key_path(&self, agent_id: &str) -> PathBuf {
        self.keys_dir.join(format!("{agent_id}.pub"))
    }

    fn enc_key_path(&self, agent_id: &str) -> PathBuf {
        self.keys_dir.join(format!("{agent_id}.enc"))
    }

    fn load_master_key(&self) -> WritResult<[u8; 32]> {
        let hex = fs::read_to_string(self.master_key_path()).map_err(|_| {
            WritError::InvalidInput("master encryption key not found — run writ init".into())
        })?;
        let bytes = hex_decode_bytes(hex.trim())?;
        if bytes.len() != 32 {
            return Err(WritError::InvalidInput(
                "master key has wrong length".into(),
            ));
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(arr)
    }

    fn encrypt(&self, key: &[u8; 32], plaintext: &[u8]) -> WritResult<Vec<u8>> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| WritError::Other(format!("AES init: {e}")))?;

        // Generate random nonce
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| WritError::Other(format!("AES encrypt: {e}")))?;

        // Prepend nonce to ciphertext: [nonce (12 bytes) || ciphertext]
        let mut output = nonce_bytes.to_vec();
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    fn decrypt(&self, key: &[u8; 32], data: &[u8]) -> WritResult<Vec<u8>> {
        if data.len() < NONCE_SIZE {
            return Err(WritError::InvalidInput("encrypted data too short".into()));
        }

        let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
        let nonce = Nonce::from_slice(nonce_bytes);

        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|e| WritError::Other(format!("AES init: {e}")))?;

        cipher.decrypt(nonce, ciphertext).map_err(|_| {
            WritError::InvalidInput("decryption failed — wrong key or corrupted data".into())
        })
    }

    /// Set file permissions to 0600 (owner read/write only) on Unix.
    /// No-op on non-Unix platforms.
    #[cfg(unix)]
    fn restrict_permissions(path: &Path) -> WritResult<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn restrict_permissions(_path: &Path) -> WritResult<()> {
        Ok(())
    }

    fn validate_agent_id(agent_id: &str) -> WritResult<()> {
        if agent_id.is_empty() {
            return Err(WritError::InvalidInput("agent_id cannot be empty".into()));
        }
        // Prevent path traversal in agent IDs
        if agent_id.contains('/') || agent_id.contains('\\') || agent_id.contains("..") {
            return Err(WritError::PathTraversal(agent_id.to_string()));
        }
        // Only allow alphanumeric, hyphens, underscores, dots
        if !agent_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(WritError::InvalidInput(format!(
                "agent_id '{agent_id}' contains invalid characters"
            )));
        }
        Ok(())
    }
}

/// Decode a hex string to bytes.
fn hex_decode_bytes(hex: &str) -> WritResult<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return Err(WritError::InvalidInput("odd-length hex string".into()));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|_| WritError::InvalidInput("invalid hex character".into()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_keystore(dir: &Path) -> KeyStore {
        let keys_dir = dir.join("keys");
        fs::create_dir_all(&keys_dir).unwrap();
        let ks = KeyStore::open(dir);
        ks.ensure_master_key().unwrap();
        ks
    }

    #[test]
    fn test_ensure_master_key_creates_file() {
        let dir = tempdir().unwrap();
        let _ks = setup_keystore(dir.path());
        let master_path = dir.path().join("keys/.master");
        assert!(master_path.exists());
        let hex = fs::read_to_string(&master_path).unwrap();
        assert_eq!(
            hex.len(),
            64,
            "master key should be 64 hex chars (32 bytes)"
        );
    }

    #[test]
    fn test_ensure_master_key_idempotent() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());
        let hex1 = fs::read_to_string(dir.path().join("keys/.master")).unwrap();
        ks.ensure_master_key().unwrap();
        let hex2 = fs::read_to_string(dir.path().join("keys/.master")).unwrap();
        assert_eq!(hex1, hex2, "second call should not regenerate key");
    }

    #[test]
    fn test_store_and_load_agent_key() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (signing, verifying) = crypto::generate_keypair();
        ks.store_agent_key("agent-1", &signing, &verifying).unwrap();

        // Load and verify signing key roundtrips
        let loaded_signing = ks.load_agent_signing_key("agent-1").unwrap();
        assert_eq!(
            loaded_signing.to_bytes(),
            signing.to_bytes(),
            "signing key should roundtrip"
        );

        // Load and verify public key roundtrips
        let loaded_verifying = ks.load_agent_verifying_key("agent-1").unwrap();
        assert_eq!(
            loaded_verifying.as_bytes(),
            verifying.as_bytes(),
            "verifying key should roundtrip"
        );
    }

    #[test]
    fn test_store_duplicate_agent_rejected() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s1, v1) = crypto::generate_keypair();
        ks.store_agent_key("agent-1", &s1, &v1).unwrap();

        let (s2, v2) = crypto::generate_keypair();
        let err = ks.store_agent_key("agent-1", &s2, &v2).unwrap_err();
        assert!(
            format!("{err}").contains("already exists"),
            "duplicate should be rejected: {err}"
        );
    }

    #[test]
    fn test_has_agent() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        assert!(!ks.has_agent("ghost"));

        let (s, v) = crypto::generate_keypair();
        ks.store_agent_key("agent-x", &s, &v).unwrap();
        assert!(ks.has_agent("agent-x"));
    }

    #[test]
    fn test_list_agents() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s1, v1) = crypto::generate_keypair();
        let (s2, v2) = crypto::generate_keypair();
        ks.store_agent_key("alice", &s1, &v1).unwrap();
        ks.store_agent_key("bob", &s2, &v2).unwrap();

        let agents = ks.list_agents().unwrap();
        assert_eq!(agents, vec!["alice", "bob"]);
    }

    #[test]
    fn test_remove_agent_keys() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s, v) = crypto::generate_keypair();
        ks.store_agent_key("remove-me", &s, &v).unwrap();
        assert!(ks.has_agent("remove-me"));

        let removed = ks.remove_agent_keys("remove-me").unwrap();
        assert!(removed);
        assert!(!ks.has_agent("remove-me"));

        // Load should fail
        assert!(ks.load_agent_signing_key("remove-me").is_err());
    }

    #[test]
    fn test_remove_nonexistent_agent() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());
        let removed = ks.remove_agent_keys("ghost").unwrap();
        assert!(!removed);
    }

    #[test]
    fn test_encrypted_key_not_plaintext() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (signing, verifying) = crypto::generate_keypair();
        let signing_hex = crypto::signing_key_to_hex(&signing);
        ks.store_agent_key("secure-agent", &signing, &verifying)
            .unwrap();

        // Read the raw encrypted file
        let enc_content = fs::read_to_string(dir.path().join("keys/secure-agent.enc")).unwrap();
        // The encrypted content should NOT contain the plaintext key hex
        assert!(
            !enc_content.contains(&signing_hex),
            "encrypted file must not contain plaintext key"
        );
        // The encrypted content should be longer than the key (nonce + tag overhead)
        assert!(
            enc_content.len() > signing_hex.len(),
            "encrypted data should be longer than plaintext (nonce + auth tag)"
        );
    }

    #[test]
    fn test_wrong_master_key_fails_decrypt() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (signing, verifying) = crypto::generate_keypair();
        ks.store_agent_key("agent-1", &signing, &verifying).unwrap();

        // Corrupt the master key
        let mut bad_key = [0u8; 32];
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut bad_key);
        let bad_hex: String = bad_key.iter().map(|b| format!("{b:02x}")).collect();
        fs::write(dir.path().join("keys/.master"), &bad_hex).unwrap();

        // Load should fail due to decryption failure
        let err = ks.load_agent_signing_key("agent-1").unwrap_err();
        assert!(
            format!("{err}").contains("decryption failed"),
            "wrong key should cause decryption failure: {err}"
        );
    }

    #[test]
    fn test_path_traversal_in_agent_id_rejected() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());
        let (s, v) = crypto::generate_keypair();

        assert!(ks.store_agent_key("../evil", &s, &v).is_err());
        assert!(ks.store_agent_key("foo/bar", &s, &v).is_err());
        assert!(ks.store_agent_key("foo\\bar", &s, &v).is_err());
        assert!(ks.store_agent_key("", &s, &v).is_err());
    }

    #[test]
    fn test_valid_agent_ids() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let valid_ids = vec!["agent-1", "my_agent", "Agent.v2", "worker123", "a"];

        for id in valid_ids {
            let (s, v) = crypto::generate_keypair();
            ks.store_agent_key(id, &s, &v).unwrap();
        }

        let agents = ks.list_agents().unwrap();
        assert_eq!(agents.len(), 5);
    }

    #[test]
    fn test_multiple_agents_independent_keys() {
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s1, v1) = crypto::generate_keypair();
        let (s2, v2) = crypto::generate_keypair();
        ks.store_agent_key("agent-a", &s1, &v1).unwrap();
        ks.store_agent_key("agent-b", &s2, &v2).unwrap();

        let loaded_s1 = ks.load_agent_signing_key("agent-a").unwrap();
        let loaded_s2 = ks.load_agent_signing_key("agent-b").unwrap();

        assert_eq!(loaded_s1.to_bytes(), s1.to_bytes());
        assert_eq!(loaded_s2.to_bytes(), s2.to_bytes());
        assert_ne!(
            loaded_s1.to_bytes(),
            loaded_s2.to_bytes(),
            "different agents should have different keys"
        );
    }

    #[test]
    fn test_sign_verify_with_stored_keys() {
        // End-to-end: store keys, load them, sign, verify.
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (signing, verifying) = crypto::generate_keypair();
        ks.store_agent_key("signer", &signing, &verifying).unwrap();

        let loaded_sk = ks.load_agent_signing_key("signer").unwrap();
        let loaded_vk = ks.load_agent_verifying_key("signer").unwrap();

        let content_hash = "some-content-hash-to-sign";
        let signature = crypto::sign(content_hash, &loaded_sk);
        assert!(
            crypto::verify_signature(content_hash, &signature, &loaded_vk),
            "signature with stored keys should verify"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_master_key_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let _ks = setup_keystore(dir.path());

        let master_path = dir.path().join("keys/.master");
        let mode = fs::metadata(&master_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "master key should be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn test_encrypted_key_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s, v) = crypto::generate_keypair();
        ks.store_agent_key("perm-test", &s, &v).unwrap();

        let enc_path = dir.path().join("keys/perm-test.enc");
        let mode = fs::metadata(&enc_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "encrypted key should be 0600, got {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn test_public_key_not_restricted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let ks = setup_keystore(dir.path());

        let (s, v) = crypto::generate_keypair();
        ks.store_agent_key("pub-test", &s, &v).unwrap();

        let pub_path = dir.path().join("keys/pub-test.pub");
        let mode = fs::metadata(&pub_path).unwrap().permissions().mode() & 0o777;
        // Public keys should NOT be 0600 — they're readable by default
        assert_ne!(mode, 0o600, "public key should not be restricted to 0600");
    }
}
