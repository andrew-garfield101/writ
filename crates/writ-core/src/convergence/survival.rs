//! Survival checks: no line a spec added is lost without being reported.
//!
//! Two checks, both run before anything is written or staged:
//!
//! - **Merge survival** (finding 45). For each input side of a merge, every
//!   line it added relative to the merge base must appear in the merged
//!   output, in order, unless a reported conflict covers that base range.
//!   A miss becomes an escalation naming the spec, the path and the lines.
//! - **Own-line survival** (finding 42). A seal must not revert lines that
//!   earlier seals of the same spec added. That is the shape of a seal that
//!   captured another agent's version of the file from disk: the spec's own
//!   hunk goes back to the base text, or disappears, and nothing replaces
//!   it. Edits that replace an own line with new text are not losses.
//!
//! Both checks align with [`crate::diff::diff_ops`] (linear-space Myers), so
//! "appears in order" means "kept by a longest common subsequence".

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{ConflictRegion, PipelineEscalation};
use crate::diff::{diff_ops, EditOp};
use crate::seal::{ChangeType, Seal};

/// `conflict_class` of a merge survival escalation.
pub const MERGE_LOSS_CLASS: &str = "merge_survival_loss";
/// `conflict_class` of an own-line survival escalation.
pub const OWN_LINE_LOSS_CLASS: &str = "own_line_removed";
/// `conflict_class` of a resurrection escalation (finding 62).
pub const MERGE_RESURRECTION_CLASS: &str = "merge_resurrection";

/// Which check found the loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LossKind {
    /// A merge output lacks lines one input side added.
    Merge,
    /// A seal reverts lines earlier seals of the same spec added.
    OwnLines,
    /// A merge output brings back lines a descendant spec removed from the
    /// version it continued (finding 62). `spec` is the removing spec and
    /// `ranges` are line numbers in the merge output.
    Resurrected,
}

/// Lines a spec added that a merge or a seal would lose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurvivalLoss {
    pub kind: LossKind,
    /// Spec whose lines are lost.
    pub spec: String,
    /// Repository-relative path.
    pub path: String,
    /// 1-based inclusive line ranges in the spec's own version of the file.
    pub ranges: Vec<(usize, usize)>,
    /// The lost lines, in order.
    pub lines: Vec<String>,
    /// Own-line check only: the foreign lines in the recorded file that made
    /// the removal look like a capture of another agent's version (finding
    /// 56), first occurrences in file order, at most [`MAX_FOREIGN_SHOWN`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign: Vec<String>,
}

/// Foreign lines kept on a [`SurvivalLoss`] for display.
pub const MAX_FOREIGN_SHOWN: usize = 10;

impl SurvivalLoss {
    /// Number of lost lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// `3`, `3-5, 9` style rendering of [`Self::ranges`].
    pub fn ranges_label(&self) -> String {
        self.ranges
            .iter()
            .map(|&(a, b)| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The same loss as a convergence escalation. `other` names the
    /// version the lines are missing from (the merge output or the seal).
    pub fn to_escalation(&self, other: &str) -> PipelineEscalation {
        let (class, action) = match self.kind {
            LossKind::Merge => (
                MERGE_LOSS_CLASS,
                "Merge output dropped lines this spec sealed; resolve by hand. Nothing was written",
            ),
            LossKind::OwnLines => (
                OWN_LINE_LOSS_CLASS,
                "Seal reverts lines this spec sealed earlier; check the file on disk",
            ),
            LossKind::Resurrected => (
                MERGE_RESURRECTION_CLASS,
                "Merge output brings back lines this spec removed; resolve by hand. Nothing was written",
            ),
        };
        PipelineEscalation {
            file_path: self.path.clone(),
            // file_path carries the path; the reason starts at the lines.
            reason: self
                .to_string()
                .trim_start_matches(&format!("{}: ", self.path))
                .to_string(),
            conflict_class: class.to_string(),
            left_spec: other.to_string(),
            right_spec: self.spec.clone(),
            recommended_action: action.to_string(),
            left_content: None,
            right_content: Some(self.lines.join("\n")),
            suggested_content: None,
            suggestion_confidence: None,
        }
    }
}

impl fmt::Display for SurvivalLoss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            LossKind::Merge => "added by spec",
            LossKind::OwnLines => "sealed earlier by spec",
            LossKind::Resurrected => "of the merge output were removed by spec",
        };
        let outcome = match self.kind {
            LossKind::Resurrected => "and would come back",
            _ => "would be lost",
        };
        let n = self.lines.len();
        write!(
            f,
            "{}: line{} {} {what} '{}' {outcome}",
            self.path,
            if n == 1 { "" } else { "s" },
            self.ranges_label(),
            self.spec,
        )?;
        if let Some(first) = self.lines.first() {
            write!(f, " (\"{}\"", truncate(first, 60))?;
            if n > 1 {
                write!(f, " and {} more", n - 1)?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// A range of base lines, 0-based and half-open. A conflict that a merge
/// reported covers its base range; additions anchored there are exempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseSpan {
    pub start: usize,
    pub end: usize,
}

impl BaseSpan {
    /// Covers every addition in the file (a conflict reported for the whole file).
    pub fn whole_file() -> Self {
        Self {
            start: 0,
            end: usize::MAX,
        }
    }

    /// Overlapping or adjacent. Adjacency counts because an insertion has
    /// an empty span and diff3 and Myers may anchor it one line apart.
    fn touches(&self, other: &BaseSpan) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// Base spans of diff3 conflict regions (`base_start` is 1-based).
pub fn conflict_spans(regions: &[ConflictRegion]) -> Vec<BaseSpan> {
    regions
        .iter()
        .map(|r| {
            let start = r.base_start.saturating_sub(1);
            BaseSpan {
                start,
                end: start + r.base_lines.len(),
            }
        })
        .collect()
}

fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.lines().collect()
    }
}

/// One changed region of an edit script: old lines removed, new lines inserted.
struct Hunk {
    old_span: BaseSpan,
    deleted: Vec<usize>,
    inserted: Vec<usize>,
}

fn hunks(old: &[&str], new: &[&str]) -> Vec<Hunk> {
    let ops = diff_ops(old, new);
    let mut out = Vec::new();
    let mut old_pos = 0;
    let mut i = 0;
    while i < ops.len() {
        if let EditOp::Equal(o, _) = ops[i] {
            old_pos = o + 1;
            i += 1;
            continue;
        }
        let mut hunk = Hunk {
            old_span: BaseSpan {
                start: old_pos,
                end: old_pos,
            },
            deleted: Vec::new(),
            inserted: Vec::new(),
        };
        while i < ops.len() {
            match ops[i] {
                EditOp::Equal(..) => break,
                EditOp::Delete(o) => {
                    hunk.deleted.push(o);
                    hunk.old_span.end = o + 1;
                }
                EditOp::Insert(n) => hunk.inserted.push(n),
            }
            i += 1;
        }
        out.push(hunk);
    }
    out
}

/// For each `side` line, whether aligning `side` to `merged` drops it.
fn dropped(side: &[&str], merged: &[&str]) -> Vec<bool> {
    let mut gone = vec![false; side.len()];
    for op in diff_ops(side, merged) {
        if let EditOp::Delete(o) = op {
            gone[o] = true;
        }
    }
    gone
}

/// Indices (0-based, ascending) of `side` lines that `side` added relative
/// to `base` and that `merged` lost, excluding additions whose base range a
/// reported conflict covers.
pub fn lost_additions(base: &str, side: &str, merged: &str, covered: &[BaseSpan]) -> Vec<usize> {
    if side == merged {
        return Vec::new();
    }
    let (b, s, m) = (split_lines(base), split_lines(side), split_lines(merged));
    let gone = dropped(&s, &m);
    let mut candidates = Vec::new();
    for hunk in hunks(&b, &s) {
        if covered.iter().any(|c| c.touches(&hunk.old_span)) {
            continue;
        }
        candidates.extend(hunk.inserted.into_iter().filter(|&j| gone[j]));
    }
    content_gone(candidates, &s, &counts(m.iter().copied()))
}

fn counts<'a>(lines: impl IntoIterator<Item = &'a str>) -> HashMap<&'a str, usize> {
    let mut out = HashMap::new();
    for l in lines {
        *out.entry(l).or_insert(0) += 1;
    }
    out
}

