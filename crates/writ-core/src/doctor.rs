//! `writ doctor`: name every bad state writ can leave a repository in, and
//! print the exact command that gets the user out of it.
//!
//! **Tiers.** 0.4.0 ships the *fast* tier only: six checks that read the
//! store, the spec files, the stat cache and the environment, budgeted at
//! 300 ms on writ's own repository. The *survival* tier (per-spec added-line
//! survival against the working tree) lands in 0.4.1.
//!
//! **Honesty condition.** The first line of every report names the tier that
//! ran and when the survival tier was last green. A clean fast tier is *not*
//! a statement that finishing is safe, so the report never says so.
//!
//! **Stable ids.** Each finding carries one of the [`CHECK_IDS`]; scripts,
//! CI and tests match on these strings, so they never change once released.

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::{WritError, WritResult};
use crate::repo::Repository;

/// The tier this build runs.
pub const TIER_FAST: &str = "fast";

/// Store integrity: referenced-but-missing objects, unreadable trees,
/// orphans, and a damaged `.writ` layout.
pub const CHECK_STORE_INTEGRITY: &str = "store_integrity";
/// A spec claim whose holder is gone or idle past `stale_claim_minutes`.
pub const CHECK_STALE_CLAIM: &str = "stale_claim";
/// A file whose newest seal is on a committed spec and whose content differs
/// from git HEAD (findings 65, 74).
pub const CHECK_COMMITTED_SPEC_SEAL: &str = "committed_spec_seal";
/// Pending work touched by more than one agent, or pending too long while
/// another agent is active.
pub const CHECK_UNSEALED_AT_RISK: &str = "unsealed_at_risk";
/// Binary vs repo schema, more than one `writ` on PATH, stale managed text.
pub const CHECK_VERSION_SKEW: &str = "version_skew";
/// Files genuinely uncommitted and unsealed that finish would leave out
/// (finding 75).
pub const CHECK_LEFT_OUT: &str = "left_out";

/// Every check id, in the order the fast tier runs them.
pub const CHECK_IDS: [&str; 6] = [
    CHECK_STORE_INTEGRITY,
    CHECK_STALE_CLAIM,
    CHECK_COMMITTED_SPEC_SEAL,
    CHECK_UNSEALED_AT_RISK,
    CHECK_VERSION_SKEW,
    CHECK_LEFT_OUT,
];

/// The fast-tier part of the headline on a repository with no findings.
/// The survival part follows after "; " (see [`headline_with`]).
pub const CLEAN_HEADLINE: &str = "fast checks clean";

/// Survival tier (0.4.1): lost sealed lines (red) and sealed files git
/// ignores (yellow). Runs on `writ doctor --survival`, before finish and on
/// watch; the fast tier reads only its cached result.
pub const CHECK_SURVIVAL: &str = crate::convergence::survival::CHECK_SURVIVAL;

/// Said only when the fast tier is clean and the survival tier is green on
/// the current state (Aubs's honesty condition).
pub const SAFE_TO_FINISH: &str = "safe to finish";

/// Default minutes before an idle claim counts as stale.
pub const DEFAULT_STALE_CLAIM_MINUTES: u64 = 120;
/// Default minutes before pending work counts as at risk.
pub const DEFAULT_UNSEALED_MINUTES: u64 = 30;

/// The `[doctor]` table of `.writ/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorConfig {
    /// A claim idle this long, or whose holder's session is gone, is stale.
    #[serde(default = "default_stale_claim_minutes")]
    pub stale_claim_minutes: u64,
    /// Pending work older than this while another agent is active is at risk.
    #[serde(default = "default_unsealed_minutes")]
    pub unsealed_minutes: u64,
    /// Object hash prefixes (12+ chars) known to be permanently missing (or
    /// unreadable) and excused from `store_integrity`, like
    /// `store_integrity.py --allow-missing`. Written by
    /// `writ doctor --allow-missing`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_missing: Vec<String>,
}

fn default_stale_claim_minutes() -> u64 {
    DEFAULT_STALE_CLAIM_MINUTES
}
fn default_unsealed_minutes() -> u64 {
    DEFAULT_UNSEALED_MINUTES
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self {
            stale_claim_minutes: DEFAULT_STALE_CLAIM_MINUTES,
            unsealed_minutes: DEFAULT_UNSEALED_MINUTES,
            allow_missing: Vec::new(),
        }
    }
}

impl DoctorConfig {
    /// Read `[doctor]` from `.writ/config.toml`. Missing file or table gives
    /// defaults; an unparseable file is an error (doctor must not guess).
    pub fn load(writ_dir: &Path) -> WritResult<Self> {
        let path = writ_dir.join("config.toml");
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let cfg: crate::config::ProjectConfig = toml::from_str(&data)
            .map_err(|e| WritError::Other(format!("cannot parse {}: {e}", path.display())))?;
        Ok(cfg.doctor.unwrap_or_default())
    }

    /// True when `hash` matches an `allow_missing` prefix.
    pub fn excuses(&self, hash: &str) -> bool {
        self.allow_missing
            .iter()
            .any(|p| p.len() >= 12 && hash.starts_with(p.as_str()))
    }
}

/// How bad a finding is. `Red` blocks `writ finish` (unless `--force`);
/// `Yellow` is reported but does not block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Red,
    Yellow,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Red => "red",
            Severity::Yellow => "yellow",
        }
    }
}

/// One bad state, with the command that fixes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorFinding {
    /// One of [`CHECK_IDS`].
    pub check: String,
    pub severity: Severity,
    /// One line, plain language, names the thing that is wrong.
    pub message: String,
    /// The exact command to paste; pasting it clears the finding. `None`
    /// (JSON null) when the fix needs a human decision (e.g. which of two
    /// unmanaged binaries to keep): then `needs_human` is true and the
    /// message says what to decide. Never a command that deletes, moves or
    /// truncates a file writ does not own.
    pub fix_command: Option<String>,
    /// True when no command can be printed and a person must decide.
    #[serde(default)]
    pub needs_human: bool,
    /// Repo-relative paths the finding is about (may be empty).
    #[serde(default)]
    pub paths: Vec<String>,
}

impl DoctorFinding {
    pub fn new(
        check: &str,
        severity: Severity,
        message: impl Into<String>,
        fix_command: impl Into<String>,
        paths: Vec<String>,
    ) -> Self {
        let fix_command = fix_command.into();
        // Finding 79: a destructive fix is never printed. Debug builds fail
        // loudly so a test catches the check that tried.
        debug_assert!(
            !is_destructive(&fix_command),
            "{check}: destructive fix_command {fix_command:?}"
        );
        // Finding 91: `--force` is a human's decision, never a printed fix.
        debug_assert!(
            !fix_command.contains("--force"),
            "{check}: fix_command uses --force {fix_command:?}"
        );
        let fix_command = Some(fix_command)
            .filter(|c| !c.is_empty() && !is_destructive(c) && !c.contains("--force"));
        Self {
            check: check.to_string(),
            severity,
            message: message.into(),
            needs_human: fix_command.is_none(),
            fix_command,
            paths,
        }
    }
}

/// The full fast-tier report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Always the first line printed. Names the tier and the survival state.
    pub headline: String,
    /// Which tier ran: `"fast"` in 0.4.0.
    pub tier: String,
    /// The newest seal when the survival tier was last green here. `None`
    /// when it never was.
    pub survival_last_green: Option<String>,
    /// The survival part of the headline (also in the brief line).
    #[serde(default)]
    pub survival: String,
    /// Survival tier info notes (finding 111): not findings, never affect
    /// `clean` or "safe to finish".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub survival_info: Vec<String>,
    /// Check ids that ran, in order.
    pub checks_run: Vec<String>,
    pub findings: Vec<DoctorFinding>,
    /// True when there are no findings at all (exit 0).
    pub clean: bool,
    /// Count of red findings (block finish).
    pub red: usize,
    /// Count of yellow findings.
    pub yellow: usize,
    /// Wall time of this run in milliseconds.
    pub elapsed_ms: u64,
    /// Wall time per check id, in milliseconds (for the latency budget).
    #[serde(default)]
    pub check_ms: std::collections::BTreeMap<String, u64>,
}

impl DoctorReport {
    fn from_findings(
        findings: Vec<DoctorFinding>,
        checks_run: Vec<String>,
        started: Instant,
    ) -> Self {
        let red = findings
            .iter()
            .filter(|f| f.severity == Severity::Red)
            .count();
        let yellow = findings.len() - red;
        Self {
            headline: headline(red, yellow),
            tier: TIER_FAST.to_string(),
            survival_last_green: None,
            survival: crate::convergence::survival::SurvivalState::NotRun.note(),
            survival_info: Vec::new(),
            checks_run,
            clean: findings.is_empty(),
            findings,
            red,
            yellow,
            elapsed_ms: started.elapsed().as_millis() as u64,
            check_ms: Default::default(),
        }
    }

    /// True when `writ finish` must refuse (any red finding).
    pub fn blocks_finish(&self) -> bool {
        self.red > 0
    }

