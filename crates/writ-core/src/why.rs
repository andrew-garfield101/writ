//! `writ why <file>` (U.3): the specs that touched a file, in order; what
//! each seal added and removed; any override on it in the seal's own
//! recorded words; and the merge that produced the current content.
//!
//! Read only, derived from seals, specs and git HEAD.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::WritResult;
use crate::handoff::{line_delta, resolve_path};
use crate::repo::Repository;
use crate::seal::Seal;

/// An override on this seal, as recorded (finding 102, wording per 122).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyOverride {
    /// `accepted_after_told`, `mixed` or `rewrote_own`.
    pub resolution: String,
    /// The recorded sentence (finish prints the same).
    pub detail: String,
    /// The own lines removed on purpose.
    pub lines: Vec<String>,
    pub other_agents: Vec<String>,
    /// The removed lines that were another agent's call (the rest the
    /// sealer rewrote itself).
    pub accepted_lines: Vec<String>,
}

/// One seal of the file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyEntry {
    pub seal: String,
    /// `None` for a seal on no spec (setup, imports).
    pub spec: Option<String>,
    pub agent: String,
    pub timestamp: DateTime<Utc>,
    /// `added`, `modified` or `deleted`.
    pub change: String,
    /// Lines this seal brought in, against the version on record before it.
    pub added: Vec<String>,
    /// Lines this seal dropped: against the version on record, and the
    /// spec's own lines from its previous seal of the file.
    pub removed: Vec<String>,
    /// The override this seal used on the file, or null.
    pub r#override: Option<WhyOverride>,
    /// The committed spec this one continues (`X-followup` continues `X`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continues: Option<String>,
    /// Ruling 189: `command_text` when the auto seal hook attributed this
    /// seal's paths from the command text; absent for a single window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_by: Option<String>,
}

/// The merge that produced the current content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhyMerge {
    /// The finish commit, when the content is committed.
    pub commit: Option<String>,
    pub specs: Vec<String>,
    /// `kept`, `superseded`, `accepted_after_told` or `rewrote_own`.
    pub resolution: String,
    pub detail: String,
}

/// The answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Why {
    pub path: String,
    pub history: Vec<WhyEntry>,
    pub merge: Option<WhyMerge>,
}

