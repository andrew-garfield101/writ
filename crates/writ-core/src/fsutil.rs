//! Filesystem utilities for crash-safe writes.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::error::WritResult;

/// Write an advisory cache file (finding 158): temp file then rename, so a
/// reader never sees a partial write, but no fsync. A crash can lose the
/// newest version; every caller treats a missing or stale cache as "compute
/// again". The temp name carries the process id, so concurrent writers of
/// the same cache never collide on it.
pub fn write_advisory(path: &Path, data: &[u8]) -> WritResult<()> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = File::create(&tmp)?;
    file.write_all(data)?;
    drop(file);
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Write data to a file atomically using temp-file-then-rename.
///
/// On POSIX, `rename()` within the same filesystem is atomic: either the
/// old file or the new file is visible, never a partial write. We fsync
/// the temp file before renaming so the data is durable on disk.
pub fn atomic_write(path: &Path, data: &[u8]) -> WritResult<()> {
    let tmp = tmp_path(path);
    let result = (|| -> WritResult<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(data)?;
        sync(&file)?;
        fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Finding 218: each write gets its own temp file next to `path`
/// (`<name>.<pid>-<n>.tmp`; still ends in `.tmp`, which listings skip).
/// One shared `<stem>.tmp` let a second writer of the same file truncate
/// the first one's temp file under it: the first rename then published a
/// torn file and the second rename failed (ENOENT).
fn tmp_path(path: &Path) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}.{}-{n}.tmp", std::process::id()))
}

/// `[store] full_fsync` (finding 158): set when a repository is opened.
static FULL_FSYNC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Use `F_FULLFSYNC` on Apple platforms for every store write (the
/// `[store] full_fsync = true` knob); false restores the default.
pub fn set_full_fsync(on: bool) {
    FULL_FSYNC.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Flush a file's data to the device before it is renamed into place.
///
/// Finding 158: on Apple platforms Rust's `sync_data` is
/// `fcntl(F_FULLFSYNC)`, which also flushes the drive's write cache, 5 to
/// 10 ms per call and seven calls per seal, inside the repository lock.
/// Plain `fsync(2)` orders the data with the rename through the file
/// system journal, the guarantee every other platform's `sync_data` gives
/// (git's default does not fsync loose objects at all). Elsewhere,
/// `sync_data` as before.
fn sync(file: &File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        if FULL_FSYNC.load(std::sync::atomic::Ordering::Relaxed) {
            return file.sync_data();
        }
        use std::os::unix::io::AsRawFd;
        // SAFETY: fsync on a descriptor this function's caller owns.
        if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 218: two writers of one file (a seal recording itself on a
    /// spec while another command updates the same spec) must never leave
    /// a reader a torn file, and neither write may fail. With one shared
    /// `<name>.tmp` the second `File::create` truncates the first writer's
    /// temp file under it, and the first rename can publish a half write.
    #[test]
    fn f218_concurrent_atomic_writes_never_publish_a_torn_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spec.json");
        atomic_write(&path, b"{\"w\":0}").unwrap();
        let payload = |w: usize| -> Vec<u8> {
            let body = "x".repeat(64 * 1024);
            format!("{{\"w\":{w},\"body\":\"{body}\"}}").into_bytes()
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writers: Vec<_> = (1..=2)
            .map(|w| {
                let path = path.clone();
                let data = payload(w);
                std::thread::spawn(move || {
                    (0..300)
                        .filter(|_| atomic_write(&path, &data).is_err())
                        .count()
                })
            })
            .collect();
        let reader = {
            let (path, stop) = (path.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut torn = 0usize;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(t) = fs::read_to_string(&path) {
                        if serde_json::from_str::<serde_json::Value>(&t).is_err() {
                            torn += 1;
                        }
                    }
                }
                torn
            })
        };
        let failed: usize = writers.into_iter().map(|h| h.join().unwrap()).sum();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let torn = reader.join().unwrap();
        assert_eq!((failed, torn), (0, 0), "(failed writes, torn reads)");
    }
}