    /// The single `doctor:` line for `writ context --format brief`.
    pub fn brief_line(&self) -> String {
        if self.clean {
            return self.headline.clone();
        }
        let mut ids: Vec<&str> = Vec::new();
        for f in &self.findings {
            if !ids.contains(&f.check.as_str()) {
                ids.push(f.check.as_str());
            }
        }
        format!(
            "fast checks: {} red, {} yellow ({}); run `writ doctor`; {}",
            self.red,
            self.yellow,
            ids.join(", "),
            self.survival
        )
    }
}

/// The doctor section when the fast tier is not clean (finding 90), for
/// commands that report after acting (seal, spec done, status). `None`
/// when clean. A doctor error is reported as the headline.
pub fn notice(repo: &Repository) -> Option<crate::context::ContextDoctor> {
    match run(repo) {
        Ok(report) if report.clean => None,
        Ok(report) => {
            let mut findings = report.findings.clone();
            findings.sort_by_key(|f| f.severity != Severity::Red);
            Some(crate::context::ContextDoctor {
                headline: report.headline.clone(),
                brief: report.brief_line(),
                findings,
                findings_omitted: 0,
            })
        }
        Err(e) => {
            let line = format!("doctor could not run ({e}); run `writ doctor`");
            Some(crate::context::ContextDoctor {
                headline: line.clone(),
                brief: line,
                findings: Vec::new(),
                findings_omitted: 0,
            })
        }
    }
}

/// Put the doctor section into `ctx` (finding 86) and, when doctor is red,
/// point `next` at the first red finding's fix. A doctor error becomes the
/// headline, never a missing section.
pub fn attach_to_context(repo: &Repository, ctx: &mut crate::context::ContextOutput) {
    let section = match run(repo) {
        Ok(report) => {
            if let Some(first) = report.findings.iter().find(|f| f.severity == Severity::Red) {
                let todo = match &first.fix_command {
                    Some(cmd) => format!("run `{cmd}`"),
                    None => "needs your decision; see `writ doctor`".to_string(),
                };
                ctx.recommended_action = Some(crate::context::RecommendedAction {
                    action: "doctor_fix".to_string(),
                    message: format!("doctor {}: {}; {todo}", first.check, first.message),
                    priority: "high".to_string(),
                });
            } else {
                defer_to_clean_doctor(&mut ctx.recommended_action);
            }
            let mut findings = report.findings.clone();
            // Reds first (stable), so a budget trim drops yellows first.
            findings.sort_by_key(|f| f.severity != Severity::Red);
            crate::context::ContextDoctor {
                headline: report.headline.clone(),
                brief: report.brief_line(),
                findings,
                findings_omitted: 0,
            }
        }
        Err(e) => {
            let line = format!("doctor could not run ({e}); run `writ doctor`");
            crate::context::ContextDoctor {
                headline: line.clone(),
                brief: line,
                findings: Vec::new(),
                findings_omitted: 0,
            }
        }
    };
    ctx.doctor = Some(section);
}

/// Finding 97: `next` defers to doctor. A clean doctor never sits under a
/// high-priority claim about repository health (converge, review_risk):
/// with no red finding those are shown at medium. A blocking dependency is
/// a fact about the spec's own ordering, not a health claim, and stays high.
fn defer_to_clean_doctor(action: &mut Option<crate::context::RecommendedAction>) {
    if let Some(a) = action.as_mut() {
        if a.priority == "high" && a.action != "wait_for_dependency" {
            a.priority = "medium".to_string();
        }
    }
}

/// The `doctor` line for `writ context --format brief`: the brief line of
/// a fresh run, or the error that stopped doctor (never silently absent).
pub fn brief_line_for(repo: &Repository) -> String {
    match run(repo) {
        Ok(report) => report.brief_line(),
        Err(e) => format!("doctor could not run ({e}); run `writ doctor`"),
    }
}

/// The headline for a fast-tier run with these counts, survival never run.
pub fn headline(red: usize, yellow: usize) -> String {
    let note = crate::convergence::survival::SurvivalState::NotRun.note();
    headline_with(red, yellow, &note, false)
}

/// The headline: the fast-tier counts, then the survival `note`, then
/// "safe to finish" only when `safe` (both tiers green on the current
/// state) and there is no finding at all.
pub fn headline_with(red: usize, yellow: usize, note: &str, safe: bool) -> String {
    if red == 0 && yellow == 0 {
        if safe {
            return format!("{CLEAN_HEADLINE}; {note}; {SAFE_TO_FINISH}");
        }
        return format!("{CLEAN_HEADLINE}; {note}");
    }
    format!(
        "fast checks: {} finding(s), {red} red, {yellow} yellow; {note}",
        red + yellow
    )
}

// ── Survival tier section (Haris) ──────────────────────────────────────

/// Put the survival state into a fast-tier report from the cache alone:
/// `survival_last_green`, the survival note and the headline.
fn apply_survival_state(repo: &Repository, report: &mut DoctorReport) {
    use crate::convergence::survival::SurvivalState;
    let state = SurvivalState::current(repo);
    report.survival_last_green = SurvivalState::last_green(repo);
    report.survival = state.note();
    report.survival_info = crate::convergence::survival::SurvivalCache::load(repo)
        .map(|c| c.info)
        .unwrap_or_default();
    report.headline = headline_with(report.red, report.yellow, &report.survival, state.is_safe());
}

/// `writ doctor --survival`: the fast tier, then the survival tier (served
/// from its cache when nothing changed), its findings added to the report.
pub fn run_survival(repo: &Repository) -> WritResult<DoctorReport> {
    use crate::convergence::survival::{run_tier, SurvivalState};
    let mut report = run(repo)?;
    let lap = Instant::now();
    let tier = run_tier(repo)?;
    report
        .check_ms
        .insert(CHECK_SURVIVAL.to_string(), lap.elapsed().as_millis() as u64);
    report.checks_run.push(CHECK_SURVIVAL.to_string());
    report.tier = format!("{TIER_FAST}+{CHECK_SURVIVAL}");
    let (fast_red, fast_yellow) = (report.red, report.yellow);
    report.findings.extend(tier.cache.findings.iter().cloned());
    report.survival_info = tier.cache.info.clone();
    report.red = report
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Red)
        .count();
    report.yellow = report.findings.len() - report.red;
    report.clean = report.findings.is_empty();
    let state = SurvivalState::current(repo);
    report.survival_last_green = tier.cache.last_green.clone();
    report.survival = state.note();
    // The headline counts the fast tier; the survival note says the rest.
    report.headline = headline_with(fast_red, fast_yellow, &report.survival, state.is_safe());
    report.elapsed_ms += report.check_ms[CHECK_SURVIVAL];
    Ok(report)
}

// ── End of survival tier section ───────────────────────────────────────

/// Run the fast tier against an open repository.
pub fn run(repo: &Repository) -> WritResult<DoctorReport> {
    let started = Instant::now();
    let mut findings = Vec::new();
    let mut checks_run = Vec::new();
    let mut check_ms = std::collections::BTreeMap::new();
    let mut lap = Instant::now();
    let mut done = |id: &str, checks_run: &mut Vec<String>, lap: &mut Instant| {
        checks_run.push(id.to_string());
        check_ms.insert(id.to_string(), lap.elapsed().as_millis() as u64);
        *lap = Instant::now();
    };

    let layout = crate::repair::layout_problems(repo.writ_dir());
    // An unparseable config.toml is itself a layout finding; use defaults.
    let config = DoctorConfig::load(repo.writ_dir()).unwrap_or_default();
    findings.extend(check_layout(&layout));
    if layout.iter().any(|p| p.blocks_store_scan()) {
        // Every later check reads the index or the records `writ repair`
        // must fix first; run only what does not, and say so in checks_run.
        done(CHECK_STORE_INTEGRITY, &mut checks_run, &mut lap);
        findings.extend(check_version_skew(repo.root()));
        done(CHECK_VERSION_SKEW, &mut checks_run, &mut lap);
        let mut report = DoctorReport::from_findings(findings, checks_run, started);
        report.check_ms = check_ms;
        return Ok(report);
    }
    findings.extend(check_store_integrity(
        repo.root(),
        repo.writ_dir(),
        &config,
    )?);
    done(CHECK_STORE_INTEGRITY, &mut checks_run, &mut lap);

    let specs = repo.list_specs()?;
    let now = chrono::Utc::now();
    findings.extend(check_stale_claims(&specs, &config, now));
    done(CHECK_STALE_CLAIM, &mut checks_run, &mut lap);

    let actor = Actor::resolve(repo, &specs, now);
    let stuck = check_committed_spec_seals(repo, &actor)?;
    let mut claimed: std::collections::HashSet<String> =
        stuck.iter().flat_map(|f| f.paths.clone()).collect();
    findings.extend(stuck);
    done(CHECK_COMMITTED_SPEC_SEAL, &mut checks_run, &mut lap);

    let pending = repo.state()?.changes;
    let ctx = PendingContext::build(repo, &specs, &pending, &config, now, &claimed)?;
    let at_risk = check_unsealed_at_risk(repo.root(), &ctx, &config, &actor);
    claimed.extend(at_risk.iter().flat_map(|f| f.paths.clone()));
    findings.extend(at_risk);
    done(CHECK_UNSEALED_AT_RISK, &mut checks_run, &mut lap);

    findings.extend(check_version_skew(repo.root()));
    done(CHECK_VERSION_SKEW, &mut checks_run, &mut lap);

    findings.extend(check_left_out(repo.root(), &specs, &ctx, &claimed, &actor));
    done(CHECK_LEFT_OUT, &mut checks_run, &mut lap);

    let mut report = DoctorReport::from_findings(findings, checks_run, started);
    report.check_ms = check_ms;
    apply_survival_state(repo, &mut report);
    Ok(report)
}

