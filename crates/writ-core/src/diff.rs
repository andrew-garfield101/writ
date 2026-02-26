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

/// Maximum line count for the O(m*n) LCS algorithm. Files larger than this
/// use a linear-time fallback that compares lines by hash set membership.
const LCS_LINE_LIMIT: usize = 10_000;

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

    // For large files, use linear-time fallback to avoid O(m*n) blowup.
    let tagged = if old_lines.len() > LCS_LINE_LIMIT || new_lines.len() > LCS_LINE_LIMIT {
        compute_linear_diff(&old_lines, &new_lines)
    } else {
        let table = lcs_table(&old_lines, &new_lines);
        let ops = lcs_backtrack(&table, &old_lines, &new_lines);
        ops_to_tagged(&ops, &old_lines, &new_lines)
    };

    // Group into hunks with context
    group_into_hunks(&tagged, context_lines)
}

/// Convert LCS edit ops to tagged diff lines.
fn ops_to_tagged(
    ops: &[EditOp],
    old_lines: &[&str],
    new_lines: &[&str],
) -> Vec<(LineOp, String, Option<usize>, Option<usize>)> {
    let mut tagged = Vec::new();
    for op in ops {
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
    tagged
}

/// Linear-time diff for large files. Walks both line arrays in parallel,
/// matching equal lines greedily and emitting adds/removes for differences.
/// Not as precise as LCS (may produce slightly larger diffs) but runs in
/// O(m + n) time regardless of file size.
fn compute_linear_diff(
    old_lines: &[&str],
    new_lines: &[&str],
) -> Vec<(LineOp, String, Option<usize>, Option<usize>)> {
    let mut tagged = Vec::new();
    let mut oi = 0;
    let mut ni = 0;

    while oi < old_lines.len() && ni < new_lines.len() {
        if old_lines[oi] == new_lines[ni] {
            tagged.push((
                LineOp::Context,
                old_lines[oi].to_string(),
                Some(oi + 1),
                Some(ni + 1),
            ));
            oi += 1;
            ni += 1;
        } else {
            // Look ahead a few lines to find a resync point.
            let resync = find_resync(old_lines, new_lines, oi, ni, 8);
            match resync {
                Some((new_oi, new_ni)) => {
                    // Emit deletes for skipped old lines.
                    for idx in oi..new_oi {
                        tagged.push((
                            LineOp::Remove,
                            old_lines[idx].to_string(),
                            Some(idx + 1),
                            None,
                        ));
                    }
                    // Emit inserts for skipped new lines.
                    for idx in ni..new_ni {
                        tagged.push((LineOp::Add, new_lines[idx].to_string(), None, Some(idx + 1)));
                    }
                    oi = new_oi;
                    ni = new_ni;
                }
                None => {
                    // No resync found — emit current lines as remove + add.
                    tagged.push((
                        LineOp::Remove,
                        old_lines[oi].to_string(),
                        Some(oi + 1),
                        None,
                    ));
                    tagged.push((LineOp::Add, new_lines[ni].to_string(), None, Some(ni + 1)));
                    oi += 1;
                    ni += 1;
                }
            }
        }
    }

    // Remaining old lines are deletes.
    for idx in oi..old_lines.len() {
        tagged.push((
            LineOp::Remove,
            old_lines[idx].to_string(),
            Some(idx + 1),
            None,
        ));
    }
    // Remaining new lines are inserts.
    for idx in ni..new_lines.len() {
        tagged.push((LineOp::Add, new_lines[idx].to_string(), None, Some(idx + 1)));
    }

    tagged
}

/// Look ahead up to `window` lines in both old and new to find a matching
/// line (resync point). Returns the (old_idx, new_idx) of the match, or None.
fn find_resync(
    old: &[&str],
    new: &[&str],
    oi: usize,
    ni: usize,
    window: usize,
) -> Option<(usize, usize)> {
    let max_oi = (oi + window).min(old.len());
    let max_ni = (ni + window).min(new.len());

    // Check if any upcoming new line matches current old line.
    for nj in (ni + 1)..max_ni {
        if old[oi] == new[nj] {
            return Some((oi, nj));
        }
    }
    // Check if any upcoming old line matches current new line.
    for oj in (oi + 1)..max_oi {
        if old[oj] == new[ni] {
            return Some((oj, ni));
        }
    }
    // Check diagonal matches.
    for d in 1..window {
        let oj = oi + d;
        let nj = ni + d;
        if oj < old.len() && nj < new.len() && old[oj] == new[nj] {
            return Some((oj, nj));
        }
    }
    None
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

    #[test]
    fn test_large_file_uses_linear_fallback() {
        // Generate files exceeding LCS_LINE_LIMIT to trigger the linear fallback.
        let line_count = LCS_LINE_LIMIT + 500;
        let old: String = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        // Modify a handful of lines in the middle.
        let mut new_lines: Vec<String> = (0..line_count).map(|i| format!("line {i}")).collect();
        let mid = line_count / 2;
        new_lines[mid] = "CHANGED-A".to_string();
        new_lines[mid + 1] = "CHANGED-B".to_string();
        let new_content = new_lines.join("\n");

        let start = std::time::Instant::now();
        let hunks = compute_line_diff(&old, &new_content, 3);
        let elapsed = start.elapsed();

        // Must complete in under 5 seconds (linear algo is near-instant for 10K lines).
        assert!(
            elapsed.as_secs() < 5,
            "linear diff took too long: {elapsed:?}"
        );

        // Should detect the two changed lines.
        let total_removes: usize = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.op == LineOp::Remove)
            .count();
        let total_adds: usize = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.op == LineOp::Add)
            .count();

        assert!(
            total_removes >= 2,
            "expected at least 2 removes, got {total_removes}"
        );
        assert!(
            total_adds >= 2,
            "expected at least 2 adds, got {total_adds}"
        );
    }

    #[test]
    fn test_linear_diff_identical_large_file() {
        // Identical large files should produce no hunks even through the linear path.
        let line_count = LCS_LINE_LIMIT + 100;
        let content: String = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let hunks = compute_line_diff(&content, &content, 3);
        assert!(
            hunks.is_empty(),
            "identical large files should produce no hunks"
        );
    }

    #[test]
    fn test_linear_diff_complete_replacement() {
        // All old lines removed, all new lines added.
        let line_count = LCS_LINE_LIMIT + 100;
        let old: String = (0..line_count)
            .map(|i| format!("old-{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new_content: String = (0..line_count)
            .map(|i| format!("new-{i}"))
            .collect::<Vec<_>>()
            .join("\n");

        let hunks = compute_line_diff(&old, &new_content, 3);
        assert!(
            !hunks.is_empty(),
            "complete replacement should produce hunks"
        );

        let total_removes: usize = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.op == LineOp::Remove)
            .count();
        let total_adds: usize = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.op == LineOp::Add)
            .count();

        assert_eq!(total_removes, line_count);
        assert_eq!(total_adds, line_count);
    }
}
