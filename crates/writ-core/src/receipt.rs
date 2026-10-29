//! The receipt (0.4.3): what a finish committed, from records only.
//!
//! One code path for three outputs: the body `writ finish` writes into each
//! commit ([`render_commit`], under 40 lines, spec list first, survival
//! line last), the markdown `writ finish --receipt <path>` writes and
//! `writ handoff --format pr` prints ([`render_pr`]), and the same body
//! rebuilt after the fact for a commit (`writ handoff --commit <rev>`).
//!
//! Every fact comes from a record: spec records (title, closing summary),
//! seals (agents, counts, overrides, the agent who gave the closing
//! summary), `finish_refused` events, the content the commit holds, and
//! the survival result the finish computed (kept in
//! `.writ/receipts/<commit>.json` with the range, so the rebuild uses the
//! same range and the same survival numbers). An absence is stated as an
//! absence: a spec with no closing summary says so, never a sentence
//! writ made up.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::convergence::survival::AuditReport;
use crate::error::{WritError, WritResult};
use crate::repo::Repository;
use crate::seal::Seal;
use crate::spec::Spec;

/// Lines the commit form may use (the whole message stays under 40).
pub const COMMIT_BODY_MAX_LINES: usize = 34;
/// Items shown per secondary section (shared files, overrides, refusals,
/// open specs) in the commit form before "+N more".
const SECTION_CAP: usize = 4;
/// A closing summary longer than this is cut in the commit form, marked.
const WORDS_MAX_CHARS: usize = 160;
/// Prefix of an auto seal's summary (`auto: <tool> <path>`).
pub const AUTO_SEAL_PREFIX: &str = "auto:";

/// The survival result the finish computed for the specs it committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurvivalLine {
    /// Distinctive sealed lines checked.
    pub checked: usize,
    /// Specs with seals the check covered.
    pub specs: usize,
    /// Lost lines plus sealed deletions that did not hold.
    pub lost: usize,
    /// The finish went ahead with `--force` over `lost`.
    #[serde(default)]
    pub forced: bool,
    /// False when no finish recorded it (a commit made before 0.4.3).
    #[serde(default = "yes")]
    pub recorded: bool,
    /// Finding 199: the committed specs' seals and how many of them carry
    /// a signature. Counted from the seals when the receipt is built, so a
    /// rebuilt receipt counts them too; None only on a bare line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seals: Option<SignedCount>,
}

/// Seals and signed seals behind a receipt (finding 199).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedCount {
    pub seals: usize,
    pub signed: usize,
}

fn yes() -> bool {
    true
}

impl SurvivalLine {
    /// From the finish tier's report over `specs` specs.
    pub fn from_report(report: &AuditReport, specs: usize, forced: bool) -> Self {
        SurvivalLine {
            checked: report.checked,
            specs,
            lost: report.lost + report.deleted_but_present().count(),
            forced,
            recorded: true,
            seals: None,
        }
    }

    /// No spec in the finish had a seal: nothing to check.
    pub fn nothing_sealed() -> Self {
        SurvivalLine {
            checked: 0,
            specs: 0,
            lost: 0,
            forced: false,
            recorded: true,
            seals: None,
        }
    }

    fn not_recorded() -> Self {
        SurvivalLine {
            checked: 0,
            specs: 0,
            lost: 0,
            forced: false,
            recorded: false,
            seals: None,
        }
    }

    /// The one line, as both forms print it: the survival result, then
    /// the seals and how many are signed (finding 199).
    pub fn text(&self) -> String {
        let base = self.survival_text();
        match self.seals {
            Some(c) if c.seals > 0 => format!("{base}; {} seals, {} signed", c.seals, c.signed),
            _ => base,
        }
    }

    fn survival_text(&self) -> String {
        if !self.recorded {
            return "Survival: not recorded for this commit (finished before writ 0.4.3)".into();
        }
        if self.specs == 0 {
            return "Survival: no sealed lines to check".into();
        }
        if self.lost == 0 {
            return format!(
                "Survival: {} sealed line(s) of {} spec(s) checked, none lost",
                self.checked, self.specs
            );
        }
        format!(
            "Survival: {} sealed line(s) of {} spec(s) checked, {} lost{}",
            self.checked,
            self.specs,
            self.lost,
            if self.forced {
                "; finished with --force"
            } else {
                ""
            }
        )
    }
}

/// What a finish keeps so the receipt can be rebuilt for its commit:
/// `.writ/receipts/<commit>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptRecord {
    pub commit: String,
    pub specs: Vec<String>,
    /// The range: after the previous finish (None: everything) ...
    pub after: Option<DateTime<Utc>>,
    /// ... until this finish built the receipt.
    pub until: DateTime<Utc>,
    pub survival: SurvivalLine,
}

/// The agent's own closing words for a spec (`writ spec done -s`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingWords {
    /// The agent whose seal carried the summary; None when no seal did.
    pub agent: Option<String>,
    pub text: String,
}

