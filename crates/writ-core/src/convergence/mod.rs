//! Convergence — writ's answer to merging.
//!
//! When two specs modify overlapping files, convergence detects conflicts
//! and produces structured merge results. Conflicts are JSON-serializable
//! data — not text markers — so orchestrator agents can resolve them
//! programmatically.
//!
//! ## Architecture (v2.0)
//!
//! The convergence engine is structured as a six-phase pipeline:
//!
//! - **Phase 1** (Structural Diff): Language-aware diffing via [`analyzers`]
//! - **Phase 2** (Classification): Categorize conflict regions
//! - **Phase 3** (Deterministic Resolution): Pattern-based via [`patterns`]
//! - **Phase 4** (Spec-Aware Resolution): Use spec context (Sprint 2)
//! - **Phase 5** (LLM-Assisted Resolution): AI resolution (Sprint 2)
//! - **Phase 6** (Verification): Validate merged result
//!
//! Submodules:
//! - [`types`] — Core types: StructuralUnit, ClassifiedConflict, ResolutionProposal, etc.
//! - [`analyzers`] — LanguageAnalyzer trait + implementations (Generic, Python, etc.)
//! - [`patterns`] — Pattern trait + PatternRegistry for deterministic resolution

pub mod analyzers;
pub mod diff3;
pub mod patterns;
pub mod phase1;
pub mod phase2;
pub mod phase4;
pub mod pipeline;
pub mod types;

use diff3::rebuild_with_resolutions;
pub use diff3::three_way_merge;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Result of analyzing convergence between two specs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceReport {
    /// Left spec ID.
    pub left_spec: String,
    /// Right spec ID.
    pub right_spec: String,
    /// Base seal ID (None if comparing from empty state).
    pub base_seal_id: Option<String>,
    /// Latest seal ID for the left spec.
    pub left_seal_id: String,
    /// Latest seal ID for the right spec.
    pub right_seal_id: String,
    /// Files that merged cleanly (both sides changed, no overlaps).
    pub auto_merged: Vec<MergedFile>,
    /// Files with overlapping changes that need resolution.
    pub conflicts: Vec<FileConflict>,
    /// Paths only the left spec modified.
    pub left_only: Vec<String>,
    /// Paths only the right spec modified.
    pub right_only: Vec<String>,
    /// True if there are no conflicts.
    pub is_clean: bool,
}

/// A file that was auto-merged successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedFile {
    /// Relative path.
    pub path: String,
    /// The merged content.
    pub content: String,
}

/// A file where both specs made overlapping changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileConflict {
    /// Relative path.
    pub path: String,
    /// Content at the base (None if the file didn't exist).
    pub base_content: Option<String>,
    /// Content in the left spec's latest seal.
    pub left_content: String,
    /// Content in the right spec's latest seal.
    pub right_content: String,
    /// The specific conflict regions within the file.
    pub regions: Vec<ConflictRegion>,
}

/// A specific region within a file where both sides diverge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictRegion {
    /// 1-based line number in the base file where this region starts.
    pub base_start: usize,
    /// The original base lines.
    pub base_lines: Vec<String>,
    /// What the left spec changed it to.
    pub left_lines: Vec<String>,
    /// What the right spec changed it to.
    pub right_lines: Vec<String>,
}

/// A resolution provided for a conflicted file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResolution {
    /// Path of the file being resolved.
    pub path: String,
    /// The resolved content to use.
    pub content: String,
}

/// Result of a three-way merge for a single file.
#[derive(Debug, Clone)]
pub enum FileMergeResult {
    /// The merge was clean — contains the merged content.
    Clean(String),
    /// The merge has conflicts that need resolution.
    Conflict(Vec<ConflictRegion>),
}

/// Strategy for resolving `BothModified` conflicts that Layers 2-3
/// (additive composition, import accumulation, etc.) cannot auto-resolve.
///
/// Layers 1-3 always run regardless of strategy. The strategy only governs
/// the fallback for genuinely irreconcilable conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConvergeStrategy {
    /// Leave irreconcilable conflicts unresolved for manual review.
    Manual,
    /// **DEPRECATED** — preserved for backward compatibility during migration.
    /// Prefers the most recently sealed version. This silently drops work and
    /// is being replaced by `Escalate`. Will be removed in a future version.
    #[deprecated(
        since = "0.2.0",
        note = "use Escalate instead — MostRecent silently drops work"
    )]
    MostRecent,
    /// Return structured conflict data for orchestrator agent resolution.
    Orchestrator,
    /// Escalate unresolvable conflicts with full context (Phase 4/5/6 path).
    /// This is the recommended strategy — it never silently loses work.
    /// Unresolved conflicts are recorded as `EscalationRecord`s in the report.
    Escalate,
}

impl Default for ConvergeStrategy {
    fn default() -> Self {
        ConvergeStrategy::Escalate
    }
}

// ---------------------------------------------------------------------------
// Region-level conflict classification and resolution (Layers 2-3)
// ---------------------------------------------------------------------------

/// How a conflict region was classified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictClass {
    /// Both sides inserted different content at the same point (base is empty).
    BothInserted,
    /// One side deleted base content, the other modified it.
    DeleteVsModify { modifier: MergeSide },
    /// Both sides made different changes to the same base content.
    BothModified,
}

/// Which side of the merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MergeSide {
    Left,
    Right,
}

/// A resolved conflict region with metadata about how it was resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionResolution {
    /// The resolved content lines for this region.
    pub lines: Vec<String>,
    /// How the classification + resolution worked.
    pub class: ConflictClass,
    /// Resolution method description.
    pub method: String,
    /// Confidence level (0.0 = uncertain fallback, 1.0 = structurally certain).
    pub confidence: f64,
}

/// Lightweight escalation info for the current (v1) pipeline.
/// This bridges to the full `types::EscalationRecord` used by the v2 pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineEscalation {
    pub file_path: String,
    pub reason: String,
    pub conflict_class: String,
    pub left_spec: String,
    pub right_spec: String,
    pub recommended_action: String,
}

/// Result of resolving all conflict regions for a single file.
#[derive(Debug, Clone)]
pub struct ResolvedFile {
    /// The fully merged file content.
    pub content: String,
    /// Per-region resolution details.
    pub resolutions: Vec<RegionResolution>,
    /// True if all regions were resolved (no remaining conflicts).
    pub fully_resolved: bool,
    /// Regions that could not be auto-resolved (only present when
    /// `fully_resolved` is false).
    pub unresolved_regions: Vec<ConflictRegion>,
    /// Escalation records for conflicts that could not be auto-resolved.
    pub escalation_records: Vec<PipelineEscalation>,
}

/// Classify a single conflict region.
///
/// `total_base_lines` is the number of lines in the original base file.
/// This disambiguates "both sides inserted new content" from "both sides
/// replaced existing content" — the merge algorithm may decompose the
/// latter as Delete + trailing Insert, producing empty `base_lines` in the
/// `ConflictRegion` even though base content was consumed.
pub fn classify_region(region: &ConflictRegion, total_base_lines: usize) -> ConflictClass {
    let base_empty = region.base_lines.is_empty();
    let left_empty = region.left_lines.is_empty();
    let right_empty = region.right_lines.is_empty();

    if base_empty && left_empty && right_empty {
        return ConflictClass::BothModified;
    }

    if !base_empty {
        if left_empty && !right_empty {
            return ConflictClass::DeleteVsModify {
                modifier: MergeSide::Right,
            };
        }
        if right_empty && !left_empty {
            return ConflictClass::DeleteVsModify {
                modifier: MergeSide::Left,
            };
        }
        return ConflictClass::BothModified;
    }

    // base_lines is empty. Distinguish pure insertion (new content at an
    // insertion point) from replacement (both sides deleted base content
    // and inserted different content at the same spot).
    //
    // Heuristic: if the base file had content and this conflict region is
    // at the end (base_start > total_base_lines), both sides replaced
    // the file rather than inserting into it → BothModified.
    if total_base_lines > 0 && region.base_start > total_base_lines {
        return ConflictClass::BothModified;
    }

    ConflictClass::BothInserted
}

/// Check if one side's lines form an ordered subsequence of the other's.
/// Returns the superset side if detected. Uses ordered matching to respect
/// code line ordering and handle duplicates correctly.
fn detect_superset(left: &[String], right: &[String]) -> Option<MergeSide> {
    fn is_subsequence(sub: &[String], sup: &[String]) -> bool {
        let mut sup_iter = sup.iter();
        sub.iter().all(|s| sup_iter.any(|t| t == s))
    }

    if left.len() > right.len() && is_subsequence(right, left) {
        Some(MergeSide::Left)
    } else if right.len() > left.len() && is_subsequence(left, right) {
        Some(MergeSide::Right)
    } else {
        None
    }
}

/// Attempt additive composition: when both sides contain all base content
/// plus unique additions, compose by keeping base + left additions + right additions.
///
/// Uses ordered subsequence matching (same as superset detection) to verify
/// both sides preserved the base. Additions are lines not present in the base,
/// extracted in their original order. Left additions come first when
/// `left_first` is true (deterministic ordering by spec ID).
///
/// Returns `None` if either side dropped base content (not purely additive).
fn try_additive_composition(
    base: &[String],
    left: &[String],
    right: &[String],
    left_first: bool,
) -> Option<Vec<String>> {
    fn is_subsequence(sub: &[String], sup: &[String]) -> bool {
        let mut sup_iter = sup.iter();
        sub.iter().all(|s| sup_iter.any(|t| t.trim() == s.trim()))
    }

    // Both sides must contain all base lines (they're additive, not destructive).
    if !is_subsequence(base, left) || !is_subsequence(base, right) {
        return None;
    }

    // Extract additions: lines in each side that aren't in base.
    // Use a consumption-based approach to handle duplicate lines correctly.
    fn extract_additions(side: &[String], base: &[String]) -> Vec<String> {
        let mut base_remaining: Vec<&str> = base.iter().map(|s| s.trim()).collect();
        let mut additions = Vec::new();

        for line in side {
            let trimmed = line.trim();
            if let Some(pos) = base_remaining.iter().position(|b| *b == trimmed) {
                base_remaining.remove(pos);
            } else {
                additions.push(line.clone());
            }
        }
        additions
    }

    let left_additions = extract_additions(left, base);
    let right_additions = extract_additions(right, base);

    // Only compose if at least one side actually added something.
    if left_additions.is_empty() && right_additions.is_empty() {
        return None;
    }

    // Deduplicate: if both sides added the same line, keep it once.
    let mut right_unique: Vec<String> = Vec::new();
    let left_set: std::collections::HashSet<String> = left_additions
        .iter()
        .map(|l| l.trim().to_string())
        .collect();
    for line in &right_additions {
        if !left_set.contains(line.trim()) {
            right_unique.push(line.clone());
        }
    }

    let mut composed = base.to_vec();
    if left_first {
        composed.extend(left_additions);
        composed.extend(right_unique);
    } else {
        composed.extend(right_unique);
        composed.extend(left_additions);
    }

    Some(composed)
}

// ---------------------------------------------------------------------------
// Text utilities
// ---------------------------------------------------------------------------

/// Check if `needle` appears in `haystack` as a whole word — i.e. the
/// characters immediately before and after the match are not alphanumeric
/// or underscore.  This prevents `"User"` from matching `"UserProfile"`.
pub(crate) fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let bytes = haystack.as_bytes();
    for (idx, _) in haystack.match_indices(needle) {
        let before_ok = idx == 0 || {
            let b = bytes[idx - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after_idx = idx + needle.len();
        let after_ok = after_idx >= bytes.len() || {
            let b = bytes[after_idx];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Layer 3: Structural pattern detection
// ---------------------------------------------------------------------------

/// Returns true if a line looks like an import statement in any supported language.
fn is_import_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    // Python: import X, from X import Y
    if trimmed.starts_with("import ")
        || (trimmed.starts_with("from ") && trimmed.contains(" import "))
    {
        return true;
    }

    // Rust: use X, pub use X, mod X, pub mod X
    if trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("mod ")
        || trimmed.starts_with("pub mod ")
    {
        return true;
    }

    // JavaScript/TypeScript: import ..., require(...), export
    if trimmed.starts_with("import ")
        || trimmed.starts_with("import{")
        || trimmed.contains("require(")
        || trimmed.starts_with("export ")
        || trimmed.starts_with("export{")
    {
        return true;
    }

    // Go: import "X" or import (
    if trimmed == "import (" || trimmed.starts_with("import \"") {
        return true;
    }

    // C/C++: #include
    if trimmed.starts_with("#include ") || trimmed.starts_with("#include<") {
        return true;
    }

    false
}

/// Returns true if a line looks like a decorator or attribute annotation.
fn is_decorator_line(line: &str) -> bool {
    let trimmed = line.trim();

    // Python/Java/TypeScript: @decorator, @Annotation(args)
    if trimmed.starts_with('@') && trimmed.len() > 1 {
        let rest = &trimmed[1..];
        return rest.starts_with(|c: char| c.is_alphabetic() || c == '_');
    }

    // Rust: #[attribute], #![attribute]
    if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
        return true;
    }

    false
}

/// Returns true if all non-empty lines in the slice form import statements.
///
/// Handles multi-line imports by tracking brace/paren depth. For example,
/// a JS `import {\n  foo,\n  bar\n} from 'baz';` is recognized as a single
/// import spanning 4 lines.
fn is_import_region(lines: &[String]) -> bool {
    if lines.is_empty() {
        return false;
    }

    let mut depth: i32 = 0;
    let mut has_import = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if depth == 0 {
            // Outside any multi-line block — line must start an import.
            if !is_import_line(line) {
                return false;
            }
            has_import = true;
        }

        // Track brace/paren depth for multi-line imports.
        // Braces: JS/TS `import { ... }`, Rust `use crate::{ ... }`
        // Parens: Go `import ( ... )` — only count when line is `import (`
        //         or we're already inside a block.
        for ch in trimmed.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                '(' if trimmed == "import (" || depth > 0 => depth += 1,
                ')' if depth > 0 => depth = (depth - 1).max(0),
                _ => {}
            }
        }
    }

    has_import && depth == 0
}

/// Merge two sets of import lines with language-aware handling.
///
/// Detects the import language and dispatches to a language-specific merger
/// when available (JS/TS gets source-module grouping), falling back to
/// simple dedup + sort for other languages.
fn merge_imports(left: &[String], right: &[String]) -> Vec<String> {
    let all_lines: Vec<String> = left.iter().chain(right.iter()).cloned().collect();
    let lang = detect_import_language(&all_lines);

    match lang {
        ImportLanguage::JavaScript => merge_js_imports(left, right),
        ImportLanguage::Go => merge_go_imports(left, right),
        ImportLanguage::Python => merge_python_imports(left, right),
        _ => merge_imports_simple(left, right),
    }
}

/// Simple import merge: union, dedup by trimmed content, sort.
/// Used as the default for non-JS languages.
fn merge_imports_simple(left: &[String], right: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for line in left.iter().chain(right.iter()) {
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed) {
            result.push(line.clone());
        }
    }

    result.sort();
    result
}

// ---------------------------------------------------------------------------
// Language detection and JS/TS-specific import merging
// ---------------------------------------------------------------------------

/// Detected language for import-specific merge strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportLanguage {
    JavaScript,
    Python,
    Rust,
    Go,
    Cpp,
    Unknown,
}

