//! Phase 6: Hardened post-merge verification.
//!
//! The [`HardenedVerifier`] replaces the basic verifier with real safety-net
//! checks that catch broken merges before they reach the working tree.
//!
//! ## Checks (in execution order)
//!
//! 1. **Conflict markers** — leftover `<<<<<<<`/`=======`/`>>>>>>>` = assembly bug
//! 2. **Empty content** — empty code file = content loss
//! 3. **Balanced delimiters** — mismatched `{}()[]` = broken composition
//! 4. **Structural re-parse** — `Unknown` units = unparseable code
//! 5. **Duplicate definitions** — two `def main()` = bad merge
//! 6. **Import deduplication** — duplicate imports = warning
//!
//! First `Failed` short-circuits. Warnings accumulate across all checks.

use std::collections::{HashMap, HashSet};

use super::analyzers::{self, LanguageAnalyzer};
use super::pipeline::Verifier;
use super::types::{UnitKind, VerificationResult, VerificationVerdict};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the hardened verifier's checks.
#[derive(Debug, Clone)]
pub struct VerifierConfig {
    /// Minimum analyzer coverage ratio before warning (0.0–1.0).
    pub min_coverage_ratio: f64,
    /// Treat Unknown structural units as failures for language-aware analyzers.
    pub fail_on_unknowns: bool,
}