/// Finding 53: a line survives if its content is still in the output, at
/// any position (a moved block is not a loss). Of the `candidates` (indices
/// into `src` that the positional alignment dropped), keep only as many per
/// content as the output is short of: `count(src) - count(output)`, the
/// later occurrences first counted as surviving. Ascending.
fn content_gone(candidates: Vec<usize>, src: &[&str], out: &HashMap<&str, usize>) -> Vec<usize> {
    if candidates.is_empty() {
        return candidates;
    }
    let src_counts = counts(src.iter().copied());
    let mut deficit: HashMap<&str, usize> = src_counts
        .iter()
        .map(|(&l, &n)| (l, n.saturating_sub(out.get(l).copied().unwrap_or(0))))
        .collect();
    let mut lost: Vec<usize> = candidates
        .into_iter()
        .filter(|&i| match deficit.get_mut(src[i]) {
            Some(d) if *d > 0 => {
                *d -= 1;
                true
            }
            _ => false,
        })
        .collect();
    lost.sort_unstable();
    lost
}

/// 1-based inclusive ranges from ascending 0-based indices.
fn to_ranges(idx: &[usize]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in idx {
        match out.last_mut() {
            Some((_, end)) if *end == i => *end = i + 1,
            _ => out.push((i + 1, i + 1)),
        }
    }
    out
}

fn loss(kind: LossKind, spec: &str, path: &str, text: &str, idx: &[usize]) -> SurvivalLoss {
    let lines = split_lines(text);
    SurvivalLoss {
        kind,
        spec: spec.to_string(),
        path: path.to_string(),
        ranges: to_ranges(idx),
        lines: idx.iter().map(|&i| lines[i].to_string()).collect(),
        foreign: Vec::new(),
    }
}

/// Merge survival for one file. `sides` are `(spec, content)` pairs, each
/// spec's own version of the file; `covered` are the base spans of
/// conflicts the merge reported.
pub fn merge_losses(
    path: &str,
    base: &str,
    sides: &[(String, String)],
    merged: &str,
    covered: &[BaseSpan],
) -> Vec<SurvivalLoss> {
    sides
        .iter()
        .filter_map(|(spec, side)| {
            let idx = lost_additions(base, side, merged, covered);
            (!idx.is_empty()).then(|| loss(LossKind::Merge, spec, path, side, &idx))
        })
        .collect()
}

/// Lines a seal removed on purpose: in its recorded base (`old`), not in
/// what it recorded (`new`). The informed-removal rule (CC, decision on
/// finding 56): a removal is intentional if and only if the removing seal's
/// own recorded base contained the line.
///
/// Counted per copy: a line the seal kept one copy of but removed another
/// (common text like `None` or `} else {`) counts once per removed copy.
pub fn informed_removals<'a>(old: &'a str, new: &str) -> HashMap<&'a str, usize> {
    let kept = counts(new.lines());
    counts(old.lines())
        .into_iter()
        .filter_map(|(l, n)| {
            let gone = n.saturating_sub(kept.get(l).copied().unwrap_or(0));
            (gone > 0).then_some((l, gone))
        })
        .collect()
}

/// [`merge_losses`] that accepts informed removals: a side's addition the
/// merge dropped is not a loss when it is in `excused[spec]`, the lines a
/// later seal outside that spec removed while its recorded base held them
/// (see [`informed_removals`]; the caller picks the seals). Concurrent
/// edits (a removing seal whose base never held the line) are still
/// losses, whichever side the merge kept.
pub fn merge_losses_informed(
    path: &str,
    base: &str,
    sides: &[(String, String)],
    excused: &HashMap<String, HashMap<String, usize>>,
    merged: &str,
    covered: &[BaseSpan],
) -> Vec<SurvivalLoss> {
    sides
        .iter()
        .filter_map(|(spec, side)| {
            let lines = split_lines(side);
            let mut ok = excused.get(spec).cloned().unwrap_or_default();
            let idx: Vec<usize> = lost_additions(base, side, merged, covered)
                .into_iter()
                .filter(|&i| match ok.get_mut(lines[i]) {
                    Some(n) if *n > 0 => {
                        *n -= 1;
                        false
                    }
                    _ => true,
                })
                .collect();
            (!idx.is_empty()).then(|| loss(LossKind::Merge, spec, path, side, &idx))
        })
        .collect()
}

/// Does a file a seal recorded as its "before" (`before`) already contain
/// `version`'s edits relative to `base`? True when every line `version`
/// added is in `before`, and every base line `version` removed is gone
/// from `before` as often. Used for descent (finding 62): a spec whose
/// seal started from another spec's version continues it.
pub fn contains_edits(base: &str, version: &str, before: &str) -> bool {
    if version == before {
        return true;
    }
    if !lost_additions(base, version, before, &[]).is_empty() {
        return false;
    }
    let have = counts(before.lines());
    let kept = counts(version.lines());
    informed_removals(base, version)
        .keys()
        .all(|l| have.get(l).copied().unwrap_or(0) <= kept.get(l).copied().unwrap_or(0))
}

/// The dual of merge survival (finding 62): every later deletion survives.
/// For each `(removing spec, ancestor version, descendant version)`, a line
/// the descendant removed from the ancestor it continued must not be in the
/// merge output, beyond the copies the descendant kept and the copies other
/// merged sides added relative to `base`. `sides` are the versions that took
/// part in the merge (superseded ancestors excluded); the remover's own
/// entry is ignored.
/// Excess copies are reported at their positions in `merged`.
pub fn resurrections(
    path: &str,
    base: &str,
    sides: &[(String, String)],
    descents: &[(String, String, String)],
    merged: &str,
) -> Vec<SurvivalLoss> {
    let out_lines = split_lines(merged);
    let in_merged = counts(out_lines.iter().copied());
    let in_base = counts(base.lines());
    let mut found = Vec::new();
    for (remover, ancestor, descendant) in descents {
        let removed = informed_removals(ancestor, descendant);
        let kept = counts(descendant.lines());
        let mut excess: HashMap<&str, usize> = HashMap::new();
        for (line, _) in removed {
            let others: usize = sides
                .iter()
                .filter(|(s, _)| s != remover)
                .map(|(_, t)| {
                    let n = t.lines().filter(|l| *l == line).count();
                    n.saturating_sub(in_base.get(line).copied().unwrap_or(0))
                })
                .sum();
            let allowed = kept.get(line).copied().unwrap_or(0) + others;
            let have = in_merged.get(line).copied().unwrap_or(0);
            if have > allowed {
                excess.insert(line, have - allowed);
            }
        }
        if excess.is_empty() {
            continue;
        }
        // Report the last copies in the output (the earlier ones are allowed).
        let mut idx: Vec<usize> = Vec::new();
        for (i, l) in out_lines.iter().enumerate().rev() {
            if let Some(n) = excess.get_mut(l) {
                if *n > 0 {
                    *n -= 1;
                    idx.push(i);
                }
            }
        }
        idx.sort_unstable();
        found.push(loss(LossKind::Resurrected, remover, path, merged, &idx));
    }
    found
}

/// Lines `version` added relative to `base`: `(0-based index in version, text)`.
pub fn additions<'a>(base: &str, version: &'a str) -> Vec<(usize, &'a str)> {
    let (b, v) = (split_lines(base), split_lines(version));
    hunks(&b, &v)
        .into_iter()
        .flat_map(|h| h.inserted)
        .map(|j| (j, v[j]))
        .collect()
}

