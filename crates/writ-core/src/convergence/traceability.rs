//! Content traceability validation — the no-silent-addition rule.
//!
//! Ensures that merged output contains only content traceable to the three
//! inputs (base, left, right). Two-tier check:
//!
//! - **Tier 1 (Named unit check):** Every function, class, struct, or named
//!   definition in the merged output must have a matching name in at least one
//!   input. Catches hallucinated definitions.
//!
//! - **Tier 2 (Line-level traceability):** Every non-trivial line in the merged
//!   output must have a best-match similarity >= 0.90 against all input lines
//!   (on normalized content). Catches novel lines.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::analyzers;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Input to the traceability checker.
#[derive(Debug, Clone)]
pub struct TraceabilityInput {
    /// Path of the file being checked (used for analyzer dispatch).
    pub file_path: String,
    /// Base (common ancestor) content.
    pub base: String,
    /// Left side content.
    pub left: String,
    /// Right side content.
    pub right: String,
    /// Merged output to validate.
    pub merged: String,
}

/// A named unit in the merged output not found in any input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NovelUnit {
    /// The name of the novel definition.
    pub name: String,
    /// What kind of unit (e.g. "function", "class", "struct").
    pub unit_kind: String,
    /// Line span in the merged output (0-indexed, start inclusive, end exclusive).
    pub span: (usize, usize),
}

/// A line in the merged output that could not be traced to any input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UntracedLine {
    /// 1-based line number in the merged output.
    pub line_number: usize,
    /// The content of the untraced line.
    pub content: String,
    /// Best similarity score found across all input lines.
    pub best_similarity: f64,
    /// Which input had the best match ("base", "left", or "right").
    pub best_match_source: Option<String>,
    /// The content of the best-matching input line.
    pub best_match_content: Option<String>,
}

/// Outcome of the traceability check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TraceabilityVerdict {
    /// All content is traceable to inputs.
    Pass,
    /// Content traceability check failed.
    Fail,
}

/// Full report from the traceability validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceabilityReport {
    /// File that was checked.
    pub file_path: String,
    /// Overall verdict.
    pub verdict: TraceabilityVerdict,
    /// Named units in merged output not found in any input (Tier 1 failures).
    pub novel_units: Vec<NovelUnit>,
    /// Lines in merged output not traceable to inputs (Tier 2 flags).
    pub untraced_lines: Vec<UntracedLine>,
    /// Total non-trivial content lines checked in Tier 2.
    pub lines_checked: usize,
    /// Number of lines that passed traceability in Tier 2.
    pub lines_passed: usize,
    /// The threshold used (0 for beta strict mode).
    pub threshold: usize,
    /// Human-readable summary for actionable failure reporting.
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Normalization & Similarity
// ---------------------------------------------------------------------------

/// Normalize a line for similarity comparison.
/// Strips leading/trailing whitespace, collapses internal whitespace to single spaces.
fn normalize_line(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compute Levenshtein edit distance using single-row DP (O(min(m,n)) space).
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let (m, n) = (a_bytes.len(), b_bytes.len());

    // Ensure shorter string is in the inner loop for space efficiency.
    let (a_bytes, b_bytes, m, n) = if m > n {
        (b_bytes, a_bytes, n, m)
    } else {
        (a_bytes, b_bytes, m, n)
    };

    let mut prev: Vec<usize> = (0..=m).collect();

    for j in 1..=n {
        let mut curr = vec![0; m + 1];
        curr[0] = j;
        for i in 1..=m {
            let cost = if a_bytes[i - 1] == b_bytes[j - 1] {
                0
            } else {
                1
            };
            curr[i] = (prev[i] + 1).min(curr[i - 1] + 1).min(prev[i - 1] + cost);
        }
        prev = curr;
    }

    prev[m]
}

/// Compute similarity between two lines using normalized Levenshtein distance.
/// Returns a value in [0.0, 1.0] where 1.0 = identical after normalization.
fn line_similarity(a: &str, b: &str) -> f64 {
    let na = normalize_line(a);
    let nb = normalize_line(b);

    if na == nb {
        return 1.0;
    }
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }

    let distance = levenshtein_distance(&na, &nb);
    let max_len = na.len().max(nb.len());
    1.0 - (distance as f64 / max_len as f64)
}

// ---------------------------------------------------------------------------
// Trivial line detection
// ---------------------------------------------------------------------------

/// Returns true if a line is trivial and should be excluded from
/// line-level traceability checking (Tier 2).
///
/// Language-aware: dispatches on file extension to detect comments and
/// language-specific structural scaffolding that legitimately appears in
/// merged output without being traceable to a specific input.
pub fn is_trivial(line: &str, file_path: &str) -> bool {
    let trimmed = line.trim();

    // Base checks (all languages)
    if is_trivial_base(trimmed) {
        return true;
    }

    // Language-specific checks
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "py" | "pyi" => is_trivial_python(trimmed),
        "rs" => is_trivial_rust(trimmed),
        "go" => is_trivial_go(trimmed),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => is_trivial_js_ts(trimmed),
        _ => false,
    }
}

