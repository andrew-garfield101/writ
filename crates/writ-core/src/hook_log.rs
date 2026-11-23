//! Reading `.writ/hooks.log` for a hook killed before it finished (ruling
//! 213, reported by `writ doctor` in 0.4.6).
//!
//! Every hook firing from 0.4.5 on writes a `start` record before any work
//! and an `end` record as its last act, each carrying the process `pid`.
//! The rule is the stranger kit's (`hook_firings` in
//! `tools/stranger_kit/stranger.py`): walking the log in order, a firing
//! runs from its start to the next start with the same pid (pids are
//! reused); a firing with no `end` was killed, whatever it logged first. A
//! log with starts but no end record at all cannot tell (a build between
//! the two records) and reports nothing. One rule on top of the kit's,
//! because doctor runs while hooks do: a pid's last firing with no end whose
//! pid is still alive is running, not killed.
//!
//! Read only. Only `start` and `end` lines (and the records of a killed
//! firing) are parsed, so the cost is one read of the file.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::auto_seal::HOOKS_LOG;

/// One firing that started and never wrote its `end` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KilledFiring {
    /// Hook event (`PostToolUse`, `Stop`, ...).
    pub event: String,
    /// Host tool name, when the event has one.
    pub tool: Option<String>,
    /// The writ identity a later record of the firing resolved, else the
    /// host's `agent_type`, else its `agent_id`, else `session <id>`.
    pub identity: String,
    /// True when `identity` is a writ identity (a resolved `agent`, or the
    /// host's `agent_type`, which names a named subagent); false for the
    /// raw `agent_id` or session fallbacks, which `--agent` cannot take.
    pub identity_is_writ: bool,
    /// The start record's timestamp, as written.
    pub ts: String,
    pub pid: u64,
    /// The outcome of the last record the firing wrote (`start` when it
    /// wrote nothing after it).
    pub last_outcome: String,
    /// Paths named by the firing's records, in order, without repeats.
    pub paths: Vec<String>,
}

/// Killed firings in `.writ/hooks.log` that started after `after`.
/// `None` when the log cannot tell (no log, or starts with no end record).
pub fn killed_firings_in(
    writ_dir: &Path,
    after: Option<DateTime<Utc>>,
    alive: impl Fn(u32) -> bool,
) -> Option<Vec<KilledFiring>> {
    let text = std::fs::read_to_string(writ_dir.join(HOOKS_LOG)).ok()?;
    killed_firings(&text, after, alive)
}

/// [`killed_firings_in`] over the log's text. Two passes: the first parses
/// only `start` and `end` lines to find the firings with no end; the second
/// parses only the lines of those firings (rare), for their identity,
/// last outcome and paths.
pub fn killed_firings(
    text: &str,
    after: Option<DateTime<Utc>>,
    alive: impl Fn(u32) -> bool,
) -> Option<Vec<KilledFiring>> {
    let lines: Vec<&str> = text.lines().collect();
    // Each firing: (pid, start line, line of the pid's next start or EOF, ended).
    let mut firings: Vec<(u64, usize, usize, bool)> = Vec::new();
    let mut open_by_pid: std::collections::HashMap<u64, usize> = Default::default();
    let mut ends = 0usize;
    for (n, line) in lines.iter().enumerate() {
        let is_start = line.contains(r#""outcome":"start""#);
        if !is_start && !line.contains(r#""outcome":"end""#) {
            continue;
        }
        let Some(pid) = record_pid(line) else {
            continue;
        };
        if is_start {
            if let Some(prev) = open_by_pid.insert(pid, firings.len()) {
                firings[prev].2 = n;
            }
            firings.push((pid, n, lines.len(), false));
        } else {
            ends += 1;
            if let Some(i) = open_by_pid.remove(&pid) {
                firings[i].3 = true;
            }
        }
    }
    if ends == 0 {
        return None;
    }
    let out = firings
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.3)
        // CC's rule over the kit's: a pid's open last firing with a live
        // pid is a hook still running.
        .filter(|(i, f)| {
            !(open_by_pid.get(&f.0) == Some(i) && u32::try_from(f.0).is_ok_and(&alive))
        })
        .filter_map(|(_, &(pid, from, to, _))| {
            let recs: Vec<Value> = lines[from..to]
                .iter()
                .filter(|l| l.contains(r#""pid":"#))
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|r| r.get("pid").and_then(Value::as_u64) == Some(pid))
                .collect();
            describe(&recs)
        })
        .filter(|k| after.is_none_or(|a| parse_ts(&k.ts).is_none_or(|t| t > a)))
        .collect();
    Some(out)
}

/// The `pid` of one record line, parsed only when the line is a record.
fn record_pid(line: &str) -> Option<u64> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("pid")
        .and_then(Value::as_u64)
}

