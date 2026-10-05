//! GitOps — abstraction over git operations for the finish workflow.
//!
//! Provides a `GitOps` trait so callers don't shell out to `git`.
//! The `Git2Ops` implementation uses libgit2 via the `git2` crate.

use crate::{WritError, WritResult};
use std::path::{Path, PathBuf};

/// One path for [`GitOps::stage_contents`]: `content: None` stages a deletion.
#[derive(Debug, Clone)]
pub struct StageEntry {
    pub path: String,
    pub content: Option<Vec<u8>>,
}

/// Result of [`GitOps::stage_contents`].
#[derive(Debug, Clone, Default)]
pub struct StageOutcome {
    /// Paths written to the git index.
    pub staged: Vec<String>,
    /// Paths git would not take, with the reason (finding 44).
    pub refused: Vec<(String, String)>,
}

/// Abstraction over git operations needed by `writ finish`.
pub trait GitOps {
    /// Stage exact contents (not the working tree) for each entry. A path
    /// git cannot take is reported in `refused`, never silently dropped.
    fn stage_contents(&self, entries: &[StageEntry]) -> WritResult<StageOutcome>;

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

    /// True when `path` (relative to the root) is in the git index.
    fn is_tracked(&self, path: &str) -> WritResult<bool>;

    /// Write the staged tree (the git index) to `dest`, replacing what is
    /// there, so a build can be checked before committing (finding 48).
    /// Returns the number of files written.
    fn export_index(&self, dest: &Path) -> WritResult<usize>;

    /// Reset the git index to HEAD without touching the working tree
    /// (`git reset --mixed`), undoing staging when finish aborts.
    fn reset_index_to_head(&self) -> WritResult<()>;

