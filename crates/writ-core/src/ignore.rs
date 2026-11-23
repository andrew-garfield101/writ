//! Ignore rules: live `.gitignore` layered under `.writignore`.
//!
//! Matching uses full gitignore semantics via the `ignore` crate's
//! `gitignore` module: anchored paths (`/build`), directory-only patterns
//! (`.venv*/`), `**` globs (`**/node_modules/`, `build/**`), character classes
//! (`*.py[cod]`), and negation (`!keep.log`).
//!
//! Layering, lowest to highest precedence, as git orders them:
//! 1. The user's global excludes file (`core.excludesFile`, or git's
//!    default `$XDG_CONFIG_HOME/git/ignore` when unset), then the repo's
//!    `.git/info/exclude`.
//! 2. Repo root `.gitignore`, read fresh on every load.
//! 3. Nested `.gitignore` files: each applies only below its directory, and
//!    a deeper file overrides a shallower one. Read lazily, the first time
//!    a path below that directory is matched, so a walk never reads a
//!    `.gitignore` inside a directory it pruned.
//! 4. `.writignore` if it exists, otherwise writ's built-in defaults.
//!
//! The highest layer with a matching rule decides (within a file, the last
//! matching line). A path under an ignored directory stays ignored, as in
//! git. `.writ` is ALWAYS ignored, regardless of any file (negation cannot
//! un-ignore it).
//!
//! Finding 210, git's own rule: ignore rules apply only to untracked files.
//! A path in git's index is tracked by writ whatever a git derived rule
//! says (layers 1 to 3, and the block `writ init` imported from
//! `.gitignore` into `.writignore`), and a directory holding such a path is
//! walked; untracked files in it stay ignored. A `.writignore` line written
//! by hand (or the writ defaults) still hides a tracked path: that is the
//! user's choice, and doctor names the count. Git's index is read only when
//! a git derived rule ignores something, once per load.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;

/// Directory names that are ALWAYS ignored, regardless of ignore file contents.
const ALWAYS_IGNORED_DIRS: &[&str] = &[".writ"];

/// Default ignore rules used when no `.writignore` file exists.
const DEFAULT_IGNORE_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".pytest_cache",
];

/// Max rules accepted per ignore file (safety limit).
const MAX_RULES: usize = 1000;
/// Max length of a single pattern (safety limit).
const MAX_PATTERN_LEN: usize = 1024;

/// A pattern that could not be compiled, with its source and reason.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidPattern {
    /// Which file the pattern came from (`.gitignore`, `.writignore`, ...).
    pub source: String,
    /// The raw pattern text.
    pub pattern: String,
    /// Why it was rejected.
    pub reason: String,
}

/// The header `writ init` writes above the lines it copies from
/// `.gitignore` into `.writignore` ([`create_writignore`]).
const IMPORT_HEADER: &str = "# --- imported from .gitignore ---";

/// The source tag on `.writignore` globs from the imported block, so one
/// matcher keeps the file's line order and still says which kind of line
/// decided.
const IMPORTED_FROM: &str = ".writignore (imported from .gitignore)";

/// A compiled, layered set of ignore rules.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    /// Global excludes file, then `.git/info/exclude`.
    global: Gitignore,
    /// Repo root `.gitignore`.
    root: Gitignore,
    /// Nested `.gitignore` files, when loaded from a repo on disk.
    nested: Option<Nested>,
    /// `.writignore`, or the built-in defaults. Globs from the block
    /// `writ init` imported from `.gitignore` carry [`IMPORTED_FROM`]: git
    /// derived, so they never hide a path git tracks (finding 210).
    writ: Gitignore,
    /// Git's index, for repos loaded from disk (finding 210).
    git: Option<GitTracked>,
    invalid: Vec<InvalidPattern>,
}

/// Which kind of layer ignored a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `.writignore` outside its imported block, or writ's defaults.
    Writ,
    /// git's own rules: global excludes, `.git/info/exclude`, `.gitignore`
    /// files, and the block imported from `.gitignore`.
    Git,
}

/// Paths in git's index, read lazily, plus a memo of directories a git
/// derived rule ignores (a walk asks about each ancestor of every file).
#[derive(Debug, Clone)]
struct GitTracked {
    repo_root: PathBuf,
    paths: OnceLock<Arc<Vec<String>>>,
    git_ignored_dirs: Arc<Mutex<HashMap<String, bool>>>,
}