/// Detect the import language from a set of import lines.
fn detect_import_language(lines: &[String]) -> ImportLanguage {
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // JS/TS: `from 'source'` or `from "source"`, require()
        if trimmed.contains(" from '")
            || trimmed.contains(" from \"")
            || trimmed.contains("require(")
        {
            return ImportLanguage::JavaScript;
        }
        // Python: `from X import Y` (distinct from JS — `from` comes first)
        if trimmed.starts_with("from ") && trimmed.contains(" import ") {
            return ImportLanguage::Python;
        }
        // Rust: use X, pub use X
        if trimmed.starts_with("use ") || trimmed.starts_with("pub use ") {
            return ImportLanguage::Rust;
        }
        // Go: import "X" or import (
        if trimmed.starts_with("import \"") || trimmed == "import (" {
            return ImportLanguage::Go;
        }
        // C/C++: #include
        if trimmed.starts_with("#include") {
            return ImportLanguage::Cpp;
        }
    }
    ImportLanguage::Unknown
}

/// A parsed JS/TS import statement.
#[derive(Debug, Clone)]
struct JsImport {
    /// Default import name (e.g., `React` from `import React from 'react'`).
    default_import: Option<String>,
    /// Named imports (e.g., `["useState", "useEffect"]`).
    named_imports: Vec<String>,
    /// Namespace import (e.g., `React` from `import * as React from 'react'`).
    namespace: Option<String>,
    /// Source module string including quotes (e.g., `'react'`).
    source: String,
    /// Whether this is `import type` or `export type`.
    is_type: bool,
    /// Whether this is a side-effect import (e.g., `import 'polyfill'`).
    is_side_effect: bool,
    /// Whether this is an export/re-export.
    is_export: bool,
}

