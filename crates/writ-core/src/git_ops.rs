//! GitOps — abstraction over git operations for the finish workflow.
//!
//! Provides a `GitOps` trait so callers don't shell out to `git`.
//! The `Git2Ops` implementation uses libgit2 via the `git2` crate.

use crate::{WritError, WritResult};
use std::path::{Path, PathBuf};

/// Abstraction over git operations needed by `writ finish`.
pub trait GitOps {
    /// Stage specific files. Returns the number of files staged.
    fn stage_files(&self, paths: &[&str]) -> WritResult<usize>;

    /// Stage all changes (equivalent to `git add .`). Returns file count.
    fn stage_all(&self) -> WritResult<usize>;

    /// Create a commit with the given message. Returns the commit hash.
    fn commit(&self, message: &str) -> WritResult<String>;

    /// Get the current branch name, or None if detached HEAD.
    fn current_branch(&self) -> WritResult<Option<String>>;

    /// Switch to an existing branch or create a new one.
    fn checkout_or_create_branch(&self, name: &str) -> WritResult<()>;

    /// Check if there are staged changes ready to commit.
    fn has_staged_changes(&self) -> WritResult<bool>;

    /// Get the repository root path.
    fn root(&self) -> &Path;
}

// ---------------------------------------------------------------------------
// Git2 implementation (requires `bridge` feature)
// ---------------------------------------------------------------------------

#[cfg(feature = "bridge")]
mod git2_impl {
    use super::*;
    use git2::{IndexAddOption, Repository, Signature};

    /// GitOps implementation backed by libgit2.
    pub struct Git2Ops {
        root: PathBuf,
    }

    impl Git2Ops {
        /// Open a git repository at the given path.
        pub fn open(root: &Path) -> WritResult<Self> {
            // Verify it's a valid git repo
            Repository::open(root).map_err(|e| {
                WritError::Other(format!("not a git repository: {e}"))
            })?;
            Ok(Self {
                root: root.to_path_buf(),
            })
        }

        fn repo(&self) -> WritResult<Repository> {
            Repository::open(&self.root).map_err(|e| {
                WritError::Other(format!("failed to open git repository: {e}"))
            })
        }

        fn default_signature(repo: &Repository) -> WritResult<Signature<'static>> {
            repo.signature().map_err(|e| {
                WritError::Other(format!(
                    "git user not configured (set user.name and user.email): {e}"
                ))
            })
        }
    }

    impl GitOps for Git2Ops {
        fn stage_files(&self, paths: &[&str]) -> WritResult<usize> {
            let repo = self.repo()?;
            let mut index = repo.index().map_err(|e| {
                WritError::Other(format!("failed to read git index: {e}"))
            })?;

            let mut count = 0;
            for path in paths {
                let full = self.root.join(path);
                if full.exists() {
                    index.add_path(Path::new(path)).map_err(|e| {
                        WritError::Other(format!("failed to stage '{}': {e}", path))
                    })?;
                    count += 1;
                }
            }

            index.write().map_err(|e| {
                WritError::Other(format!("failed to write git index: {e}"))
            })?;
            Ok(count)
        }

        fn stage_all(&self) -> WritResult<usize> {
            let repo = self.repo()?;
            let mut index = repo.index().map_err(|e| {
                WritError::Other(format!("failed to read git index: {e}"))
            })?;

            // Count entries before
            let before = index.len();

            index
                .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
                .map_err(|e| {
                    WritError::Other(format!("failed to stage all files: {e}"))
                })?;

            // Remove deleted files from index
            index
                .update_all(["*"].iter(), None)
                .map_err(|e| {
                    WritError::Other(format!("failed to update index for deletions: {e}"))
                })?;

            index.write().map_err(|e| {
                WritError::Other(format!("failed to write git index: {e}"))
            })?;

            let after = index.len();
            // Return approximate count (may differ from actual staged changes)
            Ok(if after >= before { after - before } else { before - after })
        }

        fn commit(&self, message: &str) -> WritResult<String> {
            let repo = self.repo()?;
            let sig = Self::default_signature(&repo)?;
            let mut index = repo.index().map_err(|e| {
                WritError::Other(format!("failed to read git index: {e}"))
            })?;

            let tree_oid = index.write_tree().map_err(|e| {
                WritError::Other(format!("failed to write tree: {e}"))
            })?;
            let tree = repo.find_tree(tree_oid).map_err(|e| {
                WritError::Other(format!("failed to find tree: {e}"))
            })?;

            // Get parent commit (HEAD), if any
            let parent = match repo.head() {
                Ok(head) => Some(head.peel_to_commit().map_err(|e| {
                    WritError::Other(format!("failed to resolve HEAD: {e}"))
                })?),
                Err(_) => None, // Initial commit
            };

            let parents: Vec<&git2::Commit> =
                parent.as_ref().map(|p| vec![p]).unwrap_or_default();

            let oid = repo
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
                .map_err(|e| {
                    WritError::Other(format!("failed to create commit: {e}"))
                })?;

            Ok(oid.to_string())
        }

        fn current_branch(&self) -> WritResult<Option<String>> {
            let repo = self.repo()?;
            let head = match repo.head() {
                Ok(h) => h,
                Err(_) => return Ok(None), // No commits yet
            };
            if head.is_branch() {
                let name = head.shorthand().map(|s| s.to_string());
                Ok(name)
            } else {
                Ok(None) // Detached HEAD
            }
        }

        fn checkout_or_create_branch(&self, name: &str) -> WritResult<()> {
            let repo = self.repo()?;

            // Try to find existing branch
            match repo.find_branch(name, git2::BranchType::Local) {
                Ok(branch) => {
                    // Checkout existing branch
                    let refname = branch.get().name().ok_or_else(|| {
                        WritError::Other("branch ref has no name".into())
                    })?;
                    repo.set_head(refname).map_err(|e| {
                        WritError::Other(format!("failed to set HEAD to {}: {e}", name))
                    })?;
                    repo.checkout_head(Some(
                        git2::build::CheckoutBuilder::new().force(),
                    ))
                    .map_err(|e| {
                        WritError::Other(format!("failed to checkout {}: {e}", name))
                    })?;
                }
                Err(_) => {
                    // Create new branch from HEAD
                    let head = repo.head().map_err(|e| {
                        WritError::Other(format!("no HEAD to branch from: {e}"))
                    })?;
                    let commit = head.peel_to_commit().map_err(|e| {
                        WritError::Other(format!("HEAD is not a commit: {e}"))
                    })?;
                    let branch = repo.branch(name, &commit, false).map_err(|e| {
                        WritError::Other(format!("failed to create branch '{}': {e}", name))
                    })?;
                    let refname = branch.get().name().ok_or_else(|| {
                        WritError::Other("new branch ref has no name".into())
                    })?;
                    repo.set_head(refname).map_err(|e| {
                        WritError::Other(format!("failed to set HEAD: {e}"))
                    })?;
                    repo.checkout_head(Some(
                        git2::build::CheckoutBuilder::new().force(),
                    ))
                    .map_err(|e| {
                        WritError::Other(format!("failed to checkout new branch: {e}"))
                    })?;
                }
            }

            Ok(())
        }

        fn has_staged_changes(&self) -> WritResult<bool> {
            let repo = self.repo()?;
            let head_tree = match repo.head() {
                Ok(head) => Some(head.peel_to_tree().map_err(|e| {
                    WritError::Other(format!("failed to get HEAD tree: {e}"))
                })?),
                Err(_) => None, // No commits — any staged content counts
            };

            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, None)
                .map_err(|e| {
                    WritError::Other(format!("failed to diff index: {e}"))
                })?;

            Ok(diff.deltas().len() > 0)
        }

        fn root(&self) -> &Path {
            &self.root
        }
    }
}

