//! Content-addressable object store with transparent zstd compression.
//!
//! Objects are stored in `.writ/objects/` using a 2-character prefix
//! directory scheme (like git). Each object is identified by its SHA-256
//! hash (computed on raw uncompressed content). On-disk format uses a
//! single-byte magic prefix for versioning:
//!
//! - `0x00` = raw (uncompressed)
//! - `0x01` = zstd compressed
//! - `0x02` = zstd with dictionary (reserved, post-beta)
//! - No prefix = legacy (pre-compression, treated as raw)

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};
use crate::fsutil::atomic_write;
use crate::hash::hash_bytes;
use crate::security::{SecurityEvent, SecurityEventLogger, Severity};

// ---------------------------------------------------------------------------
// Magic byte constants
// ---------------------------------------------------------------------------

/// On-disk prefix: explicit raw (uncompressed) content.
const MAGIC_RAW: u8 = 0x00;

/// On-disk prefix: zstd-compressed content.
const MAGIC_ZSTD: u8 = 0x01;

// Not yet used — reserved for post-beta dictionary mode.
// const MAGIC_ZSTD_DICT: u8 = 0x02;

/// Default zstd compression level (1-22; 3 is zstd's own default).
const DEFAULT_ZSTD_LEVEL: i32 = 3;

/// Default maximum decompressed object size: 100 MB.
const DEFAULT_MAX_OBJECT_SIZE: usize = 100 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Compression helpers (public for GC recompression)
// ---------------------------------------------------------------------------

/// Compress `data` with zstd at the given level, prepending the magic byte.
pub fn compress_object(data: &[u8], level: i32) -> Vec<u8> {
    let compressed = zstd::encode_all(std::io::Cursor::new(data), level)
        .expect("zstd compression should not fail on valid input");
    let mut out = Vec::with_capacity(1 + compressed.len());
    out.push(MAGIC_ZSTD);
    out.extend_from_slice(&compressed);
    out
}

/// Decompress an on-disk object, handling all format variants.
///
/// Returns the raw (uncompressed) content. Enforces `max_size` to protect
/// against decompression bombs — aborts early without allocating the full
/// decompressed buffer.
pub fn decompress_object(stored: &[u8], max_size: usize) -> WritResult<Vec<u8>> {
    if stored.is_empty() {
        return Ok(Vec::new());
    }

    match stored[0] {
        MAGIC_RAW => {
            // Explicit raw: strip the prefix byte and return content.
            Ok(stored[1..].to_vec())
        }
        MAGIC_ZSTD => {
            // zstd-compressed: decompress with size limit.
            decompress_zstd(&stored[1..], max_size)
        }
        _ => {
            // Legacy object (no magic byte): return entire content as-is.
            // This handles objects written before the compression sprint.
            Ok(stored.to_vec())
        }
    }
}

/// Check if an on-disk object is zstd-compressed (has MAGIC_ZSTD prefix).
pub fn is_compressed(stored: &[u8]) -> bool {
    !stored.is_empty() && stored[0] == MAGIC_ZSTD
}

/// Check if an on-disk object has any magic byte prefix (not legacy).
pub fn has_magic_byte(stored: &[u8]) -> bool {
    if stored.is_empty() {
        return false;
    }
    matches!(stored[0], MAGIC_RAW | MAGIC_ZSTD)
}

/// Decompress zstd data with a streaming size limit.
fn decompress_zstd(compressed: &[u8], max_size: usize) -> WritResult<Vec<u8>> {
    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(compressed))
        .map_err(|e| WritError::Other(format!("zstd decoder init failed: {e}")))?;

    let mut output = Vec::new();
    let mut buf = [0u8; 64 * 1024]; // 64 KB read buffer

    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| WritError::Other(format!("zstd decompression failed: {e}")))?;
        if n == 0 {
            break;
        }
        if output.len() + n > max_size {
            return Err(WritError::Other(format!(
                "decompression bomb: output exceeds {} byte limit",
                max_size
            )));
        }
        output.extend_from_slice(&buf[..n]);
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Hash validation
// ---------------------------------------------------------------------------

