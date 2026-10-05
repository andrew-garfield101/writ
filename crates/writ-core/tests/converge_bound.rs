//! Finding 36: convergence builds an unbounded O(m*n) LCS table.
//!
//! `convergence::diff3::build_action_table` calls `diff::lcs_table` with no
//! line limit, so a three-way merge allocates 8 * n^2 bytes per side. Measured
//! on released 0.2.1 (`writ converge`, two disjoint one-line edits, common
//! base): 10,000 lines 792 MB, 20,000 lines 3,140 MB / 3.2 s, 30,000 lines
//! 7,054 MB / 8.2 s. A 4 GB raspberry_pi machine is OOM-killed above about
//! 19,000 lines; writ's own `repo.rs` is 34,142 lines.
//!
//! Fixed by S.9 converge-bound (Haris: b2934a18a459, ce9041d7fe34, linear-space
//! Myers for diff and diff3). After the fix, release build: 20,000 lines
//! 7.0 MB / 6 ms, 50,000 lines 16.3 MB / 15 ms, `Repository::converge` 20,000
//! lines 9.9 MB / 29 ms.
//!
//! This binary installs a counting global allocator so peak heap is measured
//! in-process on every platform. The allocator also refuses any allocation that
//! would take live heap past `HARD_CAP`, so a regression back to a quadratic
//! table aborts in milliseconds (SIGABRT, "memory allocation failed") instead
//! of eating the machine's memory.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use tempfile::tempdir;

use writ_core::convergence::{three_way_merge, FileMergeResult};
use writ_core::seal::{AgentIdentity, AgentType, TaskStatus, Verification};
use writ_core::spec::Spec;
use writ_core::Repository;

/// Acceptance budget from CC (2026-10-05): converge of a 20,000-line file
/// with two disjoint one-line edits stays under 100 MB peak.
const PEAK_BUDGET: usize = 100 * 1024 * 1024;
/// Fail-fast guard: no single test in this binary may hold more than this.
const HARD_CAP: usize = 1024 * 1024 * 1024;

struct PeakAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// Tests share one allocator, so they must not measure concurrently.
static SERIAL: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        let p = System.alloc(layout);
        if p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if !reserve(layout.size()) {
            return std::ptr::null_mut();
        }
        let p = System.alloc_zeroed(layout);
        if p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old = layout.size();
        if new_size > old && !reserve(new_size - old) {
            return std::ptr::null_mut();
        }
        let p = System.realloc(ptr, layout, new_size);
        if p.is_null() {
            if new_size > old {
                LIVE.fetch_sub(new_size - old, Ordering::SeqCst);
            }
        } else if new_size < old {
            LIVE.fetch_sub(old - new_size, Ordering::SeqCst);
        }
        p
    }
}

/// Account for `bytes` more live heap; false if that would cross `HARD_CAP`.
fn reserve(bytes: usize) -> bool {
    let now = LIVE.fetch_add(bytes, Ordering::SeqCst) + bytes;
    if now > HARD_CAP {
        LIVE.fetch_sub(bytes, Ordering::SeqCst);
        return false;
    }
    PEAK.fetch_max(now, Ordering::SeqCst);
    true
}

#[global_allocator]
static GLOBAL: PeakAlloc = PeakAlloc;

/// Run `f` and return its result with the peak heap growth above the live
/// baseline at entry.
fn measure<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let out = f();
    (out, PEAK.load(Ordering::SeqCst) - base)
}

fn mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// `n` distinct lines, the same shape as the 0.2.1 measurement fixture.
fn lines(n: usize) -> Vec<String> {
    (0..n)
        .map(|i| format!("line {i:06} lorem ipsum dolor sit amet consectetur"))
        .collect()
}

