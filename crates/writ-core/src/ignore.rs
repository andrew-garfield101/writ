//! Ignore rules: live `.gitignore` layered under `.writignore`.
//!
//! Matching uses full gitignore semantics via the `ignore` crate's
//! `gitignore` module: anchored paths (`/build`), directory-only patterns
//! (`.venv*/`), `**` globs (`**/node_modules/`, `build/**`), character classes
//! (`*.py[cod]`), and negation (`!keep.log`).
//!
//! Layering, lowest to highest precedence (last match wins):
//! 1. Repo root `.gitignore`, read fresh on every load.
//! 2. `.writignore` if it exists, otherwise writ's built-in defaults.
//!
//! `.writ` is ALWAYS ignored, regardless of either file (negation cannot
//! un-ignore it).

use std::fs;
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

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

/// A compiled, layered set of ignore rules.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    matcher: Gitignore,
    invalid: Vec<InvalidPattern>,
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
        let mut count = 0;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
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
            if let Err(e) = self.builder.add_line(None, trimmed) {
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

    fn build(mut self) -> IgnoreRules {
        let matcher = match self.builder.build() {
            Ok(m) => m,
            Err(e) => {
                // Only reachable if the glob set as a whole fails to compile.
                self.reject("<all>", "", &e.to_string());
                Gitignore::empty()
            }
        };
        IgnoreRules {
            matcher,
            invalid: self.invalid,
        }
    }
}

impl IgnoreRules {
    /// Load live rules for a repo: root `.gitignore`, then `.writignore`
    /// (or built-in defaults when `.writignore` is absent).
    pub fn load(repo_root: &Path) -> Self {
        let mut read_errors = Vec::new();
        let gitignore = read_optional(&repo_root.join(".gitignore"), &mut read_errors);
        let writignore = read_optional(&repo_root.join(".writignore"), &mut read_errors);
        let mut rules = Self::layered(gitignore.as_deref(), writignore.as_deref());
        rules.invalid.extend(read_errors);
        rules
    }

    /// Build rules from raw `.gitignore` and `.writignore` contents.
    ///
    /// `.writignore` rules are layered after `.gitignore`, so they win on
    /// conflict (e.g. `!docs/` in `.writignore` re-includes a gitignored dir).
    pub fn layered(gitignore: Option<&str>, writignore: Option<&str>) -> Self {
        let mut b = RulesBuilder::new();
        if let Some(content) = gitignore {
            b.add_content(".gitignore", content);
        }
        match writignore {
            Some(content) => b.add_content(".writignore", content),
            None => b.add_dir_names("defaults", DEFAULT_IGNORE_DIRS),
        }
        b.build()
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

    /// Patterns that failed to compile or exceeded safety limits.
    pub fn invalid_patterns(&self) -> &[InvalidPattern] {
        &self.invalid
    }

    /// Should the directory at this repo-relative path be pruned from walks?
    ///
    /// Only the path itself is checked; walkers prune parents before
    /// reaching children. Use [`IgnoreRules::is_path_ignored`] for arbitrary paths.
    pub fn is_dir_ignored(&self, rel_path: &str) -> bool {
        if is_always_ignored(rel_path) {
            return true;
        }
        self.matcher.matched(rel_path, true).is_ignore()
    }

    /// Should the file at this repo-relative path be ignored?
    ///
    /// Only the path itself is checked (see [`IgnoreRules::is_dir_ignored`]).
    pub fn is_file_ignored(&self, rel_path: &str) -> bool {
        if is_always_ignored(rel_path) {
            return true;
        }
        self.matcher.matched(rel_path, false).is_ignore()
    }

    /// Is this repo-relative file path ignored, either directly or because
    /// any parent directory is ignored? Used for paths that did not come
    /// from a pruned walk (index entries, seal trees).
    pub fn is_path_ignored(&self, rel_path: &str) -> bool {
        let rel_path = rel_path.trim_start_matches('/');
        if rel_path.is_empty() {
            return false;
        }
        if is_always_ignored(rel_path) {
            return true;
        }
        self.matcher
            .matched_path_or_any_parents(rel_path, false)
            .is_ignore()
    }
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
            // A.1 reads the ROOT .gitignore only. Git would also honor
            // sub/.gitignore. Pinned so a change here is a deliberate one.
            let (dir, repo) = repo_with("", &["sub/keep.rs", "sub/local/scratch.txt"]);
            write(dir.path(), "sub/.gitignore", "local/\n");
            let paths: Vec<String> = state_paths(&repo).into_iter().map(|(p, _)| p).collect();
            assert!(
                paths.contains(&"sub/local/scratch.txt".to_string()),
                "nested .gitignore now honored; update this test and the docs: {paths:?}"
            );
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
}
