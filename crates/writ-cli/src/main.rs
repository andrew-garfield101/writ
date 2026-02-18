//! writ CLI — the human (and agent) interface to writ.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use writ_core::context::ContextScope;
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

        /// Agent or human identifier.
        #[arg(long, default_value = "human")]
        agent: String,

        /// Linked spec ID.
        #[arg(long)]
        spec: Option<String>,

        /// Task status: in-progress, complete, or blocked.
        #[arg(long, default_value = "complete")]
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
        } => cmd_seal(&cwd, &summary, &agent, spec, &status, paths, tests_passed, tests_failed, linted),
        Commands::Show {
            seal_id,
            diff,
            format,
        } => cmd_show(&cwd, &seal_id, diff, &format),
        Commands::Log { format, limit } => cmd_log(&cwd, &format, limit),
        Commands::Diff { from, to, format } => cmd_diff(&cwd, from, to, &format),
        Commands::Context {
            spec,
            seal_limit,
            format,
        } => cmd_context(&cwd, spec, seal_limit, &format),
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
            } => cmd_spec_add(&cwd, &id, &title, &description),
            SpecCommands::Status => cmd_spec_status(&cwd),
            SpecCommands::Show { id } => cmd_spec_show(&cwd, &id),
            SpecCommands::Update {
                id,
                status,
                depends_on,
                file_scope,
                format,
            } => cmd_spec_update(&cwd, &id, status, depends_on, file_scope, &format),
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
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let agent = AgentIdentity {
        id: agent_id.to_string(),
        agent_type: if agent_id.starts_with("agent") {
            AgentType::Agent
        } else {
            AgentType::Human
        },
    };

    let task_status = match status {
        "in-progress" => TaskStatus::InProgress,
        "blocked" => TaskStatus::Blocked,
        _ => TaskStatus::Complete,
    };

    let verification = Verification {
        tests_passed,
        tests_failed,
        linted,
    };

    let seal = if let Some(paths) = paths {
        repo.seal_paths(
            agent,
            summary.to_string(),
            spec_id,
            task_status,
            verification,
            &paths,
        )?
    } else {
        repo.seal(
            agent,
            summary.to_string(),
            spec_id,
            task_status,
            verification,
        )?
    };

    println!("sealed {}", &seal.id[..12]);
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

    Ok(())
}

fn cmd_log(
    cwd: &PathBuf,
    format: &str,
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let mut seals = repo.log()?;

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

    Ok(())
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
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let scope = match spec {
        Some(id) => ContextScope::Spec(id),
        None => ContextScope::Full,
    };

    let ctx = repo.context(scope, seal_limit)?;

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
                    println!("  {} {} — {}", s.id, s.agent, s.summary);
                }
                println!();
            }

            println!("tracked: {} file(s)", ctx.tracked_files);
        }
        _ => {
            // Default: JSON
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
        }
    }

    Ok(())
}

fn cmd_spec_add(
    cwd: &PathBuf,
    id: &str,
    title: &str,
    description: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let spec = Spec::new(id.to_string(), title.to_string(), description.to_string());
    repo.add_spec(&spec)?;
    println!("spec added: {id}");
    println!("  title: {title}");
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
