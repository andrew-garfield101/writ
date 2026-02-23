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
