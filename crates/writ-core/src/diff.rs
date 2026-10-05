//! Diff computation for writ.
//!
//! Provides line-level diffing between file contents, producing
//! structured output suitable for both human display and LLM consumption.

use std::collections::HashMap;

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

/// One step of an edit script from `old` to `new`, in file order.
#[derive(Debug, PartialEq)]
pub(crate) enum EditOp {
    Equal(usize, usize), // old_idx, new_idx
    Insert(usize),       // new_idx
    Delete(usize),       // old_idx
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

    let ops = diff_ops(&old_lines, &new_lines);
    let tagged = ops_to_tagged(&ops, &old_lines, &new_lines);

    // Group into hunks with context
    group_into_hunks(&tagged, context_lines)
}

/// Compute a minimal edit script from `old` to `new`.
///
/// This is the single entry point for line diffing; both `compute_line_diff`
/// and the three-way merge in `convergence::diff3` go through it. It uses
/// Myers' O((N+M)·D) algorithm with the linear-space divide-and-conquer
/// refinement, so memory is O(N+M) at every file size (finding 36) and the
/// script is minimal at every file size (finding 23).
///
/// Ops come out in file order. Within one changed region all deletes come
/// before all inserts.
pub(crate) fn diff_ops(old: &[&str], new: &[&str]) -> Vec<EditOp> {
    let (old_ids, new_ids) = intern_lines(old, new);

    // A line that never occurs on the other side can never be part of a
    // common subsequence, so drop it before running Myers. This keeps the
    // result minimal and makes D (and the run time) small when one side is
    // mostly new content, e.g. a full rewrite.
    let new_present = presence(&old_ids, &new_ids);
    let old_present = presence(&new_ids, &old_ids);
    let old_keep: Vec<usize> = (0..old.len()).filter(|&i| old_present[i]).collect();
    let new_keep: Vec<usize> = (0..new.len()).filter(|&j| new_present[j]).collect();
    let a: Vec<u32> = old_keep.iter().map(|&i| old_ids[i]).collect();
    let b: Vec<u32> = new_keep.iter().map(|&j| new_ids[j]).collect();

    let mut matches = Vec::new();
    let mut myers = Myers::new(a.len() + b.len());
    myers.conquer(&a, 0, &b, 0, &mut matches);

    let mut ops = Vec::with_capacity(old.len() + new.len());
    let (mut oi, mut ni) = (0, 0);
    for (ra, rb) in matches {
        let (mo, mn) = (old_keep[ra], new_keep[rb]);
        ops.extend((oi..mo).map(EditOp::Delete));
        ops.extend((ni..mn).map(EditOp::Insert));
        ops.push(EditOp::Equal(mo, mn));
        oi = mo + 1;
        ni = mn + 1;
    }
    ops.extend((oi..old.len()).map(EditOp::Delete));
    ops.extend((ni..new.len()).map(EditOp::Insert));
    ops
}

/// Map every distinct line to a small integer so the inner loop compares
/// `u32`s instead of strings.
fn intern_lines<'a>(old: &[&'a str], new: &[&'a str]) -> (Vec<u32>, Vec<u32>) {
    let mut ids: HashMap<&'a str, u32> = HashMap::with_capacity(old.len() + new.len());
    let mut intern = |line: &'a str| -> u32 {
        let next = ids.len() as u32;
        *ids.entry(line).or_insert(next)
    };
    let old_ids = old.iter().map(|l| intern(l)).collect();
    let new_ids = new.iter().map(|l| intern(l)).collect();
    (old_ids, new_ids)
}

/// For each id in `side`, whether it also occurs in `other`.
fn presence(other: &[u32], side: &[u32]) -> Vec<bool> {
    let max = other
        .iter()
        .chain(side)
        .copied()
        .max()
        .map_or(0, |m| m as usize + 1);
    let mut seen = vec![false; max];
    for &id in other {
        seen[id as usize] = true;
    }
    side.iter().map(|&id| seen[id as usize]).collect()
}

