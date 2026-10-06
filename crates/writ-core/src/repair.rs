//! Store repair: regenerate referenced-but-missing objects (finding 28).
//!
//! `writ repair` is the recovery procedure run by hand on 2026-10-05, made a
//! command. It walks the live set exactly as gc does (every seal tree, every
//! seal change list, every spec genesis tree, every workspace index), lists
//! each referenced object that is absent from `.writ/objects/`, and tries to
//! regenerate it from content that still exists outside the store:
//!
//! 1. the working tree, at the path that references it;
//! 2. git history for that path, newest commit first.
//!
//! A candidate is accepted only when its SHA-256 equals the missing hash, so
//! repair can never write wrong content. Accepted content goes through the
//! normal `ObjectStore::store`, compressed with the project's storage
//! settings. Tree objects cannot be regenerated from either source; they are
//! reported, never guessed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::Serialize;

use crate::gc::{self, MissingObject, UnreadableTree};
use crate::hash::hash_bytes;
use crate::object::ObjectStore;
use crate::{WritError, WritResult};

/// Where a recovered object's content came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoverySource {
    /// The file currently on disk at the referencing path.
    WorkingTree,
    /// The blob at the referencing path in this git commit.
    Git { commit: String },
}

impl std::fmt::Display for RecoverySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoverySource::WorkingTree => write!(f, "working tree"),
            RecoverySource::Git { commit } => {
                write!(f, "git {}", &commit[..commit.len().min(12)])
            }
        }
    }
}

/// A missing object whose content was found and verified.
#[derive(Debug, Clone, Serialize)]
pub struct RecoveredObject {
    pub hash: String,
    /// The path (or root) that references it.
    pub path: String,
    pub source: RecoverySource,
}

/// A missing object no source could regenerate.
#[derive(Debug, Clone, Serialize)]
pub struct UnrecoverableObject {
    pub hash: String,
    /// The path (or root) that references it.
    pub path: String,
    /// Every path that references it; each was searched.
    pub searched_paths: Vec<String>,
    pub reason: String,
}

/// Outcome of a repair run.
#[derive(Debug, Clone, Serialize)]
pub struct RepairReport {
    /// True when nothing was written.
    pub dry_run: bool,
    /// Layout fixes made first (or, on a dry run, that would be made).
    pub layout: Vec<String>,
    /// True when a dry run could not scan the store because of a layout
    /// problem a real run fixes first.
    pub store_scan_skipped: bool,
    /// Every referenced object that was missing when the run started.
    pub missing: Vec<MissingObject>,
    /// Missing objects regenerated (or, on a dry run, regenerable).
    pub recovered: Vec<RecoveredObject>,
    /// Missing objects no source could regenerate.
    pub unrecoverable: Vec<UnrecoverableObject>,
    /// Trees that exist but cannot be decoded. Repair cannot fix these, and
    /// blobs they list are not visible to the scan.
    pub unreadable_trees: Vec<UnreadableTree>,
}

impl RepairReport {
    /// True when nothing is (or, on a dry run, would be) left missing or
    /// unreadable. A dry run reports what a real run would leave, so
    /// scripts can ask "can this be fixed" before fixing.
    pub fn is_clean(&self) -> bool {
        self.unrecoverable.is_empty() && self.unreadable_trees.is_empty()
    }
}

