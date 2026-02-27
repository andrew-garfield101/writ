//! Three-way merge algorithm (Phase 1 foundation).
//!
//! Provides `three_way_merge()` — the diff3 implementation that produces
//! clean merges or structured conflict regions. Also provides
//! `rebuild_with_resolutions()` for reassembling a file after conflict
//! regions have been resolved.

use crate::diff::{lcs_backtrack, lcs_table, EditOp};

use super::{ConflictRegion, FileMergeResult, LineAction, RegionResolution};

/// Build a per-base-line action table from LCS edit operations.
///
/// Returns:
/// - `actions[i]`: what this side did to base line i (Keep, Delete, or Replace)
/// - `inserts_before[i]`: lines inserted before base line i
/// - `inserts_after`: lines appended after the last base line
pub(super) fn build_action_table(
    base_lines: &[&str],
    new_lines: &[&str],
) -> (Vec<LineAction>, Vec<Vec<String>>, Vec<String>) {
    let table = lcs_table(base_lines, new_lines);
    let ops = lcs_backtrack(&table, base_lines, new_lines);

    let base_len = base_lines.len();
    let mut actions = vec![LineAction::Keep; base_len];
    let mut inserts_before: Vec<Vec<String>> = vec![Vec::new(); base_len + 1];

    let mut pending_inserts: Vec<String> = Vec::new();
    for op in &ops {
        match op {
            EditOp::Equal(oi, _ni) => {
                if !pending_inserts.is_empty() {
                    inserts_before[*oi].extend(pending_inserts.drain(..));
                }
            }
            EditOp::Insert(ni) => {
                pending_inserts.push(new_lines[*ni].to_string());
            }
            EditOp::Delete(oi) => {
                if !pending_inserts.is_empty() {
                    inserts_before[*oi].extend(pending_inserts.drain(..));
                }
                actions[*oi] = LineAction::Delete;
            }
        }
    }

    let inserts_after = pending_inserts;

    let mut i = 0;
    while i < base_len {
        if actions[i] == LineAction::Delete {
            let run_start = i;
            while i < base_len && actions[i] == LineAction::Delete {
                i += 1;
            }
            let mut replacement = Vec::new();
            for j in run_start..=i.min(base_len) {
                if j < inserts_before.len() {
                    replacement.extend(inserts_before[j].drain(..));
                }
            }
            if !replacement.is_empty() {
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
/// Uses LCS-based edit operations for precise positional information.
pub fn three_way_merge(base: &str, left: &str, right: &str) -> FileMergeResult {
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
                        base_start: i + 1,
                        base_lines: vec![],
                        left_lines: li.clone(),
                        right_lines: ri.clone(),
                    });
                }
            }
            (true, true) => {}
        }

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
                    conflicts.push(ConflictRegion {
                        base_start: i + 1,
                        base_lines: vec![base_lines[i].to_string()],
                        left_lines: l.clone(),
                        right_lines: r.clone(),
                    });
                }
            }
            (LineAction::Delete, LineAction::Replace(r)) => {
                conflicts.push(ConflictRegion {
                    base_start: i + 1,
                    base_lines: vec![base_lines[i].to_string()],
                    left_lines: vec![],
                    right_lines: r.clone(),
                });
            }
            (LineAction::Replace(l), LineAction::Delete) => {
                conflicts.push(ConflictRegion {
                    base_start: i + 1,
                    base_lines: vec![base_lines[i].to_string()],
                    left_lines: l.clone(),
                    right_lines: vec![],
                });
            }
        }
    }

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
        let trailing = left.ends_with('\n') || right.ends_with('\n');
        if trailing && !merged.is_empty() && !merged.ends_with('\n') {
            merged.push('\n');
        }
        FileMergeResult::Clean(merged)
    } else {
        FileMergeResult::Conflict(conflicts)
    }
}

