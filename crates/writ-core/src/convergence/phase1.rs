//! Phase 1: Structural Diff.
//!
//! Takes raw file content (base, left, right) and produces a
//! [`StructuralDiff`] with conflict regions annotated by the appropriate
//! [`LanguageAnalyzer`]. If diff3 finds no conflicts, returns
//! [`Phase1Result::Clean`] directly.

use super::analyzers;
use super::types::{Phase1Result, StructuralConflictRegion, StructuralDiff};
use super::{ConflictRegion, FileMergeResult};

/// Run Phase 1: three-way diff + structural analysis.
///
/// Calls diff3 to detect conflicts, then runs the appropriate language
/// analyzer on each conflict region to produce structural units.
///
/// Returns `Clean` if diff3 finds no conflicts, or `Conflicts` with
/// a [`StructuralDiff`] containing structurally annotated regions.
pub fn run(file_path: &str, base: &str, left: &str, right: &str) -> Phase1Result {
    let merge_result = super::three_way_merge(base, left, right);

    match merge_result {
        FileMergeResult::Clean(content) => Phase1Result::Clean(content),
        FileMergeResult::Conflict(regions) => {
            let analyzer = analyzers::analyzer_for_path(file_path);
            let structural_regions = regions
                .iter()
                .map(|region| build_structural_region(region, analyzer.as_ref()))
                .collect();

            Phase1Result::Conflicts(StructuralDiff {
                file_path: file_path.to_string(),
                analyzer_used: analyzer.name().to_string(),
                regions: structural_regions,
            })
        }
    }
}

/// Convert a line-based `ConflictRegion` into a `StructuralConflictRegion`
/// by parsing each side's content with the language analyzer.
fn build_structural_region(
    region: &ConflictRegion,
    analyzer: &dyn analyzers::LanguageAnalyzer,
) -> StructuralConflictRegion {
    let base_text = region.base_lines.join("\n");
    let left_text = region.left_lines.join("\n");
    let right_text = region.right_lines.join("\n");

    let base_units = if base_text.is_empty() {
        vec![]
    } else {
        analyzer.parse_structure(&base_text)
    };
    let left_units = if left_text.is_empty() {
        vec![]
    } else {
        analyzer.parse_structure(&left_text)
    };
    let right_units = if right_text.is_empty() {
        vec![]
    } else {
        analyzer.parse_structure(&right_text)
    };

    let base_start = region.base_start;
    let base_end = base_start + region.base_lines.len();

    StructuralConflictRegion {
        base_units,
        left_units,
        right_units,
        base_span: (base_start, base_end),
        left_span: (0, region.left_lines.len()),
        right_span: (0, region.right_lines.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::UnitKind;

    #[test]
    fn test_clean_merge_returns_clean() {
        // Both sides identical → no conflict.
        let base = "line one\nline two\n";
        let left = "line one\nline two\n";
        let right = "line one\nline two\n";
        match run("file.txt", base, left, right) {
            Phase1Result::Clean(_) => {}
            Phase1Result::Conflicts(_) => panic!("expected Clean"),
        }
    }

    #[test]
    fn test_one_side_changed_returns_clean() {
        // Only left changed → diff3 auto-resolves.
        let base = "line one\n";
        let left = "line one modified\n";
        let right = "line one\n";
        match run("file.txt", base, left, right) {
            Phase1Result::Clean(content) => {
                assert!(content.contains("modified"));
            }
            Phase1Result::Conflicts(_) => panic!("expected Clean"),
        }
    }

    #[test]
    fn test_both_modified_returns_conflicts() {
        let base = "original line\n";
        let left = "left version\n";
        let right = "right version\n";
        match run("file.txt", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                assert_eq!(diff.file_path, "file.txt");
                assert!(!diff.regions.is_empty());
                let region = &diff.regions[0];
                assert!(!region.left_units.is_empty());
                assert!(!region.right_units.is_empty());
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }

    #[test]
    fn test_structural_units_populated_for_python() {
        let base = "import os\n";
        let left = "import os\nimport sys\n";
        let right = "import os\nimport json\n";
        match run("models.py", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                assert_eq!(diff.analyzer_used, "python");
                // At least one region should have import-typed units.
                let has_import = diff.regions.iter().any(|r| {
                    r.left_units.iter().any(|u| u.kind == UnitKind::Import)
                        || r.right_units.iter().any(|u| u.kind == UnitKind::Import)
                });
                assert!(has_import, "Python imports should be typed as Import");
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }

    #[test]
    fn test_generic_analyzer_used_for_unknown_extension() {
        let base = "data: 1\n";
        let left = "data: 2\n";
        let right = "data: 3\n";
        match run("config.yaml", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                assert_eq!(diff.analyzer_used, "generic");
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }

    #[test]
    fn test_multiple_conflict_regions() {
        // Two separate conflicts: lines 1 and 3, with a shared line 2.
        let base = "aaa\nshared\nbbb\n";
        let left = "left-aaa\nshared\nleft-bbb\n";
        let right = "right-aaa\nshared\nright-bbb\n";
        match run("file.txt", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                assert!(
                    diff.regions.len() >= 2,
                    "expected at least 2 conflict regions, got {}",
                    diff.regions.len()
                );
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }

    #[test]
    fn test_empty_base_both_insert() {
        let base = "";
        let left = "left content\n";
        let right = "right content\n";
        match run("new_file.txt", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                assert!(!diff.regions.is_empty());
                let region = &diff.regions[0];
                // Base should be empty.
                assert!(region.base_units.is_empty());
                // Both sides should have content.
                assert!(!region.left_units.is_empty());
                assert!(!region.right_units.is_empty());
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }

    #[test]
    fn test_base_span_reflects_conflict_position() {
        // Line 2 is the conflict (1-based: base_start = 2).
        let base = "keep\noriginal\nkeep\n";
        let left = "keep\nleft-change\nkeep\n";
        let right = "keep\nright-change\nkeep\n";
        match run("file.txt", base, left, right) {
            Phase1Result::Conflicts(diff) => {
                let region = &diff.regions[0];
                // base_start should be > 0 (conflict is not at the very beginning).
                assert!(
                    region.base_span.0 > 0,
                    "conflict should not start at line 0, got {}",
                    region.base_span.0
                );
            }
            Phase1Result::Clean(_) => panic!("expected Conflicts"),
        }
    }
}
