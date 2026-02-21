//! Diff computation for writ.
//!
//! Provides line-level diffing between file contents, producing
//! structured output suitable for both human display and LLM consumption.

use serde::{Deserialize, Serialize};

use crate::seal::ChangeType;

/// What kind of diff operation on a line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LineOp {
    /// Line exists only in the "after" version.
    Add,
    /// Line exists only in the "before" version.
    Remove,
    /// Line is identical in both versions.
    Context,
}

/// A single line within a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub op: LineOp,
    pub content: String,
    /// 1-based line number in the old file (None for Add lines).
    pub old_lineno: Option<usize>,
    /// 1-based line number in the new file (None for Remove lines).
    pub new_lineno: Option<usize>,
}

/// A contiguous block of changes within a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffHunk {
    /// Starting line in the old file (1-based).
    pub old_start: usize,
    /// Number of lines from the old file in this hunk.
    pub old_count: usize,
    /// Starting line in the new file (1-based).
    pub new_start: usize,
    /// Number of lines from the new file in this hunk.
    pub new_count: usize,
    /// The individual diff lines.
    pub lines: Vec<DiffLine>,
}

/// The diff result for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    /// Relative path of the file.
    pub path: String,
    /// What kind of change.
    pub change_type: ChangeType,
    /// Diff hunks (empty for binary files).
    pub hunks: Vec<DiffHunk>,
    /// True if the file appears to be binary.
    pub is_binary: bool,
    /// Lines added count.
    pub additions: usize,
    /// Lines removed count.
    pub deletions: usize,
}

/// The full diff output for a comparison.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffOutput {
    /// What is being compared.
    pub description: String,
    /// Per-file diffs.
    pub files: Vec<FileDiff>,
    /// Total files changed.
    pub files_changed: usize,
    /// Total lines added across all files.
    pub total_additions: usize,
    /// Total lines removed across all files.
    pub total_deletions: usize,
}

/// Returns true if the data appears to be binary (contains null byte in first 8KB).
pub fn is_binary(data: &[u8]) -> bool {
    let check_len = data.len().min(8192);
    data[..check_len].contains(&0)
}

/// Compute the longest common subsequence table for two slices of lines.
pub(crate) fn lcs_table(old: &[&str], new: &[&str]) -> Vec<Vec<usize>> {
    let m = old.len();
    let n = new.len();
    let mut table = vec![vec![0usize; n + 1]; m + 1];

    for i in 1..=m {
        for j in 1..=n {
            if old[i - 1] == new[j - 1] {
                table[i][j] = table[i - 1][j - 1] + 1;
            } else {
                table[i][j] = table[i - 1][j].max(table[i][j - 1]);
            }
        }
    }

    table
}

/// Edit operation produced by LCS backtracking.
#[derive(Debug, PartialEq)]
pub(crate) enum EditOp {
    Equal(usize, usize), // old_idx, new_idx
    Insert(usize),       // new_idx
    Delete(usize),       // old_idx
}

/// Backtrack through the LCS table to produce a sequence of edit operations.
pub(crate) fn lcs_backtrack(table: &[Vec<usize>], old: &[&str], new: &[&str]) -> Vec<EditOp> {
    let mut ops = Vec::new();
    let mut i = old.len();
    let mut j = new.len();

    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            ops.push(EditOp::Equal(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || table[i][j - 1] >= table[i - 1][j]) {
            ops.push(EditOp::Insert(j - 1));
            j -= 1;
        } else {
            ops.push(EditOp::Delete(i - 1));
            i -= 1;
        }
    }

    ops.reverse();
    ops
}

/// Compute diff hunks between two strings (treated as line sequences).
///
/// `context_lines` controls how many unchanged lines surround each hunk.
pub fn compute_line_diff(old: &str, new: &str, context_lines: usize) -> Vec<DiffHunk> {
    let old_lines: Vec<&str> = if old.is_empty() {
        Vec::new()
    } else {
        old.lines().collect()
    };
    let new_lines: Vec<&str> = if new.is_empty() {
        Vec::new()
    } else {
        new.lines().collect()
    };

    let table = lcs_table(&old_lines, &new_lines);
    let ops = lcs_backtrack(&table, &old_lines, &new_lines);

    // Convert edit ops to tagged lines with line numbers
    let mut tagged: Vec<(LineOp, String, Option<usize>, Option<usize>)> = Vec::new();
    for op in &ops {
        match op {
            EditOp::Equal(oi, ni) => {
                tagged.push((
                    LineOp::Context,
                    old_lines[*oi].to_string(),
                    Some(*oi + 1),
                    Some(*ni + 1),
                ));
            }
            EditOp::Delete(oi) => {
                tagged.push((
                    LineOp::Remove,
                    old_lines[*oi].to_string(),
                    Some(*oi + 1),
                    None,
                ));
            }
            EditOp::Insert(ni) => {
                tagged.push((LineOp::Add, new_lines[*ni].to_string(), None, Some(*ni + 1)));
            }
        }
    }

    // Group into hunks with context
    group_into_hunks(&tagged, context_lines)
}