/// Find every referenced-but-missing object under `writ_dir` and regenerate
/// what can be recovered from the working tree at `root` or its git history.
///
/// With `dry_run`, sources are still searched and verified so the report
/// says what a real run would recover, but nothing is written.
pub fn repair_store(root: &Path, writ_dir: &Path, dry_run: bool) -> WritResult<RepairReport> {
    let blocked = layout_problems(writ_dir)
        .iter()
        .any(LayoutProblem::blocks_store_scan);
    let layout = repair_layout(writ_dir, dry_run)?;
    if dry_run && blocked {
        return Ok(RepairReport {
            dry_run,
            layout,
            store_scan_skipped: true,
            missing: Vec::new(),
            recovered: Vec::new(),
            unrecoverable: Vec::new(),
            unreadable_trees: Vec::new(),
        });
    }
    let check = gc::check_store(writ_dir)?;
    let mut report = RepairReport {
        dry_run,
        layout,
        store_scan_skipped: false,
        missing: check.missing_objects.clone(),
        recovered: Vec::new(),
        unrecoverable: Vec::new(),
        unreadable_trees: check.unreadable_trees,
    };
    if report.missing.is_empty() {
        return Ok(report);
    }

    let tree_roots = root_tree_hashes(writ_dir)?;
    let store = object_store(writ_dir);

    let (trees, blobs): (Vec<&MissingObject>, Vec<&MissingObject>) = check
        .missing_objects
        .iter()
        .partition(|m| tree_roots.contains(&m.hash));
    for m in trees {
        report.unrecoverable.push(UnrecoverableObject {
            hash: m.hash.clone(),
            path: m.referenced_as.clone(),
            searched_paths: Vec::new(),
            reason: "tree object: no source holds its contents; blobs it lists \
                     are not checked"
                .to_string(),
        });
    }

    // Every path that references each missing blob (a renamed file is the
    // common case where only a later path has the history), then grouped by
    // path so each path's history is read at most once.
    let missing_blobs: HashSet<&str> = blobs.iter().map(|m| m.hash.as_str()).collect();
    let mut paths_of = referencing_paths(writ_dir, &missing_blobs)?;
    for m in &blobs {
        let paths = paths_of.entry(m.hash.clone()).or_default();
        if !paths.contains(&m.referenced_as) {
            paths.insert(0, m.referenced_as.clone());
        }
    }
    let mut by_path: HashMap<&str, Vec<&str>> = HashMap::new();
    for (hash, paths) in &paths_of {
        for path in paths {
            by_path
                .entry(path.as_str())
                .or_default()
                .push(hash.as_str());
        }
    }
    let mut paths: Vec<&str> = by_path.keys().copied().collect();
    paths.sort_unstable();

    let mut remaining: HashSet<&str> = missing_blobs.clone();
    let mut git = GitHistory::open(root);

    for path in paths {
        let mut wanted: HashSet<&str> = by_path[path]
            .iter()
            .copied()
            .filter(|h| remaining.contains(h))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let mut found: Vec<(String, Vec<u8>, RecoverySource)> = Vec::new();

        if let Some(data) = read_working_file(root, path) {
            take_if_wanted(&mut wanted, data, RecoverySource::WorkingTree, &mut found);
        }
        if !wanted.is_empty() {
            if let Some(git) = git.as_mut() {
                git.search(path, &mut wanted, &mut found)?;
            }
        }

        for (hash, data, source) in found {
            if !dry_run {
                let stored = store.store(&data)?;
                if stored != hash {
                    return Err(WritError::Other(format!(
                        "repair: store returned {stored} for verified content {hash}"
                    )));
                }
            }
            remaining.remove(hash.as_str());
            report.recovered.push(RecoveredObject {
                hash,
                path: path.to_string(),
                source,
            });
        }
    }

    let reason = match &git {
        Some(_) => "not in the working tree or any git commit touching any referencing path",
        None => "not in the working tree; no git history available",
    };
    for m in blobs {
        if remaining.contains(m.hash.as_str()) {
            report.unrecoverable.push(UnrecoverableObject {
                hash: m.hash.clone(),
                path: m.referenced_as.clone(),
                searched_paths: paths_of.remove(&m.hash).unwrap_or_default(),
                reason: reason.to_string(),
            });
        }
    }

    report.recovered.sort_by(|a, b| a.hash.cmp(&b.hash));
    report.unrecoverable.sort_by(|a, b| a.hash.cmp(&b.hash));
    Ok(report)
}

/// Every path under which a seal change, a readable seal or genesis tree, or
/// a workspace index references one of `wanted`. The live-set scan keeps one
/// path per hash; repair needs them all.
fn referencing_paths(
    writ_dir: &Path,
    wanted: &HashSet<&str>,
) -> WritResult<HashMap<String, Vec<String>>> {
    let mut paths: HashMap<String, Vec<String>> = HashMap::new();
    let mut add = |hash: &str, path: &str| {
        if wanted.contains(hash) {
            let list = paths.entry(hash.to_string()).or_default();
            if !list.iter().any(|p| p == path) {
                list.push(path.to_string());
            }
        }
    };
    if wanted.is_empty() {
        return Ok(paths);
    }

    let store = ObjectStore::new(&writ_dir.join("objects"));
    let mut trees: Vec<String> = Vec::new();
    for seal in gc::load_all_seals(writ_dir)? {
        for change in &seal.changes {
            for hash in [&change.old_hash, &change.new_hash].into_iter().flatten() {
                add(hash, &change.path);
            }
        }
        trees.push(seal.tree);
    }
    trees.extend(
        gc::load_all_specs(writ_dir)?
            .into_iter()
            .filter_map(|s| s.genesis_tree),
    );
    trees.sort_unstable();
    trees.dedup();
    for tree in trees.iter().filter(|t| !t.is_empty()) {
        // Missing or unreadable trees are already reported by check_store.
        let Ok(data) = store.retrieve(tree) else {
            continue;
        };
        let Ok(map) = serde_json::from_slice::<HashMap<String, serde_json::Value>>(&data) else {
            continue;
        };
        for (path, value) in &map {
            let hash = match value {
                serde_json::Value::String(h) => Some(h.as_str()),
                serde_json::Value::Object(o) => o.get("hash").and_then(|h| h.as_str()),
                _ => None,
            };
            if let Some(hash) = hash {
                add(hash, path);
            }
        }
    }

    let ws_dir = writ_dir.join("workspaces");
    if ws_dir.is_dir() {
        for entry in fs::read_dir(&ws_dir)? {
            let index_path = entry?.path().join("index.json");
            if index_path.is_file() {
                let index = crate::index::Index::load(&index_path)?;
                for (file, e) in &index.entries {
                    add(&e.hash, file);
                }
            }
        }
    }
    Ok(paths)
}

