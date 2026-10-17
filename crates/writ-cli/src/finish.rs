//! The commit engine behind every `writ finish` path (S.2).
//!
//! `cmd_finish`, `--accept` and `--auto` all commit through [`commit_specs`],
//! so each honors the strategy (finding 37), stages only sealed content,
//! checks the staged tree before committing (finding 48), lists staged files
//! that open specs also sealed (findings 48, 49), and marks every spec whose
//! files landed as committed with the right hash.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use colored::Colorize;
use writ_core::git_ops::{Git2Ops, GitOps};
use writ_core::repo::FinishPlan;
use writ_core::spec::Spec;
use writ_core::Repository;

type CliResult<T> = Result<T, Box<dyn std::error::Error>>;

/// Default check when the project root has a `Cargo.toml`.
pub const DEFAULT_CARGO_CHECK: &str = "cargo check --workspace --quiet";

/// Options shared by every finish path.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    /// Stage drifted and unsealed working-tree content too (last commit).
    pub include_unsealed: bool,
    /// Finding 49: refuse to commit when a completing spec's own sealed
    /// blob is stale (a later seal by a spec outside this finish recorded
    /// different content). Content-based; the finish check is separate.
    pub strict: bool,
    /// Command run on each staged tree before committing; None skips.
    pub check: Option<String>,
    /// `--force`: commit even when the survival check finds lost lines
    /// (they are printed, with the seals that added them).
    pub force: bool,
    /// `--receipt <path>`: also write the full receipt markdown (the PR
    /// description form, `writ handoff --format pr`) here.
    pub receipt: Option<PathBuf>,
}

/// One commit: the specs it closes, its message, and what it stages.
struct CommitUnit {
    specs: Vec<Spec>,
    message: String,
}

/// A commit that was made, for the caller's report.
pub struct Committed {
    pub hash: String,
    pub message: String,
    pub spec_ids: Vec<String>,
    /// The receipt in this commit's body; None when it could not be built
    /// (reported on stderr; the commit is made without it).
    pub receipt: Option<writ_core::receipt::Receipt>,
}

/// The check command for this project: `[workflow] finish_check` when set
/// (empty disables), else cargo check for a Cargo project, else none.
/// `--no-check` overrides everything.
pub fn resolve_check(repo: &Repository, no_check: bool) -> Option<String> {
    if no_check {
        return None;
    }
    let config = match writ_core::config::ProjectConfig::load(repo.writ_dir()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!(
                "{} .writ/config.toml could not be read ({e}); [workflow] finish_check ignored, using the default check",
                "warning:".yellow().bold()
            );
            Default::default()
        }
    };
    match config.finish_check() {
        Some(cmd) if cmd.trim().is_empty() => None,
        Some(cmd) => Some(cmd.to_string()),
        None if repo.root().join("Cargo.toml").is_file() => Some(DEFAULT_CARGO_CHECK.to_string()),
        None => None,
    }
}

