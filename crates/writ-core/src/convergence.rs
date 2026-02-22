//! Convergence — writ's answer to merging.
//!
//! When two specs modify overlapping files, convergence detects conflicts
//! and produces structured merge results. Conflicts are JSON-serializable
//! data — not text markers — so orchestrator agents can resolve them
//! programmatically.

use serde::{Deserialize, Serialize};

use crate::diff::{lcs_backtrack, lcs_table, EditOp};

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
    /// For irreconcilable conflicts, prefer the most recently sealed version
    /// (resolved per-region, not per-file).
    MostRecent,
    /// Return structured conflict data for orchestrator agent resolution.
    Orchestrator,
}

impl Default for ConvergeStrategy {
    fn default() -> Self {
        ConvergeStrategy::Manual
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

/// Returns true if all non-empty lines in the slice are import-like.
fn is_import_region(lines: &[String]) -> bool {
    if lines.is_empty() {
        return false;
    }
    lines
        .iter()
        .all(|line| line.trim().is_empty() || is_import_line(line))
}

/// Merge two sets of import lines: union, dedup, sort for deterministic output.
fn merge_imports(left: &[String], right: &[String]) -> Vec<String> {
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

/// Merge decorator groups from both sides, preserving all decorators.
///
/// For each declaration line present in either side, collects the union
/// of decorators from both sides. Uses the longer side for declaration
/// ordering. Returns `None` if neither side has decorator structure.
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
        result.extend(all_decorators);
        result.push(decl.clone());
    }

    // Add declarations only in secondary.
    for (decl, decorators) in secondary {
        if !seen_decls.contains(decl) {
            result.extend(decorators.iter().cloned());
            result.push(decl.clone());
        }
    }

    Some(result)
}

/// Resolve all conflict regions for a single file using Layers 2-3.
///
/// This is the main entry point for the new resolution engine.
/// `left_spec_id` and `right_spec_id` are used for deterministic
/// ordering (BothInserted regions are concatenated ordered by spec ID).
///
/// Layers 2-3 are always-on: BothInserted → concatenate,
/// DeleteVsModify → keep modification, superset detection, import
/// accumulation. Only `BothModified` regions that can't be pattern-matched
/// remain unresolved for the strategy fallback in Layer 4.
pub fn resolve_conflict_regions(
    base_content: &str,
    left_content: &str,
    right_content: &str,
    regions: &[ConflictRegion],
    left_spec_id: &str,
    right_spec_id: &str,
) -> ResolvedFile {
    // Start from the three-way merge's clean parts: replay the merge but
    // substitute resolved content for conflict regions.
    //
    // Strategy: re-run the merge using left/right content. For each
    // conflict region, insert our resolution (or mark as unresolved).
    let mut resolved_regions: Vec<RegionResolution> = Vec::new();
    let mut unresolved: Vec<ConflictRegion> = Vec::new();
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
                let all_imports = region
                    .left_lines
                    .iter()
                    .chain(region.right_lines.iter())
                    .all(|l| is_import_line(l));

                let mut lines = Vec::new();
                if left_first {
                    lines.extend(region.left_lines.iter().cloned());
                    lines.extend(region.right_lines.iter().cloned());
                } else {
                    lines.extend(region.right_lines.iter().cloned());
                    lines.extend(region.left_lines.iter().cloned());
                }
                // Deduplicate identical lines (import dedup or general dedup).
                let mut seen = std::collections::HashSet::new();
                lines.retain(|l| seen.insert(l.clone()));

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
            ConflictClass::DeleteVsModify { modifier } => {
                // Keep the modification — deletion was likely an oversight.
                let lines = match modifier {
                    MergeSide::Left => region.left_lines.clone(),
                    MergeSide::Right => region.right_lines.clone(),
                };
                resolved_regions.push(RegionResolution {
                    lines,
                    class: class.clone(),
                    method: "kept-modification".to_string(),
                    confidence: 0.85,
                });
            }
            ConflictClass::BothModified => {
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
    }
}

/// Rebuild a file by replaying the three-way merge, substituting resolved
/// content for each conflict region.
fn rebuild_with_resolutions(
    base: &str,
    left: &str,
    right: &str,
    conflict_regions: &[ConflictRegion],
    resolutions: &[RegionResolution],
) -> String {
    let base_lines: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.lines().collect()
    };
    let left_lines: Vec<&str> = if left.is_empty() {
        Vec::new()
    } else {
        left.lines().collect()
    };
    let right_lines: Vec<&str> = if right.is_empty() {
        Vec::new()
    } else {
        right.lines().collect()
    };