#[cfg(feature = "bridge")]
pub use git2_impl::Git2Ops;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(feature = "bridge")]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn init_git_repo(dir: &Path) -> git2::Repository {
        let repo = git2::Repository::init(dir).unwrap();
        // Configure user for commits
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test.com").unwrap();
        repo
    }

    #[test]
    fn test_open_valid_repo() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path());
        assert!(ops.is_ok());
    }

    #[test]
    fn test_open_not_a_repo() {
        let dir = tempdir().unwrap();
        let ops = Git2Ops::open(dir.path());
        assert!(ops.is_err());
    }

    #[test]
    fn test_stage_and_commit() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();

        // Create a file and stage it
        fs::write(dir.path().join("hello.txt"), "world").unwrap();
        let staged = ops.stage_files(&["hello.txt"]).unwrap();
        assert_eq!(staged, 1);

        // Should have staged changes
        assert!(ops.has_staged_changes().unwrap());

        // Commit
        let hash = ops.commit("initial commit").unwrap();
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 40); // SHA-1 hex

        // No more staged changes
        assert!(!ops.has_staged_changes().unwrap());
    }

    #[test]
    fn test_stage_all() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();

        fs::write(dir.path().join("a.txt"), "aaa").unwrap();
        fs::write(dir.path().join("b.txt"), "bbb").unwrap();
        ops.stage_all().unwrap();
        assert!(ops.has_staged_changes().unwrap());

        let hash = ops.commit("add files").unwrap();
        assert!(!hash.is_empty());
    }

    #[test]
    fn test_current_branch() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();

        // No commits yet — no branch
        assert!(ops.current_branch().unwrap().is_none());

        // Make a commit to establish a branch
        fs::write(dir.path().join("f.txt"), "x").unwrap();
        ops.stage_all().unwrap();
        ops.commit("init").unwrap();

        let branch = ops.current_branch().unwrap();
        assert!(branch.is_some());
        // Default branch is usually "main" or "master"
        let name = branch.unwrap();
        assert!(name == "main" || name == "master");
    }

    #[test]
    fn test_checkout_or_create_branch() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();

        // Need an initial commit first
        fs::write(dir.path().join("f.txt"), "x").unwrap();
        ops.stage_all().unwrap();
        ops.commit("init").unwrap();

        // Create a new branch
        ops.checkout_or_create_branch("feature-x").unwrap();
        assert_eq!(
            ops.current_branch().unwrap().as_deref(),
            Some("feature-x")
        );

        // Switch back (assuming default was "master" or "main")
        // Create it explicitly first
        ops.checkout_or_create_branch("test-main").unwrap();
        assert_eq!(
            ops.current_branch().unwrap().as_deref(),
            Some("test-main")
        );

        // Switch to existing branch
        ops.checkout_or_create_branch("feature-x").unwrap();
        assert_eq!(
            ops.current_branch().unwrap().as_deref(),
            Some("feature-x")
        );
    }

    #[test]
    fn test_root_returns_path() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();
        assert_eq!(ops.root(), dir.path());
    }
}