/// Base trivial checks shared across all languages.
fn is_trivial_base(trimmed: &str) -> bool {
    // Empty / pure whitespace
    if trimmed.is_empty() {
        return true;
    }

    // Single-character lines (braces, brackets, parens)
    if trimmed.len() == 1 {
        return true;
    }

    // Common structural-only patterns
    matches!(
        trimmed,
        "}" | "{"
            | "};"
            | "},"
            | "});"
            | "})"
            | "]"
            | "];"
            | "],"
            | ")"
            | ");"
            | "),"
            | "pass"
            | "..."
            | "else:"
            | "else {"
            | "} else {"
            | "} else"
            | "return"
            | "return;"
            | "break"
            | "break;"
            | "continue"
            | "continue;"
    )
}

/// Python-specific trivial line detection.
fn is_trivial_python(trimmed: &str) -> bool {
    // Comment-only lines
    if trimmed.starts_with('#') {
        return true;
    }

    // Python structural keywords (bare, no logic)
    matches!(trimmed, "try:" | "except:" | "finally:" | "else:" | "raise")
}

/// Rust-specific trivial line detection.
fn is_trivial_rust(trimmed: &str) -> bool {
    // Comment-only lines (line comments, doc comments, inner doc comments)
    if trimmed.starts_with("//") {
        return true;
    }

    // Block comment lines (opening, closing, or interior)
    if is_block_comment_line(trimmed) {
        return true;
    }

    // Rust structural keywords
    matches!(
        trimmed,
        "unsafe {" | "loop {" | "Ok(())" | "Ok(());" | "None" | "None;"
    )
}

/// Go-specific trivial line detection.
fn is_trivial_go(trimmed: &str) -> bool {
    // Comment-only lines
    if trimmed.starts_with("//") {
        return true;
    }

    // Block comment lines
    if is_block_comment_line(trimmed) {
        return true;
    }

    // Go structural keywords
    matches!(trimmed, "default:")
}

/// TypeScript/JavaScript-specific trivial line detection.
fn is_trivial_js_ts(trimmed: &str) -> bool {
    // Comment-only lines
    if trimmed.starts_with("//") {
        return true;
    }

    // Block comment lines
    if is_block_comment_line(trimmed) {
        return true;
    }

    // Structural keywords
    matches!(trimmed, "default:" | "default: {")
}

/// Detect C-style block comment lines: `/* ... */`, `*`, `*/`.
/// Used by Rust, Go, JS/TS where `/* */` comments are valid.
fn is_block_comment_line(trimmed: &str) -> bool {
    // Opening: `/*` or `/**`
    if trimmed.starts_with("/*") {
        return true;
    }
    // Closing: `*/`
    if trimmed == "*/" {
        return true;
    }
    // Interior lines: `* text...` (common doc-comment style)
    if trimmed.starts_with("* ") || trimmed == "*" {
        return true;
    }
    false
}

// ---------------------------------------------------------------------------
// Core Algorithm
// ---------------------------------------------------------------------------

/// Validate that no content in the merged output is untraceable to inputs.
///
/// Uses threshold=0 (strict beta mode): any single untraceable non-trivial
/// line or novel definition name causes failure.
pub fn validate_no_additions(input: &TraceabilityInput) -> TraceabilityReport {
    validate_no_additions_with_threshold(input, 0)
}

