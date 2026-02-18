//! Error types for writ operations.

use std::fmt;
use std::io;

/// All possible writ errors.
#[derive(Debug)]
pub enum WritError {
    /// The current directory is not a writ repository.
    NotARepo,
    /// A writ repository already exists here.
    AlreadyExists,
    /// An I/O error occurred.
    Io(io::Error),
    /// JSON serialization/deserialization failed.
    Json(serde_json::Error),
    /// An object with the given hash was not found.
    ObjectNotFound(String),
    /// No changes to seal.
    NothingToSeal,
    /// A seal with this ID was not found.
    SealNotFound(String),
    /// A spec with this ID was not found.
    SpecNotFound(String),
    /// Spec has no seals — cannot converge.
    SpecHasNoSeals(String),
    /// Convergence has unresolved conflicts.
    UnresolvedConflicts(usize),
    /// Could not acquire the repository lock within the timeout.
    LockTimeout,
    /// Generic error with a message.
    Other(String),
}

impl fmt::Display for WritError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WritError::NotARepo => write!(f, "not a writ repository (missing .writ/)"),
            WritError::AlreadyExists => write!(f, ".writ/ already exists"),
            WritError::Io(e) => write!(f, "I/O error: {e}"),
            WritError::Json(e) => write!(f, "JSON error: {e}"),
            WritError::ObjectNotFound(hash) => write!(f, "object not found: {hash}"),
            WritError::NothingToSeal => write!(f, "no changes to seal"),
            WritError::SealNotFound(id) => write!(f, "seal not found: {id}"),
            WritError::SpecNotFound(id) => write!(f, "spec not found: {id}"),
            WritError::SpecHasNoSeals(id) => write!(f, "spec has no seals: {id}"),
            WritError::UnresolvedConflicts(n) => {
                write!(f, "{n} unresolved conflict(s) — provide resolutions before applying")
            }
            WritError::LockTimeout => write!(f, "could not acquire repository lock within timeout"),
            WritError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for WritError {}

impl From<io::Error> for WritError {
    fn from(e: io::Error) -> Self {
        WritError::Io(e)
    }
}

impl From<serde_json::Error> for WritError {
    fn from(e: serde_json::Error) -> Self {
        WritError::Json(e)
    }
}

/// Convenience alias for Results in writ.
pub type WritResult<T> = Result<T, WritError>;