/// One committed spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptSpec {
    pub id: String,
    pub title: String,
    pub agents: Vec<String>,
    pub seals: usize,
    /// Seals whose summary starts with `auto:` (the PostToolUse hook).
    pub auto_seals: usize,
    pub first_seal: Option<DateTime<Utc>>,
    pub last_seal: Option<DateTime<Utc>>,
    /// None: the agent gave no summary (no `spec done -s`; the title
    /// stood in, or the spec was closed for it).
    pub words: Option<ClosingWords>,
    /// Not done yet (only in a pending receipt): no finish would commit it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
}

/// A file two or more committed specs sealed, and how it resolved in the
/// commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptShared {
    pub path: String,
    pub specs: Vec<String>,
    /// `<spec> by <agents>` per spec, in the order of `specs`.
    pub sealers: Vec<String>,
    /// kept, superseded, missing, accepted_after_told, rewrote_own, mixed.
    pub resolution: String,
    pub detail: String,
}

/// A committed file that specs outside this commit also sealed in range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptAlsoSealed {
    pub path: String,
    pub specs: Vec<String>,
}

/// A `seal_refused` event of a committed spec, or a `finish_refused`
/// event, in range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRefusal {
    /// `seal` (a committed spec's seal was refused) or `finish`.
    pub kind: String,
    pub at: DateTime<Utc>,
    pub agent: Option<String>,
    pub reason: String,
    pub specs: Vec<String>,
    pub files: Vec<String>,
}

/// The receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// The commit, once made; None for a pending finish.
    pub commit: Option<String>,
    pub after: Option<DateTime<Utc>>,
    pub until: DateTime<Utc>,
    pub specs: Vec<ReceiptSpec>,
    pub shared: Vec<ReceiptShared>,
    pub also_sealed: Vec<ReceiptAlsoSealed>,
    /// Ruling 177: lines a committed spec's seal moved from one path to
    /// another in the same seal.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moves: Vec<String>,
    /// Overrides accepted, in the seal's recorded wording (who, which seal).
    pub overrides: Vec<String>,
    pub refusals: Vec<ReceiptRefusal>,
    pub survival: SurvivalLine,
    /// Ruling 183 (Amis): seals in range made with the foreign-line check
    /// off by the fixture switch. 0 prints nothing.
    #[serde(default)]
    pub check_off_seals: usize,
}

/// Ruling 183: the one line when any seal in range was made with the
/// foreign-line check off; None otherwise.
pub fn check_off_line(r: &Receipt) -> Option<String> {
    (r.check_off_seals > 0).then(|| {
        format!(
            "{} seal(s) made with the foreign-line check disabled in config",
            r.check_off_seals
        )
    })
}

/// Where the content a receipt resolves shared files against comes from.
pub enum Content<'a> {
    /// The git index (a finish, after staging, before committing).
    GitIndex,
    /// A commit's tree.
    Commit(&'a str),
    /// The finish plan's sealed blobs (a pending finish).
    Plan(BTreeMap<String, Option<String>>),
}

/// The newest finish before now: the newest `committed_at` of any spec.
pub fn last_finish(specs: &[Spec]) -> Option<DateTime<Utc>> {
    specs.iter().filter_map(|s| s.committed_at).max()
}