/// The latest of an earlier spec's seals of a file that a later version
/// saw (finding 62, per-seal anchor; Aubs's refinement of option c).
///
/// `versions` are the earlier spec's sealed versions of the file, oldest
/// first, with `first_old` the file before its first seal. Seal `k` was seen
/// when `later` keeps at least one line seal `k` added (relative to the
/// version before it); a seal that added nothing is seen when `later` also
/// lacks what it removed. The anchor is the latest seen seal: the later
/// version's effective base. `None`: it saw none of them.
///
/// "Kept at least one line", not "kept all": a later version that removed
/// one of a seal's lines on purpose still saw that seal (Bri's fixture).
/// Only distinctive lines count: non-blank, and in neither the merge `base`
/// nor the file before that seal (a rewrite that restores base text says
/// nothing about which version was copied).
pub fn anchor_seal(base: &str, first_old: &str, versions: &[&str], later: &str) -> Option<usize> {
    let have = counts(later.lines());
    let in_base = counts(base.lines());
    let mut anchor = None;
    let mut prev = first_old;
    for (k, v) in versions.iter().enumerate() {
        let adds = additions(prev, v);
        let seen = if adds.is_empty() {
            let kept = counts(v.lines());
            let removed = informed_removals(prev, v);
            !removed.is_empty()
                && removed
                    .keys()
                    .all(|l| have.get(l).copied().unwrap_or(0) <= kept.get(l).copied().unwrap_or(0))
        } else {
            // Only distinctive lines are evidence: not blank, and not text
            // the file already had before this seal (a blank line or a
            // repeated `}` says nothing about which version was copied).
            // A seal with no distinctive line is undecidable: not seen,
            // which keeps its work.
            let before = counts(prev.lines());
            adds.iter().any(|(_, l)| {
                !l.trim().is_empty()
                    && !before.contains_key(l)
                    && !in_base.contains_key(l)
                    && have.contains_key(l)
            })
        };
        if seen {
            anchor = Some(k);
        }
        prev = v;
    }
    anchor
}

/// A non-blocking convergence notice for the spec whose version did not
/// carry another spec's lines (finding 62). The merge kept them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvergenceNotice {
    pub path: String,
    /// The spec (and agent) whose version lacked the lines.
    pub spec: String,
    pub agent: String,
    /// The spec (and agent) that added them.
    pub added_by_spec: String,
    pub added_by_agent: String,
    /// 1-based ranges in the adding spec's latest version of the file.
    pub ranges: Vec<(usize, usize)>,
    pub lines: Vec<String>,
    /// The command that seals a deliberate removal.
    pub command: String,
    pub message: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl ConvergenceNotice {
    /// Build a notice for `idx` (0-based) lines of `added_version`.
    pub fn new(
        path: &str,
        (spec, agent): (&str, &str),
        (added_by_spec, added_by_agent): (&str, &str),
        added_version: &str,
        idx: &[usize],
    ) -> Self {
        let all = split_lines(added_version);
        let lines: Vec<String> = idx.iter().map(|&i| all[i].to_string()).collect();
        let ranges = to_ranges(idx);
        let label = ranges
            .iter()
            .map(|&(a, b)| {
                if a == b {
                    a.to_string()
                } else {
                    format!("{a}-{b}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let command = format!(
            "writ seal --agent {agent} --spec {spec} --paths {path} -s \"remove lines kept by convergence\""
        );
        let shown: Vec<String> = lines
            .iter()
            .take(20)
            .map(|l| format!("\"{}\"", truncate(l, 120)))
            .collect();
        let more = lines.len().saturating_sub(20);
        let message = format!(
            "{path}: line(s) {label} added by spec '{added_by_spec}' (agent {added_by_agent}) were not in spec '{spec}''s version and were kept: {}{}. If '{spec}' meant to remove them, remove them again and seal: {command}",
            shown.join(", "),
            if more > 0 { format!(" and {more} more") } else { String::new() },
        );
        Self {
            path: path.to_string(),
            spec: spec.to_string(),
            agent: agent.to_string(),
            added_by_spec: added_by_spec.to_string(),
            added_by_agent: added_by_agent.to_string(),
            ranges,
            lines,
            command,
            message,
            created_at: chrono::Utc::now(),
        }
    }
}

/// Indices (0-based) of lines `version` added relative to `base` that
/// `other` does not contain (by content, counting copies).
pub fn additions_missing_from(base: &str, version: &str, other: &str) -> Vec<usize> {
    let mut have = counts(other.lines());
    let mut out = Vec::new();
    for (i, l) in additions(base, version) {
        match have.get_mut(l) {
            Some(n) if *n > 0 => *n -= 1,
            _ => out.push(i),
        }
    }
    out
}

/// Own-line survival for one file a seal is about to record.
///
/// `own_base` is the file before the spec's first seal of it, `own_last`
/// the file at the spec's latest seal of it, `new` what this seal records
/// (empty for a deletion). A line the spec added (in `own_last`, not in
/// `own_base`) is lost when the hunk that removes it inserts nothing, or
/// only lines that are already in `own_base`: the spec's work reverts to
/// the base. A hunk that replaces it with new text is an edit, not a loss.
/// A removed line whose content is still in `new` or in `elsewhere` (the
/// other files this seal records) moved and is not a loss (finding 53).
///
/// Only the capture shape is a loss: `new` must also hold foreign content,
/// a line that is neither in `own_base` nor in any of `known` (every
/// version of the file this spec sealed, and the file as last sealed by
/// anyone). Lines another agent already sealed are known: capturing them
/// loses nothing of theirs (finding 56). Without foreign content the spec
/// is editing its own work, removals included, and passes.
///
/// Intentional cleanup is not a loss (finding 69, sprint 3 allowances):
/// (b) a removed line whose content is still in `new` or `elsewhere` moved
/// (finding 53, above); (c) a removed line whose un-marked form
/// ([`unmark`]: `#[ignore = ".."] fn t()` → `fn t()`, `"x",  # ignored` →
/// `"x",`) this same seal *adds* is an un-ignore. Merely lacking the marked
/// line is not enough: a stale rewrite that happens to lack it would pass as
/// cleanup, which is finding 42 again. A dropped *standalone* annotation
/// (`#[ignore = ".."]` on its own line, a decorator) is excused when the
/// code line it annotated is still in `new` and was this spec's own
/// addition: the spec un-ignores its own test. A stale rewrite from before
/// the spec's seal lacks that line too and stays refused; a marker the spec
/// put on a base line is its only work there and its loss is refused.
/// (a) cross-spec removals are passed in by the caller through
/// [`own_line_loss_excused`].
pub fn own_line_loss(
    spec: &str,
    path: &str,
    own_base: &str,
    own_last: &str,
    known: &[&str],
    new: &str,
    elsewhere: &[&str],
) -> Option<SurvivalLoss> {
    own_line_loss_excused(
        spec,
        path,
        own_base,
        own_last,
        known,
        new,
        elsewhere,
        &HashMap::new(),
    )
}

/// [`own_line_loss`] with allowance (a), finding 69: `excused` counts, per
/// line content, the copies that seals of *other* specs already removed on
/// purpose (their recorded base held the line, see [`informed_removals`])
/// and that postdate this spec's latest seal of the path or belong to a
/// committed spec. A lost line is consumed from `excused` per copy; what
/// remains is the loss.
#[allow(clippy::too_many_arguments)]
pub fn own_line_loss_excused(
    spec: &str,
    path: &str,
    own_base: &str,
    own_last: &str,
    known: &[&str],
    new: &str,
    elsewhere: &[&str],
    excused: &HashMap<String, usize>,
) -> Option<SurvivalLoss> {
    if own_last == new {
        return None;
    }
    let (b, last, n) = (
        split_lines(own_base),
        split_lines(own_last),
        split_lines(new),
    );
    let known: HashSet<&str> = b
        .iter()
        .copied()
        .chain(last.iter().copied())
        .chain(known.iter().flat_map(|t| t.lines()))
        .collect();
    let mut seen = HashSet::new();
    let foreign: Vec<String> = n
        .iter()
        .copied()
        .filter(|l| !known.contains(l) && seen.insert(*l))
        .take(MAX_FOREIGN_SHOWN)
        .map(String::from)
        .collect();
    if foreign.is_empty() {
        return None;
    }
    let own: HashSet<usize> = hunks(&b, &last)
        .into_iter()
        .flat_map(|h| h.inserted)
        .collect();
    if own.is_empty() {
        return None;
    }
    let base_lines: HashSet<&str> = b.iter().copied().collect();
    let mut candidates: Vec<usize> = Vec::new();
    // Lines this seal adds relative to the spec's last version, per copy:
    // the evidence for allowance (c).
    let mut added_now: HashMap<&str, usize> = HashMap::new();
    for hunk in hunks(&last, &n) {
        for &j in &hunk.inserted {
            *added_now.entry(n[j]).or_insert(0) += 1;
        }
        let reverts = hunk.inserted.iter().all(|&j| base_lines.contains(n[j]));
        if reverts {
            candidates.extend(hunk.deleted.iter().copied().filter(|i| own.contains(i)));
        }
    }
    // (c) Un-ignore: the removed line's un-marked form is added by this
    // seal; or the removed line is a standalone annotation whose annotated
    // code line is still here and was this spec's own addition.
    let in_new = counts(n.iter().copied());
    candidates.retain(|&i| {
        if is_standalone_annotation(last[i]) {
            return match annotated_line(&last, i) {
                Some(code) => !(own.contains(&code) && in_new.contains_key(last[code])),
                None => true,
            };
        }
        let Some(unmarked) = unmark(last[i]) else {
            return true;
        };
        match added_now.get_mut(unmarked.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        }
    });
    let pool = counts(
        n.iter()
            .copied()
            .chain(elsewhere.iter().flat_map(|t| t.lines())),
    );
    // (b) Moved content survives (finding 53).
    let mut lost = content_gone(candidates, &last, &pool);
    // (a) Removals other specs already sealed on purpose.
    if !excused.is_empty() {
        let mut left = excused.clone();
        lost.retain(|&i| match left.get_mut(last[i]) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        });
    }
    (!lost.is_empty()).then(|| SurvivalLoss {
        foreign,
        ..loss(LossKind::OwnLines, spec, path, own_last, &lost)
    })
}

/// The un-marked form of an annotated line (allowance (c), finding 69).
///
/// Strips exactly one annotation: a leading attribute or decorator
/// (`#[ignore = "reason"]`, `@pytest.mark.skip(...)`) followed by code on
/// the same line, or a trailing comment (`# ...`, `// ...`, `/* ... */`)
/// after code. Indentation is kept. `None` when the line carries no
/// annotation, or is nothing but one: a standalone `#[ignore]` line has no
/// un-marked form, so dropping it is never excused by this rule.
pub fn unmark(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    let body = trimmed.trim_start();
    let indent = &trimmed[..trimmed.len() - body.len()];
    if let Some(rest) = strip_leading_annotation(body) {
        let rest = rest.trim_start();
        return (!rest.is_empty()).then(|| format!("{indent}{rest}"));
    }
    let code = strip_trailing_comment(body)?.trim_end();
    (!code.is_empty() && code != body).then(|| format!("{indent}{code}"))
}

/// A line that is nothing but one attribute or decorator: `#[ignore]`,
/// `#[ignore = "reason"]`, `@pytest.mark.skip(reason="x")`, `@skip`.
pub fn is_standalone_annotation(line: &str) -> bool {
    let body = line.trim();
    match strip_leading_annotation(body) {
        Some(rest) => rest.trim().is_empty(),
        // `@skip` alone: no whitespace follows, so the strip refuses it.
        None => {
            body.starts_with('@')
                && body.len() > 1
                && body[1..]
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        }
    }
}

/// Index in `lines` of the code line the standalone annotation at `i`
/// applies to: the next line that is not itself a standalone annotation,
/// if it is not blank.
fn annotated_line(lines: &[&str], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < lines.len() && is_standalone_annotation(lines[j]) {
        j += 1;
    }
    (j < lines.len() && !lines[j].trim().is_empty()).then_some(j)
}

/// `#[...]` or `@name(.name)*(...)?` at the start of `body`; the rest after it.
fn strip_leading_annotation(body: &str) -> Option<&str> {
    if let Some(after) = body.strip_prefix("#[") {
        let end = matching_close(after, '[', ']')?;
        return Some(&after[end + 1..]);
    }
    let after = body.strip_prefix('@')?;
    let name_len = after
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
        .unwrap_or(after.len());
    if name_len == 0 {
        return None;
    }
    let rest = &after[name_len..];
    if let Some(args) = rest.strip_prefix('(') {
        let end = matching_close(args, '(', ')')?;
        return Some(&args[end + 1..]);
    }
    // A decorator without arguments must be followed by whitespace, not
    // more code glued to it (an email address, a Razor directive).
    rest.starts_with(char::is_whitespace).then_some(rest)
}

/// Index in `s` of the close that balances one already-open `open`,
/// skipping quoted strings.
fn matching_close(s: &str, open: char, close: char) -> Option<usize> {
    let mut depth = 1usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// `body` without a trailing `# ...`, `// ...` or `/* ... */` comment that
/// follows code and whitespace; `None` when there is no such comment.
fn strip_trailing_comment(body: &str) -> Option<&str> {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut prev_ws = false;
    let bytes = body.as_bytes();
    for (i, c) in body.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            prev_ws = false;
            continue;
        }
        if c == '"' || c == '\'' {
            quote = Some(c);
        } else if prev_ws && i > 0 {
            let starts_comment = c == '#'
                || (c == '/' && matches!(bytes.get(i + 1), Some(b'/')))
                || (c == '/' && matches!(bytes.get(i + 1), Some(b'*')) && body.ends_with("*/"));
            if starts_comment {
                return Some(&body[..i]);
            }
        }
        prev_ws = c.is_whitespace();
    }
    None
}

/// A spec's own view of one path across its seal chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnVersion {
    /// Content hash before the spec's first seal of the path (None: new file).
    pub first_old: Option<String>,
    /// Content hash at the spec's latest seal of the path (None: deleted).
    pub last_new: Option<String>,
    /// Every content hash the spec sealed for the path, oldest first.
    pub sealed: Vec<String>,
    /// Each seal's recorded `(old_hash, new_hash)` for the path, oldest first.
    pub changes: Vec<(Option<String>, Option<String>)>,
    /// When each of [`Self::changes`] was sealed.
    pub times: Vec<chrono::DateTime<chrono::Utc>>,
    /// Which agent sealed each of [`Self::changes`].
    pub agents: Vec<String>,
}