/// Group tagged diff lines into hunks, including context lines around changes.
fn group_into_hunks(
    tagged: &[(LineOp, String, Option<usize>, Option<usize>)],
    context_lines: usize,
) -> Vec<DiffHunk> {
    if tagged.is_empty() {
        return Vec::new();
    }

    // Find indices of changed lines
    let change_indices: Vec<usize> = tagged
        .iter()
        .enumerate()
        .filter(|(_, (op, ..))| *op != LineOp::Context)
        .map(|(i, _)| i)
        .collect();

    if change_indices.is_empty() {
        return Vec::new();
    }

    // Build ranges: each change gets context_lines before and after
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &ci in &change_indices {
        let start = ci.saturating_sub(context_lines);
        let end = (ci + context_lines + 1).min(tagged.len());
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 {
                last.1 = end; // merge overlapping ranges
            } else {
                ranges.push((start, end));
            }
        } else {
            ranges.push((start, end));
        }
    }

    // Convert ranges to hunks
    let mut hunks = Vec::new();
    for (start, end) in ranges {
        let mut lines = Vec::new();
        let mut old_start = None;
        let mut new_start = None;
        let mut old_count = 0usize;
        let mut new_count = 0usize;

        for (op, content, old_ln, new_ln) in &tagged[start..end] {
            if old_start.is_none() {
                old_start = Some(old_ln.unwrap_or(1));
            }
            if new_start.is_none() {
                new_start = Some(new_ln.unwrap_or(1));
            }

            match op {
                LineOp::Context => {
                    old_count += 1;
                    new_count += 1;
                }
                LineOp::Remove => {
                    old_count += 1;
                }
                LineOp::Add => {
                    new_count += 1;
                }
            }

            lines.push(DiffLine {
                op: op.clone(),
                content: content.clone(),
                old_lineno: *old_ln,
                new_lineno: *new_ln,
            });
        }

        hunks.push(DiffHunk {
            old_start: old_start.unwrap_or(1),
            old_count,
            new_start: new_start.unwrap_or(1),
            new_count,
            lines,
        });
    }

    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identical_content() {
        let hunks = compute_line_diff("hello\nworld\n", "hello\nworld\n", 3);
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_single_add() {
        let hunks = compute_line_diff("hello\n", "hello\nworld\n", 3);
        assert_eq!(hunks.len(), 1);
        let hunk = &hunks[0];
        let adds: Vec<_> = hunk.lines.iter().filter(|l| l.op == LineOp::Add).collect();
        assert_eq!(adds.len(), 1);
        assert_eq!(adds[0].content, "world");
    }

    #[test]
    fn test_single_remove() {
        let hunks = compute_line_diff("hello\nworld\n", "hello\n", 3);
        assert_eq!(hunks.len(), 1);
        let removes: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.op == LineOp::Remove)
            .collect();
        assert_eq!(removes.len(), 1);
        assert_eq!(removes[0].content, "world");
    }

    #[test]
    fn test_modification() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nchanged\nline3\n";
        let hunks = compute_line_diff(old, new, 3);
        assert_eq!(hunks.len(), 1);
        let removes: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.op == LineOp::Remove)
            .collect();
        let adds: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.op == LineOp::Add)
            .collect();
        assert_eq!(removes.len(), 1);
        assert_eq!(removes[0].content, "line2");
        assert_eq!(adds.len(), 1);
        assert_eq!(adds[0].content, "changed");
    }

    #[test]
    fn test_empty_to_content() {
        let hunks = compute_line_diff("", "hello\nworld\n", 3);
        assert_eq!(hunks.len(), 1);
        let adds: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.op == LineOp::Add)
            .collect();
        assert_eq!(adds.len(), 2);
    }

    #[test]
    fn test_content_to_empty() {
        let hunks = compute_line_diff("hello\nworld\n", "", 3);
        assert_eq!(hunks.len(), 1);
        let removes: Vec<_> = hunks[0]
            .lines
            .iter()
            .filter(|l| l.op == LineOp::Remove)
            .collect();
        assert_eq!(removes.len(), 2);
    }

    #[test]
    fn test_binary_detection() {
        assert!(is_binary(b"hello\x00world"));
        assert!(!is_binary(b"hello world"));
        assert!(!is_binary(b""));
    }
}
