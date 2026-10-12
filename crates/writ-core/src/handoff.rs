//! `writ handoff` (U.3): what happened since a point, for the next person
//! or agent. Which agents worked, one line per spec, which files more than
//! one spec sealed and how each merge resolved, stale rewrite notices,
//! unsealed work and whose it is, the doctor result, and the one next
//! action (doctor's, from the same code path).
//!
//! Read only: no new state. Everything is derived from seals, specs, the
//! working tree, git HEAD and doctor's own run.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};
use crate::repo::Repository;
use crate::seal::{OverrideContext, Seal};
use crate::spec::{CommitState, Spec, SpecStatus};

/// Where the handoff window starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Since {
    /// After the newest finish (the newest `committed_at` of any spec).
    LastFinish,
    /// After this seal (full id or unique prefix).
    Seal(String),
    /// After this time (RFC 3339, or `YYYY-MM-DD` meaning midnight UTC).
    Time(DateTime<Utc>),
}

impl Since {
    /// Parse the `--since` argument: `last-finish`, a time, or a seal id.
    pub fn parse(arg: &str) -> Self {
        if arg == "last-finish" {
            return Since::LastFinish;
        }
        if let Ok(t) = DateTime::parse_from_rfc3339(arg) {
            return Since::Time(t.with_timezone(&Utc));
        }
        if let Ok(d) = chrono::NaiveDate::parse_from_str(arg, "%Y-%m-%d") {
            if let Some(t) = d.and_hms_opt(0, 0, 0) {
                return Since::Time(t.and_utc());
            }
        }
        Since::Seal(arg.to_string())
    }
}

/// The window, resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    /// What was asked: `last-finish`, a seal id, a time, or `everything`.
    pub since: String,
    /// The instant work must be after to count; `None` for everything.
    pub after: Option<DateTime<Utc>>,
}

/// One spec with work in the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffSpec {
    pub id: String,
    pub title: String,
    /// Seals of this spec in the window.
    pub seals: usize,
    /// `committed in <hash>`, `done, waiting for finish`, `in progress`.
    pub state: String,
    /// Agents that sealed it in the window.
    pub agents: Vec<String>,
    /// The committed spec this one continues (`X-followup` continues `X`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    /// `spec done` summaries given after the spec was committed (finding
    /// 124), as `agent: summary`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_commit_notes: Vec<String>,
}

/// A file more than one spec sealed in the window, and how it resolved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffMerge {
    pub path: String,
    /// Specs that sealed it, in order of their first seal of it.
    pub specs: Vec<String>,
    /// `kept`, `superseded`, `missing`, `accepted_after_told`, `mixed` or
    /// `rewrote_own`.
    pub resolution: String,
    /// One sentence; for an override, the seal's own recorded wording.
    pub detail: String,
    /// The finish commit that produced the current content, when committed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// Pending work and whose it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsealedWork {
    pub path: String,
    /// The agent whose open spec last sealed the path, else the holder of an
    /// open spec scoping it, else `unknown`.
    pub agent: String,
}

/// Doctor's result as handoff shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffDoctor {
    pub headline: String,
    pub tier: String,
    pub red: usize,
    pub yellow: usize,
    /// `check: message` per finding.
    pub findings: Vec<String>,
}

/// The whole handoff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub window: Window,
    pub agents: Vec<String>,
    pub specs: Vec<HandoffSpec>,
    pub merges: Vec<HandoffMerge>,
    /// Open stale rewrite notices, one sentence each.
    pub stale_rewrites: Vec<String>,
    pub unsealed: Vec<UnsealedWork>,
    pub doctor: HandoffDoctor,
    /// Doctor's next action, verbatim.
    pub next_action: String,
}