/// Per path, the spec's first and latest sealed versions, from its seals'
/// own change records (oldest first), never from the seal trees, which
/// snapshot every spec's files.
pub fn own_versions<'a>(seals: impl IntoIterator<Item = &'a Seal>) -> HashMap<String, OwnVersion> {
    let mut out: HashMap<String, OwnVersion> = HashMap::new();
    for seal in seals {
        for change in &seal.changes {
            let new = match change.change_type {
                ChangeType::Deleted => None,
                _ => change.new_hash.clone(),
            };
            let v = out
                .entry(change.path.clone())
                .or_insert_with(|| OwnVersion {
                    first_old: change.old_hash.clone(),
                    ..OwnVersion::default()
                });
            v.last_new = new.clone();
            v.changes.push((change.old_hash.clone(), new.clone()));
            v.times.push(seal.timestamp);
            v.agents.push(seal.agent.id.clone());
            v.sealed.extend(new);
        }
    }
    out
}

// ── Survival audit (0.4.1 survival tier groundwork) ────────────────────

/// What the sealed additions are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Against {
    /// The files on disk.
    WorkingTree,
    /// The files at git HEAD (needs the `bridge` feature and a repository).
    Head,
}

/// A sealed addition the target lacks and no later seal removed on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostLine {
    /// 1-based line in the spec's last sealed version of the file.
    pub line: usize,
    pub text: String,
}

/// A sealed addition the target lacks because a later seal removed it
/// while its recorded base held it (an informed removal; not a loss).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededLine {
    pub line: usize,
    pub text: String,
    pub by_seal: String,
    pub by_spec: Option<String>,
}

/// Why a file's additions were not line-checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAuditStatus {
    /// Every addition was checked.
    Checked,
    /// The spec's last seal deleted the file, and the target lacks it too.
    DeletedAsSealed,
    /// The spec's last seal deleted the file, but the target still has it.
    DeletedButPresent,
    /// The sealed content is binary.
    Binary,
    /// The sealed blob is missing from the store.
    Unreadable,
}