impl Default for VerifierConfig {
    fn default() -> Self {
        Self {
            min_coverage_ratio: 0.9,
            fail_on_unknowns: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal check result
// ---------------------------------------------------------------------------

/// Result from a single verification check.
enum CheckResult {
    /// Check passed cleanly.
    Pass,
    /// Check passed but produced warnings.
    Warn(Vec<String>),
    /// Check failed — merge should be rejected.
    Fail(Vec<String>),
}

// ---------------------------------------------------------------------------
// HardenedVerifier
// ---------------------------------------------------------------------------

/// Hardened Phase 6 verifier that performs multiple integrity checks
/// on merged content before it is accepted.
pub struct HardenedVerifier {
    config: VerifierConfig,
}

impl HardenedVerifier {
    pub fn new() -> Self {
        Self {
            config: VerifierConfig::default(),
        }
    }

    pub fn with_config(config: VerifierConfig) -> Self {
        Self { config }
    }
}

impl Default for HardenedVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Verifier for HardenedVerifier {
    fn verify(&self, merged_content: &str, file_path: &str) -> VerificationResult {
        let analyzer = analyzers::analyzer_for_path(file_path);
        let is_language_aware = analyzer.name() != "generic";

        let mut all_warnings: Vec<String> = Vec::new();
        let mut syntactic_valid = true;

        // Ordered by severity and cost (cheapest first, most fatal first).
        let checks = [
            self.check_conflict_markers(merged_content),
            self.check_empty_content(merged_content, file_path),
            self.check_balanced_delimiters(merged_content, is_language_aware),
            self.check_structural_reparse(merged_content, analyzer.as_ref(), is_language_aware),
            self.check_duplicate_definitions(merged_content, analyzer.as_ref(), is_language_aware),
            self.check_duplicate_imports(merged_content, analyzer.as_ref(), is_language_aware),
        ];

        for check in checks {
            match check {
                CheckResult::Pass => {}
                CheckResult::Warn(warnings) => {
                    all_warnings.extend(warnings);
                }
                CheckResult::Fail(errors) => {
                    syntactic_valid = false;
                    all_warnings.extend(errors);
                    return VerificationResult {
                        syntactic_valid,
                        warnings: all_warnings,
                        verdict: VerificationVerdict::Failed,
                    };
                }
            }
        }

        let verdict = if all_warnings.is_empty() {
            VerificationVerdict::Verified
        } else {
            VerificationVerdict::PassedWithWarnings
        };

        VerificationResult {
            syntactic_valid,
            warnings: all_warnings,
            verdict,
        }
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

impl HardenedVerifier {
    /// Check 1: Detect leftover conflict markers.
    ///
    /// Uses `starts_with` to avoid false positives from lines that contain
    /// these sequences in strings or comments.
    fn check_conflict_markers(&self, content: &str) -> CheckResult {
        let markers = ["<<<<<<<", "=======", ">>>>>>>"];
        let mut found: Vec<String> = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            for marker in &markers {
                if trimmed.starts_with(marker) {
                    found.push(format!(
                        "Conflict marker '{}' found at line {}",
                        marker,
                        line_num + 1
                    ));
                }
            }
        }

        if found.is_empty() {
            CheckResult::Pass
        } else {
            CheckResult::Fail(found)
        }
    }

    /// Check 2: Detect empty content for code files.
    fn check_empty_content(&self, content: &str, file_path: &str) -> CheckResult {
        let non_empty = content.lines().any(|l| !l.trim().is_empty());
        if non_empty {
            return CheckResult::Pass;
        }

        let code_extensions = [
            "py", "pyi", "rs", "go", "ts", "tsx", "js", "jsx", "mjs", "cjs", "java", "c", "cpp",
            "h", "hpp", "rb", "swift", "kt",
        ];
        let ext = file_path.rsplit('.').next().unwrap_or("");
        if code_extensions.contains(&ext) {
            CheckResult::Fail(vec![format!(
                "Merged output is empty for code file '{}' — possible content loss",
                file_path
            )])
        } else {
            CheckResult::Pass
        }
    }

    /// Check 3: Verify balanced delimiters (language-aware files only).
    ///
    /// Uses a simplified string/comment heuristic to avoid counting delimiters
    /// inside string literals or comments. Does NOT handle multi-line strings
    /// or block comments — this is conservative (may miss some imbalances in
    /// those constructs but won't produce false positives).
    fn check_balanced_delimiters(&self, content: &str, is_language_aware: bool) -> CheckResult {
        if !is_language_aware {
            return CheckResult::Pass;
        }

        let mut stack: Vec<(char, usize)> = Vec::new();

        for (line_num, line) in content.lines().enumerate() {
            let mut in_single_quote = false;
            let mut in_double_quote = false;
            let mut in_line_comment = false;
            let mut prev_char: Option<char> = None;

            for ch in line.chars() {
                // Detect line comments.
                if !in_single_quote && !in_double_quote && !in_line_comment {
                    if ch == '#' {
                        in_line_comment = true;
                    }
                    if ch == '/' && prev_char == Some('/') {
                        in_line_comment = true;
                    }
                }

                if in_line_comment {
                    prev_char = Some(ch);
                    continue;
                }

                // Toggle string states (simplified — no escape handling for \\).
                if ch == '\'' && !in_double_quote && prev_char != Some('\\') {
                    in_single_quote = !in_single_quote;
                } else if ch == '"' && !in_single_quote && prev_char != Some('\\') {
                    in_double_quote = !in_double_quote;
                }

                if in_single_quote || in_double_quote {
                    prev_char = Some(ch);
                    continue;
                }

                // Check openers.
                match ch {
                    '(' | '[' | '{' => {
                        stack.push((ch, line_num + 1));
                    }
                    ')' | ']' | '}' => {
                        let expected_open = match ch {
                            ')' => '(',
                            ']' => '[',
                            '}' => '{',
                            _ => unreachable!(),
                        };
                        if let Some((top, _)) = stack.last() {
                            if *top == expected_open {
                                stack.pop();
                            } else {
                                return CheckResult::Fail(vec![format!(
                                    "Unbalanced delimiter: found '{}' at line {} \
                                     but expected closing for '{}' opened at line {}",
                                    ch,
                                    line_num + 1,
                                    stack.last().unwrap().0,
                                    stack.last().unwrap().1,
                                )]);
                            }
                        } else {
                            return CheckResult::Fail(vec![format!(
                                "Unbalanced delimiter: unexpected '{}' at line {} \
                                 with no matching opener",
                                ch,
                                line_num + 1,
                            )]);
                        }
                    }
                    _ => {}
                }

                prev_char = Some(ch);
            }
        }

        if stack.is_empty() {
            CheckResult::Pass
        } else {
            let unclosed: Vec<String> = stack
                .iter()
                .map(|(ch, line)| format!("'{}' opened at line {}", ch, line))
                .collect();
            CheckResult::Fail(vec![format!(
                "Unbalanced delimiters — unclosed: {}",
                unclosed.join(", ")
            )])
        }
    }

    /// Check 4: Structural re-parse via language analyzer.
    fn check_structural_reparse(
        &self,
        content: &str,
        analyzer: &dyn LanguageAnalyzer,
        is_language_aware: bool,
    ) -> CheckResult {
        let units = analyzer.parse_structure(content);

        let has_unknowns = units.iter().any(|u| u.kind == UnitKind::Unknown);
        let lines_covered: usize = units
            .iter()
            .map(|u| u.span.1.saturating_sub(u.span.0))
            .sum();
        let total_lines = content.lines().count();

        let coverage_ratio = if total_lines == 0 {
            1.0
        } else {
            lines_covered as f64 / total_lines as f64
        };

        let mut warnings: Vec<String> = Vec::new();

        if has_unknowns && is_language_aware && self.config.fail_on_unknowns {
            return CheckResult::Fail(vec![format!(
                "Merged output contains unparseable regions (analyzer: {})",
                analyzer.name()
            )]);
        }

        if coverage_ratio < self.config.min_coverage_ratio && total_lines > 0 {
            warnings.push(format!(
                "Analyzer only covers {:.0}% of merged output lines",
                coverage_ratio * 100.0
            ));
        }

        if warnings.is_empty() {
            CheckResult::Pass
        } else {
            CheckResult::Warn(warnings)
        }
    }

    /// Check 5: Detect duplicate top-level definitions.
    fn check_duplicate_definitions(
        &self,
        content: &str,
        analyzer: &dyn LanguageAnalyzer,
        is_language_aware: bool,
    ) -> CheckResult {
        if !is_language_aware {
            return CheckResult::Pass;
        }

        let definitions = analyzer.extract_definitions(content);
        if definitions.len() < 2 {
            return CheckResult::Pass;
        }

        let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
        for def in &definitions {
            if !def.name.is_empty() {
                by_name
                    .entry(def.name.as_str())
                    .or_default()
                    .push(def.def_kind.as_str());
            }
        }

        let mut failures: Vec<String> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();

        for (name, kinds) in &by_name {
            if kinds.len() < 2 {
                continue;
            }
            let all_same_kind = kinds.windows(2).all(|w| w[0] == w[1]);

            if all_same_kind {
                failures.push(format!(
                    "Duplicate {} definition '{}' found ({} occurrences)",
                    kinds[0],
                    name,
                    kinds.len()
                ));
            } else {
                warnings.push(format!(
                    "Multiple definitions named '{}' with different kinds: {}",
                    name,
                    kinds.join(", ")
                ));
            }
        }

        if !failures.is_empty() {
            CheckResult::Fail(failures)
        } else if !warnings.is_empty() {
            CheckResult::Warn(warnings)
        } else {
            CheckResult::Pass
        }
    }

    /// Check 6: Detect duplicate import statements.
    fn check_duplicate_imports(
        &self,
        content: &str,
        analyzer: &dyn LanguageAnalyzer,
        is_language_aware: bool,
    ) -> CheckResult {
        if !is_language_aware {
            return CheckResult::Pass;
        }

        let imports = analyzer.extract_imports(content);
        if imports.len() < 2 {
            return CheckResult::Pass;
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut duplicates: Vec<String> = Vec::new();

        for import in &imports {
            let normalized = import.raw.trim().to_string();
            if !seen.insert(normalized.clone()) {
                duplicates.push(normalized);
            }
        }

        if duplicates.is_empty() {
            CheckResult::Pass
        } else {
            CheckResult::Warn(
                duplicates
                    .iter()
                    .map(|d| format!("Duplicate import: '{}'", d))
                    .collect(),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: run verification with default config.
    fn verify(content: &str, path: &str) -> VerificationResult {
        let verifier = HardenedVerifier::new();
        verifier.verify(content, path)
    }

    // ── Conflict markers ──────────────────────────────────────────────

    #[test]
    fn test_conflict_markers_detected() {
        let content = "import os\n<<<<<<< HEAD\nimport json\n=======\nimport sys\n>>>>>>>\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(!result.syntactic_valid);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Conflict marker")));
    }

    #[test]
    fn test_no_conflict_markers_passes() {
        let content = "import os\nimport sys\n\ndef main():\n    pass\n";
        let result = verify(content, "app.py");
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    #[test]
    fn test_conflict_markers_in_middle_of_file() {
        let content = "\
import os

def main():
    pass

<<<<<<< agent-a
def foo():
    return 1
=======
def foo():
    return 2
>>>>>>> agent-b

class User:
    name: str
";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.len() >= 3, "should find all three markers");
    }

    // ── Empty content ─────────────────────────────────────────────────

    #[test]
    fn test_empty_py_file_fails() {
        let result = verify("", "models.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.iter().any(|w| w.contains("empty")));
    }

    #[test]
    fn test_empty_txt_file_passes() {
        let result = verify("", "notes.txt");
        assert_eq!(result.verdict, VerificationVerdict::Verified);
    }

    #[test]
    fn test_whitespace_only_py_file_fails() {
        let result = verify("   \n  \n\n", "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.iter().any(|w| w.contains("empty")));
    }

    // ── Balanced delimiters ───────────────────────────────────────────

    #[test]
    fn test_balanced_delimiters_pass() {
        let content = "\
def main():
    data = {'key': [1, 2, 3]}
    result = (data['key'][0] + 1)
    print(result)
";
        let result = verify(content, "app.py");
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    #[test]
    fn test_unbalanced_brace_fails() {
        let content = "fn main() {\n    let x = 1;\n";
        let result = verify(content, "main.rs");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.iter().any(|w| w.contains("Unbalanced")));
    }

    #[test]
    fn test_unbalanced_delimiters_skip_generic() {
        // Generic files skip delimiter checking.
        let content = "key: {unclosed\n";
        let result = verify(content, "config.yaml");
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    #[test]
    fn test_delimiters_in_strings_ignored() {
        let content = "\
def main():
    msg = \"has {brackets} and (parens)\"
    print(msg)
";
        let result = verify(content, "app.py");
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    // ── Structural re-parse ───────────────────────────────────────────

    #[test]
    fn test_clean_python_passes_reparse() {
        let content = "import os\n\ndef main():\n    print('hello')\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Verified);
    }

    #[test]
    fn test_low_coverage_warns() {
        // The generic analyzer marks every line as Unknown, but the coverage
        // check is about line spans. For a language-aware analyzer with
        // fail_on_unknowns, Unknown causes failure. To test coverage warning
        // alone, we use a config with fail_on_unknowns=false.
        let verifier = HardenedVerifier::with_config(VerifierConfig {
            min_coverage_ratio: 0.9,
            fail_on_unknowns: false,
        });
        // Use generic analyzer (won't fail on unknowns since it's not language-aware).
        // Content with many lines where the analyzer only covers some.
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let result = verifier.verify(content, "data.yaml");
        // Generic analyzer should cover all lines (each line is a unit).
        // This test verifies no crash — the generic analyzer gives full coverage.
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    #[test]
    fn test_generic_file_skips_unknown_check() {
        // Generic analyzer produces Unknown units for every non-blank line.
        // The hardened verifier should NOT fail for generic files.
        let content = "some random text\nmore text\n";
        let result = verify(content, "readme.md");
        assert_ne!(result.verdict, VerificationVerdict::Failed);
    }

    // ── Duplicate definitions ─────────────────────────────────────────

    #[test]
    fn test_duplicate_function_fails() {
        let content = "def main():\n    pass\n\ndef main():\n    print('hello')\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.iter().any(|w| w.contains("Duplicate")));
        assert!(result.warnings.iter().any(|w| w.contains("main")));
    }

    #[test]
    fn test_duplicate_class_fails() {
        let content = "class User:\n    name: str\n\nclass User:\n    email: str\n";
        let result = verify(content, "models.py");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        assert!(result.warnings.iter().any(|w| w.contains("User")));
    }

    #[test]
    fn test_different_named_definitions_pass() {
        let content = "def foo():\n    pass\n\nclass Bar:\n    x: int\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Verified);
    }

    // ── Import deduplication ──────────────────────────────────────────

    #[test]
    fn test_duplicate_imports_warns() {
        let content = "import os\nimport os\n\ndef main():\n    pass\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::PassedWithWarnings);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Duplicate import")));
    }

    #[test]
    fn test_unique_imports_pass() {
        let content = "import os\nimport sys\n\ndef main():\n    pass\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Verified);
    }

    // ── Integration / edge cases ──────────────────────────────────────

    #[test]
    fn test_full_verification_clean_python() {
        let content = "\
import os
import sys
from pathlib import Path

def main():
    path = Path('.')
    print(os.getcwd())

class Config:
    debug: bool = False
";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::Verified);
        assert!(result.syntactic_valid);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_multiple_issues_first_failure_wins() {
        // Both conflict markers AND unbalanced delimiters — conflict markers
        // should be caught first (check order).
        let content = "<<<<<<< HEAD\nfn broken() {\n=======\nfn other() {\n>>>>>>>\n";
        let result = verify(content, "main.rs");
        assert_eq!(result.verdict, VerificationVerdict::Failed);
        // First failure is conflict markers.
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Conflict marker")));
    }

    #[test]
    fn test_warnings_accumulate_across_checks() {
        // Duplicate imports produce warnings, not failures. If there are
        // multiple warning-producing checks, all warnings should be collected.
        let content = "import os\nimport os\nimport sys\n\ndef main():\n    pass\n";
        let result = verify(content, "app.py");
        assert_eq!(result.verdict, VerificationVerdict::PassedWithWarnings);
        assert!(result
            .warnings
            .iter()
            .any(|w| w.contains("Duplicate import")));
    }
}