/// Join multi-line import statements into single logical lines.
///
/// Tracks brace depth to know when a multi-line import ends.
/// Example: `import {\n  foo,\n  bar\n} from 'baz';` becomes
/// `import { foo, bar } from 'baz';`.
fn join_multiline_imports(lines: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth: i32 = 0;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() && depth == 0 {
            continue;
        }

        if depth == 0 {
            current = trimmed.to_string();
        } else {
            current.push(' ');
            current.push_str(trimmed);
        }

        for ch in trimmed.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth = (depth - 1).max(0),
                _ => {}
            }
        }

        if depth == 0 {
            result.push(current.clone());
            current.clear();
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

/// Parse a single JS/TS import statement into structured form.
///
/// Returns `None` for `require()` calls (which can't be consolidated
/// by source module) or lines that aren't valid JS imports.
fn parse_js_import(line: &str) -> Option<JsImport> {
    let trimmed = line.trim().trim_end_matches(';').trim();

    // require() calls — preserve as-is, can't consolidate
    if trimmed.contains("require(") {
        return None;
    }

    let (is_export, rest) = if trimmed.starts_with("export ") {
        (true, trimmed[7..].trim())
    } else if trimmed.starts_with("import ") {
        (false, trimmed[7..].trim())
    } else {
        return None;
    };

    let (is_type, rest) = if rest.starts_with("type ") {
        (true, rest[5..].trim())
    } else {
        (false, rest)
    };

    // Side-effect import: `import 'source'` (no `from`)
    if !is_export && !rest.contains(" from ") {
        if rest.starts_with('\'') || rest.starts_with('"') {
            return Some(JsImport {
                default_import: None,
                named_imports: vec![],
                namespace: None,
                source: rest.to_string(),
                is_type,
                is_side_effect: true,
                is_export,
            });
        }
        return None;
    }

    let from_idx = rest.rfind(" from ")?;
    let source = rest[from_idx + 6..].trim().to_string();
    let binding_part = rest[..from_idx].trim();

    let mut default_import = None;
    let mut named_imports = Vec::new();
    let mut namespace = None;

    if binding_part.starts_with("* as ") {
        namespace = Some(binding_part[5..].trim().to_string());
    } else if binding_part.starts_with('{') {
        // Named only: { X, Y }
        let inner = binding_part
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        named_imports = inner
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    } else if binding_part.contains('{') {
        // Default + named: X, { Y, Z }
        if let Some(brace_start) = binding_part.find('{') {
            let default_part = binding_part[..brace_start]
                .trim()
                .trim_end_matches(',')
                .trim();
            if !default_part.is_empty() {
                default_import = Some(default_part.to_string());
            }
            if let Some(brace_end) = binding_part.rfind('}') {
                let inner = binding_part[brace_start + 1..brace_end].trim();
                named_imports = inner
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    } else if !binding_part.is_empty() {
        // Default only: X
        default_import = Some(binding_part.to_string());
    }

    Some(JsImport {
        default_import,
        named_imports,
        namespace,
        source,
        is_type,
        is_side_effect: false,
        is_export,
    })
}

/// Render a `JsImport` back to a single-line import string with semicolon.
fn render_js_import(import: &JsImport) -> String {
    let keyword = if import.is_export { "export" } else { "import" };
    let type_prefix = if import.is_type { "type " } else { "" };

    if import.is_side_effect {
        return format!("{} {};", keyword, import.source);
    }

    if let Some(ns) = &import.namespace {
        return format!(
            "{} {}* as {} from {};",
            keyword, type_prefix, ns, import.source
        );
    }

    let mut parts = Vec::new();
    if let Some(default) = &import.default_import {
        parts.push(default.clone());
    }
    if !import.named_imports.is_empty() {
        let mut sorted = import.named_imports.clone();
        sorted.sort();
        parts.push(format!("{{ {} }}", sorted.join(", ")));
    }

    if parts.is_empty() {
        format!("{} {};", keyword, import.source)
    } else {
        format!(
            "{} {}{} from {};",
            keyword,
            type_prefix,
            parts.join(", "),
            import.source
        )
    }
}

/// Merge two sets of JS/TS import lines with source-module grouping.
///
/// Consolidates named imports from the same module (e.g., two imports from
/// `'react'` become one), preserves default imports, handles type imports
/// separately from value imports, and deduplicates.
fn merge_js_imports(left: &[String], right: &[String]) -> Vec<String> {
    let left_joined = join_multiline_imports(left);
    let right_joined = join_multiline_imports(right);

    // Key: (source, is_type, is_export) -> merged import
    let mut groups: std::collections::BTreeMap<(String, bool, bool), JsImport> =
        std::collections::BTreeMap::new();
    let mut unparsed: Vec<String> = Vec::new();

    for line in left_joined.iter().chain(right_joined.iter()) {
        if let Some(import) = parse_js_import(line) {
            let key = (import.source.clone(), import.is_type, import.is_export);

            if let Some(existing) = groups.get_mut(&key) {
                // Merge into existing entry for this source module.
                if import.default_import.is_some() && existing.default_import.is_none() {
                    existing.default_import = import.default_import;
                }
                if import.namespace.is_some() && existing.namespace.is_none() {
                    existing.namespace = import.namespace;
                }
                for named in &import.named_imports {
                    if !existing.named_imports.contains(named) {
                        existing.named_imports.push(named.clone());
                    }
                }
            } else {
                groups.insert(key, import);
            }
        } else {
            // require() or unrecognized — dedup and preserve.
            let trimmed = line.trim().to_string();
            if !unparsed.iter().any(|u: &String| u.trim() == trimmed) {
                unparsed.push(line.clone());
            }
        }
    }

    let mut result: Vec<String> = groups.values().map(render_js_import).collect();
    result.extend(unparsed);
    result
}

// ---------------------------------------------------------------------------
// Go-specific import merging
// ---------------------------------------------------------------------------

/// Extract Go import paths from lines, handling both single imports
/// and `import (...)` block syntax. Returns quoted import paths.
fn extract_go_imports(lines: &[String]) -> Vec<String> {
    let mut imports = Vec::new();
    let mut in_block = false;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "import (" {
            in_block = true;
            continue;
        }
        if trimmed == ")" && in_block {
            in_block = false;
            continue;
        }
        if in_block {
            // Inside import block — line is a quoted path (possibly with alias).
            let t = trimmed.trim_end_matches(';').trim();
            if !t.is_empty() {
                imports.push(t.to_string());
            }
        } else if trimmed.starts_with("import \"") || trimmed.starts_with("import '") {
            // Single import: `import "fmt"`
            let path = trimmed[7..].trim().trim_end_matches(';').trim();
            imports.push(path.to_string());
        }
    }

    imports
}

/// Check if a Go import path is stdlib (no dot in the package path).
/// E.g., `"fmt"` → stdlib, `"github.com/pkg/errors"` → third-party.
fn is_go_stdlib(import_path: &str) -> bool {
    let unquoted = import_path.trim().trim_matches('"').trim_matches('\'');
    // Aliased imports: `alias "path"` — extract the path part.
    let path = if unquoted.contains('"') {
        unquoted.rsplit('"').nth(1).unwrap_or(unquoted)
    } else {
        unquoted
    };
    !path.contains('.')
}

/// Merge two sets of Go import lines with goimports-style grouping.
///
/// Extracts import paths from both sides (handling `import (...)` blocks),
/// deduplicates, then outputs a single `import (...)` block with stdlib
/// imports first (sorted), a blank line separator, and third-party imports
/// second (sorted).
fn merge_go_imports(left: &[String], right: &[String]) -> Vec<String> {
    let left_imports = extract_go_imports(left);
    let right_imports = extract_go_imports(right);

    // Dedup by trimmed content.
    let mut seen = std::collections::HashSet::new();
    let mut all_imports: Vec<String> = Vec::new();
    for imp in left_imports.iter().chain(right_imports.iter()) {
        let trimmed = imp.trim().to_string();
        if seen.insert(trimmed.clone()) {
            all_imports.push(trimmed);
        }
    }

    // Split into stdlib and third-party.
    let mut stdlib: Vec<&String> = all_imports.iter().filter(|i| is_go_stdlib(i)).collect();
    let mut third_party: Vec<&String> = all_imports.iter().filter(|i| !is_go_stdlib(i)).collect();
    stdlib.sort();
    third_party.sort();

    // If only one import, use single-line syntax.
    if stdlib.len() + third_party.len() == 1 {
        let single = stdlib.first().or(third_party.first()).unwrap();
        return vec![format!("import {}", single)];
    }

    // Build import block.
    let mut result = vec!["import (".to_string()];
    for imp in &stdlib {
        result.push(format!("\t{}", imp));
    }
    if !stdlib.is_empty() && !third_party.is_empty() {
        result.push(String::new()); // blank line separator
    }
    for imp in &third_party {
        result.push(format!("\t{}", imp));
    }
    result.push(")".to_string());
    result
}

// ---------------------------------------------------------------------------
// Python-specific import merging
// ---------------------------------------------------------------------------

/// Common Python standard library module names (top-level).
/// Used as a heuristic for PEP 8 import ordering.
const PYTHON_STDLIB: &[&str] = &[
    "__future__",
    "abc",
    "aifc",
    "argparse",
    "array",
    "ast",
    "asynchat",
    "asyncio",
    "asyncore",
    "atexit",
    "audioop",
    "base64",
    "bdb",
    "binascii",
    "binhex",
    "bisect",
    "builtins",
    "bz2",
    "calendar",
    "cgi",
    "cgitb",
    "chunk",
    "cmath",
    "cmd",
    "code",
    "codecs",
    "codeop",
    "collections",
    "colorsys",
    "compileall",
    "concurrent",
    "configparser",
    "contextlib",
    "contextvars",
    "copy",
    "copyreg",
    "cProfile",
    "crypt",
    "csv",
    "ctypes",
    "curses",
    "dataclasses",
    "datetime",
    "dbm",
    "decimal",
    "difflib",
    "dis",
    "distutils",
    "doctest",
    "email",
    "encodings",
    "enum",
    "errno",
    "faulthandler",
    "fcntl",
    "filecmp",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getopt",
    "getpass",
    "gettext",
    "glob",
    "grp",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "idlelib",
    "imaplib",
    "imghdr",
    "imp",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "lib2to3",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "mailbox",
    "mailcap",
    "marshal",
    "math",
    "mimetypes",
    "mmap",
    "modulefinder",
    "multiprocessing",
    "netrc",
    "nis",
    "nntplib",
    "numbers",
    "operator",
    "optparse",
    "os",
    "ossaudiodev",
    "pathlib",
    "pdb",
    "pickle",
    "pickletools",
    "pipes",
    "pkgutil",
    "platform",
    "plistlib",
    "poplib",
    "posix",
    "posixpath",
    "pprint",
    "profile",
    "pstats",
    "pty",
    "pwd",
    "py_compile",
    "pyclbr",
    "pydoc",
    "queue",
    "quopri",
    "random",
    "re",
    "readline",
    "reprlib",
    "resource",
    "rlcompleter",
    "runpy",
    "sched",
    "secrets",
    "select",
    "selectors",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtpd",
    "smtplib",
    "sndhdr",
    "socket",
    "socketserver",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "stringprep",
    "struct",
    "subprocess",
    "sunau",
    "symtable",
    "sys",
    "sysconfig",
    "syslog",
    "tabnanny",
    "tarfile",
    "telnetlib",
    "tempfile",
    "termios",
    "test",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "trace",
    "traceback",
    "tracemalloc",
    "tty",
    "turtle",
    "turtledemo",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uu",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "winreg",
    "winsound",
    "wsgiref",
    "xdrlib",
    "xml",
    "xmlrpc",
    "zipapp",
    "zipfile",
    "zipimport",
    "zlib",
];

/// Classify a Python import's top-level module for PEP 8 ordering.
/// Returns: 0 = __future__, 1 = stdlib, 2 = third-party, 3 = local/relative.
fn python_import_group(module: &str) -> u8 {
    if module == "__future__" {
        return 0;
    }
    // Relative imports: start with .
    if module.starts_with('.') {
        return 3;
    }
    // Extract top-level module name (before first dot).
    let top_level = module.split('.').next().unwrap_or(module);
    if PYTHON_STDLIB.contains(&top_level) {
        1
    } else {
        2
    }
}

/// A parsed Python import statement.
#[derive(Debug, Clone)]
struct PyImport {
    /// The module being imported from (e.g., `os.path`, `flask`, `.utils`).
    module: String,
    /// Named imports for `from X import Y, Z` style. Empty for bare `import X`.
    names: Vec<String>,
    /// True if this is `import X` (bare), false if `from X import Y`.
    is_bare: bool,
    /// True if this is `from X import *`.
    is_star: bool,
}

/// Parse a single Python import line into structured form.
fn parse_python_import(line: &str) -> Option<PyImport> {
    let trimmed = line.trim();

    // `from X import Y, Z` or `from X import *`
    if trimmed.starts_with("from ") {
        let rest = trimmed[5..].trim();
        let import_idx = rest.find(" import ")?;
        let module = rest[..import_idx].trim().to_string();
        let names_part = rest[import_idx + 8..].trim();

        if names_part == "*" {
            return Some(PyImport {
                module,
                names: vec![],
                is_bare: false,
                is_star: true,
            });
        }

        // Handle parenthesized imports: `from X import (A, B, C)`
        let names_str = names_part
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim();
        let names: Vec<String> = names_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        return Some(PyImport {
            module,
            names,
            is_bare: false,
            is_star: false,
        });
    }

    // `import X` or `import X, Y` or `import X as Y`
    if trimmed.starts_with("import ") {
        let rest = trimmed[7..].trim();
        // Could be multiple: `import os, sys` — split by comma.
        let modules: Vec<String> = rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // For bare imports, module is the first part (before `as` if present).
        // We store each as a separate import for proper grouping.
        if modules.len() == 1 {
            return Some(PyImport {
                module: modules[0].clone(),
                names: vec![],
                is_bare: true,
                is_star: false,
            });
        }
        // Multiple bare imports on one line — return the first, caller handles.
        // For simplicity, store the full comma-separated text as module.
        return Some(PyImport {
            module: rest.to_string(),
            names: vec![],
            is_bare: true,
            is_star: false,
        });
    }

    None
}

/// Render a `PyImport` back to a single import line.
fn render_python_import(import: &PyImport) -> String {
    if import.is_bare {
        return format!("import {}", import.module);
    }
    if import.is_star {
        return format!("from {} import *", import.module);
    }
    let mut sorted = import.names.clone();
    sorted.sort();
    format!("from {} import {}", import.module, sorted.join(", "))
}

/// Merge two sets of Python import lines with same-module consolidation
/// and PEP 8 ordering.
///
/// - `from X import A` + `from X import B` → `from X import A, B`
/// - Groups: `__future__`, stdlib, third-party, local/relative
/// - Blank line between groups
/// - Named imports sorted alphabetically within each statement
fn merge_python_imports(left: &[String], right: &[String]) -> Vec<String> {
    // Key: (module, is_bare, is_star) → merged import
    let mut groups: std::collections::BTreeMap<(String, bool, bool), PyImport> =
        std::collections::BTreeMap::new();

    for line in left.iter().chain(right.iter()) {
        if let Some(import) = parse_python_import(line) {
            let key = (import.module.clone(), import.is_bare, import.is_star);

            if let Some(existing) = groups.get_mut(&key) {
                // Merge named imports from same module.
                for name in &import.names {
                    if !existing.names.contains(name) {
                        existing.names.push(name.clone());
                    }
                }
            } else {
                groups.insert(key, import);
            }
        }
    }

    // Classify each import into PEP 8 groups and sort.
    let mut grouped: Vec<(u8, String)> = groups
        .values()
        .map(|imp| {
            let group = python_import_group(&imp.module);
            let rendered = render_python_import(imp);
            (group, rendered)
        })
        .collect();

    grouped.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    // Build result with blank lines between groups.
    let mut result = Vec::new();
    let mut last_group: Option<u8> = None;

    for (group, line) in &grouped {
        if let Some(prev) = last_group {
            if *group != prev {
                result.push(String::new()); // blank line between groups
            }
        }
        result.push(line.clone());
        last_group = Some(*group);
    }

    result
}

/// Parse lines into groups of (declaration_line, preceding_decorators).
/// A decorator is any line matching `is_decorator_line`; the next
/// non-decorator, non-empty line is the declaration it annotates.
fn parse_decorator_groups(lines: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups = Vec::new();
    let mut current_decorators: Vec<String> = Vec::new();

    for line in lines {
        if is_decorator_line(line) {
            current_decorators.push(line.clone());
        } else if !line.trim().is_empty() {
            groups.push((line.clone(), std::mem::take(&mut current_decorators)));
        }
    }

    groups
}

/// Consolidate multiple `#[derive(...)]` attributes into a single one.
///
/// When two agents both add derives to the same struct, the decorator union
/// may contain `#[derive(Debug, Clone, Serialize)]` and `#[derive(Debug, Clone, Deserialize)]`.
/// This function merges them into `#[derive(Clone, Debug, Deserialize, Serialize)]`.
/// Non-derive attributes pass through unchanged.
fn consolidate_rust_derives(decorators: &[String]) -> Vec<String> {
    let mut derive_traits: Vec<String> = Vec::new();
    let mut non_derive: Vec<String> = Vec::new();

    for dec in decorators {
        let trimmed = dec.trim();
        if trimmed.starts_with("#[derive(") && trimmed.ends_with(")]") {
            let inner = &trimmed[9..trimmed.len() - 2];
            for trait_name in inner.split(',') {
                let t = trait_name.trim().to_string();
                if !t.is_empty() && !derive_traits.contains(&t) {
                    derive_traits.push(t);
                }
            }
        } else {
            non_derive.push(dec.clone());
        }
    }

    let mut result = Vec::new();
    if !derive_traits.is_empty() {
        derive_traits.sort();
        result.push(format!("#[derive({})]", derive_traits.join(", ")));
    }
    result.extend(non_derive);
    result
}

/// Merge decorator groups from both sides, preserving all decorators.
///
/// For each declaration line present in either side, collects the union
/// of decorators from both sides. Uses the longer side for declaration
/// ordering. Returns `None` if neither side has decorator structure.
/// For Rust `#[derive(...)]` attributes, consolidates derive lists.
fn merge_with_decorator_preservation(
    left: &[String],
    right: &[String],
    _base: &[String],
) -> Option<Vec<String>> {
    let left_groups = parse_decorator_groups(left);
    let right_groups = parse_decorator_groups(right);

    // Only applies when at least one side has decorators.
    let has_decorators = left_groups.iter().any(|(_, d)| !d.is_empty())
        || right_groups.iter().any(|(_, d)| !d.is_empty());
    if !has_decorators {
        return None;
    }

    let mut result = Vec::new();
    let mut seen_decls = std::collections::HashSet::new();

    // Use the longer side as the ordering base.
    let (primary, secondary) = if left.len() >= right.len() {
        (&left_groups, &right_groups)
    } else {
        (&right_groups, &left_groups)
    };

    for (decl, decorators) in primary {
        seen_decls.insert(decl.clone());
        let mut all_decorators = decorators.clone();
        // Add decorators from the other side for the same declaration.
        if let Some((_, other_decorators)) = secondary.iter().find(|(d, _)| d == decl) {
            for dec in other_decorators {
                if !all_decorators.contains(dec) {
                    all_decorators.push(dec.clone());
                }
            }
        }
        // Consolidate Rust #[derive(...)] attributes.
        let all_decorators = consolidate_rust_derives(&all_decorators);
        result.extend(all_decorators);
        result.push(decl.clone());
    }

    // Add declarations only in secondary.
    for (decl, decorators) in secondary {
        if !seen_decls.contains(decl) {
            let decorators = consolidate_rust_derives(decorators);
            result.extend(decorators);
            result.push(decl.clone());
        }
    }

    Some(result)
}

/// Resolve all conflict regions for a single file using deterministic patterns.
///
/// This is the main entry point for the resolution engine.
/// `left_spec_id` and `right_spec_id` are used for deterministic
/// ordering (BothInserted regions are concatenated ordered by spec ID).
///
/// BothInserted → concatenate/import-accumulate.
/// DeleteVsModify → **always escalated** (per v2.0 spec — never silently resolve).
/// BothModified → pattern matching (superset, additive, imports, decorators, definitions).
/// Unresolved → escalation record for the strategy fallback.
pub fn resolve_conflict_regions(
    file_path: &str,
    base_content: &str,
    left_content: &str,
    right_content: &str,
    regions: &[ConflictRegion],
    left_spec_id: &str,
    right_spec_id: &str,
) -> ResolvedFile {
    let mut resolved_regions: Vec<RegionResolution> = Vec::new();
    let mut unresolved: Vec<ConflictRegion> = Vec::new();
    let mut escalation_records: Vec<PipelineEscalation> = Vec::new();
    let mut all_resolved = true;

    let total_base_lines = if base_content.is_empty() {
        0
    } else {
        base_content.lines().count()
    };

    // Determine spec ordering for deterministic concatenation.
    let left_first = left_spec_id <= right_spec_id;

    for region in regions {
        let class = classify_region(region, total_base_lines);

        match &class {
            ConflictClass::BothInserted => {
                // Use multiline-aware import region detection.
                let all_imports =
                    is_import_region(&region.left_lines) && is_import_region(&region.right_lines);

                let lines = if all_imports {
                    // Check if JS/TS for source-module grouping.
                    let combined: Vec<String> = region
                        .left_lines
                        .iter()
                        .chain(region.right_lines.iter())
                        .cloned()
                        .collect();
                    let lang = detect_import_language(&combined);

                    match lang {
                        ImportLanguage::JavaScript => {
                            merge_js_imports(&region.left_lines, &region.right_lines)
                        }
                        ImportLanguage::Python => {
                            merge_python_imports(&region.left_lines, &region.right_lines)
                        }
                        ImportLanguage::Go => {
                            merge_go_imports(&region.left_lines, &region.right_lines)
                        }
                        _ => {
                            // Other languages: keep spec-ID ordering with line-level dedup.
                            let mut l = Vec::new();
                            if left_first {
                                l.extend(region.left_lines.iter().cloned());
                                l.extend(region.right_lines.iter().cloned());
                            } else {
                                l.extend(region.right_lines.iter().cloned());
                                l.extend(region.left_lines.iter().cloned());
                            }
                            let mut seen = std::collections::HashSet::new();
                            l.retain(|line| seen.insert(line.clone()));
                            l
                        }
                    }
                } else {
                    let mut l = Vec::new();
                    if left_first {
                        l.extend(region.left_lines.iter().cloned());
                        l.extend(region.right_lines.iter().cloned());
                    } else {
                        l.extend(region.right_lines.iter().cloned());
                        l.extend(region.left_lines.iter().cloned());
                    }
                    l
                };

                let method = if all_imports {
                    "import-accumulation"
                } else {
                    "concatenated"
                };

                resolved_regions.push(RegionResolution {
                    lines,
                    class: class.clone(),
                    method: method.to_string(),
                    confidence: if all_imports { 0.95 } else { 0.9 },
                });
            }
            ConflictClass::DeleteVsModify { .. } => {
                // v2.0 spec: DeleteVsModify always escalates — the deletion
                // may be intentional and we must never silently discard it.
                all_resolved = false;
                unresolved.push(region.clone());
                escalation_records.push(PipelineEscalation {
                    file_path: file_path.to_string(),
                    reason: "DeleteVsModify always escalates per v2.0 spec".to_string(),
                    conflict_class: "delete-vs-modify".to_string(),
                    left_spec: left_spec_id.to_string(),
                    right_spec: right_spec_id.to_string(),
                    recommended_action: "Review whether deletion or modification is correct"
                        .to_string(),
                });
                resolved_regions.push(RegionResolution {
                    lines: vec![],
                    class: class.clone(),
                    method: "escalated-delete-vs-modify".to_string(),
                    confidence: 0.0,
                });
            }
            ConflictClass::BothModified => {
                // End-of-file append detection: when both sides preserved ALL
                // base content and each appended unique new content, the diff3
                // algorithm may split the result into a clean shared region
                // (the original base) plus a conflict with empty base_lines for
                // the divergent additions. Verify both full files kept the base
                // as a prefix — this distinguishes genuine appends from
                // complete file replacements where the diff also produces
                // empty-base regions.
                if region.base_lines.is_empty()
                    && total_base_lines > 0
                    && !region.left_lines.is_empty()
                    && !region.right_lines.is_empty()
                {
                    let base_lines: Vec<&str> = base_content.lines().collect();
                    let left_lines: Vec<&str> = left_content.lines().collect();
                    let right_lines: Vec<&str> = right_content.lines().collect();
                    let left_additive = left_lines.len() >= base_lines.len()
                        && left_lines[..base_lines.len()] == base_lines[..];
                    let right_additive = right_lines.len() >= base_lines.len()
                        && right_lines[..base_lines.len()] == base_lines[..];

                    if left_additive && right_additive {
                        let mut l = Vec::new();
                        if left_first {
                            l.extend(region.left_lines.iter().cloned());
                            l.extend(region.right_lines.iter().cloned());
                        } else {
                            l.extend(region.right_lines.iter().cloned());
                            l.extend(region.left_lines.iter().cloned());
                        }
                        resolved_regions.push(RegionResolution {
                            lines: l,
                            class: class.clone(),
                            method: "concatenated".to_string(),
                            confidence: 0.9,
                        });
                        continue;
                    }
                }

                // Check for superset first.
                if let Some(superset_side) =
                    detect_superset(&region.left_lines, &region.right_lines)
                {
                    let lines = match superset_side {
                        MergeSide::Left => region.left_lines.clone(),
                        MergeSide::Right => region.right_lines.clone(),
                    };
                    resolved_regions.push(RegionResolution {
                        lines,
                        class: class.clone(),
                        method: "superset-detected".to_string(),
                        confidence: 0.95,
                    });
                    continue;
                }

                // Layer 3: Additive composition — if both sides kept all
                // base content and each added unique new content, compose
                // by taking base + left additions + right additions.
                // This prevents silent data loss when multiple agents add
                // classes/functions/definitions to the same file.
                if !region.base_lines.is_empty() {
                    if let Some(composed) = try_additive_composition(
                        &region.base_lines,
                        &region.left_lines,
                        &region.right_lines,
                        left_first,
                    ) {
                        resolved_regions.push(RegionResolution {
                            lines: composed,
                            class: class.clone(),
                            method: "additive-composition".to_string(),
                            confidence: 0.85,
                        });
                        continue;
                    }
                }

                // Layer 3: Import accumulation — if both sides are pure
                // import regions, merge by taking the union of all imports.
                if is_import_region(&region.left_lines) && is_import_region(&region.right_lines) {
                    let lines = merge_imports(&region.left_lines, &region.right_lines);
                    resolved_regions.push(RegionResolution {
                        lines,
                        class: class.clone(),
                        method: "import-accumulation".to_string(),
                        confidence: 0.92,
                    });
                    continue;
                }

                // Layer 3: Decorator preservation — if either side has
                // decorator lines, try to preserve decorators from both.
                if region.left_lines.iter().any(|l| is_decorator_line(l))
                    || region.right_lines.iter().any(|l| is_decorator_line(l))
                {
                    if let Some(merged) = merge_with_decorator_preservation(
                        &region.left_lines,
                        &region.right_lines,
                        &region.base_lines,
                    ) {
                        resolved_regions.push(RegionResolution {
                            lines: merged,
                            class: class.clone(),
                            method: "decorator-preservation".to_string(),
                            confidence: 0.88,
                        });
                        continue;
                    }

                    // Fallback: pure decorator region (no associated declaration
                    // in the conflict — e.g., both sides modified #[derive(...)]).
                    // Consolidate all decorators and merge Rust derives.
                    if region.left_lines.iter().all(|l| is_decorator_line(l))
                        && region.right_lines.iter().all(|l| is_decorator_line(l))
                    {
                        let mut all_decorators: Vec<String> = Vec::new();
                        for dec in region.left_lines.iter().chain(region.right_lines.iter()) {
                            if !all_decorators.contains(dec) {
                                all_decorators.push(dec.clone());
                            }
                        }
                        let consolidated = consolidate_rust_derives(&all_decorators);
                        resolved_regions.push(RegionResolution {
                            lines: consolidated,
                            class: class.clone(),
                            method: "decorator-consolidation".to_string(),
                            confidence: 0.92,
                        });
                        continue;
                    }
                }

                // Unresolved — falls through to Layer 4 strategy.
                all_resolved = false;
                unresolved.push(region.clone());
                resolved_regions.push(RegionResolution {
                    lines: vec![],
                    class: class.clone(),
                    method: "unresolved".to_string(),
                    confidence: 0.0,
                });
            }
        }
    }

    // Rebuild the file content by replaying the three-way merge with
    // our resolved regions substituted in.
    let content = rebuild_with_resolutions(
        base_content,
        left_content,
        right_content,
        regions,
        &resolved_regions,
    );

    ResolvedFile {
        content,
        resolutions: resolved_regions,
        fully_resolved: all_resolved,
        unresolved_regions: unresolved,
        escalation_records,
    }
}

// ---------------------------------------------------------------------------
// Smart merge — top-level API wrapping three-way merge with Layers 2-3
// ---------------------------------------------------------------------------

/// Result of `smart_merge`: three-way merge enhanced with Layer 2-3
/// auto-resolution. Either fully resolved or partially resolved with
/// remaining conflict regions for Layer 4 strategy fallback.
#[derive(Debug, Clone)]
pub enum SmartMergeResult {
    /// All conflicts were auto-resolved by Layers 2-3.
    Clean {
        content: String,
        resolutions: Vec<RegionResolution>,
    },
    /// Some regions remain as conflicts for Layer 4 fallback.
    Partial {
        /// Best-effort content (unresolved regions include both sides).
        content: String,
        resolutions: Vec<RegionResolution>,
        unresolved: Vec<ConflictRegion>,
    },
}

// ---------------------------------------------------------------------------
// Post-merge cleanup (Layer 5)
// ---------------------------------------------------------------------------

/// Deduplicate import statements across an entire file.
///
/// After merge/concatenation, the same import can appear multiple times —
/// e.g., once at the top from the original file, and again mid-file from
/// appended code. This function:
/// - Removes exact-duplicate import lines (any language)
/// - Consolidates same-module Python `from X import Y` statements scattered
///   across the file into the first occurrence
fn dedup_imports_whole_file(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = Vec::new();

    // Track seen imports: key → index in result where it first appeared.
    // For Python `from X import Y,Z`: key = module name, value = index.
    let mut seen_bare: std::collections::HashSet<String> = std::collections::HashSet::new();
    // For Python from-imports: module → index in result.
    let mut seen_from: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for line in &lines {
        let trimmed = line.trim();

        // Try parsing as a Python import for consolidation.
        if let Some(py) = parse_python_import(trimmed) {
            if py.is_star {
                // Star imports: dedup by module.
                let key = format!("from {} import *", py.module);
                if seen_bare.contains(&key) {
                    continue; // skip duplicate
                }
                seen_bare.insert(key);
                result.push(line.to_string());
            } else if py.is_bare {
                // Bare `import X`: dedup by exact text.
                let key = format!("import {}", py.module);
                if seen_bare.contains(&key) {
                    continue;
                }
                seen_bare.insert(key);
                result.push(line.to_string());
            } else {
                // `from X import Y, Z`: consolidate into first occurrence.
                if let Some(&first_idx) = seen_from.get(&py.module) {
                    // Merge names into the existing line.
                    if let Some(existing) = parse_python_import(result[first_idx].trim()) {
                        let mut merged_names = existing.names.clone();
                        for name in &py.names {
                            if !merged_names.contains(name) {
                                merged_names.push(name.clone());
                            }
                        }
                        let updated = PyImport {
                            module: existing.module,
                            names: merged_names,
                            is_bare: false,
                            is_star: false,
                        };
                        result[first_idx] = render_python_import(&updated);
                    }
                    // Don't push the duplicate line.
                } else {
                    seen_from.insert(py.module.clone(), result.len());
                    result.push(line.to_string());
                }
            }
        } else if is_import_line(line) {
            // Non-Python imports: dedup by exact text.
            if seen_bare.contains(trimmed) {
                continue;
            }
            seen_bare.insert(trimmed.to_string());
            result.push(line.to_string());
        } else {
            result.push(line.to_string());
        }
    }

    let mut merged = result.join("\n");
    if content.ends_with('\n') && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
}

/// Prune unused imports from merged Python content.
///
/// Scans for imported symbols that never appear in the non-import body
/// of the file. Conservative: keeps bare `import X` (since `X.method()`
/// may be used), keeps `import *`, and only removes specific names from
/// `from X import Y, Z` when `Y` or `Z` isn't referenced anywhere.
fn prune_unused_python_imports(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    // Collect the non-import body text for symbol scanning.
    let body: String = lines
        .iter()
        .filter(|l| !is_import_line(l) && !l.trim().is_empty())
        .cloned()
        .collect::<Vec<&str>>()
        .join("\n");

    let mut result: Vec<String> = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        if let Some(py) = parse_python_import(trimmed) {
            // Keep bare imports (import os — could use os.X).
            if py.is_bare || py.is_star {
                result.push(line.to_string());
                continue;
            }

            // For `from X import Y, Z`: check which names are used.
            let used_names: Vec<String> = py
                .names
                .iter()
                .filter(|name| {
                    // Handle `as` aliases: `from X import Y as Z` — check Z.
                    let check_name = if let Some(alias_idx) = name.find(" as ") {
                        &name[alias_idx + 4..]
                    } else {
                        name.as_str()
                    };
                    // Check if the symbol appears in the body as a whole word.
                    contains_word(&body, check_name)
                })
                .cloned()
                .collect();

            if used_names.is_empty() {
                // No names from this import are used — drop the entire line.
                continue;
            }

            if used_names.len() == py.names.len() {
                // All names used — keep as-is.
                result.push(line.to_string());
            } else {
                // Some names unused — rewrite with only used names.
                let pruned = PyImport {
                    module: py.module,
                    names: used_names,
                    is_bare: false,
                    is_star: false,
                };
                result.push(render_python_import(&pruned));
            }
        } else {
            result.push(line.to_string());
        }
    }

    let mut merged = result.join("\n");
    if content.ends_with('\n') && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
}

/// Fix formatting in merged Python content.
///
/// Ensures 2 blank lines before top-level `class` and `def` definitions
/// (PEP 8). Does not add blank lines at the very start of the file or
/// after import blocks.
fn fix_python_formatting(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut result: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_top_level_def = (trimmed.starts_with("def ") || trimmed.starts_with("class "))
            && !line.starts_with(' ')
            && !line.starts_with('\t');

        if is_top_level_def && i > 0 {
            // Count trailing blank lines already in result.
            let mut trailing_blanks = 0;
            for prev in result.iter().rev() {
                if prev.trim().is_empty() {
                    trailing_blanks += 1;
                } else {
                    break;
                }
            }

            // Check if the preceding non-blank line is an import —
            // only need 2 blanks between import block and first definition.
            let prev_is_import = result
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| is_import_line(l))
                .unwrap_or(false);

            let needed = if prev_is_import { 2 } else { 2 };

            // Add blank lines to reach the needed count.
            while trailing_blanks < needed {
                result.push(String::new());
                trailing_blanks += 1;
            }

            // Remove excess blank lines (more than 2).
            while trailing_blanks > needed {
                if let Some(last) = result.last() {
                    if last.trim().is_empty() {
                        result.pop();
                        trailing_blanks -= 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }

        result.push(line.to_string());
    }

    let mut merged = result.join("\n");
    if content.ends_with('\n') && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
}

/// Apply post-merge cleanup to the final merged content.
///
/// Runs language-aware cleanup passes:
/// 1. Duplicate import removal (all languages)
/// 2. Unused import pruning (Python only)
/// 3. Formatting fixes (Python only)
///
/// Only activates when the content contains import-like patterns,
/// keeping it a no-op for non-code files.
pub fn post_merge_cleanup(content: &str, path: &str) -> String {
    // Quick language hint from file extension.
    let ext_lang = if path.ends_with(".py") {
        Some(ImportLanguage::Python)
    } else if path.ends_with(".js")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".jsx")
    {
        Some(ImportLanguage::JavaScript)
    } else if path.ends_with(".go") {
        Some(ImportLanguage::Go)
    } else if path.ends_with(".rs") {
        Some(ImportLanguage::Rust)
    } else {
        None
    };

    // Quick check: does this content have any imports?
    let has_imports = content.lines().any(|l| is_import_line(l));
    if !has_imports {
        // Even without imports, Python files still benefit from formatting.
        if ext_lang == Some(ImportLanguage::Python) {
            return fix_python_formatting(content);
        }
        return content.to_string();
    }

    // Step 1: Deduplicate imports.
    let cleaned = dedup_imports_whole_file(content);

    // Steps 2-3: Python-specific cleanup.
    let lang = ext_lang.unwrap_or_else(|| {
        let all_lines: Vec<String> = cleaned.lines().map(|l| l.to_string()).collect();
        detect_import_language(&all_lines)
    });

    if lang == ImportLanguage::Python {
        let pruned = prune_unused_python_imports(&cleaned);
        fix_python_formatting(&pruned)
    } else {
        cleaned
    }
}

/// Perform a three-way merge with Layers 2-3 auto-resolution.
///
/// Calls `three_way_merge()` (Layer 1), then attempts to resolve any
/// conflict regions using region classification and structural pattern
/// matching (Layers 2-3). Only truly irreconcilable `BothModified`
/// regions remain as conflicts for Layer 4 strategy fallback.
///
/// After conflict resolution, applies Layer 5 post-merge cleanup:
/// duplicate import removal, unused import pruning, and formatting fixes.
///
/// `left_spec` and `right_spec` are spec IDs used for deterministic
/// ordering when concatenating `BothInserted` regions.
pub fn smart_merge(
    file_path: &str,
    base: &str,
    left: &str,
    right: &str,
    left_spec: &str,
    right_spec: &str,
) -> SmartMergeResult {
    // Layer 1: standard three-way merge.
    match three_way_merge(base, left, right) {
        FileMergeResult::Clean(content) => {
            // Layer 5: post-merge cleanup.
            let cleaned = post_merge_cleanup(&content, "");
            SmartMergeResult::Clean {
                content: cleaned,
                resolutions: vec![],
            }
        }
        FileMergeResult::Conflict(regions) => {
            // Layers 2-3: attempt to auto-resolve conflict regions.
            let resolved = resolve_conflict_regions(
                file_path, base, left, right, &regions, left_spec, right_spec,
            );

            if resolved.fully_resolved {
                // Layer 5: post-merge cleanup.
                let cleaned = post_merge_cleanup(&resolved.content, "");
                SmartMergeResult::Clean {
                    content: cleaned,
                    resolutions: resolved.resolutions,
                }
            } else {
                // Layer 5: post-merge cleanup (even partial merges benefit).
                let cleaned = post_merge_cleanup(&resolved.content, "");
                SmartMergeResult::Partial {
                    content: cleaned,
                    resolutions: resolved.resolutions,
                    unresolved: resolved.unresolved_regions,
                }
            }
        }
    }
}

/// Result of `converge_all` — multi-branch convergence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergeAllReport {
    /// Spec used as merge base (on HEAD chain).
    pub base_spec: String,
    /// Spec IDs in the order they were merged.
    pub merge_order: Vec<String>,
    /// Per-merge step results.
    pub merges: Vec<MergeStepResult>,
    /// Strategy used for conflict resolution.
    pub strategy: String,
    /// Total files auto-merged across all steps.
    pub total_auto_merged: usize,
    /// Total conflicts encountered across all steps.
    pub total_conflicts: usize,
    /// Total conflicts resolved by the chosen strategy.
    pub total_resolutions: usize,
    /// True if all merges are clean (or all conflicts were resolved).
    pub is_clean: bool,
    /// True if most-recent strategy discarded non-empty content in any step.
    /// A degraded merge completed structurally but may have broken cross-file
    /// references.
    #[serde(default)]
    pub degraded: bool,
    /// Whether changes were applied to the working directory.
    pub applied: bool,
    /// Warnings about potential content loss or semantic inconsistency.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    /// Conflicts escalated for human/orchestrator review (Escalate strategy).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub escalations: Vec<PipelineEscalation>,
    /// Post-convergence quality assessment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_report: Option<ConvergenceQualityReport>,
}