/// Linear-space Myers diff (Myers 1986, section 4b). Produces the matched
/// index pairs of a longest common subsequence, in order.
struct Myers {
    /// Furthest-reaching x per diagonal, forward search. Indexed by k + offset.
    vf: Vec<usize>,
    /// Furthest-reaching x per diagonal, backward search (from the far corner).
    vb: Vec<usize>,
    offset: isize,
}

impl Myers {
    fn new(total_len: usize) -> Self {
        // Diagonals reached in one call are bounded by d_max + |delta|,
        // both at most total_len.
        let size = 2 * total_len + 4;
        Myers {
            vf: vec![0; size],
            vb: vec![0; size],
            offset: total_len as isize + 2,
        }
    }

    fn idx(&self, k: isize) -> usize {
        (k + self.offset) as usize
    }

    /// Push matches for `a` vs `b` (absolute offsets `a0`, `b0`) into `out`.
    fn conquer(
        &mut self,
        a: &[u32],
        a0: usize,
        b: &[u32],
        b0: usize,
        out: &mut Vec<(usize, usize)>,
    ) {
        let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        out.extend((0..prefix).map(|i| (a0 + i, b0 + i)));
        let (a, b) = (&a[prefix..], &b[prefix..]);
        let (a0, b0) = (a0 + prefix, b0 + prefix);

        let suffix = a
            .iter()
            .rev()
            .zip(b.iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        let (a, b) = (&a[..a.len() - suffix], &b[..b.len() - suffix]);

        if !a.is_empty() && !b.is_empty() {
            let (x, y) = self.middle_snake(a, b);
            self.conquer(&a[..x], a0, &b[..y], b0, out);
            self.conquer(&a[x..], a0 + x, &b[y..], b0 + y, out);
        }

        let (ea, eb) = (a0 + a.len(), b0 + b.len());
        out.extend((0..suffix).map(|i| (ea + i, eb + i)));
    }

    /// Find a split point (x, y) on an optimal edit path through `a` x `b`,
    /// with 0 < x + y < n + m. Both inputs are non-empty and differ in their
    /// first and last elements, so D >= 1 and the split is strictly inside.
    fn middle_snake(&mut self, a: &[u32], b: &[u32]) -> (usize, usize) {
        let n = a.len() as isize;
        let m = b.len() as isize;
        let delta = n - m;
        let odd = delta & 1 == 1;
        let i1 = self.idx(1);
        self.vf[i1] = 0;
        self.vb[i1] = 0;
        let d_max = (n + m + 1) / 2;

        for d in 0..=d_max {
            // Forward pass from (0, 0).
            for k in (-d..=d).step_by(2) {
                let mut x =
                    if k == -d || (k != d && self.vf[self.idx(k - 1)] < self.vf[self.idx(k + 1)]) {
                        self.vf[self.idx(k + 1)]
                    } else {
                        self.vf[self.idx(k - 1)] + 1
                    } as isize;
                let mut y = x - k;
                let (x0, y0) = (x, y);
                while x < n && y < m && a[x as usize] == b[y as usize] {
                    x += 1;
                    y += 1;
                }
                let ik = self.idx(k);
                self.vf[ik] = x as usize;
                if odd && (k - delta).abs() < d {
                    let back = self.vb[self.idx(-(k - delta))] as isize;
                    if x + back >= n {
                        return (x0 as usize, y0 as usize);
                    }
                }
            }
            // Backward pass from (n, m); x counts steps back from the end.
            for k in (-d..=d).step_by(2) {
                let mut x =
                    if k == -d || (k != d && self.vb[self.idx(k - 1)] < self.vb[self.idx(k + 1)]) {
                        self.vb[self.idx(k + 1)]
                    } else {
                        self.vb[self.idx(k - 1)] + 1
                    } as isize;
                let mut y = x - k;
                while x < n && y < m && a[(n - x - 1) as usize] == b[(m - y - 1) as usize] {
                    x += 1;
                    y += 1;
                }
                let ik = self.idx(k);
                self.vb[ik] = x as usize;
                if !odd && (k - delta).abs() <= d {
                    let fwd = self.vf[self.idx(-(k - delta))] as isize;
                    if x + fwd >= n {
                        return ((n - x) as usize, (m - y) as usize);
                    }
                }
            }
        }
        unreachable!("Myers middle snake always meets within (n + m + 1) / 2 rounds")
    }
}

/// Convert edit ops to tagged diff lines.
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
        // Above the old 10,000-line LCS limit; Myers handles it in linear space.
        let line_count = 10_500;
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