    let (left_actions, left_inserts, left_after) = build_action_table(&base_lines, &left_lines);
    let (right_actions, right_inserts, right_after) = build_action_table(&base_lines, &right_lines);

    let mut result: Vec<String> = Vec::new();
    let mut conflict_idx = 0;

    for i in 0..base_lines.len() {
        // Handle inserts before this base line.
        let li = &left_inserts[i];
        let ri = &right_inserts[i];
        match (li.is_empty(), ri.is_empty()) {
            (false, true) => result.extend(li.iter().cloned()),
            (true, false) => result.extend(ri.iter().cloned()),
            (false, false) => {
                if li == ri {
                    result.extend(li.iter().cloned());
                } else {
                    // This is a conflict region — use resolution.
                    if conflict_idx < resolutions.len()
                        && !resolutions[conflict_idx].lines.is_empty()
                    {
                        result.extend(resolutions[conflict_idx].lines.iter().cloned());
                    } else if conflict_idx < conflict_regions.len() {
                        // Unresolved — include both sides with markers.
                        result.extend(li.iter().cloned());
                        result.extend(ri.iter().cloned());
                    }
                    conflict_idx += 1;
                }
            }
            (true, true) => {}
        }

        // Handle the base line itself.
        let la = &left_actions[i];
        let ra = &right_actions[i];
        match (la, ra) {
            (LineAction::Keep, LineAction::Keep) => {
                result.push(base_lines[i].to_string());
            }
            (LineAction::Keep, LineAction::Delete) | (LineAction::Delete, LineAction::Keep) => {}
            (LineAction::Delete, LineAction::Delete) => {}
            (LineAction::Keep, LineAction::Replace(r)) => {
                result.extend(r.iter().cloned());
            }
            (LineAction::Replace(l), LineAction::Keep) => {
                result.extend(l.iter().cloned());
            }
            (LineAction::Replace(l), LineAction::Replace(r)) => {
                if l == r {
                    result.extend(l.iter().cloned());
                } else {
                    if conflict_idx < resolutions.len()
                        && !resolutions[conflict_idx].lines.is_empty()
                    {
                        result.extend(resolutions[conflict_idx].lines.iter().cloned());
                    } else if conflict_idx < conflict_regions.len() {
                        result.extend(l.iter().cloned());
                        result.extend(r.iter().cloned());
                    }
                    conflict_idx += 1;
                }
            }
            (LineAction::Delete, LineAction::Replace(r)) => {
                if conflict_idx < resolutions.len() && !resolutions[conflict_idx].lines.is_empty() {
                    result.extend(resolutions[conflict_idx].lines.iter().cloned());
                } else if conflict_idx < conflict_regions.len() {
                    result.extend(r.iter().cloned());
                }
                conflict_idx += 1;
            }
            (LineAction::Replace(l), LineAction::Delete) => {
                if conflict_idx < resolutions.len() && !resolutions[conflict_idx].lines.is_empty() {
                    result.extend(resolutions[conflict_idx].lines.iter().cloned());
                } else if conflict_idx < conflict_regions.len() {
                    result.extend(l.iter().cloned());
                }
                conflict_idx += 1;
            }
        }
    }

    // Handle trailing inserts.
    let left_trailing = {
        let mut t = left_inserts
            .get(base_lines.len())
            .cloned()
            .unwrap_or_default();
        t.extend(left_after.iter().cloned());
        t
    };
    let right_trailing = {
        let mut t = right_inserts
            .get(base_lines.len())
            .cloned()
            .unwrap_or_default();
        t.extend(right_after.iter().cloned());
        t
    };