/// Build the receipt for `spec_ids` over `(after, until]`.
pub fn build(
    repo: &Repository,
    spec_ids: &[String],
    after: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
    content: &Content,
    mut survival: SurvivalLine,
    commit: Option<String>,
) -> WritResult<Receipt> {
    let all_specs = repo.list_specs()?;
    let wanted: BTreeSet<&str> = spec_ids.iter().map(String::as_str).collect();
    let in_range = |t: DateTime<Utc>| t <= until && after.is_none_or(|a| t > a);

    // Seals of the committed specs up to `until`, oldest first.
    let mut own: BTreeMap<String, Vec<Seal>> = BTreeMap::new();
    for id in &wanted {
        let mut seals: Vec<Seal> = repo
            .spec_seals(id)?
            .into_iter()
            .filter(|s| s.timestamp <= until)
            .collect();
        seals.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
        own.insert(id.to_string(), seals);
    }

    let all_own = own.values().flatten();
    survival.seals = Some(SignedCount {
        seals: all_own.clone().count(),
        signed: all_own.filter(|s| s.signature.is_some()).count(),
    });

    let mut specs = Vec::new();
    for id in &wanted {
        let Some(spec) = all_specs.iter().find(|s| s.id == *id) else {
            continue;
        };
        let seals = &own[*id];
        specs.push(receipt_spec(spec, seals));
    }

    // Paths, with the committed specs' seals of each in time order.
    let mut by_path: BTreeMap<&str, Vec<&Seal>> = BTreeMap::new();
    for seals in own.values() {
        for s in seals {
            for c in &s.changes {
                let v = by_path.entry(c.path.as_str()).or_default();
                if !v.iter().any(|x| x.id == s.id) {
                    v.push(s);
                }
            }
        }
    }
    let mut shared = Vec::new();
    for (path, seals) in by_path.iter_mut() {
        seals.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
        let mut order: Vec<String> = Vec::new();
        for s in seals.iter() {
            if let Some(id) = &s.spec_id {
                if !order.contains(id) {
                    order.push(id.clone());
                }
            }
        }
        if order.len() < 2 {
            continue;
        }
        let sealers = order
            .iter()
            .map(|id| {
                let who: BTreeSet<&str> = seals
                    .iter()
                    .filter(|s| s.spec_id.as_deref() == Some(id))
                    .map(|s| s.agent.id.as_str())
                    .collect();
                let how = if crate::handoff::spec_attributed_from_command(seals, id, path) {
                    format!(", {}", crate::seal::ATTRIBUTED_FROM_COMMAND)
                } else {
                    String::new()
                };
                format!(
                    "{id} by {}{how}",
                    who.into_iter().collect::<Vec<_>>().join(", ")
                )
            })
            .collect();
        let (resolution, detail) = match crate::handoff::override_resolution(repo, path, seals) {
            Some(r) => r,
            None => {
                let text = content_of(repo, content, path)?;
                crate::handoff::resolve_against(repo, path, seals, &text, "the commit")
            }
        };
        shared.push(ReceiptShared {
            path: path.to_string(),
            specs: order,
            sealers,
            resolution,
            detail,
        });
    }

    // Specs outside the commit that sealed a committed path in range.
    let mut also: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for spec in all_specs.iter().filter(|s| !wanted.contains(s.id.as_str())) {
        if spec.sealed_by.is_empty() {
            continue;
        }
        for s in repo.spec_seals(&spec.id)? {
            if !in_range(s.timestamp) {
                continue;
            }
            for c in &s.changes {
                if by_path.contains_key(c.path.as_str()) {
                    also.entry(c.path.clone())
                        .or_default()
                        .insert(spec.id.clone());
                }
            }
        }
    }
    let also_sealed = also
        .into_iter()
        .map(|(path, specs)| ReceiptAlsoSealed {
            path,
            specs: specs.into_iter().collect(),
        })
        .collect();

    // Ruling 177: moves, then overrides that are not one of them.
    let history = repo.log_all()?;
    let mut moves = Vec::new();
    let mut moved_at: BTreeSet<(String, String)> = BTreeSet::new();
    for seals in own.values() {
        for s in seals {
            for m in crate::convergence::survival::seal_moves(repo, s, &history) {
                moved_at.insert((s.id.clone(), m.from.clone()));
                moves.push((s.timestamp, m.describe()));
            }
        }
    }
    moves.sort();
    let moves = moves.into_iter().map(|(_, d)| d).collect();
    let mut overrides = Vec::new();
    for seals in own.values() {
        for s in seals {
            for o in &s.override_context {
                if moved_at.contains(&(s.id.clone(), o.path.clone())) {
                    continue;
                }
                overrides.push((
                    s.timestamp,
                    crate::convergence::survival::describe_override(repo, s, o).1,
                ));
            }
        }
    }
    overrides.sort();
    let overrides = overrides.into_iter().map(|(_, d)| d).collect();

    let committed_paths: BTreeSet<&str> = by_path.keys().copied().collect();
    let refusals = refusals_in(repo.writ_dir(), &wanted, &committed_paths, after, until)?;
    let check_off_seals = own
        .values()
        .flatten()
        .filter(|s| in_range(s.timestamp) && s.foreign_line_check_off)
        .count();

    Ok(Receipt {
        commit,
        after,
        until,
        specs,
        shared,
        also_sealed,
        moves,
        overrides,
        refusals,
        survival,
        check_off_seals,
    })
}

fn receipt_spec(spec: &Spec, seals: &[Seal]) -> ReceiptSpec {
    let agents: BTreeSet<String> = seals.iter().map(|s| s.agent.id.clone()).collect();
    let auto_seals = seals
        .iter()
        .filter(|s| s.summary.starts_with(AUTO_SEAL_PREFIX))
        .count();
    // The agent's own words: a closing summary that is not the title
    // standing in (finding 68). The speaker is the agent whose seal
    // carried that summary (`spec done`'s final seal), when one did.
    let words = spec
        .completion_summary
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != spec.title.trim())
        .map(|t| ClosingWords {
            agent: seals
                .iter()
                .rev()
                .find(|s| s.summary.trim() == t)
                .map(|s| s.agent.id.clone())
                // `spec done` with nothing left to seal: the claim holder
                // closed it (a non-holder close is its own event, 52).
                .or_else(|| spec.claimed_by.clone()),
            text: t.to_string(),
        });
    ReceiptSpec {
        id: spec.id.clone(),
        title: spec.title.clone(),
        agents: agents.into_iter().collect(),
        seals: seals.len(),
        auto_seals,
        first_seal: seals.first().map(|s| s.timestamp),
        last_seal: seals.last().map(|s| s.timestamp),
        words,
        open: !spec.is_committable()
            && !matches!(
                spec.commit_state,
                crate::spec::CommitState::Committed | crate::spec::CommitState::Pushed
            ),
    }
}