/// If `data` hashes to a wanted object, move it from `wanted` to `found`.
fn take_if_wanted(
    wanted: &mut HashSet<&str>,
    data: Vec<u8>,
    source: RecoverySource,
    found: &mut Vec<(String, Vec<u8>, RecoverySource)>,
) {
    let hash = hash_bytes(&data);
    if wanted.remove(hash.as_str()) {
        found.push((hash, data, source));
    }
}

/// Seal trees and spec genesis trees: the only tree objects the live set
/// references. A missing one cannot be rebuilt from file content.
fn root_tree_hashes(writ_dir: &Path) -> WritResult<HashSet<String>> {
    let mut trees: HashSet<String> = gc::load_all_seals(writ_dir)?
        .into_iter()
        .map(|s| s.tree)
        .filter(|t| !t.is_empty())
        .collect();
    trees.extend(
        gc::load_all_specs(writ_dir)?
            .into_iter()
            .filter_map(|s| s.genesis_tree),
    );
    Ok(trees)
}

/// The object store with the project's compression settings, matching
/// `Repository::open`.
fn object_store(writ_dir: &Path) -> ObjectStore {
    let storage = gc::GcConfig::load(writ_dir)
        .map(|c| c.storage)
        .unwrap_or_default();
    let level = if storage.compression == "none" {
        0
    } else {
        storage.compression_level
    };
    ObjectStore::with_config(
        &writ_dir.join("objects"),
        level,
        storage.max_object_size_bytes,
    )
}

/// Read `root/path` if it is a regular file inside `root`.
fn read_working_file(root: &Path, path: &str) -> Option<Vec<u8>> {
    let rel = Path::new(path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    let full = root.join(rel);
    if !full.is_file() {
        return None;
    }
    fs::read(full).ok()
}

/// Read access to git history through the `git` binary: `git log` to list
/// the commits touching a path, one long-lived `git cat-file --batch` to
/// read blobs.
struct GitHistory {
    root: PathBuf,
    /// Path of `root` inside the git work tree (`rev-parse --show-prefix`).
    prefix: String,
    cat_file: Option<CatFile>,
}

impl GitHistory {
    /// None when `git` is unavailable or `root` is not in a git work tree.
    fn open(root: &Path) -> Option<Self> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--show-prefix"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let prefix = String::from_utf8(out.stdout).ok()?.trim_end().to_string();
        Some(Self {
            root: root.to_path_buf(),
            prefix,
            cat_file: None,
        })
    }

    /// Commits on any ref that touched `path`, newest first.
    fn commits_touching(&self, path: &str) -> WritResult<Vec<String>> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["--literal-pathspecs", "log", "--all", "--format=%H", "--"])
            .arg(path)
            .stderr(Stdio::null())
            .output()
            .map_err(|e| WritError::Other(format!("repair: cannot run git log: {e}")))?;
        if !out.status.success() {
            // No commits yet, or a path git rejects: nothing to search.
            return Ok(Vec::new());
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Walk `path`'s history newest first, moving each wanted hash found
    /// into `found`. Stops as soon as nothing is wanted.
    fn search(
        &mut self,
        path: &str,
        wanted: &mut HashSet<&str>,
        found: &mut Vec<(String, Vec<u8>, RecoverySource)>,
    ) -> WritResult<()> {
        // cat-file --batch reads one request per line.
        if path.contains('\n') {
            return Ok(());
        }
        let commits = self.commits_touching(path)?;
        let mut seen_blobs: HashSet<String> = HashSet::new();
        for commit in commits {
            if wanted.is_empty() {
                break;
            }
            let spec = format!("{commit}:{}{path}", self.prefix);
            let Some((oid, data)) = self.cat_file()?.blob(&spec)? else {
                continue; // deleted in this commit, or not a blob
            };
            if seen_blobs.insert(oid) {
                take_if_wanted(wanted, data, RecoverySource::Git { commit }, found);
            }
        }
        Ok(())
    }

    fn cat_file(&mut self) -> WritResult<&mut CatFile> {
        if self.cat_file.is_none() {
            self.cat_file = Some(CatFile::spawn(&self.root)?);
        }
        Ok(self.cat_file.as_mut().expect("just spawned"))
    }
}