/// Result of a single merge step in `converge_all`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeStepResult {
    /// Left spec (base side).
    pub left_spec: String,
    /// Right spec (being merged in).
    pub right_spec: String,
    /// Number of files auto-merged cleanly.
    pub auto_merged: usize,
    /// Number of conflicts.
    pub conflicts: usize,
    /// Number of left-only files.
    pub left_only: usize,
    /// Number of right-only files.
    pub right_only: usize,
    /// Paths of conflicted files.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub conflict_files: Vec<String>,
    /// How each conflict was resolved (if strategy was applied).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolutions: Vec<ResolutionRecord>,
    /// True if this step had no conflicts (or all were resolved).
    pub clean: bool,
    /// True if most-recent strategy discarded non-empty content in this step.
    #[serde(default)]
    pub degraded: bool,
    /// Error message if this merge step failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Record of how a specific file conflict was resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionRecord {
    /// Path of the resolved file.
    pub path: String,
    /// Strategy used: "auto-merged", "most-recent", "left-wins", "right-wins".
    pub strategy: String,
    /// Which spec's version was chosen (for most-recent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_spec: Option<String>,
    /// Warning about content that may have been lost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost_content_warning: Option<String>,
}

/// Post-convergence quality report — shows what was chosen, what was
/// discarded, and consistency metrics so humans/orchestrators can verify
/// the merged result makes sense.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceQualityReport {
    /// Per-file details of what version was chosen and what alternatives existed.
    pub file_decisions: Vec<FileDecision>,
    /// Consistency metrics across related files (e.g., nav item counts in HTML).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub consistency_checks: Vec<ConsistencyCheck>,
    /// Overall quality score (0-100).
    pub quality_score: u32,
    /// Lowest per-file confidence across all auto-resolved files (0.0-1.0).
    /// Agents should review files where min_confidence < 0.85.
    pub min_confidence: f64,
    /// Average per-file confidence across all auto-resolved files (0.0-1.0).
    pub avg_confidence: f64,
    /// Human-readable summary of the quality assessment.
    pub summary: String,
}

/// Record of how a specific file was resolved during convergence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDecision {
    /// File path.
    pub path: String,
    /// How this file was handled: "auto-merged", "auto-resolved", "left-only",
    /// "right-only", "most-recent", "conflict-unresolved".
    pub decision: String,
    /// Line count of the chosen version.
    pub chosen_lines: usize,
    /// Which spec's version was chosen (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chosen_spec: Option<String>,
    /// Alternative versions that were discarded.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub alternatives: Vec<FileAlternative>,
    /// Per-file confidence (0.0-1.0). Some(1.0) for clean auto-merges,
    /// Some(avg) for auto-resolved, Some(0.7) for strategy fallback,
    /// None for left-only/right-only (no conflict).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// A discarded alternative version of a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAlternative {
    /// Which spec had this version.
    pub spec: String,
    /// Line count of this alternative.
    pub lines: usize,
    /// Why it was discarded.
    pub reason: String,
}

/// A consistency check across related files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyCheck {
    /// What was checked (e.g., "nav_item_count", "css_link_count").
    pub metric: String,
    /// Per-file values for this metric.
    pub values: Vec<FileMetricValue>,
    /// Whether the check passed (all values are consistent).
    pub consistent: bool,
    /// Human-readable description of the inconsistency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// A metric value for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetricValue {
    /// File path.
    pub path: String,
    /// The metric value.
    pub value: usize,
}

// ---------------------------------------------------------------------------
// Three-way merge algorithm
// ---------------------------------------------------------------------------