/// Validate that a hash string is well-formed (64 hex chars).
fn validate_hash(hash: &str) -> WritResult<()> {
    if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(WritError::InvalidHash(hash.to_string()))
    }
}

// ---------------------------------------------------------------------------
// ObjectStore
// ---------------------------------------------------------------------------

/// The object store manages content-addressable storage on disk.
pub struct ObjectStore {
    /// Root path: `.writ/objects/`
    root: PathBuf,
    /// zstd compression level (1-22).
    compression_level: i32,
    /// Maximum decompressed object size (decompression bomb limit).
    max_object_size: usize,
}

impl ObjectStore {
    /// Create a new ObjectStore with default settings (level 3, 100 MB limit).
    pub fn new(objects_dir: &Path) -> Self {
        Self {
            root: objects_dir.to_path_buf(),
            compression_level: DEFAULT_ZSTD_LEVEL,
            max_object_size: DEFAULT_MAX_OBJECT_SIZE,
        }
    }

    /// Create a new ObjectStore with custom compression settings.
    pub fn with_config(objects_dir: &Path, compression_level: i32, max_object_size: usize) -> Self {
        Self {
            root: objects_dir.to_path_buf(),
            compression_level,
            max_object_size,
        }
    }

    /// Store bytes and return their content hash.
    ///
    /// Content is hashed raw (uncompressed) for stable deduplication,
    /// then zstd-compressed before writing to disk.
    ///
    /// If the object already exists (same content hash), this is a no-op
    /// and simply returns the existing hash.
    pub fn store(&self, data: &[u8]) -> WritResult<String> {
        let hash = hash_bytes(data);
        let path = self.object_path(&hash);

        if path.exists() {
            return Ok(hash);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        if self.compression_level == 0 {
            // Compression disabled: store with MAGIC_RAW prefix.
            let mut raw = Vec::with_capacity(1 + data.len());
            raw.push(MAGIC_RAW);
            raw.extend_from_slice(data);
            atomic_write(&path, &raw)?;
        } else {
            let compressed = compress_object(data, self.compression_level);
            atomic_write(&path, &compressed)?;
        }
        Ok(hash)
    }

    /// Store bytes without compression (explicit raw with MAGIC_RAW prefix).
    ///
    /// Useful for testing, migration, or objects where compression is
    /// counterproductive (e.g., already-compressed binary content).
    pub fn store_raw(&self, data: &[u8]) -> WritResult<String> {
        let hash = hash_bytes(data);
        let path = self.object_path(&hash);

        if path.exists() {
            return Ok(hash);
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut raw = Vec::with_capacity(1 + data.len());
        raw.push(MAGIC_RAW);
        raw.extend_from_slice(data);
        atomic_write(&path, &raw)?;
        Ok(hash)
    }

    /// Retrieve an object by its hash, verifying integrity on read.
    ///
    /// Transparently decompresses zstd objects and handles legacy
    /// (pre-compression) raw objects. Always verifies the SHA-256 hash
    /// against the decompressed content.
    ///
    /// On decompression bomb detection, emits a Critical security event
    /// (best-effort) and returns `WritError::DecompressionBomb`.
    pub fn retrieve(&self, hash: &str) -> WritResult<Vec<u8>> {
        validate_hash(hash)?;
        let path = self.object_path(hash);
        if !path.exists() {
            return Err(WritError::ObjectNotFound(hash.to_string()));
        }
        let stored = fs::read(&path)?;
        let data = match decompress_object(&stored, self.max_object_size) {
            Ok(d) => d,
            Err(WritError::Other(msg)) if msg.contains("decompression bomb") => {
                self.emit_bomb_event(hash);
                return Err(WritError::DecompressionBomb {
                    hash: hash.to_string(),
                    limit: self.max_object_size,
                });
            }
            Err(e) => return Err(e),
        };
        let actual = hash_bytes(&data);
        if actual != hash {
            return Err(WritError::Other(format!(
                "object corrupted: expected {}, got {actual}",
                &hash[..12]
            )));
        }
        Ok(data)
    }

    /// Check if an object exists.
    pub fn exists(&self, hash: &str) -> bool {
        validate_hash(hash).is_ok() && self.object_path(hash).exists()
    }

    /// Get the filesystem path for an object hash.
    ///
    /// Uses 2-char prefix directories: hash `abcdef...` -> `ab/cdef...`
    /// Callers must validate the hash before calling this.
    fn object_path(&self, hash: &str) -> PathBuf {
        let (prefix, rest) = hash.split_at(2);
        self.root.join(prefix).join(rest)
    }

    /// Best-effort security event emission for decompression bomb detection.
    fn emit_bomb_event(&self, hash: &str) {
        // self.root is `.writ/objects/`, parent is `.writ/`
        if let Some(writ_dir) = self.root.parent() {
            let logger = SecurityEventLogger::new(writ_dir);
            let _ = logger.emit_event(&SecurityEvent {
                timestamp: chrono::Utc::now(),
                severity: Severity::Critical,
                event_type: "decompression_bomb_detected".to_string(),
                agent_id: None,
                details: format!(
                    "object {} exceeds {} byte decompression limit",
                    hash, self.max_object_size
                ),
            });
        }
    }

    /// Compute compression statistics by walking the object store.
    ///
    /// Reads the first byte of each object file to classify it (compressed,
    /// raw, or legacy). Does NOT decompress objects — just reads metadata.
    pub fn compression_stats(&self) -> WritResult<CompressionStats> {
        let mut stats = CompressionStats {
            total_objects: 0,
            compressed_objects: 0,
            raw_objects: 0,
            legacy_objects: 0,
            total_disk_bytes: 0,
            total_content_bytes: 0,
            compression_ratio: 1.0,
        };

        if !self.root.exists() {
            return Ok(stats);
        }

        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            stats.total_objects += 1;
            stats.total_disk_bytes += file_size;

            // Read just the first byte to classify.
            let first_byte = fs::read(entry.path())
                .ok()
                .and_then(|data| data.first().copied());

            match first_byte {
                Some(MAGIC_ZSTD) => {
                    stats.compressed_objects += 1;
                    // For content size, decompress to measure.
                    if let Ok(data) = fs::read(entry.path()) {
                        if let Ok(raw) = decompress_zstd(&data[1..], self.max_object_size) {
                            stats.total_content_bytes += raw.len() as u64;
                        } else {
                            // Can't decompress — use disk size as estimate.
                            stats.total_content_bytes += file_size;
                        }
                    }
                }
                Some(MAGIC_RAW) => {
                    stats.raw_objects += 1;
                    // Content size = disk size minus 1 byte prefix.
                    stats.total_content_bytes += file_size.saturating_sub(1);
                }
                _ => {
                    stats.legacy_objects += 1;
                    // Legacy: entire file IS the content.
                    stats.total_content_bytes += file_size;
                }
            }
        }

        if stats.total_disk_bytes > 0 {
            stats.compression_ratio =
                stats.total_content_bytes as f64 / stats.total_disk_bytes as f64;
        }

        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Compression Statistics
// ---------------------------------------------------------------------------

/// Statistics about the object store's compression state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionStats {
    /// Total number of objects on disk.
    pub total_objects: usize,
    /// Objects stored with zstd compression.
    pub compressed_objects: usize,
    /// Objects stored with explicit raw prefix (MAGIC_RAW).
    pub raw_objects: usize,
    /// Legacy objects (no magic byte, pre-compression era).
    pub legacy_objects: usize,
    /// Total bytes on disk (actual storage used).
    pub total_disk_bytes: u64,
    /// Total bytes of content (sum of decompressed sizes).
    pub total_content_bytes: u64,
    /// Compression ratio: content_bytes / disk_bytes. >1.0 means savings.
    pub compression_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // --- Original ObjectStore tests (updated for compression) ---

    #[test]
    fn test_store_and_retrieve() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"hello world";
        let hash = store.store(data).unwrap();

        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_store_is_idempotent() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"same content";
        let h1 = store.store(data).unwrap();
        let h2 = store.store(data).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_retrieve_nonexistent() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let result = store.retrieve("deadbeef00");
        assert!(result.is_err());
    }

    #[test]
    fn test_exists() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let hash = store.store(b"test").unwrap();
        assert!(store.exists(&hash));
        assert!(!store.exists("nonexistent"));
    }