/// Build the handoff. `survival` runs doctor's survival tier too.
pub fn handoff(repo: &Repository, since: Option<Since>, survival: bool) -> WritResult<Handoff> {
    let specs = repo.list_specs()?;
    let window = resolve_window(repo, &specs, since)?;
    let in_window = |t: DateTime<Utc>| window.after.is_none_or(|a| t > a);

    let mut seals_by_spec: BTreeMap<String, Vec<Seal>> = BTreeMap::new();
    for spec in &specs {
        let mut seals: Vec<Seal> = repo
            .spec_seals(&spec.id)?
            .into_iter()
            .filter(|s| in_window(s.timestamp))
            .collect();
        if seals.is_empty() {
            continue;
        }
        seals.sort_by_key(|s| s.timestamp);
        seals_by_spec.insert(spec.id.clone(), seals);
    }

    let mut agents: BTreeSet<String> = BTreeSet::new();
    let mut out_specs = Vec::new();
    let empty: Vec<Seal> = Vec::new();
    for spec in &specs {
        let notes: Vec<String> = spec
            .post_commit_notes
            .iter()
            .filter(|n| in_window(n.at))
            .map(|n| format!("{}: {}", n.agent, n.summary))
            .collect();
        let seals = match seals_by_spec.get(&spec.id) {
            Some(s) => s,
            None if !notes.is_empty() => &empty,
            None => continue,
        };
        let who: BTreeSet<String> = seals.iter().map(|s| s.agent.id.clone()).collect();
        agents.extend(who.iter().cloned());
        out_specs.push(HandoffSpec {
            id: spec.id.clone(),
            title: spec.title.clone(),
            seals: seals.len(),
            state: spec_state(spec),
            agents: who.into_iter().collect(),
            continues: crate::doctor::continues(&spec.id, &specs).map(|s| s.id.clone()),
            post_commit_notes: notes,
        });
    }
    out_specs.sort_by(|a, b| a.id.cmp(&b.id));

    let merges = merges(repo, &specs, &seals_by_spec)?;
    let stale_rewrites = repo
        .stale_rewrite_notices(None)
        .into_iter()
        .filter(|n| in_window(n.created_at))
        .map(|n| n.message)
        .collect();
    let unsealed = unsealed_work(repo, &specs)?;
    agents.extend(
        unsealed
            .iter()
            .filter(|u| u.agent != UNKNOWN)
            .map(|u| u.agent.clone()),
    );

    let report = if survival {
        crate::doctor::run_survival(repo)?
    } else {
        crate::doctor::run(repo)?
    };
    let doctor = HandoffDoctor {
        headline: report.headline.clone(),
        tier: report.tier.clone(),
        red: report.red,
        yellow: report.yellow,
        findings: report
            .findings
            .iter()
            .map(|f| format!("{}: {}", f.check, f.message))
            .collect(),
    };
    Ok(Handoff {
        window,
        agents: agents.into_iter().collect(),
        specs: out_specs,
        merges,
        stale_rewrites,
        unsealed,
        doctor,
        next_action: report.next_action,
    })
}

const UNKNOWN: &str = "unknown";

fn resolve_window(repo: &Repository, specs: &[Spec], since: Option<Since>) -> WritResult<Window> {
    let last_finish = || specs.iter().filter_map(|s| s.committed_at).max();
    Ok(match since {
        None => match last_finish() {
            Some(t) => Window {
                since: "last-finish".into(),
                after: Some(t),
            },
            None => Window {
                since: "everything".into(),
                after: None,
            },
        },
        Some(Since::LastFinish) => Window {
            since: "last-finish".into(),
            after: last_finish(),
        },
        Some(Since::Time(t)) => Window {
            since: t.to_rfc3339(),
            after: Some(t),
        },
        Some(Since::Seal(id)) => {
            let full = repo.resolve_seal_id(&id).map_err(|_| {
                WritError::Other(format!(
                    "--since '{id}' is not a seal id, a time (RFC 3339 or YYYY-MM-DD) or last-finish"
                ))
            })?;
            let seal = repo.load_seal(&full)?;
            Window {
                since: full[..12.min(full.len())].to_string(),
                after: Some(seal.timestamp),
            }
        }
    })
}

/// The state line for a spec.
pub fn spec_state(spec: &Spec) -> String {
    let short = |h: &Option<String>| {
        h.as_deref()
            .map(|h| h[..h.len().min(12)].to_string())
            .unwrap_or_else(|| "git".into())
    };
    match spec.commit_state {
        CommitState::Committed => format!("committed in {}", short(&spec.commit_hash)),
        CommitState::Pushed => format!("committed in {} and pushed", short(&spec.commit_hash)),
        _ if spec.status == SpecStatus::Complete => "done, waiting for finish".into(),
        _ => match &spec.claimed_by {
            Some(h) => format!("in progress, claimed by {h}"),
            None => "open, unclaimed".into(),
        },
    }
}

