//! writ CLI — the human (and agent) interface to writ.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use writ_core::context::{ContextFilter, ContextScope};
use writ_core::diff::LineOp;
use writ_core::seal::{AgentIdentity, AgentType, ChangeType, TaskStatus, Verification};
use writ_core::spec::{Spec, SpecUpdate};
use writ_core::Repository;

#[derive(Parser)]
#[command(name = "writ", about = "writ — AI-native version control", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new writ repository.
    Init,

    /// One-command setup: init + detect git + import baseline.
    Install {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show working directory state.
    State {
        /// Output format: "human" (default), "json", or "brief".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Create a seal (structured checkpoint) from current changes.
    Seal {
        /// Summary of what changed and why.
        #[arg(long, short)]
        summary: String,

        /// Agent or human identifier. Defaults to "human" (sets agent_type=human).
        /// Any other value sets agent_type=agent.
        #[arg(long, default_value = "human")]
        agent: String,

        /// Linked spec ID.
        #[arg(long)]
        spec: Option<String>,

        /// Task status: in-progress, complete, or blocked.
        #[arg(long, default_value = "in-progress")]
        status: String,

        /// Seal only these paths (comma-separated). Remaining changes stay pending.
        #[arg(long, value_delimiter = ',')]
        paths: Option<Vec<String>>,

        /// Number of tests that passed.
        #[arg(long)]
        tests_passed: Option<u32>,

        /// Number of tests that failed.
        #[arg(long)]
        tests_failed: Option<u32>,

        /// Whether the code was linted.
        #[arg(long)]
        linted: bool,

        /// Allow sealing with no file changes (e.g. metadata-only updates).
        #[arg(long)]
        allow_empty: bool,

        /// Expected HEAD seal ID (for optimistic conflict detection).
        #[arg(long)]
        expected_head: Option<String>,
    },

    /// Inspect a specific seal.
    Show {
        /// Seal ID (supports short prefix).
        seal_id: String,

        /// Include the diff introduced by this seal.
        #[arg(long)]
        diff: bool,

        /// Output format: "human" (default), "json", or "brief".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show seal history.
    Log {
        /// Output format: "human" (default), "json", or "brief".
        #[arg(long, default_value = "human")]
        format: String,

        /// Maximum number of seals to show.
        #[arg(long, short)]
        limit: Option<usize>,

        /// Show seals for a specific spec (uses spec-scoped head).
        #[arg(long)]
        spec: Option<String>,

        /// Show seals from ALL branches (global + spec heads), not just HEAD.
        /// Useful for seeing work by agents on diverged branches.
        #[arg(long)]
        all: bool,
    },

    /// Show what changed (content-level diff).
    Diff {
        /// First seal ID (for seal-to-seal diff).
        #[arg(long)]
        from: Option<String>,

        /// Second seal ID (for seal-to-seal diff).
        #[arg(long)]
        to: Option<String>,

        /// Output format: "human" (default), "json", or "brief".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Dump structured context for LLM consumption.
    Context {
        /// Scope to a specific spec ID.
        #[arg(long)]
        spec: Option<String>,

        /// Maximum number of recent seals to include.
        #[arg(long, default_value = "10")]
        seal_limit: usize,

        /// Filter seals by task status (in-progress, complete, blocked).
        #[arg(long)]
        status: Option<String>,

        /// Filter seals by agent ID.
        #[arg(long)]
        agent: Option<String>,

        /// Output format: "json" (default), "human", or "brief".
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// Restore working directory to a specific seal's state.
    Restore {
        /// Seal ID to restore to (supports short prefix).
        seal_id: String,

        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Analyze convergence between two specs (three-way merge).
    Converge {
        /// Left spec ID.
        left_spec: String,

        /// Right spec ID.
        right_spec: String,

        /// Output format: "human" (default), "json", or "brief".
        #[arg(long, default_value = "human")]
        format: String,

        /// Apply the convergence result to the working directory (clean merges only).
        #[arg(long)]
        apply: bool,
    },

    /// Manage specs (requirements).
    Spec {
        #[command(subcommand)]
        action: SpecCommands,
    },

    /// Bridge between git and writ.
    Bridge {
        #[command(subcommand)]
        action: BridgeCommands,
    },

    /// Push local state to a remote.
    Push {
        /// Remote name (default: origin).
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Pull remote state into local.
    Pull {
        /// Remote name (default: origin).
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Manage remotes.
    Remote {
        #[command(subcommand)]
        action: RemoteCommands,
    },
}

#[derive(Subcommand)]
enum SpecCommands {
    /// Register a new spec.
    Add {
        /// Unique spec identifier.
        #[arg(long)]
        id: String,

        /// Spec title.
        #[arg(long)]
        title: String,

        /// Spec description.
        #[arg(long, default_value = "")]
        description: String,

        /// Acceptance criteria (repeat for multiple).
        #[arg(long)]
        acceptance_criteria: Option<Vec<String>>,

        /// Design notes (repeat for multiple).
        #[arg(long)]
        design_notes: Option<Vec<String>>,

        /// Tech stack (comma-separated).
        #[arg(long, value_delimiter = ',')]
        tech_stack: Option<Vec<String>>,
    },

    /// Show all specs and their status.
    Status,

    /// Show details of a single spec.
    Show {
        /// Spec ID to show.
        id: String,
    },

    /// Update a spec's status or metadata.
    Update {
        /// Spec ID to update.
        id: String,

        /// New status: pending, in-progress, complete, blocked.
        #[arg(long)]
        status: Option<String>,

        /// Replacement dependency list (comma-separated spec IDs).
        #[arg(long, value_delimiter = ',')]
        depends_on: Option<Vec<String>>,

        /// Replacement file scope list (comma-separated paths).
        #[arg(long, value_delimiter = ',')]
        file_scope: Option<Vec<String>>,

        /// Replacement acceptance criteria (repeat for multiple).
        #[arg(long)]
        acceptance_criteria: Option<Vec<String>>,

        /// Replacement design notes (repeat for multiple).
        #[arg(long)]
        design_notes: Option<Vec<String>>,

        /// Replacement tech stack (comma-separated).
        #[arg(long, value_delimiter = ',')]
        tech_stack: Option<Vec<String>>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

#[derive(Subcommand)]
enum BridgeCommands {
    /// Import git state as a writ baseline seal.
    Import {
        /// Git ref to import (default: HEAD).
        #[arg(long, default_value = "HEAD")]
        git_ref: String,

        /// Agent identifier for the import seal.
        #[arg(long, default_value = "bridge")]
        agent: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Export writ seals as git commits.
    Export {
        /// Git branch to create commits on (default: writ/export).
        #[arg(long, default_value = "writ/export")]
        branch: String,

        /// Print a structured PR body summarizing the exported seals.
        #[arg(long)]
        pr_body: bool,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show bridge sync status.
    Status {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

#[derive(Subcommand)]
enum RemoteCommands {
    /// Initialize a bare remote directory.
    Init {
        /// Path to create the remote at.
        path: PathBuf,
    },

    /// Add a named remote.
    Add {
        /// Remote name (e.g. "origin").
        name: String,

        /// Filesystem path to the bare remote directory.
        path: String,
    },

    /// Remove a named remote.
    Remove {
        /// Remote name to remove.
        name: String,
    },

    /// List configured remotes.
    List,

    /// Show sync status with a remote.
    Status {
        /// Remote name (default: origin).
        #[arg(long, default_value = "origin")]
        remote: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        process::exit(1);
    });

    let result = match cli.command {
        Commands::Init => cmd_init(&cwd),
        Commands::Install { format } => cmd_install(&cwd, &format),
        Commands::State { format } => cmd_state(&cwd, &format),
        Commands::Seal {
            summary,
            agent,
            spec,
            status,
            paths,
            tests_passed,
            tests_failed,
            linted,
            allow_empty,
            expected_head,
        } => cmd_seal(&cwd, &summary, &agent, spec, &status, paths, tests_passed, tests_failed, linted, allow_empty, expected_head),
        Commands::Show {
            seal_id,
            diff,
            format,
        } => cmd_show(&cwd, &seal_id, diff, &format),
        Commands::Log { format, limit, spec, all } => cmd_log(&cwd, &format, limit, spec, all),
        Commands::Diff { from, to, format } => cmd_diff(&cwd, from, to, &format),
        Commands::Context {
            spec,
            seal_limit,
            status,
            agent,
            format,
        } => cmd_context(&cwd, spec, seal_limit, status, agent, &format),
        Commands::Restore {
            seal_id,
            force,
            format,
        } => cmd_restore(&cwd, &seal_id, force, &format),
        Commands::Converge {
            left_spec,
            right_spec,
            format,
            apply,
        } => cmd_converge(&cwd, &left_spec, &right_spec, &format, apply),
        Commands::Spec { action } => match action {
            SpecCommands::Add {
                id,
                title,
                description,
                acceptance_criteria,
                design_notes,
                tech_stack,
            } => cmd_spec_add(
                &cwd,
                &id,
                &title,
                &description,
                acceptance_criteria,
                design_notes,
                tech_stack,
            ),
            SpecCommands::Status => cmd_spec_status(&cwd),
            SpecCommands::Show { id } => cmd_spec_show(&cwd, &id),
            SpecCommands::Update {
                id,
                status,
                depends_on,
                file_scope,
                acceptance_criteria,
                design_notes,
                tech_stack,
                format,
            } => cmd_spec_update(
                &cwd,
                &id,
                status,
                depends_on,
                file_scope,
                acceptance_criteria,
                design_notes,
                tech_stack,
                &format,
            ),
        },
        Commands::Bridge { action } => match action {
            BridgeCommands::Import {
                git_ref,
                agent,
                format,
            } => cmd_bridge_import(&cwd, &git_ref, &agent, &format),
            BridgeCommands::Export { branch, pr_body, format } => {
                cmd_bridge_export(&cwd, &branch, pr_body, &format)
            }
            BridgeCommands::Status { format } => cmd_bridge_status(&cwd, &format),
        },
        Commands::Push { remote, format } => cmd_push(&cwd, &remote, &format),
        Commands::Pull { remote, format } => cmd_pull(&cwd, &remote, &format),
        Commands::Remote { action } => match action {
            RemoteCommands::Init { path } => cmd_remote_init(&path),
            RemoteCommands::Add { name, path } => cmd_remote_add(&cwd, &name, &path),
            RemoteCommands::Remove { name } => cmd_remote_remove(&cwd, &name),
            RemoteCommands::List => cmd_remote_list(&cwd),
            RemoteCommands::Status { remote, format } => {
                cmd_remote_status(&cwd, &remote, &format)
            }
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

fn cmd_init(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Repository::init(cwd)?;
    println!("initialized writ repository in .writ/");
    Ok(())
}

fn cmd_install(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let result = Repository::install(cwd)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            // Init status
            if result.initialized {
                println!("initialized writ repository in .writ/");
            } else {
                println!("writ repository already exists");
            }

            // .writignore
            if result.writignore_created {
                println!("created .writignore");
            }

            // Git status
            if result.git_detected {
                let branch = result.git_branch.as_deref().unwrap_or("(detached)");
                let head = result.git_head_short.as_deref().unwrap_or("unknown");
                println!("git: {} @ {}", branch, head);

                if let Some(true) = result.git_dirty {
                    let count = result.git_dirty_count.unwrap_or(0);
                    eprintln!(
                        "warning: git working tree has {} uncommitted change(s)",
                        count
                    );
                }
            } else {
                println!("no git repository detected");
            }

            // Import status
            if result.git_imported {
                let seal_short = result
                    .imported_seal_id
                    .as_deref()
                    .map(|s| &s[..12.min(s.len())])
                    .unwrap_or("?");
                let files = result.imported_files.unwrap_or(0);

                if result.reimported {
                    println!("re-imported git baseline: {} file(s), seal {}", files, seal_short);
                } else {
                    println!("imported git baseline: {} file(s), seal {}", files, seal_short);
                }
            } else if result.already_imported {
                println!("git baseline already synced");
            } else if let Some(ref reason) = result.import_skipped_reason {
                println!("import skipped: {}", reason);
            }

            if let Some(ref err) = result.import_error {
                eprintln!("import error: {}", err);
            }

            // Tracked files
            println!("tracked: {} file(s)", result.tracked_files);

            // Frameworks
            let detected: Vec<_> = result
                .frameworks_detected
                .iter()
                .filter(|f| f.detected)
                .collect();
            for f in &detected {
                println!(
                    "detected {:?} ({})",
                    f.framework,
                    f.indicators.join(", ")
                );
            }

            // Hooks
            for hook in &result.hooks_installed {
                for f in &hook.files_created {
                    println!("  + {f}");
                }
                for f in &hook.files_updated {
                    println!("  ~ {f}");
                }
            }

            // Next steps
            println!();
            println!("ready. next steps:");
            for op in &result.available_operations {
                println!("  {}", op);
            }
        }
    }

    Ok(())
}

fn cmd_state(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let state = repo.state()?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&state)?);
        }
        "brief" => {
            println!("{}", state.brief());
        }
        _ => {
            if state.is_clean() {
                println!("nothing to seal — working directory clean");
                println!("  tracked: {} file(s)", state.tracked_count);
            } else {
                println!(
                    "{} change(s) detected ({} tracked):\n",
                    state.changes.len(),
                    state.tracked_count
                );
                for f in &state.changes {
                    let marker = match f.status {
                        writ_core::state::FileStatus::New => "+  new",
                        writ_core::state::FileStatus::Modified => "~  mod",
                        writ_core::state::FileStatus::Deleted => "-  del",
                    };
                    println!("  {marker}  {}", f.path);
                }
            }
        }
    }

    Ok(())
}

fn cmd_seal(
    cwd: &PathBuf,
    summary: &str,
    agent_id: &str,
    spec_id: Option<String>,
    status: &str,
    paths: Option<Vec<String>>,
    tests_passed: Option<u32>,
    tests_failed: Option<u32>,
    linted: bool,
    allow_empty: bool,
    expected_head: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let agent = AgentIdentity {
        id: agent_id.to_string(),
        agent_type: if agent_id == "human" {
            AgentType::Human
        } else {
            AgentType::Agent
        },
    };

    let task_status = match status {
        "in-progress" => TaskStatus::InProgress,
        "blocked" => TaskStatus::Blocked,
        "complete" => TaskStatus::Complete,
        other => {
            eprintln!("WARNING: unknown status '{other}', using 'in-progress'");
            TaskStatus::InProgress
        }
    };

    let verification = Verification {
        tests_passed,
        tests_failed,
        linted,
    };

    let (seal, conflict_warning) = if let Some(paths) = paths {
        let s = repo.seal_paths(
            agent,
            summary.to_string(),
            spec_id,
            task_status,
            verification,
            &paths,
            allow_empty,
        )?;
        (s, None)
    } else if expected_head.is_some() {
        repo.seal_with_check(
            agent,
            summary.to_string(),
            spec_id,
            task_status,
            verification,
            allow_empty,
            expected_head,
        )?
    } else {
        let s = repo.seal(
            agent,
            summary.to_string(),
            spec_id,
            task_status,
            verification,
            allow_empty,
        )?;
        (s, None)
    };

    println!("sealed {}", &seal.id[..12]);

    if let Some(ref w) = conflict_warning {
        if w.is_clean {
            println!("  note: HEAD moved ({} intervening seal(s)), but no file overlap",
                w.intervening_seals.len());
        } else {
            println!("  WARNING: HEAD moved, {} overlapping file(s):",
                w.overlapping_files.len());
            for f in &w.overlapping_files {
                println!("    ! {f}");
            }
            println!("  consider running `writ converge` to reconcile");
        }
    }
    println!("  summary: {}", seal.summary);
    if seal.verification.tests_passed.is_some() || seal.verification.tests_failed.is_some() || seal.verification.linted {
        print!("  verified:");
        if let Some(p) = seal.verification.tests_passed {
            print!(" {p} passed");
        }
        if let Some(f) = seal.verification.tests_failed {
            print!(" {f} failed");
        }
        if seal.verification.linted {
            print!(" linted");
        }
        println!();
    }
    println!("  changes: {} file(s)", seal.changes.len());
    for c in &seal.changes {
        let marker = match c.change_type {
            ChangeType::Added => "+",
            ChangeType::Modified => "~",
            ChangeType::Deleted => "-",
        };
        println!("    {marker} {}", c.path);
    }

    if let Some(ref sid) = seal.spec_id {
        let changed: Vec<String> = seal.changes.iter().map(|c| c.path.clone()).collect();
        if let Some(w) = repo.check_file_scope(sid, &changed) {
            eprintln!("  WARNING: {} file(s) outside spec '{}' file_scope {:?}:",
                w.out_of_scope_files.len(), w.spec_id, w.declared_scope);
            for f in &w.out_of_scope_files {
                eprintln!("    ! {f}");
            }
        }

        if seal.status == TaskStatus::Complete {
            let prior_seals = repo.spec_log(sid).unwrap_or_default();
            let has_in_progress = prior_seals.iter().any(|s| {
                s.id != seal.id && s.status == TaskStatus::InProgress
            });
            if !has_in_progress && prior_seals.len() <= 1 {
                eprintln!("  HINT: This is the only seal for spec '{sid}' and it's marked 'complete'.");
                eprintln!("        Consider using --status in-progress for intermediate work,");
                eprintln!("        reserving --status complete for the final checkpoint.");
            }
        }
    }

    Ok(())
}

fn cmd_log(
    cwd: &PathBuf,
    format: &str,
    limit: Option<usize>,
    spec: Option<String>,
    all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let mut seals = match (&spec, all) {
        (Some(spec_id), _) => repo.spec_log(spec_id)?,
        (None, true) => repo.log_all()?,
        (None, false) => repo.log()?,
    };

    if let Some(n) = limit {
        seals.truncate(n);
    }

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&seals)?);
        }
        "brief" => {
            for seal in &seals {
                let spec_part = seal
                    .spec_id
                    .as_deref()
                    .map(|s| format!(" spec:{s}"))
                    .unwrap_or_default();
                println!(
                    "{} {} {}{}",
                    &seal.id[..12],
                    seal.agent.id,
                    seal.summary,
                    spec_part
                );
            }
        }
        _ => {
            if seals.is_empty() {
                println!("no seals yet");
                return Ok(());
            }

            for (i, seal) in seals.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("seal {}", &seal.id[..12]);
                println!("  agent:   {}", seal.agent.id);
                println!(
                    "  time:    {}",
                    seal.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
                );
                if let Some(ref spec) = seal.spec_id {
                    println!("  spec:    {spec}");
                }
                println!("  status:  {:?}", seal.status);
                println!("  summary: {}", seal.summary);
                println!("  changes: {} file(s)", seal.changes.len());
            }
        }
    }

    print_diverged_branch_warnings(&repo);

    Ok(())
}

fn print_diverged_branch_warnings(repo: &Repository) {
    if let Ok(diverged) = repo.diverged_branches() {
        if !diverged.is_empty() {
            let total_seals: usize = diverged.iter().map(|b| b.seal_count).sum();
            eprintln!();
            eprintln!(
                "  WARNING: {} diverged branch(es) with {} seal(s) not reachable from HEAD:",
                diverged.len(),
                total_seals,
            );
            for b in &diverged {
                eprintln!(
                    "    branch '{}': {} seal(s) by {} (tip: {})",
                    b.spec_id,
                    b.seal_count,
                    b.agents.join(", "),
                    b.tip_seal,
                );
            }
            eprintln!("  Run 'writ converge' to merge, or 'writ log --spec <id>' to inspect.");
            eprintln!();
        }
    }
}

fn cmd_diff(
    cwd: &PathBuf,
    from: Option<String>,
    to: Option<String>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let diff_output = match (from, to) {
        (Some(f), Some(t)) => repo.diff_seals(&f, &t)?,
        (None, None) => repo.diff()?,
        _ => {
            return Err("must provide both --from and --to, or neither".into());
        }
    };

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&diff_output)?);
        }
        "brief" => {
            if diff_output.files.is_empty() {
                println!("no changes");
            } else {
                println!(
                    "{} file(s) changed, {} addition(s), {} deletion(s)",
                    diff_output.files_changed,
                    diff_output.total_additions,
                    diff_output.total_deletions,
                );
                for f in &diff_output.files {
                    let marker = match f.change_type {
                        ChangeType::Added => "+",
                        ChangeType::Modified => "~",
                        ChangeType::Deleted => "-",
                    };
                    println!(
                        "  {marker} {} (+{}, -{})",
                        f.path, f.additions, f.deletions
                    );
                }
            }
        }
        _ => {
            // Human-readable unified diff format
            if diff_output.files.is_empty() {
                println!("no changes");
            } else {
                for file_diff in &diff_output.files {
                    if file_diff.is_binary {
                        println!("Binary file {} differs", file_diff.path);
                        continue;
                    }

                    println!("--- a/{}", file_diff.path);
                    println!("+++ b/{}", file_diff.path);

                    for hunk in &file_diff.hunks {
                        println!(
                            "@@ -{},{} +{},{} @@",
                            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                        );
                        for line in &hunk.lines {
                            let prefix = match line.op {
                                LineOp::Add => "+",
                                LineOp::Remove => "-",
                                LineOp::Context => " ",
                            };
                            println!("{prefix}{}", line.content);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn cmd_context(
    cwd: &PathBuf,
    spec: Option<String>,
    seal_limit: usize,
    status: Option<String>,
    agent: Option<String>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let scope = match spec {
        Some(id) => ContextScope::Spec(id),
        None => ContextScope::Full,
    };

    let filter_status = match status.as_deref() {
        Some("in-progress") => Some(TaskStatus::InProgress),
        Some("complete") => Some(TaskStatus::Complete),
        Some("blocked") => Some(TaskStatus::Blocked),
        Some(other) => {
            return Err(format!("unknown status filter: '{other}' (use in-progress, complete, or blocked)").into());
        }
        None => None,
    };

    let filter = ContextFilter {
        status: filter_status,
        agent,
    };

    let ctx = repo.context(scope, seal_limit, &filter)?;

    match format {
        "brief" => {
            println!(
                "scope:{} tracked:{} changes:{} seals:{}",
                ctx.active_spec
                    .as_ref()
                    .map(|s| s.id.as_str())
                    .unwrap_or("full"),
                ctx.tracked_files,
                ctx.pending_changes
                    .as_ref()
                    .map(|d| d.files_changed)
                    .unwrap_or(0),
                ctx.recent_seals.len(),
            );
        }
        "human" => {
            println!("=== writ context ===\n");

            if let Some(ref spec) = ctx.active_spec {
                println!("spec: {} — {}", spec.id, spec.title);
                println!("  status: {:?}", spec.status);
                println!();
            }

            println!("working state:");
            if ctx.working_state.clean {
                println!("  clean ({} tracked)", ctx.working_state.tracked_count);
            } else {
                for f in &ctx.working_state.new_files {
                    println!("  +  new  {f}");
                }
                for f in &ctx.working_state.modified_files {
                    println!("  ~  mod  {f}");
                }
                for f in &ctx.working_state.deleted_files {
                    println!("  -  del  {f}");
                }
            }
            println!();

            if let Some(ref nudge) = ctx.seal_nudge {
                println!("  ⚠ {}", nudge.message);
                println!();
            }

            if let Some(ref pc) = ctx.pending_changes {
                println!(
                    "pending: {} file(s), +{} -{}",
                    pc.files_changed, pc.total_additions, pc.total_deletions
                );
                println!();
            }

            if !ctx.recent_seals.is_empty() {
                println!("recent seals:");
                for s in &ctx.recent_seals {
                    let spec_part = s
                        .spec_id
                        .as_deref()
                        .map(|id| format!(" spec:{id}"))
                        .unwrap_or_default();
                    let verify_part = match &s.verification {
                        Some(v) => {
                            let mut parts = Vec::new();
                            if let Some(p) = v.tests_passed {
                                parts.push(format!("{p}ok"));
                            }
                            if let Some(f) = v.tests_failed {
                                parts.push(format!("{f}fail"));
                            }
                            if v.linted {
                                parts.push("lint".to_string());
                            }
                            format!(" [{}]", parts.join(","))
                        }
                        None => String::new(),
                    };
                    println!(
                        "  {} {} [{}] — {}{}{}",
                        s.id, s.agent, s.status, s.summary, spec_part, verify_part
                    );
                    for p in &s.changed_paths {
                        println!("    → {p}");
                    }
                }
                println!();
            }

            println!("tracked: {} file(s)", ctx.tracked_files);

            if !ctx.available_operations.is_empty() {
                println!();
                println!("available operations:");
                for op in &ctx.available_operations {
                    println!("  {op}");
                }
            }

            if ctx.convergence_recommended {
                println!();
                println!("  *** CONVERGENCE RECOMMENDED ***");
                println!("  Diverged branches detected — run `writ converge` to merge them.");
            }

            print_diverged_branch_warnings(&repo);
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(&ctx)?);
        }
    }

    Ok(())
}

fn cmd_restore(
    cwd: &PathBuf,
    seal_id: &str,
    force: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    if !force {
        let short = &seal_id[..std::cmp::min(12, seal_id.len())];
        eprintln!(
            "warning: this will overwrite your working directory to match seal {short}"
        );
        eprintln!("  any unsealed changes will be lost");
        eprint!("continue? [y/N] ");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("restore cancelled");
            return Ok(());
        }
    }

    let result = repo.restore(seal_id)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            println!("restored to seal {}", &result.seal_id[..12]);
            if !result.created.is_empty() {
                println!("  created: {} file(s)", result.created.len());
                for f in &result.created {
                    println!("    + {f}");
                }
            }
            if !result.modified.is_empty() {
                println!("  modified: {} file(s)", result.modified.len());
                for f in &result.modified {
                    println!("    ~ {f}");
                }
            }
            if !result.deleted.is_empty() {
                println!("  deleted: {} file(s)", result.deleted.len());
                for f in &result.deleted {
                    println!("    - {f}");
                }
            }
            println!("  total tracked: {} file(s)", result.total_files);
        }
    }

    Ok(())
}

fn cmd_show(
    cwd: &PathBuf,
    seal_id: &str,
    show_diff: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let seal = repo.get_seal(seal_id)?;

    match format {
        "json" => {
            if show_diff {
                let diff = repo.diff_seal(seal_id)?;
                let combined = serde_json::json!({
                    "seal": seal,
                    "diff": diff,
                });
                println!("{}", serde_json::to_string_pretty(&combined)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&seal)?);
            }
        }
        "brief" => {
            let spec_part = seal
                .spec_id
                .as_deref()
                .map(|s| format!(" spec:{s}"))
                .unwrap_or_default();
            println!(
                "{} {} {:?} {}{}",
                &seal.id[..12],
                seal.agent.id,
                seal.status,
                seal.summary,
                spec_part
            );
        }
        _ => {
            println!("seal {}", &seal.id[..12]);
            println!("  full id:   {}", seal.id);
            println!("  agent:     {} ({:?})", seal.agent.id, seal.agent.agent_type);
            println!(
                "  time:      {}",
                seal.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
            );
            if let Some(ref parent) = seal.parent {
                println!("  parent:    {}", &parent[..12]);
            } else {
                println!("  parent:    (none — initial seal)");
            }
            println!("  tree:      {}", &seal.tree[..12]);
            if let Some(ref spec) = seal.spec_id {
                println!("  spec:      {spec}");
            }
            println!("  status:    {:?}", seal.status);
            if seal.verification.tests_passed.is_some() || seal.verification.tests_failed.is_some() || seal.verification.linted {
                print!("  verified: ");
                if let Some(p) = seal.verification.tests_passed {
                    print!(" {p} passed");
                }
                if let Some(f) = seal.verification.tests_failed {
                    print!(" {f} failed");
                }
                if seal.verification.linted {
                    print!(" linted");
                }
                println!();
            }
            println!("  summary:   {}", seal.summary);
            println!("  changes:   {} file(s)", seal.changes.len());
            for c in &seal.changes {
                let marker = match c.change_type {
                    ChangeType::Added => "+",
                    ChangeType::Modified => "~",
                    ChangeType::Deleted => "-",
                };
                println!("    {marker} {}", c.path);
            }

            if show_diff {
                let diff = repo.diff_seal(seal_id)?;
                println!();
                for file_diff in &diff.files {
                    if file_diff.is_binary {
                        println!("Binary file {} differs", file_diff.path);
                        continue;
                    }
                    println!("--- a/{}", file_diff.path);
                    println!("+++ b/{}", file_diff.path);
                    for hunk in &file_diff.hunks {
                        println!(
                            "@@ -{},{} +{},{} @@",
                            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                        );
                        for line in &hunk.lines {
                            let prefix = match line.op {
                                LineOp::Add => "+",
                                LineOp::Remove => "-",
                                LineOp::Context => " ",
                            };
                            println!("{prefix}{}", line.content);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn cmd_spec_update(
    cwd: &PathBuf,
    id: &str,
    status: Option<String>,
    depends_on: Option<Vec<String>>,
    file_scope: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    design_notes: Option<Vec<String>>,
    tech_stack: Option<Vec<String>>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let parsed_status = match status {
        Some(s) => Some(s.parse::<writ_core::spec::SpecStatus>().map_err(|e| e)?),
        None => None,
    };

    let update = SpecUpdate {
        status: parsed_status,
        depends_on,
        file_scope,
        acceptance_criteria,
        design_notes,
        tech_stack,
    };

    let spec = repo.update_spec(id, update)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&spec)?);
        }
        _ => {
            println!("spec updated: {}", spec.id);
            println!("  status:     {:?}", spec.status);
            if !spec.depends_on.is_empty() {
                println!("  depends on: {}", spec.depends_on.join(", "));
            }
            if !spec.file_scope.is_empty() {
                println!("  file scope: {}", spec.file_scope.join(", "));
            }
            if !spec.acceptance_criteria.is_empty() {
                println!("  criteria:   {}", spec.acceptance_criteria.join("; "));
            }
            if !spec.design_notes.is_empty() {
                println!("  notes:      {}", spec.design_notes.join("; "));
            }
            if !spec.tech_stack.is_empty() {
                println!("  tech stack: {}", spec.tech_stack.join(", "));
            }
        }
    }

    Ok(())
}

fn cmd_spec_add(
    cwd: &PathBuf,
    id: &str,
    title: &str,
    description: &str,
    acceptance_criteria: Option<Vec<String>>,
    design_notes: Option<Vec<String>>,
    tech_stack: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let mut spec = Spec::new(id.to_string(), title.to_string(), description.to_string());
    if let Some(ac) = acceptance_criteria {
        spec.acceptance_criteria = ac;
    }
    if let Some(dn) = design_notes {
        spec.design_notes = dn;
    }
    if let Some(ts) = tech_stack {
        spec.tech_stack = ts;
    }
    repo.add_spec(&spec)?;
    println!("spec added: {id}");
    println!("  title: {title}");
    if !spec.acceptance_criteria.is_empty() {
        println!("  criteria:   {}", spec.acceptance_criteria.join("; "));
    }
    if !spec.design_notes.is_empty() {
        println!("  notes:      {}", spec.design_notes.join("; "));
    }
    if !spec.tech_stack.is_empty() {
        println!("  tech stack: {}", spec.tech_stack.join(", "));
    }
    Ok(())
}

fn cmd_spec_status(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let specs = repo.list_specs()?;

    if specs.is_empty() {
        println!("no specs registered");
        return Ok(());
    }

    for spec in &specs {
        let status_marker = match spec.status {
            writ_core::spec::SpecStatus::Pending => "  ",
            writ_core::spec::SpecStatus::InProgress => "> ",
            writ_core::spec::SpecStatus::Complete => "v ",
            writ_core::spec::SpecStatus::Blocked => "x ",
        };
        let seal_count = spec.sealed_by.len();
        println!(
            "  {status_marker}{:<20} {:?}  ({seal_count} seal(s))",
            spec.id, spec.status
        );
    }

    Ok(())
}

fn cmd_converge(
    cwd: &PathBuf,
    left_spec: &str,
    right_spec: &str,
    format: &str,
    apply: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let report = repo.converge(left_spec, right_spec)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "brief" => {
            let status = if report.is_clean { "clean" } else { "conflict" };
            println!(
                "{} + {} = {} (auto:{} conflicts:{} left:{} right:{})",
                report.left_spec,
                report.right_spec,
                status,
                report.auto_merged.len(),
                report.conflicts.len(),
                report.left_only.len(),
                report.right_only.len(),
            );
        }
        _ => {
            println!(
                "convergence: {} + {}",
                report.left_spec, report.right_spec
            );

            if let Some(ref base) = report.base_seal_id {
                println!("  base: seal {}", &base[..12.min(base.len())]);
            } else {
                println!("  base: (empty state)");
            }
            println!();

            if !report.auto_merged.is_empty() {
                println!("  auto-merged ({} file(s)):", report.auto_merged.len());
                for m in &report.auto_merged {
                    println!("    ~ {}", m.path);
                }
                println!();
            }

            if !report.left_only.is_empty() {
                println!("  left only ({} file(s)):", report.left_only.len());
                for p in &report.left_only {
                    println!("    ~ {p}");
                }
                println!();
            }

            if !report.right_only.is_empty() {
                println!("  right only ({} file(s)):", report.right_only.len());
                for p in &report.right_only {
                    println!("    ~ {p}");
                }
                println!();
            }

            if !report.conflicts.is_empty() {
                println!("  conflicts ({} file(s)):", report.conflicts.len());
                for c in &report.conflicts {
                    println!("    {}:", c.path);
                    for (i, region) in c.regions.iter().enumerate() {
                        println!("      region {} (line {}):", i + 1, region.base_start);
                        if !region.base_lines.is_empty() {
                            for bl in &region.base_lines {
                                println!("        base:  {bl}");
                            }
                        }
                        for ll in &region.left_lines {
                            println!("        left:  {ll}");
                        }
                        for rl in &region.right_lines {
                            println!("        right: {rl}");
                        }
                    }
                }
                println!();
            }

            if report.is_clean {
                println!("  result: clean");
                if !apply {
                    println!("  run with --apply to write merged files");
                }
            } else {
                println!(
                    "  result: {} conflict(s) — resolve before applying",
                    report.conflicts.len()
                );
            }
        }
    }

    if apply {
        if !report.is_clean {
            eprintln!("error: cannot --apply with unresolved conflicts");
            eprintln!("  use JSON output to inspect conflicts and resolve programmatically");
            std::process::exit(1);
        }
        repo.apply_convergence(&report, &[])?;
        if format != "json" {
            println!("\n  applied — merged files written to working directory");
            println!("  seal with `writ seal` to capture the converged state");
        }
    }

    Ok(())
}

fn cmd_spec_show(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let spec = repo.load_spec(id)?;

    println!("spec: {}", spec.id);
    println!("  title:       {}", spec.title);
    println!("  status:      {:?}", spec.status);
    println!(
        "  created:     {}",
        spec.created_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    if !spec.description.is_empty() {
        println!("  description: {}", spec.description);
    }
    if !spec.depends_on.is_empty() {
        println!("  depends on:  {}", spec.depends_on.join(", "));
    }
    if !spec.sealed_by.is_empty() {
        println!("  sealed by:   {} seal(s)", spec.sealed_by.len());
        for sid in &spec.sealed_by {
            println!("    {}", &sid[..12]);
        }
    }

    Ok(())
}

fn cmd_bridge_import(
    cwd: &PathBuf,
    git_ref: &str,
    agent_id: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let agent = AgentIdentity {
        id: agent_id.to_string(),
        agent_type: AgentType::Agent,
    };

    let result = repo.bridge_import(Some(git_ref), agent)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            println!("imported git {} as seal {}", &result.git_commit[..12], &result.seal_id[..12]);
            println!("  ref:   {}", result.git_ref);
            println!("  files: {}", result.files_imported);
        }
    }

    Ok(())
}

fn cmd_bridge_export(
    cwd: &PathBuf,
    branch: &str,
    pr_body: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let result = repo.bridge_export(Some(branch))?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            if result.seals_exported == 0 {
                println!("nothing to export — all seals already synced");
            } else {
                println!(
                    "exported {} seal(s) to branch {}",
                    result.seals_exported, result.branch
                );
                for e in &result.exported {
                    println!("  {} → {} — {}", &e.seal_id[..12], &e.git_commit[..12], e.summary);
                }
            }
        }
    }

    if pr_body && !result.exported.is_empty() {
        println!("\n--- PR Body ---\n");
        println!("## Agent Work Summary\n");
        println!("Exported {} seal(s) from writ to branch `{}`.\n", result.seals_exported, result.branch);
        println!("| Seal | Agent | Summary |");
        println!("|------|-------|---------|");
        for e in &result.exported {
            let agent = e.agent_id.as_deref().unwrap_or("unknown");
            println!("| `{}` | {} | {} |", &e.seal_id[..12], agent, e.summary);
        }
        println!("\n*Generated by writ bridge export*");
    }

    Ok(())
}

fn cmd_bridge_status(
    cwd: &PathBuf,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let status = repo.bridge_status()?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&status)?),
        _ => {
            if !status.initialized {
                println!("bridge not initialized — run `writ bridge import` first");
            } else {
                if let Some(ref imp) = status.last_import {
                    println!("last import: git {} → seal {}", &imp.git_commit[..12], &imp.seal_id[..12]);
                    println!("  ref: {}", imp.git_ref);
                }
                if let Some(ref exp) = status.last_export {
                    println!("last export: seal {} → git {} (branch: {})", &exp.seal_id[..12], &exp.git_commit[..12], exp.branch);
                }
                println!("pending: {} seal(s) to export", status.pending_export_count);
            }
        }
    }

    Ok(())
}