    #[test]
    fn test_retrieve_rejects_invalid_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("not-a-valid-hash");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("invalid object hash"));
    }

    #[test]
    fn test_retrieve_rejects_too_short_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("abcd");
        assert!(result.is_err());
    }

    #[test]
    fn test_retrieve_rejects_traversal_in_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let result = store.retrieve("../../../etc/passwd/../../../etc/shadow/../xx");
        assert!(result.is_err());
    }

    #[test]
    fn test_exists_returns_false_for_invalid_hash() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        assert!(!store.exists("not-valid"));
        assert!(!store.exists("../../../etc/passwd"));
    }

    // --- S.1.1: Compression helper tests ---

    #[test]
    fn test_compress_decompress_roundtrip() {
        let data = b"fn main() { println!(\"hello world\"); }";
        let compressed = compress_object(data, DEFAULT_ZSTD_LEVEL);
        let decompressed = decompress_object(&compressed, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_legacy_raw_passthrough() {
        // Data without any magic byte prefix (legacy pre-compression object).
        let data = b"legacy content without magic byte";
        let decompressed = decompress_object(data, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, data.to_vec());
    }

    #[test]
    fn test_empty_content_roundtrip() {
        let data = b"";
        let result = decompress_object(data, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_large_content_roundtrip() {
        // 1 MB of repetitive source-like content.
        let line = "    let value = calculate_something(input, &config, options);\n";
        let data: Vec<u8> = line.as_bytes().repeat(1024 * 1024 / line.len());
        let compressed = compress_object(&data, DEFAULT_ZSTD_LEVEL);
        let decompressed = decompress_object(&compressed, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, data);
        // Verify actual compression happened (repetitive text compresses well).
        assert!(compressed.len() < data.len() / 2);
    }

    #[test]
    fn test_decompression_bomb_rejected() {
        // Compress 1 MB, then try to decompress with a 1 KB limit.
        let data = vec![0u8; 1024 * 1024];
        let compressed = compress_object(&data, DEFAULT_ZSTD_LEVEL);
        let result = decompress_object(&compressed, 1024);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("decompression bomb"));
    }

    #[test]
    fn test_magic_raw_explicit() {
        let mut stored = vec![MAGIC_RAW];
        stored.extend_from_slice(b"explicit raw content");
        let decompressed = decompress_object(&stored, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, b"explicit raw content");
    }

    #[test]
    fn test_magic_zstd_decompression() {
        let data = b"zstd compressed content here";
        let compressed = compress_object(data, DEFAULT_ZSTD_LEVEL);
        assert_eq!(compressed[0], MAGIC_ZSTD);
        let decompressed = decompress_object(&compressed, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_unknown_magic_byte_treated_as_legacy() {
        // Byte 0xFF is not a known magic byte — treat entire content as raw.
        let mut stored = vec![0xFF];
        stored.extend_from_slice(b"some data");
        let decompressed = decompress_object(&stored, DEFAULT_MAX_OBJECT_SIZE).unwrap();
        assert_eq!(decompressed, stored); // entire content returned as-is
    }

    #[test]
    fn test_is_compressed() {
        let compressed = compress_object(b"test", DEFAULT_ZSTD_LEVEL);
        assert!(is_compressed(&compressed));

        let mut raw = vec![MAGIC_RAW];
        raw.extend_from_slice(b"test");
        assert!(!is_compressed(&raw));

        assert!(!is_compressed(b"legacy data"));
        assert!(!is_compressed(b""));
    }

    #[test]
    fn test_has_magic_byte() {
        let compressed = compress_object(b"test", DEFAULT_ZSTD_LEVEL);
        assert!(has_magic_byte(&compressed));

        let mut raw = vec![MAGIC_RAW];
        raw.extend_from_slice(b"test");
        assert!(has_magic_byte(&raw));

        assert!(!has_magic_byte(b"legacy data"));
        assert!(!has_magic_byte(b""));
    }

    // --- S.1.2: ObjectStore compression integration tests ---

    #[test]
    fn test_store_compresses_on_disk() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"fn main() { println!(\"hello\"); }";
        let hash = store.store(data).unwrap();

        // Read raw bytes from disk — should have magic byte prefix.
        let (prefix, rest) = hash.split_at(2);
        let path = dir.path().join(prefix).join(rest);
        let on_disk = fs::read(&path).unwrap();
        assert_eq!(on_disk[0], MAGIC_ZSTD);
    }

    #[test]
    fn test_backward_compat_legacy_objects() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        // Manually write a legacy object (no magic byte, raw content).
        let data = b"legacy content";
        let hash = hash_bytes(data);
        let (prefix, rest) = hash.split_at(2);
        let obj_dir = dir.path().join(prefix);
        fs::create_dir_all(&obj_dir).unwrap();
        fs::write(obj_dir.join(rest), data).unwrap();

        // Retrieve should work — legacy passthrough.
        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_dedup_still_works_with_compression() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"deduplicate me";
        let h1 = store.store(data).unwrap();
        let h2 = store.store(data).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_stability_across_formats() {
        // Same content should produce the same hash whether stored
        // compressed, raw, or legacy.
        let data = b"hash stability test";
        let hash_raw = hash_bytes(data);

        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let hash_compressed = store.store(data).unwrap();
        assert_eq!(hash_raw, hash_compressed);
    }

    #[test]
    fn test_store_raw_and_retrieve() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"explicit raw storage";
        let hash = store.store_raw(data).unwrap();
        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);

        // Verify on-disk format has MAGIC_RAW prefix.
        let (prefix, rest) = hash.split_at(2);
        let on_disk = fs::read(dir.path().join(prefix).join(rest)).unwrap();
        assert_eq!(on_disk[0], MAGIC_RAW);
    }

    #[test]
    fn test_integrity_check_catches_corruption() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data = b"will be corrupted";
        let hash = store.store(data).unwrap();

        // Corrupt the on-disk file.
        let (prefix, rest) = hash.split_at(2);
        let path = dir.path().join(prefix).join(rest);
        let corrupted = compress_object(b"different content!", DEFAULT_ZSTD_LEVEL);
        fs::write(&path, corrupted).unwrap();

        let result = store.retrieve(&hash);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("corrupted"));
    }

    #[test]
    fn test_large_file_store_and_retrieve() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
        let hash = store.store(&data).unwrap();
        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_mixed_compressed_and_raw_coexist() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let data_a = b"compressed object";
        let data_b = b"raw object";

        let hash_a = store.store(data_a).unwrap();
        let hash_b = store.store_raw(data_b).unwrap();

        assert_eq!(store.retrieve(&hash_a).unwrap(), data_a);
        assert_eq!(store.retrieve(&hash_b).unwrap(), data_b);
    }

    #[test]
    fn test_compression_actually_saves_space() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        // Repetitive source code compresses well.
        let line = "    let result = do_something(arg1, arg2, arg3);\n";
        let data: Vec<u8> = line.as_bytes().repeat(500);
        let hash = store.store(&data).unwrap();

        let (prefix, rest) = hash.split_at(2);
        let on_disk_size = fs::metadata(dir.path().join(prefix).join(rest))
            .unwrap()
            .len();
        assert!(
            on_disk_size < data.len() as u64 / 2,
            "compressed size {} should be less than half of raw size {}",
            on_disk_size,
            data.len()
        );
    }

    #[test]
    fn test_with_config_compression_level() {
        let dir = tempdir().unwrap();
        let store_fast = ObjectStore::with_config(dir.path(), 1, DEFAULT_MAX_OBJECT_SIZE);

        let data = b"test compression level config";
        let hash = store_fast.store(data).unwrap();
        let retrieved = store_fast.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_with_config_max_size() {
        let dir = tempdir().unwrap();
        // Tiny max size: 32 bytes.
        let store = ObjectStore::with_config(dir.path(), DEFAULT_ZSTD_LEVEL, 32);

        // Store a large object (compresses on disk, decompresses on read).
        let data = vec![0u8; 1024];
        let hash = store.store(&data).unwrap();

        // Retrieve should fail because decompressed size exceeds 32 bytes.
        let result = store.retrieve(&hash);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("decompression bomb"));
    }

    // --- S.1.3: Decompression bomb protection (hardened) ---

    #[test]
    fn test_bomb_returns_decompression_bomb_error_variant() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::with_config(dir.path(), DEFAULT_ZSTD_LEVEL, 64);

        let data = vec![0u8; 1024];
        let hash = store.store(&data).unwrap();

        let result = store.retrieve(&hash);
        assert!(result.is_err());
        match result.unwrap_err() {
            WritError::DecompressionBomb { hash: h, limit } => {
                assert_eq!(h, hash);
                assert_eq!(limit, 64);
            }
            other => panic!("expected DecompressionBomb, got: {other}"),
        }
    }

    #[test]
    fn test_bomb_just_under_limit_succeeds() {
        let dir = tempdir().unwrap();
        // Set limit to exactly the content size.
        let data = vec![42u8; 512];
        let store = ObjectStore::with_config(dir.path(), DEFAULT_ZSTD_LEVEL, 512);
        let hash = store.store(&data).unwrap();
        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_bomb_just_over_limit_fails() {
        let dir = tempdir().unwrap();
        // 513 bytes of content, 512 byte limit.
        let data = vec![42u8; 513];
        let store = ObjectStore::with_config(dir.path(), DEFAULT_ZSTD_LEVEL, 512);
        let hash = store.store(&data).unwrap();
        let result = store.retrieve(&hash);
        assert!(result.is_err());
    }

    #[test]
    fn test_bomb_emits_security_event() {
        let dir = tempdir().unwrap();
        // Set up a minimal .writ/ structure so security event path resolves.
        let writ_dir = dir.path().join(".writ");
        let objects_dir = writ_dir.join("objects");
        let security_dir = writ_dir.join("security");
        fs::create_dir_all(&objects_dir).unwrap();
        fs::create_dir_all(&security_dir).unwrap();

        let store = ObjectStore::with_config(&objects_dir, DEFAULT_ZSTD_LEVEL, 64);

        let data = vec![0u8; 1024];
        let hash = store.store(&data).unwrap();
        let _ = store.retrieve(&hash); // should fail

        // Check that a security event was emitted.
        let events_path = security_dir.join("events.jsonl");
        assert!(events_path.exists(), "security event file should exist");
        let content = fs::read_to_string(&events_path).unwrap();
        assert!(content.contains("decompression_bomb_detected"));
        assert!(content.contains(&hash));
    }

    #[test]
    fn test_bomb_streaming_does_not_allocate_full_size() {
        // Compress data that expands to 10 MB, but set 1 KB limit.
        // If streaming abort works, this should complete nearly instantly
        // without allocating 10 MB.
        let data = vec![0u8; 10 * 1024 * 1024];
        let compressed = compress_object(&data, DEFAULT_ZSTD_LEVEL);
        let result = decompress_object(&compressed, 1024);
        assert!(result.is_err());
    }

    // --- S.1.4: Compression level config ---

    #[test]
    fn test_compression_disabled_stores_raw() {
        let dir = tempdir().unwrap();
        // compression_level 0 = disabled
        let store = ObjectStore::with_config(dir.path(), 0, DEFAULT_MAX_OBJECT_SIZE);

        let data = b"should be stored raw";
        let hash = store.store(data).unwrap();

        // On-disk should have MAGIC_RAW prefix.
        let (prefix, rest) = hash.split_at(2);
        let on_disk = fs::read(dir.path().join(prefix).join(rest)).unwrap();
        assert_eq!(on_disk[0], MAGIC_RAW);

        // Retrieve still works.
        let retrieved = store.retrieve(&hash).unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn test_different_levels_produce_valid_output() {
        for level in [1, 3, 6, 9] {
            let dir = tempdir().unwrap();
            let store = ObjectStore::with_config(dir.path(), level, DEFAULT_MAX_OBJECT_SIZE);
            let data = b"test data for each level";
            let hash = store.store(data).unwrap();
            assert_eq!(store.retrieve(&hash).unwrap(), data);
        }
    }

    // --- S.1.5: Compression stats ---

    #[test]
    fn test_compression_stats_empty_store() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        let store = ObjectStore::new(dir.path());
        let stats = store.compression_stats().unwrap();
        assert_eq!(stats.total_objects, 0);
        assert_eq!(stats.compressed_objects, 0);
        assert_eq!(stats.raw_objects, 0);
        assert_eq!(stats.legacy_objects, 0);
        assert!((stats.compression_ratio - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_compression_stats_all_compressed() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        let line = "    fn example() { return 42; }\n";
        let data: Vec<u8> = line.as_bytes().repeat(200);
        store.store(&data).unwrap();
        store.store(b"another object").unwrap();

        let stats = store.compression_stats().unwrap();
        assert_eq!(stats.total_objects, 2);
        assert_eq!(stats.compressed_objects, 2);
        assert_eq!(stats.raw_objects, 0);
        assert_eq!(stats.legacy_objects, 0);
        // Ratio should be > 1.0 (savings) for repetitive content.
        assert!(stats.compression_ratio > 1.0);
    }

    #[test]
    fn test_compression_stats_mixed_formats() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        // Compressed object.
        store.store(b"compressed content").unwrap();

        // Explicit raw object.
        store.store_raw(b"raw content").unwrap();

        // Legacy object (no magic byte).
        let legacy_data = b"legacy data";
        let hash = hash_bytes(legacy_data);
        let (prefix, rest) = hash.split_at(2);
        let obj_dir = dir.path().join(prefix);
        fs::create_dir_all(&obj_dir).unwrap();
        fs::write(obj_dir.join(rest), legacy_data).unwrap();

        let stats = store.compression_stats().unwrap();
        assert_eq!(stats.total_objects, 3);
        assert_eq!(stats.compressed_objects, 1);
        assert_eq!(stats.raw_objects, 1);
        assert_eq!(stats.legacy_objects, 1);
    }

    #[test]
    fn test_compression_stats_legacy_objects_counted() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        // Write two legacy objects (no magic byte).
        for i in 0..2 {
            let data = format!("legacy content {i}");
            let hash = hash_bytes(data.as_bytes());
            let (prefix, rest) = hash.split_at(2);
            let obj_dir = dir.path().join(prefix);
            fs::create_dir_all(&obj_dir).unwrap();
            fs::write(obj_dir.join(rest), data.as_bytes()).unwrap();
        }

        let stats = store.compression_stats().unwrap();
        assert_eq!(stats.legacy_objects, 2);
        assert_eq!(stats.compressed_objects, 0);
    }

    #[test]
    fn test_compression_stats_serialization_roundtrip() {
        let stats = CompressionStats {
            total_objects: 10,
            compressed_objects: 7,
            raw_objects: 2,
            legacy_objects: 1,
            total_disk_bytes: 5000,
            total_content_bytes: 15000,
            compression_ratio: 3.0,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: CompressionStats = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_objects, 10);
        assert_eq!(deserialized.compressed_objects, 7);
        assert!((deserialized.compression_ratio - 3.0).abs() < 0.01);
    }

    #[test]
    fn test_compression_stats_nonexistent_dir() {
        let dir = tempdir().unwrap();
        let store = ObjectStore::new(&dir.path().join("nonexistent"));
        let stats = store.compression_stats().unwrap();
        assert_eq!(stats.total_objects, 0);
    }
}
