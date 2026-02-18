//! Content hashing using SHA-256.

use sha2::{Digest, Sha256};

/// Compute the SHA-256 hash of arbitrary bytes, returned as a hex string.
pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    hex_encode(&result)
}

/// Compute the SHA-256 hash of a string.
pub fn hash_str(s: &str) -> String {
    hash_bytes(s.as_bytes())
}

/// Encode raw bytes as a lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_deterministic() {
        let h1 = hash_str("hello world");
        let h2 = hash_str("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_different_inputs() {
        let h1 = hash_str("hello");
        let h2 = hash_str("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_length() {
        let h = hash_str("test");
        // SHA-256 produces 64 hex characters
        assert_eq!(h.len(), 64);
    }
}
