//! File hash cache keyed by stat data, the way git's index avoids re-reading
//! unchanged files.
//!
//! `compute_state` hashes every file in the working tree to find changes.
//! With this cache a file is re-read and re-hashed only when its size,
//! mtime, ctime or inode differ from the values recorded when it was last
//! hashed; otherwise the recorded SHA-256 is reused.
//!
//! **Racy entries.** A file written in the same clock tick as the scan that
//! hashed it could change again without moving its mtime. Like git, an
//! entry is only stored when the file's mtime and ctime are strictly older
//! than the start of the scan; a "racy" file is simply hashed again next
//! time until it ages.
//!
//! **Documented limit.** A rewrite that keeps size, mtime, ctime and inode
//! all identical is not detected. On Unix the kernel sets ctime on every
//! content write and user space cannot set it back, so this needs a write
//! within one ctime tick of an earlier, already-cached one, or a platform
//! without ctime (Windows, where ctime and inode are recorded as 0 and the
//! key is size and mtime only).
//!
//! The cache lives at `.writ/stat_cache.json`. It is advisory: a missing,
//! unreadable or stale-format file means "hash everything", and write
//! failures are ignored.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::hash::hash_bytes;

/// File name under `.writ/`.
pub const STAT_CACHE_FILE: &str = "stat_cache.json";
const VERSION: u32 = 1;

/// The stat fields a cached hash is valid for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatKey {
    pub size: u64,
    pub mtime_ns: i64,
    pub ctime_ns: i64,
    pub ino: u64,
}

impl StatKey {
    pub fn from_metadata(meta: &Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                size: meta.len(),
                mtime_ns: meta.mtime() as i64 * 1_000_000_000 + meta.mtime_nsec() as i64,
                ctime_ns: meta.ctime() as i64 * 1_000_000_000 + meta.ctime_nsec() as i64,
                ino: meta.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                size: meta.len(),
                mtime_ns: meta.modified().map(system_ns).unwrap_or(0),
                ctime_ns: 0,
                ino: 0,
            }
        }
    }
}

fn system_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    key: StatKey,
    hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: HashMap<String, Entry>,
}

/// One scan's view of the cache: lookups, then [`StatCache::save`].
#[derive(Debug)]
pub struct StatCache {
    path: Option<PathBuf>,
    old: HashMap<String, Entry>,
    new: HashMap<String, Entry>,
    scan_start_ns: i64,
    /// Files whose hash came from the cache in this scan.
    pub hits: usize,
    /// Files read and hashed in this scan.
    pub misses: usize,
}

impl StatCache {
    /// Load the cache for a scan of `repo_root`. Without a `.writ/`
    /// directory the cache is in-memory only and never saved.
    pub fn open(repo_root: &Path) -> Self {
        let writ_dir = repo_root.join(".writ");
        let path = writ_dir.is_dir().then(|| writ_dir.join(STAT_CACHE_FILE));
        let old = path
            .as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
            .filter(|c| c.version == VERSION)
            .map(|c| c.entries)
            .unwrap_or_default();
        Self::with_entries(path, old, system_ns(SystemTime::now()))
    }

    fn with_entries(path: Option<PathBuf>, old: HashMap<String, Entry>, now_ns: i64) -> Self {
        Self {
            path,
            old,
            new: HashMap::new(),
            scan_start_ns: now_ns,
            hits: 0,
            misses: 0,
        }
    }

    /// The cached hash for `rel_path` if `key` matches what was recorded.
    pub fn lookup(&self, rel_path: &str, key: &StatKey) -> Option<&str> {
        self.old
            .get(rel_path)
            .filter(|e| e.key == *key)
            .map(|e| e.hash.as_str())
    }

    /// SHA-256 of `full_path`, from the cache when its stat data matches,
    /// otherwise read and hashed. `None` if the file cannot be read.
    pub fn hash_file(&mut self, rel_path: &str, full_path: &Path) -> Option<String> {
        let meta = fs::metadata(full_path).ok()?;
        let key = StatKey::from_metadata(&meta);
        let hash = match self.lookup(rel_path, &key).map(str::to_string) {
            Some(h) => {
                self.hits += 1;
                h
            }
            None => {
                let content = fs::read(full_path).ok()?;
                self.misses += 1;
                // Stat again after the read: if the file changed meanwhile,
                // the hash may not match the first key; record nothing.
                let after = fs::metadata(full_path)
                    .ok()
                    .map(|m| StatKey::from_metadata(&m));
                let h = hash_bytes(&content);
                if after != Some(key) || content.len() as u64 != key.size {
                    return Some(h);
                }
                h
            }
        };
        self.record(rel_path, key, &hash);
        Some(hash)
    }

    fn record(&mut self, rel_path: &str, key: StatKey, hash: &str) {
        // Racy: changed at or after the scan started; do not trust next time.
        if key.mtime_ns >= self.scan_start_ns || key.ctime_ns >= self.scan_start_ns {
            return;
        }
        self.new.insert(
            rel_path.to_string(),
            Entry {
                key,
                hash: hash.to_string(),
            },
        );
    }