/// Commit `specs` with `strategy` ("single", "per-spec" or "grouped").
///
/// `single_message` is the message of a single-strategy commit. Commits are
/// made in order; if a staged tree fails the check, the index is reset,
/// nothing more is committed and the error names the commit that failed.
pub fn commit_specs(
    repo: &Repository,
    git: &Git2Ops,
    specs: &[Spec],
    strategy: &str,
    single_message: &str,
    opts: &CommitOptions,
) -> CliResult<Vec<Committed>> {
    let units = plan_units(repo, specs, strategy, single_message)?;
    let survival = survival_gate(repo, specs, opts.force)?;
    // The receipt's range starts after the previous finish, the same for
    // every commit of this finish.
    let receipt_after = writ_core::receipt::last_finish(&repo.list_specs()?);
    if opts.strict {
        if let Err(e) = refuse_stale(repo, specs) {
            let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
            let files: Vec<String> = repo
                .finish_plan(&ids)
                .map(|p| p.stale.iter().map(|f| f.path.clone()).collect())
                .unwrap_or_default();
            record_refusal(
                repo,
                writ_core::doctor::FinishRefusal::StrictStale,
                &e.to_string(),
                &ids,
                &files,
            );
            return Err(e);
        }
    }
    let last = units.len().saturating_sub(1);
    let mut carried: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut made = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let ids: Vec<String> = unit.specs.iter().map(|s| s.id.clone()).collect();
        let mut plan = repo.finish_plan(&ids)?;
        let already: Vec<(String, String)> = plan
            .stage
            .iter()
            .filter_map(|(p, _)| carried.get(p).map(|(_, spec)| (p.clone(), spec.clone())))
            .collect();
        plan.stage.retain(|(p, _)| !carried.contains_key(p));
        print_shared_open(&plan, opts.strict);
        crate::stage_finish_plan(repo, git, &plan, opts.include_unsealed && i == last)?;

        if !git.has_staged_changes()? {
            mark_without_new_commit(repo, git, unit, &already, &carried)?;
            continue;
        }
        if let Some(ref cmd) = opts.check {
            if let Err(e) = check_staged_tree(repo, git, cmd) {
                git.reset_index_to_head()?;
                let files: Vec<String> = plan.stage.iter().map(|(p, _)| p.clone()).collect();
                record_refusal(
                    repo,
                    writ_core::doctor::FinishRefusal::CompileCheck,
                    &e.to_string(),
                    &ids,
                    &files,
                );
                return Err(format!(
                    "{e}\nnothing committed for {} ({} earlier commit(s) kept); fix the build, seal, and finish again, or pass --no-check",
                    ids.join(", "),
                    made.len()
                )
                .into());
            }
        }
        let message = with_carried_note(&unit.message, &already);
        // 0.4.3 receipt: built from records after staging (the content is
        // the index this commit is made from), before committing.
        let receipt = match writ_core::receipt::for_finish(
            repo,
            &ids,
            receipt_after,
            chrono::Utc::now(),
            survival.clone(),
        ) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!(
                    "{} the receipt could not be built ({e}); committing without it",
                    "warning:".yellow().bold()
                );
                None
            }
        };
        let message = match &receipt {
            Some(r) => format!("{message}\n\n{}", writ_core::receipt::render_commit(r)),
            None => message,
        };
        let hash = git.commit(&message)?;
        let receipt = receipt.map(|mut r| {
            r.commit = Some(hash.clone());
            let record = writ_core::receipt::ReceiptRecord {
                commit: hash.clone(),
                specs: ids.clone(),
                after: r.after,
                until: r.until,
                survival: r.survival.clone(),
            };
            if let Err(e) = writ_core::receipt::write_record(repo.writ_dir(), &record) {
                eprintln!(
                    "{} could not record the receipt for {} in .writ/receipts: {e}",
                    "warning:".yellow().bold(),
                    &hash[..hash.len().min(8)]
                );
            }
            r
        });
        for (path, _) in &plan.stage {
            let owner = unit
                .specs
                .iter()
                .find(|s| spec_sealed(repo, s, path))
                .map(|s| s.id.clone())
                .unwrap_or_else(|| ids[0].clone());
            carried.insert(path.clone(), (hash.clone(), owner));
        }
        for id in &ids {
            mark_committed(repo, id, &hash);
        }
        made.push(Committed {
            hash,
            message,
            spec_ids: ids,
            receipt,
        });
    }
    if let Some(path) = &opts.receipt {
        write_receipt_file(path, &made)?;
    }
    Ok(made)
}

