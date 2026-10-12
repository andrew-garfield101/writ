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
///
/// Finding 115 (the 105 rule, [`saw_addition_in`] clause 2, anchored): a
/// seal that replaced lines was also seen when `later` already carries an
/// earlier anchored seal of the same spec and lacks every non-blank line
/// this seal replaced, each of them that spec's own content (in the
/// anchored version, not in the base). This is the sprint 4 shape: a
/// descendant that deleted the block a seal had relabelled carries none of
/// the relabel's lines, and lacks the lines it replaced. Without an anchor,
/// or when a replaced line is base text, absence is no evidence: a stale
/// rewrite of the same lines lacks them too, and still conflicts.
pub fn anchor_seal(base: &str, first_old: &str, versions: &[&str], later: &str) -> Option<usize> {
    let have = counts(later.lines());
    let in_base = counts(base.lines());
    let mut anchor: Option<usize> = None;
    let mut prev = first_old;
    for (k, v) in versions.iter().enumerate() {
        let adds = additions(prev, v);
        let ancestor = anchor.map(|a| counts(versions[a].lines()));
        // A replaced line is evidence only when it was the other spec's own
        // content that `later` demonstrably carried: in the anchored seal's
        // version and not in the base. A base line (or one `later` never
        // had) says nothing by being absent: a stale rewrite of the same
        // base line lacks it too (Bri's 115 negative).
        let held = |l: &str| {
            !in_base.contains_key(l) && ancestor.as_ref().is_some_and(|c| c.contains_key(l))
        };
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
            let kept_one = adds.iter().any(|(_, l)| {
                !l.trim().is_empty()
                    && !before.contains_key(l)
                    && !in_base.contains_key(l)
                    && have.contains_key(l)
            });
            kept_one || {
                let kept = counts(v.lines());
                let replaced: Vec<&str> = informed_removals(prev, v)
                    .into_keys()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                !replaced.is_empty()
                    && replaced.iter().all(|l| {
                        held(l)
                            && have.get(l).copied().unwrap_or(0)
                                <= kept.get(l).copied().unwrap_or(0)
                    })
            }
        };
        if seen {
            anchor = Some(k);
        }
        prev = v;
    }
    anchor
}

/// One own seal of a file, precomputed for [`saw_addition`]: the
/// distinctive lines it added, the lines it removed (non-blank), and the
/// line counts of what it sealed.
#[derive(Debug, Clone)]
pub struct SealStep<'a> {
    pub adds: HashSet<&'a str>,
    pub removed: Vec<&'a str>,
    pub kept: HashMap<&'a str, usize>,
    /// Every other distinctive line of the sealed version (not blank, not
    /// in the path's genesis, not added by this step): what else had
    /// entered the file by then.
    pub others: Vec<&'a str>,
}

impl<'a> SealStep<'a> {
    /// `steps`: the spec's own seals of the file as `(its previous own
    /// version, what it sealed)`, oldest first. `genesis`: the path's
    /// earliest known version, for [`Self::others`] (`None`: `in_base`).
    pub fn all(
        steps: &[(&'a str, &'a str)],
        in_base: &HashSet<&str>,
        genesis: Option<&HashSet<&str>>,
    ) -> Vec<Self> {
        let genesis = genesis.unwrap_or(in_base);
        steps
            .iter()
            .map(|(prev, new)| {
                let before = counts(prev.lines());
                let adds: HashSet<&'a str> = additions(prev, new)
                    .into_iter()
                    .map(|(_, l)| l)
                    .filter(|l| {
                        !l.trim().is_empty() && !before.contains_key(l) && !in_base.contains(l)
                    })
                    .collect();
                let mut others: Vec<&'a str> = new
                    .lines()
                    .filter(|l| !l.trim().is_empty() && !genesis.contains(l) && !adds.contains(l))
                    .collect();
                others.sort_unstable();
                others.dedup();
                Self {
                    adds,
                    others,
                    removed: informed_removals(prev, new)
                        .into_keys()
                        .filter(|l| !l.trim().is_empty())
                        .collect(),
                    kept: counts(new.lines()),
                }
            })
            .collect()
    }
}

/// Finding 105: did `version` (another spec's sealed version) see the own
/// seal that added `line`? The anchor rule per seal: `steps` are the
/// spec's own seals of the file as `(its previous own version, what it
/// sealed)`, oldest first. A step that added `line` was seen when
/// `version` keeps another distinctive line that step added, or lacks
/// every line that step removed (it replaced them, and they are gone).
/// Third (CC, finding 69 shape): `version` keeps every other distinctive
/// line of that step's sealed version; a stale copy from before the seal
/// would lack whatever else had entered the file, a targeted removal keeps
/// it. A step that only inserted lines into a file with no other
/// distinctive line, none kept, is undecidable: not seen, which keeps its
/// work. `true` when no step added `line` (not ours).
pub fn saw_addition(
    version: &str,
    line: &str,
    steps: &[(&str, &str)],
    in_base: &HashSet<&str>,
) -> bool {
    saw_addition_in(
        &counts(version.lines()),
        line,
        &SealStep::all(steps, in_base, None),
    )
}

/// [`saw_addition`] with the path's earliest known version (`genesis`) for
/// the third clause, so lines that entered the file after genesis but
/// before the spec's first seal count as evidence.
pub fn saw_addition_since(
    version: &str,
    line: &str,
    steps: &[(&str, &str)],
    in_base: &HashSet<&str>,
    genesis: &HashSet<&str>,
) -> bool {
    saw_addition_in(
        &counts(version.lines()),
        line,
        &SealStep::all(steps, in_base, Some(genesis)),
    )
}

/// [`saw_addition`] over precomputed steps and the version's line counts.
pub fn saw_addition_in(have: &HashMap<&str, usize>, line: &str, steps: &[SealStep<'_>]) -> bool {
    let mut ours = false;
    for step in steps.iter().filter(|s| s.adds.contains(line)) {
        ours = true;
        if step.adds.iter().any(|l| *l != line && have.contains_key(l)) {
            return true;
        }
        if !step.removed.is_empty()
            && step.removed.iter().all(|l| {
                have.get(l).copied().unwrap_or(0) <= step.kept.get(l).copied().unwrap_or(0)
            })
        {
            return true;
        }
        if !step.others.is_empty() && step.others.iter().all(|l| have.contains_key(l)) {
            return true;
        }
    }
    !ours
}

/// Lines of `old` that `new` replaced in place with new text: each hunk
/// of the edit that deletes lines and inserts at least one line for which
/// `is_new_text` holds (never seen in the file before). A remover that
/// rewrote a line in place saw it; a stale copy puts old text, or
/// nothing, where the line was (finding 105).
pub fn replaced_in_place<'a>(
    old: &'a str,
    new: &str,
    is_new_text: impl Fn(&str) -> bool,
) -> HashSet<&'a str> {
    let (o, n) = (split_lines(old), split_lines(new));
    let mut out = HashSet::new();
    for h in hunks(&o, &n) {
        if !h.deleted.is_empty() && h.inserted.iter().any(|&j| is_new_text(n[j])) {
            out.extend(h.deleted.iter().map(|&i| o[i]));
        }
    }
    out
}

/// Did a seal of another spec that recorded `old -> new` see the own seal
/// that added `line`? The seal-time allowance's single entry point (finding
/// 105): [`saw_addition_since`] (all three clauses, with `genesis` the
/// path's earliest known version), or it replaced the line in place with
/// text new to the file (not in `old`, not in `in_base`).
pub fn remover_saw(
    old: &str,
    new: &str,
    line: &str,
    steps: &[(&str, &str)],
    in_base: &HashSet<&str>,
    genesis: &HashSet<&str>,
) -> bool {
    let old_lines: HashSet<&str> = old.lines().collect();
    saw_addition_since(new, line, steps, in_base, genesis)
        || replaced_in_place(old, new, |l| {
            !l.trim().is_empty() && !old_lines.contains(l) && !in_base.contains(l)
        })
        .contains(line)
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
            "{path}: line(s) {label} added by spec '{added_by_spec}' (agent {added_by_agent}) were not in the version of spec '{spec}' and were kept: {}{}. If '{spec}' meant to remove them, remove them again and seal: {command}",
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

// ── Survival audit (the survival tier of `writ doctor`) ───────────────

/// What the sealed additions are checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Against {
    /// The files on disk.
    WorkingTree,
    /// The files at git HEAD (needs the `bridge` feature and a repository).
    /// Specs not yet committed are listed in `not_committed`, not audited.
    Head,
    /// Per spec: committed specs against HEAD, the rest against the
    /// working tree (the survival tier on demand and on watch).
    Auto,
    /// The tree `writ finish` will commit: the staged sealed content where
    /// it stages a path, HEAD elsewhere ([`audit_finish`]).
    FinishTree,
}