fn cmd_push(
    cwd: &PathBuf,
    remote: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let result = repo.push(remote)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            if !result.head_updated && result.objects_pushed == 0 && result.seals_pushed == 0 {
                println!("everything up-to-date with '{}'", result.remote);
            } else {
                println!("pushed to '{}'", result.remote);
                if result.objects_pushed > 0 {
                    println!("  objects: {}", result.objects_pushed);
                }
                if result.seals_pushed > 0 {
                    println!("  seals:   {}", result.seals_pushed);
                }
                if result.specs_pushed > 0 {
                    println!("  specs:   {}", result.specs_pushed);
                }
                if result.head_updated {
                    println!("  HEAD updated");
                }
            }
        }
    }

    Ok(())
}

fn cmd_pull(
    cwd: &PathBuf,
    remote: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let result = repo.pull(remote)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            if !result.head_updated && result.objects_pulled == 0 && result.seals_pulled == 0 {
                println!("already up-to-date with '{}'", result.remote);
            } else {
                println!("pulled from '{}'", result.remote);
                if result.objects_pulled > 0 {
                    println!("  objects: {}", result.objects_pulled);
                }
                if result.seals_pulled > 0 {
                    println!("  seals:   {}", result.seals_pulled);
                }
                if result.specs_pulled > 0 {
                    println!("  specs:   {}", result.specs_pulled);
                }
                if result.head_updated {
                    println!("  HEAD updated");
                }
                if !result.spec_conflicts.is_empty() {
                    println!("  spec conflicts: {}", result.spec_conflicts.len());
                    for c in &result.spec_conflicts {
                        println!("    {} — field '{}': local='{}' remote='{}'", c.spec_id, c.field, c.local_value, c.remote_value);
                    }
                }
            }
        }
    }

    Ok(())
}