impl GitTracked {
    fn new(repo_root: &Path) -> Self {
        GitTracked {
            repo_root: repo_root.to_path_buf(),
            paths: OnceLock::new(),
            git_ignored_dirs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Sorted, deduplicated index paths; empty when there is no readable
    /// index (not a git repo, or a format this reader does not know), so
    /// every git derived rule applies as before.
    fn paths(&self) -> &[String] {
        self.paths
            .get_or_init(|| git_index::tracked_paths_cached(&self.repo_root))
    }

    fn contains(&self, rel_path: &str) -> bool {
        self.paths()
            .binary_search_by(|p| p.as_str().cmp(rel_path))
            .is_ok()
    }

    /// True when git tracks a path below the directory `dir`.
    fn has_under(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        let paths = self.paths();
        let i = paths.partition_point(|p| p.as_str() < prefix.as_str());
        paths.get(i).is_some_and(|p| p.starts_with(&prefix))
    }
}

/// Lazily loaded `<dir>/.gitignore` matchers, keyed by repo-relative dir.
#[derive(Debug, Clone)]
struct Nested {
    repo_root: PathBuf,
    cache: Arc<Mutex<HashMap<String, Option<Arc<Gitignore>>>>>,
}

impl Nested {
    fn new(repo_root: &Path) -> Self {
        Nested {
            repo_root: repo_root.to_path_buf(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The matcher for `<dir>/.gitignore`, `None` when there is none.
    fn get(&self, dir: &str) -> Option<Arc<Gitignore>> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = cache.get(dir) {
            return m.clone();
        }
        let path = self.repo_root.join(dir).join(".gitignore");
        let matcher = fs::read_to_string(&path).ok().map(|content| {
            let mut b = RulesBuilder::new();
            b.add_content(&format!("{dir}/.gitignore"), &content);
            Arc::new(b.build_matcher())
        });
        cache.insert(dir.to_string(), matcher.clone());
        matcher
    }
}

/// Incremental builder that applies safety limits and records bad patterns.
struct RulesBuilder {
    builder: GitignoreBuilder,
    invalid: Vec<InvalidPattern>,
}

impl RulesBuilder {
    fn new() -> Self {
        // Paths are always matched repo-relative, so the matcher root is empty.
        RulesBuilder {
            builder: GitignoreBuilder::new(""),
            invalid: Vec::new(),
        }
    }

    /// Add every rule from `content`. Later calls take precedence.
    fn add_content(&mut self, source: &str, content: &str) {
        self.add_lines(source, content, false);
    }

    /// Add every rule from `content`; with `tag_import`, the pattern lines
    /// right after [`IMPORT_HEADER`], up to the first blank or comment line
    /// (the importer writes them with neither), are tagged [`IMPORTED_FROM`].
    fn add_lines(&mut self, source: &str, content: &str, tag_import: bool) {
        let mut count = 0;
        let mut in_block = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if tag_import && trimmed == IMPORT_HEADER {
                in_block = true;
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                in_block = false;
                continue;
            }
            if trimmed.len() > MAX_PATTERN_LEN {
                self.reject(source, trimmed, "pattern exceeds 1024 characters");
                continue;
            }
            if count >= MAX_RULES {
                self.reject(source, trimmed, "more than 1000 rules in file");
                continue;
            }
            count += 1;
            let from = in_block.then(|| PathBuf::from(IMPORTED_FROM));
            if let Err(e) = self.builder.add_line(from, trimmed) {
                self.reject(source, trimmed, &e.to_string());
            }
        }
    }

    fn add_dir_names(&mut self, source: &str, names: &[&str]) {
        let content: String = names.iter().map(|n| format!("{n}/\n")).collect();
        self.add_content(source, &content);
    }

    fn reject(&mut self, source: &str, pattern: &str, reason: &str) {
        self.invalid.push(InvalidPattern {
            source: source.to_string(),
            pattern: pattern.to_string(),
            reason: reason.to_string(),
        });
    }

    /// Compile; a set that fails as a whole (unreachable in practice) is
    /// recorded and matches nothing.
    fn finish(mut self) -> (Gitignore, Vec<InvalidPattern>) {
        let matcher = match self.builder.build() {
            Ok(m) => m,
            Err(e) => {
                self.reject("<all>", "", &e.to_string());
                Gitignore::empty()
            }
        };
        (matcher, self.invalid)
    }

    /// Compile, dropping the record of rejected patterns (nested files).
    fn build_matcher(self) -> Gitignore {
        self.finish().0
    }
}

impl IgnoreRules {
    /// Load live rules for a repo: global excludes (git's
    /// `core.excludesFile` resolution) and `.git/info/exclude`, root
    /// `.gitignore`, nested `.gitignore` files, then `.writignore` (or
    /// built-in defaults when `.writignore` is absent).
    pub fn load(repo_root: &Path) -> Self {
        Self::load_with_excludes(repo_root, global_excludes_file().as_deref())
    }

    /// [`Self::load`] with an explicit global excludes file (`None`: none).
    pub fn load_with_excludes(repo_root: &Path, excludes_file: Option<&Path>) -> Self {
        let mut read_errors = Vec::new();
        let mut global = RulesBuilder::new();
        if let Some(path) = excludes_file {
            if let Some(content) = read_optional(path, &mut read_errors) {
                global.add_content(&path.display().to_string(), &content);
            }
        }
        let info_exclude = repo_root.join(".git").join("info").join("exclude");
        if let Some(content) = read_optional(&info_exclude, &mut read_errors) {
            global.add_content(".git/info/exclude", &content);
        }
        let gitignore = read_optional(&repo_root.join(".gitignore"), &mut read_errors);
        let writignore = read_optional(&repo_root.join(".writignore"), &mut read_errors);
        let mut rules = Self::layered(gitignore.as_deref(), writignore.as_deref());
        let (global, global_invalid) = global.finish();
        rules.global = global;
        rules.nested = Some(Nested::new(repo_root));
        rules.git = Some(GitTracked::new(repo_root));
        rules.invalid.splice(0..0, global_invalid);
        rules.invalid.extend(read_errors);
        rules
    }

    /// Build rules from raw `.gitignore` and `.writignore` contents (no
    /// global excludes, no nested files).
    ///
    /// `.writignore` rules are layered after `.gitignore`, so they win on
    /// conflict (e.g. `!docs/` in `.writignore` re-includes a gitignored dir).
    pub fn layered(gitignore: Option<&str>, writignore: Option<&str>) -> Self {
        let mut root = RulesBuilder::new();
        if let Some(content) = gitignore {
            root.add_content(".gitignore", content);
        }
        let mut writ = RulesBuilder::new();
        match writignore {
            Some(content) => writ.add_lines(".writignore", content, true),
            None => writ.add_dir_names("defaults", DEFAULT_IGNORE_DIRS),
        }
        let (root, mut invalid) = root.finish();
        let (writ, writ_invalid) = writ.finish();
        invalid.extend(writ_invalid);
        IgnoreRules {
            global: Gitignore::empty(),
            root,
            nested: None,
            writ,
            git: None,
            invalid,
        }
    }

    /// Hardcoded defaults (used when neither ignore file exists).
    pub fn defaults() -> Self {
        Self::layered(None, None)
    }

    /// Parse `.writignore` content alone (no `.gitignore`, no defaults).
    ///
    /// Enforces safety limits: max 1000 rules, max 1024 chars per pattern.
    pub fn parse(content: &str) -> Self {
        Self::layered(None, Some(content))
    }

    /// Patterns that failed to compile or exceeded safety limits (global,
    /// root and `.writignore` files; nested files are not listed).
    pub fn invalid_patterns(&self) -> &[InvalidPattern] {
        &self.invalid
    }

    /// The highest layer with a matching rule decides: ignore, whitelist,
    /// or no opinion, with the kind of layer that ignored it. Only
    /// `rel_path` itself is matched, not its parents.
    fn decide(&self, rel_path: &str, is_dir: bool) -> Verdict {
        let ignored_by = |d: Decision, source: Source| match d {
            Decision::Ignore => Verdict::Ignored(source),
            Decision::Whitelist => Verdict::Whitelisted,
            Decision::None => Verdict::NoRule,
        };
        match self.writ.matched(rel_path, is_dir) {
            Match::Ignore(glob) if glob.from() == Some(Path::new(IMPORTED_FROM)) => {
                return Verdict::Ignored(Source::Git)
            }
            Match::Ignore(_) => return Verdict::Ignored(Source::Writ),
            Match::Whitelist(_) => return Verdict::Whitelisted,
            Match::None => {}
        }
        if let Some(nested) = &self.nested {
            // Proper ancestors, deepest first: "a/b/c" -> "a/b", "a".
            let mut end = rel_path.len();
            while let Some(slash) = rel_path[..end].rfind('/') {
                let dir = &rel_path[..slash];
                if let Some(m) = nested.get(dir) {
                    let below = &rel_path[slash + 1..];
                    if let Some(d) = Decision::of(m.matched(below, is_dir)) {
                        return ignored_by(d, Source::Git);
                    }
                }
                end = slash;
            }
        }
        let d = Decision::of(self.root.matched(rel_path, is_dir))
            .or_else(|| Decision::of(self.global.matched(rel_path, is_dir)))
            .unwrap_or(Decision::None);
        ignored_by(d, Source::Git)
    }

    /// Does a git derived rule ignore the directory itself? Memoized when
    /// the repo's index is known, since walks ask once per file.
    fn dir_git_ignored(&self, dir: &str) -> bool {
        let Some(git) = &self.git else {
            return self.decide(dir, true) == Verdict::Ignored(Source::Git);
        };
        if let Some(&hit) = git
            .git_ignored_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(dir)
        {
            return hit;
        }
        let hit = self.decide(dir, true) == Verdict::Ignored(Source::Git);
        git.git_ignored_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(dir.to_string(), hit);
        hit
    }

    /// True when a proper ancestor of `rel_path` is ignored by a git
    /// derived rule. A walk only reaches such a path when the ancestor
    /// holds a tracked path (finding 210); git ignores everything else in it.
    fn git_ignored_ancestor(&self, rel_path: &str) -> bool {
        self.git.is_some()
            && rel_path
                .match_indices('/')
                .any(|(i, _)| self.dir_git_ignored(&rel_path[..i]))
    }

    /// Git's rule for a path a git derived rule ignores: ignored unless git
    /// tracks it (a directory: unless git tracks a path below it).
    fn git_ignores(&self, rel_path: &str, is_dir: bool) -> bool {
        match &self.git {
            None => true,
            Some(git) if is_dir => !git.has_under(rel_path),
            Some(git) => !git.contains(rel_path),
        }
    }

    /// Should the directory at this repo-relative path be pruned from walks?
    ///
    /// The path itself is checked, plus (only when a repo's git index is
    /// known) whether a git derived rule ignores an ancestor: walkers prune
    /// parents before reaching children, but a git ignored directory that
    /// holds a tracked path is walked (finding 210). Use
    /// [`IgnoreRules::is_path_ignored`] for arbitrary paths.
    pub fn is_dir_ignored(&self, rel_path: &str) -> bool {
        if is_always_ignored(rel_path) {
            return true;
        }
        match self.decide(rel_path, true) {
            Verdict::Ignored(Source::Writ) => true,
            Verdict::Ignored(Source::Git) => self.git_ignores(rel_path, true),
            _ => self.git_ignored_ancestor(rel_path) && self.git_ignores(rel_path, true),
        }
    }

    /// Should the file at this repo-relative path be ignored?
    ///
    /// Checked as [`IgnoreRules::is_dir_ignored`] checks a directory.
    pub fn is_file_ignored(&self, rel_path: &str) -> bool {
        if is_always_ignored(rel_path) {
            return true;
        }
        match self.decide(rel_path, false) {
            Verdict::Ignored(Source::Writ) => true,
            Verdict::Ignored(Source::Git) => self.git_ignores(rel_path, false),
            _ => self.git_ignored_ancestor(rel_path) && self.git_ignores(rel_path, false),
        }
    }

    /// Is this repo-relative file path ignored, either directly or because
    /// any parent directory is ignored? Used for paths that did not come
    /// from a pruned walk (index entries, seal trees). A path git tracks is
    /// ignored only by a writ rule (finding 210).
    pub fn is_path_ignored(&self, rel_path: &str) -> bool {
        let rel_path = rel_path.trim_start_matches('/');
        if rel_path.is_empty() {
            return false;
        }
        if is_always_ignored(rel_path) {
            return true;
        }
        let mut git_hit = false;
        // Parents shallowest first, as a walk would prune them.
        for (i, _) in rel_path.match_indices('/') {
            match self.decide(&rel_path[..i], true) {
                Verdict::Ignored(Source::Writ) => return true,
                Verdict::Ignored(Source::Git) => git_hit = true,
                _ => {}
            }
        }
        match self.decide(rel_path, false) {
            Verdict::Ignored(Source::Writ) => return true,
            Verdict::Ignored(Source::Git) => git_hit = true,
            _ => {}
        }
        git_hit && self.git_ignores(rel_path, false)
    }

    /// Paths git tracks that a writ rule (a hand line in `.writignore`, or
    /// the writ defaults) hides from writ: the one case where writ and git
    /// still disagree after finding 210, by the user's choice. `.writ` is
    /// not counted (always ignored, never a choice). Reads git's index;
    /// empty when there is none. One `.writignore` match per index path and
    /// per directory (memoized), since only that layer can hide a tracked
    /// path.
    pub fn tracked_but_writ_ignored(&self) -> Vec<String> {
        let Some(git) = &self.git else {
            return Vec::new();
        };
        let hand_ignores = |rel: &str, is_dir: bool| match self.writ.matched(rel, is_dir) {
            Match::Ignore(glob) => glob.from() != Some(Path::new(IMPORTED_FROM)),
            _ => false,
        };
        let mut dirs: HashMap<&str, bool> = HashMap::new();
        let mut hidden = Vec::new();
        for path in git.paths().iter() {
            if path.ends_with('/') || is_always_ignored(path) {
                continue;
            }
            let mut by_dir = false;
            for (i, _) in path.match_indices('/') {
                let dir = &path[..i];
                let hit = *dirs.entry(dir).or_insert_with(|| hand_ignores(dir, true));
                if hit {
                    by_dir = true;
                    break;
                }
            }
            if by_dir || hand_ignores(path, false) {
                hidden.push(path.clone());
            }
        }
        hidden
    }
}

/// A path's verdict from the highest layer with a matching rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Ignored(Source),
    Whitelisted,
    NoRule,
}

/// One layer's verdict on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Ignore,
    Whitelist,
    None,
}

impl Decision {
    fn of<T>(m: Match<T>) -> Option<Self> {
        match m {
            Match::None => None,
            Match::Ignore(_) => Some(Decision::Ignore),
            Match::Whitelist(_) => Some(Decision::Whitelist),
        }
    }
}

/// Environment override for the global excludes file: a path, or empty for
/// none. Without it, git's resolution applies (`core.excludesFile` in the
/// global git config, else `$XDG_CONFIG_HOME/git/ignore`).
pub const EXCLUDES_FILE_ENV: &str = "WRIT_EXCLUDES_FILE";

/// The global excludes file to honor, if any. Unit tests never read the
/// developer's git config, so results do not depend on the machine.
fn global_excludes_file() -> Option<std::path::PathBuf> {
    if let Some(v) = std::env::var_os(EXCLUDES_FILE_ENV) {
        return (!v.is_empty()).then(|| v.into());
    }
    if cfg!(test) {
        return None;
    }
    ignore::gitignore::gitconfig_excludes_path()
}

/// True when any component of the path is an always-ignored directory.
fn is_always_ignored(rel_path: &str) -> bool {
    rel_path
        .split('/')
        .any(|component| ALWAYS_IGNORED_DIRS.contains(&component))
}

/// Read a file that may legitimately be absent. A missing file is `None`;
/// any other I/O error is recorded so callers can surface it, and the file
/// is then treated as absent so read-only commands keep working.
fn read_optional(path: &Path, errors: &mut Vec<InvalidPattern>) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            errors.push(InvalidPattern {
                source: path.display().to_string(),
                pattern: String::new(),
                reason: format!("unreadable: {e}"),
            });
            None
        }
    }
}