/// What one side did at a particular base line position.
#[derive(Debug, Clone, PartialEq)]
enum LineAction {
    /// Keep the base line unchanged.
    Keep,
    /// Delete the base line (replace with nothing).
    Delete,
    /// Replace the base line with different content.
    Replace(Vec<String>),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_three_way_merge_no_overlap() {
        let base = "line1\nline2\nline3\nline4\nline5";
        let left = "LEFT1\nline2\nline3\nline4\nline5";
        let right = "line1\nline2\nline3\nline4\nRIGHT5";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "LEFT1\nline2\nline3\nline4\nRIGHT5");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_conflict() {
        let base = "line1\nline2\nline3";
        let left = "line1\nLEFT\nline3";
        let right = "line1\nRIGHT\nline3";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(_) => panic!("expected conflict"),
            FileMergeResult::Conflict(regions) => {
                assert_eq!(regions.len(), 1);
                assert_eq!(regions[0].base_lines, vec!["line2"]);
                assert_eq!(regions[0].left_lines, vec!["LEFT"]);
                assert_eq!(regions[0].right_lines, vec!["RIGHT"]);
                assert_eq!(regions[0].base_start, 2); // 1-based
            }
        }
    }

    #[test]
    fn test_three_way_merge_identical_change() {
        let base = "line1\nline2\nline3";
        let left = "line1\nSAME\nline3";
        let right = "line1\nSAME\nline3";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "line1\nSAME\nline3");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_only_left_changed() {
        let base = "line1\nline2\nline3";
        let left = "line1\nLEFT\nline3";
        let right = "line1\nline2\nline3"; // unchanged

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "line1\nLEFT\nline3");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_only_right_changed() {
        let base = "line1\nline2\nline3";
        let left = "line1\nline2\nline3"; // unchanged
        let right = "line1\nline2\nRIGHT";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "line1\nline2\nRIGHT");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_add_lines() {
        let base = "line1\nline3";
        let left = "line1\nLEFT_NEW\nline3";
        let right = "line1\nline3\nRIGHT_NEW";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "line1\nLEFT_NEW\nline3\nRIGHT_NEW");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_empty_base() {
        let base = "";
        let left = "left content";
        let right = "right content";

        // Both added content from empty — different content = conflict.
        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(_) => panic!("expected conflict"),
            FileMergeResult::Conflict(regions) => {
                assert!(!regions.is_empty());
            }
        }
    }

    #[test]
    fn test_three_way_merge_both_identical_from_empty() {
        let base = "";
        let left = "same content";
        let right = "same content";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(content) => {
                assert_eq!(content, "same content");
            }
            FileMergeResult::Conflict(_) => panic!("expected clean merge"),
        }
    }

    #[test]
    fn test_three_way_merge_multi_region_conflict() {
        let base = "a\nb\nc\nd\ne";
        let left = "A\nb\nC\nd\ne";
        let right = "X\nb\nY\nd\ne";

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(_) => panic!("expected conflict"),
            FileMergeResult::Conflict(regions) => {
                // Both changed line 1 and line 3 differently.
                assert_eq!(regions.len(), 2);
                assert_eq!(regions[0].left_lines, vec!["A"]);
                assert_eq!(regions[0].right_lines, vec!["X"]);
                assert_eq!(regions[1].left_lines, vec!["C"]);
                assert_eq!(regions[1].right_lines, vec!["Y"]);
            }
        }
    }

    #[test]
    fn test_three_way_merge_delete_vs_modify() {
        let base = "line1\nline2\nline3";
        let left = "line1\nline3"; // deleted line2
        let right = "line1\nMODIFIED\nline3"; // modified line2

        match three_way_merge(base, left, right) {
            FileMergeResult::Clean(_) => panic!("expected conflict"),
            FileMergeResult::Conflict(regions) => {
                assert_eq!(regions.len(), 1);
                assert_eq!(regions[0].base_lines, vec!["line2"]);
                assert!(regions[0].left_lines.is_empty()); // deleted
                assert_eq!(regions[0].right_lines, vec!["MODIFIED"]);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Layer 2-3: Region classification and resolution tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_classify_both_inserted() {
        let region = ConflictRegion {
            base_start: 1,
            base_lines: vec![],
            left_lines: vec!["import os".to_string()],
            right_lines: vec!["import sys".to_string()],
        };
        // base_start=1 <= total_base_lines=10 → insertion within file.
        assert_eq!(classify_region(&region, 10), ConflictClass::BothInserted);
    }

    #[test]
    fn test_classify_trailing_replacement_is_both_modified() {
        let region = ConflictRegion {
            base_start: 2,
            base_lines: vec![],
            left_lines: vec!["LEFT".to_string()],
            right_lines: vec!["RIGHT".to_string()],
        };
        // base_start=2 > total_base_lines=1 → trailing after base deleted.
        assert_eq!(classify_region(&region, 1), ConflictClass::BothModified);
    }

    #[test]
    fn test_classify_delete_vs_modify_left() {
        let region = ConflictRegion {
            base_start: 2,
            base_lines: vec!["original".to_string()],
            left_lines: vec!["modified".to_string()],
            right_lines: vec![],
        };
        assert_eq!(
            classify_region(&region, 5),
            ConflictClass::DeleteVsModify {
                modifier: MergeSide::Left
            }
        );
    }

    #[test]
    fn test_classify_delete_vs_modify_right() {
        let region = ConflictRegion {
            base_start: 2,
            base_lines: vec!["original".to_string()],
            left_lines: vec![],
            right_lines: vec!["modified".to_string()],
        };
        assert_eq!(
            classify_region(&region, 5),
            ConflictClass::DeleteVsModify {
                modifier: MergeSide::Right
            }
        );
    }

    #[test]
    fn test_classify_both_modified() {
        let region = ConflictRegion {
            base_start: 2,
            base_lines: vec!["original".to_string()],
            left_lines: vec!["left_version".to_string()],
            right_lines: vec!["right_version".to_string()],
        };
        assert_eq!(classify_region(&region, 5), ConflictClass::BothModified);
    }

    #[test]
    fn test_resolve_both_inserted_concatenates() {
        // left_spec="api-dev" < right_spec="auth-dev", so left goes first.
        let base = "";
        let left = "import flask\nimport os";
        let right = "import auth\nimport db";

        let regions = match three_way_merge(base, left, right) {
            FileMergeResult::Conflict(r) => r,
            _ => panic!("expected conflict"),
        };

        let resolved = resolve_conflict_regions(
            "test.py", base, left, right, &regions, "api-dev", "auth-dev",
        );
        assert!(resolved.fully_resolved);
        assert!(resolved.content.contains("import flask"));
        assert!(resolved.content.contains("import auth"));

        // Verify ordering: left content (api-dev) before right (auth-dev).
        let flask_pos = resolved.content.find("import flask").unwrap();
        let auth_pos = resolved.content.find("import auth").unwrap();
        assert!(flask_pos < auth_pos);
    }

    #[test]
    fn test_resolve_both_inserted_commutativity() {
        let base = "";
        let left = "import flask";
        let right = "import auth";

        let regions_lr = match three_way_merge(base, left, right) {
            FileMergeResult::Conflict(r) => r,
            _ => panic!("expected conflict"),
        };
        let regions_rl = match three_way_merge(base, right, left) {
            FileMergeResult::Conflict(r) => r,
            _ => panic!("expected conflict"),
        };

        // Both orderings should produce the same content because
        // ordering is by spec ID, not left/right position.
        let result_lr =
            resolve_conflict_regions("test.py", base, left, right, &regions_lr, "alpha", "beta");
        let result_rl =
            resolve_conflict_regions("test.py", base, right, left, &regions_rl, "beta", "alpha");

        assert_eq!(result_lr.content, result_rl.content);
    }

    #[test]
    fn test_resolve_delete_vs_modify_escalates() {
        let base = "line1\nline2\nline3";
        let left = "line1\nline3"; // deleted line2
        let right = "line1\nMODIFIED\nline3"; // modified line2

        let regions = match three_way_merge(base, left, right) {
            FileMergeResult::Conflict(r) => r,
            _ => panic!("expected conflict"),
        };

        let resolved = resolve_conflict_regions("test.txt", base, left, right, &regions, "a", "b");
        // v2.0 spec: DeleteVsModify always escalates.
        assert!(
            !resolved.fully_resolved,
            "DeleteVsModify should not be auto-resolved"
        );
        assert!(
            !resolved.escalation_records.is_empty(),
            "Should have an escalation record"
        );
        assert!(
            resolved.escalation_records[0]
                .reason
                .contains("DeleteVsModify"),
            "Escalation should reference DeleteVsModify"
        );
    }

    #[test]
    fn test_default_strategy_is_escalate() {
        assert_eq!(ConvergeStrategy::default(), ConvergeStrategy::Escalate);
    }

    #[test]
    fn test_resolve_superset_ordered() {
        // Right is a strict ordered superset of left.
        let region = ConflictRegion {
            base_start: 2,
            base_lines: vec!["old".to_string()],
            left_lines: vec!["a".to_string(), "b".to_string()],
            right_lines: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        let result = detect_superset(&region.left_lines, &region.right_lines);
        assert_eq!(result, Some(MergeSide::Right));

        // Out-of-order should NOT be a superset.
        let region2 = ConflictRegion {
            base_start: 2,
            base_lines: vec!["old".to_string()],
            left_lines: vec!["b".to_string(), "a".to_string()],
            right_lines: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        let result2 = detect_superset(&region2.left_lines, &region2.right_lines);
        assert_eq!(result2, None);
    }

    #[test]
    fn test_resolve_import_accumulation_python() {
        let base = "import os\n\ndef main():\n    pass";
        let left = "import os\nimport sys\n\ndef main():\n    pass";
        let right = "import os\nimport json\n\ndef main():\n    pass";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        match &result {
            SmartMergeResult::Clean {
                content,
                resolutions,
            } => {
                assert!(content.contains("import os"));
                assert!(content.contains("import sys"));
                assert!(content.contains("import json"));
                let import_res = resolutions
                    .iter()
                    .find(|r| r.method == "import-accumulation");
                assert!(
                    import_res.is_some(),
                    "should have import-accumulation resolution"
                );
            }
            SmartMergeResult::Partial { .. } => {
                panic!("expected fully resolved import merge");
            }
        }
    }

    #[test]
    fn test_resolve_import_accumulation_js() {
        let base = "import React from 'react';\n\nfunction App() {}";
        let left = "import React from 'react';\nimport axios from 'axios';\n\nfunction App() {}";
        let right = "import React from 'react';\nimport lodash from 'lodash';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        match &result {
            SmartMergeResult::Clean { content, .. } => {
                assert!(content.contains("axios"));
                assert!(content.contains("lodash"));
            }
            SmartMergeResult::Partial { .. } => {
                panic!("expected fully resolved JS import merge");
            }
        }
    }

    #[test]
    fn test_resolve_import_accumulation_rust() {
        let base = "use std::io;\n\nfn main() {}";
        let left = "use std::io;\nuse std::fs;\n\nfn main() {}";
        let right = "use std::io;\nuse std::collections::HashMap;\n\nfn main() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        match &result {
            SmartMergeResult::Clean { content, .. } => {
                assert!(content.contains("use std::fs"));
                assert!(content.contains("use std::collections::HashMap"));
            }
            SmartMergeResult::Partial { .. } => {
                panic!("expected fully resolved Rust import merge");
            }
        }
    }

    #[test]
    fn test_resolve_decorator_preservation() {
        let base = "line1\n@app.route('/api')\ndef handler():\n    pass\nline5";
        let left = "line1\n@app.route('/api')\n@require_auth\ndef handler():\n    pass\nline5";
        let right = "line1\n@app.route('/api')\n@rate_limit(100)\ndef handler():\n    pass\nline5";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        match &result {
            SmartMergeResult::Clean { content, .. } | SmartMergeResult::Partial { content, .. } => {
                assert!(
                    content.contains("@require_auth"),
                    "require_auth decorator lost: {content}"
                );
                assert!(
                    content.contains("@rate_limit(100)"),
                    "rate_limit decorator lost: {content}"
                );
                assert!(
                    content.contains("@app.route('/api')"),
                    "app.route decorator lost: {content}"
                );
                assert!(
                    content.contains("def handler():"),
                    "handler declaration lost: {content}"
                );
            }
        }
    }

    #[test]
    fn test_smart_merge_fully_resolves() {
        // Both sides insert different content from empty base → BothInserted.
        let base = "";
        let left = "import os";
        let right = "import sys";

        let result = smart_merge("test.txt", base, left, right, "alpha", "beta");
        match result {
            SmartMergeResult::Clean {
                content,
                resolutions,
            } => {
                assert!(content.contains("import os"));
                assert!(content.contains("import sys"));
                assert_eq!(resolutions.len(), 1);
                assert_eq!(resolutions[0].method, "import-accumulation");
            }
            SmartMergeResult::Partial { .. } => panic!("expected fully resolved"),
        }
    }

    #[test]
    fn test_smart_merge_partial_resolution() {
        // One region resolvable (BothInserted imports), one not (BothModified function body).
        let base = "import os\ndef work():\n    pass";
        let left = "import os\nimport sys\ndef work():\n    do_left()";
        let right = "import os\nimport json\ndef work():\n    do_right()";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        // The function body change is BothModified and can't be pattern-matched.
        match &result {
            SmartMergeResult::Partial {
                resolutions,
                unresolved,
                ..
            } => {
                let resolved_count = resolutions
                    .iter()
                    .filter(|r| r.method != "unresolved")
                    .count();
                assert!(
                    resolved_count > 0,
                    "should have at least one resolved region"
                );
                assert!(!unresolved.is_empty(), "should have unresolved regions");
            }
            SmartMergeResult::Clean { .. } => {
                // Import accumulation might resolve the import conflict, and
                // superset detection or another heuristic might resolve the body.
                // Either outcome is acceptable as long as no content is lost.
            }
        }
    }

    #[test]
    fn test_smart_merge_tr13_regression() {
        // TR13 scenario: api-dev and auth-dev both modified app.py.
        // Old behavior: one agent's entire file was chosen, other's deleted.
        // New behavior: both contributions should be preserved.
        let base = "\
from flask import Flask

app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

if __name__ == '__main__':
    app.run()";

        let left = "\
from flask import Flask
import requests

app = Flask(__name__)

@app.route('/')
def index():
    return 'Hello'

@app.route('/api/data')
def api_data():
    return {'status': 'ok'}

if __name__ == '__main__':
    app.run()";

        let right = "\
from flask import Flask
from auth import require_auth

app = Flask(__name__)

@app.route('/')
@require_auth
def index():
    return 'Hello'

if __name__ == '__main__':
    app.run()";

        let result = smart_merge("test.txt", base, left, right, "api-dev", "auth-dev");

        // Critical: both agents' work must be present.
        match &result {
            SmartMergeResult::Clean { content, .. } => {
                assert!(
                    content.contains("import requests"),
                    "api-dev import lost: {content}"
                );
                assert!(
                    content.contains("from auth import require_auth"),
                    "auth-dev import lost: {content}"
                );
                assert!(
                    content.contains("api_data"),
                    "api-dev route lost: {content}"
                );
                assert!(
                    content.contains("@require_auth"),
                    "auth-dev decorator lost: {content}"
                );
            }
            SmartMergeResult::Partial {
                content,
                unresolved,
                ..
            } => {
                // Even partial resolution should preserve both agents' work
                // in the best-effort output.
                assert!(
                    content.contains("import requests") || content.contains("api_data"),
                    "api-dev contribution completely lost (TR13 regression): {content}"
                );
                assert!(
                    content.contains("require_auth"),
                    "auth-dev contribution completely lost (TR13 regression): {content}"
                );
                // Unresolved regions should be small, not entire files.
                for region in unresolved {
                    assert!(
                        region.left_lines.len() < 15 && region.right_lines.len() < 15,
                        "unresolved region too large — suggests whole-file fallback"
                    );
                }
            }
        }
    }

    #[test]
    fn test_smart_merge_confidence_scoring() {
        // BothInserted → 0.9
        {
            let regions = vec![ConflictRegion {
                base_start: 1,
                base_lines: vec![],
                left_lines: vec!["a".to_string()],
                right_lines: vec!["b".to_string()],
            }];
            let resolved = resolve_conflict_regions("test.txt", "", "a", "b", &regions, "x", "y");
            assert!((resolved.resolutions[0].confidence - 0.9).abs() < 0.01);
            assert_eq!(resolved.resolutions[0].method, "concatenated");
        }

        // DeleteVsModify → escalated (0.0 confidence, per v2.0 spec)
        {
            let regions = vec![ConflictRegion {
                base_start: 2,
                base_lines: vec!["original".to_string()],
                left_lines: vec![],
                right_lines: vec!["modified".to_string()],
            }];
            let resolved = resolve_conflict_regions(
                "test.txt",
                "line1\noriginal\nline3",
                "line1\nline3",
                "line1\nmodified\nline3",
                &regions,
                "x",
                "y",
            );
            assert!(
                (resolved.resolutions[0].confidence - 0.0).abs() < 0.01,
                "DeleteVsModify should be escalated with 0.0 confidence"
            );
            assert!(
                resolved.resolutions[0].method.contains("escalated"),
                "Method should indicate escalation"
            );
        }

        // Superset → 0.95
        {
            let regions = vec![ConflictRegion {
                base_start: 2,
                base_lines: vec!["old".to_string()],
                left_lines: vec!["a".to_string()],
                right_lines: vec!["a".to_string(), "b".to_string()],
            }];
            let resolved = resolve_conflict_regions(
                "test.txt",
                "line1\nold\nline3",
                "line1\na\nline3",
                "line1\na\nb\nline3",
                &regions,
                "x",
                "y",
            );
            assert!((resolved.resolutions[0].confidence - 0.95).abs() < 0.01);
            assert_eq!(resolved.resolutions[0].method, "superset-detected");
        }
    }

    #[test]
    fn test_css_bracket_preservation() {
        let base = "\
.app {
  font-family: sans-serif;
  padding: 20px;
}

h1 {
  color: #333;
}
";
        let left = "\
.app {
  font-family: sans-serif;
  padding: 20px;
}

h1 {
  color: #333;
}

.header {
  display: flex;
  background: #1a1a2e;
}

.user-avatar {
  width: 40px;
  border-radius: 50%;
}
";
        let right = "\
.app {
  font-family: sans-serif;
  padding: 20px;
}

h1 {
  color: #333;
}

.sidebar {
  width: 220px;
  background: #1a1a2e;
}

.sidebar a {
  color: #b0b0c0;
  text-decoration: none;
}
";
        let result = smart_merge("test.txt", base, left, right, "header", "sidebar");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content,
            SmartMergeResult::Partial { content, .. } => content,
        };

        let opens = content.matches('{').count();
        let closes = content.matches('}').count();
        assert_eq!(
            opens, closes,
            "CSS brackets unbalanced: {} opens, {} closes\nmerged:\n{}",
            opens, closes, content
        );

        assert!(content.contains(".header"), "header styles lost");
        assert!(content.contains(".sidebar"), "sidebar styles lost");
        assert!(content.contains(".user-avatar"), "user-avatar styles lost");
        assert!(content.contains(".sidebar a"), "sidebar a styles lost");
    }

    // -----------------------------------------------------------------------
    // JS/TS import merge tests (Sprint 13 — Amis)
    // -----------------------------------------------------------------------

    #[test]
    fn test_import_accumulation_js_named() {
        // Two agents add different named imports from the same module.
        // Should consolidate into a single import statement.
        let base = "import { useState } from 'react';\n\nfunction App() { return null; }";
        let left =
            "import { useState, useCallback } from 'react';\n\nfunction App() { return null; }";
        let right =
            "import { useState, useEffect } from 'react';\n\nfunction App() { return null; }";

        let result = smart_merge("test.txt", base, left, right, "agent-a", "agent-b");
        match &result {
            SmartMergeResult::Clean {
                content,
                resolutions,
            } => {
                // All three named imports should be consolidated into one statement.
                assert!(
                    content.contains("useCallback"),
                    "useCallback lost: {content}"
                );
                assert!(content.contains("useEffect"), "useEffect lost: {content}");
                assert!(content.contains("useState"), "useState lost: {content}");
                // Should be a single import from 'react', not two separate ones.
                assert_eq!(
                    content.matches("from 'react'").count(),
                    1,
                    "expected one consolidated import from 'react', got:\n{content}"
                );
                let import_res = resolutions
                    .iter()
                    .find(|r| r.method == "import-accumulation");
                assert!(import_res.is_some(), "should use import-accumulation");
            }
            SmartMergeResult::Partial { .. } => {
                panic!("expected fully resolved JS named import merge");
            }
        }
    }

    #[test]
    fn test_import_accumulation_js_default() {
        // Two agents add different default imports from different modules.
        let base = "import React from 'react';\n\nexport default function App() {}";
        let left = "import React from 'react';\nimport axios from 'axios';\n\nexport default function App() {}";
        let right = "import React from 'react';\nimport lodash from 'lodash';\n\nexport default function App() {}";

        let result = smart_merge("test.txt", base, left, right, "api-agent", "util-agent");
        match &result {
            SmartMergeResult::Clean { content, .. } => {
                assert!(content.contains("axios"), "axios import lost: {content}");
                assert!(content.contains("lodash"), "lodash import lost: {content}");
                assert!(content.contains("React"), "React import lost: {content}");
                // Each from a different source — should remain separate imports.
                assert!(
                    content.contains("from 'axios'"),
                    "axios source lost: {content}"
                );
                assert!(
                    content.contains("from 'lodash'"),
                    "lodash source lost: {content}"
                );
            }
            SmartMergeResult::Partial { .. } => {
                panic!("expected fully resolved JS default import merge");
            }
        }
    }

    #[test]
    fn test_import_accumulation_js_multiline() {
        // Multi-line import blocks should be recognized and merged.
        let base = "import {\n  useState\n} from 'react';\n\nfunction App() {}";
        let left = "import {\n  useState,\n  useCallback\n} from 'react';\n\nfunction App() {}";
        let right = "import {\n  useState,\n  useEffect\n} from 'react';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "agent-a", "agent-b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(
            content.contains("useCallback"),
            "useCallback lost in multiline merge: {content}"
        );
        assert!(
            content.contains("useEffect"),
            "useEffect lost in multiline merge: {content}"
        );
        assert!(
            content.contains("useState"),
            "useState lost in multiline merge: {content}"
        );
    }

    #[test]
    fn test_import_dedup_same_source() {
        // Both agents add the exact same import — should be deduped.
        let base = "import React from 'react';\n\nfunction App() {}";
        let left =
            "import React from 'react';\nimport { useState } from 'react';\n\nfunction App() {}";
        let right =
            "import React from 'react';\nimport { useState } from 'react';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        // useState should appear exactly once (deduped).
        assert_eq!(
            content.matches("useState").count(),
            1,
            "useState should be deduped, got:\n{content}"
        );
    }

    #[test]
    fn test_import_accumulation_js_type_imports() {
        // Type imports and value imports from the same source stay separate.
        let base = "import { useState } from 'react';\n\nfunction App() {}";
        let left = "import { useState } from 'react';\nimport type { FC } from 'react';\n\nfunction App() {}";
        let right = "import { useState, useEffect } from 'react';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("useEffect"), "useEffect lost: {content}");
        assert!(content.contains("type"), "type import lost: {content}");
        assert!(content.contains("FC"), "FC type import lost: {content}");
    }

    #[test]
    fn test_import_accumulation_js_require() {
        // require() calls can't be consolidated by source — just deduped.
        let base = "const React = require('react');\n\nmodule.exports = {};";
        let left = "const React = require('react');\nconst axios = require('axios');\n\nmodule.exports = {};";
        let right = "const React = require('react');\nconst lodash = require('lodash');\n\nmodule.exports = {};";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("axios"), "axios require lost: {content}");
        assert!(content.contains("lodash"), "lodash require lost: {content}");
    }

    #[test]
    fn test_import_accumulation_js_mixed_default_and_named() {
        // One agent adds a default import, the other adds named imports
        // from the same source. Should consolidate.
        let base = "\nfunction App() {}";
        let left = "import React from 'react';\n\nfunction App() {}";
        let right = "import { useState } from 'react';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("React"), "default import lost: {content}");
        assert!(content.contains("useState"), "named import lost: {content}");
        // Should consolidate into one import from 'react'.
        assert_eq!(
            content.matches("from 'react'").count(),
            1,
            "expected one consolidated import from 'react', got:\n{content}"
        );
    }

    #[test]
    fn test_import_accumulation_js_namespace() {
        // Namespace imports should be preserved.
        let base = "\nfunction App() {}";
        let left = "import * as React from 'react';\n\nfunction App() {}";
        let right = "import axios from 'axios';\n\nfunction App() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(
            content.contains("* as React"),
            "namespace import lost: {content}"
        );
        assert!(content.contains("axios"), "axios import lost: {content}");
    }

    #[test]
    fn test_import_accumulation_js_side_effect() {
        // Side-effect imports should be preserved.
        let base = "\nfunction setup() {}";
        let left = "import 'polyfill';\n\nfunction setup() {}";
        let right = "import 'normalize.css';\n\nfunction setup() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(
            content.contains("polyfill"),
            "polyfill side-effect import lost: {content}"
        );
        assert!(
            content.contains("normalize.css"),
            "normalize.css side-effect import lost: {content}"
        );
    }

    #[test]
    fn test_import_region_multiline_detection() {
        // is_import_region should recognize multi-line JS imports.
        let lines = vec![
            "import {".to_string(),
            "  useState,".to_string(),
            "  useEffect".to_string(),
            "} from 'react';".to_string(),
        ];
        assert!(
            is_import_region(&lines),
            "multi-line JS import not detected as import region"
        );

        // Go import block.
        let go_lines = vec![
            "import (".to_string(),
            "  \"fmt\"".to_string(),
            "  \"os\"".to_string(),
            ")".to_string(),
        ];
        assert!(
            is_import_region(&go_lines),
            "Go import block not detected as import region"
        );

        // Rust multi-line use.
        let rust_lines = vec![
            "use std::{".to_string(),
            "    io,".to_string(),
            "    fs,".to_string(),
            "};".to_string(),
        ];
        assert!(
            is_import_region(&rust_lines),
            "Rust multi-line use not detected as import region"
        );

        // Non-import content should fail.
        let non_import = vec![
            "fn main() {".to_string(),
            "    println!(\"hello\");".to_string(),
            "}".to_string(),
        ];
        assert!(
            !is_import_region(&non_import),
            "non-import code falsely detected as import region"
        );
    }

    #[test]
    fn test_join_multiline_imports() {
        let lines = vec![
            "import {".to_string(),
            "  useState,".to_string(),
            "  useEffect,".to_string(),
            "} from 'react';".to_string(),
            "import axios from 'axios';".to_string(),
        ];
        let joined = join_multiline_imports(&lines);
        assert_eq!(joined.len(), 2);
        assert!(joined[0].contains("useState"));
        assert!(joined[0].contains("useEffect"));
        assert!(joined[0].contains("from 'react'"));
        assert_eq!(joined[1], "import axios from 'axios';");
    }

    #[test]
    fn test_parse_js_import_named() {
        let import = parse_js_import("import { useState, useEffect } from 'react';").unwrap();
        assert_eq!(import.source, "'react'");
        assert!(import.named_imports.contains(&"useState".to_string()));
        assert!(import.named_imports.contains(&"useEffect".to_string()));
        assert!(import.default_import.is_none());
        assert!(!import.is_type);
    }

    #[test]
    fn test_parse_js_import_default() {
        let import = parse_js_import("import React from 'react';").unwrap();
        assert_eq!(import.source, "'react'");
        assert_eq!(import.default_import, Some("React".to_string()));
        assert!(import.named_imports.is_empty());
    }

    #[test]
    fn test_parse_js_import_default_plus_named() {
        let import =
            parse_js_import("import React, { useState, useEffect } from 'react';").unwrap();
        assert_eq!(import.source, "'react'");
        assert_eq!(import.default_import, Some("React".to_string()));
        assert!(import.named_imports.contains(&"useState".to_string()));
        assert!(import.named_imports.contains(&"useEffect".to_string()));
    }

    #[test]
    fn test_parse_js_import_type() {
        let import = parse_js_import("import type { FC, ReactNode } from 'react';").unwrap();
        assert_eq!(import.source, "'react'");
        assert!(import.is_type);
        assert!(import.named_imports.contains(&"FC".to_string()));
        assert!(import.named_imports.contains(&"ReactNode".to_string()));
    }

    #[test]
    fn test_parse_js_import_side_effect() {
        let import = parse_js_import("import 'polyfill';").unwrap();
        assert_eq!(import.source, "'polyfill'");
        assert!(import.is_side_effect);
    }

    #[test]
    fn test_parse_js_import_namespace() {
        let import = parse_js_import("import * as React from 'react';").unwrap();
        assert_eq!(import.source, "'react'");
        assert_eq!(import.namespace, Some("React".to_string()));
    }

    #[test]
    fn test_parse_js_import_reexport() {
        let import = parse_js_import("export { Button, Input } from './components';").unwrap();
        assert_eq!(import.source, "'./components'");
        assert!(import.is_export);
        assert!(import.named_imports.contains(&"Button".to_string()));
        assert!(import.named_imports.contains(&"Input".to_string()));
    }

    #[test]
    fn test_detect_import_language() {
        let js = vec!["import React from 'react';".to_string()];
        assert_eq!(detect_import_language(&js), ImportLanguage::JavaScript);

        let py = vec!["from flask import Flask".to_string()];
        assert_eq!(detect_import_language(&py), ImportLanguage::Python);

        let rs = vec!["use std::io;".to_string()];
        assert_eq!(detect_import_language(&rs), ImportLanguage::Rust);

        let go = vec!["import \"fmt\"".to_string()];
        assert_eq!(detect_import_language(&go), ImportLanguage::Go);

        let cpp = vec!["#include <stdio.h>".to_string()];
        assert_eq!(detect_import_language(&cpp), ImportLanguage::Cpp);
    }

    #[test]
    fn test_merge_js_imports_consolidation() {
        // Direct unit test for merge_js_imports: same source → consolidated.
        let left = vec![
            "import { useState } from 'react';".to_string(),
            "import axios from 'axios';".to_string(),
        ];
        let right = vec![
            "import { useEffect } from 'react';".to_string(),
            "import lodash from 'lodash';".to_string(),
        ];
        let merged = merge_js_imports(&left, &right);

        // Should have 3 imports: consolidated react, axios, lodash
        assert_eq!(
            merged.len(),
            3,
            "expected 3 merged imports, got: {merged:?}"
        );

        let react_import = merged.iter().find(|l| l.contains("'react'")).unwrap();
        assert!(
            react_import.contains("useEffect"),
            "useEffect missing from consolidated: {react_import}"
        );
        assert!(
            react_import.contains("useState"),
            "useState missing from consolidated: {react_import}"
        );
    }

    // -----------------------------------------------------------------------
    // Rust merge pattern tests (Sprint 13 — Amis)
    // -----------------------------------------------------------------------

    #[test]
    fn test_rust_derive_accumulation() {
        // Two agents add different derives to the same struct.
        let base = "#[derive(Debug, Clone)]\npub struct Config {\n    pub name: String,\n}";
        let left =
            "#[derive(Debug, Clone, Serialize)]\npub struct Config {\n    pub name: String,\n}";
        let right =
            "#[derive(Debug, Clone, Deserialize)]\npub struct Config {\n    pub name: String,\n}";

        let result = smart_merge("test.txt", base, left, right, "agent-a", "agent-b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(
            content.contains("Serialize"),
            "Serialize derive lost: {content}"
        );
        assert!(
            content.contains("Deserialize"),
            "Deserialize derive lost: {content}"
        );
        assert!(content.contains("Debug"), "Debug derive lost: {content}");
        assert!(content.contains("Clone"), "Clone derive lost: {content}");
        // Should be consolidated into a single #[derive(...)].
        assert_eq!(
            content.matches("#[derive(").count(),
            1,
            "expected one consolidated derive, got:\n{content}"
        );
    }

    #[test]
    fn test_rust_derive_with_other_attributes() {
        // Derives should consolidate, but other attributes should be kept separate.
        let base = "#[derive(Debug)]\npub struct Foo {}";
        let left =
            "#[derive(Debug, Clone)]\n#[serde(rename_all = \"camelCase\")]\npub struct Foo {}";
        let right = "#[derive(Debug, Serialize)]\npub struct Foo {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("Clone"), "Clone lost: {content}");
        assert!(content.contains("Serialize"), "Serialize lost: {content}");
        assert!(
            content.contains("serde(rename_all"),
            "serde attribute lost: {content}"
        );
        // One consolidated derive + serde attribute.
        assert_eq!(
            content.matches("#[derive(").count(),
            1,
            "derives not consolidated: {content}"
        );
    }

    #[test]
    fn test_rust_multiline_use_import_region() {
        // Multi-line `use` blocks should be detected and merged.
        let base = "use std::io;\n\nfn main() {}";
        let left = "use std::io;\nuse std::{\n    fs,\n    path::PathBuf,\n};\n\nfn main() {}";
        let right = "use std::io;\nuse std::collections::HashMap;\n\nfn main() {}";

        let result = smart_merge("test.txt", base, left, right, "a", "b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("fs"), "fs import lost: {content}");
        assert!(
            content.contains("PathBuf"),
            "PathBuf import lost: {content}"
        );
        assert!(
            content.contains("HashMap"),
            "HashMap import lost: {content}"
        );
    }

    #[test]
    fn test_consolidate_rust_derives_unit() {
        // Direct unit test for consolidate_rust_derives.
        let decorators = vec![
            "#[derive(Debug, Clone, Serialize)]".to_string(),
            "#[derive(Debug, Clone, Deserialize)]".to_string(),
            "#[serde(rename_all = \"camelCase\")]".to_string(),
        ];
        let result = consolidate_rust_derives(&decorators);
        assert_eq!(result.len(), 2, "expected derive + serde, got: {result:?}");
        assert!(result[0].starts_with("#[derive("));
        assert!(result[0].contains("Serialize"));
        assert!(result[0].contains("Deserialize"));
        assert!(result[0].contains("Clone"));
        assert!(result[0].contains("Debug"));
        // Should be sorted alphabetically.
        let derive_line = &result[0];
        let clone_pos = derive_line.find("Clone").unwrap();
        let debug_pos = derive_line.find("Debug").unwrap();
        let deser_pos = derive_line.find("Deserialize").unwrap();
        let ser_pos = derive_line.find("Serialize").unwrap();
        assert!(clone_pos < debug_pos);
        assert!(debug_pos < deser_pos);
        assert!(deser_pos < ser_pos);
    }

    // -----------------------------------------------------------------------
    // Go merge pattern tests (Sprint 13 — Amis)
    // -----------------------------------------------------------------------

    #[test]
    fn test_go_import_block_merge() {
        // Two agents add different imports to a Go file with import blocks.
        let base = "package main\n\nimport (\n\t\"fmt\"\n)\n\nfunc main() {}";
        let left = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc main() {}";
        let right = "package main\n\nimport (\n\t\"fmt\"\n\t\"strings\"\n)\n\nfunc main() {}";

        let result = smart_merge("test.txt", base, left, right, "agent-a", "agent-b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        assert!(content.contains("fmt"), "fmt import lost: {content}");
        assert!(content.contains("os"), "os import lost: {content}");
        assert!(
            content.contains("strings"),
            "strings import lost: {content}"
        );
    }

    #[test]
    fn test_go_import_grouped_sorting() {
        // Go imports should group stdlib first, then third-party.
        let left = vec![
            "import (".to_string(),
            "\t\"fmt\"".to_string(),
            "\t\"github.com/pkg/errors\"".to_string(),
            ")".to_string(),
        ];
        let right = vec![
            "import (".to_string(),
            "\t\"os\"".to_string(),
            "\t\"github.com/stretchr/testify\"".to_string(),
            ")".to_string(),
        ];

        let merged = merge_go_imports(&left, &right);

        let content = merged.join("\n");
        assert!(content.contains("\"fmt\""), "fmt lost: {content}");
        assert!(content.contains("\"os\""), "os lost: {content}");
        assert!(
            content.contains("github.com/pkg/errors"),
            "errors lost: {content}"
        );
        assert!(
            content.contains("github.com/stretchr/testify"),
            "testify lost: {content}"
        );

        // Stdlib should come before third-party.
        let fmt_pos = content.find("\"fmt\"").unwrap();
        let errors_pos = content.find("github.com/pkg/errors").unwrap();
        assert!(
            fmt_pos < errors_pos,
            "stdlib should precede third-party: {content}"
        );
    }

    #[test]
    fn test_go_import_dedup() {
        let left = vec![
            "import (".to_string(),
            "\t\"fmt\"".to_string(),
            "\t\"os\"".to_string(),
            ")".to_string(),
        ];
        let right = vec![
            "import (".to_string(),
            "\t\"fmt\"".to_string(),
            "\t\"strings\"".to_string(),
            ")".to_string(),
        ];

        let merged = merge_go_imports(&left, &right);
        let content = merged.join("\n");

        assert_eq!(
            content.matches("\"fmt\"").count(),
            1,
            "fmt should be deduped: {content}"
        );
        assert!(content.contains("\"os\""), "os lost: {content}");
        assert!(content.contains("\"strings\""), "strings lost: {content}");
    }

    #[test]
    fn test_go_single_import_merge() {
        let left = vec!["import \"fmt\"".to_string()];
        let right = vec!["import \"os\"".to_string()];

        let merged = merge_go_imports(&left, &right);
        let content = merged.join("\n");
        assert!(content.contains("\"fmt\""), "fmt lost: {content}");
        assert!(content.contains("\"os\""), "os lost: {content}");
        assert!(
            content.contains("import ("),
            "should use block syntax for multiple imports: {content}"
        );
    }

    #[test]
    fn test_is_go_stdlib() {
        assert!(is_go_stdlib("\"fmt\""));
        assert!(is_go_stdlib("\"os\""));
        assert!(is_go_stdlib("\"net/http\""));
        assert!(!is_go_stdlib("\"github.com/pkg/errors\""));
        assert!(!is_go_stdlib("\"golang.org/x/crypto\""));
    }

    #[test]
    fn test_extract_go_imports() {
        let lines = vec![
            "import (".to_string(),
            "\t\"fmt\"".to_string(),
            "\t\"os\"".to_string(),
            "".to_string(),
            "\t\"github.com/pkg/errors\"".to_string(),
            ")".to_string(),
        ];
        let imports = extract_go_imports(&lines);
        assert_eq!(imports.len(), 3);
        assert!(imports.contains(&"\"fmt\"".to_string()));
        assert!(imports.contains(&"\"os\"".to_string()));
        assert!(imports.contains(&"\"github.com/pkg/errors\"".to_string()));
    }

    // -----------------------------------------------------------------------
    // Python merge pattern tests (Sprint 14 — Amis)
    // -----------------------------------------------------------------------

    #[test]
    fn test_python_import_consolidation_same_module() {
        // `from models import Book` + `from models import User` → `from models import Book, User`
        let left = vec!["from models import Book".to_string()];
        let right = vec!["from models import User".to_string()];

        let merged = merge_python_imports(&left, &right);
        assert_eq!(
            merged.len(),
            1,
            "should consolidate into one line: {merged:?}"
        );
        assert!(merged[0].contains("Book"), "Book lost: {merged:?}");
        assert!(merged[0].contains("User"), "User lost: {merged:?}");
        assert!(
            merged[0].starts_with("from models import"),
            "wrong module: {merged:?}"
        );
    }

    #[test]
    fn test_python_import_consolidation_multiple_names() {
        // `from models import Book, BookCollection` + `from models import User, Book`
        // → `from models import Book, BookCollection, User`
        let left = vec!["from models import Book, BookCollection".to_string()];
        let right = vec!["from models import User, Book".to_string()];

        let merged = merge_python_imports(&left, &right);
        assert_eq!(merged.len(), 1, "should consolidate: {merged:?}");
        assert!(merged[0].contains("Book"), "Book lost");
        assert!(merged[0].contains("BookCollection"), "BookCollection lost");
        assert!(merged[0].contains("User"), "User lost");
        // Check dedup — Book should appear once.
        let book_count = merged[0].matches("Book").count();
        // "Book" appears in "Book" and "BookCollection", so 2 occurrences.
        assert_eq!(
            book_count, 2,
            "Book should appear in 'Book' and 'BookCollection': {}",
            merged[0]
        );
    }

    #[test]
    fn test_python_import_ordering_pep8() {
        // Imports should be ordered: stdlib → third-party → local.
        let left = vec![
            "from flask import Flask".to_string(),
            "import os".to_string(),
        ];
        let right = vec![
            "from .utils import helper".to_string(),
            "import sys".to_string(),
        ];

        let merged = merge_python_imports(&left, &right);
        let content = merged.join("\n");

        // stdlib (os, sys) should come before flask, which should come before .utils.
        let os_pos = content.find("import os").unwrap();
        let sys_pos = content.find("import sys").unwrap();
        let flask_pos = content.find("from flask").unwrap();
        let utils_pos = content.find("from .utils").unwrap();

        assert!(
            os_pos < flask_pos,
            "stdlib should precede third-party: {content}"
        );
        assert!(
            sys_pos < flask_pos,
            "stdlib should precede third-party: {content}"
        );
        assert!(
            flask_pos < utils_pos,
            "third-party should precede local: {content}"
        );
    }

    #[test]
    fn test_python_import_ordering_blank_lines_between_groups() {
        let left = vec![
            "import os".to_string(),
            "from flask import Flask".to_string(),
        ];
        let right = vec![
            "import sys".to_string(),
            "from .models import User".to_string(),
        ];

        let merged = merge_python_imports(&left, &right);

        // Should have blank lines between groups.
        // Group 1 (stdlib): import os, import sys
        // Group 2 (third-party): from flask import Flask
        // Group 3 (local): from .models import User
        let mut found_blank_lines = 0;
        for line in &merged {
            if line.is_empty() {
                found_blank_lines += 1;
            }
        }
        assert!(
            found_blank_lines >= 2,
            "need blank lines between 3 groups, found {found_blank_lines}: {merged:?}"
        );
    }

    #[test]
    fn test_python_import_dedup_identical() {
        // Two identical imports → one.
        let left = vec!["from os.path import join".to_string()];
        let right = vec!["from os.path import join".to_string()];

        let merged = merge_python_imports(&left, &right);
        assert_eq!(merged.len(), 1, "duplicate should be deduped: {merged:?}");
        assert_eq!(merged[0], "from os.path import join");
    }

    #[test]
    fn test_python_bare_import_preserved() {
        // `import os` + `import sys` should remain separate lines.
        let left = vec!["import os".to_string()];
        let right = vec!["import sys".to_string()];

        let merged = merge_python_imports(&left, &right);
        assert!(
            merged.contains(&"import os".to_string()),
            "os lost: {merged:?}"
        );
        assert!(
            merged.contains(&"import sys".to_string()),
            "sys lost: {merged:?}"
        );
    }

    #[test]
    fn test_python_star_import_preserved() {
        let left = vec!["from typing import *".to_string()];
        let right = vec!["from collections import OrderedDict".to_string()];

        let merged = merge_python_imports(&left, &right);
        let content = merged.join("\n");
        assert!(
            content.contains("from typing import *"),
            "star import lost: {content}"
        );
        assert!(
            content.contains("from collections import OrderedDict"),
            "collections lost: {content}"
        );
    }

    #[test]
    fn test_python_future_import_first() {
        // __future__ imports should always be first.
        let left = vec![
            "import os".to_string(),
            "from __future__ import annotations".to_string(),
        ];
        let right = vec!["from flask import Flask".to_string()];

        let merged = merge_python_imports(&left, &right);
        let first_non_empty = merged.iter().find(|l| !l.is_empty()).unwrap();
        assert!(
            first_non_empty.contains("__future__"),
            "__future__ should be first: {merged:?}"
        );
    }

    #[test]
    fn test_parse_python_import_bare() {
        let parsed = parse_python_import("import os").unwrap();
        assert!(parsed.is_bare);
        assert_eq!(parsed.module, "os");
        assert!(parsed.names.is_empty());
    }

    #[test]
    fn test_parse_python_import_from() {
        let parsed = parse_python_import("from os.path import join, exists").unwrap();
        assert!(!parsed.is_bare);
        assert_eq!(parsed.module, "os.path");
        assert!(parsed.names.contains(&"join".to_string()));
        assert!(parsed.names.contains(&"exists".to_string()));
    }

    #[test]
    fn test_parse_python_import_parenthesized() {
        let parsed = parse_python_import("from flask import (Flask, jsonify, request)").unwrap();
        assert!(!parsed.is_bare);
        assert_eq!(parsed.module, "flask");
        assert_eq!(parsed.names.len(), 3);
        assert!(parsed.names.contains(&"Flask".to_string()));
        assert!(parsed.names.contains(&"jsonify".to_string()));
        assert!(parsed.names.contains(&"request".to_string()));
    }

    #[test]
    fn test_parse_python_import_star() {
        let parsed = parse_python_import("from typing import *").unwrap();
        assert!(parsed.is_star);
        assert_eq!(parsed.module, "typing");
    }

    #[test]
    fn test_python_import_group_classification() {
        assert_eq!(python_import_group("__future__"), 0);
        assert_eq!(python_import_group("os"), 1);
        assert_eq!(python_import_group("sys"), 1);
        assert_eq!(python_import_group("os.path"), 1);
        assert_eq!(python_import_group("collections"), 1);
        assert_eq!(python_import_group("flask"), 2);
        assert_eq!(python_import_group("requests"), 2);
        assert_eq!(python_import_group("django.db"), 2);
        assert_eq!(python_import_group(".utils"), 3);
        assert_eq!(python_import_group("..models"), 3);
    }

    #[test]
    fn test_python_import_smart_merge_integration() {
        // Full integration: smart_merge on a Python file with import conflict.
        // Symbols are referenced in the body so the pruner keeps them.
        let base = "import os\n\ndef main():\n    app = Flask(__name__)\n    return jsonify({})\n";
        let left = "import os\nfrom flask import Flask\n\ndef main():\n    app = Flask(__name__)\n    return jsonify({})\n";
        let right = "import os\nfrom flask import jsonify\n\ndef main():\n    app = Flask(__name__)\n    return jsonify({})\n";

        let result = smart_merge("test.txt", base, left, right, "agent-a", "agent-b");
        let content = match &result {
            SmartMergeResult::Clean { content, .. } => content.clone(),
            SmartMergeResult::Partial { content, .. } => content.clone(),
        };

        // Both flask imports should be consolidated.
        assert!(content.contains("Flask"), "Flask lost: {content}");
        assert!(content.contains("jsonify"), "jsonify lost: {content}");
        assert!(
            content.contains("def main"),
            "main function lost: {content}"
        );
    }

    // -----------------------------------------------------------------------
    // Post-merge cleanup tests (Sprint 14 — Amis)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dedup_imports_exact_duplicate() {
        // Exact duplicate imports should be removed.
        let content = "import os\nimport sys\nimport os\n\nx = os.path.join('a', 'b')\n";
        let result = dedup_imports_whole_file(content);
        assert_eq!(
            result.matches("import os").count(),
            1,
            "duplicate import os should be deduped: {result}"
        );
        assert!(result.contains("import sys"), "sys lost: {result}");
    }

    #[test]
    fn test_dedup_imports_python_from_consolidation() {
        // Scattered `from X import Y` lines should be consolidated into the first.
        let content =
            "from models import Book\nimport os\nfrom models import User\n\nprint(Book, User)\n";
        let result = dedup_imports_whole_file(content);
        // Should consolidate into one line at the first occurrence.
        assert_eq!(
            result.matches("from models import").count(),
            1,
            "should consolidate scattered from-imports: {result}"
        );
        assert!(result.contains("Book"), "Book lost: {result}");
        assert!(result.contains("User"), "User lost: {result}");
    }

    #[test]
    fn test_dedup_imports_star_duplicate() {
        let content = "from typing import *\nfrom typing import *\n\nx: int = 1\n";
        let result = dedup_imports_whole_file(content);
        assert_eq!(
            result.matches("from typing import *").count(),
            1,
            "duplicate star import should be deduped: {result}"
        );
    }

    #[test]
    fn test_dedup_imports_non_python() {
        // Non-Python imports dedup by exact text.
        let content =
            "import React from 'react';\nimport React from 'react';\n\nconst App = () => {};\n";
        let result = dedup_imports_whole_file(content);
        assert_eq!(
            result.matches("import React from 'react';").count(),
            1,
            "duplicate JS import should be deduped: {result}"
        );
    }

    #[test]
    fn test_prune_unused_imports() {
        // `useCallback` is imported but never referenced in body.
        let content = "from flask import Flask, jsonify\nimport os\n\napp = Flask(__name__)\nresult = jsonify(data)\n";
        let result = prune_unused_python_imports(content);
        assert!(result.contains("Flask"), "Flask is used: {result}");
        assert!(result.contains("jsonify"), "jsonify is used: {result}");
        assert!(
            result.contains("import os"),
            "bare import os should be kept: {result}"
        );
    }

    #[test]
    fn test_prune_unused_imports_removes_unreferenced() {
        let content =
            "from flask import Flask, jsonify, abort\nimport os\n\napp = Flask(__name__)\n";
        let result = prune_unused_python_imports(content);
        assert!(result.contains("Flask"), "Flask is used: {result}");
        // jsonify and abort are not referenced in body.
        assert!(
            !result.contains("jsonify"),
            "jsonify should be pruned: {result}"
        );
        assert!(
            !result.contains("abort"),
            "abort should be pruned: {result}"
        );
        // import os is bare — should be kept (could use os.path etc.).
        assert!(result.contains("import os"), "bare import kept: {result}");
    }

    #[test]
    fn test_prune_unused_keeps_bare_imports() {
        // Bare imports should never be pruned (could use X.method).
        let content = "import os\nimport sys\n\nprint('hello')\n";
        let result = prune_unused_python_imports(content);
        assert!(
            result.contains("import os"),
            "bare import os should be kept: {result}"
        );
        assert!(
            result.contains("import sys"),
            "bare import sys should be kept: {result}"
        );
    }

    #[test]
    fn test_prune_unused_keeps_star_imports() {
        let content = "from typing import *\n\nx: int = 1\n";
        let result = prune_unused_python_imports(content);
        assert!(
            result.contains("from typing import *"),
            "star import should be kept: {result}"
        );
    }

    #[test]
    fn test_prune_drops_entire_unused_from_import() {
        // When ALL names from a from-import are unused, drop the whole line.
        let content = "from unused_module import foo, bar\n\ndef main():\n    print('hello')\n";
        let result = prune_unused_python_imports(content);
        assert!(
            !result.contains("unused_module"),
            "entirely unused import should be removed: {result}"
        );
    }

    #[test]
    fn test_fix_python_formatting_two_blank_lines_before_def() {
        let content = "import os\ndef main():\n    pass\n";
        let result = fix_python_formatting(content);
        // Should have 2 blank lines before `def main()`.
        let def_pos = result.find("def main").unwrap();
        let before_def = &result[..def_pos];
        let trailing_newlines = before_def
            .lines()
            .rev()
            .take_while(|l| l.trim().is_empty())
            .count();
        assert!(
            trailing_newlines >= 2,
            "should have 2 blank lines before def: {result:?}"
        );
    }

    #[test]
    fn test_fix_python_formatting_two_blank_lines_before_class() {
        let content = "import os\nclass MyClass:\n    pass\n";
        let result = fix_python_formatting(content);
        let class_pos = result.find("class MyClass").unwrap();
        let before_class = &result[..class_pos];
        let trailing_newlines = before_class
            .lines()
            .rev()
            .take_while(|l| l.trim().is_empty())
            .count();
        assert!(
            trailing_newlines >= 2,
            "should have 2 blank lines before class: {result:?}"
        );
    }

    #[test]
    fn test_fix_python_formatting_normalizes_excess_blanks() {
        let content = "import os\n\n\n\n\ndef main():\n    pass\n";
        let result = fix_python_formatting(content);
        // Should normalize to exactly 2 blank lines.
        let def_pos = result.find("def main").unwrap();
        let before_def = &result[..def_pos];
        let trailing_newlines = before_def
            .lines()
            .rev()
            .take_while(|l| l.trim().is_empty())
            .count();
        assert_eq!(
            trailing_newlines, 2,
            "should normalize to exactly 2 blank lines: {result:?}"
        );
    }

    #[test]
    fn test_fix_python_formatting_indented_def_untouched() {
        // Indented def/class should NOT get extra blank lines.
        let content = "class Foo:\n    def bar(self):\n        pass\n";
        let result = fix_python_formatting(content);
        // The `def bar` is indented — should not add blank lines before it.
        assert!(
            !result.contains("\n\n\n    def bar"),
            "indented def should not get extra blanks: {result:?}"
        );
    }

    #[test]
    fn test_post_merge_cleanup_full_pipeline() {
        // Full pipeline test: scattered duplicate imports, unused imports,
        // missing blank lines before definitions.
        let content = "\
import os
from flask import Flask, jsonify
import sys
from models import Book
from flask import abort
from models import User
def create_app():
    app = Flask(__name__)
    book = Book()
    user = User()
    return app
class Config:
    DEBUG = True
";
        let result = post_merge_cleanup(content, "app.py");

        // Dedup: `from flask` consolidated, `from models` consolidated.
        assert_eq!(
            result.matches("from flask import").count(),
            1,
            "flask imports should be consolidated: {result}"
        );
        assert_eq!(
            result.matches("from models import").count(),
            1,
            "models imports should be consolidated: {result}"
        );

        // Pruning: `jsonify` and `abort` are not referenced → should be pruned.
        // `Flask`, `Book`, `User` are referenced → should be kept.
        assert!(result.contains("Flask"), "Flask used: {result}");
        assert!(result.contains("Book"), "Book used: {result}");
        assert!(result.contains("User"), "User used: {result}");
        assert!(
            !result.contains("jsonify"),
            "jsonify unused → pruned: {result}"
        );
        assert!(!result.contains("abort"), "abort unused → pruned: {result}");

        // Formatting: 2 blank lines before def and class.
        assert!(result.contains("def create_app"), "def lost: {result}");
        assert!(result.contains("class Config"), "class lost: {result}");
    }

    #[test]
    fn test_post_merge_cleanup_no_imports_passthrough() {
        // Files without imports should pass through unchanged.
        let content = "def main():\n    print('hello')\n";
        let result = post_merge_cleanup(content, "main.py");
        assert_eq!(result, content, "no-import file should be unchanged");
    }

    // ── Additive Composition Tests ──────────────────────────────────────

    #[test]
    fn test_additive_composition_both_sides_add() {
        let base: Vec<String> = vec!["class Base:".into(), "    pass".into()];
        let left: Vec<String> = vec![
            "class Base:".into(),
            "    pass".into(),
            "class Inventory:".into(),
            "    name = 'item'".into(),
        ];
        let right: Vec<String> = vec![
            "class Base:".into(),
            "    pass".into(),
            "class Order:".into(),
            "    total = 0".into(),
        ];

        let result = try_additive_composition(&base, &left, &right, true).unwrap();
        assert!(result.iter().any(|l| l.contains("Base")), "base preserved");
        assert!(
            result.iter().any(|l| l.contains("Inventory")),
            "left addition kept"
        );
        assert!(
            result.iter().any(|l| l.contains("Order")),
            "right addition kept"
        );
    }

    #[test]
    fn test_additive_composition_one_side_deletes_base() {
        let base: Vec<String> = vec!["class Base:".into(), "    pass".into()];
        let left: Vec<String> = vec!["class Replacement:".into(), "    pass".into()];
        let right: Vec<String> = vec![
            "class Base:".into(),
            "    pass".into(),
            "class Order:".into(),
        ];

        let result = try_additive_composition(&base, &left, &right, true);
        assert!(
            result.is_none(),
            "should reject when left drops base content"
        );
    }

    #[test]
    fn test_additive_composition_deduplicates_shared_additions() {
        let base: Vec<String> = vec!["class Base:".into()];
        let left: Vec<String> = vec![
            "class Base:".into(),
            "class Shared:".into(),
            "class LeftOnly:".into(),
        ];
        let right: Vec<String> = vec![
            "class Base:".into(),
            "class Shared:".into(),
            "class RightOnly:".into(),
        ];

        let result = try_additive_composition(&base, &left, &right, true).unwrap();
        let shared_count = result.iter().filter(|l| l.contains("Shared")).count();
        assert_eq!(
            shared_count, 1,
            "shared addition should appear once, not twice"
        );
        assert!(result.iter().any(|l| l.contains("LeftOnly")));
        assert!(result.iter().any(|l| l.contains("RightOnly")));
    }

    #[test]
    fn test_additive_composition_deterministic_ordering() {
        let base: Vec<String> = vec!["# models".into()];
        let left: Vec<String> = vec!["# models".into(), "class A:".into()];
        let right: Vec<String> = vec!["# models".into(), "class B:".into()];

        let lf = try_additive_composition(&base, &left, &right, true).unwrap();
        let rf = try_additive_composition(&base, &left, &right, false).unwrap();

        let lf_a = lf.iter().position(|l| l.contains("A")).unwrap();
        let lf_b = lf.iter().position(|l| l.contains("B")).unwrap();
        assert!(lf_a < lf_b, "left_first=true → A before B");

        let rf_a = rf.iter().position(|l| l.contains("A")).unwrap();
        let rf_b = rf.iter().position(|l| l.contains("B")).unwrap();
        assert!(rf_b < rf_a, "left_first=false → B before A");
    }

    #[test]
    fn test_additive_composition_no_additions_returns_none() {
        let base: Vec<String> = vec!["class Base:".into(), "    pass".into()];
        let left = base.clone();
        let right = base.clone();

        let result = try_additive_composition(&base, &left, &right, true);
        assert!(
            result.is_none(),
            "identical content with no additions → None"
        );
    }

    #[test]
    fn test_additive_composition_via_resolve_direct() {
        // Directly construct a BothModified conflict region where both sides
        // preserved all base content and added unique lines — the exact
        // scenario additive composition is designed for.
        let base_lines: Vec<String> = vec!["class Base:".into(), "    pass".into()];
        let left_lines: Vec<String> = vec![
            "class Base:".into(),
            "    pass".into(),
            "class Inventory:".into(),
            "    name = 'item'".into(),
        ];
        let right_lines: Vec<String> = vec![
            "class Base:".into(),
            "    pass".into(),
            "class Order:".into(),
            "    total = 0".into(),
        ];

        let regions = vec![ConflictRegion {
            base_start: 0,
            base_lines: base_lines,
            left_lines: left_lines,
            right_lines: right_lines,
        }];

        let base = "class Base:\n    pass\n";
        let left = "class Base:\n    pass\nclass Inventory:\n    name = 'item'\n";
        let right = "class Base:\n    pass\nclass Order:\n    total = 0\n";

        let resolved =
            resolve_conflict_regions("test.py", base, left, right, &regions, "spec-a", "spec-b");

        assert!(
            resolved.fully_resolved,
            "should fully resolve via additive composition"
        );
        assert!(resolved.content.contains("Base"), "base preserved");
        assert!(resolved.content.contains("Inventory"), "left addition kept");
        assert!(resolved.content.contains("Order"), "right addition kept");

        let methods: Vec<&str> = resolved
            .resolutions
            .iter()
            .map(|r| r.method.as_str())
            .collect();
        assert!(
            methods.contains(&"additive-composition"),
            "method should be additive-composition, got: {:?}",
            methods
        );
        assert!(
            (resolved.resolutions[0].confidence - 0.85).abs() < 0.01,
            "confidence should be 0.85"
        );
    }

    #[test]
    fn test_additive_composition_via_resolve_conflict_regions() {
        // End-to-end: both agents append new classes after the base.
        // Three-way merge produces BothInserted, which concatenation handles.
        // The key assertion: no content is lost.
        let base = "class Base:\n    pass\n";
        let left = "class Base:\n    pass\n\nclass Inventory:\n    name = 'item'\n";
        let right = "class Base:\n    pass\n\nclass Order:\n    total = 0\n";

        let merge_result = three_way_merge(base, left, right);
        match merge_result {
            FileMergeResult::Conflict(regions) => {
                let resolved = resolve_conflict_regions(
                    "test.py", base, left, right, &regions, "spec-a", "spec-b",
                );
                assert!(
                    resolved.fully_resolved,
                    "divergent additions should auto-resolve"
                );
                assert!(
                    resolved.content.contains("Inventory"),
                    "left content preserved"
                );
                assert!(
                    resolved.content.contains("Order"),
                    "right content preserved"
                );
                assert!(resolved.content.contains("Base"), "base content preserved");
            }
            FileMergeResult::Clean(content) => {
                assert!(content.contains("Base"), "base preserved in clean merge");
            }
        }
    }

    #[test]
    fn test_both_agents_append_to_file_uses_concatenation() {
        // Simpler scenario: both agents only append after the base.
        // This should be BothInserted and resolved via concatenation.
        let base = "class Base:\n    pass\n";
        let left = "class Base:\n    pass\n\nclass Inventory:\n    name = 'item'\n";
        let right = "class Base:\n    pass\n\nclass Order:\n    total = 0\n";

        let merge_result = three_way_merge(base, left, right);
        if let FileMergeResult::Conflict(regions) = merge_result {
            let resolved = resolve_conflict_regions(
                "test.py", base, left, right, &regions, "spec-a", "spec-b",
            );
            assert!(
                resolved.fully_resolved,
                "BothInserted should be auto-resolved: {:?}",
                resolved.unresolved_regions
            );
            assert!(resolved.content.contains("Inventory"), "left content kept");
            assert!(resolved.content.contains("Order"), "right content kept");
        }
        // Clean merge is also fine — the point is nothing gets lost.
    }

    // -----------------------------------------------------------------------
    // Sprint 16 — word-boundary matching
    // -----------------------------------------------------------------------

    #[test]
    fn test_contains_word_exact_match() {
        assert!(contains_word("class User:", "User"));
    }

    #[test]
    fn test_contains_word_rejects_prefix_substring() {
        // "User" should NOT match inside "UserProfile"
        assert!(!contains_word("class UserProfile:", "User"));
    }

    #[test]
    fn test_contains_word_rejects_suffix_substring() {
        // "User" should NOT match inside "SuperUser"
        assert!(!contains_word("class SuperUser:", "User"));
    }

    #[test]
    fn test_contains_word_rejects_mid_word_substring() {
        assert!(!contains_word("get_username()", "User"));
    }

    #[test]
    fn test_contains_word_respects_underscore_boundary() {
        // "User" at an underscore boundary is NOT a word match — underscore
        // is part of Python identifiers.
        assert!(!contains_word("current_User = 1", "User"));
    }

    #[test]
    fn test_contains_word_matches_with_punctuation_boundary() {
        assert!(contains_word("x = User()", "User"));
        assert!(contains_word("isinstance(x, User)", "User"));
        assert!(contains_word("User.objects.all()", "User"));
    }

    #[test]
    fn test_contains_word_start_and_end_of_string() {
        assert!(contains_word("User", "User"));
        assert!(contains_word("User\n", "User"));
        assert!(contains_word("\nUser", "User"));
    }

    #[test]
    fn test_contains_word_empty_needle() {
        assert!(!contains_word("anything", ""));
    }

    #[test]
    fn test_prune_unused_imports_word_boundary() {
        // "User" is imported but only "UserProfile" appears in the body.
        // With word-boundary matching, "User" should be pruned.
        let content =
            "from models import User, Product\n\nclass UserProfile:\n    product = Product()\n";
        let result = prune_unused_python_imports(content);
        assert!(
            !result.contains("User,") && !result.contains(", User"),
            "User should be pruned — only UserProfile exists in body: {result}"
        );
        assert!(
            result.contains("Product"),
            "Product IS used as a whole word: {result}"
        );
    }
}
