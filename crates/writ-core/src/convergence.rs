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

    let (left_actions, left_inserts, left_after) =
        build_action_table(&base_lines, &left_lines);
    let (right_actions, right_inserts, right_after) =
        build_action_table(&base_lines, &right_lines);

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
        let mut t = left_inserts.get(base_lines.len()).cloned().unwrap_or_default();
        t.extend(left_after.iter().cloned());
        t
    };
    let right_trailing = {
        let mut t = right_inserts.get(base_lines.len()).cloned().unwrap_or_default();
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
}