/// Event type written when `writ finish` refuses (finding 76).
pub const EVENT_FINISH_REFUSED: &str = "finish_refused";

/// Why finish refused. Serialized as the `reason` field of the event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishRefusal {
    /// `writ doctor` reported a red finding (no `--force`).
    DoctorRed,
    /// The staged tree failed the finish check (`cargo check` or
    /// `[workflow] finish_check`).
    CompileCheck,
    /// Merge would lose a spec's sealed lines (merge-survival check).
    SurvivalCheck,
    /// Convergence left unresolved conflicts.
    ConvergenceConflict,
    /// `--strict`: a completed spec's own sealed version is stale.
    StrictStale,
}

/// One `finish_refused` line in `.writ/security/events.jsonl`: the five
/// `SecurityEvent` fields plus `reason`, `specs` and `files`, so the hurdle
/// metric can be read from the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinishRefusedEvent {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub severity: crate::security::Severity,
    pub event_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub details: String,
    pub reason: FinishRefusal,
    pub specs: Vec<String>,
    pub files: Vec<String>,
}

/// Record a finish refusal. The caller still refuses if this write fails;
/// the error is returned so the caller can report it, never swallowed.
pub fn record_finish_refused(
    writ_dir: &Path,
    agent_id: Option<&str>,
    reason: FinishRefusal,
    details: &str,
    specs: &[String],
    files: &[String],
) -> WritResult<()> {
    let event = FinishRefusedEvent {
        timestamp: chrono::Utc::now(),
        severity: crate::security::Severity::Warning,
        event_type: EVENT_FINISH_REFUSED.to_string(),
        agent_id: agent_id.map(str::to_string),
        details: details.to_string(),
        reason,
        specs: specs.to_vec(),
        files: files.to_vec(),
    };
    crate::security::SecurityEventLogger::new(writ_dir).emit_record(&event)
}

/// The report for a repository that `Repository::open` refused because its
/// schema is newer than this binary (finding 59). `None` for any other open
/// error, which the caller reports as an error.
pub fn open_refusal_report(err: &WritError) -> Option<DoctorReport> {
    let msg = err.to_string();
    if !msg.contains("this repository uses writ schema v") {
        return None;
    }
    let finding = DoctorFinding::new(
        CHECK_VERSION_SKEW,
        Severity::Red,
        msg,
        "brew upgrade writ",
        Vec::new(),
    );
    Some(DoctorReport::from_findings(
        vec![finding],
        vec![CHECK_VERSION_SKEW.to_string()],
        Instant::now(),
    ))
}

/// Check 1a: the `.writ` layout. Every problem here is fixed by
/// `writ repair` (it creates directories, rebuilds HEAD and the index, moves
/// unparseable records to `.writ/quarantine/`). A missing `version.toml`
/// (pre-versioning repository) is yellow; everything else is red.
fn check_layout(problems: &[crate::repair::LayoutProblem]) -> Vec<DoctorFinding> {
    use crate::repair::LayoutProblem;
    problems
        .iter()
        .map(|p| {
            let severity = match p {
                LayoutProblem::VersionFile { reason } if reason == "missing" => Severity::Yellow,
                _ => Severity::Red,
            };
            DoctorFinding::new(
                CHECK_STORE_INTEGRITY,
                severity,
                p.describe(),
                "writ repair",
                vec![p.path()],
            )
        })
        .collect()
}

