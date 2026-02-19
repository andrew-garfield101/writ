//! Filesystem utilities for crash-safe writes.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::error::WritResult;

/// Write data to a file atomically using temp-file-then-rename.
///
/// On POSIX, `rename()` within the same filesystem is atomic: either the
/// old file or the new file is visible, never a partial write. We fsync
/// the temp file before renaming so the data is durable on disk.
pub fn atomic_write(path: &Path, data: &[u8]) -> WritResult<()> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(data)?;
    file.sync_data()?;
    fs::rename(&tmp, path)?;
    Ok(())
}