/// A running `git cat-file --batch`.
struct CatFile {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CatFile {
    fn spawn(root: &Path) -> WritResult<Self> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| WritError::Other(format!("repair: cannot run git cat-file: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    /// Fetch `<rev>:<path>`. Returns the git object id and content for a
    /// blob, None for anything else (missing, ambiguous, tree, submodule).
    fn blob(&mut self, spec: &str) -> WritResult<Option<(String, Vec<u8>)>> {
        let io_err = |e: std::io::Error| WritError::Other(format!("repair: git cat-file: {e}"));
        writeln!(self.stdin, "{spec}").map_err(io_err)?;
        self.stdin.flush().map_err(io_err)?;

        let mut header = String::new();
        if self.stdout.read_line(&mut header).map_err(io_err)? == 0 {
            return Err(WritError::Other(
                "repair: git cat-file exited unexpectedly".to_string(),
            ));
        }
        // "<oid> <type> <size>" on success; "<spec> missing" or
        // "<spec> ambiguous" otherwise. The spec may contain spaces, so
        // parse from the right.
        let header = header.trim_end_matches('\n');
        let mut parts = header.rsplitn(3, ' ');
        let (Some(size), Some(kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            return Ok(None);
        };
        let Ok(size) = size.parse::<usize>() else {
            return Ok(None); // "missing" / "ambiguous": no body follows
        };
        // Body plus the trailing newline.
        let mut body = vec![0u8; size + 1];
        self.stdout.read_exact(&mut body).map_err(io_err)?;
        body.truncate(size);
        Ok((kind == "blob").then(|| (oid.to_string(), body)))
    }
}

impl Drop for CatFile {
    fn drop(&mut self) {
        // Closing stdin ends the batch; reap the child.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
    use crate::Repository;
    use tempfile::{tempdir, TempDir};

    fn seal(repo: &Repository, summary: &str) -> crate::seal::Seal {
        repo.seal(
            AgentIdentity {
                id: "haris-test".to_string(),
                agent_type: AgentType::Agent,
            },
            summary.to_string(),
            None,
            TaskStatus::InProgress,
            Verification::default(),
            false,
        )
        .unwrap()
    }

    fn object_file(writ_dir: &Path, hash: &str) -> PathBuf {
        writ_dir.join("objects").join(&hash[..2]).join(&hash[2..])
    }

    fn delete_object(writ_dir: &Path, hash: &str) {
        fs::remove_file(object_file(writ_dir, hash)).unwrap();
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn h(content: &str) -> String {
        hash_bytes(content.as_bytes())
    }

    /// A writ repo (no git) with two sealed files.
    fn sealed_repo() -> (TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.path().join("b.txt"), "beta\n").unwrap();
        seal(&repo, "baseline");
        (dir, repo)
    }

    #[test]
    fn test_clean_store_reports_nothing_missing() {
        let (dir, repo) = sealed_repo();
        let report = repair_store(dir.path(), repo.writ_dir(), false).unwrap();
        assert!(report.missing.is_empty());
        assert!(report.recovered.is_empty());
        assert!(report.is_clean());
    }

    #[test]
    fn test_recovers_from_working_tree_and_writes_compressed_object() {
        let (dir, repo) = sealed_repo();
        let hash = h("alpha\n");
        delete_object(repo.writ_dir(), &hash);

        let report = repair_store(dir.path(), repo.writ_dir(), false).unwrap();

        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].referenced_as, "a.txt");
        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].hash, hash);
        assert_eq!(report.recovered[0].path, "a.txt");
        assert_eq!(report.recovered[0].source, RecoverySource::WorkingTree);
        assert!(report.is_clean());
        // Written through ObjectStore::store: compressed, and reads back.
        let stored = fs::read(object_file(repo.writ_dir(), &hash)).unwrap();
        assert!(crate::object::is_compressed(&stored));
        let store = ObjectStore::new(&repo.writ_dir().join("objects"));
        assert_eq!(store.retrieve(&hash).unwrap(), b"alpha\n");
        assert!(gc::check_store(repo.writ_dir()).unwrap().is_clean());
    }

    #[test]
    fn test_dry_run_lists_but_writes_nothing() {
        let (dir, repo) = sealed_repo();
        let hash = h("alpha\n");
        delete_object(repo.writ_dir(), &hash);

        let report = repair_store(dir.path(), repo.writ_dir(), true).unwrap();

        assert!(report.dry_run);
        assert_eq!(
            report.recovered.len(),
            1,
            "dry run still says what is recoverable"
        );
        assert!(!object_file(repo.writ_dir(), &hash).exists());
        assert!(
            report.is_clean(),
            "dry run is clean when a real run would recover everything"
        );
    }

    #[test]
    fn test_modified_working_file_is_not_written() {
        // The working copy changed since the seal: its hash does not match,
        // so repair must refuse it rather than store the wrong content.
        let (dir, repo) = sealed_repo();
        let hash = h("alpha\n");
        delete_object(repo.writ_dir(), &hash);
        fs::write(dir.path().join("a.txt"), "alpha, edited\n").unwrap();

        let report = repair_store(dir.path(), repo.writ_dir(), false).unwrap();

        assert!(report.recovered.is_empty());
        assert_eq!(report.unrecoverable.len(), 1);
        assert_eq!(report.unrecoverable[0].hash, hash);
        assert_eq!(report.unrecoverable[0].path, "a.txt");
        assert!(report.unrecoverable[0].reason.contains("no git history"));
        assert!(!object_file(repo.writ_dir(), &hash).exists());
        assert!(!object_file(repo.writ_dir(), &h("alpha, edited\n")).exists());
        assert!(!report.is_clean());
    }