/// The survival tier before finish (0.4.1): every line the committing
/// specs sealed must be in the tree this finish commits (their staged
/// sealed content, HEAD elsewhere), or a later seal must have replaced it.
/// Lost lines refuse the finish before anything is staged, naming each
/// line and the seal that added it, and record a `finish_refused` event
/// (`survival_check`), unless `force`.
pub fn survival_gate(
    repo: &Repository,
    specs: &[Spec],
    force: bool,
) -> CliResult<writ_core::receipt::SurvivalLine> {
    use writ_core::convergence::survival::{audit_finish_plan, describe_lost};
    use writ_core::receipt::SurvivalLine;
    let ids: Vec<String> = specs
        .iter()
        .filter(|s| !s.sealed_by.is_empty())
        .map(|s| s.id.clone())
        .collect();
    if ids.is_empty() {
        return Ok(SurvivalLine::nothing_sealed());
    }
    print_override_context(repo, specs);
    let started = Instant::now();
    let report = audit_finish_plan(repo, &ids)?;
    // Findings 113, 116: a sealed file git and writ both ignore is not
    // staged; finish says what `writ doctor --survival` says about it.
    for note in writ_core::convergence::survival::info_notes(&report) {
        println!("  {} survival: {note}", "[info]".cyan());
    }
    let undone: Vec<String> = report
        .deleted_but_present()
        .map(|(spec, f)| format!("{} (deletion sealed by {spec})", f.path))
        .collect();
    if report.lost == 0 && undone.is_empty() {
        println!(
            "  {} survival: {} sealed line(s) of {} spec(s) checked, none lost ({} ms)",
            "✓".green(),
            report.checked,
            ids.len(),
            started.elapsed().as_millis()
        );
        return Ok(SurvivalLine::from_report(&report, ids.len(), false));
    }
    let mut lines: Vec<String> = report
        .lost_lines()
        .map(|(spec, path, l)| format!("{spec}: {}", describe_lost(path, l)))
        .collect();
    lines.extend(undone);
    println!(
        "{}",
        format!(
            "Survival check: this finish would lose {} sealed line(s):",
            lines.len()
        )
        .red()
        .bold()
    );
    for l in &lines {
        println!("  {} {l}", "·".red());
    }
    if force {
        eprintln!(
            "{} --force: finishing without {} sealed line(s) listed above",
            "warning:".yellow().bold(),
            lines.len()
        );
        return Ok(SurvivalLine::from_report(&report, ids.len(), true));
    }
    let mut files: Vec<String> = report
        .lost_lines()
        .map(|(_, p, _)| p.to_string())
        .chain(report.deleted_but_present().map(|(_, f)| f.path.clone()))
        .collect();
    files.sort();
    files.dedup();
    record_refusal(
        repo,
        writ_core::doctor::FinishRefusal::SurvivalCheck,
        &format!("survival_check: {}", lines.join("; ")),
        &ids,
        &files,
    );
    Err(format!(
        "finish refused: {} sealed line(s) would be lost, nothing staged:\n  {}\nrestore them and seal (`writ show <seal> --diff` shows each), or pass --force to finish without them",
        lines.len(),
        lines.join("\n  ")
    )
    .into())
}

/// Write the full receipt markdown of each commit made (`--receipt`).
fn write_receipt_file(path: &Path, made: &[Committed]) -> CliResult<()> {
    let receipts: Vec<writ_core::receipt::Receipt> =
        made.iter().filter_map(|c| c.receipt.clone()).collect();
    if receipts.is_empty() {
        println!(
            "  {} no commit made, no receipt written to {}",
            "·".dimmed(),
            path.display()
        );
        return Ok(());
    }
    std::fs::write(path, writ_core::receipt::render_all(&receipts, true))
        .map_err(|e| format!("could not write the receipt to {}: {e}", path.display()))?;
    println!("  {} receipt written to {}", "✓".green(), path.display());
    Ok(())
}

/// Finding 102: each removal a committing spec confirmed after the
/// seal-time refusal, as the record states it.
fn print_override_context(repo: &Repository, specs: &[Spec]) {
    for spec in specs {
        for id in &spec.sealed_by {
            let Ok(seal) = repo.load_seal(id) else {
                continue;
            };
            for o in &seal.override_context {
                // Finding 122: one wording, from the record (seal.rs).
                // Ruling 177: a move says moved.
                let line = writ_core::convergence::survival::describe_override(repo, &seal, o).1;
                println!("  {} {line}", "note:".cyan());
            }
        }
    }
}