/// The refusal records in `(after, until]`, oldest first: every
/// `finish_refused`, and each `seal_refused` naming a spec in `wanted` or
/// a path the committed specs sealed (finding 203: an identity whose only
/// seal was refused has no committed spec, but the file it was refused on
/// is in the commit).
fn refusals_in(
    writ_dir: &Path,
    wanted: &BTreeSet<&str>,
    committed_paths: &BTreeSet<&str>,
    after: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
) -> WritResult<Vec<ReceiptRefusal>> {
    /// The shape both refusal records share (finding 76).
    #[derive(Deserialize)]
    struct Refused {
        timestamp: DateTime<Utc>,
        event_type: String,
        #[serde(default)]
        agent_id: Option<String>,
        reason: serde_json::Value,
        #[serde(default)]
        specs: Vec<String>,
        #[serde(default)]
        files: Vec<String>,
    }
    let path = writ_dir.join("security").join("events.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if !line.contains("_refused") {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Refused>(line) else {
            continue;
        };
        let kind = match e.event_type.as_str() {
            crate::doctor::EVENT_FINISH_REFUSED => "finish",
            crate::repo::EVENT_SEAL_REFUSED
                if e.specs.iter().any(|s| wanted.contains(s.as_str()))
                    || e.files.iter().any(|f| committed_paths.contains(f.as_str())) =>
            {
                "seal"
            }
            _ => continue,
        };
        if e.timestamp > until || after.is_some_and(|a| e.timestamp <= a) {
            continue;
        }
        out.push(ReceiptRefusal {
            kind: kind.to_string(),
            at: e.timestamp,
            agent: e.agent_id,
            reason: e.reason.as_str().unwrap_or_default().to_string(),
            specs: e.specs,
            files: e.files,
        });
    }
    out.sort_by_key(|r| r.at);
    Ok(out)
}

