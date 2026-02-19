//! .writignore — user-configurable file/directory ignore rules.
//!
//! Supports a simplified .gitignore-like format:
//! - Blank lines are ignored
//! - Lines starting with `#` are comments
//! - Directory names (e.g., `target`) match any directory with that name
//! - Glob patterns (e.g., `*.pyc`) match against filenames
//!
//! `.writ` is ALWAYS ignored. When a `.writignore` file exists, it replaces
//! the other defaults — users own their ignore list.

use std::fs;
use std::path::Path;

/// Directories that are ALWAYS ignored, regardless of `.writignore` contents.
const ALWAYS_IGNORED_DIRS: &[&str] = &[".writ"];

/// Default ignore rules used when no `.writignore` file exists.
const DEFAULT_IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];

/// A parsed set of ignore rules.
#[derive(Debug, Clone)]
pub struct IgnoreRules {
    /// Directory names to ignore (exact match against any path component).
    dir_names: Vec<String>,
    /// Glob patterns matched against filenames (e.g., `*.pyc`).
    file_globs: Vec<String>,
}

impl IgnoreRules {
    /// Load from `.writignore` at repo root, or fall back to defaults.
    pub fn load(repo_root: &Path) -> Self {
        let path = repo_root.join(".writignore");
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                return Self::parse(&content);
            }
        }
        Self::defaults()
    }

    /// Hardcoded defaults (used when no `.writignore` exists).
    pub fn defaults() -> Self {
        let mut dir_names: Vec<String> =
            ALWAYS_IGNORED_DIRS.iter().map(|s| s.to_string()).collect();
        dir_names.extend(DEFAULT_IGNORE_DIRS.iter().map(|s| s.to_string()));
        IgnoreRules {
            dir_names,
            file_globs: Vec::new(),
        }
    }

    /// Parse `.writignore` content into rules.
    ///
    /// Enforces safety limits: max 1000 rules, max 1024 chars per pattern.
    pub fn parse(content: &str) -> Self {
        const MAX_RULES: usize = 1000;
        const MAX_PATTERN_LEN: usize = 1024;

        let mut dir_names: Vec<String> =
            ALWAYS_IGNORED_DIRS.iter().map(|s| s.to_string()).collect();
        let mut file_globs = Vec::new();
        let mut count = 0;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if count >= MAX_RULES || trimmed.len() > MAX_PATTERN_LEN {
                continue;
            }
            count += 1;

            if trimmed.contains('*') || trimmed.contains('?') {
                file_globs.push(trimmed.to_string());
            } else {
                dir_names.push(trimmed.trim_end_matches('/').to_string());
            }
        }

        IgnoreRules {
            dir_names,
            file_globs,
        }
    }

    /// Should this directory name be skipped during WalkDir?
    pub fn is_dir_ignored(&self, name: &str) -> bool {
        self.dir_names.iter().any(|d| d == name)
    }

    /// Should this file be ignored? Checks filename against glob patterns.
    pub fn is_file_ignored(&self, rel_path: &str) -> bool {
        let filename = rel_path.rsplit('/').next().unwrap_or(rel_path);
        self.file_globs
            .iter()
            .any(|pattern| glob_match(pattern, filename))
    }
}

/// Simple glob matching: `*` matches any characters, `?` matches one character.
fn glob_match(pattern: &str, text: &str) -> bool {
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
}