/// Record a `finish_refused` event (finding 76). Every finish refusal goes
/// through here, including doctor's. A failed write is reported on stderr;
/// the refusal itself still happens.
pub fn record_refusal(
    repo: &Repository,
    reason: writ_core::doctor::FinishRefusal,
    details: &str,
    specs: &[String],
    files: &[String],
) {
    let agent = writ_core::agent::resolve_agent_id_in(
        None,
        repo.settings().default_agent.as_deref(),
        true,
        Some(repo.writ_dir()),
    )
    .id;
    if let Err(e) = writ_core::doctor::record_finish_refused(
        repo.writ_dir(),
        Some(&agent),
        reason,
        details,
        specs,
        files,
    ) {
        eprintln!(
            "{} could not record the finish refusal in .writ/security/events.jsonl: {e}",
            "warning:".yellow().bold()
        );
    }
}

/// Split `specs` into commits for `strategy`.
fn plan_units(
    repo: &Repository,
    specs: &[Spec],
    strategy: &str,
    single_message: &str,
) -> CliResult<Vec<CommitUnit>> {
    match strategy {
        "single" => Ok(vec![CommitUnit {
            specs: specs.to_vec(),
            message: single_message.to_string(),
        }]),
        "per-spec" => {
            let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
            let order = repo.finish_order(&ids)?;
            Ok(order
                .iter()
                .filter_map(|id| specs.iter().find(|s| &s.id == id))
                .map(|s| CommitUnit {
                    specs: vec![s.clone()],
                    message: spec_message(s),
                })
                .collect())
        }
        "grouped" => {
            let refs: Vec<&Spec> = specs.iter().collect();
            Ok(crate::compute_spec_groups(&refs)
                .into_iter()
                .map(|g| {
                    let lines: Vec<String> = g.specs.iter().map(|s| spec_message(s)).collect();
                    let message = if lines.len() == 1 {
                        lines[0].clone()
                    } else {
                        format!("{}\n\n{}", g.label, lines.join("\n"))
                    };
                    CommitUnit {
                        specs: g.specs.into_iter().cloned().collect(),
                        message,
                    }
                })
                .collect())
        }
        other => Err(format!("unknown strategy '{other}'. Use: single, per-spec, grouped").into()),
    }
}

fn spec_message(s: &Spec) -> String {
    format!(
        "{}: {}",
        s.id,
        s.completion_summary.as_deref().unwrap_or(&s.title)
    )
}

/// Append the files this commit's specs also sealed that an earlier
/// commit of this finish already carried.
fn with_carried_note(message: &str, already: &[(String, String)]) -> String {
    if already.is_empty() {
        return message.to_string();
    }
    let mut out = format!("{message}\n\nAlso sealed here, committed earlier with:");
    for (path, spec) in already {
        out.push_str(&format!("\n  {path} ({spec})"));
    }
    out
}

/// A unit staged nothing new: its files went with earlier commits of this
/// finish, or their sealed content is already in HEAD. The specs still
/// landed, so mark them with the commit that carried their newest file,
/// else HEAD.
fn mark_without_new_commit(
    repo: &Repository,
    git: &Git2Ops,
    unit: &CommitUnit,
    already: &[(String, String)],
    carried: &BTreeMap<String, (String, String)>,
) -> CliResult<()> {
    let hash = already
        .iter()
        .filter_map(|(p, _)| carried.get(p).map(|(h, _)| h.clone()))
        .next_back()
        .or(git.head_hash()?);
    let Some(hash) = hash else {
        return Ok(());
    };
    for s in &unit.specs {
        mark_committed(repo, &s.id, &hash);
        println!(
            "  {} {} — nothing new to commit; marked committed at {}",
            "·".dimmed(),
            s.id,
            &hash[..hash.len().min(8)]
        );
    }
    Ok(())
}

fn spec_sealed(repo: &Repository, spec: &Spec, path: &str) -> bool {
    repo.spec_seals(&spec.id)
        .map(|seals| {
            seals
                .iter()
                .any(|s| s.changes.iter().any(|c| c.path == path))
        })
        .unwrap_or(false)
}

/// Mark a spec committed; a failure is reported, never swallowed.
fn mark_committed(repo: &Repository, spec_id: &str, hash: &str) {
    if let Err(e) = repo.mark_spec_committed(spec_id, hash) {
        eprintln!(
            "{} could not mark spec {spec_id} committed at {}: {e}",
            "warning:".yellow().bold(),
            &hash[..hash.len().min(8)]
        );
    }
}