fn cmd_remote_init(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    Repository::remote_init(path)?;
    println!("initialized bare remote at {}", path.display());
    Ok(())
}

fn cmd_remote_add(
    cwd: &PathBuf,
    name: &str,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.remote_add(name, path)?;
    println!("remote '{name}' added → {path}");
    Ok(())
}

fn cmd_remote_remove(
    cwd: &PathBuf,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.remote_remove(name)?;
    println!("remote '{name}' removed");
    Ok(())
}

fn cmd_remote_list(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let remotes = repo.remote_list()?;

    if remotes.is_empty() {
        println!("no remotes configured");
    } else {
        for (name, entry) in &remotes {
            println!("  {name}\t{}", entry.path);
        }
    }

    Ok(())
}

fn cmd_remote_status(
    cwd: &PathBuf,
    remote: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let status = repo.remote_status(remote)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&status)?),
        _ => {
            println!("remote '{}' → {}", status.name, status.path);
            let local = status
                .local_head
                .as_deref()
                .map(|h| &h[..12])
                .unwrap_or("(none)");
            let remote_h = status
                .remote_head
                .as_deref()
                .map(|h| &h[..12])
                .unwrap_or("(none)");
            println!("  local HEAD:  {local}");
            println!("  remote HEAD: {remote_h}");
            println!("  ahead:  {}", status.ahead);
            println!("  behind: {}", status.behind);
        }
    }

    Ok(())
}