    /// The HEAD commit hash, or None before the first commit.
    fn head_hash(&self) -> WritResult<Option<String>>;
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
            Repository::open(root)
                .map_err(|e| WritError::Other(format!("not a git repository: {e}")))?;
            Ok(Self {
                root: root.to_path_buf(),
            })
        }

        fn repo(&self) -> WritResult<Repository> {
            Repository::open(&self.root)
                .map_err(|e| WritError::Other(format!("failed to open git repository: {e}")))
        }

        /// Git file mode for `path`: the tracked mode, else executable when
        /// the working-tree file is, else a regular blob.
        fn file_mode(&self, index: &git2::Index, path: &Path) -> u32 {
            if let Some(existing) = index.get_path(path, 0) {
                return existing.mode;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = std::fs::metadata(self.root.join(path)) {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return 0o100755;
                    }
                }
            }
            0o100644
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
        fn stage_contents(&self, entries: &[StageEntry]) -> WritResult<StageOutcome> {
            let repo = self.repo()?;
            let mut index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;
            let mut outcome = StageOutcome::default();
            for entry in entries {
                let path = Path::new(&entry.path);
                let tracked = index.get_path(path, 0).is_some();
                let result = match entry.content {
                    None if !tracked => continue, // never in git: nothing to delete
                    None => index.remove_path(path).map_err(|e| e.message().to_string()),
                    Some(ref content) => {
                        if !tracked && ignored_by_git(&repo, &entry.path) {
                            outcome.refused.push((
                                entry.path.clone(),
                                "ignored by .gitignore and not tracked by git; \
                                 run `git add -f` to commit it"
                                    .to_string(),
                            ));
                            continue;
                        }
                        let mode = self.file_mode(&index, path);
                        index_entry_add(&mut index, &entry.path, mode, content)
                    }
                };
                match result {
                    Ok(()) => outcome.staged.push(entry.path.clone()),
                    Err(reason) => outcome.refused.push((entry.path.clone(), reason)),
                }
            }
            index
                .write()
                .map_err(|e| WritError::Other(format!("failed to write git index: {e}")))?;
            Ok(outcome)
        }

        fn stage_files(&self, paths: &[&str]) -> WritResult<usize> {
            let repo = self.repo()?;
            let mut index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;

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

            index
                .write()
                .map_err(|e| WritError::Other(format!("failed to write git index: {e}")))?;
            Ok(count)
        }

        fn stage_all(&self) -> WritResult<usize> {
            let repo = self.repo()?;
            let mut index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;

            // Count entries before
            let before = index.len();

            index
                .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
                .map_err(|e| WritError::Other(format!("failed to stage all files: {e}")))?;

            // Remove deleted files from index
            index.update_all(["*"].iter(), None).map_err(|e| {
                WritError::Other(format!("failed to update index for deletions: {e}"))
            })?;

            index
                .write()
                .map_err(|e| WritError::Other(format!("failed to write git index: {e}")))?;

            let after = index.len();
            // Return approximate count (may differ from actual staged changes)
            Ok(if after >= before {
                after - before
            } else {
                before - after
            })
        }

        fn commit(&self, message: &str) -> WritResult<String> {
            let repo = self.repo()?;
            let sig = Self::default_signature(&repo)?;
            let mut index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;

            let tree_oid = index
                .write_tree()
                .map_err(|e| WritError::Other(format!("failed to write tree: {e}")))?;
            let tree = repo
                .find_tree(tree_oid)
                .map_err(|e| WritError::Other(format!("failed to find tree: {e}")))?;

            // Get parent commit (HEAD), if any
            let parent = match repo.head() {
                Ok(head) => Some(
                    head.peel_to_commit()
                        .map_err(|e| WritError::Other(format!("failed to resolve HEAD: {e}")))?,
                ),
                Err(_) => None, // Initial commit
            };

            let parents: Vec<&git2::Commit> = parent.as_ref().map(|p| vec![p]).unwrap_or_default();

            let oid = repo
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
                .map_err(|e| WritError::Other(format!("failed to create commit: {e}")))?;

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
                    let refname = branch
                        .get()
                        .name()
                        .ok_or_else(|| WritError::Other("branch ref has no name".into()))?;
                    repo.set_head(refname).map_err(|e| {
                        WritError::Other(format!("failed to set HEAD to {}: {e}", name))
                    })?;
                    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
                        .map_err(|e| {
                            WritError::Other(format!("failed to checkout {}: {e}", name))
                        })?;
                }
                Err(_) => {
                    // Create new branch from HEAD
                    let head = repo
                        .head()
                        .map_err(|e| WritError::Other(format!("no HEAD to branch from: {e}")))?;
                    let commit = head
                        .peel_to_commit()
                        .map_err(|e| WritError::Other(format!("HEAD is not a commit: {e}")))?;
                    let branch = repo.branch(name, &commit, false).map_err(|e| {
                        WritError::Other(format!("failed to create branch '{}': {e}", name))
                    })?;
                    let refname = branch
                        .get()
                        .name()
                        .ok_or_else(|| WritError::Other("new branch ref has no name".into()))?;
                    repo.set_head(refname)
                        .map_err(|e| WritError::Other(format!("failed to set HEAD: {e}")))?;
                    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
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
                Ok(head) => Some(
                    head.peel_to_tree()
                        .map_err(|e| WritError::Other(format!("failed to get HEAD tree: {e}")))?,
                ),
                Err(_) => None, // No commits — any staged content counts
            };

            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, None)
                .map_err(|e| WritError::Other(format!("failed to diff index: {e}")))?;

            Ok(diff.deltas().len() > 0)
        }

        fn root(&self) -> &Path {
            &self.root
        }

        fn is_tracked(&self, path: &str) -> WritResult<bool> {
            let repo = self.repo()?;
            let index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;
            Ok(index.get_path(Path::new(path), 0).is_some())
        }

        fn export_index(&self, dest: &Path) -> WritResult<usize> {
            let repo = self.repo()?;
            let index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;
            std::fs::create_dir_all(dest)?;
            let mut keep: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
            for entry in index.iter() {
                let rel = String::from_utf8_lossy(&entry.path).to_string();
                let blob = repo.find_blob(entry.id).map_err(|e| {
                    WritError::Other(format!("staged blob for '{rel}' not found: {e}"))
                })?;
                let target = dest.join(&rel);
                keep.insert(target.clone());
                // Unchanged files keep their mtime so incremental builds
                // in the scratch tree stay incremental.
                if std::fs::read(&target).ok().as_deref() != Some(blob.content()) {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&target, blob.content())?;
                }
                #[cfg(unix)]
                if entry.mode == 0o100755 {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            remove_unlisted(dest, &keep)?;
            Ok(keep.len())
        }

        fn head_hash(&self) -> WritResult<Option<String>> {
            let repo = self.repo()?;
            let hash = repo
                .head()
                .ok()
                .and_then(|h| h.peel_to_commit().ok())
                .map(|c| c.id().to_string());
            Ok(hash)
        }

        fn reset_index_to_head(&self) -> WritResult<()> {
            let repo = self.repo()?;
            let mut index = repo
                .index()
                .map_err(|e| WritError::Other(format!("failed to read git index: {e}")))?;
            match repo.head().and_then(|h| h.peel_to_tree()) {
                Ok(tree) => index
                    .read_tree(&tree)
                    .map_err(|e| WritError::Other(format!("failed to reset git index: {e}")))?,
                Err(_) => index
                    .clear()
                    .map_err(|e| WritError::Other(format!("failed to clear git index: {e}")))?,
            }
            index
                .write()
                .map_err(|e| WritError::Other(format!("failed to write git index: {e}")))
        }
    }
}

/// Delete files under `dir` that are not in `keep` (a previous export's
/// leftovers), leaving build output directories (`target`) alone.
#[cfg(feature = "bridge")]
fn remove_unlisted(dir: &Path, keep: &std::collections::HashSet<PathBuf>) -> WritResult<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            remove_unlisted(&path, keep)?;
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// True when git would refuse `git add path` as ignored: the path itself or
/// any ancestor directory matches an ignore rule. A directory excluded
/// wholesale hides negations inside it, as git does.
#[cfg(feature = "bridge")]
fn ignored_by_git(repo: &git2::Repository, path: &str) -> bool {
    let ignored = |p: &str| repo.is_path_ignored(Path::new(p)).unwrap_or(false);
    let mut prefix = String::new();
    for part in path
        .split('/')
        .collect::<Vec<_>>()
        .split_last()
        .map(|(_, dirs)| dirs)
        .unwrap_or(&[])
    {
        prefix.push_str(part);
        prefix.push('/');
        if ignored(&prefix) {
            return true;
        }
    }
    ignored(path)
}