    #[test]
    fn test_recovers_superseded_version_from_git_history() {
        // a.txt has three committed versions; writ sealed v2 and the working
        // tree is now v3. Only git history at the middle commit holds v2.
        let dir = tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        for v in ["v1\n", "v2\n"] {
            fs::write(root.join("a.txt"), v).unwrap();
            git(root, &["add", "a.txt"]);
            git(root, &["commit", "-qm", v.trim()]);
        }
        let repo = Repository::init(root).unwrap();
        seal(&repo, "sealed at v2");
        fs::write(root.join("a.txt"), "v3\n").unwrap();
        git(root, &["commit", "-qam", "v3"]);
        delete_object(repo.writ_dir(), &h("v2\n"));

        let report = repair_store(root, repo.writ_dir(), false).unwrap();

        assert_eq!(report.recovered.len(), 1, "{report:?}");
        assert!(matches!(
            report.recovered[0].source,
            RecoverySource::Git { .. }
        ));
        assert!(report.is_clean());
        assert!(gc::check_store(repo.writ_dir()).unwrap().is_clean());
    }

    #[test]
    fn test_writ_root_in_git_subdirectory_uses_prefix() {
        let dir = tempdir().unwrap();
        let top = dir.path();
        let root = top.join("sub");
        fs::create_dir(&root).unwrap();
        git(top, &["init", "-q"]);
        fs::write(root.join("a.txt"), "committed\n").unwrap();
        git(top, &["add", "sub/a.txt"]);
        git(top, &["commit", "-qm", "base"]);
        let repo = Repository::init(&root).unwrap();
        seal(&repo, "baseline");
        fs::write(root.join("a.txt"), "edited later\n").unwrap();
        delete_object(repo.writ_dir(), &h("committed\n"));

        let report = repair_store(&root, repo.writ_dir(), false).unwrap();

        assert_eq!(report.recovered.len(), 1, "{report:?}");
        assert!(report.is_clean());
    }

    #[test]
    fn test_three_junk_blobs_are_unrecoverable_and_the_rest_recovered() {
        // The writ repo's own shape: three referenced blobs whose files are
        // gone from disk and were never committed (the .venv311 dist-info
        // files and the import-time .so), next to blobs git still has.
        let dir = tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        fs::write(root.join("kept.txt"), "in git\n").unwrap();
        git(root, &["add", "kept.txt"]);
        git(root, &["commit", "-qm", "base"]);

        let repo = Repository::init(root).unwrap();
        let junk = [
            (".venv311/pkg-1.0.dist-info/RECORD", "record\n"),
            (".venv311/pkg-1.0.dist-info/METADATA", "metadata\n"),
            ("python/writ/_native.abi3.so", "\x7fELF not really\n"),
        ];
        for (path, content) in junk {
            let p = root.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        seal(&repo, "baseline with junk");
        for (path, content) in junk {
            fs::remove_file(root.join(path)).unwrap();
            delete_object(repo.writ_dir(), &h(content));
        }
        fs::write(root.join("kept.txt"), "edited since\n").unwrap();
        delete_object(repo.writ_dir(), &h("in git\n"));

        let report = repair_store(root, repo.writ_dir(), false).unwrap();

        assert_eq!(report.missing.len(), 4);
        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].path, "kept.txt");
        let mut lost: Vec<&str> = report
            .unrecoverable
            .iter()
            .map(|u| u.path.as_str())
            .collect();
        lost.sort_unstable();
        let mut expected: Vec<&str> = junk.iter().map(|(p, _)| *p).collect();
        expected.sort_unstable();
        assert_eq!(lost, expected);
        for u in &report.unrecoverable {
            assert!(u.reason.contains("any git commit"), "{}", u.reason);
            assert_eq!(u.searched_paths, vec![u.path.clone()]);
            assert!(!object_file(repo.writ_dir(), &u.hash).exists());
        }
        assert!(!report.is_clean(), "three objects remain missing");

        // A second run is idempotent: the same three, nothing new recovered.
        let again = repair_store(root, repo.writ_dir(), false).unwrap();
        assert_eq!(again.missing.len(), 3);
        assert!(again.recovered.is_empty());
        assert_eq!(again.unrecoverable.len(), 3);
    }

    #[test]
    fn test_missing_seal_tree_is_reported_not_guessed() {
        let (dir, repo) = sealed_repo();
        let s = seal_with_change(&repo, dir.path());
        delete_object(repo.writ_dir(), &s.tree);

        let report = repair_store(dir.path(), repo.writ_dir(), false).unwrap();

        let tree = report
            .unrecoverable
            .iter()
            .find(|u| u.hash == s.tree)
            .expect("missing tree reported");
        assert!(tree.path.starts_with("tree of seal"), "{}", tree.path);
        assert!(tree.reason.starts_with("tree object"));
        assert!(!report.is_clean());
    }