/// All default directory names written to a generated `.writignore`.
const WRITIGNORE_DEFAULTS: &[&str] = &[
    ".writ",
    ".git",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".pytest_cache",
];

/// Create a `.writignore` file in the repo root if one doesn't exist.
///
/// - If `.writignore` already exists → no-op, returns `Ok(false)`.
/// - If `.gitignore` exists → merges defaults with gitignore patterns (deduped).
/// - Otherwise → creates with defaults only.
///
/// Returns `Ok(true)` if the file was created.
pub fn create_writignore(repo_root: &Path) -> crate::WritResult<bool> {
    let writignore_path = repo_root.join(".writignore");
    if writignore_path.exists() {
        return Ok(false);
    }

    let mut content = String::new();
    content.push_str("# .writignore — controls which files writ tracks.\n");
    content.push_str("# Generated by `writ init`. Edit freely.\n");
    content.push_str("#\n");
    content.push_str("# Syntax: same as .gitignore. Rules here override .gitignore.\n");
    content.push_str("# The live .gitignore is always honored; this import is a snapshot.\n");
    content.push_str("# .writ is always ignored regardless of this file.\n\n");
    content.push_str("# --- writ defaults ---\n");

    for dir in WRITIGNORE_DEFAULTS {
        content.push_str(dir);
        content.push('\n');
    }

    let gitignore_path = repo_root.join(".gitignore");
    if gitignore_path.exists() {
        if let Ok(gitignore_content) = fs::read_to_string(&gitignore_path) {
            content.push_str("\n# --- imported from .gitignore ---\n");
            for line in gitignore_content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let normalized = trimmed.trim_end_matches('/');
                if WRITIGNORE_DEFAULTS.contains(&normalized) {
                    continue;
                }
                content.push_str(trimmed);
                content.push('\n');
            }
        }
    }

    crate::fsutil::atomic_write(&writignore_path, content.as_bytes())?;
    Ok(true)
}

/// Simple glob matching: `*` matches any characters, `?` matches one character.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let mut pi = 0;
    let mut ti = 0;
    let mut star_p = None;
    let mut star_t = None;

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_p = Some(pi);
            star_t = Some(ti);
            pi += 1;
        } else if let Some(sp) = star_p {
            pi = sp + 1;
            let st = star_t.unwrap() + 1;
            star_t = Some(st);
            ti = st;
        } else {
            return false;
        }
    }

    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }

    pi == p.len()
}