/// Rebuild a file by replaying the three-way merge with resolved content
/// substituted for conflict regions.
pub(super) fn rebuild_with_resolutions(
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
        let li = &left_inserts[i];
        let ri = &right_inserts[i];
        match (li.is_empty(), ri.is_empty()) {
            (false, true) => result.extend(li.iter().cloned()),
            (true, false) => result.extend(ri.iter().cloned()),
            (false, false) => {
                if li == ri {
                    result.extend(li.iter().cloned());
                } else {
                    if conflict_idx < resolutions.len()
                        && !resolutions[conflict_idx].lines.is_empty()
                    {
                        result.extend(resolutions[conflict_idx].lines.iter().cloned());
                    } else if conflict_idx < conflict_regions.len() {
                        result.extend(li.iter().cloned());
                        result.extend(ri.iter().cloned());
                    }
                    conflict_idx += 1;
                }
            }
            (true, true) => {}
        }

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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helper ─────────────────────────────────────────────────────────

    /// Assert the merge is clean and return the merged content.
    fn assert_clean(result: &FileMergeResult) -> String {
        match result {
            FileMergeResult::Clean(s) => s.clone(),
            FileMergeResult::Conflict(c) => {
                panic!("Expected clean merge, got {} conflict(s)", c.len())
            }
        }
    }

    /// Assert the merge has conflicts and return them.
    fn assert_conflicts(result: &FileMergeResult) -> Vec<ConflictRegion> {
        match result {
            FileMergeResult::Clean(s) => {
                panic!("Expected conflicts, got clean merge: {s:?}")
            }
            FileMergeResult::Conflict(c) => c.clone(),
        }
    }

    // ── Clean Merges ───────────────────────────────────────────────────

    #[test]
    fn test_identical_left_and_right() {
        let base = "line1\nline2\n";
        let left = "line1\nchanged\n";
        let right = "line1\nchanged\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "line1\nchanged\n");
    }

    #[test]
    fn test_left_unchanged_returns_right() {
        let base = "original\n";
        let left = "original\n";
        let right = "modified\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "modified\n");
    }

    #[test]
    fn test_right_unchanged_returns_left() {
        let base = "original\n";
        let left = "modified\n";
        let right = "original\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "modified\n");
    }

    #[test]
    fn test_non_overlapping_changes() {
        let base = "line1\nline2\nline3\n";
        let left = "LEFT\nline2\nline3\n";
        let right = "line1\nline2\nRIGHT\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(merged.contains("LEFT"));
        assert!(merged.contains("RIGHT"));
        assert!(merged.contains("line2"));
    }

    #[test]
    fn test_both_delete_same_line() {
        let base = "keep\nremove\nkeep2\n";
        let left = "keep\nkeep2\n";
        let right = "keep\nkeep2\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(!merged.contains("remove"));
        assert!(merged.contains("keep"));
        assert!(merged.contains("keep2"));
    }

    #[test]
    fn test_left_deletes_right_keeps() {
        let base = "a\nb\nc\n";
        let left = "a\nc\n";
        let right = "a\nb\nc\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(!merged.contains("b"));
    }

    #[test]
    fn test_left_inserts_right_unchanged() {
        let base = "line1\nline2\n";
        let left = "line1\nnew_line\nline2\n";
        let right = "line1\nline2\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(merged.contains("new_line"));
        assert!(merged.contains("line1"));
        assert!(merged.contains("line2"));
    }

    #[test]
    fn test_both_insert_same_content() {
        let base = "a\nc\n";
        let left = "a\nb\nc\n";
        let right = "a\nb\nc\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(merged.contains("b"));
    }

    // ── Conflicts ──────────────────────────────────────────────────────

    #[test]
    fn test_same_region_different_changes() {
        let base = "line1\noriginal\nline3\n";
        let left = "line1\nleft_change\nline3\n";
        let right = "line1\nright_change\nline3\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
        let c = &conflicts[0];
        assert!(c.left_lines.iter().any(|l| l.contains("left_change")));
        assert!(c.right_lines.iter().any(|l| l.contains("right_change")));
    }

    #[test]
    fn test_multiple_conflict_regions() {
        let base = "a\nb\nc\nd\ne\n";
        let left = "a\nL1\nc\nL2\ne\n";
        let right = "a\nR1\nc\nR2\ne\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(
            conflicts.len() >= 2,
            "Expected at least 2 conflicts, got {}",
            conflicts.len()
        );
    }

    #[test]
    fn test_delete_vs_replace_is_conflict() {
        let base = "keep\ntarget\nkeep2\n";
        let left = "keep\nkeep2\n"; // deletes target
        let right = "keep\nreplaced\nkeep2\n"; // replaces target
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_both_insert_different_before_same_line() {
        let base = "anchor\n";
        let left = "left_insert\nanchor\n";
        let right = "right_insert\nanchor\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_trailing_additions_conflict() {
        let base = "line1\n";
        let left = "line1\nleft_trailing\n";
        let right = "line1\nright_trailing\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
        let c = conflicts.last().unwrap();
        assert!(c.left_lines.iter().any(|l| l.contains("left_trailing")));
        assert!(c.right_lines.iter().any(|l| l.contains("right_trailing")));
    }

    // ── Edge Cases ─────────────────────────────────────────────────────

    #[test]
    fn test_empty_base() {
        let base = "";
        let left = "left content\n";
        let right = "right content\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_empty_left_yields_right() {
        // When left is empty, the action table treats it as "keep base" not
        // "delete everything". So right's modifications apply cleanly.
        let base = "content\n";
        let left = "";
        let right = "modified\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "modified\n");
    }

    #[test]
    fn test_empty_right_yields_left() {
        let base = "content\n";
        let left = "modified\n";
        let right = "";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "modified\n");
    }

    #[test]
    fn test_all_three_identical() {
        let content = "same\ncontent\n";
        let merged = assert_clean(&three_way_merge(content, content, content));
        assert_eq!(merged, content);
    }

    #[test]
    fn test_all_three_empty() {
        let merged = assert_clean(&three_way_merge("", "", ""));
        assert_eq!(merged, "");
    }

    #[test]
    fn test_single_line_no_newline() {
        let base = "hello";
        let left = "hello";
        let right = "world";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert_eq!(merged, "world");
    }

    #[test]
    fn test_single_line_conflict() {
        let base = "original";
        let left = "left_ver";
        let right = "right_ver";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(!conflicts.is_empty());
    }

    #[test]
    fn test_trailing_newline_preserved() {
        let base = "line\n";
        let left = "line\n";
        let right = "changed\n";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(merged.ends_with('\n'));
    }

    #[test]
    fn test_no_trailing_newline_preserved() {
        let base = "line";
        let left = "line";
        let right = "changed";
        let merged = assert_clean(&three_way_merge(base, left, right));
        assert!(!merged.ends_with('\n'));
    }

    #[test]
    fn test_large_non_overlapping() {
        let mut base_lines = Vec::new();
        let mut left_lines = Vec::new();
        let mut right_lines = Vec::new();
        for i in 0..100 {
            base_lines.push(format!("line_{i}"));
            if i < 10 {
                left_lines.push(format!("left_{i}"));
            } else {
                left_lines.push(format!("line_{i}"));
            }
            if i >= 90 {
                right_lines.push(format!("right_{i}"));
            } else {
                right_lines.push(format!("line_{i}"));
            }
        }
        let base = base_lines.join("\n") + "\n";
        let left = left_lines.join("\n") + "\n";
        let right = right_lines.join("\n") + "\n";

        let merged = assert_clean(&three_way_merge(&base, &left, &right));
        // Left changes at top, right changes at bottom
        assert!(merged.contains("left_0"));
        assert!(merged.contains("right_99"));
        // Unchanged middle preserved
        assert!(merged.contains("line_50"));
    }

    #[test]
    fn test_conflict_region_has_correct_base_start() {
        let base = "a\nb\nc\n";
        let left = "a\nleft\nc\n";
        let right = "a\nright\nc\n";
        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert_eq!(conflicts.len(), 1);
        // base_start is 1-indexed, 'b' is line 2
        assert_eq!(conflicts[0].base_start, 2);
        assert_eq!(conflicts[0].base_lines, vec!["b"]);
    }

    // ── rebuild_with_resolutions ───────────────────────────────────────

    #[test]
    fn test_rebuild_with_single_resolution() {
        let base = "a\nb\nc\n";
        let left = "a\nleft\nc\n";
        let right = "a\nright\nc\n";

        let result = three_way_merge(base, left, right);
        let conflicts = assert_conflicts(&result);
        assert_eq!(conflicts.len(), 1);

        let resolutions = vec![RegionResolution {
            lines: vec!["resolved".to_string()],
            class: super::super::ConflictClass::BothModified,
            method: "test".to_string(),
            confidence: 1.0,
        }];

        let rebuilt = rebuild_with_resolutions(base, left, right, &conflicts, &resolutions);
        assert!(rebuilt.contains("a"));
        assert!(rebuilt.contains("resolved"));
        assert!(rebuilt.contains("c"));
        assert!(!rebuilt.contains("left"));
        assert!(!rebuilt.contains("right"));
    }

    #[test]
    fn test_rebuild_preserves_clean_regions() {
        let base = "top\nconflict\nbottom\n";
        let left = "top\nleft_val\nbottom\n";
        let right = "top\nright_val\nbottom\n";

        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        let resolutions = vec![RegionResolution {
            lines: vec!["merged_val".to_string()],
            class: super::super::ConflictClass::BothModified,
            method: "test".to_string(),
            confidence: 1.0,
        }];

        let rebuilt = rebuild_with_resolutions(base, left, right, &conflicts, &resolutions);
        assert!(rebuilt.contains("top"));
        assert!(rebuilt.contains("merged_val"));
        assert!(rebuilt.contains("bottom"));
    }

    #[test]
    fn test_rebuild_multiple_resolutions() {
        let base = "a\nb\nc\nd\ne\n";
        let left = "a\nL1\nc\nL2\ne\n";
        let right = "a\nR1\nc\nR2\ne\n";

        let conflicts = assert_conflicts(&three_way_merge(base, left, right));
        assert!(conflicts.len() >= 2);

        let resolutions: Vec<RegionResolution> = conflicts
            .iter()
            .enumerate()
            .map(|(i, _)| RegionResolution {
                lines: vec![format!("resolved_{i}")],
                class: super::super::ConflictClass::BothModified,
                method: "test".to_string(),
                confidence: 1.0,
            })
            .collect();

        let rebuilt = rebuild_with_resolutions(base, left, right, &conflicts, &resolutions);
        assert!(rebuilt.contains("resolved_0"));
        assert!(rebuilt.contains("resolved_1"));
        assert!(!rebuilt.contains("L1"));
        assert!(!rebuilt.contains("R1"));
    }

    // ── build_action_table ─────────────────────────────────────────────

    #[test]
    fn test_action_table_no_changes() {
        let base: Vec<&str> = vec!["a", "b", "c"];
        let new: Vec<&str> = vec!["a", "b", "c"];
        let (actions, _, after) = build_action_table(&base, &new);
        assert!(actions.iter().all(|a| matches!(a, LineAction::Keep)));
        assert!(after.is_empty());
    }

    #[test]
    fn test_action_table_deletion() {
        let base: Vec<&str> = vec!["a", "b", "c"];
        let new: Vec<&str> = vec!["a", "c"];
        let (actions, _, _) = build_action_table(&base, &new);
        assert!(matches!(actions[0], LineAction::Keep));
        assert!(matches!(actions[1], LineAction::Delete));
        assert!(matches!(actions[2], LineAction::Keep));
    }

    #[test]
    fn test_action_table_insertion() {
        let base: Vec<&str> = vec!["a", "c"];
        let new: Vec<&str> = vec!["a", "b", "c"];
        let (actions, inserts, _) = build_action_table(&base, &new);
        assert!(matches!(actions[0], LineAction::Keep));
        assert!(matches!(actions[1], LineAction::Keep));
        // "b" should be inserted before "c" (index 1)
        assert!(
            inserts[1].contains(&"b".to_string()),
            "Expected insert before index 1, got: {inserts:?}"
        );
    }

    #[test]
    fn test_action_table_replacement() {
        let base: Vec<&str> = vec!["a", "old", "c"];
        let new: Vec<&str> = vec!["a", "new", "c"];
        let (actions, _, _) = build_action_table(&base, &new);
        assert!(matches!(actions[0], LineAction::Keep));
        assert!(
            matches!(&actions[1], LineAction::Replace(v) if v.contains(&"new".to_string())),
            "Expected Replace for changed line, got {:?}",
            actions[1]
        );
        assert!(matches!(actions[2], LineAction::Keep));
    }

    #[test]
    fn test_action_table_trailing_insert() {
        let base: Vec<&str> = vec!["a"];
        let new: Vec<&str> = vec!["a", "appended"];
        let (_, _, after) = build_action_table(&base, &new);
        assert!(
            after.contains(&"appended".to_string()),
            "Expected trailing insert, got: {after:?}"
        );
    }

    #[test]
    fn test_action_table_empty_base() {
        let base: Vec<&str> = vec![];
        let new: Vec<&str> = vec!["a", "b"];
        let (actions, _, after) = build_action_table(&base, &new);
        assert!(actions.is_empty());
        assert_eq!(after, vec!["a", "b"]);
    }
}