fn text(repo: &Repository, hash: &Option<String>) -> String {
    hash.as_deref()
        .and_then(|h| repo.object_content(h).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

/// Every seal that touched `path`, oldest first, from every spec chain and
/// the global log (deduplicated).
fn seals_of(repo: &Repository, path: &str) -> WritResult<Vec<Seal>> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let mut push = |s: Seal, out: &mut Vec<Seal>| {
        if s.changes.iter().any(|c| c.path == path) && seen.insert(s.id.clone()) {
            out.push(s);
        }
    };
    for spec in repo.list_specs()? {
        for s in repo.spec_seals(&spec.id)? {
            push(s, &mut out);
        }
    }
    for s in repo.log_all()? {
        push(s, &mut out);
    }
    out.sort_by_key(|s| s.timestamp);
    Ok(out)
}

/// Build the answer for `path` (repository-relative).
pub fn why(repo: &Repository, path: &str) -> WritResult<Why> {
    let path = path.trim_start_matches("./").to_string();
    let specs = repo.list_specs()?;
    let seals = seals_of(repo, &path)?;
    let mut history = Vec::with_capacity(seals.len());
    // Each spec's previous version of the file, for its own removed lines.
    let mut previous: std::collections::HashMap<String, String> = Default::default();
    for s in &seals {
        let Some(c) = s.changes.iter().find(|c| c.path == path) else {
            continue;
        };
        let old = text(repo, &c.old_hash);
        let new = text(repo, &c.new_hash);
        let (added, mut removed) = line_delta(&old, &new);
        if let Some(prev) = s.spec_id.as_ref().and_then(|id| previous.get(id)) {
            let now: HashSet<&str> = new.lines().collect();
            let own: Vec<String> = prev
                .lines()
                .filter(|l| !now.contains(l) && !removed.iter().any(|r| r == l))
                .map(str::to_string)
                .collect();
            removed.extend(own);
        }
        if let Some(id) = &s.spec_id {
            previous.insert(id.clone(), new.clone());
        }
        let r#override = s
            .override_context
            .iter()
            .find(|o| o.path == path)
            .map(|o| WhyOverride {
                resolution: o.resolution().to_string(),
                detail: o.describe(&s.agent.id, &s.id),
                lines: o.lines.clone(),
                other_agents: o.other_agents.clone(),
                accepted_lines: o.accepted_lines.clone(),
            });
        history.push(WhyEntry {
            seal: s.id.clone(),
            spec: s.spec_id.clone(),
            agent: s.agent.id.clone(),
            timestamp: s.timestamp,
            change: format!("{:?}", c.change_type).to_lowercase(),
            added,
            removed,
            r#override,
            continues: s
                .spec_id
                .as_deref()
                .and_then(|id| crate::doctor::continues(id, &specs))
                .map(|x| x.id.clone()),
            attributed_by: s.attributed_by.clone(),
        });
    }
    let spec_seals: Vec<&Seal> = seals.iter().filter(|s| s.spec_id.is_some()).collect();
    let merge = if spec_seals.is_empty() {
        None
    } else {
        let mut order: Vec<String> = Vec::new();
        for s in &spec_seals {
            let id = s.spec_id.clone().unwrap_or_default();
            if !order.contains(&id) {
                order.push(id);
            }
        }
        let (resolution, detail, commit) = resolve_path(repo, &path, &spec_seals, &specs);
        Some(WhyMerge {
            commit,
            specs: order,
            resolution,
            detail,
        })
    };
    Ok(Why {
        path,
        history,
        merge,
    })
}

/// Markdown (the default format).
pub fn to_markdown(w: &Why) -> String {
    let mut s = format!("# Why {}\n\n", w.path);
    if w.history.is_empty() {
        s.push_str("No seal touched this file.\n");
        return s;
    }
    s.push_str("## History\n\n");
    for e in &w.history {
        let spec = e.spec.as_deref().unwrap_or("no spec");
        let cont = e
            .continues
            .as_deref()
            .map(|c| format!(" (spec {c} continued as spec {spec} after finish)"))
            .unwrap_or_default();
        let how = match e.attributed_by.as_deref() {
            Some(crate::seal::ATTRIBUTED_BY_COMMAND_TEXT) => {
                format!(", {}", crate::seal::ATTRIBUTED_FROM_COMMAND)
            }
            _ => String::new(),
        };
        s.push_str(&format!(
            "- {} seal {} by {} on `{spec}`{cont}: {}{how}\n",
            e.timestamp.format("%Y-%m-%d %H:%M"),
            &e.seal[..e.seal.len().min(12)],
            e.agent,
            e.change
        ));
        for l in e.added.iter().take(10) {
            s.push_str(&format!("  - added `{l}`\n"));
        }
        if e.added.len() > 10 {
            s.push_str(&format!("  - and {} more added\n", e.added.len() - 10));
        }
        for l in e.removed.iter().take(10) {
            s.push_str(&format!("  - removed `{l}`\n"));
        }
        if e.removed.len() > 10 {
            s.push_str(&format!("  - and {} more removed\n", e.removed.len() - 10));
        }
        if let Some(o) = &e.r#override {
            s.push_str(&format!(
                "  - override ({}): {}; lines: {}\n",
                o.resolution,
                o.detail,
                o.lines.join(" | ")
            ));
        }
    }
    if let Some(m) = &w.merge {
        let commit = m
            .commit
            .as_deref()
            .map(|c| format!("commit {}", &c[..c.len().min(12)]))
            .unwrap_or_else(|| "not committed yet (working tree)".into());
        s.push_str(&format!(
            "\n## Current content\n\n{commit}, from {}: **{}**. {}\n",
            m.specs.join(", "),
            m.resolution,
            m.detail
        ));
    }
    s
}