/// `--strict` (finding 49): fail, listing each path with the later spec
/// and seal, when any completing spec's own blob is stale. Specs in the
/// same finish do not count against each other.
fn refuse_stale(repo: &Repository, specs: &[Spec]) -> CliResult<()> {
    let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
    let plan = repo.finish_plan(&ids)?;
    if plan.stale.is_empty() {
        println!(
            "  {} --strict: every completed spec's sealed version is current",
            "✓".green()
        );
        return Ok(());
    }
    let mut lines = Vec::new();
    for f in &plan.stale {
        let later: Vec<String> = f
            .later
            .iter()
            .map(|(spec, seal)| format!("{spec} seal {}", &seal[..seal.len().min(12)]))
            .collect();
        lines.push(format!(
            "  {} (completed spec {}) superseded by {}",
            f.path,
            f.spec_id,
            later.join(", ")
        ));
    }
    Err(format!(
        "--strict: {} file(s) have newer sealed content than the completed spec's own version; nothing committed:\n{}\nfinish without --strict to commit the newest sealed version, or reopen the spec and seal on top",
        plan.stale.len(),
        lines.join("\n")
    )
    .into())
}

/// List staged files an open spec also sealed (findings 48, 49).
pub fn print_shared_open(plan: &FinishPlan, strict: bool) {
    let short = |s: &str| s[..s.len().min(12)].to_string();
    let open = |f: &writ_core::repo::SharedOpenFile| {
        f.open_specs
            .iter()
            .map(|(spec, seal)| format!("{spec} seal {}", short(seal)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let newer: Vec<_> = plan.newer_from_open().collect();
    if !newer.is_empty() {
        println!();
        let title = if strict {
            "Newer content sealed under open specs"
        } else {
            "Includes newer content sealed under open specs (--strict refuses this)"
        };
        println!("{}", format!("{title} ({}):", newer.len()).yellow().bold());
        for f in &newer {
            println!(
                "  {}  {} — completed spec {}, newer from {}",
                "·".yellow(),
                f.path,
                f.spec_id,
                open(f)
            );
        }
    }
    let same: Vec<_> = plan.shared_open.iter().filter(|f| !f.is_newer()).collect();
    if !same.is_empty() {
        println!();
        println!(
            "{}",
            format!(
                "Staged files also sealed under open specs (same content) ({}):",
                same.len()
            )
            .yellow()
            .bold()
        );
        for f in &same {
            println!("  {}  {} — also {}", "·".yellow(), f.path, open(f));
        }
    }
}

/// Run `cmd` on a scratch copy of the staged tree (finding 48).
fn check_staged_tree(repo: &Repository, git: &Git2Ops, cmd: &str) -> CliResult<()> {
    let dir = scratch_dir(repo.root());
    let files = git.export_index(&dir)?;
    println!(
        "  {} checking staged tree ({files} files): {}",
        "→".dimmed(),
        cmd.bold()
    );
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.args(["-c", cmd]).current_dir(&dir);
    if std::env::var_os("CARGO_TARGET_DIR").is_none() {
        command.env("CARGO_TARGET_DIR", repo.root().join("target"));
    }
    let output = command
        .output()
        .map_err(|e| format!("could not run finish check `{cmd}`: {e}"))?;
    if output.status.success() {
        println!(
            "  {} staged tree checks clean ({:.1}s)",
            "✓".green(),
            started.elapsed().as_secs_f64()
        );
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let tail: Vec<&str> = stderr.lines().chain(stdout.lines()).collect();
    let shown = &tail[tail.len().saturating_sub(40)..];
    Err(format!(
        "the staged tree fails `{cmd}` ({}), so it was not committed:\n{}",
        output.status,
        shown.join("\n")
    )
    .into())
}

/// Scratch directory for the staged-tree check, stable per project so
/// incremental builds stay incremental across finishes.
fn scratch_dir(root: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    std::env::temp_dir().join(format!("writ-finish-check-{:016x}", hasher.finish()))
}
