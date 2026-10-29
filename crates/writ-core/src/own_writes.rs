//! Writ's own writes to the working tree (finding 206).
//!
//! `writ finish`, `writ restore` and `writ converge` write files into the
//! working tree. A Claude Code hook whose Bash window was open at the same
//! time sees those paths change and, with two windows open, asks the agents
//! to claim a file no agent wrote. So before each such write writ records
//! the path in a marker, `.writ/own_writes/<pid>-<start>.json`, with the
//! process id and the interval it wrote in; the hook ignores a path that a
//! marker lists when the file's modification time falls inside that
//! interval: no ambiguity line, no auto seal for it.
//!
//! The marker is finished, not deleted, when the command exits (the guard
//! records its end): a window that overlapped the command closes after it,
//! and its Post must still know which writes were writ's. The hook prunes
//! markers older than the hook's window horizon. A write by an agent to the
//! same path after the command's end has a later modification time and is
//! attributed as usual. Nothing here ever attributes a path to anyone; the
//! only effect is that writ does not ask about its own writes.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Directory under `.writ` holding the markers.
pub const DIR: &str = "own_writes";

/// A marker whose command never recorded its end (it crashed, or the write
/// came through an API with no guard) counts as running only this long
/// after its last write; then it ends at that last write.
const OPEN_HORIZON_SECS: i64 = 10 * 60;

/// Slack after a write for a filesystem that rounds modification times to
/// whole seconds.
const COARSE_NS: i128 = 1_000_000_000;

/// One command's writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnWrite {
    pub pid: u32,
    pub command: String,
    pub start: DateTime<Utc>,
    /// Updated at each write.
    pub last: DateTime<Utc>,
    /// Set when the command exits.
    #[serde(default)]
    pub end: Option<DateTime<Utc>>,
    pub paths: BTreeSet<String>,
}

impl OwnWrite {
    /// The end of the interval as of `now`.
    fn end_at(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self.end {
            Some(e) => e,
            None if (now - self.last).num_seconds() < OPEN_HORIZON_SECS => now,
            None => self.last,
        }
    }

    /// True when `rel` is one of this command's writes and `mtime_ns` (the
    /// file's modification time; None for a deleted file) is inside the
    /// interval the command wrote in.
    pub fn covers(&self, rel: &str, mtime_ns: Option<i128>, now: DateTime<Utc>) -> bool {
        if !self.paths.contains(rel) {
            return false;
        }
        let start = ns(self.start);
        let end = ns(self.end_at(now));
        match mtime_ns {
            Some(m) => {
                let slack = if m % COARSE_NS == 0 { COARSE_NS } else { 0 };
                m + slack >= start && m - slack <= end
            }
            // A deletion has no time: it is writ's when this command deleted
            // it and has not ended long ago (the hook prunes old markers).
            None => true,
        }
    }
}

fn ns(t: DateTime<Utc>) -> i128 {
    t.timestamp_nanos_opt().unwrap_or(0) as i128
}

/// The marker this process writes per repository, once it has written
/// anything there.
struct Current {
    writ_dir: PathBuf,
    path: PathBuf,
    rec: OwnWrite,
}

static CURRENT: Mutex<Vec<Current>> = Mutex::new(Vec::new());
static COMMAND: Mutex<Option<String>> = Mutex::new(None);

/// Marks the command whose writes follow; on drop records the end of every
/// marker this process wrote. Hold it for the life of the command.
pub struct Guard;

/// Start a command that writes the working tree (`finish`, `restore`,
/// `converge`). The marker itself is written on the first write.
pub fn begin(command: &str) -> Guard {
    if let Ok(mut c) = COMMAND.lock() {
        *c = Some(command.to_string());
    }
    Guard
}

impl Drop for Guard {
    fn drop(&mut self) {
        finish_current();
    }
}

fn finish_current() {
    let Ok(mut cur) = CURRENT.lock() else {
        return;
    };
    let now = Utc::now();
    for c in cur.iter_mut() {
        c.rec.end = Some(now);
        save(&c.path, &c.rec);
    }
    cur.clear();
}

