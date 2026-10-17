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
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(data)?;
    sync(&file)?;
    fs::rename(&tmp, path)?;
    Ok(())
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