    fn seal_with_change(repo: &Repository, root: &Path) -> crate::seal::Seal {
        fs::write(root.join("c.txt"), "gamma\n").unwrap();
        seal(repo, "add c")
    }

    #[test]
    fn test_working_tree_path_escape_is_ignored() {
        assert!(read_working_file(Path::new("/tmp"), "../etc/passwd").is_none());
        assert!(read_working_file(Path::new("/tmp"), "/etc/passwd").is_none());
    }

    #[test]
    fn test_dry_run_is_not_clean_when_something_would_remain_missing() {
        // One recoverable blob (a.txt still on disk) and one that is gone
        // everywhere (b.txt deleted, no git): a real run would leave b.txt
        // missing, so the dry run must say so.
        let (dir, repo) = sealed_repo();
        delete_object(repo.writ_dir(), &h("alpha\n"));
        delete_object(repo.writ_dir(), &h("beta\n"));
        fs::remove_file(dir.path().join("b.txt")).unwrap();

        let report = repair_store(dir.path(), repo.writ_dir(), true).unwrap();

        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].path, "a.txt");
        assert_eq!(report.unrecoverable.len(), 1);
        assert_eq!(report.unrecoverable[0].path, "b.txt");
        assert!(!report.is_clean());
        assert!(!object_file(repo.writ_dir(), &h("alpha\n")).exists());
    }

    #[test]
    fn test_searches_every_referencing_path_not_only_the_first() {
        // A renamed file: writ sealed the content as a_old.txt, git only ever
        // saw it as z_new.txt (referenced here by the workspace index, which
        // the live-set scan visits after seals, so a_old.txt is the primary
        // path). Searching only the primary path finds nothing.
        let dir = tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git(root, &["add", "seed.txt"]);
        git(root, &["commit", "-qm", "seed"]);

        let repo = Repository::init(root).unwrap();
        fs::write(root.join("a_old.txt"), "renamed content\n").unwrap();
        seal(&repo, "as a_old");
        fs::remove_file(root.join("a_old.txt")).unwrap();

        fs::write(root.join("z_new.txt"), "renamed content\n").unwrap();
        git(root, &["add", "z_new.txt"]);
        git(root, &["commit", "-qm", "as z_new"]);
        fs::write(root.join("z_new.txt"), "edited after the rename\n").unwrap();
        git(root, &["commit", "-qam", "edit"]);

        let hash = h("renamed content\n");
        let index_path = repo.writ_dir().join("workspaces/main/index.json");
        let mut index = crate::index::Index::load(&index_path).unwrap();
        index.upsert("z_new.txt", hash.clone(), 16);
        index.save(&index_path).unwrap();
        delete_object(repo.writ_dir(), &hash);

        let report = repair_store(root, repo.writ_dir(), false).unwrap();

        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].referenced_as, "a_old.txt");
        assert_eq!(report.recovered.len(), 1, "{report:?}");
        assert_eq!(report.recovered[0].path, "z_new.txt");
        assert!(matches!(
            report.recovered[0].source,
            RecoverySource::Git { .. }
        ));
        assert!(report.is_clean());
        assert!(gc::check_store(repo.writ_dir()).unwrap().is_clean());
    }
}

// ---------------------------------------------------------------------------
// Layout repair (sprint 3, doctor-core)
// ---------------------------------------------------------------------------

/// Directories every `.writ` must have; an empty one is valid.
pub const LAYOUT_DIRS: [&str; 6] = [
    "objects",
    "seals",
    "specs",
    "keys",
    "agents",
    "workspaces/main/heads",
];

/// Where `writ repair` moves records it cannot parse. Moved, never deleted.
pub const QUARANTINE_DIR: &str = "quarantine";

/// One thing wrong with the `.writ` layout that `writ repair` fixes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutProblem {
    /// A required directory is absent: repair creates it.
    MissingDir { path: String },
    /// `version.toml` absent or unparseable: repair writes a current one
    /// (moving a corrupt file aside first).
    VersionFile { reason: String },
    /// Main workspace `HEAD` absent: repair points it at the newest seal no
    /// other seal names as parent (empty when there are no seals).
    MissingHead,
    /// Main workspace index absent or unparseable: repair rebuilds it from
    /// the HEAD seal's tree (empty when HEAD is empty).
    Index { reason: String },
    /// `config.toml` unparseable: repair moves it aside (defaults apply).
    Config { reason: String },
    /// A seal or spec record that does not parse: repair moves it to
    /// `.writ/quarantine/`.
    UnparseableRecord { path: String, reason: String },
}