fn join(lines: &[String]) -> String {
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/// Base, left (edits line 10), right (edits line n-10), expected merge.
fn disjoint_edits(n: usize) -> (String, String, String, String) {
    let base = lines(n);
    let mut left = base.clone();
    left[10] = "LEFT EDIT".to_string();
    let mut right = base.clone();
    right[n - 10] = "RIGHT EDIT".to_string();
    let mut merged = base.clone();
    merged[10] = "LEFT EDIT".to_string();
    merged[n - 10] = "RIGHT EDIT".to_string();
    (join(&base), join(&left), join(&right), join(&merged))
}

fn assert_clean_merge(n: usize) -> usize {
    let (base, left, right, expected) = disjoint_edits(n);
    let t = std::time::Instant::now();
    let (result, peak) = measure(|| three_way_merge(&base, &left, &right));
    eprintln!(
        "three_way_merge {n} lines: peak {:.1} MB, {:?}",
        mb(peak),
        t.elapsed()
    );
    match result {
        FileMergeResult::Clean(merged) => assert!(
            merged == expected,
            "{n}-line disjoint merge produced wrong content ({} bytes vs {} expected)",
            merged.len(),
            expected.len()
        ),
        FileMergeResult::Conflict(_) => {
            panic!("{n}-line merge with disjoint one-line edits reported conflicts")
        }
    }
    peak
}

/// Harness self-check (runs today): the allocator sees the merge's heap, and a
/// 2,000-line merge is correct. Pre-fix this costs about 32 MB per side.
#[test]
fn test_harness_measures_three_way_merge_heap() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let peak = assert_clean_merge(2_000);
    assert!(
        peak > 2_000 * 50,
        "allocator recorded {peak} bytes; it is not seeing the merge's heap"
    );
}

#[test]
fn test_three_way_merge_20k_lines_disjoint_edits_under_100mb() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let peak = assert_clean_merge(20_000);
    assert!(
        peak < PEAK_BUDGET,
        "20,000-line merge peaked at {:.0} MB, budget {:.0} MB",
        mb(peak),
        mb(PEAK_BUDGET)
    );
}

/// Above the size writ's own repo.rs has today. Must stay bounded, not merely
/// survive: same 100 MB budget.
#[test]
fn test_three_way_merge_50k_lines_disjoint_edits_under_100mb() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let peak = assert_clean_merge(50_000);
    assert!(
        peak < PEAK_BUDGET,
        "50,000-line merge peaked at {:.0} MB, budget {:.0} MB",
        mb(peak),
        mb(PEAK_BUDGET)
    );
}

/// End to end through `Repository::converge` (the path `writ converge`,
/// `converge-all`, `finish` and `watch` take): common base seal, two specs each
/// editing one line of a 20,000-line file. Catches a second unbounded LCS
/// anywhere in the pipeline, not just in diff3.
#[test]
fn test_repository_converge_20k_line_file_under_100mb() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let n = 20_000;
    let (base, left, right, expected) = disjoint_edits(n);
    let dir = tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let who = |id: &str| AgentIdentity {
        id: id.to_string(),
        agent_type: AgentType::Agent,
    };
    let seal_to = |agent: &str, spec: &str, summary: &str| {
        repo.seal_paths(
            who(agent),
            summary.to_string(),
            Some(spec.to_string()),
            TaskStatus::InProgress,
            Verification::default(),
            &["big.txt".to_string()],
            false,
        )
        .unwrap()
    };

    fs::write(dir.path().join("big.txt"), &base).unwrap();
    repo.add_spec(&Spec::new("z".into(), "base".into(), String::new()))
        .unwrap();
    seal_to("z", "z", "base");
    repo.add_spec(&Spec::new("a".into(), "left".into(), String::new()))
        .unwrap();
    repo.add_spec(&Spec::new("b".into(), "right".into(), String::new()))
        .unwrap();
    fs::write(dir.path().join("big.txt"), &right).unwrap();
    seal_to("b", "b", "right edit");
    // Each spec head differs from the base seal on exactly one line.
    fs::write(dir.path().join("big.txt"), &left).unwrap();
    seal_to("a", "a", "left edit");

    let t = std::time::Instant::now();
    let (report, peak) = measure(|| repo.converge("a", "b").unwrap());
    eprintln!(
        "Repository::converge {n} lines: peak {:.1} MB, {:?}",
        mb(peak),
        t.elapsed()
    );
    assert!(report.base_seal_id.is_some(), "no common base found");
    assert!(report.is_clean, "disjoint edits reported conflicts");
    let merged = report
        .auto_merged
        .iter()
        .find(|m| m.path == "big.txt")
        .expect("big.txt not auto-merged");
    assert!(merged.content == expected, "merged content is wrong");
    assert!(
        peak < PEAK_BUDGET,
        "Repository::converge on a 20,000-line file peaked at {:.0} MB, budget {:.0} MB",
        mb(peak),
        mb(PEAK_BUDGET)
    );
}