/// A sealed addition the target lacks with no evidence it was meant to go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LostLine {
    /// 1-based line in the version of the file that held it: the spec's
    /// last sealed version, or for a line its own overridden seal removed,
    /// the version before that seal.
    pub line: usize,
    pub text: String,
    /// The spec's seal that added the line.
    pub added_by: String,
    /// The spec's own seal that removed the line under `--allow-removals`
    /// or `--force` (finding 42 variant B), if that is how it went.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<String>,
}

/// Why a missing addition is not a loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupersededBy {
    /// A later seal removed it while its recorded base held it
    /// ([`informed_removals`]).
    InformedRemoval,
    /// A later seal of a spec since committed to git recorded the file
    /// without it (finding 77: a finish accepted that version).
    CommittedSpec,
    /// The spec's own seal removed it under `--allow-removals` naming the
    /// path, or `--force`: refused first, then overridden on the record
    /// (CC decision C, sprint 4).
    OwnOverride,
}

/// A spec's own line its own overridden seal removed, where that seal
/// recorded another spec's sealed content in its place: the override
/// captured another spec's version under this spec's name (the finding 42
/// variant B signature). A yellow finding, never a block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedLine {
    pub line: usize,
    pub text: String,
    /// The spec's seal that added the line.
    pub added_by: String,
    /// The spec's own overridden seal that removed it.
    pub removed_by: String,
    /// The other spec, and its seal, whose sealed content the override
    /// recorded.
    pub other_spec: String,
    pub other_seal: String,
}

/// A sealed addition the target lacks because a later seal replaced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupersededLine {
    pub line: usize,
    pub text: String,
    pub by_seal: String,
    pub by_spec: Option<String>,
    pub reason: SupersededBy,
}

/// Why a file's additions were not line-checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAuditStatus {
    /// Every distinctive addition was checked.
    Checked,
    /// The spec's last seal deleted the file, and the target lacks it too.
    DeletedAsSealed,
    /// The spec's last seal deleted the file, but the target still has it
    /// and no later seal wrote it again.
    DeletedButPresent,
    /// The spec's last seal deleted the file and a later seal wrote it
    /// again: superseded.
    RecreatedLater,
    /// The sealed content is binary.
    Binary,
    /// The sealed blob is missing from the store.
    Unreadable,
    /// Against HEAD: the target lacks the file because git ignores its
    /// path, so no finish can commit it. Reported, not a merge loss.
    IgnoredByGit,
    /// Like [`Self::IgnoredByGit`], and writ's own ignore rules cover the
    /// path too (finding 111): an info note, never a finding.
    IgnoredByGitAndWrit,
    /// A later seal (any spec) deleted the whole file and the target lacks
    /// it: an explicit act, superseding the spec's additions.
    DeletedLater,
}

/// Survival of one spec's additions to one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSurvival {
    pub path: String,
    /// The spec's last seal of the path.
    pub seal_id: String,
    pub status: FileAuditStatus,
    /// Distinctive added lines checked.
    pub checked: usize,
    /// Added lines skipped as non-distinctive (blank, or text the file
    /// already had before the spec): they say nothing about survival.
    #[serde(default)]
    pub skipped: usize,
    pub lost: Vec<LostLine>,
    pub superseded: Vec<SupersededLine>,
    /// Own lines an own override replaced with another spec's content.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captured: Vec<CapturedLine>,
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
    #[serde(default)]
    pub skipped: usize,
    /// Against HEAD: specs not audited because they are not committed
    /// (their work is not meant to be in HEAD yet).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_committed: Vec<String>,
    pub elapsed_ms: u64,
}

impl AuditReport {
    /// No sealed addition is missing without evidence it was replaced.
    pub fn is_green(&self) -> bool {
        self.lost == 0 && self.deleted_but_present().next().is_none()
    }

    /// `(spec, file)` pairs whose sealed deletion did not hold.
    pub fn deleted_but_present(&self) -> impl Iterator<Item = (&str, &FileSurvival)> {
        self.specs.iter().flat_map(|s| {
            s.files
                .iter()
                .filter(|f| f.status == FileAuditStatus::DeletedButPresent)
                .map(move |f| (s.spec_id.as_str(), f))
        })
    }

    /// Every lost line: `(spec, file, line)`.
    pub fn lost_lines(&self) -> impl Iterator<Item = (&str, &str, &LostLine)> {
        self.specs.iter().flat_map(|s| {
            s.files.iter().flat_map(move |f| {
                f.lost
                    .iter()
                    .map(move |l| (s.spec_id.as_str(), f.path.as_str(), l))
            })
        })
    }
}

/// One seal's change to one path, texts loaded.
struct PathEvent<'a> {
    seal: &'a Seal,
    old: Option<std::rc::Rc<str>>,
    /// Empty for a deletion.
    new: std::rc::Rc<str>,
    deleted: bool,
}

/// Per-path data derived from its events, computed on first use and
/// shared by every spec that sealed the path (survival tier latency).
struct PathShared<'e> {
    events: &'e [PathEvent<'e>],
    /// Each event's sealed version, as line counts.
    counts: std::cell::OnceCell<Vec<HashMap<&'e str, usize>>>,
    /// Each event's removed lines per copy ([`informed_removals`] of its
    /// recorded base and version); `None` without a recorded base.
    removals: std::cell::OnceCell<Vec<Option<HashMap<&'e str, usize>>>>,
}

impl<'e> PathShared<'e> {
    fn new(events: &'e [PathEvent<'e>]) -> Self {
        Self {
            events,
            counts: std::cell::OnceCell::new(),
            removals: std::cell::OnceCell::new(),
        }
    }

    fn counts(&self) -> &Vec<HashMap<&'e str, usize>> {
        self.counts
            .get_or_init(|| self.events.iter().map(|e| counts(e.new.lines())).collect())
    }

    fn removals(&self) -> &Vec<Option<HashMap<&'e str, usize>>> {
        self.removals.get_or_init(|| {
            self.events
                .iter()
                .map(|e| {
                    e.old
                        .as_deref()
                        .map(|old| informed_removals(old, &e.new))
                        .filter(|r| !r.is_empty())
                })
                .collect()
        })
    }
}

/// Blob text by hash, loaded once per audit. `None`: binary.
struct Texts<'r> {
    repo: &'r crate::Repository,
    cache: HashMap<String, Option<std::rc::Rc<str>>>,
}

impl Texts<'_> {
    fn get(&mut self, hash: &str) -> crate::error::WritResult<Option<std::rc::Rc<str>>> {
        if let Some(t) = self.cache.get(hash) {
            return Ok(t.clone());
        }
        let bytes = self.repo.object_content(hash)?;
        let t = (!crate::diff::is_binary(&bytes))
            .then(|| std::rc::Rc::from(String::from_utf8_lossy(&bytes).as_ref()));
        self.cache.insert(hash.to_string(), t.clone());
        Ok(t)
    }

    /// Like [`Self::get`], with a missing blob read as `None`.
    fn get_lenient(&mut self, hash: &str) -> crate::error::WritResult<Option<std::rc::Rc<str>>> {
        match self.get(hash) {
            Err(crate::error::WritError::ObjectNotFound(_)) => Ok(None),
            r => r,
        }
    }
}