impl LayoutProblem {
    /// One line naming the problem.
    pub fn describe(&self) -> String {
        match self {
            LayoutProblem::MissingDir { path } => format!("missing directory .writ/{path}"),
            LayoutProblem::VersionFile { reason } => format!(".writ/version.toml: {reason}"),
            LayoutProblem::MissingHead => "missing .writ/workspaces/main/HEAD".to_string(),
            LayoutProblem::Index { reason } => {
                format!(".writ/workspaces/main/index.json: {reason}")
            }
            LayoutProblem::Config { reason } => format!(".writ/config.toml: {reason}"),
            LayoutProblem::UnparseableRecord { path, reason } => {
                format!("unparseable record .writ/{path}: {reason}")
            }
        }
    }

    /// The `.writ`-relative path the problem is about.
    pub fn path(&self) -> String {
        match self {
            LayoutProblem::MissingDir { path } | LayoutProblem::UnparseableRecord { path, .. } => {
                format!(".writ/{path}")
            }
            LayoutProblem::VersionFile { .. } => ".writ/version.toml".into(),
            LayoutProblem::MissingHead => ".writ/workspaces/main/HEAD".into(),
            LayoutProblem::Index { .. } => ".writ/workspaces/main/index.json".into(),
            LayoutProblem::Config { .. } => ".writ/config.toml".into(),
        }
    }

    /// True when the store scan cannot run until this is fixed.
    pub fn blocks_store_scan(&self) -> bool {
        matches!(
            self,
            LayoutProblem::UnparseableRecord { .. }
                | LayoutProblem::MissingDir { .. }
                | LayoutProblem::Index { .. }
        )
    }
}

/// Inspect the `.writ` layout. Read only.
pub fn layout_problems(writ_dir: &Path) -> Vec<LayoutProblem> {
    let mut out = Vec::new();
    for dir in LAYOUT_DIRS {
        if !writ_dir.join(dir).is_dir() {
            out.push(LayoutProblem::MissingDir { path: dir.into() });
        }
    }
    match crate::migrate::RepoVersion::load(writ_dir) {
        Ok(Some(_)) => {}
        Ok(None) => out.push(LayoutProblem::VersionFile {
            reason: "missing".into(),
        }),
        Err(e) => out.push(LayoutProblem::VersionFile {
            reason: format!("unparseable ({e})"),
        }),
    }
    let ws = writ_dir.join("workspaces").join("main");
    if ws.is_dir() && !ws.join("HEAD").is_file() {
        out.push(LayoutProblem::MissingHead);
    }
    let index = ws.join("index.json");
    if ws.is_dir() {
        match fs::read_to_string(&index) {
            Err(_) => out.push(LayoutProblem::Index {
                reason: "missing".into(),
            }),
            Ok(data) => {
                if let Err(e) = serde_json::from_str::<crate::index::Index>(&data) {
                    out.push(LayoutProblem::Index {
                        reason: format!("unparseable ({e})"),
                    });
                }
            }
        }
    }
    if let Ok(data) = fs::read_to_string(writ_dir.join("config.toml")) {
        if let Err(e) = toml::from_str::<crate::config::ProjectConfig>(&data) {
            out.push(LayoutProblem::Config {
                reason: format!("unparseable ({e})"),
            });
        }
    }
    out.extend(unparseable_records::<crate::seal::Seal>(writ_dir, "seals"));
    out.extend(unparseable_records::<crate::spec::Spec>(writ_dir, "specs"));
    out
}

fn unparseable_records<T: serde::de::DeserializeOwned>(
    writ_dir: &Path,
    dir: &str,
) -> Vec<LayoutProblem> {
    let Ok(entries) = fs::read_dir(writ_dir.join(dir)) else {
        return Vec::new();
    };
    let mut out: Vec<LayoutProblem> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .filter_map(|p| {
            let err = match fs::read_to_string(&p) {
                Ok(data) => serde_json::from_str::<T>(&data).err()?.to_string(),
                Err(e) => e.to_string(),
            };
            let name = p.file_name()?.to_string_lossy().to_string();
            Some(LayoutProblem::UnparseableRecord {
                path: format!("{dir}/{name}"),
                reason: err,
            })
        })
        .collect();
    out.sort_by_key(|p| p.path());
    out
}