        // Must complete in under 5 seconds (D = 4, so Myers is near-instant).
        assert!(
            elapsed.as_secs() < 5,
            "large-file diff took too long: {elapsed:?}"
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

        // Myers is exact above the old limit too: exactly the two lines.
        assert_eq!(total_removes, 2);
        assert_eq!(total_adds, 2);
    }

    #[test]
    fn test_linear_diff_identical_large_file() {
        // Identical large files should produce no hunks even through the linear path.
        let line_count = 10_100;
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
        let line_count = 10_100;
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

    /// Reference LCS length by the classic O(m*n) table (small inputs only).
    fn lcs_len_reference(a: &[&str], b: &[&str]) -> usize {
        let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
        for i in 1..=a.len() {
            for j in 1..=b.len() {
                t[i][j] = if a[i - 1] == b[j - 1] {
                    t[i - 1][j - 1] + 1
                } else {
                    t[i - 1][j].max(t[i][j - 1])
                };
            }
        }
        t[a.len()][b.len()]
    }

    /// Check that `ops` is a valid, in-order edit script from `old` to `new`.
    fn assert_valid_script(ops: &[EditOp], old: &[&str], new: &[&str]) {
        let (mut oi, mut ni) = (0, 0);
        for op in ops {
            match *op {
                EditOp::Equal(o, n) => {
                    assert_eq!((o, n), (oi, ni), "equal out of order");
                    assert_eq!(old[o], new[n], "equal on different lines");
                    oi += 1;
                    ni += 1;
                }
                EditOp::Delete(o) => {
                    assert_eq!(o, oi, "delete out of order");
                    oi += 1;
                }
                EditOp::Insert(n) => {
                    assert_eq!(n, ni, "insert out of order");
                    ni += 1;
                }
            }
        }
        assert_eq!(
            (oi, ni),
            (old.len(), new.len()),
            "script does not cover both sides"
        );
    }

    #[test]
    fn test_diff_ops_minimal_against_reference_lcs() {
        // Deterministic LCG; small alphabets force many repeated lines, which
        // is where a wrong middle-snake split would show up.
        let mut seed: u64 = 0x5eed;
        let mut next = |bound: u64| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) % bound
        };
        let words = ["a", "b", "c", "d", "e", "f", "", "}"];
        for case in 0..3_000 {
            let alphabet = 2 + (case % 7) as u64;
            let old: Vec<&str> = (0..next(40))
                .map(|_| words[next(alphabet) as usize])
                .collect();
            let new: Vec<&str> = (0..next(40))
                .map(|_| words[next(alphabet) as usize])
                .collect();

            let ops = diff_ops(&old, &new);
            assert_valid_script(&ops, &old, &new);
            let equal = ops
                .iter()
                .filter(|op| matches!(op, EditOp::Equal(..)))
                .count();
            assert_eq!(
                equal,
                lcs_len_reference(&old, &new),
                "not minimal for old={old:?} new={new:?}"
            );
        }
    }

    #[test]
    fn test_diff_ops_deletes_before_inserts_in_a_region() {
        let ops = diff_ops(&["a", "x", "c"], &["a", "y", "c"]);
        assert_eq!(
            ops,
            vec![
                EditOp::Equal(0, 0),
                EditOp::Delete(1),
                EditOp::Insert(1),
                EditOp::Equal(2, 2),
            ]
        );
    }

    #[test]
    fn test_diff_ops_empty_sides() {
        assert!(diff_ops(&[], &[]).is_empty());
        assert_eq!(diff_ops(&["a"], &[]), vec![EditOp::Delete(0)]);
        assert_eq!(diff_ops(&[], &["a"]), vec![EditOp::Insert(0)]);
    }
}