/// Specs to audit since `since` (the last green result): those with a seal
/// after it or committed after it. `None`: every spec.
pub fn specs_since(
    repo: &crate::Repository,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> crate::error::WritResult<Vec<String>> {
    let mut ids = Vec::new();
    for spec in repo.list_specs()? {
        let Some(since) = since else {
            ids.push(spec.id);
            continue;
        };
        let committed_after = spec.committed_at.is_some_and(|t| t > since);
        let sealed_after = spec.sealed_by.iter().rev().any(|id| {
            repo.load_seal(id)
                .map(|s| s.timestamp > since)
                .unwrap_or(false)
        });
        if committed_after || sealed_after {
            ids.push(spec.id);
        }
    }
    ids.sort();
    Ok(ids)
}

/// Added-line survival of `spec_ids` (finding 77 semantics).
///
/// For each spec and each path its seals recorded, the candidates are the
/// spec's **distinctive** additions (the [`anchor_seal`] rule: not blank,
/// and not text the file had before the spec's first seal of it):
///
/// 1. the lines its last sealed version added relative to that base, and
/// 2. lines one of its seals added and a later seal of its own removed
///    under `--allow-removals` or `--force` (finding 42 variant B: the
///    override is the only record that the removal was unchecked).
///
/// A candidate the target lacks (by content, per copy; a moved block
/// survives, finding 53) is **superseded** when a later seal removed it
/// while its recorded base held it (not counting the spec's own seals that
/// overrode the own-line check for the path, `--allow-removals`/`--force`)
/// ([`informed_removals`]), or when a later seal of
/// a spec since committed to git recorded the file without it. Otherwise
/// it is **lost**, named with the seal that added it. A file the spec's
/// last seal deleted is lost when the target still has it and no later
/// seal wrote it again.
pub fn audit(
    repo: &crate::Repository,
    spec_ids: &[String],
    against: Against,
) -> crate::error::WritResult<AuditReport> {
    audit_with(repo, spec_ids, against, None)
}

/// [`audit`] against the tree `writ finish` will commit: `staged` maps each
/// path finish stages to its sealed content (`None`: a staged deletion);
/// every other path is read at HEAD.
pub fn audit_finish(
    repo: &crate::Repository,
    spec_ids: &[String],
    staged: &HashMap<String, Option<Vec<u8>>>,
) -> crate::error::WritResult<AuditReport> {
    audit_with(repo, spec_ids, Against::FinishTree, Some(staged))
}

fn audit_with(
    repo: &crate::Repository,
    spec_ids: &[String],
    against: Against,
    staged: Option<&HashMap<String, Option<Vec<u8>>>>,
) -> crate::error::WritResult<AuditReport> {
    use std::time::Instant;
    let started = Instant::now();

    // Every seal in the store (global chain and every spec chain), oldest
    // first: under seal isolation most seals are only on spec chains.
    let mut all_seals = repo.log_all()?;
    all_seals.sort_by_key(|s| s.timestamp);
    let mut by_path: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (si, seal) in all_seals.iter().enumerate() {
        for (ci, change) in seal.changes.iter().enumerate() {
            by_path
                .entry(change.path.as_str())
                .or_default()
                .push((si, ci));
        }
    }
    let committed: HashSet<String> = repo
        .list_specs()?
        .into_iter()
        .filter(|s| s.commit_state != crate::spec::CommitState::Uncommitted)
        .map(|s| s.id)
        .collect();
    let mut not_committed = Vec::new();
    let writ_ignore = crate::ignore::IgnoreRules::load(repo.root());
    let mut texts = Texts {
        repo,
        cache: HashMap::new(),
    };

    // Phase 1, per spec: its seals, own versions, paths and targets.
    struct Prep {
        spec_rec: crate::spec::Spec,
        spec_id: String,
        seals: Vec<Seal>,
        own: HashMap<String, OwnVersion>,
        paths: Vec<String>,
        targets: HashMap<String, Vec<u8>>,
        git_ignored: HashSet<String>,
        at_head: bool,
    }
    let mut preps: Vec<Prep> = Vec::with_capacity(spec_ids.len());
    for spec_id in spec_ids {
        let spec_rec = repo.resolve_spec(spec_id)?;
        let resolved = spec_rec.id.clone();
        if against == Against::Head && !committed.contains(&resolved) {
            not_committed.push(spec_id.clone());
            continue;
        }
        let mut seals = repo.spec_seals(spec_id)?;
        seals.reverse();
        let own = own_versions(&seals);
        let mut paths: Vec<String> = own.keys().cloned().collect();
        paths.sort();

        let mut git_ignored = HashSet::new();
        let at_head = match against {
            Against::Head | Against::FinishTree => true,
            Against::Auto => committed.contains(&resolved),
            Against::WorkingTree => false,
        };
        let targets: HashMap<String, Vec<u8>> = match at_head {
            false => {
                // Finding 123: an open spec's sealed path that git and writ
                // both ignore (and HEAD does not hold) is named now, while
                // its holder can still fix it, whether or not it is on disk.
                let both: Vec<String> = paths
                    .iter()
                    .filter(|p| writ_ignore.is_path_ignored(p))
                    .cloned()
                    .collect();
                if !both.is_empty() {
                    if let Ok(at) = repo.git_head_blobs(&both) {
                        git_ignored = repo
                            .git_ignored(&both)
                            .into_iter()
                            .filter(|p| !at.contains_key(p))
                            .collect();
                    }
                }
                paths
                    .iter()
                    .filter_map(|p| {
                        std::fs::read(repo.root().join(p.as_str()))
                            .ok()
                            .map(|c| (p.clone(), c))
                    })
                    .collect()
            }
            true => {
                let owned: Vec<String> = paths
                    .iter()
                    .filter(|p| !staged.is_some_and(|m| m.contains_key(*p)))
                    .cloned()
                    .collect();
                let mut blobs = repo.git_head_blobs(&owned)?;
                let absent: Vec<String> = owned
                    .into_iter()
                    .filter(|p| !blobs.contains_key(p))
                    .collect();
                git_ignored = repo.git_ignored(&absent);
                for (p, content) in staged.into_iter().flatten() {
                    if let Some(c) = content {
                        if own.contains_key(p) {
                            blobs.insert(p.clone(), c.clone());
                        }
                    }
                }
                blobs
            }
        };
        preps.push(Prep {
            spec_rec,
            spec_id: spec_id.clone(),
            seals,
            own,
            paths,
            targets,
            git_ignored,
            at_head,
        });
    }

    // Phase 2, per path (shared by every spec that sealed it): its events
    // in store order. Built once per path, not once per spec and path.
    let mut path_events: HashMap<&str, Vec<PathEvent<'_>>> = HashMap::new();
    for prep in &preps {
        for path in &prep.paths {
            if path_events.contains_key(path.as_str()) {
                continue;
            }
            let mut events = Vec::new();
            for &(si, ci) in by_path.get(path.as_str()).map(Vec::as_slice).unwrap_or(&[]) {
                let seal = &all_seals[si];
                let change = &seal.changes[ci];
                let deleted =
                    change.change_type == ChangeType::Deleted || change.new_hash.is_none();
                let new = match change.new_hash.as_deref() {
                    Some(h) if !deleted => match texts.get_lenient(h)? {
                        Some(t) => t,
                        None => continue,
                    },
                    _ => std::rc::Rc::from(""),
                };
                let old = match change.old_hash.as_deref() {
                    Some(h) => texts.get_lenient(h)?,
                    None => None,
                };
                events.push(PathEvent {
                    seal,
                    old,
                    new,
                    deleted,
                });
            }
            path_events.insert(path.as_str(), events);
        }
    }
    // Per path, derived from its events on first use and shared by every
    // spec: each event's line counts and the lines it removed.
    let shared: HashMap<&str, PathShared<'_>> = path_events
        .iter()
        .map(|(p, ev)| (*p, PathShared::new(ev)))
        .collect();

    // Phase 3, per spec and path: the audit.
    let mut histories: HashMap<&str, HashMap<String, chrono::DateTime<chrono::Utc>>> =
        HashMap::new();
    let mut specs = Vec::with_capacity(preps.len());
    for prep in &preps {
        let Prep {
            spec_rec,
            spec_id,
            seals,
            own,
            paths,
            targets,
            git_ignored,
            at_head,
        } = prep;
        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let events = &path_events[path.as_str()];
            let target = targets
                .get(path)
                .map(|b| String::from_utf8_lossy(b).into_owned());
            let own_last = own[path].times.last().copied().unwrap_or_default();
            let deleted_later = target.is_none()
                && events
                    .last()
                    .is_some_and(|e| e.deleted && e.seal.timestamp > own_last);
            // Finding 116: when the spec's own last seal of an ignored path
            // recorded its deletion, that seal retired it: the path is
            // checked as a sealed deletion, never noted as sealed-but-ignored.
            let retired = own[path].last_new.is_none();
            if deleted_later
                || ((target.is_none() || !*at_head)
                    && !retired
                    && git_ignored.contains(path.as_str()))
            {
                let status = if deleted_later {
                    FileAuditStatus::DeletedLater
                } else if writ_ignore.is_path_ignored(path) {
                    FileAuditStatus::IgnoredByGitAndWrit
                } else {
                    FileAuditStatus::IgnoredByGit
                };
                files.push(FileSurvival {
                    path: path.clone(),
                    seal_id: seals
                        .iter()
                        .rev()
                        .find(|s| s.changes.iter().any(|c| &c.path == path))
                        .map(|s| s.id.clone())
                        .unwrap_or_default(),
                    status,
                    checked: 0,
                    skipped: 0,
                    lost: Vec::new(),
                    superseded: Vec::new(),
                    captured: Vec::new(),
                });
                continue;
            }
            let history = histories
                .entry(path.as_str())
                .or_insert_with(|| line_history(events));
            files.push(audit_file(
                spec_rec,
                spec_id,
                path,
                seals,
                &own[path],
                events,
                &shared[path.as_str()],
                history,
                target.as_deref(),
                &committed,
                &mut texts,
            )?);
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

    let (checked, lost, superseded, skipped) =
        specs.iter().fold((0, 0, 0, 0), |(c, l, s, k), sp| {
            let sk: usize = sp.files.iter().map(|f| f.skipped).sum();
            (c + sp.checked, l + sp.lost, s + sp.superseded, k + sk)
        });
    Ok(AuditReport {
        against,
        specs,
        checked,
        lost,
        superseded,
        skipped,
        not_committed,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// When each line first appeared in any sealed version of a path (in a
/// seal's recorded base or its result), from all its events. Built once
/// per path per audit.
fn line_history(events: &[PathEvent<'_>]) -> HashMap<String, chrono::DateTime<chrono::Utc>> {
    let mut out: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut seen_texts: HashSet<*const u8> = HashSet::new();
    for e in events {
        let t = e.seal.timestamp;
        for text in [e.old.as_deref(), Some(&*e.new)].into_iter().flatten() {
            // Consecutive events share blobs; read each text once.
            if !seen_texts.insert(text.as_ptr()) {
                continue;
            }
            for l in text.lines() {
                match out.get_mut(l) {
                    Some(first) if *first <= t => {}
                    Some(first) => *first = t,
                    None => {
                        out.insert(l.to_string(), t);
                    }
                }
            }
        }
    }
    out
}

/// A candidate addition: where it was, what it says, who added it, and
/// after when a removal of it counts.
struct Candidate {
    line: usize,
    text: String,
    added_by: String,
    removed_by: Option<String>,
    after: chrono::DateTime<chrono::Utc>,
}

/// Is `seal` an override of the own-line check for `path`?
fn overridden(seal: &Seal, path: &str) -> bool {
    seal.forced || seal.allow_removals.iter().any(|p| p == path)
}

#[allow(clippy::too_many_arguments)]
fn audit_file(
    spec_rec: &crate::spec::Spec,
    spec_id: &str,
    path: &str,
    own_seals: &[Seal],
    v: &OwnVersion,
    events: &[PathEvent<'_>],
    shared: &PathShared<'_>,
    history: &HashMap<String, chrono::DateTime<chrono::Utc>>,
    target: Option<&str>,
    committed: &HashSet<String>,
    texts: &mut Texts<'_>,
) -> crate::error::WritResult<FileSurvival> {
    let own_events: Vec<&PathEvent<'_>> = events
        .iter()
        .filter(|e| e.seal.spec_id.as_deref() == Some(spec_id))
        .collect();
    let last_seal = own_seals
        .iter()
        .rev()
        .find(|s| s.changes.iter().any(|c| c.path == path));
    let last_time = v.times.last().copied().unwrap_or_default();
    let mut file = FileSurvival {
        path: path.to_string(),
        seal_id: last_seal.map(|s| s.id.clone()).unwrap_or_default(),
        status: FileAuditStatus::Checked,
        checked: 0,
        skipped: 0,
        lost: Vec::new(),
        superseded: Vec::new(),
        captured: Vec::new(),
    };

    let Some(last_hash) = v.last_new.as_deref() else {
        let recreated = events
            .iter()
            .any(|e| e.seal.timestamp > last_time && !e.deleted);
        file.status = match (target.is_some(), recreated) {
            (false, _) => FileAuditStatus::DeletedAsSealed,
            (true, true) => FileAuditStatus::RecreatedLater,
            (true, false) => FileAuditStatus::DeletedButPresent,
        };
        return Ok(file);
    };
    let version = match texts.get(last_hash) {
        Ok(Some(t)) => t,
        Ok(None) => {
            file.status = FileAuditStatus::Binary;
            return Ok(file);
        }
        Err(crate::error::WritError::ObjectNotFound(_)) => {
            file.status = FileAuditStatus::Unreadable;
            return Ok(file);
        }
        Err(e) => return Err(e),
    };
    // Finding 106: a path whose history starts in git (no writ version
    // before the spec's first seal) takes its base from the spec's genesis
    // snapshot or git, not the empty file.
    let git_base = match v.first_old {
        Some(_) => None,
        None => texts
            .repo
            .baseline_text(spec_rec, path, v.times.first().copied())?
            .map(|t| std::rc::Rc::<str>::from(t.as_str())),
    };
    let base = match v.first_old.as_deref() {
        Some(h) => texts
            .get_lenient(h)?
            .unwrap_or_else(|| std::rc::Rc::from("")),
        None => git_base.clone().unwrap_or_else(|| std::rc::Rc::from("")),
    };
    // Non-distinctive: blank, or text the file held before the spec first
    // sealed it, in its base or in any earlier sealed version (the anchor
    // rule: a stale rewrite that restores old text says nothing).
    // Only the spec's own lines are ever asked about, so only those are
    // looked up in the path's history (first time each line appeared).
    let first_own = own_events.first().map(|e| e.seal.timestamp);
    let mut in_base: HashSet<&str> = base.lines().collect();
    if let Some(t) = first_own {
        for e in &own_events {
            in_base.extend(
                e.new
                    .lines()
                    .filter(|l| history.get(*l).is_some_and(|first| *first < t)),
            );
        }
    }
    let distinctive = |l: &str| !l.trim().is_empty() && !in_base.contains(l);

    // The spec's own seals of the file as (previous own version, sealed).
    let steps: Vec<(&str, &str)> = own_events
        .iter()
        .enumerate()
        .map(|(k, e)| {
            let prev: &str = if k == 0 {
                &base
            } else {
                &own_events[k - 1].new
            };
            (prev, &*e.new)
        })
        .collect();
    // Finding 105: a foreign seal that recorded removing `l` but never saw
    // the own seal that added it (its version is a stale copy).
    // The path's earliest known version: the first event's recorded base,
    // or what it sealed when the file was new to writ.
    let genesis: HashSet<&str> = match events.first() {
        Some(e) => match (e.old.as_deref(), git_base.as_deref()) {
            (Some(old), _) => old.lines().collect(),
            (None, Some(g)) if own_events.first().is_some_and(|o| std::ptr::eq(*o, e)) => {
                g.lines().collect()
            }
            (None, _) => e.new.lines().collect(),
        },
        None => HashSet::new(),
    };
    let steps = SealStep::all(&steps, &in_base, Some(&genesis));
    // Which own seal added each distinctive text (the latest that did).
    let mut adder: HashMap<&str, &str> = HashMap::new();
    for (step, e) in steps.iter().zip(&own_events) {
        for l in &step.adds {
            adder.insert(l, e.seal.id.as_str());
        }
    }
    let last_id = file.seal_id.clone();

    let target_text = target.unwrap_or("");
    let have = counts(target_text.lines());
    let version_lines = split_lines(&version);
    let mut candidates: Vec<Candidate> = Vec::new();

    // 1. Additions in the last sealed version.
    let added: Vec<usize> = additions(&base, &version)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    let (kept, skipped): (Vec<usize>, Vec<usize>) = added
        .into_iter()
        .partition(|&i| distinctive(version_lines[i]));
    file.skipped += skipped.len();
    file.checked += kept.len();
    for i in content_gone(kept, &version_lines, &have) {
        let text = version_lines[i];
        candidates.push(Candidate {
            line: i + 1,
            text: text.to_string(),
            added_by: adder.get(text).map_or(last_id.clone(), |s| s.to_string()),
            removed_by: None,
            after: last_time,
        });
    }

    // 2. Own additions that an own overridden seal removed (CC decision
    // C): intentional, the agent was refused and overrode on the record,
    // unless the override recorded another spec's sealed content in their
    // place (captured: yellow).
    // Line counts of each event's sealed version, computed once, on use.
    let event_counts = || shared.counts();
    // Per event, the lines it replaced in place with text new to the file
    // (finding 105), computed on first use.
    let in_place: Vec<std::cell::OnceCell<HashSet<&str>>> =
        events.iter().map(|_| std::cell::OnceCell::new()).collect();
    let rewrote = |i: usize, l: &str| -> bool {
        let e = &events[i];
        let Some(old) = e.old.as_deref() else {
            return false;
        };
        in_place[i]
            .get_or_init(|| {
                let old_lines: HashSet<&str> = old.lines().collect();
                replaced_in_place(old, &e.new, |x| {
                    !x.trim().is_empty()
                        && !old_lines.contains(x)
                        && !in_base.contains(x)
                        && !genesis.contains(x)
                        && history
                            .get(x)
                            .is_none_or(|first| *first >= e.seal.timestamp)
                })
            })
            .contains(l)
    };
    let unseen_removal =
        |l: &str, after: chrono::DateTime<chrono::Utc>, until: chrono::DateTime<chrono::Utc>| {
            events
                .iter()
                .zip(event_counts())
                .enumerate()
                .filter(|(_, (o, _))| o.seal.spec_id.as_deref() != Some(spec_id))
                .filter(|(_, (o, _))| o.seal.timestamp > after && o.seal.timestamp <= until)
                .find(|(i, (o, have))| {
                    !have.contains_key(l)
                        && o.old
                            .as_deref()
                            .is_some_and(|old| old.lines().any(|x| x == l))
                        && !saw_addition_in(have, l, &steps)
                        && !rewrote(*i, l)
                })
                .map(|(_, (o, _))| o.seal)
        };
    // Another spec's sealed additions to the file, built on first use.
    let mut foreign: Option<HashMap<&str, &Seal>> = None;
    let mut earlier: HashSet<&str> = HashSet::new();
    for (k, e) in own_events.iter().enumerate() {
        let is_override = overridden(e.seal, path);
        // Against the spec's own previous version of the file (the recorded
        // base is the shared index, which may be another spec's version).
        let old: &str = match k {
            0 => &base,
            _ => &own_events[k - 1].new,
        };
        // Own lines added by an earlier own seal that this seal removed.
        let mut gone: Vec<&str> = steps[k]
            .removed
            .iter()
            .copied()
            .filter(|l| earlier.contains(l))
            .collect();
        earlier.extend(steps[k].adds.iter().copied());
        if gone.is_empty() {
            continue;
        }
        gone.sort_unstable();
        // The capture signature (overrides only): the file this seal
        // recorded is another spec's sealed version, or its new lines are
        // another spec's sealed additions.
        let captured_from = if !is_override {
            None
        } else {
            let foreign = foreign.get_or_insert_with(|| {
                events
                    .iter()
                    .filter(|e| e.seal.spec_id.as_deref().is_some_and(|s| s != spec_id))
                    .flat_map(|e| {
                        additions(e.old.as_deref().unwrap_or(""), &e.new)
                            .into_iter()
                            .map(move |(_, l)| (l, e.seal))
                    })
                    .filter(|(l, _)| distinctive(l))
                    .collect()
            });
            events
                .iter()
                .find(|o| {
                    o.seal.spec_id.as_deref().is_some_and(|s| s != spec_id)
                        && !o.deleted
                        && *o.new == *e.new
                })
                .map(|o| o.seal)
                .or_else(|| steps[k].adds.iter().find_map(|l| foreign.get(l).copied()))
        };
        let old_lines: Vec<&str> = old.lines().collect();
        for l in gone {
            if have.contains_key(l) || version_lines.contains(&l) {
                if is_override {
                    file.checked += 1;
                }
                continue;
            }
            let line = old_lines.iter().position(|x| *x == l).map_or(0, |i| i + 1);
            if !is_override {
                // Own iteration, unless the removal carried over a foreign
                // seal's stale copy (finding 105: the backstop, whatever
                // the seal-time check did).
                let added_at = own_events[..k]
                    .iter()
                    .rev()
                    .find(|p| p.new.lines().any(|x| x == l))
                    .map(|p| p.seal.timestamp)
                    .unwrap_or_default();
                if unseen_removal(l, added_at, e.seal.timestamp).is_some() {
                    file.checked += 1;
                    file.lost.push(LostLine {
                        line,
                        text: l.to_string(),
                        added_by: adder.get(l).map_or(String::new(), |s| s.to_string()),
                        removed_by: None,
                    });
                }
                continue;
            }
            file.checked += 1;
            match captured_from {
                Some(other) => file.captured.push(CapturedLine {
                    line,
                    text: l.to_string(),
                    added_by: adder.get(l).map_or(String::new(), |s| s.to_string()),
                    removed_by: e.seal.id.clone(),
                    other_spec: other.spec_id.clone().unwrap_or_default(),
                    other_seal: other.id.clone(),
                }),
                None => file.superseded.push(SupersededLine {
                    line,
                    text: l.to_string(),
                    by_seal: e.seal.id.clone(),
                    by_spec: e.seal.spec_id.clone(),
                    reason: SupersededBy::OwnOverride,
                }),
            }
        }
    }
    if candidates.is_empty() {
        return Ok(file);
    }

    // Later removals by any seal: informed (base held the line), per copy.
    // (event index, seal, its line counts, lines it removed per copy)
    type Remover<'s, 'l> = (
        usize,
        &'s Seal,
        &'l HashMap<&'l str, usize>,
        HashMap<&'l str, usize>,
    );
    let mut removers: Vec<Remover> = events
        .iter()
        .zip(event_counts())
        .enumerate()
        .zip(shared.removals())
        .filter_map(|((i, (e, have)), r)| Some((i, e.seal, have, r.clone()?)))
        .collect();
    for c in candidates {
        let informed = removers.iter_mut().find_map(|(i, seal, new, removed)| {
            if seal.timestamp <= c.after {
                return None;
            }
            // Finding 105: another spec's removal is informed only if its
            // version saw the seal that added the line, or it rewrote the
            // line in place.
            if seal.spec_id.as_deref() != Some(spec_id)
                && !saw_addition_in(new, &c.text, &steps)
                && !rewrote(*i, &c.text)
            {
                return None;
            }
            match removed.get_mut(c.text.as_str()) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    Some(*seal)
                }
                _ => None,
            }
        });
        let by = informed
            .map(|s| (s, SupersededBy::InformedRemoval))
            .or_else(|| {
                events
                    .iter()
                    .filter(|e| e.seal.timestamp > c.after)
                    .filter(|e| {
                        e.seal
                            .spec_id
                            .as_deref()
                            .is_some_and(|s| s != spec_id && committed.contains(s))
                    })
                    .find(|e| !e.new.lines().any(|x| x == c.text))
                    .map(|e| (e.seal, SupersededBy::CommittedSpec))
            });
        match by {
            Some((seal, reason)) => file.superseded.push(SupersededLine {
                line: c.line,
                text: c.text,
                by_seal: seal.id.clone(),
                by_spec: seal.spec_id.clone(),
                reason,
            }),
            None => file.lost.push(LostLine {
                line: c.line,
                text: c.text,
                added_by: c.added_by,
                removed_by: c.removed_by,
            }),
        }
    }
    Ok(file)
}

// ── Survival tier (`writ doctor --survival`, before finish, on watch) ──

/// Cache of the last survival tier run, `.writ/survival_cache.json`.
pub const SURVIVAL_CACHE_FILE: &str = "survival_cache.json";

/// Doctor check id of the survival tier.
pub const CHECK_SURVIVAL: &str = "survival";

/// The last survival tier run and the last green one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurvivalCache {
    /// writ version that wrote it; another version's cache is ignored.
    pub version: String,
    /// The state the run saw: seal set, spec records, audited files' stat.
    pub key: String,
    /// Paths the run audited (their stat is part of `key`).
    pub paths: Vec<String>,
    /// Newest seal when the run happened.
    pub newest_seal: Option<String>,
    /// No lost line and no sealed deletion undone.
    pub green: bool,
    /// The run's findings (red: lost lines; yellow: files git ignores).
    pub findings: Vec<crate::doctor::DoctorFinding>,
    /// Lost lines in the run.
    pub lost: usize,
    /// Info notes (finding 111): sealed paths both git and writ ignore.
    /// Never findings; never affect green or "safe to finish".
    #[serde(default)]
    pub info: Vec<String>,
    /// Newest seal at the last green run, and that seal's time (the bound
    /// of the next run).
    pub last_green: Option<String>,
    pub last_green_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Wall time of the run and how many specs it audited.
    pub elapsed_ms: u64,
    pub specs_audited: usize,
}

impl SurvivalCache {
    fn path(repo: &crate::Repository) -> std::path::PathBuf {
        repo.writ_dir().join(SURVIVAL_CACHE_FILE)
    }

    /// The cache on disk, if this writ version wrote it.
    pub fn load(repo: &crate::Repository) -> Option<Self> {
        let bytes = std::fs::read(Self::path(repo)).ok()?;
        let c: Self = serde_json::from_slice(&bytes).ok()?;
        (c.version == env!("CARGO_PKG_VERSION")).then_some(c)
    }

    fn save(&self, repo: &crate::Repository) {
        if let Ok(bytes) = serde_json::to_vec(self) {
            let _ = crate::fsutil::atomic_write(&Self::path(repo), &bytes);
        }
    }

    /// True when the state the run saw is the current state.
    pub fn is_current(&self, repo: &crate::Repository) -> bool {
        state_key(repo, &self.paths).is_some_and(|k| k == self.key)
    }
}

/// What the fast tier says about survival, from the cache alone (no audit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurvivalState {
    /// The tier never ran here (or this writ version never ran it).
    NotRun,
    /// The last run is green and nothing changed since. `notes`: yellow
    /// findings (sealed files git ignores).
    Green { seal: String, notes: usize },
    /// The last run found losses and nothing changed since.
    Red { lost: usize, seal: Option<String> },
    /// Something changed since the last run.
    Stale { last_green: Option<String> },
}

impl SurvivalState {
    /// From the cache, re-checking its key against the current state.
    pub fn current(repo: &crate::Repository) -> Self {
        let Some(c) = SurvivalCache::load(repo) else {
            return Self::NotRun;
        };
        if !c.is_current(repo) {
            return Self::Stale {
                last_green: c.last_green,
            };
        }
        let notes = c.findings.len();
        match (c.green, c.newest_seal) {
            (true, Some(seal)) => Self::Green { seal, notes },
            (true, None) => Self::NotRun,
            (false, seal) => Self::Red { lost: c.lost, seal },
        }
    }

    /// The headline suffix after the fast-tier part.
    pub fn note(&self) -> String {
        const RUN: &str = "run `writ doctor --survival`";
        match self {
            Self::NotRun => format!("survival not run; {RUN}"),
            Self::Green { seal, notes: 0 } => format!("survival green as of {}", short(seal)),
            Self::Green { seal, notes } => format!(
                "survival green as of {} with {notes} note(s); run `writ doctor --survival`",
                short(seal)
            ),
            Self::Red { lost: 0, seal } => format!(
                "survival red: the merge finish would commit has an unresolved conflict{}; {RUN}",
                seal.as_deref()
                    .map(|s| format!(" as of {}", short(s)))
                    .unwrap_or_default()
            ),
            Self::Red { lost, seal } => format!(
                "survival red: {lost} sealed line(s) lost{}; {RUN}",
                seal.as_deref()
                    .map(|s| format!(" as of {}", short(s)))
                    .unwrap_or_default()
            ),
            Self::Stale {
                last_green: Some(s),
            } => format!(
                "survival last green as of {}, store changed since; {RUN}",
                short(s)
            ),
            Self::Stale { last_green: None } => {
                format!("survival not green since the store changed; {RUN}")
            }
        }
    }

    /// `survival_last_green` for the report: the seal of the last green
    /// run, whatever happened since.
    pub fn last_green(repo: &crate::Repository) -> Option<String> {
        SurvivalCache::load(repo).and_then(|c| c.last_green)
    }

    pub fn is_green(&self) -> bool {
        matches!(self, Self::Green { .. })
    }

    /// Green with no finding at all: the survival half of "safe to finish".
    pub fn is_safe(&self) -> bool {
        matches!(self, Self::Green { notes: 0, .. })
    }
}

fn short(id: &str) -> &str {
    &id[..12.min(id.len())]
}

/// The state a survival result depends on: the seal set, the spec records
/// and the audited files (stat only). `None` when the store cannot be read.
pub fn state_key(repo: &crate::Repository, paths: &[String]) -> Option<String> {
    let mut h = blake3::Hasher::new();
    let mut seal_ids: Vec<String> = std::fs::read_dir(repo.writ_dir().join("seals"))
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    seal_ids.sort();
    for id in &seal_ids {
        h.update(id.as_bytes());
        h.update(b"\n");
    }
    let mut specs: Vec<(String, u64, i128)> = std::fs::read_dir(repo.writ_dir().join("specs"))
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((
                e.file_name().to_string_lossy().into_owned(),
                m.len(),
                mtime(&m),
            ))
        })
        .collect();
    specs.sort();
    for (name, len, t) in &specs {
        h.update(format!("s {name} {len} {t}\n").as_bytes());
    }
    for a in accepted_removals(repo) {
        h.update(format!("a {a}\n").as_bytes());
    }
    for p in paths {
        match std::fs::metadata(repo.root().join(p)) {
            Ok(m) => h.update(format!("f {p} {} {}\n", m.len(), mtime(&m)).as_bytes()),
            Err(_) => h.update(format!("f {p} absent\n").as_bytes()),
        };
    }
    Some(h.finalize().to_hex().to_string())
}

fn mtime(m: &std::fs::Metadata) -> i128 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i128)
}