/// Check 1b: every live object present and readable.
///
/// Objects `writ repair` can regenerate get fix `writ repair`. Objects it
/// cannot (and unreadable trees) get the `writ doctor --allow-missing`
/// command that records the loss as accepted, so the fix still clears the
/// finding when pasted. Repair runs as a dry run here only when something
/// is missing, so the clean path stays a single store scan.
fn check_store_integrity(
    root: &Path,
    writ_dir: &Path,
    config: &DoctorConfig,
) -> WritResult<Vec<DoctorFinding>> {
    let mut check = cached_store_check(writ_dir)?;
    check.missing_objects.retain(|m| !config.excuses(&m.hash));
    check.unreadable_trees.retain(|t| !config.excuses(&t.hash));
    if check.is_clean() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut accept: Vec<String> = Vec::new();
    let mut accept_paths: Vec<String> = Vec::new();
    if !check.missing_objects.is_empty() {
        let plan = crate::repair::repair_store(root, writ_dir, true)?;
        let unrecoverable: Vec<&crate::repair::UnrecoverableObject> = plan
            .unrecoverable
            .iter()
            .filter(|u| !config.excuses(&u.hash))
            .collect();
        let mut recoverable: Vec<String> = plan.recovered.iter().map(|r| r.path.clone()).collect();
        recoverable.sort();
        recoverable.dedup();
        if !recoverable.is_empty() {
            out.push(DoctorFinding::new(
                CHECK_STORE_INTEGRITY,
                Severity::Red,
                format!(
                    "{} referenced object(s) missing from .writ/objects; writ repair can regenerate them",
                    plan.recovered.len()
                ),
                "writ repair",
                recoverable,
            ));
        }
        for u in unrecoverable {
            accept.push(u.hash[..12].to_string());
            accept_paths.push(u.path.clone());
        }
    }
    for t in &check.unreadable_trees {
        accept.push(t.hash[..12].to_string());
        accept_paths.push(t.referenced_as.clone());
    }
    if !accept.is_empty() {
        accept.sort();
        accept.dedup();
        accept_paths.sort();
        accept_paths.dedup();
        out.push(DoctorFinding::new(
            CHECK_STORE_INTEGRITY,
            Severity::Red,
            format!(
                "{} object(s) missing or unreadable that no source can regenerate; the fix records the loss as accepted",
                accept.len()
            ),
            allow_missing_command(&accept),
            accept_paths,
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Check 2: stale claims
// ---------------------------------------------------------------------------

/// True for a spec still being worked on (not complete, not closed).
fn spec_is_open(spec: &crate::spec::Spec) -> bool {
    use crate::spec::{LifecycleState, SpecStatus};
    spec.status != SpecStatus::Complete
        && !matches!(
            spec.lifecycle_state,
            LifecycleState::Cancelled | LifecycleState::Completed | LifecycleState::Archived
        )
}

/// Whether a claim's holder is provably gone (finding 91). Shared by
/// doctor's `stale_claim` check and `writ spec release --stale`, so the
/// `--stale` fix doctor prints always succeeds when pasted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleVerdict {
    /// Proof the holder is gone; the string names it.
    Gone(String),
    /// The holder's process is running; the string names pid and start.
    Alive(String),
    /// No proof either way; the string names the missing evidence.
    Unproven(String),
}

/// Judge a claim for `--stale`: proof needs the recorded pid, its start
/// time and host, this machine's host name matching it, and a readable
/// process table. Idle time is never evidence.
pub fn stale_verdict(
    spec: &crate::spec::Spec,
    host: Option<&str>,
    table: Option<&[crate::agent::session::ProcInfo]>,
) -> StaleVerdict {
    let missing: Vec<&str> = [
        ("pid", spec.claimed_pid.is_none()),
        ("pid start time", spec.claimed_pid_start.is_none()),
        ("host", spec.claimed_host.is_none()),
    ]
    .into_iter()
    .filter_map(|(name, absent)| absent.then_some(name))
    .collect();
    let (Some(pid), Some(start), Some(claim_host)) = (
        spec.claimed_pid,
        spec.claimed_pid_start.as_deref(),
        spec.claimed_host.as_deref(),
    ) else {
        return StaleVerdict::Unproven(format!(
            "the claim records no {} (taken by an older writ or a session writ could not identify)",
            missing.join(", no ")
        ));
    };
    let Some(host) = host else {
        return StaleVerdict::Unproven(format!(
            "the claim was taken on host '{claim_host}' and this machine's host name cannot be read"
        ));
    };
    if claim_host != host {
        return StaleVerdict::Unproven(format!(
            "the claim was taken on host '{claim_host}', this is host '{host}'; writ cannot see another host's processes"
        ));
    }
    let Some(table) = table else {
        return StaleVerdict::Unproven(format!(
            "the process table cannot be read on host '{host}', so pid {pid} cannot be checked"
        ));
    };
    match table.iter().find(|p| p.pid == pid) {
        Some(p) if p.start == start => StaleVerdict::Alive(format!(
            "the holder is alive: pid {pid} (started {start}) is running on host '{host}'"
        )),
        Some(p) => StaleVerdict::Gone(format!(
            "pid {pid} on host '{host}' now belongs to a process started {}, not the holder started {start}",
            p.start
        )),
        None => StaleVerdict::Gone(format!("pid {pid} on host '{host}' has exited")),
    }
}

/// The verdict for `spec` on this machine, reading the process table only
/// when the claim can be judged by process.
pub fn stale_verdict_here(spec: &crate::spec::Spec) -> StaleVerdict {
    let host = crate::agent::session::host_name();
    let judgeable = spec.claimed_pid.is_some()
        && spec.claimed_pid_start.is_some()
        && spec.claimed_host.is_some()
        && spec.claimed_host == host;
    let table = if judgeable {
        crate::agent::session::process_table()
    } else {
        None
    };
    stale_verdict(spec, host.as_deref(), table.as_deref())
}

fn check_stale_claims(
    specs: &[crate::spec::Spec],
    config: &DoctorConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<DoctorFinding> {
    let claimed: Vec<&crate::spec::Spec> = specs
        .iter()
        .filter(|s| s.claimed_by.is_some() && spec_is_open(s))
        .collect();
    if claimed.is_empty() {
        return Vec::new();
    }
    let host = crate::agent::session::host_name();
    // One `ps` only when some claim can be judged by process.
    let table = if claimed
        .iter()
        .any(|s| s.claimed_pid.is_some() && s.claimed_pid_start.is_some() && s.claimed_host == host)
    {
        crate::agent::session::process_table()
    } else {
        None
    };
    let limit = chrono::Duration::minutes(config.stale_claim_minutes as i64);
    let mut out = Vec::new();
    for spec in claimed {
        let holder = spec.claimed_by.as_deref().unwrap_or_default();
        let idle = now - std::cmp::max(spec.last_activity, spec.updated_at);
        // Finding 91: the printed fix is `--stale` and only with proof the
        // holder is gone; without proof a human decides. Never `--force`.
        let finding = match stale_verdict(spec, host.as_deref(), table.as_deref()) {
            StaleVerdict::Alive(_) => continue,
            StaleVerdict::Gone(proof) => DoctorFinding::new(
                CHECK_STALE_CLAIM,
                Severity::Yellow,
                format!("spec '{}' is claimed by '{holder}', but {proof}", spec.id),
                format!("writ spec release {} --stale", shell_quote(&spec.id)),
                Vec::new(),
            ),
            StaleVerdict::Unproven(why) if idle > limit => DoctorFinding::new(
                CHECK_STALE_CLAIM,
                Severity::Yellow,
                format!(
                    "spec '{}' is claimed by '{holder}', idle {} min (limit {} min, [doctor] stale_claim_minutes), \
                     and writ cannot prove the holder is gone: {why}. Ask '{holder}'; if it is gone, \
                     a human may run `writ spec release {} --force`",
                    spec.id,
                    idle.num_minutes(),
                    config.stale_claim_minutes,
                    shell_quote(&spec.id)
                ),
                "",
                Vec::new(),
            ),
            StaleVerdict::Unproven(_) => continue,
        };
        out.push(finding);
    }
    out
}

// ---------------------------------------------------------------------------
// Check 3: files stuck on a committed spec (findings 65, 74)
// ---------------------------------------------------------------------------

fn check_committed_spec_seals(repo: &Repository, actor: &Actor) -> WritResult<Vec<DoctorFinding>> {
    let stuck = repo.stuck_files()?;
    let mut by_spec: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for f in stuck {
        by_spec.entry(f.spec_id).or_default().push(f.path);
    }
    let mut out = Vec::new();
    for (spec_id, mut paths) in by_spec {
        paths.sort();
        let fix = seal_fix(
            actor,
            None,
            &format!("follow-up to {spec_id}: content the committed spec did not carry"),
            &paths,
        );
        out.push(DoctorFinding::new(
            CHECK_COMMITTED_SPEC_SEAL,
            Severity::Red,
            format!(
                "{} file(s) last sealed on committed spec '{spec_id}' differ from git HEAD; no finish will commit them",
                paths.len()
            ),
            fix,
            paths,
        ));
    }
    Ok(out)
}

/// Quote for a POSIX shell when needed, so fix commands paste verbatim.
pub fn shell_quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./,:=@+".contains(c));
    if plain {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

// ---------------------------------------------------------------------------
// Checks 4 and 6: pending work
// ---------------------------------------------------------------------------

/// Who has sealed each pending path on open specs, and who is active now.
struct PendingContext {
    /// Pending paths, sorted.
    pending: Vec<String>,
    /// Pending path -> (agents that sealed it on open specs, newest open
    /// spec that sealed it).
    sealers:
        std::collections::BTreeMap<String, (std::collections::BTreeSet<String>, Option<String>)>,
    /// Agents with a seal or claim activity inside the unsealed window.
    active: std::collections::BTreeSet<String>,
    /// Pending path -> every seal of it on an open spec: (agent, spec, when).
    touches:
        std::collections::BTreeMap<String, Vec<(String, String, chrono::DateTime<chrono::Utc>)>>,
}

impl PendingContext {
    /// `skip`: paths another check already reported.
    fn build(
        repo: &Repository,
        specs: &[crate::spec::Spec],
        pending: &[crate::state::FileState],
        config: &DoctorConfig,
        now: chrono::DateTime<chrono::Utc>,
        skip: &std::collections::HashSet<String>,
    ) -> WritResult<Self> {
        let window = chrono::Duration::minutes(config.unsealed_minutes as i64);
        let mut pending_paths: Vec<String> = pending
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| !skip.contains(p))
            .collect();
        pending_paths.sort();
        let wanted: std::collections::HashSet<&str> =
            pending_paths.iter().map(String::as_str).collect();
        let mut sealers: std::collections::BTreeMap<
            String,
            (std::collections::BTreeSet<String>, Option<String>),
        > = Default::default();
        let mut newest: std::collections::HashMap<String, chrono::DateTime<chrono::Utc>> =
            Default::default();
        let mut active = std::collections::BTreeSet::new();
        let mut touches: std::collections::BTreeMap<
            String,
            Vec<(String, String, chrono::DateTime<chrono::Utc>)>,
        > = Default::default();
        for spec in specs.iter().filter(|s| spec_is_open(s)) {
            if let Some(holder) = &spec.claimed_by {
                if now - spec.last_activity <= window {
                    active.insert(holder.clone());
                }
            }
            if spec.sealed_by.is_empty() {
                continue;
            }
            for seal in repo.spec_seals(&spec.id)? {
                if now - seal.timestamp <= window {
                    active.insert(seal.agent.id.clone());
                }
                for c in seal
                    .changes
                    .iter()
                    .filter(|c| wanted.contains(c.path.as_str()))
                {
                    touches.entry(c.path.clone()).or_default().push((
                        seal.agent.id.clone(),
                        spec.id.clone(),
                        seal.timestamp,
                    ));
                    let entry = sealers.entry(c.path.clone()).or_default();
                    entry.0.insert(seal.agent.id.clone());
                    let at = newest.entry(c.path.clone()).or_insert(seal.timestamp);
                    if seal.timestamp >= *at {
                        *at = seal.timestamp;
                        entry.1 = Some(spec.id.clone());
                    }
                }
            }
        }
        Ok(Self {
            pending: pending_paths,
            sealers,
            active,
            touches,
        })
    }
}

/// Agents other than `agent` who worked on `path` (finding 102), from the
/// same per-path data `unsealed_at_risk` uses: agents that sealed `path` on
/// an open spec after `since` (the caller's spec's last seal of the path),
/// plus active agents whose open spec sealed `path` or has it in its file
/// scope. Sorted and deduped; empty means nobody.
pub fn other_touchers(
    repo: &Repository,
    path: &str,
    spec_id: &str,
    agent: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> WritResult<Vec<String>> {
    let specs = repo.list_specs()?;
    let config = DoctorConfig::load(repo.writ_dir())?;
    let now = chrono::Utc::now();
    let pending = [crate::state::FileState {
        path: path.to_string(),
        status: crate::state::FileStatus::Modified,
        hash: None,
    }];
    let ctx = PendingContext::build(repo, &specs, &pending, &config, now, &Default::default())?;
    let mut out = std::collections::BTreeSet::new();
    for (who, _spec, at) in ctx.touches.get(path).into_iter().flatten() {
        let after = since.map_or(true, |t| *at > t);
        if after || ctx.active.contains(who) {
            out.insert(who.clone());
        }
    }
    let in_scope = |s: &crate::spec::Spec| {
        s.file_scope
            .iter()
            .any(|e| e == path || (e.ends_with('/') && path.starts_with(e.as_str())))
    };
    for spec in specs
        .iter()
        .filter(|s| spec_is_open(s) && s.id != spec_id && in_scope(s))
    {
        if let Some(holder) = spec.claimed_by.as_ref().filter(|h| ctx.active.contains(*h)) {
            out.insert(holder.clone());
        }
    }
    out.remove(agent);
    Ok(out.into_iter().collect())
}

/// Minutes since `root/path` was last written; None when it is gone.
fn pending_age_minutes(root: &Path, path: &str, now: std::time::SystemTime) -> Option<i64> {
    let modified = std::fs::metadata(root.join(path)).ok()?.modified().ok()?;
    Some(now.duration_since(modified).ok()?.as_secs() as i64 / 60)
}

/// The seal command for `paths` that works for whoever pastes it: seal
/// onto `owner` when the acting identity holds it open, else onto one of
/// the acting identity's open claimed specs, else create and claim a
/// follow-up spec first (a seal needs an open spec for its agent).
/// The seal command for `paths` that works for whoever pastes it, in the
/// order (findings 81, 84): seal onto `owner` when the acting identity
/// holds it open, else onto one of its open claimed specs, else onto one
/// follow-up spec per run with a fixed id. Each such fix creates (or, if it
/// exists and is the agent's, reuses) that spec itself. Every seal names its
/// spec with `--spec`.
fn seal_fix(actor: &Actor, owner: Option<&str>, summary: &str, paths: &[String]) -> String {
    let seal = |spec: &str| {
        format!(
            "writ seal -s {} --spec {} --paths {}",
            shell_quote(summary),
            shell_quote(spec),
            shell_quote(&paths.join(","))
        )
    };
    if let Some(owner) = owner.filter(|o| actor.open_claims.iter().any(|c| c == o)) {
        return seal(owner);
    }
    if let Some(first) = actor.open_claims.first() {
        return seal(first);
    }
    // Finding 84: every fix stands alone; `spec add --id --claim` is
    // idempotent for the same agent, so any paste order works.
    format!(
        "writ spec add {} --id {} --claim && {}",
        shell_quote("follow-up to writ doctor findings"),
        actor.follow_up_id,
        seal(&actor.follow_up_id)
    )
}

/// The identity running doctor, the open specs it has claimed, and the
/// follow-up spec doctor's fixes create when it has none.
struct Actor {
    open_claims: Vec<String>,
    /// `doctor-follow-up-<yyyymmdd>`, suffixed `-2`, `-3`... past ids taken.
    follow_up_id: String,
}

impl Actor {
    fn resolve(
        repo: &Repository,
        specs: &[crate::spec::Spec],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let id = crate::agent::resolve_agent_id_in(
            None,
            repo.settings().default_agent.as_deref(),
            true,
            Some(repo.writ_dir()),
        )
        .id;
        let mut open_claims: Vec<String> = specs
            .iter()
            .filter(|s| spec_is_open(s) && s.claimed_by.as_deref() == Some(id.as_str()))
            .map(|s| s.id.clone())
            .collect();
        open_claims.sort();
        Self {
            open_claims,
            follow_up_id: follow_up_id(specs, now),
        }
    }
}

/// A spec id for doctor's follow-up spec that no existing spec uses.
fn follow_up_id(specs: &[crate::spec::Spec], now: chrono::DateTime<chrono::Utc>) -> String {
    let base = format!("doctor-follow-up-{}", now.format("%Y%m%d"));
    let taken = |id: &str| specs.iter().any(|s| s.id == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .unwrap_or(base)
}

fn check_unsealed_at_risk(
    root: &Path,
    ctx: &PendingContext,
    config: &DoctorConfig,
    actor: &Actor,
) -> Vec<DoctorFinding> {
    let wall = std::time::SystemTime::now();
    // Group by the owning spec so each fix is one seal.
    let mut groups: std::collections::BTreeMap<Option<String>, (Vec<String>, Vec<String>)> =
        Default::default();
    for path in &ctx.pending {
        let (agents, spec) = ctx.sealers.get(path).cloned().unwrap_or_default();
        let why = if agents.len() > 1 {
            Some(format!(
                "{path}: sealed by {} agents ({})",
                agents.len(),
                agents.iter().cloned().collect::<Vec<_>>().join(", ")
            ))
        } else {
            let others: Vec<&String> = ctx.active.iter().filter(|a| !agents.contains(*a)).collect();
            match pending_age_minutes(root, path, wall) {
                Some(age) if age > config.unsealed_minutes as i64 && !others.is_empty() => {
                    Some(format!(
                        "{path}: pending {age} min while {} active",
                        others
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                }
                _ => None,
            }
        };
        if let Some(why) = why {
            let g = groups.entry(spec).or_default();
            g.0.push(path.clone());
            g.1.push(why);
        }
    }
    groups
        .into_iter()
        .map(|(spec, (paths, whys))| {
            let shown: Vec<&str> = whys.iter().take(3).map(String::as_str).collect();
            let more = whys.len().saturating_sub(shown.len());
            let tail = if more > 0 {
                format!("; +{more} more")
            } else {
                String::new()
            };
            DoctorFinding::new(
                CHECK_UNSEALED_AT_RISK,
                Severity::Yellow,
                format!(
                    "{} unsealed file(s) at risk of being overwritten: {}{tail}",
                    paths.len(),
                    shown.join("; ")
                ),
                seal_fix(
                    actor,
                    spec.as_deref(),
                    "seal pending work flagged by writ doctor",
                    &paths,
                ),
                paths,
            )
        })
        .collect()
}

fn check_left_out(
    root: &Path,
    specs: &[crate::spec::Spec],
    ctx: &PendingContext,
    flagged: &std::collections::HashSet<String>,
    actor: &Actor,
) -> Vec<DoctorFinding> {
    if !specs.iter().any(|s| s.is_committable()) {
        return Vec::new();
    }
    let candidates: Vec<String> = ctx
        .pending
        .iter()
        .filter(|p| !flagged.contains(*p))
        .cloned()
        .collect();
    let paths = differs_from_head(root, &candidates);
    if paths.is_empty() {
        return Vec::new();
    }
    vec![DoctorFinding::new(
        CHECK_LEFT_OUT,
        Severity::Yellow,
        format!(
            "{} uncommitted, unsealed file(s) that the next finish will leave out (a human may instead commit them unaudited with `writ finish --include-unsealed`)",
            paths.len()
        ),
        seal_fix(actor, None, "seal work finish would leave out", &paths),
        paths,
    )]
}

/// The subset of `paths` whose working-tree content differs from git HEAD
/// (absent on both sides counts as equal). Finding 75: a file identical to
/// HEAD is not "left out". Without git (no `bridge` feature or no
/// repository) every path is returned.
pub fn differs_from_head(root: &Path, paths: &[String]) -> Vec<String> {
    #[cfg(feature = "bridge")]
    {
        if let Some(head) = git_head_blobs(root) {
            return paths
                .iter()
                .filter(|p| {
                    let disk = std::fs::read(root.join(p.as_str())).ok().and_then(|bytes| {
                        git2::Oid::hash_object(git2::ObjectType::Blob, &bytes).ok()
                    });
                    disk != head(p.as_str())
                })
                .cloned()
                .collect();
        }
    }
    let _ = root;
    paths.to_vec()
}

#[cfg(feature = "bridge")]
fn git_head_blobs(root: &Path) -> Option<impl Fn(&str) -> Option<git2::Oid>> {
    let repo = git2::Repository::discover(root).ok()?;
    let workdir = repo.workdir()?.canonicalize().ok()?;
    let prefix = root
        .canonicalize()
        .ok()?
        .strip_prefix(&workdir)
        .ok()?
        .to_path_buf();
    let tree_id = repo.head().ok()?.peel_to_tree().ok()?.id();
    Some(move |path: &str| {
        let tree = repo.find_tree(tree_id).ok()?;
        let entry = tree.get_path(&prefix.join(path)).ok()?;
        Some(entry.id())
    })
}

// ---------------------------------------------------------------------------
// Check 5: version skew
// ---------------------------------------------------------------------------

fn check_version_skew(root: &Path) -> Vec<DoctorFinding> {
    let mut out = Vec::new();
    let stale = crate::hooks::stale_managed_text(root);
    if !stale.is_empty() {
        out.push(DoctorFinding::new(
            CHECK_VERSION_SKEW,
            Severity::Yellow,
            format!(
                "writ-managed agent instructions are older than this writ ({})",
                env!("CARGO_PKG_VERSION")
            ),
            "writ init -y",
            stale,
        ));
    }
    let writs = writs_on_path(std::env::var_os("PATH").as_deref());
    if writs.len() > 1 {
        let installs: Vec<WritInstall> = writs.iter().map(|p| WritInstall::probe(p)).collect();
        out.push(path_skew_finding(&installs));
    }
    out
}

/// Who manages a `writ` binary, which decides the command that removes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Inside a Homebrew prefix: `brew unlink writ`.
    Homebrew,
    /// `~/.cargo/bin`: `cargo uninstall writ`.
    Cargo,
    /// A Python environment's `bin/` (the `writ-vcs` wheel); holds that
    /// environment's interpreter.
    Pip { python: std::path::PathBuf },
    /// Anything else: writ does not know who owns it.
    Unmanaged,
}

/// One `writ` on PATH.
#[derive(Debug, Clone)]
pub struct WritInstall {
    pub path: std::path::PathBuf,
    pub kind: InstallKind,
    /// `(major, minor, patch)` from `writ --version`, when it answers.
    pub version: Option<(u64, u64, u64)>,
}

impl WritInstall {
    fn probe(path: &Path) -> Self {
        let version = std::process::Command::new(path)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .ok()
            .and_then(|o| parse_version(&String::from_utf8_lossy(&o.stdout)));
        Self {
            path: path.to_path_buf(),
            kind: install_kind(path, dirs::home_dir().as_deref()),
            version,
        }
    }
}

/// `writ 0.4.0` -> (0, 4, 0).
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let word = text
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut it = word.split(['.', '-', '+']).map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// Classify a binary by where it (or its symlink target) lives.
pub fn install_kind(path: &Path, home: Option<&Path>) -> InstallKind {
    let real = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let real_s = real.to_string_lossy();
    let link_s = path.to_string_lossy();
    if ["/Cellar/", "/homebrew/", "/linuxbrew/"]
        .iter()
        .any(|m| real_s.contains(m) || link_s.contains(m))
    {
        return InstallKind::Homebrew;
    }
    if let Some(home) = home {
        if path.parent() == Some(home.join(".cargo").join("bin").as_path()) {
            return InstallKind::Cargo;
        }
    }
    if let Some(dir) = path.parent() {
        for py in ["python", "python3"] {
            let python = dir.join(py);
            if python.exists() {
                return InstallKind::Pip { python };
            }
        }
    }
    InstallKind::Unmanaged
}

/// The command that removes `install` through its manager, or None when
/// writ does not know who owns it. Never a raw file deletion.
fn removal_command(install: &WritInstall) -> Option<String> {
    match &install.kind {
        InstallKind::Homebrew => Some("brew unlink writ".to_string()),
        InstallKind::Cargo => Some("cargo uninstall writ".to_string()),
        InstallKind::Pip { python } => Some(format!(
            "{} -m pip uninstall -y writ-vcs",
            shell_quote(&python.display().to_string())
        )),
        InstallKind::Unmanaged => None,
    }
}

/// Keep the newest version (ties and unknown versions: the one PATH runs
/// first); remove every other copy through its own package manager. If any
/// copy to remove has no known manager the choice is the user's: the
/// finding has no fix_command and says so.
fn path_skew_finding(installs: &[WritInstall]) -> DoctorFinding {
    let keep = installs
        .iter()
        .enumerate()
        .max_by(|(ia, a), (ib, b)| a.version.cmp(&b.version).then(ib.cmp(ia)))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let describe = |w: &WritInstall| {
        let v = w
            .version
            .map(|(a, b, c)| format!("{a}.{b}.{c}"))
            .unwrap_or_else(|| "version unknown".into());
        format!("{} ({v})", w.path.display())
    };
    let others: Vec<&WritInstall> = installs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != keep)
        .map(|(_, w)| w)
        .collect();
    let commands: Option<Vec<String>> = others.iter().map(|w| removal_command(w)).collect();
    let mut paths: Vec<String> = installs
        .iter()
        .map(|w| w.path.display().to_string())
        .collect();
    paths.dedup();
    let listed = installs.iter().map(describe).collect::<Vec<_>>().join(", ");
    match commands {
        Some(mut cmds) if !cmds.is_empty() => {
            cmds.dedup();
            DoctorFinding::new(
                CHECK_VERSION_SKEW,
                Severity::Yellow,
                format!(
                    "{} writ binaries on PATH: {listed}; the fix keeps {}",
                    installs.len(),
                    installs[keep].path.display()
                ),
                cmds.join(" && "),
                paths,
            )
        }
        _ => DoctorFinding::new(
            CHECK_VERSION_SKEW,
            Severity::Yellow,
            format!(
                "{} writ binaries on PATH: {listed}; writ cannot tell who installed {}, so choose which to keep and remove the other yourself",
                installs.len(),
                others
                    .iter()
                    .filter(|w| w.kind == InstallKind::Unmanaged)
                    .map(|w| w.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "",
            paths,
        ),
    }
}

/// Commands doctor must never print as a fix: they delete, move or
/// truncate files writ does not own (finding 79).
const DESTRUCTIVE_VERBS: [&str; 8] = [
    "rm", "mv", "rmdir", "unlink", "truncate", "dd", "shred", "find",
];

/// True when any `&&`/`;`/`|`-separated step of `cmd` starts with a
/// destructive verb or writes with a bare `>` redirect.
pub fn is_destructive(cmd: &str) -> bool {
    cmd.split(['&', ';', '|'])
        .map(str::trim)
        .filter(|step| !step.is_empty())
        .any(|step| {
            let first = step.split_whitespace().next().unwrap_or("");
            let verb = first.rsplit('/').next().unwrap_or(first);
            DESTRUCTIVE_VERBS.contains(&verb) || step.contains(" > ") || step.starts_with('>')
        })
}

/// Distinct `writ` executables on `path`, in PATH order (resolved through
/// symlinks, so one binary linked twice counts once).
pub fn writs_on_path(path: Option<&std::ffi::OsStr>) -> Vec<std::path::PathBuf> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for dir in std::env::split_paths(path) {
        let candidate = dir.join(if cfg!(windows) { "writ.exe" } else { "writ" });
        if !is_executable(&candidate) {
            continue;
        }
        let real = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if seen.insert(real) {
            out.push(candidate);
        }
    }
    out
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

// ---------------------------------------------------------------------------
// Store scan cache
// ---------------------------------------------------------------------------

/// File under `.writ/` caching the seal part of the live set.
pub const DOCTOR_CACHE_FILE: &str = "doctor_cache.json";
const DOCTOR_CACHE_VERSION: u32 = 2;

/// Seal records are immutable and trees are content-addressed, so what a
/// seal references never changes: the cache holds the references of every
/// seal already scanned, and a run parses only seals added since. Specs,
/// workspace indexes and the pending-convergence preview are cheap and
/// change in place, so they are scanned on every run. A seal that vanished
/// (gc) or a tree object whose size or mtime moved forces a full rescan.
/// Presence is never cached: every referenced object is stat-ed each run.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreScanCache {
    version: u32,
    /// writ version that wrote the cache.
    writ: String,
    /// Seal ids whose references are in `refs`.
    seals: std::collections::BTreeSet<String>,
    refs: std::collections::BTreeMap<String, String>,
    /// Expanded tree hash -> (size, mtime ns) of its object file.
    trees: std::collections::BTreeMap<String, (u64, i64)>,
    unreadable_trees: Vec<crate::gc::UnreadableTree>,
}

impl StoreScanCache {
    fn load(path: &Path, objects: &Path, seal_ids: &std::collections::BTreeSet<String>) -> Self {
        let cached: Option<Self> = std::fs::read(path)
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok());
        cached
            .filter(|c| c.version == DOCTOR_CACHE_VERSION && c.writ == env!("CARGO_PKG_VERSION"))
            .filter(|c| c.seals.is_subset(seal_ids))
            .filter(|c| {
                c.trees
                    .iter()
                    .all(|(h, key)| object_stat(objects, h).as_ref() == Some(key))
            })
            .unwrap_or_else(|| Self {
                version: DOCTOR_CACHE_VERSION,
                writ: env!("CARGO_PKG_VERSION").to_string(),
                ..Default::default()
            })
    }
}

/// `gc::check_store`, with the seal part of the live-set walk cached (see
/// [`StoreScanCache`]).
pub fn cached_store_check(writ_dir: &Path) -> WritResult<crate::gc::StoreCheck> {
    let objects = writ_dir.join("objects");
    let seal_ids = seal_ids_on_disk(writ_dir)?;
    let cache_path = writ_dir.join(DOCTOR_CACHE_FILE);
    let mut cache = StoreScanCache::load(&cache_path, &objects, &seal_ids);

    let new_ids: Vec<String> = seal_ids.difference(&cache.seals).cloned().collect();
    if !new_ids.is_empty() {
        let mut live = crate::gc::LiveObjects::new(writ_dir);
        for id in &new_ids {
            let path = writ_dir.join("seals").join(format!("{id}.json"));
            let seal: crate::seal::Seal = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            live.add_seal(&seal);
        }
        for (h, at) in live.refs() {
            cache.refs.entry(h.clone()).or_insert_with(|| at.clone());
        }
        for h in live.expanded_trees() {
            if let Some(k) = object_stat(&objects, h) {
                cache.trees.insert(h.clone(), k);
            }
        }
        cache
            .unreadable_trees
            .extend(live.unreadable_trees().iter().cloned());
        cache.seals.extend(new_ids);
        // Advisory: a failed write only costs the next run a rescan.
        if let Ok(bytes) = serde_json::to_vec(&cache) {
            let _ = crate::fsutil::atomic_write(&cache_path, &bytes);
        }
    }

    // Roots that change in place: scanned every run.
    let mut fresh = crate::gc::LiveObjects::new(writ_dir);
    for spec in crate::gc::load_all_specs(writ_dir)? {
        fresh.add_spec(&spec);
    }
    fresh.add_workspace_indexes(writ_dir)?;
    fresh.add_pending_convergence(writ_dir)?;

    let mut refs: std::collections::BTreeMap<&String, &String> = cache.refs.iter().collect();
    for (h, at) in fresh.refs() {
        refs.entry(h).or_insert(at);
    }
    let missing_objects: Vec<crate::gc::MissingObject> = refs
        .into_iter()
        .filter(|(h, _)| object_stat(&objects, h).is_none())
        .map(|(h, at)| crate::gc::MissingObject {
            hash: h.clone(),
            referenced_as: at.clone(),
        })
        .collect();
    let missing: std::collections::HashSet<&str> =
        missing_objects.iter().map(|m| m.hash.as_str()).collect();
    let unreadable_trees = cache
        .unreadable_trees
        .iter()
        .chain(fresh.unreadable_trees())
        .filter(|t| !missing.contains(t.hash.as_str()))
        .cloned()
        .collect();
    Ok(crate::gc::StoreCheck {
        missing_objects,
        unreadable_trees,
    })
}

/// Ids of every seal record in `.writ/seals/`.
fn seal_ids_on_disk(writ_dir: &Path) -> WritResult<std::collections::BTreeSet<String>> {
    let dir = writ_dir.join("seals");
    if !dir.is_dir() {
        return Ok(Default::default());
    }
    Ok(std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect())
}

/// (size, mtime ns) of an object file, None when absent or the hash is
/// not a valid object name.
fn object_stat(objects: &Path, hash: &str) -> Option<(u64, i64)> {
    if hash.len() < 3 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let meta = std::fs::metadata(objects.join(&hash[..2]).join(&hash[2..])).ok()?;
    meta.is_file().then(|| (meta.len(), mtime_ns(&meta)))
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// `writ doctor --allow-missing <p1> <p2> ...`
pub fn allow_missing_command(prefixes: &[String]) -> String {
    format!("writ doctor --allow-missing {}", prefixes.join(" "))
}

/// The `[doctor]` config line that excuses `prefixes`.
pub fn allow_missing_config_line(prefixes: &[String]) -> String {
    let quoted: Vec<String> = prefixes.iter().map(|p| format!("\"{p}\"")).collect();
    format!("[doctor] allow_missing = [{}]", quoted.join(", "))
}

/// Add `prefixes` (12+ hex chars each) to `[doctor] allow_missing` in
/// `.writ/config.toml`, keeping the rest of the file byte for byte. The
/// result is parsed before it is written; nothing is written on error.
pub fn add_allow_missing(writ_dir: &Path, prefixes: &[String]) -> WritResult<Vec<String>> {
    for p in prefixes {
        if p.len() < 12 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(WritError::Other(format!(
                "--allow-missing takes object hash prefixes of at least 12 hex characters, got '{p}'"
            )));
        }
    }
    let path = writ_dir.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut merged = DoctorConfig::load(writ_dir)?.allow_missing;
    for p in prefixes {
        if !merged.contains(p) {
            merged.push(p.clone());
        }
    }
    let line = {
        let quoted: Vec<String> = merged.iter().map(|p| format!("\"{p}\"")).collect();
        format!("allow_missing = [{}]", quoted.join(", "))
    };
    let new_text = set_doctor_key(&text, "allow_missing", &line);
    let parsed: crate::config::ProjectConfig = toml::from_str(&new_text)
        .map_err(|e| WritError::Other(format!("refusing to write {}: {e}", path.display())))?;
    let got = parsed.doctor.unwrap_or_default().allow_missing;
    if got != merged {
        return Err(WritError::Other(format!(
            "refusing to write {}: [doctor] allow_missing would read back as {got:?}",
            path.display()
        )));
    }
    crate::fsutil::atomic_write(&path, new_text.as_bytes())?;
    Ok(merged)
}

/// `[doctor] key = ...` set in place; see [`crate::config::set_toml_key`].
fn set_doctor_key(text: &str, key: &str, line: &str) -> String {
    crate::config::set_toml_key(text, "doctor", key, line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, start: &str) -> crate::agent::session::ProcInfo {
        crate::agent::session::ProcInfo {
            pid,
            ppid: 1,
            start: start.to_string(),
            comm: "claude".to_string(),
        }
    }

    fn claimed(pid: Option<u32>, start: Option<&str>, host: Option<&str>) -> crate::spec::Spec {
        let mut spec = crate::spec::Spec::new("g".into(), "g".into(), String::new());
        spec.claimed_by = Some("ghost".into());
        spec.claimed_pid = pid;
        spec.claimed_pid_start = start.map(str::to_string);
        spec.claimed_host = host.map(str::to_string);
        spec
    }

    const START: &str = "Sun Oct 11 00:00:00 2026";

    #[test]
    fn stale_verdict_gone_when_pid_exited_or_reused_on_this_host() {
        let spec = claimed(Some(42), Some(START), Some("h"));
        let table = [proc(7, START)];
        assert!(matches!(
            stale_verdict(&spec, Some("h"), Some(&table)),
            StaleVerdict::Gone(p) if p.contains("pid 42") && p.contains("exited")
        ));
        let reused = [proc(42, "Mon Oct 12 00:00:00 2026")];
        assert!(matches!(
            stale_verdict(&spec, Some("h"), Some(&reused)),
            StaleVerdict::Gone(p) if p.contains("now belongs to")
        ));
    }

    #[test]
    fn stale_verdict_alive_when_pid_and_start_match() {
        let spec = claimed(Some(42), Some(START), Some("h"));
        let table = [proc(42, START)];
        assert!(matches!(
            stale_verdict(&spec, Some("h"), Some(&table)),
            StaleVerdict::Alive(p) if p.contains("pid 42") && p.contains("alive")
        ));
    }

    #[test]
    fn stale_verdict_unproven_names_the_missing_evidence() {
        let table = [proc(7, START)];
        let foreign = claimed(Some(42), Some(START), Some("far.invalid"));
        assert!(matches!(
            stale_verdict(&foreign, Some("h"), Some(&table)),
            StaleVerdict::Unproven(w) if w.contains("far.invalid") && w.contains("'h'")
        ));
        let bare = claimed(None, None, None);
        assert!(matches!(
            stale_verdict(&bare, Some("h"), Some(&table)),
            StaleVerdict::Unproven(w) if w.contains("no pid, no pid start time, no host")
        ));
        // A pid with no start time is not proof: the pid may have been reused.
        let no_start = claimed(Some(42), None, Some("h"));
        assert!(matches!(
            stale_verdict(&no_start, Some("h"), Some(&table)),
            StaleVerdict::Unproven(w) if w.contains("no pid start time")
        ));
        let here = claimed(Some(42), Some(START), Some("h"));
        assert!(matches!(
            stale_verdict(&here, None, Some(&table)),
            StaleVerdict::Unproven(w) if w.contains("host name cannot be read")
        ));
        assert!(matches!(
            stale_verdict(&here, Some("h"), None),
            StaleVerdict::Unproven(w) if w.contains("process table")
        ));
    }

    /// Finding 91: every stale_claim shape doctor can report (holder gone,
    /// another host, no evidence) prints `--stale` or nothing, never `--force`.
    #[test]
    fn stale_claim_findings_never_print_force() {
        let now = chrono::Utc::now();
        let old = now - chrono::Duration::hours(3);
        let mut gone = claimed(Some(u32::MAX - 1), Some(START), None);
        gone.claimed_host = crate::agent::session::host_name();
        let mut foreign = claimed(Some(42), Some(START), Some("far.invalid"));
        foreign.id = "f".into();
        let mut bare = claimed(None, None, None);
        bare.id = "b".into();
        let mut specs = vec![gone, foreign, bare];
        for s in &mut specs {
            s.last_activity = old;
            s.updated_at = old;
        }
        let found = check_stale_claims(&specs, &DoctorConfig::default(), now);
        assert_eq!(found.len(), 3, "{found:#?}");
        for f in &found {
            let fix = f.fix_command.as_deref().unwrap_or("");
            assert!(!fix.contains("--force"), "{f:?}");
            assert_eq!(f.needs_human, f.fix_command.is_none());
        }
        if crate::agent::session::host_name().is_some() && cfg!(unix) {
            assert_eq!(
                found[0].fix_command.as_deref(),
                Some("writ spec release g --stale")
            );
        }
        assert!(found[1].message.contains("far.invalid") && found[1].message.contains("--force"));
        assert!(found[2].message.contains("no pid"));
    }

    /// Finding 91: the constructor never lets `--force` through as a fix,
    /// whichever check builds it (release builds drop it to needs_human).
    #[test]
    #[cfg(not(debug_assertions))]
    fn constructor_drops_a_force_fix() {
        let f = DoctorFinding::new(
            "x",
            Severity::Yellow,
            "m",
            "writ spec release g --force",
            vec![],
        );
        assert!(f.fix_command.is_none() && f.needs_human);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "uses --force")]
    fn constructor_refuses_a_force_fix_in_debug() {
        let _ = DoctorFinding::new(
            "x",
            Severity::Yellow,
            "m",
            "writ spec release g --force",
            vec![],
        );
    }

    #[test]
    fn clean_doctor_demotes_a_high_next_action_and_keeps_the_rest() {
        let mk_action = |action: &str, p: &str| {
            Some(crate::context::RecommendedAction {
                action: action.to_string(),
                message: "m".to_string(),
                priority: p.to_string(),
            })
        };
        let mk = |p: &str| mk_action("converge", p);
        let mut blocked = mk_action("wait_for_dependency", "high");
        defer_to_clean_doctor(&mut blocked);
        assert_eq!(blocked.unwrap().priority, "high");
        let mut high = mk("high");
        defer_to_clean_doctor(&mut high);
        assert_eq!(high.unwrap().priority, "medium");
        let mut low = mk("low");
        defer_to_clean_doctor(&mut low);
        assert_eq!(low.unwrap().priority, "low");
        let mut none = None;
        defer_to_clean_doctor(&mut none);
        assert!(none.is_none());
    }

    #[test]
    fn clean_headline_is_exact() {
        assert_eq!(
            headline(0, 0),
            "fast checks clean; survival not run; run `writ doctor --survival`"
        );
    }

    /// Without a green survival result on the current state the headline
    /// never says "safe to finish" (Aubs's honesty condition).
    #[test]
    fn headlines_never_claim_safety() {
        for (r, y) in [(0, 0), (1, 0), (0, 3), (2, 2)] {
            let h = headline(r, y);
            assert!(h.starts_with("fast checks"), "{h}");
            assert!(h.contains("survival not run"));
            assert!(!h.contains(SAFE_TO_FINISH));
        }
    }

    #[test]
    fn check_ids_are_unique_and_stable() {
        let mut ids = CHECK_IDS.to_vec();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 6);
        assert_eq!(
            CHECK_IDS,
            [
                "store_integrity",
                "stale_claim",
                "committed_spec_seal",
                "unsealed_at_risk",
                "version_skew",
                "left_out"
            ]
        );
    }

    #[test]
    fn red_blocks_finish_yellow_does_not() {
        let y = DoctorFinding::new(CHECK_LEFT_OUT, Severity::Yellow, "m", "f", vec![]);
        let r = DoctorFinding::new(CHECK_STORE_INTEGRITY, Severity::Red, "m", "f", vec![]);
        let rep = DoctorReport::from_findings(vec![y.clone()], vec![], Instant::now());
        assert!(!rep.blocks_finish());
        assert!(!rep.clean);
        let rep = DoctorReport::from_findings(vec![y, r], vec![], Instant::now());
        assert!(rep.findings.iter().all(|f| !f
            .fix_command
            .as_deref()
            .unwrap_or("")
            .contains("--dry-run")));
        assert!(rep.blocks_finish());
        assert_eq!((rep.red, rep.yellow), (1, 1));
        assert!(rep.brief_line().contains("store_integrity"));
    }

    #[test]
    fn finish_refused_event_is_a_readable_security_event() {
        let dir = std::env::temp_dir().join(format!(
            "writ-doctor-ev-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        record_finish_refused(
            &dir,
            Some("amis"),
            FinishRefusal::CompileCheck,
            "cargo check failed",
            &["s1".into()],
            &["a.rs".into()],
        )
        .unwrap();
        let logger = crate::security::SecurityEventLogger::new(&dir);
        let events = logger.read_events(None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "finish_refused");
        let line = std::fs::read_to_string(dir.join("security/events.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["reason"], "compile_check");
        assert_eq!(v["specs"][0], "s1");
        assert_eq!(v["files"][0], "a.rs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_doctor_key_keeps_other_lines_and_replaces_in_place() {
        let base = "[project]\nname = \"x\"\n# keep me\n";
        let added = set_doctor_key(base, "allow_missing", "allow_missing = [\"a\"]");
        assert!(added.starts_with(base));
        assert!(added.ends_with("[doctor]\nallow_missing = [\"a\"]\n"));
        let again = set_doctor_key(&added, "allow_missing", "allow_missing = [\"a\", \"b\"]");
        assert_eq!(again.matches("[doctor]").count(), 1);
        assert!(again.contains("allow_missing = [\"a\", \"b\"]"));
        assert!(again.contains("# keep me"));
        let mid = "[doctor]\nstale_claim_minutes = 5\n[watch]\ninterval = 2\n";
        let ins = set_doctor_key(mid, "allow_missing", "allow_missing = []");
        assert_eq!(
            ins,
            "[doctor]\nallow_missing = []\nstale_claim_minutes = 5\n[watch]\ninterval = 2\n"
        );
    }

    #[test]
    fn destructive_commands_are_detected() {
        for bad in [
            "rm /opt/homebrew/bin/writ",
            "/bin/rm -f x",
            "writ seal -s x && rm y",
            "mv a b",
            "truncate -s0 f",
            "echo > f",
        ] {
            assert!(is_destructive(bad), "{bad}");
        }
        for ok in [
            "writ repair",
            "writ spec add 'a && b' --claim && writ seal -s 'x' --paths a",
            "brew unlink writ",
            "cargo uninstall writ",
            "writ doctor --allow-missing abcdefabcdef",
        ] {
            assert!(!is_destructive(ok), "{ok}");
        }
    }

    fn install(path: &str, kind: InstallKind, v: Option<(u64, u64, u64)>) -> WritInstall {
        WritInstall {
            path: path.into(),
            kind,
            version: v,
        }
    }

    #[test]
    fn path_skew_never_prints_a_destructive_fix() {
        let brew = |v| install("/opt/homebrew/bin/writ", InstallKind::Homebrew, v);
        let cargo = |v| install("/home/u/.cargo/bin/writ", InstallKind::Cargo, v);
        let pip = |v| {
            install(
                "/v/bin/writ",
                InstallKind::Pip {
                    python: "/v/bin/python".into(),
                },
                v,
            )
        };
        let other = |v| install("/work/target/release/writ", InstallKind::Unmanaged, v);

        // Older brew copy shadowed by a newer cargo build: unlink brew.
        let f = path_skew_finding(&[cargo(Some((0, 4, 0))), brew(Some((0, 3, 0)))]);
        assert_eq!(f.fix_command.as_deref(), Some("brew unlink writ"));
        // Newer brew copy: the cargo one goes.
        let f = path_skew_finding(&[cargo(Some((0, 3, 0))), brew(Some((0, 4, 0)))]);
        assert_eq!(f.fix_command.as_deref(), Some("cargo uninstall writ"));
        let f = path_skew_finding(&[brew(Some((0, 4, 0))), pip(Some((0, 3, 0)))]);
        assert_eq!(
            f.fix_command.as_deref(),
            Some("/v/bin/python -m pip uninstall -y writ-vcs")
        );
        // An unmanaged copy to remove: no command, the user decides.
        let f = path_skew_finding(&[brew(Some((0, 4, 0))), other(Some((0, 3, 0)))]);
        assert_eq!(f.fix_command, None);
        assert!(f.needs_human);
        let v = serde_json::to_value(&f).unwrap();
        assert!(v["fix_command"].is_null());
        assert_eq!(v["needs_human"], true);
        assert!(f.message.contains("choose which to keep"));
        assert_eq!(f.severity, Severity::Yellow);
        for f in [
            path_skew_finding(&[other(None), brew(None)]),
            path_skew_finding(&[brew(None), other(None)]),
        ] {
            let cmd = f.fix_command.clone().unwrap_or_default();
            assert!(!is_destructive(&cmd), "{cmd}");
        }
    }

    #[test]
    fn install_kind_by_location() {
        let home = Path::new("/home/u");
        assert_eq!(
            install_kind(Path::new("/opt/homebrew/bin/writ"), Some(home)),
            InstallKind::Homebrew
        );
        assert_eq!(
            install_kind(Path::new("/home/u/.cargo/bin/writ"), Some(home)),
            InstallKind::Cargo
        );
        assert_eq!(
            install_kind(Path::new("/nowhere/writ"), Some(home)),
            InstallKind::Unmanaged
        );
        assert_eq!(parse_version("writ 0.4.0\n"), Some((0, 4, 0)));
    }

    #[test]
    fn finding_json_shape() {
        let f = DoctorFinding::new(
            CHECK_STALE_CLAIM,
            Severity::Red,
            "m",
            "writ spec release x --stale",
            vec!["a".into()],
        );
        let v = serde_json::to_value(&f).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "check",
                "fix_command",
                "message",
                "needs_human",
                "paths",
                "severity"
            ]
        );
        assert_eq!(v["severity"], "red");
    }
}