/// Fix every layout problem (or, with `dry_run`, only list them). Returns
/// one line per action taken (or that would be taken).
pub fn repair_layout(writ_dir: &Path, dry_run: bool) -> WritResult<Vec<String>> {
    let problems = layout_problems(writ_dir);
    let mut actions = Vec::new();
    // Records first, so HEAD and index rebuilds only see parseable seals.
    let (records, rest): (Vec<_>, Vec<_>) = problems
        .into_iter()
        .partition(|p| matches!(p, LayoutProblem::UnparseableRecord { .. }));
    for p in records.iter().chain(rest.iter()) {
        let action = match p {
            LayoutProblem::MissingDir { path } => {
                if !dry_run {
                    fs::create_dir_all(writ_dir.join(path))?;
                }
                format!("created .writ/{path}")
            }
            LayoutProblem::UnparseableRecord { path, .. } => {
                let to = writ_dir.join(QUARANTINE_DIR).join(path);
                if !dry_run {
                    if let Some(parent) = to.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::rename(writ_dir.join(path), &to)?;
                }
                format!("moved .writ/{path} to .writ/{QUARANTINE_DIR}/{path}")
            }
            LayoutProblem::VersionFile { .. } => {
                if !dry_run {
                    move_aside(&crate::migrate::RepoVersion::path(writ_dir))?;
                    crate::migrate::RepoVersion::new().save(writ_dir)?;
                }
                "wrote a current .writ/version.toml".to_string()
            }
            LayoutProblem::Config { .. } => {
                if !dry_run {
                    move_aside(&writ_dir.join("config.toml"))?;
                }
                "moved .writ/config.toml to .writ/config.toml.corrupt (defaults apply)".to_string()
            }
            LayoutProblem::MissingHead => {
                let head = newest_tip_seal(writ_dir)?;
                if !dry_run {
                    let ws = writ_dir.join("workspaces").join("main");
                    fs::create_dir_all(&ws)?;
                    fs::write(ws.join("HEAD"), head.clone().unwrap_or_default())?;
                }
                match head {
                    Some(id) => format!(
                        "set main HEAD to newest tip seal {}",
                        &id[..id.len().min(12)]
                    ),
                    None => "wrote an empty main HEAD (no seals)".to_string(),
                }
            }
            LayoutProblem::Index { .. } => {
                let ws = writ_dir.join("workspaces").join("main");
                let head = fs::read_to_string(ws.join("HEAD"))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .or(newest_tip_seal(writ_dir)?);
                let index = match &head {
                    Some(id) => index_from_seal(writ_dir, id)?,
                    None => crate::index::Index::default(),
                };
                if !dry_run {
                    fs::create_dir_all(&ws)?;
                    move_aside(&ws.join("index.json"))?;
                    index.save(&ws.join("index.json"))?;
                }
                format!(
                    "rebuilt main index from {} ({} entries)",
                    head.as_deref()
                        .map(|h| format!("seal {}", &h[..h.len().min(12)]))
                        .unwrap_or_else(|| "nothing".into()),
                    index.entries.len()
                )
            }
        };
        actions.push(action);
    }
    Ok(actions)
}

/// Rename `path` to `<path>.corrupt` if it exists.
fn move_aside(path: &Path) -> WritResult<()> {
    if path.exists() {
        let mut to = path.as_os_str().to_owned();
        to.push(".corrupt");
        fs::rename(path, PathBuf::from(to))?;
    }
    Ok(())
}

/// The newest seal (by timestamp) that no other seal names as its parent.
/// Unparseable seal records are skipped (they are quarantined first on a
/// real run).
fn newest_tip_seal(writ_dir: &Path) -> WritResult<Option<String>> {
    let seals: Vec<crate::seal::Seal> = match fs::read_dir(writ_dir.join("seals")) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .filter_map(|p| fs::read_to_string(p).ok())
            .filter_map(|d| serde_json::from_str(&d).ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    let parents: HashSet<&str> = seals.iter().filter_map(|s| s.parent.as_deref()).collect();
    Ok(seals
        .iter()
        .filter(|s| !parents.contains(s.id.as_str()))
        .max_by_key(|s| s.timestamp)
        .map(|s| s.id.clone()))
}

/// The index recorded by a seal's tree.
fn index_from_seal(writ_dir: &Path, seal_id: &str) -> WritResult<crate::index::Index> {
    let data = fs::read_to_string(writ_dir.join("seals").join(format!("{seal_id}.json")))?;
    let seal: crate::seal::Seal = serde_json::from_str(&data)?;
    let tree = object_store(writ_dir).retrieve(&seal.tree)?;
    let entries = serde_json::from_slice(&tree)?;
    Ok(crate::index::Index { entries })
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn repair_layout_fixes_every_problem_it_lists() {
        let dir = tempfile::tempdir().unwrap();
        let repo = crate::Repository::init(dir.path()).unwrap();
        let w = repo.writ_dir().to_path_buf();
        fs::remove_dir_all(w.join("specs")).unwrap();
        fs::remove_file(w.join("workspaces/main/index.json")).unwrap();
        fs::write(w.join("seals/bad.json"), "{not json").unwrap();
        fs::write(w.join("config.toml"), "[[[").unwrap();
        assert_eq!(layout_problems(&w).len(), 4, "{:?}", layout_problems(&w));
        let dry = repair_layout(&w, true).unwrap();
        assert_eq!(dry.len(), 4);
        assert_eq!(layout_problems(&w).len(), 4, "dry run wrote");
        repair_layout(&w, false).unwrap();
        assert!(layout_problems(&w).is_empty(), "{:?}", layout_problems(&w));
        assert!(w.join("quarantine/seals/bad.json").is_file());
    }
}