/// Result of one survival tier run.
#[derive(Debug, Clone)]
pub struct TierRun {
    pub cache: SurvivalCache,
    /// Served from the cache (nothing changed since the last run).
    pub cached: bool,
    pub report: Option<AuditReport>,
}

/// Run the survival tier: every spec not yet committed (against the
/// working tree) and every spec sealed or committed since the last green
/// result (committed ones against HEAD). Served from the cache when the
/// state it saw has not changed.
pub fn run_tier(repo: &crate::Repository) -> crate::error::WritResult<TierRun> {
    let prev = SurvivalCache::load(repo);
    if let Some(c) = prev.as_ref().filter(|c| c.is_current(repo)) {
        return Ok(TierRun {
            cache: c.clone(),
            cached: true,
            report: None,
        });
    }
    let started = std::time::Instant::now();
    let since = prev.as_ref().and_then(|c| c.last_green_at);
    let specs = repo.list_specs()?;
    let mut ids: Vec<String> = specs
        .iter()
        .filter(|s| s.commit_state == crate::spec::CommitState::Uncommitted)
        .filter(|s| !s.sealed_by.is_empty())
        .map(|s| s.id.clone())
        .collect();
    for id in specs_since(repo, since)? {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();
    let against = if cfg!(feature = "bridge") && repo.git_head_blobs(&[]).is_ok() {
        Against::Auto
    } else {
        Against::WorkingTree
    };
    // Committable specs are judged on the tree finish would commit, so the
    // tier and finish never disagree (finding 103); the rest per `against`.
    let committable: Vec<String> = specs
        .iter()
        .filter(|s| s.is_committable() && !s.sealed_by.is_empty() && ids.contains(&s.id))
        .map(|s| s.id.clone())
        .collect();
    let rest: Vec<String> = ids
        .iter()
        .filter(|id| !committable.contains(id))
        .cloned()
        .collect();
    let mut report = audit(repo, &rest, against)?;
    let mut escalations = Vec::new();
    if !committable.is_empty() {
        let (tree, conv) = finish_tree(repo, &committable)?;
        let mut fin = audit_finish(repo, &committable, &tree)?;
        // A file the merge escalates has no result finish would commit
        // (finish refuses at the merge): the escalation is the verdict.
        let escalated: HashSet<&str> = conv
            .escalations
            .iter()
            .map(|e| e.file_path.as_str())
            .collect();
        for spec in &mut fin.specs {
            spec.files.retain(|f| !escalated.contains(f.path.as_str()));
            spec.checked = spec.files.iter().map(|f| f.checked).sum();
            spec.lost = spec.files.iter().map(|f| f.lost.len()).sum();
            spec.superseded = spec.files.iter().map(|f| f.superseded.len()).sum();
        }
        report.checked += fin.specs.iter().map(|s| s.checked).sum::<usize>();
        report.lost += fin.specs.iter().map(|s| s.lost).sum::<usize>();
        report.superseded += fin.specs.iter().map(|s| s.superseded).sum::<usize>();
        report.skipped += fin.skipped;
        report.specs.extend(fin.specs);
        report.specs.sort_by(|a, b| a.spec_id.cmp(&b.spec_id));
        escalations = conv.escalations;
    }
    let newest = repo.log_all()?.into_iter().max_by_key(|s| s.timestamp);
    let mut paths: Vec<String> = report
        .specs
        .iter()
        .flat_map(|s| s.files.iter().map(|f| f.path.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    // Escalations first: finish refuses on them before any line check.
    let mut findings = escalation_findings(repo, &escalations);
    findings.extend(self::findings(repo, &report));
    let green = report.is_green() && escalations.is_empty();
    let (last_green, last_green_at) = if green {
        (
            newest.as_ref().map(|s| s.id.clone()),
            newest.as_ref().map(|s| s.timestamp),
        )
    } else {
        (
            prev.as_ref().and_then(|c| c.last_green.clone()),
            prev.as_ref().and_then(|c| c.last_green_at),
        )
    };
    let info = info_notes(&report);
    let cache = SurvivalCache {
        info,
        version: env!("CARGO_PKG_VERSION").to_string(),
        key: state_key(repo, &paths).unwrap_or_default(),
        paths,
        newest_seal: newest.map(|s| s.id),
        green,
        findings,
        // Finding 108: the count the headline shows is the lines the red
        // findings name: the audit's plus each merge loss's.
        lost: report.lost
            + escalations
                .iter()
                .filter(|e| e.conflict_class == MERGE_LOSS_CLASS)
                .map(|e| escalation_lines(e).len().max(1))
                .sum::<usize>(),
        last_green,
        last_green_at,
        elapsed_ms: started.elapsed().as_millis() as u64,
        specs_audited: ids.len(),
    };
    cache.save(repo);
    Ok(TierRun {
        cache,
        cached: false,
        report: Some(report),
    })
}

/// Finding 111: one info line per sealed path that git and writ both
/// ignore (deduplicated across specs), naming the two exits.
pub fn info_notes(report: &AuditReport) -> Vec<String> {
    let mut by_path: std::collections::BTreeMap<&str, Vec<(&str, &str)>> = Default::default();
    for spec in &report.specs {
        for f in spec
            .files
            .iter()
            .filter(|f| f.status == FileAuditStatus::IgnoredByGitAndWrit)
        {
            by_path
                .entry(f.path.as_str())
                .or_default()
                .push((spec.spec_id.as_str(), f.seal_id.as_str()));
        }
    }
    by_path
        .into_iter()
        .map(|(path, sealed)| {
            let by: Vec<String> = sealed
                .iter()
                .map(|(spec, seal)| format!("spec '{spec}' seal {}", short(seal)))
                .collect();
            format!(
                "{path} was sealed ({}) but git and writ both ignore it now, so no finish can commit it. Either remove the ignore rule and seal the file (or its deletion), or leave it: the seal stays as history. To retire it without editing the ignore rules, delete the file and run `writ seal --paths {path}`",
                by.join(", ")
            )
        })
        .collect()
}

/// Path -> content finish would commit (`None`: a staged deletion).
pub type FinishTree = HashMap<String, Option<Vec<u8>>>;

/// The tree `writ finish` would commit for `spec_ids` (findings 103, 104):
/// each staged path's sealed content from the finish plan, overlaid with
/// the seal-tree convergence result finish materializes, as
/// `path -> content` (`None`: a staged deletion). Also the convergence
/// report: finish refuses on any escalation. Writes nothing (dry run).
pub fn finish_tree(
    repo: &crate::Repository,
    spec_ids: &[String],
) -> crate::error::WritResult<(FinishTree, crate::repo::SealTreeConvergenceReport)> {
    // Finding 110: a diagnostic never writes to the store; the dry run
    // merges the same way and stores nothing.
    let dry = repo.finalize_convergence_dry_run()?;
    let plan = repo.finish_plan(spec_ids)?;
    let mut tree = HashMap::new();
    for (path, hash) in &plan.stage {
        let content = match hash {
            Some(h) => Some(repo.object_content(h)?),
            None => None,
        };
        tree.insert(path.clone(), content);
    }
    for (path, hash) in &dry.report.shadow_results {
        if tree.contains_key(path) {
            tree.insert(path.clone(), Some(dry.content(repo, hash)?));
        }
    }
    Ok((tree, dry.report))
}

/// Finding 104: the convergence notices whose lines the merged result
/// actually holds. A notice says the lines "were kept"; when the content
/// finish commits lacks them (no merge result, or one without them), the
/// claim is false and the notice is dropped.
pub fn notices_that_hold<'r>(
    repo: &crate::Repository,
    report: &'r crate::repo::SealTreeConvergenceReport,
) -> Vec<&'r ConvergenceNotice> {
    report
        .notices
        .iter()
        .filter(|n| {
            let Some((_, hash)) = report.shadow_results.iter().find(|(p, _)| *p == n.path) else {
                return false;
            };
            let Ok(bytes) = repo.object_content(hash) else {
                return false;
            };
            let merged = String::from_utf8_lossy(&bytes);
            let have = counts(merged.lines());
            n.lines.iter().all(|l| have.contains_key(l.as_str()))
        })
        .collect()
}

/// Up to this many lost lines are quoted per finding.
pub const MAX_LINES_SHOWN: usize = 5;

/// One line naming a lost line and the seal that added it.
pub fn describe_lost(path: &str, l: &LostLine) -> String {
    let mut s = format!(
        "{path}:{} {:?} (added by seal {}",
        l.line,
        l.text,
        short(&l.added_by)
    );
    if let Some(r) = &l.removed_by {
        s.push_str(&format!(
            ", removed by its own seal {}, which overrode the removal check",
            short(r)
        ));
    }
    s.push(')');
    s
}

/// Doctor findings for an audit: one red per (spec, file) with lost lines
/// or an undone sealed deletion; one yellow per sealed file git ignores.
pub fn findings(
    repo: &crate::Repository,
    report: &AuditReport,
) -> Vec<crate::doctor::DoctorFinding> {
    use crate::doctor::{DoctorFinding, Severity};
    let accepted = accepted_removals(repo);
    let mut out = Vec::new();
    for spec in &report.specs {
        for f in &spec.files {
            if !f.lost.is_empty() {
                let mut shown: Vec<String> = f
                    .lost
                    .iter()
                    .take(MAX_LINES_SHOWN)
                    .map(|l| describe_lost(&f.path, l))
                    .collect();
                if f.lost.len() > MAX_LINES_SHOWN {
                    shown.push(format!("and {} more", f.lost.len() - MAX_LINES_SHOWN));
                }
                out.push(DoctorFinding::new(
                    CHECK_SURVIVAL,
                    Severity::Red,
                    format!(
                        "{} sealed line(s) of spec '{}' are missing and no seal removed them: {}. Restore them (`writ show <seal> --diff` shows the original) and seal, or decide they go",
                        f.lost.len(),
                        spec.spec_id,
                        shown.join("; ")
                    ),
                    String::new(),
                    vec![f.path.clone()],
                ));
            }
            let mut captured: Vec<&CapturedLine> = f
                .captured
                .iter()
                .filter(|c| {
                    !accepted
                        .iter()
                        .any(|a| c.removed_by.starts_with(a.as_str()))
                })
                .collect();
            captured.sort_by(|a, b| a.removed_by.cmp(&b.removed_by));
            let mut i = 0;
            while i < captured.len() {
                let seal = captured[i].removed_by.clone();
                let group: Vec<&CapturedLine> = captured[i..]
                    .iter()
                    .take_while(|c| c.removed_by == seal)
                    .copied()
                    .collect();
                i += group.len();
                let c0 = group[0];
                let shown: Vec<String> = group
                    .iter()
                    .take(MAX_LINES_SHOWN)
                    .map(|c| {
                        format!(
                            "{}:{} {:?} (added by seal {})",
                            f.path,
                            c.line,
                            c.text,
                            short(&c.added_by)
                        )
                    })
                    .collect();
                out.push(DoctorFinding::new(
                    CHECK_SURVIVAL,
                    Severity::Yellow,
                    format!(
                        "override captured another spec's version: spec '{}' seal {} overrode the removal check on {} and recorded the content of spec '{}' (seal {}) in place of {} of its own line(s): {}{}. If that was meant, confirm it with `writ finish --accept-removals {}`; else restore the lines and seal",
                        spec.spec_id,
                        short(&seal),
                        f.path,
                        c0.other_spec,
                        short(&c0.other_seal),
                        group.len(),
                        shown.join("; "),
                        if group.len() > MAX_LINES_SHOWN { format!("; and {} more", group.len() - MAX_LINES_SHOWN) } else { String::new() },
                        short(&seal),
                    ),
                    String::new(),
                    vec![f.path.clone()],
                ));
            }
            match f.status {
                FileAuditStatus::DeletedButPresent => out.push(DoctorFinding::new(
                    CHECK_SURVIVAL,
                    Severity::Red,
                    format!(
                        "spec '{}' sealed the deletion of {} (seal {}) but the file is back and no seal wrote it again",
                        spec.spec_id,
                        f.path,
                        short(&f.seal_id)
                    ),
                    String::new(),
                    vec![f.path.clone()],
                )),
                FileAuditStatus::IgnoredByGit => out.push(DoctorFinding::new(
                    CHECK_SURVIVAL,
                    Severity::Yellow,
                    format!(
                        "spec '{}' sealed {} (seal {}) but git ignores it ({}), so no finish can commit it: remove that ignore rule, or stop sealing the path",
                        spec.spec_id,
                        f.path,
                        short(&f.seal_id),
                        ignore_source(repo, &f.path)
                    ),
                    String::new(),
                    vec![f.path.clone()],
                )),
                _ => {}
            }
        }
    }
    out
}

/// The seal of `spec_id` that added `line` to `path`: the latest of the
/// spec's own seals of the path whose step (from its previous own
/// version) added it (finding 109). `None` when no own seal added it.
pub fn adding_seal(
    repo: &crate::Repository,
    spec_id: &str,
    path: &str,
    line: &str,
) -> crate::error::WritResult<Option<Seal>> {
    let mut seals = repo.spec_seals(spec_id)?;
    seals.reverse();
    let text = |h: &str| -> Option<String> {
        let b = repo.object_content(h).ok()?;
        (!crate::diff::is_binary(&b)).then(|| String::from_utf8_lossy(&b).into_owned())
    };
    let mut prev: Option<String> = None;
    let mut found = None;
    for seal in seals {
        let Some(change) = seal.changes.iter().find(|c| c.path == path) else {
            continue;
        };
        let old = prev
            .clone()
            .or_else(|| change.old_hash.as_deref().and_then(text))
            .unwrap_or_default();
        let new = change
            .new_hash
            .as_deref()
            .and_then(text)
            .unwrap_or_default();
        if additions(&old, &new).iter().any(|(_, l)| *l == line) {
            found = Some(seal.clone());
        }
        prev = Some(new);
    }
    Ok(found)
}

/// The lines a merge-loss escalation says would be lost.
pub fn escalation_lines(e: &super::PipelineEscalation) -> Vec<String> {
    e.right_content
        .as_deref()
        .map(|c| c.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Red findings for the seal-tree convergence escalations finish would
/// refuse on: a merge loss names each line, the seal that added it and its
/// spec, with the merge as the remover (finding 109); a conflict names the
/// file and specs.
fn escalation_findings(
    repo: &crate::Repository,
    escalations: &[super::PipelineEscalation],
) -> Vec<crate::doctor::DoctorFinding> {
    escalations
        .iter()
        .map(|e| {
            let message = if e.conflict_class == MERGE_LOSS_CLASS {
                let spec = &e.right_spec;
                let lines: Vec<String> = escalation_lines(e)
                    .iter()
                    .take(MAX_LINES_SHOWN)
                    .map(|l| {
                        let by = adding_seal(repo, spec, &e.file_path, l)
                            .ok()
                            .flatten()
                            .map(|s| format!("added by seal {} of spec '{spec}'", short(&s.id)))
                            .unwrap_or_else(|| format!("sealed by spec '{spec}'"));
                        format!("{}: {l:?} ({by})", e.file_path)
                    })
                    .collect();
                format!(
                    "{} sealed line(s) of spec '{spec}' would be lost by the merge finish would commit: {}. Restore them (`writ show <seal> --diff` shows the original) and seal, or resolve the merge (`writ converge-all --dry-run`)",
                    escalation_lines(e).len().max(1),
                    lines.join("; ")
                )
            } else {
                format!(
                    "the merge finish would commit has an unresolved conflict in {} (specs '{}' and '{}'): {}. Resolve it (`writ converge-all --dry-run` shows it) and seal",
                    e.file_path, e.left_spec, e.right_spec, e.reason
                )
            };
            crate::doctor::DoctorFinding::new(
                CHECK_SURVIVAL,
                crate::doctor::Severity::Red,
                message,
                String::new(),
                vec![e.file_path.clone()],
            )
        })
        .collect()
}

/// Seals whose captured removals a human confirmed with
/// `writ finish --accept-removals <seal>` (prefixes), in
/// `.writ/survival_accepted.json`.
pub const ACCEPTED_FILE: &str = "survival_accepted.json";

pub fn accepted_removals(repo: &crate::Repository) -> Vec<String> {
    std::fs::read(repo.writ_dir().join(ACCEPTED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Record a human's confirmation of an override's captured removals
/// (`writ finish --accept-removals <seal>`). The seal must exist; the
/// stored id is the full one. Returns it.
pub fn accept_removals(repo: &crate::Repository, seal: &str) -> crate::error::WritResult<String> {
    let id = repo.resolve_seal_id(seal)?;
    let mut all = accepted_removals(repo);
    if !all.contains(&id) {
        all.push(id.clone());
        all.sort();
        crate::fsutil::atomic_write(
            &repo.writ_dir().join(ACCEPTED_FILE),
            &serde_json::to_vec_pretty(&all)?,
        )?;
    }
    Ok(id)
}

/// The rule that makes git ignore `path` (`.gitignore:54: bench/x/`),
/// from `git check-ignore -v`; a generic phrase when git cannot say.
fn ignore_source(repo: &crate::Repository, path: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo.root())
        .args(["check-ignore", "-v", "--no-index", "--"])
        .arg(path)
        .output();
    out.ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let line = String::from_utf8_lossy(&o.stdout).into_owned();
            let rule = line.split('\t').next()?.trim().to_string();
            // "source:line:pattern" -> "source:line: pattern"
            let mut parts = rule.splitn(3, ':');
            let (src, n, pat) = (parts.next()?, parts.next()?, parts.next()?);
            Some(format!("{src}:{n}: {pat}"))
        })
        .unwrap_or_else(|| "a .gitignore rule".to_string())
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