/// Add `content` at `path` to the git index as a blob.
#[cfg(feature = "bridge")]
fn index_entry_add(
    index: &mut git2::Index,
    path: &str,
    mode: u32,
    content: &[u8],
) -> Result<(), String> {
    let entry = git2::IndexEntry {
        ctime: git2::IndexTime::new(0, 0),
        mtime: git2::IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: content.len() as u32,
        id: git2::Oid::zero(),
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    index
        .add_frombuffer(&entry, content)
        .map_err(|e| e.message().to_string())
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
        assert_eq!(ops.current_branch().unwrap().as_deref(), Some("feature-x"));

        // Switch back (assuming default was "master" or "main")
        // Create it explicitly first
        ops.checkout_or_create_branch("test-main").unwrap();
        assert_eq!(ops.current_branch().unwrap().as_deref(), Some("test-main"));

        // Switch to existing branch
        ops.checkout_or_create_branch("feature-x").unwrap();
        assert_eq!(ops.current_branch().unwrap().as_deref(), Some("feature-x"));
    }

    #[test]
    fn test_root_returns_path() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();
        assert_eq!(ops.root(), dir.path());
    }

    fn head_blob(repo: &git2::Repository, path: &str) -> Option<String> {
        let tree = repo.head().ok()?.peel_to_tree().ok()?;
        let entry = tree.get_path(Path::new(path)).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        Some(String::from_utf8_lossy(blob.content()).to_string())
    }

    #[test]
    fn export_index_writes_staged_content_not_disk_and_reset_unstages() {
        let dir = tempfile::tempdir().unwrap();
        let repo = init_git_repo(dir.path());
        fs::write(dir.path().join("a.txt"), "disk\n").unwrap();
        let ops = Git2Ops::open(dir.path()).unwrap();
        ops.stage_contents(&[StageEntry {
            path: "a.txt".into(),
            content: Some(b"staged\n".to_vec()),
        }])
        .unwrap();
        assert!(ops.is_tracked("a.txt").unwrap());
        assert!(!ops.is_tracked("missing.txt").unwrap());
        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("tree");

        let n = ops.export_index(&dest).unwrap();

        assert!(n >= 1);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "staged\n");
        fs::write(dest.join("stale.txt"), "old export").unwrap();
        ops.export_index(&dest).unwrap();
        assert!(!dest.join("stale.txt").exists(), "leftover not removed");
        ops.reset_index_to_head().unwrap();
        assert!(!ops.has_staged_changes().unwrap());
        drop(repo);
    }

    #[test]
    fn stage_contents_commits_given_content_not_disk() {
        let dir = tempdir().unwrap();
        let repo = init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "edited after seal").unwrap();
        let out = ops
            .stage_contents(&[StageEntry {
                path: "a.txt".into(),
                content: Some(b"sealed".to_vec()),
            }])
            .unwrap();
        assert_eq!(out.staged, vec!["a.txt".to_string()]);
        ops.commit("c").unwrap();
        assert_eq!(head_blob(&repo, "a.txt").as_deref(), Some("sealed"));
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "edited after seal",
            "working tree untouched"
        );
    }

    #[test]
    fn stage_contents_stages_deletions_of_tracked_files() {
        let dir = tempdir().unwrap();
        let repo = init_git_repo(dir.path());
        let ops = Git2Ops::open(dir.path()).unwrap();
        fs::write(dir.path().join("gone.txt"), "x").unwrap();
        ops.stage_files(&["gone.txt"]).unwrap();
        ops.commit("add").unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let out = ops
            .stage_contents(&[
                StageEntry {
                    path: "gone.txt".into(),
                    content: None,
                },
                StageEntry {
                    path: "never.txt".into(),
                    content: None,
                },
            ])
            .unwrap();
        assert_eq!(out.staged, vec!["gone.txt".to_string()]);
        ops.commit("rm").unwrap();
        assert!(head_blob(&repo, "gone.txt").is_none());
    }

    #[test]
    fn stage_contents_refuses_gitignored_untracked_path_with_reason() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        fs::write(dir.path().join(".gitignore"), "results/\n").unwrap();
        fs::create_dir_all(dir.path().join("results")).unwrap();
        // A negation inside an excluded directory does not un-ignore it.
        fs::write(dir.path().join("results/.gitignore"), "*\n!.gitignore\n").unwrap();
        let ops = Git2Ops::open(dir.path()).unwrap();
        let out = ops
            .stage_contents(&[StageEntry {
                path: "results/.gitignore".into(),
                content: Some(b"*".to_vec()),
            }])
            .unwrap();
        assert!(out.staged.is_empty());
        assert_eq!(out.refused.len(), 1);
        assert_eq!(out.refused[0].0, "results/.gitignore");
        assert!(out.refused[0].1.contains(".gitignore"), "{:?}", out.refused);
    }
}
