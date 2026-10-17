//! Repository locking for concurrent safety.
//!
//! Uses advisory file locks (`flock(2)` on Unix) via the `fs2` crate.
//! The OS automatically releases locks when a process crashes, so no
//! PID tracking or stale lock detection is needed.

use std::fs::{File, OpenOptions};
use std::path::Path;
use std::time::Duration;

use fs2::FileExt;

use crate::error::{WritError, WritResult};

/// An exclusive repository lock.
///
/// Held for the lifetime of the value. When dropped, the lock is
/// released automatically (both the `flock` and the `File` handle).
pub struct RepoLock {
    _file: File,
}

impl RepoLock {
    /// Acquire an exclusive lock on the repository.
    ///
    /// Polls with a short sleep interval until the lock is acquired or
    /// the timeout expires. Returns `WritError::LockTimeout` on failure.
    pub fn acquire(writ_dir: &Path, timeout: Duration) -> WritResult<Self> {
        Self::acquire_named(writ_dir, "writ.lock", timeout)
    }

    /// Acquire an exclusive lock using a custom lock file name.
    ///
    /// Used for remote locking where the lock file is `remote.lock`.
    pub fn acquire_named(dir: &Path, name: &str, timeout: Duration) -> WritResult<Self> {
        let lock_path = dir.join(name);
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;

        if file.try_lock_exclusive().is_ok() {
            return Ok(RepoLock { _file: file });
        }
        // Finding 158: wait in the kernel (a blocking flock on a helper
        // thread), not by polling every 10 ms: fifty polling waiters burnt
        // most of the machine in system time and the lock went to whoever
        // polled at the right instant. On timeout the waiter is abandoned;
        // when it gets the lock later it finds the receiver gone and drops
        // its descriptor, which releases the lock (the original descriptor
        // is closed by then: same open file description).
        let waiter = file.try_clone()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let got = waiter.lock_exclusive().map(|()| waiter);
            let _ = tx.send(got);
        });
        match rx.recv_timeout(timeout) {
            Ok(Ok(locked)) => {
                drop(file);
                Ok(RepoLock { _file: locked })
            }
            Ok(Err(e)) => Err(WritError::Io(e)),
            Err(_) => Err(WritError::LockTimeout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn test_lock_acquire_release() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("writ.lock");

        {
            let _lock = RepoLock::acquire(dir.path(), Duration::from_secs(1)).unwrap();
            assert!(lock_path.exists());
        }
        // After drop, a new lock should succeed immediately.
        let _lock2 = RepoLock::acquire(dir.path(), Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn test_lock_blocks_second() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let _lock = RepoLock::acquire(&dir_path, Duration::from_secs(1)).unwrap();

        // Second attempt with a very short timeout should fail.
        let result = RepoLock::acquire(&dir_path, Duration::from_millis(50));
        assert!(matches!(result, Err(WritError::LockTimeout)));
    }

    #[test]
    fn test_lock_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();

        let _lock = RepoLock::acquire(&dir_path, Duration::from_secs(1)).unwrap();

        let start = std::time::Instant::now();
        let result = RepoLock::acquire(&dir_path, Duration::from_millis(100));
        let elapsed = start.elapsed();

        assert!(matches!(result, Err(WritError::LockTimeout)));
        assert!(elapsed >= Duration::from_millis(100));
    }

    #[test]
    fn test_lock_released_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = dir.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));

        let b = barrier.clone();
        let dp = dir_path.clone();
        let handle = std::thread::spawn(move || {
            let _lock = RepoLock::acquire(&dp, Duration::from_secs(5)).unwrap();
            b.wait(); // Signal that lock is held.
            std::thread::sleep(Duration::from_millis(100));
            // _lock dropped here
        });

        barrier.wait(); // Wait for thread to acquire lock.
                        // The thread holds the lock for ~100ms — we give ourselves 2s to acquire.
        let lock2 = RepoLock::acquire(&dir_path, Duration::from_secs(2));
        assert!(lock2.is_ok());

        handle.join().unwrap();
    }
}