/// A reader of the path list in git's index (`.git/index`), versions 2
/// to 4, for finding 210. Only paths are read; no checksum is verified,
/// since a torn or unknown index only means writ falls back to applying
/// every git derived rule, as it did before.
mod git_index {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    /// The index file of the repo whose work tree is `repo_root`: `.git`
    /// as a directory, or a `.git` file naming the git directory (linked
    /// worktrees, submodules).
    fn index_file(repo_root: &Path) -> Option<PathBuf> {
        let dot_git = repo_root.join(".git");
        let meta = fs::metadata(&dot_git).ok()?;
        if meta.is_dir() {
            return Some(dot_git.join("index"));
        }
        let text = fs::read_to_string(&dot_git).ok()?;
        let dir = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
        let dir = Path::new(dir);
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            repo_root.join(dir)
        };
        Some(dir.join("index"))
    }

    /// The last index read in this process, keyed by its file, size, mtime
    /// and inode (git writes the index to a lock file and renames it, so a
    /// rewrite is a new inode): one seal loads the ignore rules several
    /// times, and long running processes (watch, MCP, Python) see git's
    /// later writes.
    type Key = (PathBuf, u64, Option<SystemTime>, u64);
    type Cached = (Key, Arc<Vec<String>>);
    static LAST: Mutex<Option<Cached>> = Mutex::new(None);

    /// [`tracked_paths`], reusing the last read while the file is unchanged.
    /// Empty when there is no readable index.
    pub(super) fn tracked_paths_cached(repo_root: &Path) -> Arc<Vec<String>> {
        let Some(file) = index_file(repo_root) else {
            return Arc::default();
        };
        let Ok(meta) = fs::metadata(&file) else {
            return Arc::default();
        };
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let inode = 0;
        let key = (file, meta.len(), meta.modified().ok(), inode);
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, paths)) = last.as_ref() {
            if *k == key && key.2.is_some() {
                return Arc::clone(paths);
            }
        }
        let paths = Arc::new(read_paths(&key.0).unwrap_or_default());
        *last = Some((key, Arc::clone(&paths)));
        paths
    }

    /// Sorted, deduplicated paths in the index (every stage, so a path with
    /// a merge conflict counts as tracked). `None` when there is no index or
    /// it cannot be read.
    #[cfg(test)]
    pub(super) fn tracked_paths(repo_root: &Path) -> Option<Vec<String>> {
        read_paths(&index_file(repo_root)?)
    }

    fn read_paths(file: &Path) -> Option<Vec<String>> {
        let data = fs::read(file).ok()?;
        let mut paths = parse(&data)?;
        paths.sort_unstable();
        paths.dedup();
        Some(paths)
    }

    fn be32(data: &[u8], at: usize) -> Option<u32> {
        Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
    }

    /// Git's offset varint (version 4 path prefix lengths).
    fn varint(data: &[u8], pos: &mut usize) -> Option<usize> {
        let mut c = *data.get(*pos)?;
        *pos += 1;
        let mut val = (c & 0x7f) as usize;
        while c & 0x80 != 0 {
            c = *data.get(*pos)?;
            *pos += 1;
            val = val.checked_add(1)?.checked_mul(128)? + (c & 0x7f) as usize;
        }
        Some(val)
    }

    /// Entry paths of an index file's bytes.
    pub(super) fn parse(data: &[u8]) -> Option<Vec<String>> {
        if data.get(0..4)? != b"DIRC" {
            return None;
        }
        let version = be32(data, 4)?;
        if !(2..=4).contains(&version) {
            return None;
        }
        let count = be32(data, 8)? as usize;
        // ctime, mtime (8 each), dev, ino, mode, uid, gid, size (4 each),
        // object id (20), flags (2).
        const FIXED: usize = 62;
        let mut paths = Vec::with_capacity(count.min(data.len() / FIXED));
        let mut pos = 12;
        let mut prev: Vec<u8> = Vec::new();
        for _ in 0..count {
            let flags = u16::from_be_bytes(data.get(pos + 60..pos + 62)?.try_into().ok()?);
            let extended = version >= 3 && flags & 0x4000 != 0;
            let mut at = pos + FIXED + if extended { 2 } else { 0 };
            let path: Vec<u8> = if version == 4 {
                let strip = varint(data, &mut at)?;
                let keep = prev.len().checked_sub(strip)?;
                let rest = data.get(at..)?;
                let nul = rest.iter().position(|&b| b == 0)?;
                let mut path = prev[..keep].to_vec();
                path.extend_from_slice(&rest[..nul]);
                pos = at + nul + 1;
                path
            } else {
                let rest = data.get(at..)?;
                let nul = rest.iter().position(|&b| b == 0)?;
                let path = rest[..nul].to_vec();
                // Entries are NUL padded to a multiple of 8 bytes.
                let len = at - pos + nul;
                pos += (len + 8) & !7;
                path
            };
            if let Ok(p) = std::str::from_utf8(&path) {
                paths.push(p.to_string());
            }
            prev = path;
        }
        Some(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_include_always_ignored() {
        let rules = IgnoreRules::defaults();
        assert!(rules.is_dir_ignored(".writ"));
        assert!(rules.is_dir_ignored(".git"));
        assert!(rules.is_dir_ignored("target"));
        assert!(rules.is_dir_ignored("node_modules"));
    }

    #[test]
    fn test_parse_blank_and_comments() {
        let rules = IgnoreRules::parse("# comment\n\n  \n");
        assert!(rules.is_dir_ignored(".writ"));
        // Defaults are NOT included when parsing custom file
        assert!(!rules.is_dir_ignored("target"));
    }

    #[test]
    fn test_parse_dir_names() {
        let rules = IgnoreRules::parse("build\ndist/\n");
        assert!(rules.is_dir_ignored("build"));
        assert!(rules.is_dir_ignored("dist"));
    }

    #[test]
    fn test_parse_glob_patterns() {
        let rules = IgnoreRules::parse("*.pyc\n*.o\n");
        assert!(rules.is_file_ignored("module.pyc"));
        assert!(rules.is_file_ignored("src/main.o"));
        assert!(!rules.is_file_ignored("main.rs"));
    }

    #[test]
    fn test_always_ignored_with_custom() {
        let rules = IgnoreRules::parse("custom_dir\n");
        assert!(rules.is_dir_ignored(".writ"));
        assert!(rules.is_dir_ignored("custom_dir"));
    }

    // ── gitignore semantics (A.1) ───────────────────────────

    #[test]
    fn test_root_dir_glob_matches_venv_variants() {
        let rules = IgnoreRules::parse(".venv*/\n");
        assert!(rules.is_dir_ignored(".venv311"));
        assert!(rules.is_dir_ignored(".venv"));
        assert!(rules.is_path_ignored(".venv311/lib/python3.11/site.py"));
        assert!(!rules.is_path_ignored("venv_notes.md"));
        // Directory-only pattern does not match a file of the same shape.
        assert!(!rules.is_file_ignored(".venv-file"));
    }

    #[test]
    fn test_nested_double_star_dir_glob() {
        let rules = IgnoreRules::parse("**/node_modules/\n");
        assert!(rules.is_dir_ignored("node_modules"));
        assert!(rules.is_dir_ignored("web/app/node_modules"));
        assert!(rules.is_path_ignored("web/app/node_modules/react/index.js"));
        assert!(!rules.is_path_ignored("web/app/src/index.js"));
    }

    #[test]
    fn test_trailing_double_star_glob() {
        let rules = IgnoreRules::parse("build/**\n");
        assert!(rules.is_path_ignored("build/out/app.bin"));
        assert!(rules.is_file_ignored("build/app.bin"));
        assert!(!rules.is_path_ignored("src/build.rs"));
    }

    #[test]
    fn test_anchored_pattern_only_matches_at_root() {
        let rules = IgnoreRules::parse("/docs/\n");
        assert!(rules.is_dir_ignored("docs"));
        assert!(!rules.is_dir_ignored("book/docs"));
    }

    #[test]
    fn test_unanchored_dir_matches_at_any_depth() {
        let rules = IgnoreRules::parse("testing/\n");
        assert!(rules.is_dir_ignored("testing"));
        assert!(rules.is_dir_ignored("crates/writ-py/testing"));
    }

    #[test]
    fn test_middle_slash_path_is_anchored() {
        let rules = IgnoreRules::parse("crates/writ-py/writ.data/scripts/writ\n");
        assert!(rules.is_file_ignored("crates/writ-py/writ.data/scripts/writ"));
        assert!(!rules.is_file_ignored("writ"));
    }

    #[test]
    fn test_character_class_glob() {
        let rules = IgnoreRules::parse("*.py[cod]\n");
        assert!(rules.is_file_ignored("pkg/mod.pyc"));
        assert!(rules.is_file_ignored("mod.pyo"));
        assert!(!rules.is_file_ignored("mod.py"));
    }

    #[test]
    fn test_negation_reincludes_file() {
        let rules = IgnoreRules::parse("*.log\n!keep.log\n");
        assert!(rules.is_file_ignored("debug.log"));
        assert!(!rules.is_file_ignored("keep.log"));
    }

    #[test]
    fn test_gitignore_applies_without_writignore_rule() {
        let rules = IgnoreRules::layered(Some("scripts/\n"), Some("custom\n"));
        assert!(rules.is_dir_ignored("scripts"));
        assert!(rules.is_dir_ignored("custom"));
    }

    #[test]
    fn test_writignore_overrides_gitignore() {
        // .writignore is layered last, so its negation wins.
        let rules = IgnoreRules::layered(Some("docs/\n"), Some("!docs/\n"));
        assert!(!rules.is_dir_ignored("docs"));
        // ...and its additions apply on top of .gitignore.
        let rules = IgnoreRules::layered(Some("!notes.md\n"), Some("*.md\n"));
        assert!(rules.is_file_ignored("notes.md"));
    }

    #[test]
    fn test_defaults_still_apply_with_gitignore_but_no_writignore() {
        let rules = IgnoreRules::layered(Some("dist/\n"), None);
        assert!(rules.is_dir_ignored("dist"));
        assert!(rules.is_dir_ignored("target"));
        assert!(rules.is_dir_ignored(".git"));
    }

    #[test]
    fn test_writ_always_ignored_cannot_be_negated() {
        let rules = IgnoreRules::layered(Some("!.writ/\n"), Some("!.writ\n!.writ/**\n"));
        assert!(rules.is_dir_ignored(".writ"));
        assert!(rules.is_path_ignored(".writ/seals/abc.json"));
        assert!(rules.is_path_ignored("sub/.writ/index.json"));
    }

    #[test]
    fn test_is_path_ignored_checks_parents() {
        let rules = IgnoreRules::parse("target\n");
        assert!(rules.is_path_ignored("target/release/writ"));
        assert!(rules.is_path_ignored("crates/x/target/debug/build.log"));
        assert!(!rules.is_path_ignored("crates/x/src/target.rs"));
        assert!(!rules.is_path_ignored(""));
    }

    #[test]
    fn test_safety_limits_record_rejections() {
        let long = "a".repeat(MAX_PATTERN_LEN + 1);
        let mut content = format!("{long}\n");
        for i in 0..=MAX_RULES {
            content.push_str(&format!("rule{i}\n"));
        }
        let rules = IgnoreRules::parse(&content);
        let invalid = rules.invalid_patterns();
        assert_eq!(invalid.len(), 2);
        assert!(invalid[0].reason.contains("1024"));
        assert_eq!(invalid[1].pattern, format!("rule{MAX_RULES}"));
        assert!(rules.is_dir_ignored("rule0"));
        assert!(!rules.is_dir_ignored(&format!("rule{MAX_RULES}")));
    }

    #[test]
    fn test_invalid_glob_is_recorded_not_fatal() {
        let rules = IgnoreRules::parse("a{b\n*.log\n");
        assert_eq!(rules.invalid_patterns().len(), 1);
        assert_eq!(rules.invalid_patterns()[0].source, ".writignore");
        assert!(rules.is_file_ignored("x.log"));
    }

    #[test]
    fn test_load_reads_live_gitignore_every_time() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".writignore"), "target\n").unwrap();
        assert!(!IgnoreRules::load(dir.path()).is_dir_ignored(".venv311"));
        // A rule added to .gitignore after init is honored on the next load.
        fs::write(dir.path().join(".gitignore"), ".venv*/\n").unwrap();
        let rules = IgnoreRules::load(dir.path());
        assert!(rules.is_dir_ignored(".venv311"));
        assert!(rules.is_dir_ignored("target"));
        assert!(rules.invalid_patterns().is_empty());
    }

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("*.pyc", "foo.pyc"));
        assert!(!glob_match("*.pyc", "foo.py"));
        assert!(glob_match("test_*", "test_main"));
    }

    #[test]
    fn test_glob_match_question() {
        assert!(glob_match("?.txt", "a.txt"));
        assert!(!glob_match("?.txt", "ab.txt"));
    }

    #[test]
    fn test_glob_match_exact() {
        assert!(glob_match("Makefile", "Makefile"));
        assert!(!glob_match("Makefile", "makefile"));
    }

    #[test]
    fn test_load_fallback_to_defaults() {
        let rules = IgnoreRules::load(Path::new("/tmp/nonexistent_writ_repo_xyz"));
        assert!(rules.is_dir_ignored("target"));
        assert!(rules.is_dir_ignored("node_modules"));
    }

    #[test]
    fn test_create_writignore_no_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        let created = create_writignore(dir.path()).unwrap();
        assert!(created);
        assert!(dir.path().join(".writignore").exists());
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert!(content.contains(".writ"));
        assert!(content.contains("target"));
        assert!(content.contains("node_modules"));
        assert!(!content.contains("imported from .gitignore"));
    }

    #[test]
    fn test_create_writignore_with_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "build\n*.log\n").unwrap();
        let created = create_writignore(dir.path()).unwrap();
        assert!(created);
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert!(content.contains("build"));
        assert!(content.contains("*.log"));
        assert!(content.contains("imported from .gitignore"));
    }

    #[test]
    fn test_create_writignore_skips_existing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".writignore"), "custom_only\n").unwrap();
        let created = create_writignore(dir.path()).unwrap();
        assert!(!created);
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert_eq!(content, "custom_only\n");
    }

    #[test]
    fn test_create_writignore_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".gitignore"),
            "target\nnode_modules/\nbuild\n",
        )
        .unwrap();
        let created = create_writignore(dir.path()).unwrap();
        assert!(created);
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        // "target" and "node_modules" are in defaults, should not appear in gitignore section
        let gitignore_section = content.split("imported from .gitignore").nth(1).unwrap();
        assert!(!gitignore_section.contains("target"));
        assert!(!gitignore_section.contains("node_modules"));
        assert!(gitignore_section.contains("build"));
    }

    #[test]
    fn test_create_writignore_has_header() {
        let dir = tempfile::tempdir().unwrap();
        create_writignore(dir.path()).unwrap();
        let content = fs::read_to_string(dir.path().join(".writignore")).unwrap();
        assert!(content.starts_with("# .writignore"));
    }

    // ── Repo-level contract tests (B.2, A.1 group) ──────────
    //
    // The matcher tests above prove pattern semantics. These prove the
    // rules reach the places the leaks showed up: state, context, seal.
    // ── ignore-fidelity: nested files, global excludes ─────────────

    fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for (rel, body) in files {
            let p = t.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
        t
    }

    #[test]
    fn test_nested_rules_apply_only_below_their_directory() {
        let t = tree(&[("a/.gitignore", "*.log\nout/\n/top.txt\n")]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(r.is_file_ignored("a/x.log"));
        assert!(r.is_file_ignored("a/deep/y.log"));
        assert!(r.is_dir_ignored("a/out"));
        assert!(r.is_path_ignored("a/out/z.rs"));
        // Anchored to a/, not to the repo root.
        assert!(r.is_file_ignored("a/top.txt"));
        assert!(!r.is_file_ignored("a/deep/top.txt"));
        // Siblings and the root are untouched.
        assert!(!r.is_file_ignored("x.log"));
        assert!(!r.is_file_ignored("b/x.log"));
        assert!(!r.is_dir_ignored("out"));
        assert!(!r.is_file_ignored("top.txt"));
    }

    #[test]
    fn test_negation_in_nested_file_overrides_root() {
        let t = tree(&[
            (".gitignore", "*.log\n"),
            ("keep/.gitignore", "!important.log\n"),
            ("keep/deeper/.gitignore", "important.log\n"),
        ]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(r.is_file_ignored("other.log"));
        assert!(r.is_file_ignored("keep/other.log"));
        assert!(!r.is_file_ignored("keep/important.log"));
        assert!(!r.is_path_ignored("keep/sub/important.log"));
        // The deeper file wins over the shallower one.
        assert!(r.is_file_ignored("keep/deeper/important.log"));
    }

    #[test]
    fn test_nested_cannot_reinclude_under_ignored_parent() {
        let t = tree(&[(".gitignore", "vendor/\n"), ("vendor/.gitignore", "!*\n")]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(r.is_path_ignored("vendor/lib.rs"));
    }

    #[test]
    fn test_writignore_stays_on_top_and_writ_always_ignored() {
        let t = tree(&[
            ("docs/.gitignore", "*.md\n"),
            (".writignore", "!docs/keep.md\n"),
            ("sub/.gitignore", "!.writ\n!.writ/\n"),
        ]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(r.is_file_ignored("docs/other.md"));
        assert!(!r.is_file_ignored("docs/keep.md"));
        assert!(r.is_dir_ignored("sub/.writ"));
        assert!(r.is_dir_ignored(".writ"));
    }

    #[test]
    fn test_global_excludes_honored_and_lowest_precedence() {
        let t = tree(&[(".gitignore", "!keep.so\n")]);
        let global = tempfile::NamedTempFile::new().unwrap();
        fs::write(global.path(), "*.so\n.DS_Store\n").unwrap();
        let r = IgnoreRules::load_with_excludes(t.path(), Some(global.path()));
        assert!(r.is_file_ignored("python/writ/_native.abi3.so"));
        assert!(r.is_file_ignored("a/.DS_Store"));
        // Repo files override the global file.
        assert!(!r.is_file_ignored("keep.so"));
    }

    #[test]
    fn test_global_excludes_disabled_when_unset() {
        let t = tree(&[]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(!r.is_file_ignored("python/writ/_native.abi3.so"));
        // A configured path that does not exist is the same as none.
        let r = IgnoreRules::load_with_excludes(t.path(), Some(&t.path().join("missing")));
        assert!(!r.is_file_ignored("x.so"));
        assert!(r.invalid_patterns().is_empty());
    }

    #[test]
    fn test_git_info_exclude_honored() {
        let t = tree(&[(".git/info/exclude", "secret.txt\n")]);
        let r = IgnoreRules::load_with_excludes(t.path(), None);
        assert!(r.is_file_ignored("secret.txt"));
    }

    #[test]
    fn test_excludes_env_override() {
        // Unit tests never read git config; the env var is the only way in.
        // Resolved through global_excludes_file, read here without setting
        // the variable process-wide (other tests run in parallel).
        assert!(std::env::var_os(EXCLUDES_FILE_ENV).is_some() || global_excludes_file().is_none());
    }

    mod repo_level {
        use std::fs;
        use std::path::Path;

        use serde_json::Value;
        use tempfile::{tempdir, TempDir};

        use crate::context::{ContextFilter, ContextScope};
        use crate::seal::{AgentIdentity, AgentType, Seal, TaskStatus, Verification};
        use crate::state::FileStatus;
        use crate::Repository;

        fn agent() -> AgentIdentity {
            AgentIdentity {
                id: "bri-test".to_string(),
                agent_type: AgentType::Agent,
            }
        }

        fn write(root: &Path, rel: &str, body: &str) {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }

        fn seal_all(repo: &Repository, summary: &str) -> Seal {
            repo.seal(
                agent(),
                summary.to_string(),
                None,
                TaskStatus::InProgress,
                Verification::default(),
                false,
            )
            .unwrap()
        }

        /// Repo with `.gitignore` written BEFORE init, one source file per dir.
        fn repo_with(gitignore: &str, files: &[&str]) -> (TempDir, Repository) {
            let dir = tempdir().unwrap();
            fs::write(dir.path().join(".gitignore"), gitignore).unwrap();
            for rel in files {
                write(dir.path(), rel, &format!("// {rel}\n"));
            }
            let repo = Repository::init(dir.path()).unwrap();
            (dir, repo)
        }

        /// Every string anywhere in the serialized full context.
        fn context_strings(repo: &Repository) -> Vec<String> {
            let ctx = repo
                .context(ContextScope::Full, 10, &ContextFilter::default())
                .unwrap();
            let mut out = Vec::new();
            collect_strings(&serde_json::to_value(&ctx).unwrap(), &mut out);
            out
        }

        /// Strings under the live working-tree views only (no seal history).
        fn live_view_strings(repo: &Repository) -> Vec<String> {
            let ctx = repo
                .context(ContextScope::Full, 10, &ContextFilter::default())
                .unwrap();
            let value = serde_json::to_value(&ctx).unwrap();
            let mut out = Vec::new();
            for key in [
                "working_state",
                "pending_changes",
                "file_scope",
                "seal_nudge",
            ] {
                if let Some(v) = value.get(key) {
                    collect_strings(v, &mut out);
                }
            }
            out
        }

        /// Top-level context keys whose subtree contains `needle`.
        fn keys_containing(repo: &Repository, needle: &str) -> Vec<String> {
            let ctx = repo
                .context(ContextScope::Full, 10, &ContextFilter::default())
                .unwrap();
            let value = serde_json::to_value(&ctx).unwrap();
            let obj = value.as_object().unwrap();
            obj.iter()
                .filter(|(_, v)| {
                    let mut out = Vec::new();
                    collect_strings(v, &mut out);
                    out.iter().any(|s| s.starts_with(needle))
                })
                .map(|(k, _)| k.clone())
                .collect()
        }

        fn collect_strings(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::String(s) => out.push(s.clone()),
                Value::Array(items) => items.iter().for_each(|i| collect_strings(i, out)),
                Value::Object(map) => map.values().for_each(|i| collect_strings(i, out)),
                _ => {}
            }
        }

        fn leaked<'a>(strings: &'a [String], prefixes: &[&str]) -> Vec<&'a String> {
            strings
                .iter()
                .filter(|s| prefixes.iter().any(|p| s.starts_with(p)))
                .collect()
        }

        fn state_paths(repo: &Repository) -> Vec<(String, FileStatus)> {
            repo.state()
                .unwrap()
                .changes
                .into_iter()
                .map(|c| (c.path, c.status))
                .collect()
        }

        fn touch_all(root: &Path, files: &[&str]) {
            for rel in files {
                write(root, rel, &format!("// {rel} edited\n"));
            }
        }

        const VENV_FILES: &[&str] = &[
            "src/main.rs",
            ".venv/lib/a.py",
            ".venv311/lib/python3.11/site.py",
            ".venv-tools/bin/tool",
        ];

        #[test]
        fn test_context_excludes_root_dir_glob_venv_variants() {
            let (dir, repo) = repo_with(".venv*/\n", VENV_FILES);
            seal_all(&repo, "baseline");
            touch_all(dir.path(), VENV_FILES);

            let strings = context_strings(&repo);
            let bad = leaked(&strings, &[".venv/", ".venv311/", ".venv-tools/"]);
            assert!(bad.is_empty(), "ignored paths leaked into context: {bad:?}");
            assert!(strings.iter().any(|s| s == "src/main.rs"));
        }

        #[test]
        fn test_context_excludes_nested_double_star_node_modules() {
            let files = [
                "web/src/app.ts",
                "node_modules/react/index.js",
                "web/app/node_modules/lodash/index.js",
            ];
            let (dir, repo) = repo_with("**/node_modules/\n", &files);
            seal_all(&repo, "baseline");
            touch_all(dir.path(), &files);

            let strings = context_strings(&repo);
            let bad: Vec<_> = strings
                .iter()
                .filter(|s| s.contains("node_modules/"))
                .collect();
            assert!(bad.is_empty(), "node_modules leaked: {bad:?}");
            assert!(strings.iter().any(|s| s == "web/src/app.ts"));
        }

        #[test]
        fn test_context_excludes_trailing_double_star_build() {
            let files = ["src/build.rs", "build/out/app.bin", "build/app.map"];
            let (dir, repo) = repo_with("build/**\n", &files);
            seal_all(&repo, "baseline");
            touch_all(dir.path(), &files);

            let strings = context_strings(&repo);
            assert!(leaked(&strings, &["build/"]).is_empty());
            assert!(strings.iter().any(|s| s == "src/build.rs"));
        }

        #[test]
        fn test_gitignore_rule_added_after_init_applies_without_reinit() {
            let (dir, repo) = repo_with("", &["src/lib.rs"]);
            seal_all(&repo, "baseline");

            // Rule lands after init; artifacts appear after the rule.
            fs::write(dir.path().join(".gitignore"), "dist/\n").unwrap();
            write(dir.path(), "dist/bundle.js", "bundle\n");
            write(dir.path(), "dist/nested/chunk.js", "chunk\n");

            let paths = state_paths(&repo);
            assert!(
                paths.iter().all(|(p, _)| !p.starts_with("dist/")),
                "dist/ visible after live .gitignore rule: {paths:?}"
            );
            assert!(leaked(&context_strings(&repo), &["dist/"]).is_empty());
        }

        #[test]
        fn test_writignore_rule_wins_over_gitignore() {
            let files = ["gen/keep.rs", "gen/other.rs", "docs/guide.md"];
            let (_dir, repo) = repo_with("gen/*.rs\ndocs/\n", &files);
            fs::write(
                repo_root(&repo).join(".writignore"),
                "!gen/keep.rs\n!docs/\n",
            )
            .unwrap();

            let paths: Vec<String> = state_paths(&repo).into_iter().map(|(p, _)| p).collect();
            assert!(paths.contains(&"gen/keep.rs".to_string()), "{paths:?}");
            assert!(paths.contains(&"docs/guide.md".to_string()), "{paths:?}");
            assert!(!paths.contains(&"gen/other.rs".to_string()), "{paths:?}");
        }

        #[test]
        fn test_writ_dir_always_ignored_even_if_unignored() {
            let (dir, repo) = repo_with("!.writ/\n", &["src/a.rs"]);
            fs::write(dir.path().join(".writignore"), "!.writ\n!.writ/**\n").unwrap();

            let paths = state_paths(&repo);
            assert!(
                paths.iter().all(|(p, _)| !p.starts_with(".writ/")),
                "{paths:?}"
            );
            let seal = seal_all(&repo, "baseline");
            assert!(seal.changes.iter().all(|c| !c.path.starts_with(".writ/")));
            assert!(leaked(&context_strings(&repo), &[".writ/"]).is_empty());
        }

        #[test]
        fn test_seal_does_not_capture_gitignored_files() {
            let (_dir, repo) = repo_with(".venv*/\nbuild/\n", VENV_FILES);
            write(repo_root(&repo), "build/out.o", "obj\n");

            let seal = seal_all(&repo, "baseline");
            let sealed: Vec<&str> = seal.changes.iter().map(|c| c.path.as_str()).collect();
            assert!(sealed.contains(&"src/main.rs"));
            assert!(sealed.contains(&".gitignore"));
            assert_eq!(sealed.len(), 2, "sealed ignored paths: {sealed:?}");
        }

        #[test]
        fn test_previously_tracked_file_now_ignored_is_hidden_not_deleted() {
            let files = ["src/a.rs", "cache/blob.bin", "cache/deep/x.bin"];
            let (dir, repo) = repo_with("", &files);
            seal_all(&repo, "baseline");
            let tracked_before = repo.state().unwrap().tracked_count;

            fs::write(dir.path().join(".gitignore"), "cache/\n").unwrap();
            let paths = state_paths(&repo);
            assert!(
                paths.iter().all(|(p, _)| !p.starts_with("cache/")),
                "cache/ reported as {paths:?}"
            );
            assert_eq!(repo.state().unwrap().tracked_count, tracked_before - 2);
            // Seal history (recent_seals, ownership) legitimately names the
            // baseline's cache/ files; only the live views must hide them.
            let ctx = live_view_strings(&repo);
            let bad = leaked(&ctx, &["cache/"]);
            assert!(bad.is_empty(), "cache/ leaked into live context: {bad:?}");
            // History-derived sections may still name them. agent_activity's
            // files_owned is on that list today; if it grows with ignored paths
            // that is an A.2 budget problem, not an A.1 leak.
            let mut keys = keys_containing(&repo, "cache/");
            keys.retain(|k| k != "recent_seals" && k != "agent_activity");
            assert!(keys.is_empty(), "cache/ in non-history sections: {keys:?}");

            // Sealing the .gitignore change records no deletion under cache/.
            let seal = seal_all(&repo, "ignore cache");
            assert!(
                seal.changes.iter().all(|c| !c.path.starts_with("cache/")),
                "{:?}",
                seal.changes
            );
            // Files are untouched on disk.
            assert!(dir.path().join("cache/deep/x.bin").exists());
        }

        #[test]
        fn test_no_gitignore_falls_back_to_writignore_defaults() {
            let dir = tempdir().unwrap();
            for rel in [
                "src/a.rs",
                "target/debug/a",
                "node_modules/x/i.js",
                ".venv/bin/py",
            ] {
                write(dir.path(), rel, "x\n");
            }
            let repo = Repository::init(dir.path()).unwrap();
            let paths: Vec<String> = state_paths(&repo).into_iter().map(|(p, _)| p).collect();
            assert_eq!(paths, vec!["src/a.rs".to_string()]);
        }

        #[test]
        fn test_nested_gitignore_file_behavior_documented() {
            // ignore-fidelity (findings 19, 30): sub/.gitignore is honored,
            // as git does. Flipped from the A.1 pin (root file only).
            let (dir, repo) = repo_with("", &["sub/keep.rs", "sub/local/scratch.txt"]);
            write(dir.path(), "sub/.gitignore", "local/\n");
            let paths: Vec<String> = state_paths(&repo).into_iter().map(|(p, _)| p).collect();
            assert!(
                !paths.contains(&"sub/local/scratch.txt".to_string()),
                "nested .gitignore not honored: {paths:?}"
            );
            assert!(paths.contains(&"sub/keep.rs".to_string()), "{paths:?}");
        }

        /// Finding 30's shape: a results directory carrying its own
        /// `.gitignore` of `*` plus `!.gitignore` is not staged.
        #[test]
        fn test_nested_star_gitignore_keeps_only_itself() {
            let (dir, repo) = repo_with("", &["bench/run.py", "bench/results/r1.json"]);
            write(dir.path(), "bench/results/.gitignore", "*\n!.gitignore\n");
            let paths: Vec<String> = state_paths(&repo).into_iter().map(|(p, _)| p).collect();
            assert!(!paths.iter().any(|p| p.ends_with("r1.json")), "{paths:?}");
            assert!(
                paths.contains(&"bench/results/.gitignore".to_string()),
                "{paths:?}"
            );
            assert!(paths.contains(&"bench/run.py".to_string()), "{paths:?}");
        }

        // ── Rejected seals must not write objects (finding 17 probe) ──

        fn object_count(root: &Path) -> usize {
            fn walk(dir: &Path) -> usize {
                fs::read_dir(dir)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| {
                                let p = e.path();
                                if p.is_dir() {
                                    walk(&p)
                                } else {
                                    1
                                }
                            })
                            .sum()
                    })
                    .unwrap_or(0)
            }
            walk(&root.join(".writ").join("objects"))
        }

        fn pending_files(root: &Path, prefix: &str, n: usize) {
            for i in 0..n {
                write(
                    root,
                    &format!("{prefix}/f{i}.txt"),
                    &format!("{prefix} {i}\n"),
                );
            }
        }

        // Finding 17: Repository::seal stores blobs and the tree before the
        // agent-scope check, so a strict-scope rejection leaves N+1 orphans.
        // Remove the ignore when sprint 2 seal-isolation validates first.
        #[test]
        #[ignore = "finding 17: seal writes objects before scope validation (sprint 2)"]
        fn test_rejected_scope_violation_seal_leaves_object_count_unchanged() {
            let dir = tempdir().unwrap();
            let mut repo = Repository::init(dir.path()).unwrap();
            repo.set_enforce_scope(true);
            repo.register_agent(
                "locked",
                "agent",
                crate::agent::TrustLevel::Standard,
                vec!["src/".to_string()],
            )
            .unwrap();
            pending_files(dir.path(), "vendor", 25);
            let before = object_count(dir.path());

            let err = repo
                .seal(
                    AgentIdentity {
                        id: "locked".to_string(),
                        agent_type: AgentType::Agent,
                    },
                    "rejected".to_string(),
                    None,
                    TaskStatus::InProgress,
                    Verification::default(),
                    false,
                )
                .unwrap_err();
            assert!(matches!(err, crate::WritError::ScopeViolation(_)), "{err}");
            assert_eq!(
                object_count(dir.path()),
                before,
                "rejected seal wrote objects into the store"
            );
        }

        #[test]
        fn test_rejected_revoked_agent_seal_leaves_object_count_unchanged() {
            let dir = tempdir().unwrap();
            let repo = Repository::init(dir.path()).unwrap();
            repo.register_agent("gone", "agent", crate::agent::TrustLevel::Standard, vec![])
                .unwrap();
            repo.revoke_agent("gone", "test").unwrap();
            pending_files(dir.path(), "src", 25);
            let before = object_count(dir.path());

            let err = repo
                .seal(
                    AgentIdentity {
                        id: "gone".to_string(),
                        agent_type: AgentType::Agent,
                    },
                    "rejected".to_string(),
                    None,
                    TaskStatus::InProgress,
                    Verification::default(),
                    false,
                )
                .unwrap_err();
            assert!(matches!(err, crate::WritError::AgentInactive(_)), "{err}");
            assert_eq!(object_count(dir.path()), before);
        }

        fn repo_root(repo: &Repository) -> &Path {
            repo.root()
        }
    }

    // ── finding 210: a path git tracks is tracked by writ ───────────

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    /// The writ repo's own shape: `scripts/` in .gitignore and in the
    /// imported block of .writignore, `scripts/bump-version.sh` force added.
    fn tracked_ignored_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        git(d, &["init", "-q"]);
        write(d, "README.md", "x\n");
        write(d, "scripts/bump-version.sh", "echo bump\n");
        write(d, "scripts/lib/util.sh", "echo util\n");
        write(d, "scripts/scratch.sh", "echo scratch\n");
        write(d, "scripts/tmp/out.txt", "out\n");
        write(d, "build/keep.txt", "keep\n");
        write(d, ".gitignore", ".writ/\nscripts/\nbuild/\n");
        write(
            d,
            ".writignore",
            "# --- writ defaults ---\n.writ\n.git\ntarget\n\n\
             # --- imported from .gitignore ---\n.writ/\nscripts/\n\n\
             # hand\nbuild\n",
        );
        git(d, &["add", "README.md", ".gitignore", ".writignore"]);
        git(
            d,
            &[
                "add",
                "-f",
                "scripts/bump-version.sh",
                "scripts/lib/util.sh",
                "build/keep.txt",
            ],
        );
        dir
    }

    #[test]
    fn f210_git_tracked_path_under_a_gitignored_dir_is_not_ignored() {
        let dir = tracked_ignored_repo();
        let rules = IgnoreRules::load(dir.path());
        // Tracked by git: tracked by writ, whatever .gitignore says.
        assert!(!rules.is_path_ignored("scripts/bump-version.sh"));
        assert!(!rules.is_path_ignored("scripts/lib/util.sh"));
        assert!(!rules.is_dir_ignored("scripts"));
        assert!(!rules.is_dir_ignored("scripts/lib"));
        assert!(!rules.is_file_ignored("scripts/bump-version.sh"));
        assert!(!rules.is_file_ignored("scripts/lib/util.sh"));
        // Untracked in the same directory: ignored, as git ignores it.
        assert!(rules.is_path_ignored("scripts/scratch.sh"));
        assert!(rules.is_file_ignored("scripts/scratch.sh"));
        assert!(rules.is_dir_ignored("scripts/tmp"));
        assert!(rules.is_path_ignored("scripts/tmp/out.txt"));
        assert!(rules.is_file_ignored("scripts/tmp/out.txt"));
        // .writ stays ignored whatever git holds.
        assert!(rules.is_dir_ignored(".writ"));
    }

    #[test]
    fn f210_a_hand_writignore_line_still_hides_a_tracked_path() {
        let dir = tracked_ignored_repo();
        let rules = IgnoreRules::load(dir.path());
        // `build` is a hand line in .writignore: the user's choice stands.
        assert!(rules.is_dir_ignored("build"));
        assert!(rules.is_path_ignored("build/keep.txt"));
        assert_eq!(rules.tracked_but_writ_ignored(), vec!["build/keep.txt"]);
    }

    #[test]
    fn f210_without_a_git_index_every_gitignore_rule_applies_as_before() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "scripts/\n");
        write(dir.path(), "scripts/bump-version.sh", "echo\n");
        let rules = IgnoreRules::load(dir.path());
        assert!(rules.is_dir_ignored("scripts"));
        assert!(rules.is_path_ignored("scripts/bump-version.sh"));
        assert!(rules.tracked_but_writ_ignored().is_empty());
        // No repo at all: layered rules are unchanged too.
        let rules = IgnoreRules::layered(
            Some("scripts/\n"),
            Some("# --- imported from .gitignore ---\nlogs/\n"),
        );
        assert!(rules.is_dir_ignored("scripts"));
        assert!(rules.is_dir_ignored("logs"));
        assert!(rules.is_path_ignored("logs/a.txt"));
    }

    #[test]
    fn f210_import_block_ends_at_the_first_blank_or_comment_line() {
        let rules = IgnoreRules::layered(
            None,
            Some(
                "# --- writ defaults ---\n.git\n\n# --- imported from .gitignore ---\n\
                 docs/\nscripts/\n\n# compiled python extension\n*.so\n",
            ),
        );
        assert_eq!(rules.decide(".git", true), Verdict::Ignored(Source::Writ));
        assert_eq!(rules.decide("docs", true), Verdict::Ignored(Source::Git));
        assert_eq!(rules.decide("scripts", true), Verdict::Ignored(Source::Git));
        assert_eq!(rules.decide("a.so", false), Verdict::Ignored(Source::Writ));
        // Line order within .writignore still decides: a later hand line
        // re-includes what an imported line ignored.
        let rules = IgnoreRules::layered(
            None,
            Some("# --- imported from .gitignore ---\n*.log\n\n!keep.log\n"),
        );
        assert_eq!(rules.decide("a.log", false), Verdict::Ignored(Source::Git));
        assert_eq!(rules.decide("keep.log", false), Verdict::Whitelisted);
    }

    #[test]
    fn f210_index_reader_matches_git_ls_files_for_versions_2_3_and_4() {
        let dir = tracked_ignored_repo();
        let d = dir.path();
        write(d, "deep/a/b/c/one.txt", "1\n");
        write(d, "deep/a/b/c/two.txt", "2\n");
        write(d, "deep/a/b/three.txt", "3\n");
        write(d, "na\u{ef}ve name.txt", "4\n");
        git(d, &["add", "deep", "na\u{ef}ve name.txt"]);
        // Intent to add sets an extended flag (a version 3 entry).
        write(d, "later.txt", "5\n");
        git(d, &["add", "-N", "later.txt"]);
        let mut expected: Vec<String> = git(d, &["-c", "core.quotepath=off", "ls-files"])
            .lines()
            .map(str::to_string)
            .collect();
        expected.sort();
        for version in ["3", "4", "2"] {
            if version == "2" {
                git(d, &["rm", "-q", "--cached", "later.txt"]);
                expected.retain(|p| p != "later.txt");
            }
            git(d, &["update-index", "--index-version", version]);
            let data = fs::read(d.join(".git/index")).unwrap();
            assert_eq!(&data[4..8], &[0, 0, 0, version.parse::<u8>().unwrap()]);
            assert_eq!(
                git_index::tracked_paths(d).unwrap(),
                expected,
                "index version {version}"
            );
        }
    }

    #[test]
    fn f210_index_reader_follows_a_dot_git_file() {
        let dir = tracked_ignored_repo();
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("wt");
        git(
            dir.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "x",
            ],
        );
        git(
            dir.path(),
            &["worktree", "add", "-q", wt_path.to_str().unwrap()],
        );
        assert!(wt_path.join(".git").is_file());
        let paths = git_index::tracked_paths(&wt_path).unwrap();
        assert!(
            paths.contains(&"scripts/bump-version.sh".to_string()),
            "{paths:?}"
        );
        assert!(!IgnoreRules::load(&wt_path).is_path_ignored("scripts/bump-version.sh"));
    }

    #[test]
    fn f210_cached_index_sees_gits_next_write() {
        let dir = tracked_ignored_repo();
        let d = dir.path();
        let rules = IgnoreRules::load(d);
        assert!(rules.is_path_ignored("scripts/scratch.sh"));
        git(d, &["add", "-f", "scripts/scratch.sh"]);
        // A new load in the same process reads the rewritten index.
        assert!(!IgnoreRules::load(d).is_path_ignored("scripts/scratch.sh"));
        git(d, &["rm", "-q", "--cached", "scripts/scratch.sh"]);
        assert!(IgnoreRules::load(d).is_path_ignored("scripts/scratch.sh"));
    }

    #[test]
    fn f210_index_reader_rejects_garbage() {
        assert_eq!(git_index::parse(b""), None);
        assert_eq!(git_index::parse(b"DIRC\0\0\0\x09\0\0\0\0"), None);
        // Declares one entry, holds none.
        assert_eq!(git_index::parse(b"DIRC\0\0\0\x02\0\0\0\x01"), None);
        assert_eq!(git_index::parse(b"DIRC\0\0\0\x02\0\0\0\0"), Some(vec![]));
    }
}