fn parse_ts(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn describe(recs: &[Value]) -> Option<KilledFiring> {
    let first = recs.first()?;
    let s = |r: &Value, k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let writ_identity = recs
        .iter()
        .find_map(|r| s(r, "agent"))
        .or_else(|| s(first, "agent_type"));
    let identity_is_writ = writ_identity.is_some();
    let identity = writ_identity
        .or_else(|| s(first, "agent_id"))
        .or_else(|| s(first, "session").map(|v| format!("session {v}")))
        .unwrap_or_else(|| "an unnamed session".to_string());
    let mut paths: Vec<String> = Vec::new();
    for r in recs {
        if let Some(p) = s(r, "path") {
            if !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    Some(KilledFiring {
        event: s(first, "event").unwrap_or_else(|| "unknown event".to_string()),
        tool: s(first, "tool").or_else(|| recs.iter().find_map(|r| s(r, "tool"))),
        identity,
        identity_is_writ,
        ts: s(first, "ts").unwrap_or_default(),
        pid: first.get("pid").and_then(Value::as_u64).unwrap_or_default(),
        last_outcome: recs
            .last()
            .and_then(|r| s(r, "outcome"))
            .unwrap_or_else(|| "start".to_string()),
        paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(pid: u32, ts: &str) -> String {
        format!(
            r#"{{"event":"PostToolUse","outcome":"start","agent_type":"amis","tool":"Edit","ts":"{ts}","pid":{pid}}}"#
        )
    }
    fn end(pid: u32) -> String {
        format!(
            r#"{{"event":"PostToolUse","outcome":"end","ts":"2026-11-23T10:00:09Z","pid":{pid}}}"#
        )
    }

    #[test]
    fn f213_kit_rule_start_without_end_before_the_next_start_is_killed() {
        let log = [
            start(7, "2026-11-23T10:00:00Z"),
            r#"{"event":"PostToolUse","outcome":"attributed","path":"a.rs","agent":"amis-x","pid":7}"#
                .to_string(),
            start(7, "2026-11-23T10:00:05Z"),
            end(7),
            start(8, "2026-11-23T10:00:06Z"),
            end(8),
        ]
        .join("\n");
        let k = killed_firings(&log, None, |_| false).unwrap();
        assert_eq!(k.len(), 1, "{k:?}");
        assert_eq!(k[0].identity, "amis-x");
        assert_eq!(k[0].last_outcome, "attributed");
        assert_eq!(k[0].paths, vec!["a.rs".to_string()]);
        assert_eq!(k[0].ts, "2026-11-23T10:00:00Z");
    }

    #[test]
    fn f213_open_last_firing_is_killed_when_dead_and_running_when_alive() {
        let log = [end(1), start(9, "2026-11-23T10:00:00Z")].join("\n");
        assert_eq!(killed_firings(&log, None, |_| false).unwrap().len(), 1);
        assert!(killed_firings(&log, None, |p| p == 9).unwrap().is_empty());
        // An earlier open firing of a live pid was closed by its next start:
        // killed whatever the pid is doing now.
        let log = [
            start(9, "2026-11-23T10:00:00Z"),
            end(9).replace("9}", "3}"),
            start(9, "2026-11-23T10:00:05Z"),
        ]
        .join("\n");
        assert_eq!(killed_firings(&log, None, |p| p == 9).unwrap().len(), 1);
    }

    #[test]
    fn f213_no_end_record_cannot_tell() {
        let log = [start(1, "2026-11-23T10:00:00Z")].join("\n");
        assert!(killed_firings(&log, None, |_| false).is_none());
        assert!(killed_firings("", None, |_| false).is_none());
    }

    #[test]
    fn f213_after_drops_older_firings() {
        let log = [
            start(1, "2026-11-23T09:00:00Z"),
            start(1, "2026-11-23T11:00:00Z"),
            end(1),
        ]
        .join("\n");
        let after = parse_ts("2026-11-23T10:00:00Z");
        assert!(killed_firings(&log, after, |_| false).unwrap().is_empty());
        assert_eq!(killed_firings(&log, None, |_| false).unwrap().len(), 1);
    }
}