    /// Persist this scan's entries (files no longer seen are dropped).
    /// Skipped when nothing changed. Errors are ignored: the cache is advisory.
    pub fn save(self) {
        let Some(path) = self.path else {
            return;
        };
        let unchanged = self.new.len() == self.old.len()
            && self.new.iter().all(|(p, e)| {
                self.old
                    .get(p)
                    .is_some_and(|o| o.key == e.key && o.hash == e.hash)
            });
        if unchanged {
            return;
        }
        let file = CacheFile {
            version: VERSION,
            entries: self.new,
        };
        if let Ok(bytes) = serde_json::to_vec(&file) {
            let _ = crate::fsutil::atomic_write(&path, &bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;
    use tempfile::TempDir;

    fn repo() -> TempDir {
        let t = TempDir::new().unwrap();
        fs::create_dir(t.path().join(".writ")).unwrap();
        t
    }

    /// Age a file's mtime so it is not racy for the next scan.
    fn age(path: &Path, secs: u64) -> SystemTime {
        let t = SystemTime::now() - Duration::from_secs(secs);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
        t
    }

    fn scan(root: &Path, rel: &str) -> (Option<String>, usize, usize) {
        let mut c = StatCache::open(root);
        let h = c.hash_file(rel, &root.join(rel));
        let (hits, misses) = (c.hits, c.misses);
        c.save();
        (h, hits, misses)
    }

    /// The ctime of a just-written file is "now", so entries become
    /// cacheable only once the scan starts strictly after it.
    fn settle() {
        std::thread::sleep(Duration::from_millis(20));
    }

    #[test]
    fn unchanged_file_is_a_cache_hit() {
        let t = repo();
        let f = t.path().join("a.txt");
        fs::write(&f, "hello\n").unwrap();
        age(&f, 60);
        settle();
        let (h1, _, m1) = scan(t.path(), "a.txt");
        let (h2, hits, m2) = scan(t.path(), "a.txt");
        assert_eq!(h1, h2);
        assert_eq!(h1.as_deref(), Some(hash_bytes(b"hello\n").as_str()));
        assert_eq!((m1, hits, m2), (1, 1, 0));
    }

    #[test]
    fn modified_file_with_preserved_mtime_is_detected_by_size() {
        let t = repo();
        let f = t.path().join("a.txt");
        fs::write(&f, "hello\n").unwrap();
        let mtime = age(&f, 60);
        settle();
        scan(t.path(), "a.txt");

        fs::write(&f, "hello, longer\n").unwrap();
        File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let (h, hits, _) = scan(t.path(), "a.txt");
        assert_eq!(hits, 0);
        assert_eq!(h.as_deref(), Some(hash_bytes(b"hello, longer\n").as_str()));
    }

    /// Same size, same mtime: on Unix the content write still moves ctime,
    /// so the rewrite is detected.
    #[cfg(unix)]
    #[test]
    fn same_size_rewrite_with_preserved_mtime_is_detected_by_ctime() {
        let t = repo();
        let f = t.path().join("a.txt");
        fs::write(&f, "aaaa\n").unwrap();
        let mtime = age(&f, 60);
        settle();
        scan(t.path(), "a.txt");

        fs::write(&f, "bbbb\n").unwrap();
        File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let (h, hits, _) = scan(t.path(), "a.txt");
        assert_eq!(hits, 0);
        assert_eq!(h.as_deref(), Some(hash_bytes(b"bbbb\n").as_str()));
    }

    /// The documented limit: identical size, mtime, ctime and inode return
    /// the recorded hash without reading the file.
    #[test]
    fn identical_stat_data_is_the_documented_limit() {
        let key = StatKey {
            size: 5,
            mtime_ns: 1_000,
            ctime_ns: 1_000,
            ino: 7,
        };
        let mut old = HashMap::new();
        old.insert(
            "a.txt".to_string(),
            Entry {
                key,
                hash: "recorded".to_string(),
            },
        );
        let c = StatCache::with_entries(None, old, 10_000);
        assert_eq!(c.lookup("a.txt", &key), Some("recorded"));
        let other = StatKey { size: 6, ..key };
        assert_eq!(c.lookup("a.txt", &other), None);
        assert_eq!(c.lookup("a.txt", &StatKey { ino: 8, ..key }), None);
        assert_eq!(
            c.lookup(
                "a.txt",
                &StatKey {
                    ctime_ns: 1_001,
                    ..key
                }
            ),
            None
        );
    }

    #[test]
    fn racy_file_is_not_cached() {
        let t = repo();
        let f = t.path().join("a.txt");
        fs::write(&f, "fresh\n").unwrap();
        // mtime is now >= the next scan start only if written after it;
        // force it into the future to make the race deterministic.
        File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        scan(t.path(), "a.txt");
        let (_, hits, misses) = scan(t.path(), "a.txt");
        assert_eq!((hits, misses), (0, 1));
    }

    #[test]
    fn corrupt_cache_file_means_hash_everything() {
        let t = repo();
        let f = t.path().join("a.txt");
        fs::write(&f, "x\n").unwrap();
        fs::write(t.path().join(".writ").join(STAT_CACHE_FILE), "not json").unwrap();
        let (h, hits, misses) = scan(t.path(), "a.txt");
        assert_eq!(h.as_deref(), Some(hash_bytes(b"x\n").as_str()));
        assert_eq!((hits, misses), (0, 1));
    }

    #[test]
    fn no_writ_dir_means_no_cache_file() {
        let t = TempDir::new().unwrap();
        fs::write(t.path().join("a.txt"), "x\n").unwrap();
        scan(t.path(), "a.txt");
        assert!(!t.path().join(".writ").exists());
    }
}
