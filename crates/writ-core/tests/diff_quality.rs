//! Finding 23 repros for the sprint 2 diff-quality spec. Ignored until then.
//!
//! `compute_line_diff` switches to a greedy linear fallback when either side
//! exceeds `LCS_LINE_LIMIT` (10,000 lines). Its resync window is 8 lines, so
//! any insertion or deletion of 8+ contiguous lines never resyncs and the rest
//! of the file is reported as removed and re-added. Below the limit, the
//! O(m*n) LCS table costs ~750 MB RSS and ~360 ms for a 9,000-line file.

use writ_core::diff::{compute_line_diff, LineOp};

fn unique_lines(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("let v{i} = {i};")).collect()
}

fn counts(old: &str, new: &str) -> (usize, usize) {
    let hunks = compute_line_diff(old, new, 3);
    let mut adds = 0;
    let mut removes = 0;
    for line in hunks.iter().flat_map(|h| &h.lines) {
        match line.op {
            LineOp::Add => adds += 1,
            LineOp::Remove => removes += 1,
            LineOp::Context => {}
        }
    }
    (adds, removes)
}

fn with_insert(base: &[String], at: usize, k: usize) -> String {
    let mut lines = base.to_vec();
    lines.splice(at..at, (0..k).map(|j| format!("let inserted{j} = {j};")));
    lines.join("\n") + "\n"
}

#[test]
fn test_large_file_block_insert_reports_only_inserted_lines() {
    // 10,001 unique lines; insert 8 lines after line 10.
    // git: +8 -0. writ today: +9999 -9991.
    let base = unique_lines(10_001);
    let old = base.join("\n") + "\n";
    let new = with_insert(&base, 10, 8);
    assert_eq!(counts(&old, &new), (8, 0));
}

#[test]
fn test_insert_that_crosses_line_limit_is_still_minimal() {
    // 9,999 lines (LCS path) grows to 10,019 (fallback path).
    // git: +20 -0. writ today: +10009 -9989.
    let base = unique_lines(9_999);
    let old = base.join("\n") + "\n";
    let new = with_insert(&base, 10, 20);
    assert_eq!(counts(&old, &new), (20, 0));
}

#[test]
fn test_small_file_block_insert_is_minimal() {
    // Control: under the limit the LCS path is exact.
    let base = unique_lines(500);
    let old = base.join("\n") + "\n";
    let new = with_insert(&base, 10, 20);
    assert_eq!(counts(&old, &new), (20, 0));
}