/// Record that this process is about to write (or delete) `rel` in the
/// working tree of the repository at `writ_dir`. Best effort: a marker
/// that cannot be written only means the hook may ask about the path, as
/// before; the write itself goes ahead.
pub fn note(writ_dir: &Path, rel: &str) {
    let Ok(mut cur) = CURRENT.lock() else {
        return;
    };
    let now = Utc::now();
    let command = COMMAND
        .lock()
        .ok()
        .and_then(|c| c.clone())
        .unwrap_or_else(|| "writ".to_string());
    if !cur.iter().any(|c| c.writ_dir == writ_dir) {
        let pid = std::process::id();
        cur.push(Current {
            writ_dir: writ_dir.to_path_buf(),
            path: writ_dir.join(DIR).join(format!("{pid}-{}.json", ns(now))),
            rec: OwnWrite {
                pid,
                command,
                start: now,
                last: now,
                end: None,
                paths: BTreeSet::new(),
            },
        });
    }
    let Some(c) = cur.iter_mut().find(|c| c.writ_dir == writ_dir) else {
        return;
    };
    c.rec.last = now;
    if c.rec.paths.insert(rel.to_string()) || c.rec.end.is_some() {
        save(&c.path, &c.rec);
    }
}

fn save(path: &Path, rec: &OwnWrite) {
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_vec(rec) {
        let _ = crate::fsutil::atomic_write(path, &json);
    }
}

/// Every marker under `writ_dir`; markers whose interval ended more than
/// `prune_secs` ago are removed.
pub fn load(writ_dir: &Path, prune_secs: i64) -> Vec<OwnWrite> {
    let Ok(rd) = fs::read_dir(writ_dir.join(DIR)) else {
        return Vec::new();
    };
    let now = Utc::now();
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(rec) = fs::read_to_string(e.path())
            .ok()
            .and_then(|t| serde_json::from_str::<OwnWrite>(&t).ok())
        else {
            continue;
        };
        if (now - rec.end_at(now)).num_seconds() >= prune_secs {
            let _ = fs::remove_file(e.path());
            continue;
        }
        out.push(rec);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(paths: &[&str], start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> OwnWrite {
        OwnWrite {
            pid: 1,
            command: "finish".into(),
            start,
            last: start,
            end,
            paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn f206_covers_only_listed_paths_written_inside_the_interval() {
        let start = Utc::now() - chrono::Duration::seconds(10);
        let end = start + chrono::Duration::seconds(2);
        let r = rec(&["a.txt"], start, Some(end));
        let inside = ns(start) + 500_000_001;
        let after = ns(end) + 3 * COARSE_NS + 7;
        assert!(r.covers("a.txt", Some(inside), Utc::now()));
        // Another path, or the same path written after writ finished: not
        // writ's write.
        assert!(!r.covers("b.txt", Some(inside), Utc::now()));
        assert!(!r.covers("a.txt", Some(after), Utc::now()));
        assert!(!r.covers("a.txt", Some(ns(start) - 3 * COARSE_NS - 7), Utc::now()));
    }

    #[test]
    fn f206_an_unfinished_marker_stops_counting_after_the_horizon() {
        let old = Utc::now() - chrono::Duration::seconds(OPEN_HORIZON_SECS + 60);
        let r = rec(&["a.txt"], old, None);
        // A write long after the last recorded write is not writ's.
        assert!(!r.covers("a.txt", Some(ns(Utc::now()) + 1), Utc::now()));
    }

    #[test]
    fn f206_note_writes_a_marker_and_the_guard_records_the_end() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _g = begin("restore");
            note(dir.path(), "x.txt");
            let open = load(dir.path(), 3600);
            assert_eq!(open.len(), 1);
            assert!(open[0].end.is_none());
            assert_eq!(open[0].command, "restore");
        }
        let done = load(dir.path(), 3600);
        assert_eq!(done.len(), 1);
        assert!(done[0].end.is_some());
        assert!(done[0].paths.contains("x.txt"));
    }
}