/// Validate with configurable threshold for future tuning.
pub fn validate_no_additions_with_threshold(
    input: &TraceabilityInput,
    threshold: usize,
) -> TraceabilityReport {
    let mut novel_units = Vec::new();
    let mut untraced_lines = Vec::new();

    // ── Tier 1: Named unit check ──────────────────────────────────────────
    let analyzer = analyzers::analyzer_for_path(&input.file_path);
    let merged_defs = analyzer.extract_definitions(&input.merged);
    let base_defs = analyzer.extract_definitions(&input.base);
    let left_defs = analyzer.extract_definitions(&input.left);
    let right_defs = analyzer.extract_definitions(&input.right);

    let input_names: HashSet<&str> = base_defs
        .iter()
        .chain(left_defs.iter())
        .chain(right_defs.iter())
        .map(|d| d.name.as_str())
        .collect();

    for def in &merged_defs {
        if !def.name.is_empty() && !input_names.contains(def.name.as_str()) {
            novel_units.push(NovelUnit {
                name: def.name.clone(),
                unit_kind: def.def_kind.clone(),
                span: def.span,
            });
        }
    }

    // ── Tier 2: Line-level traceability ───────────────────────────────────
    let normalized_input_lines =
        collect_normalized_input_lines(&input.base, &input.left, &input.right, &input.file_path);

    let mut lines_checked: usize = 0;
    let mut lines_passed: usize = 0;

    for (idx, line) in input.merged.lines().enumerate() {
        if is_trivial(line, &input.file_path) {
            continue;
        }
        lines_checked += 1;

        let normalized = normalize_line(line);

        // Fast path: exact normalized match in any input
        if normalized_input_lines.contains(&normalized) {
            lines_passed += 1;
            continue;
        }

        // Slow path: find best similarity across all input lines
        let (best_score, best_source, best_content) = find_best_match(
            line,
            &input.base,
            &input.left,
            &input.right,
            &input.file_path,
        );

        if best_score >= 0.90 {
            lines_passed += 1;
        } else {
            untraced_lines.push(UntracedLine {
                line_number: idx + 1,
                content: line.to_string(),
                best_similarity: best_score,
                best_match_source: best_source,
                best_match_content: best_content,
            });
        }
    }

    // Build verdict
    let tier1_fail = !novel_units.is_empty();
    let tier2_fail = untraced_lines.len() > threshold;
    let verdict = if tier1_fail || tier2_fail {
        TraceabilityVerdict::Fail
    } else {
        TraceabilityVerdict::Pass
    };

    let summary = build_summary(&novel_units, &untraced_lines, lines_checked, threshold);

    TraceabilityReport {
        file_path: input.file_path.clone(),
        verdict,
        novel_units,
        untraced_lines,
        lines_checked,
        lines_passed,
        threshold,
        summary,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Collect all normalized non-trivial lines from the three inputs into a set
/// for fast exact-match lookups.
fn collect_normalized_input_lines(
    base: &str,
    left: &str,
    right: &str,
    file_path: &str,
) -> HashSet<String> {
    let mut set = HashSet::new();
    for source in [base, left, right] {
        for line in source.lines() {
            if !is_trivial(line, file_path) {
                set.insert(normalize_line(line));
            }
        }
    }
    set
}

/// Find the best-matching line across all three inputs.
/// Returns (best_similarity, source_name, matching_line_content).
fn find_best_match(
    target: &str,
    base: &str,
    left: &str,
    right: &str,
    file_path: &str,
) -> (f64, Option<String>, Option<String>) {
    let mut best_score = 0.0_f64;
    let mut best_source: Option<String> = None;
    let mut best_content: Option<String> = None;

    for (source_name, source_content) in [("base", base), ("left", left), ("right", right)] {
        for input_line in source_content.lines() {
            if is_trivial(input_line, file_path) {
                continue;
            }
            let sim = line_similarity(target, input_line);
            if sim > best_score {
                best_score = sim;
                best_source = Some(source_name.to_string());
                best_content = Some(input_line.to_string());
                // Short-circuit on perfect match
                if sim >= 1.0 {
                    return (best_score, best_source, best_content);
                }
            }
        }
    }

    (best_score, best_source, best_content)
}

/// Build a human-readable, actionable summary from the validation results.
fn build_summary(
    novel_units: &[NovelUnit],
    untraced_lines: &[UntracedLine],
    lines_checked: usize,
    threshold: usize,
) -> String {
    let mut parts = Vec::new();

    if !novel_units.is_empty() {
        let names: Vec<&str> = novel_units.iter().map(|u| u.name.as_str()).collect();
        parts.push(format!(
            "TIER 1 FAIL: {} novel definition(s) not in any input: {}",
            novel_units.len(),
            names.join(", ")
        ));
    }

    if untraced_lines.len() > threshold {
        parts.push(format!(
            "TIER 2 FAIL: {} untraced line(s) out of {} checked (threshold: {})",
            untraced_lines.len(),
            lines_checked,
            threshold
        ));
        for line in untraced_lines.iter().take(5) {
            let display: String = if line.content.chars().count() > 60 {
                line.content.chars().take(60).collect()
            } else {
                line.content.clone()
            };
            parts.push(format!(
                "  Line {}: {:?} (best match: {:.0}%)",
                line.line_number,
                display,
                line.best_similarity * 100.0
            ));
        }
        if untraced_lines.len() > 5 {
            parts.push(format!("  ... and {} more", untraced_lines.len() - 5));
        }
    }

    if parts.is_empty() {
        format!(
            "PASS: all {} content lines traceable to inputs",
            lines_checked
        )
    } else {
        parts.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Type tests --

    #[test]
    fn test_traceability_verdict_serialization() {
        let pass = serde_json::to_string(&TraceabilityVerdict::Pass).unwrap();
        let fail = serde_json::to_string(&TraceabilityVerdict::Fail).unwrap();
        assert_eq!(pass, "\"Pass\"");
        assert_eq!(fail, "\"Fail\"");
        assert_eq!(
            serde_json::from_str::<TraceabilityVerdict>(&pass).unwrap(),
            TraceabilityVerdict::Pass
        );
        assert_eq!(
            serde_json::from_str::<TraceabilityVerdict>(&fail).unwrap(),
            TraceabilityVerdict::Fail
        );
    }

    #[test]
    fn test_novel_unit_serialization_roundtrip() {
        let unit = NovelUnit {
            name: "PaymentProcessor".to_string(),
            unit_kind: "class".to_string(),
            span: (10, 25),
        };
        let json = serde_json::to_string(&unit).unwrap();
        let parsed: NovelUnit = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "PaymentProcessor");
        assert_eq!(parsed.unit_kind, "class");
        assert_eq!(parsed.span, (10, 25));
    }

    #[test]
    fn test_untraced_line_serialization_roundtrip() {
        let line = UntracedLine {
            line_number: 42,
            content: "    let x = novel_computation();".to_string(),
            best_similarity: 0.65,
            best_match_source: Some("left".to_string()),
            best_match_content: Some("    let x = old_computation();".to_string()),
        };
        let json = serde_json::to_string(&line).unwrap();
        let parsed: UntracedLine = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.line_number, 42);
        assert!((parsed.best_similarity - 0.65).abs() < 1e-10);
        assert_eq!(parsed.best_match_source, Some("left".to_string()));
    }

    #[test]
    fn test_traceability_report_serialization_roundtrip() {
        let report = TraceabilityReport {
            file_path: "src/main.rs".to_string(),
            verdict: TraceabilityVerdict::Pass,
            novel_units: vec![],
            untraced_lines: vec![],
            lines_checked: 50,
            lines_passed: 50,
            threshold: 0,
            summary: "PASS: all 50 content lines traceable to inputs".to_string(),
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: TraceabilityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.verdict, TraceabilityVerdict::Pass);
        assert_eq!(parsed.lines_checked, 50);
    }

    // -- Normalization tests --

    #[test]
    fn test_normalize_line_strips_whitespace() {
        assert_eq!(normalize_line("  hello world  "), "hello world");
    }

    #[test]
    fn test_normalize_line_collapses_internal_whitespace() {
        assert_eq!(normalize_line("  let   x  =   5;  "), "let x = 5;");
    }

    #[test]
    fn test_normalize_line_tabs_and_mixed() {
        assert_eq!(normalize_line("\t\tlet  x\t= 5;"), "let x = 5;");
    }

    #[test]
    fn test_normalize_line_empty() {
        assert_eq!(normalize_line(""), "");
        assert_eq!(normalize_line("   "), "");
    }

    // -- Levenshtein tests --

    #[test]
    fn test_levenshtein_identical() {
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
    }

    #[test]
    fn test_levenshtein_single_substitution() {
        assert_eq!(levenshtein_distance("cat", "hat"), 1);
    }

    #[test]
    fn test_levenshtein_classic_case() {
        assert_eq!(levenshtein_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn test_levenshtein_empty() {
        assert_eq!(levenshtein_distance("", "hello"), 5);
        assert_eq!(levenshtein_distance("hello", ""), 5);
        assert_eq!(levenshtein_distance("", ""), 0);
    }

    // -- Similarity tests --

    #[test]
    fn test_similarity_exact_match() {
        assert!((line_similarity("let x = 5;", "let x = 5;") - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_similarity_normalized_match() {
        assert!(
            (line_similarity("  let  x  = 5;  ", "let x = 5;") - 1.0).abs() < 1e-10,
            "Whitespace-only differences should normalize to exact match"
        );
    }

    #[test]
    fn test_similarity_empty() {
        assert!((line_similarity("", "hello") - 0.0).abs() < 1e-10);
        assert!((line_similarity("hello", "") - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_similarity_minor_edit_above_threshold() {
        // "let x = compute();" vs "let x = compute_v2();" -- minor edit
        let sim = line_similarity("let x = compute();", "let x = compute_v2();");
        assert!(
            sim > 0.85,
            "Minor edit should produce high similarity, got {}",
            sim
        );
    }

    #[test]
    fn test_similarity_completely_different() {
        let sim = line_similarity("fn process_data() {", "import os");
        assert!(
            sim < 0.5,
            "Completely different lines should have low similarity, got {}",
            sim
        );
    }

    // -- is_trivial tests --

    #[test]
    fn test_trivial_blank_lines() {
        assert!(is_trivial("", "test.rs"));
        assert!(is_trivial("   ", "test.rs"));
        assert!(is_trivial("\t\t", "test.rs"));
    }

    #[test]
    fn test_trivial_braces() {
        assert!(is_trivial("}", "test.rs"));
        assert!(is_trivial("  }", "test.rs"));
        assert!(is_trivial("{", "test.rs"));
        assert!(is_trivial("  };", "test.rs"));
    }

    #[test]
    fn test_trivial_keywords() {
        assert!(is_trivial("pass", "test.py"));
        assert!(is_trivial("    pass", "test.py"));
        assert!(is_trivial("return", "test.rs"));
        assert!(is_trivial("    return;", "test.rs"));
        assert!(is_trivial("else:", "test.py"));
        assert!(is_trivial("    break;", "test.rs"));
    }

    #[test]
    fn test_not_trivial_real_code() {
        assert!(!is_trivial("let x = 5;", "test.rs"));
        assert!(!is_trivial("fn main() {", "test.rs"));
        assert!(!is_trivial("import os", "test.py"));
        assert!(!is_trivial("return x + y;", "test.rs"));
        assert!(!is_trivial("class Foo:", "test.py"));
    }

    #[test]
    fn test_trivial_single_char() {
        assert!(is_trivial("(", "test.rs"));
        assert!(is_trivial(")", "test.rs"));
        assert!(is_trivial("[", "test.rs"));
    }

    #[test]
    fn test_trivial_ellipsis() {
        assert!(is_trivial("...", "test.py"));
        assert!(is_trivial("    ...", "test.py"));
    }

    // -- Language-aware is_trivial tests (C.1.1b) --

    #[test]
    fn test_trivial_python_comments() {
        assert!(is_trivial("# this is a comment", "test.py"));
        assert!(is_trivial("    # indented comment", "test.py"));
        assert!(is_trivial("# TODO: fix this later", "helpers.pyi"));
    }

    #[test]
    fn test_trivial_python_keywords() {
        assert!(is_trivial("try:", "test.py"));
        assert!(is_trivial("    try:", "test.py"));
        assert!(is_trivial("except:", "test.py"));
        assert!(is_trivial("    finally:", "test.py"));
    }

    #[test]
    fn test_not_trivial_python_logic() {
        assert!(!is_trivial("return x + y", "test.py"));
        assert!(!is_trivial("except ValueError as e:", "test.py"));
        assert!(!is_trivial("elif condition:", "test.py"));
        assert!(!is_trivial("raise ValueError('bad')", "test.py"));
        assert!(!is_trivial("x = 5  # inline comment", "test.py"));
    }

    #[test]
    fn test_trivial_rust_comments() {
        assert!(is_trivial("// line comment", "main.rs"));
        assert!(is_trivial("    // indented comment", "main.rs"));
        assert!(is_trivial("/// doc comment", "lib.rs"));
        assert!(is_trivial("//! module doc comment", "mod.rs"));
    }

    #[test]
    fn test_trivial_rust_keywords() {
        assert!(is_trivial("unsafe {", "main.rs"));
        assert!(is_trivial("    loop {", "main.rs"));
        assert!(is_trivial("} else {", "main.rs"));
        assert!(is_trivial("} else", "main.rs"));
        assert!(is_trivial("Ok(())", "main.rs"));
    }

    #[test]
    fn test_not_trivial_rust_logic() {
        assert!(!is_trivial("return result;", "main.rs"));
        assert!(!is_trivial("let x = compute();", "main.rs"));
        assert!(!is_trivial("fn main() {", "main.rs"));
        assert!(!is_trivial("Ok(value)", "main.rs"));
        assert!(!is_trivial("unsafe { ptr::read(addr) }", "main.rs"));
    }

    #[test]
    fn test_trivial_go_comments() {
        assert!(is_trivial("// Go comment", "main.go"));
        assert!(is_trivial("    // indented", "handler.go"));
    }

    #[test]
    fn test_trivial_go_keywords() {
        assert!(is_trivial("default:", "main.go"));
        assert!(is_trivial("    default:", "main.go"));
    }

    #[test]
    fn test_not_trivial_go_logic() {
        assert!(!is_trivial("return err", "main.go"));
        assert!(!is_trivial("case \"value\":", "main.go"));
        assert!(!is_trivial("func main() {", "main.go"));
    }

    #[test]
    fn test_trivial_typescript_comments() {
        assert!(is_trivial("// TS comment", "app.ts"));
        assert!(is_trivial("// JS comment", "app.js"));
        assert!(is_trivial("    // indented", "component.tsx"));
        assert!(is_trivial("// comment", "util.mjs"));
    }

    #[test]
    fn test_trivial_typescript_keywords() {
        assert!(is_trivial("default:", "app.ts"));
        assert!(is_trivial("    default:", "app.tsx"));
    }

    #[test]
    fn test_not_trivial_typescript_logic() {
        assert!(!is_trivial("return data;", "app.ts"));
        assert!(!is_trivial("case 'value':", "app.ts"));
        assert!(!is_trivial("export default App;", "app.tsx"));
        assert!(!is_trivial("const x = 5; // inline", "app.js"));
    }

    #[test]
    fn test_trivial_generic_fallback_no_comments() {
        // Generic/unknown files should NOT treat # or // as comments
        assert!(!is_trivial("# not a comment", "data.yaml"));
        assert!(!is_trivial("// not a comment", "unknown.xyz"));
        // But base structural patterns still apply
        assert!(is_trivial("}", "unknown.xyz"));
        assert!(is_trivial("", "unknown.xyz"));
        assert!(is_trivial("return;", "unknown.xyz"));
    }

    #[test]
    fn test_trivial_else_brace_patterns() {
        // } else { is trivial across brace languages
        assert!(is_trivial("} else {", "main.rs"));
        assert!(is_trivial("} else {", "main.go"));
        assert!(is_trivial("} else {", "app.ts"));
        assert!(is_trivial("else {", "main.rs"));
        assert!(is_trivial("else:", "test.py"));
    }

    #[test]
    fn test_trivial_block_comments_rust() {
        assert!(is_trivial("/* block comment */", "main.rs"));
        assert!(is_trivial("    /* indented */", "lib.rs"));
        assert!(is_trivial("/** doc block */", "lib.rs"));
        assert!(is_trivial("*/", "main.rs"));
        assert!(is_trivial("    */", "main.rs"));
        assert!(is_trivial("* continuation line", "main.rs"));
        assert!(is_trivial("    * indented continuation", "main.rs"));
        assert!(is_trivial("*", "main.rs"));
    }

    #[test]
    fn test_trivial_block_comments_go() {
        assert!(is_trivial("/* go block comment */", "main.go"));
        assert!(is_trivial("*/", "handler.go"));
        assert!(is_trivial("* middle of block comment", "main.go"));
    }

    #[test]
    fn test_trivial_block_comments_js_ts() {
        assert!(is_trivial("/* js block comment */", "app.js"));
        assert!(is_trivial("/** jsdoc comment */", "app.ts"));
        assert!(is_trivial("*/", "component.tsx"));
        assert!(is_trivial("* @param name", "app.ts"));
    }

    #[test]
    fn test_block_comments_not_trivial_for_python() {
        // Python doesn't have /* */ comments — these shouldn't be trivial
        assert!(!is_trivial("/* not a comment in python */", "test.py"));
        assert!(!is_trivial("*/", "test.py"));
    }

    #[test]
    fn test_block_comments_not_trivial_for_unknown() {
        // Unknown file types shouldn't treat block comments as trivial
        assert!(!is_trivial("/* comment */", "data.yaml"));
        assert!(!is_trivial("*/", "unknown.xyz"));
    }

    // -- validate_no_additions tests --

    fn make_input(
        file_path: &str,
        base: &str,
        left: &str,
        right: &str,
        merged: &str,
    ) -> TraceabilityInput {
        TraceabilityInput {
            file_path: file_path.to_string(),
            base: base.to_string(),
            left: left.to_string(),
            right: right.to_string(),
            merged: merged.to_string(),
        }
    }

    #[test]
    fn test_clean_merge_passes() {
        let base = "fn hello() {\n    println!(\"hello\");\n}\n";
        let left = "fn hello() {\n    println!(\"hello world\");\n}\n";
        let right = "fn hello() {\n    println!(\"hello\");\n}\n\nfn goodbye() {\n    println!(\"bye\");\n}\n";
        // Merged is just a combination of both sides' content
        let merged = "fn hello() {\n    println!(\"hello world\");\n}\n\nfn goodbye() {\n    println!(\"bye\");\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
        assert!(report.novel_units.is_empty());
        assert!(report.untraced_lines.is_empty());
    }

    #[test]
    fn test_novel_function_tier1_fail() {
        let base = "fn hello() {\n    println!(\"hello\");\n}\n";
        let left = base;
        let right = base;
        let merged =
            "fn hello() {\n    println!(\"hello\");\n}\n\nfn malicious() {\n    steal_data();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert_eq!(report.novel_units.len(), 1);
        assert_eq!(report.novel_units[0].name, "malicious");
        assert!(report.summary.contains("TIER 1 FAIL"));
    }

    #[test]
    fn test_novel_class_tier1_fail() {
        let base = "class User:\n    def __init__(self):\n        pass\n";
        let left = base;
        let right = base;
        let merged = "class User:\n    def __init__(self):\n        pass\n\nclass PaymentProcessor:\n    def charge(self):\n        pass\n";

        let input = make_input("test.py", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(report
            .novel_units
            .iter()
            .any(|u| u.name == "PaymentProcessor"));
    }

    #[test]
    fn test_whitespace_difference_passes() {
        let base = "fn hello() {\n    let x = 5;\n}\n";
        let left = base;
        let right = base;
        // Merged has different indentation but same content
        let merged = "fn hello() {\n        let x = 5;\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
    }

    #[test]
    fn test_novel_line_tier2_fail() {
        let base = "fn process() {\n    let data = load();\n    save(data);\n}\n";
        let left = base;
        let right = base;
        let merged = "fn process() {\n    let data = load();\n    let secret = steal_credentials_from_env();\n    save(data);\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(!report.untraced_lines.is_empty());
        assert!(report.untraced_lines[0]
            .content
            .contains("steal_credentials"));
    }

    #[test]
    fn test_similar_line_above_threshold_passes() {
        let base = "fn process() {\n    let result = compute_value(x);\n}\n";
        let left = base;
        let right = base;
        // Very minor edit: "x" -> "y" in a long line — should be above 0.90
        let merged = "fn process() {\n    let result = compute_value(y);\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(
            report.verdict,
            TraceabilityVerdict::Pass,
            "Minor single-char edit in long line should pass 0.90 threshold. Summary: {}",
            report.summary
        );
    }

    #[test]
    fn test_threshold_zero_strict() {
        let base = "fn a() {\n    line_one();\n}\n";
        let left = base;
        let right = base;
        let merged = "fn a() {\n    line_one();\n    completely_novel_injected_call();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert_eq!(report.untraced_lines.len(), 1);
    }

    #[test]
    fn test_threshold_one_lenient() {
        let base = "fn a() {\n    line_one();\n}\n";
        let left = base;
        let right = base;
        let merged = "fn a() {\n    line_one();\n    completely_novel_injected_call();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions_with_threshold(&input, 1);

        // 1 untraced line <= threshold of 1, but novel function would still fail Tier 1
        // Actually, "completely_novel_injected_call()" is a statement, not a definition,
        // so Tier 1 won't catch it. Tier 2 flags 1 line, threshold=1 means 1 <= 1 = pass.
        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
    }

    #[test]
    fn test_both_tier1_and_tier2_failures() {
        let base = "fn hello() {\n    println!(\"hi\");\n}\n";
        let left = base;
        let right = base;
        let merged = "fn hello() {\n    println!(\"hi\");\n}\n\nfn evil() {\n    let x = completely_novel_code_line_here();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(
            !report.novel_units.is_empty(),
            "Should have Tier 1 failures"
        );
        assert!(
            !report.untraced_lines.is_empty(),
            "Should have Tier 2 failures"
        );
        assert!(report.summary.contains("TIER 1 FAIL"));
        assert!(report.summary.contains("TIER 2 FAIL"));
    }

    #[test]
    fn test_trivial_lines_excluded() {
        let base = "fn a() {\n    println!(\"base\");\n}\n";
        let left = base;
        let right = base;
        // Only novel content is trivial lines — should pass
        let merged = "fn a() {\n    println!(\"base\");\n}\n\n\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
    }

    #[test]
    fn test_empty_merged_passes() {
        let base = "fn hello() {\n    println!(\"hi\");\n}\n";
        let input = make_input("test.rs", base, base, base, "");
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
        assert_eq!(report.lines_checked, 0);
    }

    #[test]
    fn test_summary_is_actionable_on_fail() {
        let base = "fn a() {\n    real_code();\n}\n";
        let left = base;
        let right = base;
        let merged = "fn a() {\n    real_code();\n    injected_malware();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert!(report.summary.contains("TIER 2 FAIL"));
        assert!(report.summary.contains("Line "));
        assert!(report.summary.contains("best match:"));
    }

    #[test]
    fn test_summary_is_pass_message_on_success() {
        let base = "fn a() {\n    real_code();\n}\n";
        let input = make_input("test.rs", base, base, base, base);
        let report = validate_no_additions(&input);

        assert!(report.summary.contains("PASS:"));
        assert!(report.summary.contains("traceable to inputs"));
    }

    #[test]
    fn test_reordered_definitions_pass() {
        let base =
            "fn alpha() {\n    println!(\"a\");\n}\n\nfn beta() {\n    println!(\"b\");\n}\n";
        let left = base;
        let right = base;
        // Same definitions, different order
        let merged =
            "fn beta() {\n    println!(\"b\");\n}\n\nfn alpha() {\n    println!(\"a\");\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(
            report.verdict,
            TraceabilityVerdict::Pass,
            "Reordered definitions should pass: {}",
            report.summary
        );
    }

    #[test]
    fn test_composed_function_from_both_sides() {
        let base = "fn process() {\n    step_one();\n}\n";
        let left = "fn process() {\n    step_one();\n    step_two();\n}\n";
        let right = "fn process() {\n    step_one();\n    step_three();\n}\n";
        // Merged combines both sides' additions
        let merged = "fn process() {\n    step_one();\n    step_two();\n    step_three();\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(
            report.verdict,
            TraceabilityVerdict::Pass,
            "Composed function from both sides should pass: {}",
            report.summary
        );
    }

    #[test]
    fn test_report_full_json_roundtrip() {
        let base = "fn hello() {\n    println!(\"hi\");\n}\n";
        let merged = "fn hello() {\n    println!(\"hi\");\n}\n\nfn evil() {\n    steal();\n}\n";
        let input = make_input("test.rs", base, base, base, merged);
        let report = validate_no_additions(&input);

        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: TraceabilityReport = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.verdict, report.verdict);
        assert_eq!(parsed.novel_units.len(), report.novel_units.len());
        assert_eq!(parsed.untraced_lines.len(), report.untraced_lines.len());
        assert_eq!(parsed.lines_checked, report.lines_checked);
    }

    // -- C.1.3 scenario 5: import reordering --

    #[test]
    fn test_import_reordering_passes() {
        let base = "import os\nimport sys\nimport json\n";
        let left = "import sys\nimport os\nimport json\n"; // reordered
        let right = base;
        let merged = "import sys\nimport os\nimport json\n"; // uses left's order

        let input = make_input("test.py", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(
            report.verdict,
            TraceabilityVerdict::Pass,
            "Reordered imports should pass — each line traces back: {}",
            report.summary
        );
    }

    // -- C.1.3 scenario 10: merged imports from both sides --

    #[test]
    fn test_merged_imports_from_both_sides() {
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";
        let merged = "import os\nimport json\nimport sys\n";

        let input = make_input("test.py", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(
            report.verdict,
            TraceabilityVerdict::Pass,
            "Merged imports from both sides should pass — each line from left or right: {}",
            report.summary
        );
    }

    // -- P2 #5: Generic/unknown file types --

    #[test]
    fn test_generic_file_type_tier2_still_works() {
        // For .yaml, .txt, etc., Tier 1 (named definitions) is inert because
        // GenericAnalyzer returns no definitions. Tier 2 (line-level) should
        // still catch novel content.
        let base = "key: value\nother: data\n";
        let left = "key: value\nother: data\nnew_left: yes\n";
        let right = "key: value\nother: data\nnew_right: true\n";
        let merged =
            "key: value\nother: data\nnew_left: yes\nnew_right: true\ninjected: malicious\n";

        let input = make_input("config.yaml", base, left, right, merged);
        let report = validate_no_additions(&input);

        // "injected: malicious" is not in any input
        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(
            report.novel_units.is_empty(),
            "Tier 1 should be inert for .yaml"
        );
        assert!(
            !report.untraced_lines.is_empty(),
            "Tier 2 should catch the novel line"
        );
    }

    #[test]
    fn test_generic_file_type_clean_merge_passes() {
        // Clean merge of generic file type — all content traceable
        let base = "key: value\n";
        let left = "key: value\nleft_key: data\n";
        let right = "key: value\nright_key: data\n";
        let merged = "key: value\nleft_key: data\nright_key: data\n";

        let input = make_input("data.txt", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
    }

    // -- P2 #6: Summary truncation with >5 untraced lines --

    #[test]
    fn test_summary_truncation_more_than_5_untraced() {
        let base = "fn a() {}\n";
        let left = base;
        let right = base;
        // 7 novel lines, well over the 5 displayed in summary
        let merged = "fn a() {}\nnovel_1()\nnovel_2()\nnovel_3()\nnovel_4()\nnovel_5()\nnovel_6()\nnovel_7()\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(report.untraced_lines.len() >= 7);
        // Summary should show first 5 and "... and N more"
        assert!(
            report.summary.contains("... and"),
            "Summary should truncate with '... and N more', got: {}",
            report.summary
        );
        assert!(
            report.summary.contains("more"),
            "Should mention remaining count"
        );
    }

    // -- P2 #7: Non-ASCII content (exposes P0 bug if not fixed) --

    #[test]
    fn test_non_ascii_content_does_not_panic() {
        // CJK characters, emoji, multi-byte UTF-8
        let base = "fn hello() {\n    // 你好世界\n}\n";
        let left = "fn hello() {\n    // 你好世界\n    println!(\"こんにちは\");\n}\n";
        let right = "fn hello() {\n    // 你好世界\n}\n";
        // Novel line with emoji and CJK
        let merged = "fn hello() {\n    // 你好世界\n    println!(\"こんにちは\");\n    let 変数 = \"🦀 Rust は素晴らしい 🎉🎊🎋🎌🎍🎎🎏 and more text to exceed sixty characters easily\";\n}\n";

        let input = make_input("test.rs", base, left, right, merged);
        let report = validate_no_additions(&input);

        // Should not panic — the P0 fix ensures char-safe truncation
        assert_eq!(report.verdict, TraceabilityVerdict::Fail);
        assert!(!report.untraced_lines.is_empty());
        // Summary should be valid UTF-8 (no panics)
        assert!(!report.summary.is_empty());
    }

    #[test]
    fn test_non_ascii_traceable_content_passes() {
        // All content is traceable despite being non-ASCII
        let base = "# 設定ファイル\nname: 太郎\n";
        let left = "# 設定ファイル\nname: 太郎\nage: 25\n";
        let right = "# 設定ファイル\nname: 太郎\ncity: 東京\n";
        let merged = "# 設定ファイル\nname: 太郎\nage: 25\ncity: 東京\n";

        let input = make_input("config.yaml", base, left, right, merged);
        let report = validate_no_additions(&input);

        assert_eq!(report.verdict, TraceabilityVerdict::Pass);
    }
}