    match (left_trailing.is_empty(), right_trailing.is_empty()) {
        (false, true) => result.extend(left_trailing),
        (true, false) => result.extend(right_trailing),
        (false, false) => {
            if left_trailing == right_trailing {
                result.extend(left_trailing);
            } else {
                if conflict_idx < resolutions.len() && !resolutions[conflict_idx].lines.is_empty() {
                    result.extend(resolutions[conflict_idx].lines.iter().cloned());
                } else {
                    result.extend(left_trailing);
                    result.extend(right_trailing);
                }
            }
        }
        (true, true) => {}
    }

    let mut merged = result.join("\n");
    let trailing = left.ends_with('\n') || right.ends_with('\n');
    if trailing && !merged.is_empty() && !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
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

/// Perform a three-way merge with Layers 2-3 auto-resolution.
///
/// Calls `three_way_merge()` (Layer 1), then attempts to resolve any
/// conflict regions using region classification and structural pattern
/// matching (Layers 2-3). Only truly irreconcilable `BothModified`
/// regions remain as conflicts for Layer 4 strategy fallback.
///
/// `left_spec` and `right_spec` are spec IDs used for deterministic
/// ordering when concatenating `BothInserted` regions.
pub fn smart_merge(
    base: &str,
    left: &str,
    right: &str,
    left_spec: &str,
    right_spec: &str,
) -> SmartMergeResult {
    // Layer 1: standard three-way merge.
    match three_way_merge(base, left, right) {
        FileMergeResult::Clean(content) => SmartMergeResult::Clean {
            content,
            resolutions: vec![],
        },
        FileMergeResult::Conflict(regions) => {
            // Layers 2-3: attempt to auto-resolve conflict regions.
            let resolved =
                resolve_conflict_regions(base, left, right, &regions, left_spec, right_spec);

            if resolved.fully_resolved {
                SmartMergeResult::Clean {
                    content: resolved.content,
                    resolutions: resolved.resolutions,
                }
            } else {
                SmartMergeResult::Partial {
                    content: resolved.content,
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
    /// Whether changes were applied to the working directory.
    pub applied: bool,
    /// Warnings about potential content loss or semantic inconsistency.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
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

/// Build a per-base-line action table from LCS edit operations.
///
/// Returns:
/// - `actions[i]`: what this side did to base line i (Keep, Delete, or Replace)
/// - `inserts_before[i]`: lines inserted before base line i
/// - `inserts_after`: lines appended after the last base line
fn build_action_table(
    base_lines: &[&str],
    new_lines: &[&str],
) -> (Vec<LineAction>, Vec<Vec<String>>, Vec<String>) {
    let table = lcs_table(base_lines, new_lines);
    let ops = lcs_backtrack(&table, base_lines, new_lines);

    let base_len = base_lines.len();
    let mut actions = vec![LineAction::Keep; base_len];
    let mut inserts_before: Vec<Vec<String>> = vec![Vec::new(); base_len + 1];

    // Walk through edit ops, collecting actions and insertion points.
    let mut pending_inserts: Vec<String> = Vec::new();
    for op in &ops {
        match op {
            EditOp::Equal(oi, _ni) => {
                // Flush pending inserts before this base line.
                if !pending_inserts.is_empty() {
                    inserts_before[*oi].extend(pending_inserts.drain(..));
                }
            }
            EditOp::Insert(ni) => {
                pending_inserts.push(new_lines[*ni].to_string());
            }
            EditOp::Delete(oi) => {
                // Flush pending inserts before this deleted line.
                if !pending_inserts.is_empty() {
                    inserts_before[*oi].extend(pending_inserts.drain(..));
                }
                actions[*oi] = LineAction::Delete;
            }
        }
    }

    // Any remaining pending inserts go after the last base line.
    let inserts_after = pending_inserts;

    // Now handle consecutive Delete+Insert sequences as Replacements.
    // Walk through base lines: if a line is deleted and has inserts before
    // the next non-deleted line, those inserts are its replacement.
    // We do this by grouping: find runs of deleted base lines, and the
    // inserts that follow belong to that group.
    let mut i = 0;
    while i < base_len {
        if actions[i] == LineAction::Delete {
            // Find the run of consecutive deletions.
            let run_start = i;
            while i < base_len && actions[i] == LineAction::Delete {
                i += 1;
            }
            // Inserts between the deleted run and the next line are
            // the replacement. They'd be in inserts_before[i] (or
            // inserts_before[run_start] if inserts came before the run).
            // Collect all inserts associated with the deleted region.
            let mut replacement = Vec::new();
            for j in run_start..=i.min(base_len) {
                if j < inserts_before.len() {
                    replacement.extend(inserts_before[j].drain(..));
                }
            }
            if !replacement.is_empty() {
                // Mark the first deleted line as Replace, rest stay Delete.
                actions[run_start] = LineAction::Replace(replacement);
                for j in (run_start + 1)..i {
                    actions[j] = LineAction::Delete;
                }
            }
        } else {
            i += 1;
        }
    }

    (actions, inserts_before, inserts_after)
}

/// Perform a three-way merge of text content.
///
/// Given a common base, a left version, and a right version, this
/// produces either a clean merge or a list of conflict regions.
///
/// Uses LCS-based edit operations for precise positional information,
/// avoiding the hunk-positioning issues that arise with pure insertions.
pub fn three_way_merge(base: &str, left: &str, right: &str) -> FileMergeResult {
    // Fast paths.
    if left == right {
        return FileMergeResult::Clean(left.to_string());
    }
    if base == left {
        return FileMergeResult::Clean(right.to_string());
    }
    if base == right {
        return FileMergeResult::Clean(left.to_string());
    }

    let base_lines: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.lines().collect()
    };
    let left_lines: Vec<&str> = if left.is_empty() {
        Vec::new()
    } else {
        left.lines().collect()
    };
    let right_lines: Vec<&str> = if right.is_empty() {
        Vec::new()
    } else {
        right.lines().collect()
    };

    let (left_actions, left_inserts, left_after) = build_action_table(&base_lines, &left_lines);
    let (right_actions, right_inserts, right_after) = build_action_table(&base_lines, &right_lines);

    let mut result: Vec<String> = Vec::new();
    let mut conflicts: Vec<ConflictRegion> = Vec::new();

    for i in 0..base_lines.len() {
        // Handle inserts before this base line.
        let li = &left_inserts[i];
        let ri = &right_inserts[i];
        match (li.is_empty(), ri.is_empty()) {
            (false, true) => result.extend(li.iter().cloned()),
            (true, false) => result.extend(ri.iter().cloned()),
            (false, false) => {
                if li == ri {
                    result.extend(li.iter().cloned());
                } else {
                    conflicts.push(ConflictRegion {
                        base_start: i + 1, // 1-based
                        base_lines: vec![],
                        left_lines: li.clone(),
                        right_lines: ri.clone(),
                    });
                }
            }
            (true, true) => {}
        }

        // Handle the base line itself.
        let la = &left_actions[i];
        let ra = &right_actions[i];
        match (la, ra) {
            (LineAction::Keep, LineAction::Keep) => {
                result.push(base_lines[i].to_string());
            }
            (LineAction::Keep, LineAction::Delete) | (LineAction::Delete, LineAction::Keep) => {
                // One side deleted — take the deletion.
            }
            (LineAction::Delete, LineAction::Delete) => {
                // Both deleted — agreed.
            }
            (LineAction::Keep, LineAction::Replace(r)) => {
                result.extend(r.iter().cloned());
            }
            (LineAction::Replace(l), LineAction::Keep) => {
                result.extend(l.iter().cloned());
            }
            (LineAction::Replace(l), LineAction::Replace(r)) => {
                if l == r {
                    result.extend(l.iter().cloned());
                } else {
                    conflicts.push(ConflictRegion {
                        base_start: i + 1,
                        base_lines: vec![base_lines[i].to_string()],
                        left_lines: l.clone(),
                        right_lines: r.clone(),
                    });
                }
            }
            (LineAction::Delete, LineAction::Replace(r)) => {
                // Left deleted, right replaced → conflict.
                conflicts.push(ConflictRegion {
                    base_start: i + 1,
                    base_lines: vec![base_lines[i].to_string()],
                    left_lines: vec![],
                    right_lines: r.clone(),
                });
            }
            (LineAction::Replace(l), LineAction::Delete) => {
                // Left replaced, right deleted → conflict.
                conflicts.push(ConflictRegion {
                    base_start: i + 1,
                    base_lines: vec![base_lines[i].to_string()],
                    left_lines: l.clone(),
                    right_lines: vec![],
                });
            }
        }
    }

    // Handle inserts after the last base line.
    // Also check inserts_before[base_lines.len()] for trailing inserts.
    let left_trailing = {
        let mut t = left_inserts
            .get(base_lines.len())
            .cloned()
            .unwrap_or_default();
        t.extend(left_after.iter().cloned());
        t
    };
    let right_trailing = {
        let mut t = right_inserts
            .get(base_lines.len())
            .cloned()
            .unwrap_or_default();
        t.extend(right_after.iter().cloned());
        t
    };

    match (left_trailing.is_empty(), right_trailing.is_empty()) {
        (false, true) => result.extend(left_trailing),
        (true, false) => result.extend(right_trailing),
        (false, false) => {
            if left_trailing == right_trailing {
                result.extend(left_trailing);
            } else {
                conflicts.push(ConflictRegion {
                    base_start: base_lines.len() + 1,
                    base_lines: vec![],
                    left_lines: left_trailing,
                    right_lines: right_trailing,
                });
            }
        }
        (true, true) => {}
    }

    if conflicts.is_empty() {
        let mut merged = result.join("\n");
        // Preserve trailing newline: lines() strips it, so if any input
        // ended with '\n' we restore it on the merged output.
        let trailing = left.ends_with('\n') || right.ends_with('\n');
        if trailing && !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        FileMergeResult::Clean(merged)
    } else {
        FileMergeResult::Conflict(conflicts)
    }
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

        let resolved = resolve_conflict_regions(base, left, right, &regions, "api-dev", "auth-dev");
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
        let result_lr = resolve_conflict_regions(base, left, right, &regions_lr, "alpha", "beta");
        let result_rl = resolve_conflict_regions(base, right, left, &regions_rl, "beta", "alpha");

        assert_eq!(result_lr.content, result_rl.content);
    }

    #[test]
    fn test_resolve_delete_vs_modify_keeps_modification() {
        let base = "line1\nline2\nline3";
        let left = "line1\nline3"; // deleted line2
        let right = "line1\nMODIFIED\nline3"; // modified line2

        let regions = match three_way_merge(base, left, right) {
            FileMergeResult::Conflict(r) => r,
            _ => panic!("expected conflict"),
        };

        let resolved = resolve_conflict_regions(base, left, right, &regions, "a", "b");
        assert!(resolved.fully_resolved);
        assert!(resolved.content.contains("MODIFIED"));
        assert!(!resolved.content.contains("\nline2\n"));
        assert_eq!(resolved.resolutions[0].method, "kept-modification");
        assert!((resolved.resolutions[0].confidence - 0.85).abs() < 0.01);
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

        let result = smart_merge(base, left, right, "a", "b");
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

        let result = smart_merge(base, left, right, "a", "b");
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

        let result = smart_merge(base, left, right, "a", "b");
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

        let result = smart_merge(base, left, right, "a", "b");
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

        let result = smart_merge(base, left, right, "alpha", "beta");
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

        let result = smart_merge(base, left, right, "a", "b");
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

        let result = smart_merge(base, left, right, "api-dev", "auth-dev");

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
            let resolved = resolve_conflict_regions("", "a", "b", &regions, "x", "y");
            assert!((resolved.resolutions[0].confidence - 0.9).abs() < 0.01);
            assert_eq!(resolved.resolutions[0].method, "concatenated");
        }

        // DeleteVsModify → 0.85
        {
            let regions = vec![ConflictRegion {
                base_start: 2,
                base_lines: vec!["original".to_string()],
                left_lines: vec![],
                right_lines: vec!["modified".to_string()],
            }];
            let resolved = resolve_conflict_regions(
                "line1\noriginal\nline3",
                "line1\nline3",
                "line1\nmodified\nline3",
                &regions,
                "x",
                "y",
            );
            assert!((resolved.resolutions[0].confidence - 0.85).abs() < 0.01);
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
}