fn content_of(repo: &Repository, content: &Content, path: &str) -> WritResult<String> {
    let bytes = match content {
        Content::GitIndex => git::index_blob(repo.root(), path)?,
        Content::Commit(c) => git::blob_at(repo.root(), c, path)?,
        Content::Plan(stage) => match stage.get(path) {
            Some(Some(h)) => Some(repo.object_content(h)?),
            Some(None) => None,
            // An open spec's path: its sealed lines are in the working tree.
            None => std::fs::read(repo.root().join(path)).ok(),
        },
    };
    Ok(bytes
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Record and the three entry points
// ---------------------------------------------------------------------------

fn record_path(writ_dir: &Path, commit: &str) -> PathBuf {
    writ_dir.join("receipts").join(format!("{commit}.json"))
}

/// Keep the range and survival result of a finish commit.
pub fn write_record(writ_dir: &Path, record: &ReceiptRecord) -> WritResult<()> {
    let path = record_path(writ_dir, &record.commit);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(record)?;
    crate::fsutil::atomic_write(&path, &json)?;
    Ok(())
}

/// The receipt a finish writes for `spec_ids`, after staging (content
/// from the git index), before committing.
pub fn for_finish(
    repo: &Repository,
    spec_ids: &[String],
    after: Option<DateTime<Utc>>,
    until: DateTime<Utc>,
    survival: SurvivalLine,
) -> WritResult<Receipt> {
    build(
        repo,
        spec_ids,
        after,
        until,
        &Content::GitIndex,
        survival,
        None,
    )
}

/// The receipts for a `--since` window: one per finish commit that
/// committed a spec with seals in the window (rebuilt from its record,
/// oldest first), then the pending receipt for the window's specs not
/// committed yet, if any. With nothing in the window, the pending receipt.
pub fn for_window(repo: &Repository, after: Option<DateTime<Utc>>) -> WritResult<Vec<Receipt>> {
    let specs = repo.list_specs()?;
    let mut commits: Vec<(DateTime<Utc>, String)> = Vec::new();
    let mut uncommitted = false;
    for spec in &specs {
        let in_window = repo
            .spec_seals(&spec.id)?
            .iter()
            .any(|s| after.is_none_or(|a| s.timestamp > a));
        if !in_window {
            continue;
        }
        match (&spec.commit_hash, spec.committed_at) {
            (Some(h), Some(t)) => {
                if !commits.iter().any(|(_, c)| c == h) {
                    commits.push((t, h.clone()));
                }
            }
            _ => uncommitted = true,
        }
    }
    commits.sort();
    let mut out = Vec::new();
    for (_, c) in &commits {
        out.push(for_commit(repo, c)?);
    }
    if uncommitted || out.is_empty() {
        out.push(pending_in(repo, after)?);
    }
    Ok(out)
}

/// Join several receipts the way `--receipt` writes them.
pub fn render_all(receipts: &[Receipt], pr: bool) -> String {
    if pr {
        receipts
            .iter()
            .map(render_pr)
            .collect::<Vec<_>>()
            .join("\n---\n\n")
    } else {
        receipts
            .iter()
            .map(render_commit)
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// The receipt of a pending finish: the completed specs a finish would
/// commit (their sealed content, the same survival check finish runs),
/// and the open specs with seals since the last finish, marked open.
pub fn pending(repo: &Repository) -> WritResult<Receipt> {
    let specs = repo.list_specs()?;
    pending_in(repo, last_finish(&specs))
}

fn pending_in(repo: &Repository, after: Option<DateTime<Utc>>) -> WritResult<Receipt> {
    let specs = repo.list_specs()?;
    let ids: Vec<String> = specs
        .iter()
        .filter(|s| s.is_committable())
        .map(|s| s.id.clone())
        .collect();
    let mut listed = ids.clone();
    for spec in specs.iter().filter(|s| {
        !s.is_committable()
            && s.commit_hash.is_none()
            && !s.sealed_by.is_empty()
            && !matches!(s.lifecycle_state, crate::spec::LifecycleState::Cancelled)
    }) {
        if repo
            .spec_seals(&spec.id)?
            .iter()
            .any(|s| after.is_none_or(|a| s.timestamp > a))
        {
            listed.push(spec.id.clone());
        }
    }
    let sealed: Vec<String> = specs
        .iter()
        .filter(|s| ids.contains(&s.id) && !s.sealed_by.is_empty())
        .map(|s| s.id.clone())
        .collect();
    let survival = if sealed.is_empty() {
        SurvivalLine::nothing_sealed()
    } else {
        let report = crate::convergence::survival::audit_finish_plan(repo, &sealed)?;
        SurvivalLine::from_report(&report, sealed.len(), false)
    };
    let plan = repo.finish_plan(&ids)?;
    let stage = plan.stage.into_iter().collect();
    build(
        repo,
        &listed,
        after,
        Utc::now(),
        &Content::Plan(stage),
        survival,
        None,
    )
}

/// The receipt of a finish commit, rebuilt from its record. A commit with
/// no record (made before 0.4.3) gets the specs marked with it and the
/// range from the finish before, and says survival was not recorded.
pub fn for_commit(repo: &Repository, rev: &str) -> WritResult<Receipt> {
    let commit = git::rev_parse(repo.root(), rev)?;
    let path = record_path(repo.writ_dir(), &commit);
    let record = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<ReceiptRecord>(&bytes)?,
        Err(_) => {
            let specs = repo.list_specs()?;
            let mine: Vec<&Spec> = specs
                .iter()
                .filter(|s| s.commit_hash.as_deref() == Some(commit.as_str()))
                .collect();
            if mine.is_empty() {
                return Err(WritError::Other(format!(
                    "no writ finish recorded commit {}: no receipt and no spec marked with it",
                    &commit[..commit.len().min(12)]
                )));
            }
            let until = mine
                .iter()
                .filter_map(|s| s.committed_at)
                .max()
                .unwrap_or_else(Utc::now);
            let after = specs
                .iter()
                .filter_map(|s| s.committed_at)
                .filter(|t| *t < until && !mine.iter().any(|m| m.committed_at == Some(*t)))
                .max();
            ReceiptRecord {
                commit: commit.clone(),
                specs: mine.iter().map(|s| s.id.clone()).collect(),
                after,
                until,
                survival: SurvivalLine::not_recorded(),
            }
        }
    };
    build(
        repo,
        &record.specs,
        record.after,
        record.until,
        &Content::Commit(&commit),
        record.survival,
        Some(commit.clone()),
    )
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn short(h: &str) -> &str {
    &h[..h.len().min(12)]
}

fn minute(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d %H:%M UTC").to_string()
}

fn span(first: DateTime<Utc>, last: DateTime<Utc>) -> String {
    let mins = (last - first).num_minutes();
    let d = if mins < 1 {
        "under 1m".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else {
        format!("{}h {}m", mins / 60, mins % 60)
    };
    format!(
        "over {d} ({} to {})",
        first.format("%Y-%m-%d %H:%M"),
        last.format("%H:%M UTC")
    )
}

/// The line for a spec with no closing words: the absence, and what the
/// seals record (Aubs's rule: never a synthesized sentence).
pub fn no_summary_line(s: &ReceiptSpec) -> String {
    let kind = if s.seals > 0 && s.auto_seals == s.seals {
        format!("{} auto {}", s.seals, seals_word(s.seals))
    } else if s.auto_seals > 0 {
        format!(
            "{} {}, {} auto,",
            s.seals,
            seals_word(s.seals),
            s.auto_seals
        )
    } else {
        format!("{} {}", s.seals, seals_word(s.seals))
    };
    match (s.first_seal, s.last_seal) {
        (Some(f), Some(l)) if s.seals > 1 => {
            format!("no summary from the agent; {kind} {}", span(f, l))
        }
        (Some(f), _) => format!("no summary from the agent; {kind} at {}", minute(f)),
        _ => "no summary from the agent; no seals".to_string(),
    }
}

fn seals_word(n: usize) -> &'static str {
    if n == 1 {
        "seal"
    } else {
        "seals"
    }
}

/// A resolution as a person reads it.
pub fn resolution_words(r: &str) -> String {
    match r {
        "accepted_after_told" => "accepted after being told".into(),
        "rewrote_own" => "rewrote own lines".into(),
        "moved" => "moved".into(),
        other => other.replace('_', " "),
    }
}

fn words_line(w: &ClosingWords, max: Option<usize>) -> String {
    let text = match max {
        Some(n) if w.text.chars().count() > n => {
            let cut: String = w.text.chars().take(n).collect();
            format!("{} [...]", cut.trim_end())
        }
        _ => w.text.clone(),
    };
    let text = text.replace('\n', " ");
    match &w.agent {
        Some(a) => format!("in {a}'s own words: \"{text}\""),
        None => format!("in the closing agent's own words: \"{text}\""),
    }
}

fn spec_head(s: &ReceiptSpec) -> String {
    format!(
        "{} ({}; {} {}): {}",
        s.id,
        if s.agents.is_empty() {
            "no agent".to_string()
        } else {
            s.agents.join(", ")
        },
        s.seals,
        seals_word(s.seals),
        s.title
    )
}

/// One line per spec: id, agents, seal count, title, then the agent's
/// closing words verbatim or the recorded absence of them.
fn spec_line(s: &ReceiptSpec, max_words: Option<usize>) -> String {
    let tail = match &s.words {
        Some(w) => words_line(w, max_words),
        None => no_summary_line(s),
    };
    let open = if s.open {
        " [open, not in a finish yet]"
    } else {
        ""
    };
    format!("{}{open}; {tail}", spec_head(s))
}

fn refusal_line(r: &ReceiptRefusal) -> String {
    format!(
        "{} refused {} for {} ({}): specs {}{}",
        r.kind,
        r.at.format("%Y-%m-%d %H:%M UTC"),
        r.agent.as_deref().unwrap_or("unknown"),
        r.reason,
        if r.specs.is_empty() {
            "none".to_string()
        } else {
            r.specs.join(", ")
        },
        if r.files.is_empty() {
            String::new()
        } else {
            format!("; files {}", r.files.join(", "))
        }
    )
}

fn capped<T>(items: &[T], f: impl Fn(&T) -> String, out: &mut Vec<String>) {
    for it in items.iter().take(SECTION_CAP) {
        out.push(format!("- {}", f(it)));
    }
    if items.len() > SECTION_CAP {
        out.push(format!("- +{} more", items.len() - SECTION_CAP));
    }
}

/// The commit body: spec list first, the survival line last, at most
/// [`COMMIT_BODY_MAX_LINES`] lines. Specs that do not fit are counted on
/// one line that names the command printing the full receipt.
pub fn render_commit(r: &Receipt) -> String {
    let mut tail: Vec<String> = Vec::new();
    if !r.shared.is_empty() {
        tail.push(format!("Shared files ({}):", r.shared.len()));
        capped(
            &r.shared,
            |m| {
                format!(
                    "{}: {} ({})",
                    m.path,
                    resolution_words(&m.resolution),
                    m.sealers.join("; ")
                )
            },
            &mut tail,
        );
    }
    if !r.also_sealed.is_empty() {
        tail.push(format!(
            "Also sealed by specs not in this commit ({}):",
            r.also_sealed.len()
        ));
        capped(
            &r.also_sealed,
            |a| format!("{} ({})", a.path, a.specs.join(", ")),
            &mut tail,
        );
    }
    if !r.moves.is_empty() {
        tail.push(format!("Moved ({}):", r.moves.len()));
        capped(&r.moves, |m| m.clone(), &mut tail);
    }
    if !r.overrides.is_empty() {
        tail.push(format!("Overrides accepted ({}):", r.overrides.len()));
        capped(&r.overrides, |o| o.clone(), &mut tail);
    }
    if !r.refusals.is_empty() {
        tail.push(format!("Refusals in range ({}):", r.refusals.len()));
        capped(&r.refusals, refusal_line, &mut tail);
    }
    if let Some(l) = check_off_line(r) {
        tail.push(l);
    }
    tail.push(r.survival.text());

    let mut out = vec![format!("writ receipt: {} spec(s)", r.specs.len())];
    // Room for spec lines: total minus header, tail, and a "+N more" line.
    let room = COMMIT_BODY_MAX_LINES.saturating_sub(1 + tail.len());
    let mut shown = 0;
    for s in &r.specs {
        let left = r.specs.len() - shown - 1;
        if shown + 1 + usize::from(left > 0) > room {
            break;
        }
        out.push(format!("- {}", spec_line(s, Some(WORDS_MAX_CHARS))));
        shown += 1;
    }
    if shown < r.specs.len() {
        out.push(format!(
            "- +{} more spec(s); full receipt: `writ handoff --commit <this commit> --format pr`",
            r.specs.len() - shown
        ));
    }
    out.extend(tail);
    out.join("\n")
}

/// The full receipt as markdown, for a PR description.
pub fn render_pr(r: &Receipt) -> String {
    let mut s = String::from("## writ receipt\n\n");
    s.push_str(&format!(
        "Commit: {}. Range: {} to {}.\n\n",
        r.commit
            .as_deref()
            .map(|c| format!("`{}`", short(c)))
            .unwrap_or_else(|| "not yet committed".into()),
        r.after
            .map(|t| format!("after the finish at {}", t.to_rfc3339()))
            .unwrap_or_else(|| "from the first seal".into()),
        r.until.to_rfc3339()
    ));
    s.push_str(&format!("### Specs ({})\n\n", r.specs.len()));
    if r.specs.is_empty() {
        s.push_str("None.\n");
    }
    for sp in &r.specs {
        s.push_str(&format!(
            "- `{}` {}{}: {} {} by {}\n",
            sp.id,
            sp.title,
            if sp.open {
                " [open, not in a finish yet]"
            } else {
                ""
            },
            sp.seals,
            seals_word(sp.seals),
            if sp.agents.is_empty() {
                "no agent".to_string()
            } else {
                sp.agents.join(", ")
            }
        ));
        let tail = match &sp.words {
            Some(w) => words_line(w, None),
            None => no_summary_line(sp),
        };
        s.push_str(&format!("  > {tail}\n"));
    }
    s.push_str(&format!("\n### Shared files ({})\n\n", r.shared.len()));
    if r.shared.is_empty() {
        s.push_str("No file was sealed by more than one committed spec.\n");
    }
    for m in &r.shared {
        s.push_str(&format!(
            "- `{}` sealed by {}: **{}**. {}\n",
            m.path,
            m.sealers.join("; "),
            resolution_words(&m.resolution),
            m.detail
        ));
    }
    if !r.also_sealed.is_empty() {
        s.push_str(&format!(
            "\n### Also sealed by specs not in this commit ({})\n\n",
            r.also_sealed.len()
        ));
        for a in &r.also_sealed {
            s.push_str(&format!("- `{}` ({})\n", a.path, a.specs.join(", ")));
        }
    }
    if !r.moves.is_empty() {
        s.push_str(&format!("\n### Moved ({})\n\n", r.moves.len()));
        for m in &r.moves {
            s.push_str(&format!("- {m}\n"));
        }
    }
    s.push_str(&format!(
        "\n### Overrides accepted ({})\n\n",
        r.overrides.len()
    ));
    if r.overrides.is_empty() {
        s.push_str("None.\n");
    }
    for o in &r.overrides {
        s.push_str(&format!("- {o}\n"));
    }
    s.push_str(&format!(
        "\n### Refusals in range ({})\n\n",
        r.refusals.len()
    ));
    if r.refusals.is_empty() {
        s.push_str("None.\n");
    }
    for f in &r.refusals {
        s.push_str(&format!("- {}\n", refusal_line(f)));
    }
    if let Some(l) = check_off_line(r) {
        s.push_str(&format!("\n{l}\n"));
    }
    s.push_str(&format!("\n### Survival\n\n{}\n", r.survival.text()));
    s
}

// ---------------------------------------------------------------------------
// Git reads (read only)
// ---------------------------------------------------------------------------

#[cfg(feature = "bridge")]
mod git {
    use super::*;

    fn open(root: &Path) -> WritResult<git2::Repository> {
        git2::Repository::discover(root).map_err(|_| WritError::NoGitRepo)
    }

    pub fn rev_parse(root: &Path, rev: &str) -> WritResult<String> {
        let repo = open(root)?;
        let obj = repo
            .revparse_single(rev)
            .map_err(|e| WritError::Other(format!("'{rev}' is not a commit: {e}")))?;
        let commit = obj
            .peel_to_commit()
            .map_err(|e| WritError::Other(format!("'{rev}' is not a commit: {e}")))?;
        Ok(commit.id().to_string())
    }

    pub fn blob_at(root: &Path, rev: &str, path: &str) -> WritResult<Option<Vec<u8>>> {
        let repo = open(root)?;
        let Ok(obj) = repo.revparse_single(rev) else {
            return Ok(None);
        };
        let tree = obj.peel_to_tree()?;
        let Ok(entry) = tree.get_path(Path::new(path)) else {
            return Ok(None);
        };
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return Ok(None);
        }
        let blob = repo.find_blob(entry.id())?;
        let bytes = blob.content().to_vec();
        Ok(Some(bytes))
    }

    pub fn index_blob(root: &Path, path: &str) -> WritResult<Option<Vec<u8>>> {
        let repo = open(root)?;
        let index = repo.index()?;
        let Some(entry) = index.get_path(Path::new(path), 0) else {
            return Ok(None);
        };
        let blob = repo.find_blob(entry.id)?;
        let bytes = blob.content().to_vec();
        Ok(Some(bytes))
    }
}

#[cfg(not(feature = "bridge"))]
mod git {
    use super::*;

    pub fn rev_parse(_: &Path, _: &str) -> WritResult<String> {
        Err(WritError::NoGitRepo)
    }
    pub fn blob_at(_: &Path, _: &str, _: &str) -> WritResult<Option<Vec<u8>>> {
        Err(WritError::NoGitRepo)
    }
    pub fn index_blob(_: &Path, _: &str) -> WritResult<Option<Vec<u8>>> {
        Err(WritError::NoGitRepo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, seals: usize, auto: usize, words: Option<&str>) -> ReceiptSpec {
        let t0 = DateTime::parse_from_rfc3339("2026-10-14T18:02:00Z")
            .unwrap()
            .with_timezone(&Utc);
        ReceiptSpec {
            id: id.into(),
            title: format!("title of {id}"),
            agents: vec!["haris".into()],
            seals,
            auto_seals: auto,
            first_seal: Some(t0),
            last_seal: Some(t0 + chrono::Duration::minutes(72)),
            words: words.map(|w| ClosingWords {
                agent: Some("haris".into()),
                text: w.into(),
            }),
            open: false,
        }
    }

    fn receipt(specs: Vec<ReceiptSpec>) -> Receipt {
        Receipt {
            commit: None,
            after: None,
            until: Utc::now(),
            specs,
            shared: Vec::new(),
            also_sealed: Vec::new(),
            moves: Vec::new(),
            overrides: Vec::new(),
            refusals: Vec::new(),
            survival: SurvivalLine {
                checked: 120,
                specs: 2,
                lost: 0,
                forced: false,
                recorded: true,
                seals: None,
            },
            check_off_seals: 0,
        }
    }

    #[test]
    fn commit_form_lists_specs_first_and_survival_last() {
        let r = receipt(vec![spec("a", 3, 0, Some("did a")), spec("b", 2, 0, None)]);
        let body = render_commit(&r);
        let lines: Vec<&str> = body.lines().collect();
        assert!(lines[1].starts_with("- a "));
        assert_eq!(
            *lines.last().unwrap(),
            "Survival: 120 sealed line(s) of 2 spec(s) checked, none lost"
        );
        assert!(body.contains("in haris's own words: \"did a\""));
    }

    #[test]
    fn commit_form_stays_under_forty_lines_at_fleet_size() {
        let specs = (0..50)
            .map(|i| spec(&format!("s{i}"), 20, 0, Some("x")))
            .collect();
        let mut r = receipt(specs);
        r.overrides = vec!["o".into(); 9];
        r.shared = (0..9)
            .map(|i| ReceiptShared {
                path: format!("f{i}"),
                specs: vec!["a".into(), "b".into()],
                sealers: vec!["a by x".into(), "b by y".into()],
                resolution: "kept".into(),
                detail: String::new(),
            })
            .collect();
        let body = render_commit(&r);
        assert!(body.lines().count() <= COMMIT_BODY_MAX_LINES, "{body}");
        assert!(body.contains("more spec(s); full receipt"));
        assert!(body.lines().last().unwrap().starts_with("Survival:"));
    }

    #[test]
    fn absent_summary_is_stated_never_synthesized() {
        let s = spec("auto-abc", 14, 14, None);
        assert_eq!(
            no_summary_line(&s),
            "no summary from the agent; 14 auto seals over 1h 12m (2026-10-14 18:02 to 19:14 UTC)"
        );
        let mixed = spec("m", 5, 2, None);
        assert!(no_summary_line(&mixed).starts_with("no summary from the agent; 5 seals, 2 auto,"));
    }

    #[test]
    fn survival_line_says_force_and_unrecorded() {
        let mut l = SurvivalLine::from_report(
            &AuditReport {
                against: crate::convergence::survival::Against::FinishTree,
                specs: Vec::new(),
                checked: 10,
                lost: 2,
                superseded: 0,
                skipped: 0,
                not_committed: Vec::new(),
                elapsed_ms: 0,
            },
            1,
            true,
        );
        assert_eq!(
            l.text(),
            "Survival: 10 sealed line(s) of 1 spec(s) checked, 2 lost; finished with --force"
        );
        l = SurvivalLine::not_recorded();
        assert!(l.text().contains("not recorded"));
    }
}