/// Survival of one spec's additions to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSurvival {
    pub path: String,
    /// The spec's last seal of the path.
    pub seal_id: String,
    pub status: FileAuditStatus,
    /// Added lines checked.
    pub checked: usize,
    pub lost: Vec<LostLine>,
    pub superseded: Vec<SupersededLine>,
}

/// Survival of one spec's sealed additions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecSurvival {
    pub spec_id: String,
    pub files: Vec<FileSurvival>,
    pub checked: usize,
    pub lost: usize,
    pub superseded: usize,
}

/// Result of [`audit`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReport {
    pub against: Against,
    pub specs: Vec<SpecSurvival>,
    pub checked: usize,
    pub lost: usize,
    pub superseded: usize,
    pub elapsed_ms: u64,
}

impl AuditReport {
    /// No sealed addition is missing without a later informed removal.
    pub fn is_green(&self) -> bool {
        self.lost == 0
            && self
                .specs
                .iter()
                .flat_map(|s| &s.files)
                .all(|f| f.status != FileAuditStatus::DeletedButPresent)
    }
}

/// Added-line survival of `spec_ids` (the core of `bench/finish_audit.py`).
///
/// For each spec and each path its seals captured: the lines the spec's
/// last sealed version of the file added relative to the file before the
/// spec's first seal of it must be in the target, by content and per copy
/// (a moved block survives, finding 53). A missing line is *superseded*
/// when a later seal, of any spec, removed it while its recorded base held
/// it ([`informed_removals`]); otherwise it is *lost*. A file the spec's
/// last seal deleted counts as lost when the target still has it.
///
/// Not wired to `writ doctor` yet (0.4.1 survival tier).
pub fn audit(
    repo: &crate::Repository,
    spec_ids: &[String],
    against: Against,
) -> crate::error::WritResult<AuditReport> {
    use std::time::Instant;
    let started = Instant::now();

    // Every seal in the workspace, oldest first, for the superseded check.
    let mut all_seals = repo.log()?;
    all_seals.reverse();

    let text = |hash: &str| -> crate::error::WritResult<Option<String>> {
        let bytes = repo.object_content(hash)?;
        Ok((!crate::diff::is_binary(&bytes)).then(|| String::from_utf8_lossy(&bytes).into_owned()))
    };

    let mut specs = Vec::with_capacity(spec_ids.len());
    for spec_id in spec_ids {
        let mut seals = repo.spec_seals(spec_id)?;
        seals.reverse();
        let own = own_versions(&seals);
        let mut paths: Vec<&String> = own.keys().collect();
        paths.sort();

        let targets: HashMap<String, Vec<u8>> = match against {
            Against::WorkingTree => paths
                .iter()
                .filter_map(|p| {
                    std::fs::read(repo.root().join(p.as_str()))
                        .ok()
                        .map(|c| ((*p).clone(), c))
                })
                .collect(),
            Against::Head => {
                let owned: Vec<String> = paths.iter().map(|p| (*p).clone()).collect();
                repo.git_head_blobs(&owned)?
            }
        };

        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let v = &own[path];
            let seal_id = seals
                .iter()
                .rev()
                .find(|s| s.changes.iter().any(|c| &c.path == path))
                .map(|s| s.id.clone())
                .unwrap_or_default();
            let last_time = v.times.last().copied().unwrap_or_default();
            let mut file = FileSurvival {
                path: path.clone(),
                seal_id,
                status: FileAuditStatus::Checked,
                checked: 0,
                lost: Vec::new(),
                superseded: Vec::new(),
            };
            let Some(last_hash) = v.last_new.as_deref() else {
                file.status = if targets.contains_key(path) {
                    FileAuditStatus::DeletedButPresent
                } else {
                    FileAuditStatus::DeletedAsSealed
                };
                files.push(file);
                continue;
            };
            let version = match text(last_hash) {
                Ok(Some(t)) => t,
                Ok(None) => {
                    file.status = FileAuditStatus::Binary;
                    files.push(file);
                    continue;
                }
                Err(crate::error::WritError::ObjectNotFound(_)) => {
                    file.status = FileAuditStatus::Unreadable;
                    files.push(file);
                    continue;
                }
                Err(e) => return Err(e),
            };
            let base = match v.first_old.as_deref() {
                Some(h) => match text(h) {
                    Ok(Some(t)) => t,
                    Ok(None) => String::new(),
                    Err(crate::error::WritError::ObjectNotFound(_)) => String::new(),
                    Err(e) => return Err(e),
                },
                None => String::new(),
            };
            let target = targets
                .get(path)
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();

            let version_lines = split_lines(&version);
            let added: Vec<usize> = additions(&base, &version)
                .into_iter()
                .map(|(i, _)| i)
                .collect();
            file.checked = added.len();
            let missing = content_gone(added, &version_lines, &counts(target.lines()));
            if missing.is_empty() {
                files.push(file);
                continue;
            }

            // Later seals on this path, by any spec: an informed removal
            // (base held the line, result lacks it) supersedes the addition.
            let mut removers: Vec<(&Seal, HashMap<String, usize>)> = Vec::new();
            for seal in all_seals.iter().filter(|s| s.timestamp > last_time) {
                for change in seal.changes.iter().filter(|c| &c.path == path) {
                    let Some(old) = change.old_hash.as_deref() else {
                        continue;
                    };
                    let Ok(Some(old)) = text(old) else {
                        continue;
                    };
                    let new = match (&change.change_type, change.new_hash.as_deref()) {
                        (ChangeType::Deleted, _) | (_, None) => String::new(),
                        (_, Some(h)) => match text(h) {
                            Ok(Some(t)) => t,
                            _ => continue,
                        },
                    };
                    let removed: HashMap<String, usize> = informed_removals(&old, &new)
                        .into_iter()
                        .map(|(l, n)| (l.to_string(), n))
                        .collect();
                    if !removed.is_empty() {
                        removers.push((seal, removed));
                    }
                }
            }
            for i in missing {
                let line = version_lines[i];
                let by =
                    removers
                        .iter_mut()
                        .find_map(|(seal, removed)| match removed.get_mut(line) {
                            Some(n) if *n > 0 => {
                                *n -= 1;
                                Some(*seal)
                            }
                            _ => None,
                        });
                match by {
                    Some(seal) => file.superseded.push(SupersededLine {
                        line: i + 1,
                        text: line.to_string(),
                        by_seal: seal.id.clone(),
                        by_spec: seal.spec_id.clone(),
                    }),
                    None => file.lost.push(LostLine {
                        line: i + 1,
                        text: line.to_string(),
                    }),
                }
            }
            files.push(file);
        }

        let (checked, lost, superseded) = files.iter().fold((0, 0, 0), |(c, l, s), f| {
            (c + f.checked, l + f.lost.len(), s + f.superseded.len())
        });
        specs.push(SpecSurvival {
            spec_id: spec_id.clone(),
            files,
            checked,
            lost,
            superseded,
        });
    }

    let (checked, lost, superseded) = specs.iter().fold((0, 0, 0), |(c, l, s), sp| {
        (c + sp.checked, l + sp.lost, s + sp.superseded)
    });
    Ok(AuditReport {
        against,
        specs,
        checked,
        lost,
        superseded,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::{three_way_merge, FileMergeResult};

    // ── Finding 69 allowances ───────────────────────────────────────────

    #[test]
    fn unmark_strips_one_annotation_and_keeps_indent() {
        assert_eq!(
            unmark("    #[ignore = \"flaky\"] fn t() {}").as_deref(),
            Some("    fn t() {}")
        );
        assert_eq!(
            unmark("@pytest.mark.skip(reason=\"x)\") def t():").as_deref(),
            Some("def t():")
        );
        assert_eq!(unmark("@skip def t():").as_deref(), Some("def t():"));
        assert_eq!(
            unmark("    \"test_x\",  # expected ignored until S.2").as_deref(),
            Some("    \"test_x\",")
        );
        assert_eq!(unmark("let a = 1; // todo").as_deref(), Some("let a = 1;"));
        assert_eq!(unmark("x = 1 /* why */").as_deref(), Some("x = 1"));
        // A comment marker inside a string is not a comment.
        assert_eq!(
            unmark("s = \"a # b\"  # real").as_deref(),
            Some("s = \"a # b\"")
        );
        assert_eq!(unmark("url = \"http://x\""), None);
    }

    #[test]
    fn standalone_markers_have_no_unmarked_form() {
        assert_eq!(unmark("#[ignore = \"not landed\"]"), None);
        assert_eq!(unmark("    #[ignore]"), None);
        assert_eq!(unmark("@pytest.mark.skip(reason=\"x\")"), None);
        assert_eq!(unmark("# just a comment"), None);
        assert_eq!(unmark("plain code"), None);
        assert_eq!(unmark(""), None);
        assert_eq!(unmark("user@example.com"), None);
    }

    #[test]
    fn un_ignore_adding_the_unmarked_form_is_not_a_loss() {
        // (c): the spec's marked entry is dropped and its un-marked form is
        // added elsewhere in the same seal, beside new (foreign) content.
        let base = "a\nb\nc\n";
        let last = "a\n    \"t\",  # ignored\nb\nc\n";
        let new = "a\nb\nNEWTEST\nc\n    \"t\",\n";
        assert!(own_line_loss("s", "f", base, last, &[], new, &[]).is_none());
    }

    #[test]
    fn dropping_the_marked_line_without_its_unmarked_form_is_a_loss() {
        // The coordinator's negative case: the marker is gone, nothing
        // un-marked was added. A stale rewrite looks exactly like this.
        let base = "a\nb\nc\n";
        let last = "a\n    \"t\",  # ignored\nb\n#[ignore = \"x\"]\nc\n";
        let new = "a\nb\nc\nNEWTEST\n";
        let l = own_line_loss("s", "f", base, last, &[], new, &[]).unwrap();
        assert_eq!(
            l.lines,
            vec![
                "    \"t\",  # ignored".to_string(),
                "#[ignore = \"x\"]".to_string()
            ]
        );
        // The un-marked form must be *added*: present already in the spec's
        // own last version does not count.
        let last2 = "a\n    \"t\",  # ignored\n    \"t\",\nb\n";
        let new2 = "a\n    \"t\",\nb\nTHEIRS\n";
        assert!(own_line_loss("s", "f", base, last2, &[], new2, &[]).is_some());
    }

    #[test]
    fn standalone_marker_on_own_test_is_excused_when_the_test_stays() {
        // Option B: the spec added the test and its marker; dropping the
        // marker while the test stays, beside new content, is an un-ignore.
        let base = "a\nb\n";
        let last = "a\n#[test]\n#[ignore = \"x\"]\nfn t() {}\nb\n";
        let new = "a\n#[test]\nfn t() {}\nb\nTHEIRS\n";
        assert!(own_line_loss("s", "f", base, last, &[], new, &[]).is_none());
        let py_last = "a\n@pytest.mark.skip(reason=\"x\")\ndef test_t():\nb\n";
        let py_new = "a\ndef test_t():\nb\nTHEIRS\n";
        assert!(own_line_loss("s", "f", base, py_last, &[], py_new, &[]).is_none());
        assert!(is_standalone_annotation("    #[ignore]"));
        assert!(is_standalone_annotation("@skip"));
        assert!(!is_standalone_annotation("#[ignore] fn t() {}"));
        assert!(!is_standalone_annotation("user@example.com"));
    }

    #[test]
    fn standalone_marker_drop_is_refused_without_the_annotated_own_line() {
        // Stale rewrite from before the spec's seal: test and marker gone.
        let base = "a\nb\n";
        let last = "a\n#[ignore = \"x\"]\nfn t() {}\nb\n";
        let stale = "a\nb\nTHEIRS\n";
        let l = own_line_loss("s", "f", base, last, &[], stale, &[]).unwrap();
        assert_eq!(
            l.lines,
            vec!["#[ignore = \"x\"]".to_string(), "fn t() {}".to_string()]
        );
        // The marker was the spec's only work on a base line: refused.
        let base2 = "a\nfn t() {}\nb\n";
        let last2 = "a\n#[ignore = \"x\"]\nfn t() {}\nb\n";
        let new2 = "a\nfn t() {}\nb\nTHEIRS\n";
        let l = own_line_loss("s", "f", base2, last2, &[], new2, &[]).unwrap();
        assert_eq!(l.lines, vec!["#[ignore = \"x\"]".to_string()]);
        // A marker annotating nothing (end of file, blank line) is not excused.
        let last3 = "a\nfn t() {}\nb\n#[ignore]\n";
        let new3 = "a\nTHEIRS\nfn t() {}\nb\n";
        assert!(own_line_loss("s", "f", base2, last3, &[], new3, &[]).is_some());
    }

    #[test]
    fn cross_spec_removal_excuses_per_copy() {
        // (a): another spec's seal removed two of the three markers.
        let base = "a\nb\n";
        let last = "a\nM\nM\nM\nb\n";
        let new = "a\nb\nTHEIRS\n";
        let mut excused = HashMap::new();
        excused.insert("M".to_string(), 2);
        let l = own_line_loss_excused("s", "f", base, last, &[], new, &[], &excused).unwrap();
        assert_eq!(l.lines, vec!["M".to_string()]);
        excused.insert("M".to_string(), 3);
        assert!(own_line_loss_excused("s", "f", base, last, &[], new, &[], &excused).is_none());
    }

    #[test]
    fn moved_block_beside_foreign_content_is_not_a_loss() {
        // (b) with foreign content present, so the check gets past the
        // capture-shape gate and must rely on content survival.
        let base = "a\nb\nc\nd\n";
        let last = "a\nN1\nN2\nb\nc\nd\n";
        let moved = "a\nb\nc\nTHEIRS\nN1\nN2\nd\n";
        assert!(own_line_loss("s", "f", base, last, &[], moved, &[]).is_none());
        let partial = "a\nb\nc\nTHEIRS\nN1\nd\n";
        let l = own_line_loss("s", "f", base, last, &[], partial, &[]).unwrap();
        assert_eq!(l.lines, vec!["N2".to_string()]);
    }

    fn numbered(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    fn edit(text: &str, line: usize, suffix: &str) -> String {
        text.lines()
            .enumerate()
            .map(|(i, l)| {
                if i == line {
                    format!("{l} {suffix}\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect()
    }

    fn sides(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(s, c)| (s.to_string(), c.to_string()))
            .collect()
    }

    #[test]
    fn disjoint_merge_passes() {
        let base = numbered(40);
        let a = edit(&base, 2, "A");
        let b = edit(&base, 30, "B");
        let FileMergeResult::Clean(merged) = three_way_merge(&base, &a, &b) else {
            panic!("disjoint edits conflicted");
        };
        let s = sides(&[("sa", &a), ("sb", &b)]);
        assert!(merge_losses("f", &base, &s, &merged, &[]).is_empty());
    }

    #[test]
    fn overlapping_merge_reports_conflict_and_covered_range_is_accepted() {
        let base = numbered(10);
        let a = edit(&base, 4, "A");
        let b = edit(&base, 4, "B");
        let FileMergeResult::Conflict(regions) = three_way_merge(&base, &a, &b) else {
            panic!("same-line edits merged cleanly");
        };
        let covered = conflict_spans(&regions);
        // The merge kept the left side for the conflicted region.
        let s = sides(&[("sa", &a), ("sb", &b)]);
        assert!(merge_losses("f", &base, &s, &a, &covered).is_empty());
        // Without the reported conflict, sb's line is a loss.
        let losses = merge_losses("f", &base, &s, &a, &[]);
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].spec, "sb");
        assert_eq!(losses[0].ranges, vec![(5, 5)]);
        assert_eq!(losses[0].lines, vec!["line 4 B".to_string()]);
    }

    #[test]
    fn conflict_elsewhere_does_not_cover_a_dropped_hunk() {
        let base = numbered(40);
        let a = edit(&edit(&base, 4, "A"), 30, "A30");
        let b = edit(&base, 4, "B");
        let FileMergeResult::Conflict(regions) = three_way_merge(&base, &a, &b) else {
            panic!("expected a conflict at line 4");
        };
        // A resolver that picks b wholesale drops a's line 30 hunk too.
        let s = sides(&[("sa", &a), ("sb", &b)]);
        let losses = merge_losses("f", &base, &s, &b, &conflict_spans(&regions));
        assert_eq!(losses.len(), 1);
        assert_eq!(losses[0].spec, "sa");
        assert_eq!(losses[0].ranges, vec![(31, 31)]);
    }

    #[test]
    fn silently_dropped_hunk_is_a_loss() {
        let base = numbered(40);
        let a = edit(&base, 2, "A");
        let b = edit(&base, 30, "B");
        // The finding 42 output: b only.
        let s = sides(&[("sa", &a), ("sb", &b)]);
        let losses = merge_losses("shared.txt", &base, &s, &b, &[]);
        assert_eq!(losses.len(), 1);
        let l = &losses[0];
        assert_eq!(
            (l.kind, l.spec.as_str(), l.path.as_str()),
            (LossKind::Merge, "sa", "shared.txt")
        );
        assert_eq!(l.ranges, vec![(3, 3)]);
        let esc = l.to_escalation("merged");
        assert_eq!(esc.conflict_class, MERGE_LOSS_CLASS);
        assert_eq!(esc.right_spec, "sa");
        assert!(
            esc.file_path == "shared.txt" && esc.reason.starts_with("line 3"),
            "{}",
            esc.reason
        );
    }

    #[test]
    fn moved_addition_counts_as_lost_only_when_order_breaks() {
        let base = "a\nb\nc\n";
        let side = "a\nNEW\nb\nc\n";
        // Same lines, addition kept in place: fine.
        assert!(lost_additions(base, side, "a\nNEW\nb\nc\nX\n", &[]).is_empty());
        // Addition missing entirely.
        assert_eq!(lost_additions(base, side, "a\nb\nc\n", &[]), vec![1]);
    }

    #[test]
    fn ranges_group_consecutive_lines() {
        assert_eq!(to_ranges(&[2, 3, 4, 8]), vec![(3, 5), (9, 9)]);
        assert!(to_ranges(&[]).is_empty());
    }

    #[test]
    fn empty_inputs_do_not_panic() {
        assert!(lost_additions("", "", "", &[]).is_empty());
        assert_eq!(lost_additions("", "x\n", "", &[]), vec![0]);
        assert!(lost_additions("", " ", "0", &[BaseSpan::whole_file()]).is_empty());
    }

    #[test]
    fn own_line_revert_to_base_is_a_loss() {
        // Finding 42 variant B: a's final seal captures b's version from disk.
        let base = numbered(40);
        let a = edit(&base, 2, "A");
        let b = edit(&base, 30, "B");
        let l =
            own_line_loss("sa", "shared.txt", &base, &a, &[], &b, &[]).expect("revert not flagged");
        assert_eq!(l.kind, LossKind::OwnLines);
        assert_eq!(l.ranges, vec![(3, 3)]);
        assert_eq!(l.lines, vec!["line 2 A".to_string()]);
        assert!(l.to_string().contains("shared.txt: line 3"), "{l}");
    }

    #[test]
    fn own_line_pure_removal_without_foreign_content_passes() {
        // The spec removes its own debug line: editing its own work.
        let base = "a\nb\n";
        let last = "a\ndebug\nb\n";
        assert!(own_line_loss("s", "f", base, last, &[], base, &[]).is_none());
        // Lines from an earlier own version are not foreign either.
        let earlier = "a\nold\nb\n";
        let back = "a\nold\nb\n";
        assert!(own_line_loss("s", "f", base, last, &[earlier], back, &[]).is_none());
    }

    #[test]
    fn own_line_pure_removal_with_foreign_content_is_a_loss() {
        // Same removal, but the file also holds a line no version of this
        // spec ever sealed: another agent's version was captured.
        let base = "a\nb\n";
        let last = "a\ndebug\nb\n";
        let captured = "a\nb\nTHEIRS\n";
        let l = own_line_loss("s", "f", base, last, &[last], captured, &[]).unwrap();
        assert_eq!(l.lines, vec!["debug".to_string()]);
        assert_eq!(l.ranges, vec![(2, 2)]);
        assert_eq!(l.foreign, vec!["THEIRS".to_string()]);
    }

    #[test]
    fn own_line_removal_beside_lines_another_agent_sealed_passes() {
        // Finding 56: "THEIRS" is foreign to this spec but already sealed by
        // another agent (it is in the last sealed version of the file), so
        // removing the spec's own line next to it is an intentional edit.
        let base = "a\nb\n";
        let last = "a\ndebug\nb\n";
        let sealed_by_other = "a\ndebug\nb\nTHEIRS\n";
        let new = "a\nb\nTHEIRS\n";
        assert!(own_line_loss("s", "f", base, last, &[last, sealed_by_other], new, &[]).is_none());
    }

    #[test]
    fn own_line_edit_with_new_text_is_not_a_loss() {
        let base = numbered(10);
        let a = edit(&base, 2, "A");
        let a2 = edit(&base, 2, "AA");
        assert!(own_line_loss("sa", "f", &base, &a, &[], &a2, &[]).is_none());
    }

    #[test]
    fn own_line_keeping_own_lines_is_not_a_loss() {
        let base = numbered(40);
        let a = edit(&base, 2, "A");
        let both = edit(&a, 30, "B");
        assert!(own_line_loss("sa", "f", &base, &a, &[], &both, &[]).is_none());
        // Removing a base line the spec never added is not this check's concern.
        let removed_base: String = a
            .lines()
            .filter(|l| *l != "line 9")
            .map(|l| format!("{l}\n"))
            .collect();
        assert!(own_line_loss("sa", "f", &base, &a, &[], &removed_base, &[]).is_none());
    }

    #[test]
    fn own_line_emptying_own_file_passes() {
        assert!(own_line_loss("s", "f", "", "x\ny\n", &[], "", &[]).is_none());
    }

    // ── Finding 53: content survives anywhere, duplicates counted ──────

    fn block_file(order: &[&str]) -> String {
        order.iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn own_moved_block_is_not_a_loss() {
        let base = block_file(&["a", "b", "c", "d", "e", "f"]);
        let last = block_file(&["a", "N1", "N2", "N3", "b", "c", "d", "e", "f"]);
        let moved = block_file(&["a", "b", "c", "d", "e", "N1", "N2", "N3", "f"]);
        assert!(own_line_loss("s", "f", &base, &last, &[], &moved, &[]).is_none());
    }

    #[test]
    fn own_block_moved_to_another_sealed_file_is_not_a_loss() {
        let base = block_file(&["a", "b", "c"]);
        let last = block_file(&["a", "N1", "N2", "b", "c"]);
        let other = block_file(&["x", "N1", "N2"]);
        assert!(own_line_loss("s", "f", &base, &last, &[], &base, &[&other]).is_none());
        // Moved only partly, with foreign content present: the missing line
        // is still a loss.
        let partial = block_file(&["x", "N1"]);
        let captured = block_file(&["a", "b", "c", "THEIRS"]);
        let l = own_line_loss("s", "f", &base, &last, &[], &captured, &[&partial]).unwrap();
        assert_eq!(l.lines, vec!["N2".to_string()]);
    }

    #[test]
    fn own_duplicate_lines_count_each_copy() {
        let base = block_file(&["a", "b"]);
        let last = block_file(&["a", "dup", "b", "dup"]);
        // One copy remains: one of the two added copies is gone.
        let one = block_file(&["THEIRS", "a", "dup", "b"]);
        let l = own_line_loss("s", "f", &base, &last, &[], &one, &[]).unwrap();
        assert_eq!(l.lines, vec!["dup".to_string()]);
        // Both copies remain, one moved.
        let both = block_file(&["dup", "dup", "a", "b"]);
        assert!(own_line_loss("s", "f", &base, &last, &[], &both, &[]).is_none());
    }

    #[test]
    fn merge_moved_block_is_not_a_loss() {
        let base = block_file(&["a", "b", "c", "d", "e"]);
        let side = block_file(&["a", "N1", "N2", "b", "c", "d", "e"]);
        let merged = block_file(&["a", "b", "c", "d", "N1", "N2", "e", "X"]);
        assert!(lost_additions(&base, &side, &merged, &[]).is_empty());
        let s = sides(&[("sa", &side)]);
        assert!(merge_losses("f", &base, &s, &merged, &[]).is_empty());
    }

    #[test]
    fn merge_duplicate_addition_needs_every_copy() {
        let base = block_file(&["a", "b"]);
        let side = block_file(&["a", "dup", "b", "dup"]);
        let merged = block_file(&["a", "dup", "b"]);
        assert_eq!(lost_additions(&base, &side, &merged, &[]), vec![3]);
    }

    fn excused(pairs: &[(&str, &str, &str)]) -> HashMap<String, HashMap<String, usize>> {
        // (side spec, remover's old, remover's new)
        let mut out: HashMap<String, HashMap<String, usize>> = HashMap::new();
        for (spec, old, new) in pairs {
            let e = out.entry(spec.to_string()).or_default();
            for (l, n) in informed_removals(old, new) {
                *e.entry(l.to_string()).or_insert(0) += n;
            }
        }
        out
    }

    #[test]
    fn informed_removal_counts_each_copy() {
        // The remover kept one "None" and removed the other.
        let r = informed_removals("a\nNone\nb\nNone\n", "a\nNone\nb\n");
        assert_eq!(r.get("None"), Some(&1));
        // sa added two "None"; one informed removal excuses one copy only.
        let s = sides(&[("sa", "x\nNone\nNone\n")]);
        let e = excused(&[("sa", "None\nNone\n", "None\n")]);
        let l = merge_losses_informed("f", "x\n", &s, &e, "x\n", &[]);
        assert_eq!(l[0].lines, vec!["None".to_string()]);
    }

    #[test]
    fn informed_rewrite_of_another_specs_line_is_not_a_loss() {
        // sa adds "v1"; sc's seal recorded a base holding "v1" and wrote "v2".
        let s = sides(&[("sa", "v1\n"), ("sc", "v2\n")]);
        let e = excused(&[("sa", "v1\n", "v2\n")]);
        assert!(merge_losses_informed("f", "", &s, &e, "v2\n", &[]).is_empty());
        // sc's own addition is still protected.
        let l = merge_losses_informed("f", "", &s, &e, "v1\n", &[]);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].spec, "sc");
    }

    #[test]
    fn concurrent_edits_from_a_shared_base_are_losses() {
        // No seal's recorded base held the other's line: nothing excused.
        let base = "x\n";
        let s = sides(&[("sa", "x\nA\n"), ("sb", "x\nB\n")]);
        let l = merge_losses_informed("f", base, &s, &HashMap::new(), "x\nB\n", &[]);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].spec, "sa");
    }

    #[test]
    fn contains_edits_detects_descent() {
        let base = "a\nb\nc\n";
        let x = "a\nX\nb\n"; // adds X, removes c
        assert!(contains_edits(base, x, x));
        assert!(contains_edits(base, x, "a\nX\nb\nY\n"));
        assert!(!contains_edits(base, x, "a\nb\nc\n"));
        assert!(!contains_edits(base, x, "a\nX\nb\nc\n")); // c not removed
    }

    // ── Finding 62 dual invariant: every later deletion survives ───────

    fn with_markers(base: &str) -> String {
        let mut out = String::new();
        for (i, l) in base.lines().enumerate() {
            if i % 4 == 0 && i < 20 {
                out.push_str("#[ignore = \"not landed\"]\n");
            }
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    #[test]
    fn descendant_removal_must_not_come_back() {
        let base = numbered(20);
        let a = with_markers(&base);
        assert_eq!(a.matches("#[ignore").count(), 5);
        let b = base.clone(); // B continued A and removed all five markers
        let descents = vec![("sb".to_string(), a.clone(), b.clone())];
        // sa is superseded by sb, so only sb took part in the merge.
        let s = sides(&[("sb", &b)]);
        // The fixed merge takes the descendant: nothing comes back.
        assert!(resurrections("f", &base, &s, &descents, &b).is_empty());
        // A merge from the original base brings all five back: escalated.
        let FileMergeResult::Clean(old_merge) = three_way_merge(&base, &a, &b) else {
            panic!("expected a clean three-way merge");
        };
        assert_eq!(old_merge.matches("#[ignore").count(), 5);
        let r = resurrections("f", &base, &s, &descents, &old_merge);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].kind, LossKind::Resurrected);
        assert_eq!(r[0].spec, "sb");
        assert_eq!(r[0].lines.len(), 5);
        let esc = r[0].to_escalation("seal-tree merge");
        assert_eq!(esc.conflict_class, MERGE_RESURRECTION_CLASS);
        assert!(
            esc.reason.contains("removed by spec 'sb'"),
            "{}",
            esc.reason
        );
    }

    #[test]
    fn concurrent_spec_keeps_the_markers() {
        // B never saw A's markers: no descent, the merge keeps all five.
        let base = numbered(20);
        let a = with_markers(&base);
        let b = edit(&base, 18, "B");
        let FileMergeResult::Clean(merged) = three_way_merge(&base, &a, &b) else {
            panic!("expected a clean three-way merge");
        };
        assert_eq!(merged.matches("#[ignore").count(), 5);
        let s = sides(&[("sa", &a), ("sb", &b)]);
        assert!(resurrections("f", &base, &s, &[], &merged).is_empty());
        assert!(merge_losses("f", &base, &s, &merged, &[]).is_empty());
    }

    #[test]
    fn same_text_added_by_another_side_is_allowed() {
        let base = "a\n";
        let ancestor = "a\nX\n";
        let descendant = "a\n";
        let other = "a\nX\n"; // a third spec added X on its own
        let s = sides(&[("sb", descendant), ("sc", other)]);
        let d = vec![(
            "sb".to_string(),
            ancestor.to_string(),
            descendant.to_string(),
        )];
        assert!(resurrections("f", base, &s, &d, "a\nX\n").is_empty());
        assert_eq!(resurrections("f", base, &s, &d, "a\nX\nX\n").len(), 1);
    }

    #[test]
    fn anchor_is_the_latest_seal_the_later_version_saw() {
        let base = numbered(12);
        let a1 = edit(&edit(&base, 2, "L1"), 4, "L2"); // seal 1 adds L1, L2
        let a2 = edit(&a1, 8, "L3"); // seal 2 adds L3
        let v = [a1.as_str(), a2.as_str()];
        // Copy taken between the seals, L1's line removed on purpose.
        let b_copy = edit(&base, 4, "L2");
        assert_eq!(anchor_seal(&base, &base, &v, &b_copy), Some(0));
        // Saw both.
        assert_eq!(anchor_seal(&base, &base, &v, &edit(&a2, 10, "B")), Some(1));
        // Saw neither: stale whole-file rewrite.
        assert_eq!(anchor_seal(&base, &base, &v, &edit(&base, 10, "B")), None);
        // Unseen lines of the later seal.
        assert_eq!(additions_missing_from(&a1, &a2, &b_copy), vec![8]);
    }

    #[test]
    fn notice_names_file_lines_agent_and_command() {
        let n = ConvergenceNotice::new("shared.txt", ("sb", "b"), ("sa", "a"), "x\nL3\n", &[1]);
        assert!(
            n.message
                .contains("shared.txt: line(s) 2 added by spec 'sa' (agent a)"),
            "{}",
            n.message
        );
        assert!(
            n.command.contains("--spec sb --paths shared.txt"),
            "{}",
            n.command
        );
    }

    #[test]
    fn blank_or_repeated_lines_are_not_evidence_of_a_copy() {
        let base = "fn a() {\n}\n";
        let a1 = "fn helper() {\n}\n\nfn a() {\n}\n"; // adds helper, "}", ""
        let b = "fn a() {\n}\n\n// b\n"; // has "" and "}" but not the helper
        assert_eq!(anchor_seal(base, base, &[a1], b), None);
        // Restoring base text over another spec's edit is not evidence either.
        let a2 = "NEW\n}\n";
        assert_eq!(anchor_seal(base, a2, &["fn a() {\nX\n}\n"], base), None);
    }
}