/// Text of a blob, empty for none or unreadable.
fn blob(repo: &Repository, hash: &Option<String>) -> String {
    hash.as_deref()
        .and_then(|h| repo.object_content(h).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Lines `new` has that `old` lacks, and the reverse, by content (order of
/// `new` and `old` kept, duplicates once).
pub fn line_delta(old: &str, new: &str) -> (Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for hunk in crate::diff::compute_line_diff(old, new, 0) {
        for l in hunk.lines {
            match l.op {
                crate::diff::LineOp::Add => added.push(l.content),
                crate::diff::LineOp::Remove => removed.push(l.content),
                crate::diff::LineOp::Context => {}
            }
        }
    }
    (added, removed)
}

/// The content a merge produced: git HEAD for a path whose specs are all
/// committed, else the working tree.
fn current_content(repo: &Repository, path: &str, committed: bool) -> String {
    if committed {
        if let Ok(m) = repo.git_head_blobs(&[path.to_string()]) {
            if let Some(b) = m.get(path) {
                return String::from_utf8_lossy(b).into_owned();
            }
        }
    }
    std::fs::read(repo.root().join(path))
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// How the specs that sealed `path` resolved, from their seals (in time
/// order across specs) and the current content. Shared by handoff and why.
pub fn resolve_path(
    repo: &Repository,
    path: &str,
    seals: &[&Seal],
    specs: &[Spec],
) -> (String, String, Option<String>) {
    let spec_of = |id: &str| specs.iter().find(|s| s.id == id);
    let ids: Vec<String> = {
        let mut v: Vec<String> = Vec::new();
        for s in seals {
            if let Some(id) = &s.spec_id {
                if !v.contains(id) {
                    v.push(id.clone());
                }
            }
        }
        v
    };
    let committed: Vec<&Spec> = ids.iter().filter_map(|id| spec_of(id)).collect();
    let all_committed = !committed.is_empty()
        && committed.len() == ids.len()
        && committed
            .iter()
            .all(|s| matches!(s.commit_state, CommitState::Committed | CommitState::Pushed));
    let commit = committed
        .iter()
        .filter(|s| s.commit_hash.is_some())
        .max_by_key(|s| s.committed_at)
        .and_then(|s| s.commit_hash.clone());

    // An override on the path is the resolution, in its own words.
    let overrides: Vec<(&Seal, &OverrideContext)> = seals
        .iter()
        .flat_map(|s| {
            s.override_context
                .iter()
                .filter(|o| o.path == path)
                .map(move |o| (*s, o))
        })
        .collect();
    if let Some((s, o)) = overrides
        .iter()
        .find(|(_, o)| o.resolution() != "rewrote_own")
        .or_else(|| overrides.first())
    {
        return (
            o.resolution().to_string(),
            o.describe(&s.agent.id, &s.id),
            commit,
        );
    }

    // Each spec's own added lines (added by its seals, still in its last
    // version of the path): are they in the current content?
    let current = current_content(repo, path, all_committed);
    let present: HashSet<&str> = current.lines().collect();
    // A missing line is superseded when a later seal of the path (any
    // spec in the set) dropped it; otherwise no seal removed it: missing.
    let drops: Vec<(DateTime<Utc>, HashSet<String>)> = seals
        .iter()
        .flat_map(|s| {
            s.changes
                .iter()
                .filter(|c| c.path == path)
                .map(move |c| (s.timestamp, c))
        })
        .map(|(t, c)| {
            let new = blob(repo, &c.new_hash);
            let now: HashSet<&str> = new.lines().collect();
            let dropped = blob(repo, &c.old_hash)
                .lines()
                .filter(|l| !now.contains(l))
                .map(str::to_string)
                .collect();
            (t, dropped)
        })
        .collect();
    let removed_later = |line: &str, after: DateTime<Utc>| {
        drops.iter().any(|(t, d)| *t > after && d.contains(line))
    };
    let mut superseded: BTreeMap<String, usize> = BTreeMap::new();
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    for id in &ids {
        let own: Vec<&&Seal> = seals
            .iter()
            .filter(|s| s.spec_id.as_deref() == Some(id))
            .collect();
        let Some((last_seal, last)) = own
            .iter()
            .rev()
            .find_map(|s| s.changes.iter().find(|c| c.path == path).map(|c| (**s, c)))
        else {
            continue;
        };
        let last_text = blob(repo, &last.new_hash);
        let last_lines: HashSet<&str> = last_text.lines().collect();
        let mut added: BTreeSet<String> = BTreeSet::new();
        for s in &own {
            for c in s.changes.iter().filter(|c| c.path == path) {
                let (a, _) = line_delta(&blob(repo, &c.old_hash), &blob(repo, &c.new_hash));
                added.extend(a.into_iter().filter(|l| !l.trim().is_empty()));
            }
        }
        for l in added
            .iter()
            .filter(|l| last_lines.contains(l.as_str()) && !present.contains(l.as_str()))
        {
            let bucket = if removed_later(l, last_seal.timestamp) {
                &mut superseded
            } else {
                &mut missing
            };
            *bucket.entry(id.clone()).or_insert(0) += 1;
        }
    }
    let where_ = if all_committed {
        "the commit"
    } else {
        "the working tree"
    };
    let list = |m: &BTreeMap<String, usize>| {
        m.iter()
            .map(|(id, n)| format!("{n} line(s) of {id}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !missing.is_empty() {
        return (
            "missing".into(),
            format!(
                "missing: {} not in {where_} and no seal removed them; run `writ doctor --survival`",
                list(&missing)
            ),
            commit,
        );
    }
    if superseded.is_empty() {
        return (
            "kept".into(),
            format!(
                "kept: every line {} sealed is in {where_}",
                crate::seal::join_names(&ids).unwrap_or_default()
            ),
            commit,
        );
    }
    (
        "superseded".into(),
        format!(
            "superseded: {} replaced by a later seal of the file; the rest kept",
            list(&superseded)
        ),
        commit,
    )
}

fn merges(
    repo: &Repository,
    specs: &[Spec],
    seals_by_spec: &BTreeMap<String, Vec<Seal>>,
) -> WritResult<Vec<HandoffMerge>> {
    let mut by_path: BTreeMap<String, Vec<&Seal>> = BTreeMap::new();
    for seals in seals_by_spec.values() {
        for s in seals {
            for c in &s.changes {
                by_path.entry(c.path.clone()).or_default().push(s);
            }
        }
    }
    let mut out = Vec::new();
    for (path, mut seals) in by_path {
        seals.sort_by_key(|s| s.timestamp);
        seals.dedup_by(|a, b| a.id == b.id);
        let mut order: Vec<String> = Vec::new();
        for s in &seals {
            if let Some(id) = &s.spec_id {
                if !order.contains(id) {
                    order.push(id.clone());
                }
            }
        }
        if order.len() < 2 {
            continue;
        }
        let (resolution, detail, commit) = resolve_path(repo, &path, &seals, specs);
        out.push(HandoffMerge {
            path,
            specs: order,
            resolution,
            detail,
            commit,
        });
    }
    Ok(out)
}

/// Pending files and whose they are.
fn unsealed_work(repo: &Repository, specs: &[Spec]) -> WritResult<Vec<UnsealedWork>> {
    let state = repo.state()?;
    if state.changes.is_empty() {
        return Ok(Vec::new());
    }
    let open: Vec<&Spec> = specs
        .iter()
        .filter(|s| !matches!(s.commit_state, CommitState::Committed | CommitState::Pushed))
        .collect();
    // Newest sealer of each path, over every spec (committed too: the
    // agent who last sealed it is the one whose work it most likely is).
    let mut newest: BTreeMap<String, (DateTime<Utc>, String)> = BTreeMap::new();
    for spec in specs {
        for s in repo.spec_seals(&spec.id)? {
            for c in &s.changes {
                let e = newest
                    .entry(c.path.clone())
                    .or_insert((s.timestamp, s.agent.id.clone()));
                if s.timestamp >= e.0 {
                    *e = (s.timestamp, s.agent.id.clone());
                }
            }
        }
    }
    let mut out = Vec::new();
    for f in &state.changes {
        let agent = newest
            .get(&f.path)
            .map(|(_, a)| a.clone())
            .or_else(|| {
                open.iter()
                    .find(|s| {
                        s.file_scope.iter().any(|e| {
                            e == &f.path || (e.ends_with('/') && f.path.starts_with(e.as_str()))
                        })
                    })
                    .and_then(|s| s.claimed_by.clone())
            })
            .unwrap_or_else(|| UNKNOWN.to_string());
        out.push(UnsealedWork {
            path: f.path.clone(),
            agent,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Markdown (the default format).
pub fn to_markdown(h: &Handoff) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Handoff since {}\n\n", h.window.since));
    s.push_str(&format!(
        "**Agents:** {}\n\n",
        if h.agents.is_empty() {
            "none".to_string()
        } else {
            h.agents.join(", ")
        }
    ));
    s.push_str(&format!("## Specs ({})\n\n", h.specs.len()));
    if h.specs.is_empty() {
        s.push_str("No spec sealed work in this window.\n");
    }
    for sp in &h.specs {
        let cont = sp
            .continues
            .as_deref()
            .map(|c| format!(" (spec {c} continued as spec {} after finish)", sp.id))
            .unwrap_or_default();
        s.push_str(&format!(
            "- `{}` {}: {} seal(s) by {}; {}{cont}\n",
            sp.id,
            sp.title,
            sp.seals,
            sp.agents.join(", "),
            sp.state
        ));
        for n in &sp.post_commit_notes {
            s.push_str(&format!("  - after commit, {n}\n"));
        }
    }
    s.push_str(&format!("\n## Shared files ({})\n\n", h.merges.len()));
    if h.merges.is_empty() {
        s.push_str("No file was sealed by more than one spec.\n");
    }
    for m in &h.merges {
        let commit = m
            .commit
            .as_deref()
            .map(|c| format!(" (commit {})", &c[..c.len().min(12)]))
            .unwrap_or_default();
        s.push_str(&format!(
            "- `{}` sealed by {}: **{}**{commit}. {}\n",
            m.path,
            m.specs.join(", "),
            m.resolution,
            m.detail
        ));
    }
    if !h.stale_rewrites.is_empty() {
        s.push_str("\n## Stale rewrite notices\n\n");
        for n in &h.stale_rewrites {
            s.push_str(&format!("- {n}\n"));
        }
    }
    s.push_str("\n## Unsealed work\n\n");
    if h.unsealed.is_empty() {
        s.push_str("None.\n");
    }
    for u in &h.unsealed {
        s.push_str(&format!("- `{}` ({})\n", u.path, u.agent));
    }
    s.push_str(&format!("\n## Doctor\n\n{}\n", h.doctor.headline));
    for f in &h.doctor.findings {
        s.push_str(&format!("- {f}\n"));
    }
    s.push_str(&format!("\n## Next action\n\n{}\n", h.next_action));
    s
}

/// One short block for agents (`--format brief`), under 2 KB.
pub fn to_brief(h: &Handoff) -> String {
    let mut lines = vec![format!(
        "handoff since {}: {} spec(s), {} shared file(s), {} unsealed; agents: {}",
        h.window.since,
        h.specs.len(),
        h.merges.len(),
        h.unsealed.len(),
        h.agents.join(", ")
    )];
    for sp in h.specs.iter().take(20) {
        lines.push(format!("spec {} ({} seals): {}", sp.id, sp.seals, sp.state));
    }
    if h.specs.len() > 20 {
        lines.push(format!("+{} more spec(s)", h.specs.len() - 20));
    }
    for m in h.merges.iter().take(10) {
        lines.push(format!(
            "{}: {} ({})",
            m.path,
            m.resolution,
            m.specs.join(", ")
        ));
    }
    for u in h.unsealed.iter().take(5) {
        lines.push(format!("unsealed {} ({})", u.path, u.agent));
    }
    lines.push(format!("doctor: {}", h.doctor.headline));
    lines.push(format!("next: {}", h.next_action));
    let mut out = String::new();
    for l in lines {
        let l: String = l.chars().take(300).collect();
        if out.len() + l.len() + 1 > 2000 {
            out.push_str("(trimmed; run `writ handoff` for the rest)\n");
            break;
        }
        out.push_str(&l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_parses_every_form() {
        assert_eq!(Since::parse("last-finish"), Since::LastFinish);
        assert!(matches!(Since::parse("2026-10-12"), Since::Time(_)));
        assert!(matches!(
            Since::parse("2026-10-12T08:00:00Z"),
            Since::Time(_)
        ));
        assert_eq!(
            Since::parse("ea59f117fd8c"),
            Since::Seal("ea59f117fd8c".into())
        );
    }

    #[test]
    fn line_delta_names_added_and_removed() {
        let (a, r) = line_delta("x\ny\nz\n", "x\nY\nz\nw\n");
        assert_eq!(a, vec!["Y".to_string(), "w".to_string()]);
        assert_eq!(r, vec!["y".to_string()]);
    }

    #[test]
    fn brief_stays_under_two_kilobytes() {
        let spec = HandoffSpec {
            id: "s".repeat(40),
            title: "t".into(),
            seals: 3,
            state: "committed in abc".into(),
            agents: vec!["a".into()],
            continues: None,
            post_commit_notes: Vec::new(),
        };
        let h = Handoff {
            window: Window {
                since: "last-finish".into(),
                after: None,
            },
            agents: vec!["a".into(); 50],
            specs: vec![spec; 200],
            merges: Vec::new(),
            stale_rewrites: Vec::new(),
            unsealed: Vec::new(),
            doctor: HandoffDoctor {
                headline: "fast checks clean".into(),
                tier: "fast".into(),
                red: 0,
                yellow: 0,
                findings: Vec::new(),
            },
            next_action: "nothing to do".into(),
        };
        assert!(to_brief(&h).len() <= 2048);
    }
}
