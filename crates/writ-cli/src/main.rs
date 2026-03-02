//! writ CLI — the human (and agent) interface to writ.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use colored::Colorize;
use writ_core::agent::{AgentUpdate, TrustLevel};
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
    Init {
        /// Deployment profile for GC configuration.
        /// Values: raspberry-pi, development (default), production, enterprise.
        #[arg(long, default_value = "development")]
        profile: String,
    },

    /// Remove writ from this project (inverse of install).
    Uninstall {
        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,

        /// Keep the .writignore file.
        #[arg(long)]
        keep_writignore: bool,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// One-command setup: init + detect git + import baseline.
    Install {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,

        /// Create a spec during install (convenience shortcut).
        /// Example: --spec auth --title "Authentication" --description "JWT auth system"
        #[arg(long)]
        spec: Option<String>,

        /// Title for the spec created with --spec. Defaults to the spec ID.
        #[arg(long, requires = "spec")]
        title: Option<String>,

        /// Description for the spec created with --spec.
        #[arg(long, requires = "spec")]
        description: Option<String>,
    },

    /// Show working directory state.
    State {
        /// Output format: "human" (default from settings), "json", or "brief".
        #[arg(long)]
        format: Option<String>,
    },

    /// Create a seal (structured checkpoint) from current changes.
    Seal {
        /// Summary of what changed and why.
        #[arg(long, short)]
        summary: String,

        /// Agent or human identifier. Uses `default_agent` setting if configured,
        /// otherwise defaults to "human" (sets agent_type=human).
        /// Any non-"human" value sets agent_type=agent.
        #[arg(long)]
        agent: Option<String>,

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

        /// Reject seals that modify files outside the agent's scope constraints.
        #[arg(long)]
        enforce_scope: bool,
    },

    /// Inspect a specific seal.
    Show {
        /// Seal ID (supports short prefix).
        seal_id: String,

        /// Include the diff introduced by this seal.
        #[arg(long)]
        diff: bool,

        /// Output format: "human" (default from settings), "json", or "brief".
        #[arg(long)]
        format: Option<String>,
    },

    /// Show seal history.
    Log {
        /// Output format: "human" (default from settings), "json", or "brief".
        #[arg(long)]
        format: Option<String>,

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

        /// Output format: "human" (default from settings), "json", or "brief".
        #[arg(long)]
        format: Option<String>,
    },

    /// Dump structured context for LLM consumption.
    Context {
        /// Scope to a specific spec ID.
        #[arg(long)]
        spec: Option<String>,

        /// Scope entire context to an agent's world (their specs, files, risks).
        /// Unlike --agent which filters seal history, --for-agent scopes everything.
        #[arg(long)]
        for_agent: Option<String>,

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
        /// Note: context defaults to "json" unlike other commands.
        #[arg(long)]
        format: Option<String>,
    },

    /// Human-readable summary of all work done in this writ session.
    /// Designed for the round-trip: writ install -> agents work -> writ summary -> git commit.
    Summary {
        /// Output format: "human" (default from settings), "json", "commit", or "pr".
        /// "commit" outputs a concise one-line commit message.
        /// "pr" outputs a detailed PR description with full spec/agent breakdown.
        #[arg(long)]
        format: Option<String>,
    },

    /// One-command round-trip: generate summary, git add, git commit.
    /// Equivalent to: git add . && git commit -m "$(writ summary --format commit)"
    Finish {
        /// Use the full PR-style description as the commit body instead of a one-liner.
        #[arg(long)]
        full: bool,

        /// Dry run: show what would be committed without actually committing.
        #[arg(long)]
        dry_run: bool,
    },

    /// Restore working directory to a specific seal's state.
    Restore {
        /// Seal ID to restore to (supports short prefix).
        seal_id: String,

        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,

        /// Output format: "human" (default from settings) or "json".
        #[arg(long)]
        format: Option<String>,
    },

    /// Analyze convergence between two specs (three-way merge).
    Converge {
        /// Left spec ID.
        left_spec: String,

        /// Right spec ID.
        right_spec: String,

        /// Output format: "json" (default), "human", or "brief".
        /// Note: converge defaults to "json" unlike other commands.
        #[arg(long)]
        format: Option<String>,

        /// Apply the convergence result to the working directory (clean merges only).
        #[arg(long)]
        apply: bool,
    },

    /// Converge ALL diverged branches in sequence (newest-first ordering).
    /// This is the recommended way to merge after multi-agent parallel work.
    ConvergeAll {
        /// Output format: "json" (default) or "human".
        /// Note: converge-all defaults to "json" unlike other commands.
        #[arg(long)]
        format: Option<String>,

        /// Automatically apply clean merges and resolve conflicts per strategy.
        #[arg(long)]
        apply: bool,

        /// Dry run: show what would be merged without applying.
        #[arg(long)]
        dry_run: bool,

        /// Fallback strategy for irreconcilable conflicts. Uses `convergence.strategy`
        /// setting if configured, otherwise defaults to "escalate".
        /// Deterministic patterns always run regardless of strategy.
        #[arg(long)]
        strategy: Option<String>,

        /// Auto-apply highest-confidence resolutions for escalated conflicts.
        /// Uses `convergence.auto_resolve_min_confidence` setting as threshold
        /// (default 0.85). All decisions are logged to security events for audit.
        #[arg(long)]
        auto_resolve: bool,
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

    /// Verify seal chain integrity and signatures.
    Verify {
        /// Verify the full hash chain from genesis to HEAD.
        #[arg(long)]
        chain: bool,

        /// Verify HEAD chain plus all spec branch chains.
        #[arg(long)]
        all_chains: bool,

        /// Verify a specific seal by ID (or prefix).
        #[arg(long)]
        seal: Option<String>,

        /// Output format: "human" (default from settings) or "json".
        #[arg(long)]
        format: Option<String>,
    },

    /// Manage agent identities.
    Agent {
        #[command(subcommand)]
        action: AgentCommands,
    },

    /// View security events and monitoring data.
    Security {
        #[command(subcommand)]
        action: SecurityCommands,
    },

    /// Garbage collection — lifecycle management and storage cleanup.
    Gc {
        #[command(subcommand)]
        action: GcCommands,
    },

    /// Resolve escalated convergence conflicts.
    /// Run without arguments to see pending escalations.
    Resolve {
        /// File path to resolve. Omit to list pending escalations.
        file: Option<String>,

        /// Quick resolution strategy: left, right, both, or best.
        /// "left" keeps the base version. "right" keeps the incoming version.
        /// "both" concatenates both versions. "best" uses the highest-confidence suggestion.
        #[arg(long)]
        accept: Option<String>,

        /// Resolve all pending escalations at once (requires --accept).
        #[arg(long)]
        all: bool,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Persistent repository settings.
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },
}

#[derive(Subcommand)]
enum AgentCommands {
    /// Register a new agent identity.
    Register {
        /// Agent identifier (alphanumeric, hyphens, underscores, dots).
        name: String,

        /// Trust level: full, standard, restricted, or untrusted.
        #[arg(long, default_value = "standard")]
        trust_level: String,

        /// Scope constraint glob pattern (repeatable).
        #[arg(long)]
        scope: Option<Vec<String>>,

        /// Who is registering this agent.
        #[arg(long, default_value = "human")]
        registered_by: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// List all registered agents.
    List {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show details for a specific agent.
    Show {
        /// Agent identifier.
        name: String,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Revoke an agent (permanent, removes keys).
    Revoke {
        /// Agent identifier.
        name: String,

        /// Reason for revocation.
        #[arg(long)]
        reason: String,

        /// When the compromise started (RFC 3339). If omitted, assumes now.
        /// All seals by this agent after this time are flagged as compromised.
        #[arg(long)]
        compromise_timestamp: Option<String>,
    },

    /// Suspend an agent (temporary).
    Suspend {
        /// Agent identifier.
        name: String,
    },

    /// Reactivate a suspended agent.
    Reactivate {
        /// Agent identifier.
        name: String,
    },

    /// Manage an agent's scope constraints.
    Scope {
        /// Agent identifier.
        name: String,

        /// Add a scope constraint glob pattern.
        #[arg(long)]
        add: Option<String>,

        /// Remove a scope constraint pattern.
        #[arg(long)]
        remove: Option<String>,

        /// List current scope constraints.
        #[arg(long)]
        list: bool,

        /// Replace all scope constraints (comma-separated).
        #[arg(long, value_delimiter = ',')]
        set: Option<Vec<String>>,
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
    Status {
        /// Filter by lifecycle state: active, stale, completed, cancelled, archived.
        #[arg(long)]
        state: Option<String>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Cancel a spec (transitions lifecycle to Cancelled).
    Cancel {
        /// Spec ID to cancel.
        id: String,
    },

    /// Complete a spec's lifecycle (transitions to Completed).
    /// Requires the spec's user-facing status to already be 'complete'.
    Complete {
        /// Spec ID to complete.
        id: String,
    },

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

#[derive(Subcommand)]
enum SecurityCommands {
    /// List recent security events.
    Events {
        /// Filter by severity: info, warning, or critical.
        #[arg(long)]
        severity: Option<String>,

        /// Filter by event type (e.g., convergence_started, scope_violation).
        #[arg(long)]
        event_type: Option<String>,

        /// Maximum number of events to show.
        #[arg(long, short)]
        limit: Option<usize>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

#[derive(Subcommand)]
enum GcCommands {
    /// Run garbage collection (generate plan and execute).
    Run {
        /// Dry run: show what would be cleaned without executing.
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompt for large cleanups.
        #[arg(long)]
        yes: bool,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show current storage usage and lifecycle state summary.
    Status {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show detailed storage breakdown by category.
    Storage {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show GC audit history.
    Log {
        /// Maximum number of entries to show.
        #[arg(long, short, default_value = "10")]
        limit: usize,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Set a configuration value.
    Set {
        /// Setting key (e.g., "default_agent", "convergence.auto_resolve").
        key: String,
        /// Setting value.
        value: String,
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Get a configuration value.
    Get {
        /// Setting key.
        key: String,
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// List all configuration settings.
    List {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Remove a configuration value (reset to default).
    Unset {
        /// Setting key.
        key: String,
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },
}

fn main() {
    // Reset SIGPIPE to default so piping to `head`, `grep`, etc. doesn't
    // cause a Rust BrokenPipe panic (exit 101) which kills `set -euo pipefail` scripts.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("error: cannot determine current directory: {e}");
        process::exit(1);
    });

    let result = match cli.command {
        Commands::Init { profile } => cmd_init(&cwd, &profile),
        Commands::Uninstall {
            force,
            keep_writignore,
            format,
        } => cmd_uninstall(&cwd, force, keep_writignore, &format),
        Commands::Install {
            format,
            spec,
            title,
            description,
        } => cmd_install(&cwd, &format, spec, title, description),
        Commands::State { format } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_state(&cwd, &format)
        }
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
            enforce_scope,
        } => {
            let agent = resolve_agent(agent.as_deref(), &cwd);
            cmd_seal(
                &cwd,
                &summary,
                &agent,
                spec,
                &status,
                paths,
                tests_passed,
                tests_failed,
                linted,
                allow_empty,
                expected_head,
                enforce_scope,
            )
        }
        Commands::Show {
            seal_id,
            diff,
            format,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_show(&cwd, &seal_id, diff, &format)
        }
        Commands::Log {
            format,
            limit,
            spec,
            all,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_log(&cwd, &format, limit, spec, all)
        }
        Commands::Diff { from, to, format } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_diff(&cwd, from, to, &format)
        }
        Commands::Context {
            spec,
            for_agent,
            seal_limit,
            status,
            agent,
            format,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "json");
            cmd_context(&cwd, spec, for_agent, seal_limit, status, agent, &format)
        }
        Commands::Summary { format } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_summary(&cwd, &format)
        }
        Commands::Finish { full, dry_run } => cmd_finish(&cwd, full, dry_run),
        Commands::Restore {
            seal_id,
            force,
            format,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_restore(&cwd, &seal_id, force, &format)
        }
        Commands::Converge {
            left_spec,
            right_spec,
            format,
            apply,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "json");
            cmd_converge(&cwd, &left_spec, &right_spec, &format, apply)
        }
        Commands::ConvergeAll {
            format,
            apply,
            dry_run,
            strategy,
            auto_resolve,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "json");
            let strategy = resolve_strategy(strategy.as_deref(), &cwd);
            cmd_converge_all(&cwd, &format, apply, dry_run, &strategy, auto_resolve)
        }
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
            SpecCommands::Status { state, format } => {
                cmd_spec_status(&cwd, state.as_deref(), &format)
            }
            SpecCommands::Cancel { id } => cmd_spec_cancel(&cwd, &id),
            SpecCommands::Complete { id } => cmd_spec_complete(&cwd, &id),
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
            BridgeCommands::Export {
                branch,
                pr_body,
                format,
            } => cmd_bridge_export(&cwd, &branch, pr_body, &format),
            BridgeCommands::Status { format } => cmd_bridge_status(&cwd, &format),
        },
        Commands::Push { remote, format } => cmd_push(&cwd, &remote, &format),
        Commands::Pull { remote, format } => cmd_pull(&cwd, &remote, &format),
        Commands::Remote { action } => match action {
            RemoteCommands::Init { path } => cmd_remote_init(&path),
            RemoteCommands::Add { name, path } => cmd_remote_add(&cwd, &name, &path),
            RemoteCommands::Remove { name } => cmd_remote_remove(&cwd, &name),
            RemoteCommands::List => cmd_remote_list(&cwd),
            RemoteCommands::Status { remote, format } => cmd_remote_status(&cwd, &remote, &format),
        },
        Commands::Verify {
            chain,
            all_chains,
            seal,
            format,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_verify(&cwd, chain, all_chains, seal.as_deref(), &format)
        }
        Commands::Agent { action } => match action {
            AgentCommands::Register {
                name,
                trust_level,
                scope,
                registered_by,
                format,
            } => cmd_agent_register(&cwd, &name, &trust_level, scope, &registered_by, &format),
            AgentCommands::List { format } => cmd_agent_list(&cwd, &format),
            AgentCommands::Show { name, format } => cmd_agent_show(&cwd, &name, &format),
            AgentCommands::Revoke {
                name,
                reason,
                compromise_timestamp,
            } => cmd_agent_revoke(&cwd, &name, &reason, compromise_timestamp.as_deref()),
            AgentCommands::Suspend { name } => cmd_agent_suspend(&cwd, &name),
            AgentCommands::Reactivate { name } => cmd_agent_reactivate(&cwd, &name),
            AgentCommands::Scope {
                name,
                add,
                remove,
                list,
                set,
            } => cmd_agent_scope(&cwd, &name, add, remove, list, set),
        },
        Commands::Security { action } => match action {
            SecurityCommands::Events {
                severity,
                event_type,
                limit,
                format,
            } => cmd_security_events(
                &cwd,
                severity.as_deref(),
                event_type.as_deref(),
                limit,
                &format,
            ),
        },
        Commands::Gc { action } => match action {
            GcCommands::Run {
                dry_run,
                yes,
                format,
            } => cmd_gc_run(&cwd, dry_run, yes, &format),
            GcCommands::Status { format } => cmd_gc_status(&cwd, &format),
            GcCommands::Storage { format } => cmd_gc_storage(&cwd, &format),
            GcCommands::Log { limit, format } => cmd_gc_log(&cwd, limit, &format),
        },
        Commands::Resolve {
            file,
            accept,
            all,
            format,
        } => cmd_resolve(&cwd, file, accept, all, &format),
        Commands::Config { action } => match action {
            ConfigCommands::Set { key, value, format } => {
                cmd_config_set(&cwd, &key, &value, &format)
            }
            ConfigCommands::Get { key, format } => cmd_config_get(&cwd, &key, &format),
            ConfigCommands::List { format } => cmd_config_list(&cwd, &format),
            ConfigCommands::Unset { key, format } => cmd_config_unset(&cwd, &key, &format),
        },
    };

    if let Err(e) = result {
        eprintln!("{} {e}", "error:".red().bold());
        if let Some(hint) = error_hint(e.as_ref()) {
            eprintln!("  {} {hint}", "hint:".yellow());
        }
        process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// Shared CLI helpers
// ---------------------------------------------------------------------------

/// Resolve the effective output format. If the user explicitly passed a value,
/// use it. Otherwise check settings, then fall back to the given default.
fn resolve_format(explicit: Option<&str>, cwd: &PathBuf, fallback: &str) -> String {
    if let Some(f) = explicit {
        return f.to_string();
    }
    Repository::open(cwd)
        .ok()
        .and_then(|r| r.settings().default_format.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// Resolve the effective agent ID for seals.
fn resolve_agent(explicit: Option<&str>, cwd: &PathBuf) -> String {
    if let Some(a) = explicit {
        return a.to_string();
    }
    Repository::open(cwd)
        .ok()
        .and_then(|r| r.settings().default_agent.clone())
        .unwrap_or_else(|| "human".to_string())
}

/// Resolve the effective convergence strategy.
fn resolve_strategy(explicit: Option<&str>, cwd: &PathBuf) -> String {
    if let Some(s) = explicit {
        return s.to_string();
    }
    Repository::open(cwd)
        .ok()
        .and_then(|r| r.settings().convergence.strategy.clone())
        .unwrap_or_else(|| "escalate".to_string())
}

/// Return an actionable hint for a given error, or None if no hint is needed.
fn error_hint(err: &dyn std::error::Error) -> Option<String> {
    let msg = err.to_string();

    if msg.contains("not a writ repository") {
        return Some("run `writ init` or `writ install` to create one".into());
    }
    if msg.contains(".writ/ already exists") {
        return Some(
            "this directory already has writ initialized — use `writ uninstall` first to start fresh"
                .into(),
        );
    }
    if msg.contains("no changes to seal") {
        return Some(
            "make some file changes first, or use `--allow-empty` for metadata-only seals".into(),
        );
    }
    if msg.contains("seal not found") {
        return Some("use `writ log` to see available seal IDs (prefix match supported)".into());
    }
    if msg.contains("spec not found") {
        return Some("use `writ spec status` to see available specs".into());
    }
    if msg.contains("agent not found") {
        return Some("use `writ agent list` to see registered agents".into());
    }
    if msg.contains("could not acquire repository lock") {
        return Some(
            "another writ process may be running — wait or remove `.writ/lock` if stale".into(),
        );
    }
    if msg.contains("no git repository found") {
        return Some("run `git init` first, then `writ install`".into());
    }
    if msg.contains("unresolved conflicts") {
        return Some(
            "use `writ resolve` to review and fix, or re-run with `--auto-resolve`".into(),
        );
    }
    if msg.contains("push rejected") || msg.contains("Push rejected") {
        return Some("pull first with `writ pull`, resolve, then push again".into());
    }
    if msg.contains("Pull detected diverged") {
        return Some(
            "use `writ converge-all --apply` to merge diverged branches after pulling".into(),
        );
    }
    if msg.contains("remote not found") {
        return Some("use `writ remote list` to see configured remotes".into());
    }
    if msg.contains("path rejected (traversal)") {
        return Some(
            "writ does not allow absolute paths or `..` — use relative paths within the project"
                .into(),
        );
    }
    if msg.contains("decompression bomb") {
        return Some(
            "this object decompresses beyond the safety limit — it may be corrupted".into(),
        );
    }

    None
}

/// Create a cyan braille spinner with the given message (C-2 dedup).
fn make_spinner(msg: &str) -> indicatif::ProgressBar {
    let sp = indicatif::ProgressBar::new_spinner();
    sp.set_style(
        indicatif::ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    sp.set_message(msg.to_string());
    sp.enable_steady_tick(std::time::Duration::from_millis(80));
    sp
}

/// Color an integration risk level string (C-3 dedup).
fn color_risk_level(level: &str) -> colored::ColoredString {
    match level {
        "low" => level.to_uppercase().green(),
        "medium" => level.to_uppercase().yellow(),
        "high" => level.to_uppercase().red(),
        _ => level.to_uppercase().normal(),
    }
}

fn cmd_init(cwd: &PathBuf, profile: &str) -> Result<(), Box<dyn std::error::Error>> {
    Repository::init(cwd)?;

    // Save GC config from the selected profile.
    let gc_config = writ_core::gc::GcConfig::from_profile(profile)?;
    gc_config.save(&cwd.join(".writ"))?;

    println!("initialized writ repository in .writ/");
    println!("  gc profile: {profile}");
    Ok(())
}

fn cmd_install(
    cwd: &PathBuf,
    format: &str,
    spec_id: Option<String>,
    spec_title: Option<String>,
    spec_description: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = Repository::install(cwd)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            if result.initialized {
                println!("initialized writ repository in .writ/");
            } else {
                println!("writ repository already exists");
            }

            if result.writignore_created {
                println!("created .writignore");
            }

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

            if result.git_imported {
                let seal_short = result
                    .imported_seal_id
                    .as_deref()
                    .map(|s| &s[..12.min(s.len())])
                    .unwrap_or("?");
                let files = result.imported_files.unwrap_or(0);

                if result.reimported {
                    println!(
                        "re-imported git baseline: {} file(s), seal {}",
                        files, seal_short
                    );
                } else {
                    println!(
                        "imported git baseline: {} file(s), seal {}",
                        files, seal_short
                    );
                }
            } else if result.already_imported {
                println!("git baseline already synced");
            } else if let Some(ref reason) = result.import_skipped_reason {
                println!("import skipped: {}", reason);
            }

            if let Some(ref err) = result.import_error {
                eprintln!("import error: {}", err);
            }

            println!("tracked: {} file(s)", result.tracked_files);

            let detected: Vec<_> = result
                .frameworks_detected
                .iter()
                .filter(|f| f.detected)
                .collect();
            for f in &detected {
                println!("detected {:?} ({})", f.framework, f.indicators.join(", "));
            }

            for hook in &result.hooks_installed {
                for f in &hook.files_created {
                    println!("  + {f}");
                }
                for f in &hook.files_updated {
                    println!("  ~ {f}");
                }
            }
        }
    }

    // Create a spec if --spec was provided.
    if let Some(ref id) = spec_id {
        let repo = Repository::open(cwd)?;
        let title = spec_title.as_deref().unwrap_or(id);
        let desc = spec_description.as_deref().unwrap_or("");
        repo.add_spec(&Spec::new(id.clone(), title.to_string(), desc.to_string()))?;
        if format != "json" {
            println!("spec: created '{}' ({})", id, title);
        }
    }

    if format != "json" {
        println!();
        println!("ready. next steps:");
        for op in &result.available_operations {
            println!("  {}", op);
        }
    }

    Ok(())
}

fn cmd_uninstall(
    cwd: &PathBuf,
    force: bool,
    keep_writignore: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check if .writ/ exists at all.
    if !cwd.join(".writ").exists() {
        if format == "json" {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "error": "no writ repository found"
                }))?
            );
        } else {
            eprintln!("error: no writ repository found in current directory");
        }
        process::exit(1);
    }

    if !force {
        // Preview what will be removed.
        let repo = Repository::open(cwd).ok();
        let seal_count = repo
            .as_ref()
            .and_then(|r| r.log().ok())
            .map(|s| s.len())
            .unwrap_or(0);
        let tracked = repo
            .as_ref()
            .and_then(|r| r.state().ok())
            .map(|s| s.tracked_count)
            .unwrap_or(0);

        eprintln!("warning: this will remove writ from this project");
        eprintln!(
            "  .writ/ directory ({} seal(s), {} tracked file(s))",
            seal_count, tracked
        );
        if !keep_writignore && cwd.join(".writignore").exists() {
            eprintln!("  .writignore");
        }
        eprintln!("  framework hooks (CLAUDE.md sections, command files, AGENTS.md sections)");
        eprintln!();
        eprintln!("  note: this does NOT affect git or your source files");
        eprint!("continue? [y/N] ");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("uninstall cancelled");
            return Ok(());
        }
    }

    let result = Repository::uninstall(cwd, keep_writignore)?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&result)?),
        _ => {
            if result.writ_dir_removed {
                println!(
                    "removed .writ/ ({} seal(s), {} tracked file(s))",
                    result.seals_existed, result.tracked_files
                );
            }

            if result.writignore_removed {
                println!("removed .writignore");
            }

            for hook in &result.hooks_removed {
                for f in &hook.files_removed {
                    println!("  - {f}");
                }
                for f in &hook.files_updated {
                    println!("  ~ {f} (writ section removed)");
                }
            }

            for w in &result.warnings {
                eprintln!("warning: {w}");
            }

            println!();
            println!("writ has been removed. your source files and git history are untouched.");
            println!("to reinstall: writ install");
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
                println!("{}", "nothing to seal — working directory clean".dimmed());
                println!("  tracked: {} file(s)", state.tracked_count);
            } else {
                println!(
                    "{} change(s) detected ({} tracked):\n",
                    state.changes.len().to_string().bold(),
                    state.tracked_count
                );
                for f in &state.changes {
                    match f.status {
                        writ_core::state::FileStatus::New => {
                            println!("  {}  {}", "+  new".green(), f.path.green());
                        }
                        writ_core::state::FileStatus::Modified => {
                            println!("  {}  {}", "~  mod".yellow(), f.path.yellow());
                        }
                        writ_core::state::FileStatus::Deleted => {
                            println!("  {}  {}", "-  del".red(), f.path.red());
                        }
                    };
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
    enforce_scope: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut repo = Repository::open(cwd)?;
    repo.set_enforce_scope(enforce_scope);

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

    println!("{} {}", "sealed".green().bold(), &seal.id[..12].cyan());

    if let Some(ref w) = conflict_warning {
        if w.is_clean {
            println!(
                "  {} HEAD moved ({} intervening seal(s)), but no file overlap",
                "note:".dimmed(),
                w.intervening_seals.len()
            );
        } else {
            println!(
                "  {} HEAD moved, {} overlapping file(s):",
                "WARNING:".yellow().bold(),
                w.overlapping_files.len()
            );
            for f in &w.overlapping_files {
                println!("    {} {}", "!".yellow(), f.red());
            }
            println!(
                "  consider running {} to reconcile",
                "`writ converge`".cyan()
            );
        }
    }
    println!("  {} {}", "summary:".dimmed(), seal.summary);
    if seal.verification.tests_passed.is_some()
        || seal.verification.tests_failed.is_some()
        || seal.verification.linted
    {
        print!("  {}:", "verified".dimmed());
        if let Some(p) = seal.verification.tests_passed {
            print!(" {} passed", p.to_string().green());
        }
        if let Some(f) = seal.verification.tests_failed {
            print!(" {} failed", f.to_string().red());
        }
        if seal.verification.linted {
            print!(" {}", "linted".green());
        }
        println!();
    }
    println!("  {} {} file(s)", "changes:".dimmed(), seal.changes.len());
    for c in &seal.changes {
        let marker = match c.change_type {
            ChangeType::Added => "+".green(),
            ChangeType::Modified => "~".yellow(),
            ChangeType::Deleted => "-".red(),
        };
        println!("    {marker} {}", c.path);
    }

    if seal.changes.is_empty() && !seal.summary.is_empty() && !allow_empty {
        println!(
            "  {} 0 file changes but summary is non-empty.",
            "HINT:".yellow().bold()
        );
        println!("        Another agent may have sealed overlapping files first.");
        println!(
            "        Check {} for file ownership.",
            "`writ context`".cyan()
        );
    }

    if let Some(ref sid) = seal.spec_id {
        let changed: Vec<String> = seal.changes.iter().map(|c| c.path.clone()).collect();
        if let Some(w) = repo.check_file_scope(sid, &changed) {
            println!(
                "  {} {} file(s) outside spec '{}' scope:",
                "SCOPE:".yellow().bold(),
                w.out_of_scope_files.len(),
                w.spec_id.cyan()
            );
            for f in &w.out_of_scope_files {
                println!("    {} {}", "!".yellow(), f.red());
            }
        }

        if seal.status == TaskStatus::Complete {
            let prior_seals = repo.spec_log(sid).unwrap_or_default();
            let has_in_progress = prior_seals
                .iter()
                .any(|s| s.id != seal.id && s.status == TaskStatus::InProgress);
            if !has_in_progress && prior_seals.len() <= 1 {
                eprintln!(
                    "  {} This is the only seal for spec '{}' and it's marked 'complete'.",
                    "HINT:".yellow().bold(),
                    sid.cyan()
                );
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
                println!("{}", "no seals yet".dimmed());
                return Ok(());
            }

            for (i, seal) in seals.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("{} {}", "seal".yellow(), seal.id[..12].yellow().bold());
                println!("  agent:   {}", seal.agent.id.cyan());
                println!(
                    "  time:    {}",
                    seal.timestamp
                        .format("%Y-%m-%d %H:%M:%S UTC")
                        .to_string()
                        .dimmed()
                );
                if let Some(ref spec) = seal.spec_id {
                    println!("  spec:    {}", spec.cyan());
                }
                let status_str = format!("{:?}", seal.status);
                let colored_status = match seal.status {
                    TaskStatus::Complete => status_str.green(),
                    TaskStatus::Blocked => status_str.red(),
                    TaskStatus::InProgress => status_str.yellow(),
                };
                println!("  status:  {colored_status}");
                println!("  summary: {}", seal.summary);
                println!("  changes: {} file(s)", seal.changes.len());
                if !seal.warnings.is_empty() {
                    for w in &seal.warnings {
                        println!("  {}", format!("WARNING: {w}").yellow());
                    }
                }
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
                "  {} {} diverged branch(es) with {} seal(s) not reachable from HEAD:",
                "WARNING:".yellow().bold(),
                diverged.len(),
                total_seals,
            );
            for b in &diverged {
                eprintln!(
                    "    branch '{}': {} seal(s) by {} (tip: {})",
                    b.spec_id.cyan(),
                    b.seal_count,
                    b.agents.join(", ").cyan(),
                    &b.tip_seal[..12.min(b.tip_seal.len())],
                );
            }
            eprintln!(
                "  Run {} to merge, or {} to inspect.",
                "'writ converge'".cyan(),
                "'writ log --spec <id>'".cyan()
            );
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
                    println!("  {marker} {} (+{}, -{})", f.path, f.additions, f.deletions);
                }
            }
        }
        _ => {
            // Human-readable unified diff format
            if diff_output.files.is_empty() {
                println!("{}", "no changes".dimmed());
            } else {
                for file_diff in &diff_output.files {
                    if file_diff.is_binary {
                        println!(
                            "{}",
                            format!("Binary file {} differs", file_diff.path).dimmed()
                        );
                        continue;
                    }

                    println!("{}", format!("--- a/{}", file_diff.path).bold());
                    println!("{}", format!("+++ b/{}", file_diff.path).bold());

                    for hunk in &file_diff.hunks {
                        println!(
                            "{}",
                            format!(
                                "@@ -{},{} +{},{} @@",
                                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                            )
                            .cyan()
                        );
                        for line in &hunk.lines {
                            match line.op {
                                LineOp::Add => {
                                    println!("{}", format!("+{}", line.content).green());
                                }
                                LineOp::Remove => {
                                    println!("{}", format!("-{}", line.content).red());
                                }
                                LineOp::Context => {
                                    println!(" {}", line.content);
                                }
                            }
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
    for_agent: Option<String>,
    seal_limit: usize,
    status: Option<String>,
    agent: Option<String>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let scope = if let Some(id) = spec {
        ContextScope::Spec(id)
    } else if let Some(id) = for_agent {
        ContextScope::Agent(id)
    } else {
        ContextScope::Full
    };

    let filter_status = match status.as_deref() {
        Some("in-progress") => Some(TaskStatus::InProgress),
        Some("complete") => Some(TaskStatus::Complete),
        Some("blocked") => Some(TaskStatus::Blocked),
        Some(other) => {
            return Err(format!(
                "unknown status filter: '{other}' (use in-progress, complete, or blocked)"
            )
            .into());
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
            println!("{}\n", "=== writ context ===".bold());

            if let Some(ref ss) = ctx.session_summary {
                println!("  {} {}", "SESSION COMPLETE:".green().bold(), ss.headline);
                println!("     {} file(s) changed. {}", ss.files_changed, ss.message);
                println!();
            }

            if let Some(ref spec) = ctx.active_spec {
                println!("{} {} — {}", "spec:".bold(), spec.id.cyan(), spec.title);
                let status_str = format!("{:?}", spec.status);
                let colored_status = match spec.status {
                    writ_core::spec::SpecStatus::Complete => status_str.green(),
                    writ_core::spec::SpecStatus::Blocked => status_str.red(),
                    writ_core::spec::SpecStatus::InProgress => status_str.yellow(),
                    writ_core::spec::SpecStatus::Pending => status_str.dimmed(),
                };
                println!("  status: {colored_status}");
                println!();
            }

            println!("{}:", "working state".bold());
            if ctx.working_state.clean {
                println!(
                    "  {} ({} tracked)",
                    "clean".green(),
                    ctx.working_state.tracked_count
                );
            } else {
                for f in &ctx.working_state.new_files {
                    println!("  {}  {}", "+  new".green(), f.green());
                }
                for f in &ctx.working_state.modified_files {
                    println!("  {}  {}", "~  mod".yellow(), f.yellow());
                }
                for f in &ctx.working_state.deleted_files {
                    println!("  {}  {}", "-  del".red(), f.red());
                }
            }
            println!();

            if let Some(ref nudge) = ctx.seal_nudge {
                println!("  {} {}", "WARNING:".yellow().bold(), nudge.message);
                println!();
            }

            if let Some(ref pc) = ctx.pending_changes {
                println!(
                    "{} {} file(s), {}, {}",
                    "pending:".bold(),
                    pc.files_changed,
                    format!("+{}", pc.total_additions).green(),
                    format!("-{}", pc.total_deletions).red()
                );
                println!();
            }

            if !ctx.recent_seals.is_empty() {
                println!("{}:", "recent seals".bold());
                for s in &ctx.recent_seals {
                    let spec_part = s
                        .spec_id
                        .as_deref()
                        .map(|id| format!(" spec:{}", id.cyan()))
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
                    let status_colored = match s.status.as_str() {
                        "Complete" | "complete" => s.status.green(),
                        "Blocked" | "blocked" => s.status.red(),
                        _ => s.status.yellow(),
                    };
                    println!(
                        "  {} {} [{}] — {}{}{}",
                        s.id.yellow(),
                        s.agent.cyan(),
                        status_colored,
                        s.summary,
                        spec_part,
                        verify_part
                    );
                    for p in &s.changed_paths {
                        println!("    {} {p}", "→".dimmed());
                    }
                }
                println!();
            }

            println!("tracked: {} file(s)", ctx.tracked_files);

            if !ctx.available_operations.is_empty() {
                println!();
                println!("{}:", "available operations".bold());
                for op in &ctx.available_operations {
                    println!("  {}", op.dimmed());
                }
            }

            if !ctx.file_scope_violations.is_empty() {
                println!();
                println!("{}:", "file scope violations".red().bold());
                for v in &ctx.file_scope_violations {
                    println!(
                        "  seal {} ({}) — {} file(s) outside spec '{}' scope:",
                        v.seal_id.yellow(),
                        v.agent_id.cyan(),
                        v.out_of_scope_files.len(),
                        v.spec_id
                    );
                    for f in &v.out_of_scope_files {
                        println!("    {} {}", "!".red(), f.red());
                    }
                }
            }

            {
                let risk = &ctx.integration_risk;
                println!();
                println!(
                    "  {} {} (score: {})",
                    "INTEGRATION RISK:".bold(),
                    color_risk_level(&risk.level),
                    risk.score
                );
                for f in &risk.factors {
                    println!("    {} {f}", "-".dimmed());
                }
            }

            if ctx.convergence_recommended {
                println!();
                println!("  {}", "*** CONVERGENCE RECOMMENDED ***".yellow().bold());
                println!(
                    "  Diverged branches detected — run {} to merge them.",
                    "`writ converge`".cyan()
                );
            }

            print_diverged_branch_warnings(&repo);
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(&ctx)?);
        }
    }

    Ok(())
}

fn cmd_summary(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let summary = repo.summary()?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        "commit" => {
            let files = summary.files_changed.len();
            if summary.convergence_recommended {
                println!(
                    "{} ({} files, {} diverged)",
                    summary.headline, files, summary.diverged_branch_count
                );
            } else {
                println!("{} ({} files)", summary.headline, files);
            }
        }
        "pr" => {
            println!("{}", summary.commit_message);
        }
        _ => {
            println!(
                "{}",
                "══════════════════════════════════════════════════════════════".bold()
            );
            println!("  {}", "WRIT SESSION SUMMARY".bold());
            println!(
                "{}",
                "══════════════════════════════════════════════════════════════".bold()
            );
            println!();
            println!("  {}", summary.headline);
            println!();

            if !summary.specs_summary.is_empty() {
                println!("  {}:", "Specs".bold());
                for s in &summary.specs_summary {
                    let icon = match s.status.as_str() {
                        "complete" => "✓".green(),
                        "in-progress" => "…".yellow(),
                        "blocked" => "✗".red(),
                        _ => "·".dimmed(),
                    };
                    println!(
                        "    {icon} {:<25} [{}] {} seal(s) by {}",
                        s.id.cyan(),
                        s.status,
                        s.seal_count,
                        s.agents.join(", ").cyan(),
                    );
                    println!("      {}", s.title);
                }
                println!();
            }

            if !summary.agents.is_empty() {
                println!("  {}:", "Agents".bold());
                for a in &summary.agents {
                    println!(
                        "    {:<20} {} seal(s) on {}",
                        a.id.cyan(),
                        a.seal_count,
                        a.specs_touched.join(", ").cyan(),
                    );
                }
                println!();
            }

            println!(
                "  {} {} seal(s), {} file(s) changed",
                "Total:".bold(),
                summary.total_seals.to_string().bold(),
                summary.files_changed.len().to_string().bold(),
            );

            if !summary.files_to_stage.is_empty() {
                println!();
                println!("  Files to stage ({}):", summary.files_to_stage.len());
                for f in &summary.files_to_stage {
                    println!("    {f}");
                }
            }

            if summary.convergence_recommended {
                println!();
                println!(
                    "  {} {} diverged branch(es) — run {} before committing.",
                    "⚠".yellow().bold(),
                    summary.diverged_branch_count,
                    "`writ converge`".cyan()
                );
            }

            println!();
            println!(
                "{}",
                "──────────────────────────────────────────────────────────────".dimmed()
            );
            println!(
                "  Commit message (use {}):",
                "`writ summary --format commit`".cyan()
            );
            println!(
                "{}",
                "──────────────────────────────────────────────────────────────".dimmed()
            );
            println!();
            let files = summary.files_changed.len();
            if summary.convergence_recommended {
                println!("  {}", summary.headline);
                println!(
                    "  ({} files, {} diverged branch(es))",
                    files, summary.diverged_branch_count
                );
            } else {
                println!("  {} ({} files)", summary.headline, files);
            }
            println!();
            println!(
                "  For full PR description: {}",
                "writ summary --format pr".cyan()
            );
            println!();
            println!(
                "{}",
                "══════════════════════════════════════════════════════════════".bold()
            );
        }
    }

    Ok(())
}

fn cmd_finish(cwd: &PathBuf, full: bool, dry_run: bool) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let summary = repo.summary()?;

    let commit_message = if full {
        summary.commit_message.clone()
    } else {
        let files = summary.files_changed.len();
        if summary.convergence_recommended {
            format!(
                "{} ({} files, {} diverged)",
                summary.headline, files, summary.diverged_branch_count
            )
        } else {
            format!("{} ({} files)", summary.headline, files)
        }
    };

    if dry_run {
        println!("DRY RUN — would execute:");
        println!();
        println!("  git add .");
        println!(
            "  git commit -m \"{}\"",
            commit_message.lines().next().unwrap_or("")
        );
        if full {
            println!();
            println!("Full commit message:");
            println!("──────────────────────────────────────────────────────────────");
            println!("{commit_message}");
            println!("──────────────────────────────────────────────────────────────");
        }
        println!();
        println!(
            "Files that would be staged ({}):",
            summary.files_to_stage.len()
        );
        for f in &summary.files_to_stage {
            println!("  {f}");
        }
        return Ok(());
    }

    // Verify git is available.
    let git_check = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--is-inside-work-tree")
        .current_dir(cwd)
        .output();

    match git_check {
        Ok(output) if output.status.success() => {}
        _ => {
            eprintln!("error: not inside a git repository. `writ finish` requires git.");
            eprintln!("hint: use `writ summary --format commit` to get the message manually.");
            std::process::exit(1);
        }
    }

    // git add .
    let add = std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(cwd)
        .output()?;

    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        eprintln!("error: git add failed: {stderr}");
        std::process::exit(1);
    }

    // git commit -m "<message>"
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", &commit_message])
        .current_dir(cwd)
        .output()?;

    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        if stderr.contains("nothing to commit") {
            println!("nothing to commit — working tree clean");
            return Ok(());
        }
        eprintln!("error: git commit failed: {stderr}");
        std::process::exit(1);
    }

    let stdout = String::from_utf8_lossy(&commit.stdout);
    println!("{}", stdout.trim());
    println!();
    println!("committed with message:");
    println!("  {}", commit_message.lines().next().unwrap_or(""));

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
        eprintln!("warning: this will overwrite your working directory to match seal {short}");
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
            println!("{} {}", "seal".bold(), &seal.id[..12].cyan());
            println!("  {} {}", "full id:".dimmed(), seal.id);
            println!(
                "  {} {} ({:?})",
                "agent:".dimmed(),
                seal.agent.id.cyan(),
                seal.agent.agent_type
            );
            println!(
                "  {} {}",
                "time:".dimmed(),
                seal.timestamp
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string()
                    .dimmed()
            );
            if let Some(ref parent) = seal.parent {
                println!("  {} {}", "parent:".dimmed(), &parent[..12].cyan());
            } else {
                println!(
                    "  {} {}",
                    "parent:".dimmed(),
                    "(none — initial seal)".dimmed()
                );
            }
            println!("  {} {}", "tree:".dimmed(), &seal.tree[..12]);
            if let Some(ref spec) = seal.spec_id {
                println!("  {} {}", "spec:".dimmed(), spec.cyan());
            }
            let status_str = format!("{:?}", seal.status);
            let status_colored = match seal.status {
                TaskStatus::Complete => status_str.green(),
                TaskStatus::InProgress => status_str.yellow(),
                TaskStatus::Blocked => status_str.red(),
            };
            println!("  {} {}", "status:".dimmed(), status_colored);
            if seal.verification.tests_passed.is_some()
                || seal.verification.tests_failed.is_some()
                || seal.verification.linted
            {
                print!("  {}:", "verified".dimmed());
                if let Some(p) = seal.verification.tests_passed {
                    print!(" {} passed", p.to_string().green());
                }
                if let Some(f) = seal.verification.tests_failed {
                    print!(" {} failed", f.to_string().red());
                }
                if seal.verification.linted {
                    print!(" {}", "linted".green());
                }
                println!();
            }
            println!("  {} {}", "summary:".dimmed(), seal.summary);
            println!("  {} {} file(s)", "changes:".dimmed(), seal.changes.len());
            for c in &seal.changes {
                let marker = match c.change_type {
                    ChangeType::Added => "+".green(),
                    ChangeType::Modified => "~".yellow(),
                    ChangeType::Deleted => "-".red(),
                };
                println!("    {marker} {}", c.path);
            }
            if !seal.warnings.is_empty() {
                println!("  {}:", "warnings".yellow());
                for w in &seal.warnings {
                    println!("    {} {w}", "!".yellow());
                }
            }

            if show_diff {
                let diff = repo.diff_seal(seal_id)?;
                println!();
                for file_diff in &diff.files {
                    if file_diff.is_binary {
                        println!(
                            "{} {} differs",
                            "Binary file".dimmed(),
                            file_diff.path.bold()
                        );
                        continue;
                    }
                    println!("{}", format!("--- a/{}", file_diff.path).bold());
                    println!("{}", format!("+++ b/{}", file_diff.path).bold());
                    for hunk in &file_diff.hunks {
                        println!(
                            "{}",
                            format!(
                                "@@ -{},{} +{},{} @@",
                                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                            )
                            .cyan()
                        );
                        for line in &hunk.lines {
                            match line.op {
                                LineOp::Add => {
                                    println!("{}", format!("+{}", line.content).green())
                                }
                                LineOp::Remove => {
                                    println!("{}", format!("-{}", line.content).red())
                                }
                                LineOp::Context => println!(" {}", line.content),
                            };
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

fn cmd_spec_status(
    cwd: &PathBuf,
    state_filter: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use writ_core::spec::LifecycleState;

    let repo = Repository::open(cwd)?;
    let mut specs = repo.list_specs()?;

    // Filter by lifecycle state if requested.
    if let Some(state) = state_filter {
        let target = match state.to_lowercase().as_str() {
            "active" => LifecycleState::Active,
            "stale" => LifecycleState::Stale,
            "completed" => LifecycleState::Completed,
            "cancelled" => LifecycleState::Cancelled,
            "archived" => LifecycleState::Archived,
            other => {
                return Err(format!(
                    "unknown lifecycle state: '{}'. Valid: active, stale, completed, cancelled, archived",
                    other
                ).into());
            }
        };
        specs.retain(|s| s.lifecycle_state == target);
    }

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&specs)?);
        return Ok(());
    }

    if specs.is_empty() {
        if let Some(state) = state_filter {
            println!("no specs with lifecycle state '{state}'");
        } else {
            println!("no specs registered");
        }
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
        let lifecycle = format!("{:?}", spec.lifecycle_state);
        println!(
            "  {status_marker}{:<20} {:?}  [{lifecycle}]  ({seal_count} seal(s))",
            spec.id, spec.status
        );
    }

    Ok(())
}

fn cmd_spec_cancel(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.cancel_spec(id)?;
    println!("spec '{}' cancelled", id);
    Ok(())
}

fn cmd_spec_complete(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.complete_spec(id)?;
    println!("spec '{}' lifecycle completed", id);
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
            let status = if report.is_clean {
                "clean".green().to_string()
            } else {
                "conflict".red().to_string()
            };
            println!(
                "{} + {} = {} (auto:{} conflicts:{} left:{} right:{})",
                report.left_spec.cyan(),
                report.right_spec.cyan(),
                status,
                report.auto_merged.len().to_string().green(),
                report.conflicts.len().to_string().red(),
                report.left_only.len(),
                report.right_only.len(),
            );
        }
        _ => {
            println!(
                "{} {} + {}",
                "convergence:".bold(),
                report.left_spec.cyan(),
                report.right_spec.cyan()
            );

            if let Some(ref base) = report.base_seal_id {
                println!(
                    "  base: seal {}",
                    base[..12.min(base.len())].yellow().bold()
                );
            } else {
                println!("  base: {}", "(empty state)".dimmed());
            }
            println!();

            if !report.auto_merged.is_empty() {
                println!(
                    "  {} ({} file(s)):",
                    "auto-merged".green(),
                    report.auto_merged.len()
                );
                for m in &report.auto_merged {
                    println!("    {} {}", "~".green(), m.path);
                }
                println!();
            }

            if !report.left_only.is_empty() {
                println!(
                    "  {} ({} file(s)):",
                    "left only".cyan(),
                    report.left_only.len()
                );
                for p in &report.left_only {
                    println!("    {} {p}", "~".cyan());
                }
                println!();
            }

            if !report.right_only.is_empty() {
                println!(
                    "  {} ({} file(s)):",
                    "right only".cyan(),
                    report.right_only.len()
                );
                for p in &report.right_only {
                    println!("    {} {p}", "~".cyan());
                }
                println!();
            }

            if !report.conflicts.is_empty() {
                println!(
                    "  {} ({} file(s)):",
                    "conflicts".red().bold(),
                    report.conflicts.len()
                );
                for c in &report.conflicts {
                    println!("    {}:", c.path.red());
                    for (i, region) in c.regions.iter().enumerate() {
                        println!(
                            "      region {} (line {}):",
                            i + 1,
                            region.base_start.to_string().dimmed()
                        );
                        if !region.base_lines.is_empty() {
                            for bl in &region.base_lines {
                                println!("        {}  {bl}", "base:".dimmed());
                            }
                        }
                        for ll in &region.left_lines {
                            println!("        {}  {ll}", "left:".cyan());
                        }
                        for rl in &region.right_lines {
                            println!("        {} {rl}", "right:".cyan());
                        }
                    }
                }
                println!();
            }

            if report.is_clean {
                println!("  result: {}", "clean".green().bold());
                if !apply {
                    println!("  run with {} to write merged files", "--apply".cyan());
                }
            } else {
                println!(
                    "  result: {} conflict(s) — resolve before applying",
                    report.conflicts.len().to_string().red().bold()
                );
            }
        }
    }

    if apply {
        if !report.is_clean {
            eprintln!(
                "{} cannot {} with unresolved conflicts",
                "error:".red().bold(),
                "--apply".cyan()
            );
            eprintln!("  use JSON output to inspect conflicts and resolve programmatically");
            std::process::exit(1);
        }
        repo.apply_convergence(&report, &[])?;
        if format != "json" {
            println!(
                "\n  {} merged files written to working directory",
                "applied —".green().bold()
            );
            println!(
                "  seal with {} to capture the converged state",
                "writ seal".cyan()
            );
        }
    }

    Ok(())
}

fn cmd_converge_all(
    cwd: &PathBuf,
    format: &str,
    apply: bool,
    dry_run: bool,
    strategy_str: &str,
    auto_resolve: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let strategy = match strategy_str {
        "escalate" => writ_core::convergence::ConvergeStrategy::Escalate,
        "most-recent" => {
            eprintln!("warning: 'most-recent' strategy is deprecated and will be removed; use 'escalate' instead");
            #[allow(deprecated)]
            writ_core::convergence::ConvergeStrategy::MostRecent
        }
        "manual" => writ_core::convergence::ConvergeStrategy::Manual,
        "orchestrator" => writ_core::convergence::ConvergeStrategy::Orchestrator,
        other => {
            return Err(format!(
                "unknown strategy '{}' (use 'escalate', 'manual', or 'orchestrator')",
                other
            )
            .into());
        }
    };

    let repo = Repository::open(cwd)?;

    // Capture pre-convergence risk for delta reporting.
    let pre_risk = {
        let ctx = repo.context(
            writ_core::context::ContextScope::Full,
            50,
            &writ_core::context::ContextFilter::default(),
        )?;
        ctx.integration_risk.clone()
    };

    // Dry run: run without apply, then show the report.
    let effective_apply = apply && !dry_run;

    let spinner = if format != "json" {
        Some(make_spinner("converging branches..."))
    } else {
        None
    };

    let report = repo.converge_all(strategy, effective_apply)?;

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    if report.merges.is_empty() {
        match format {
            "json" => println!("{}", serde_json::to_string_pretty(&report)?),
            _ => println!("No diverged branches — nothing to converge."),
        }
        return Ok(());
    }

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        _ => {
            println!(
                "{} {} branch(es), strategy: {}",
                "converge-all:".bold(),
                report.merge_order.len(),
                report.strategy.cyan(),
            );
            println!("  base: spec '{}'", report.base_spec.cyan());
            println!("  merging: {}", report.merge_order.join(", ").cyan());
            println!();

            if dry_run {
                println!(
                    "  {} showing merge plan without applying:",
                    "DRY RUN —".yellow().bold()
                );
                println!();
            }

            for step in &report.merges {
                println!(
                    "  {} {} {} {} {}",
                    "---".dimmed(),
                    step.left_spec.cyan(),
                    "<-".dimmed(),
                    step.right_spec.cyan(),
                    "---".dimmed()
                );
                if let Some(ref err) = step.error {
                    println!("    {} {err}", "ERROR:".red().bold());
                } else {
                    let clean_display = if step.clean {
                        "true".green()
                    } else {
                        "false".yellow()
                    };
                    println!("    clean: {clean_display}");
                    println!(
                        "    auto-merged: {}, left-only: {}, right-only: {}, conflicts: {}",
                        step.auto_merged.to_string().green(),
                        step.left_only,
                        step.right_only,
                        if step.conflicts > 0 {
                            step.conflicts.to_string().red()
                        } else {
                            step.conflicts.to_string().green()
                        },
                    );
                    if !step.conflict_files.is_empty() {
                        for path in &step.conflict_files {
                            println!("      {} {}", "CONFLICT:".red().bold(), path.red());
                        }
                    }
                    for res in &step.resolutions {
                        println!(
                            "      {} {} (strategy: {}, chose: {})",
                            "resolved:".green(),
                            res.path,
                            res.strategy,
                            res.chosen_spec.as_deref().unwrap_or("n/a"),
                        );
                    }
                    if effective_apply && step.clean {
                        println!("    {}", "applied".green().bold());
                    }
                }
                println!();
            }

            if !report.warnings.is_empty() {
                println!("  {}:", "WARNINGS".yellow().bold());
                for w in &report.warnings {
                    println!("    {} {w}", "-".yellow());
                }
                println!();
            }

            println!(
                "  {} {} branch(es) processed, {} auto-merged file(s), {} conflict(s), {} resolved",
                "SUMMARY:".bold(),
                report.merge_order.len(),
                report.total_auto_merged.to_string().green(),
                if report.total_conflicts > 0 {
                    report.total_conflicts.to_string().red()
                } else {
                    report.total_conflicts.to_string().green()
                },
                report.total_resolutions,
            );
            if report.degraded {
                println!(
                    "  {} DEGRADED — most-recent strategy discarded content; review quality report",
                    "STATUS:".red().bold()
                );
            }
            if !report.escalations.is_empty() {
                println!(
                    "  {} {} conflict(s) require review",
                    "ESCALATIONS:".red().bold(),
                    report.escalations.len()
                );
                for esc in &report.escalations {
                    let conf_info = esc
                        .suggestion_confidence
                        .map(|c| format!(" [confidence: {:.0}%]", c * 100.0))
                        .unwrap_or_default();
                    println!(
                        "    {} {}: {}{}",
                        "✗".red(),
                        esc.file_path.red(),
                        esc.reason,
                        conf_info.dimmed()
                    );
                }

                // Guided next steps for escalated conflicts.
                println!();
                println!("  {}:", "NEXT STEPS".bold());
                println!(
                    "    1. Review {} escalated file(s):",
                    report.escalations.len()
                );
                for esc in &report.escalations {
                    println!(
                        "       {}",
                        format!("writ resolve {} --accept best", esc.file_path).cyan()
                    );
                }
                let min_conf = repo
                    .settings()
                    .convergence
                    .auto_resolve_min_confidence
                    .unwrap_or(0.85);
                println!(
                    "    2. Or auto-resolve all above {:.0}% confidence:",
                    min_conf * 100.0
                );
                println!(
                    "       {}",
                    "writ converge-all --apply --auto-resolve".cyan()
                );
                println!("    3. After resolving, seal the result:");
                println!(
                    "       {}",
                    format!(
                        "writ seal -s \"converged {} branch(es)\" --agent <your-id>",
                        report.merge_order.len()
                    )
                    .cyan()
                );
            }

            if let Some(ref qr) = report.quality_report {
                println!();
                let score_colored = if qr.quality_score >= 80 {
                    format!("{}/100", qr.quality_score).green()
                } else if qr.quality_score >= 50 {
                    format!("{}/100", qr.quality_score).yellow()
                } else {
                    format!("{}/100", qr.quality_score).red()
                };
                println!("  {} (score: {})", "QUALITY REPORT".bold(), score_colored);
                println!("    {}", qr.summary);
                if qr.min_confidence < 1.0 {
                    println!(
                        "    confidence: min={}% avg={}%",
                        (qr.min_confidence * 100.0).round() as u32,
                        (qr.avg_confidence * 100.0).round() as u32,
                    );
                }
                if !qr.file_decisions.is_empty() {
                    println!();
                    println!("    {}:", "File decisions".bold());
                    for d in &qr.file_decisions {
                        let spec_info = d.chosen_spec.as_deref().unwrap_or("-");
                        println!(
                            "      {:30} {:18} {:4} lines  (spec: {})",
                            d.path, d.decision, d.chosen_lines, spec_info,
                        );
                        for alt in &d.alternatives {
                            println!(
                                "        {} {} ({} lines) — {}",
                                "discarded:".dimmed(),
                                alt.spec,
                                alt.lines,
                                alt.reason,
                            );
                        }
                    }
                }
                if !qr.consistency_checks.is_empty() {
                    println!();
                    println!("    {}:", "Consistency checks".bold());
                    for c in &qr.consistency_checks {
                        let status = if c.consistent {
                            "PASS".green()
                        } else {
                            "FAIL".red()
                        };
                        println!("      {} {}", status, c.metric);
                        if let Some(ref w) = c.warning {
                            println!("        {}", w.yellow());
                        }
                    }
                }
            }

            if dry_run {
                println!("  {}", "(dry run — no changes applied)".dimmed());
                println!(
                    "  Run with --apply to merge: {}",
                    format!("writ converge-all --apply --strategy {strategy_str}").cyan()
                );
            } else if effective_apply {
                // Post-convergence risk re-evaluation.
                let post_risk = {
                    let ctx = repo.context(
                        writ_core::context::ContextScope::Full,
                        50,
                        &writ_core::context::ContextFilter::default(),
                    )?;
                    ctx.integration_risk.clone()
                };

                let pre_score = pre_risk.score;
                let post_score = post_risk.score;
                let pre_level = pre_risk.level.as_str();
                let post_level = post_risk.level.as_str();

                println!();
                println!(
                    "  {} {} ({}) -> {} ({})",
                    "INTEGRATION RISK:".bold(),
                    color_risk_level(pre_level),
                    pre_score,
                    color_risk_level(post_level),
                    post_score,
                );
                if post_score < pre_score {
                    println!(
                        "    {} Risk reduced by {} points",
                        "↓".green(),
                        pre_score - post_score
                    );
                } else if post_score == 0 {
                    println!(
                        "    {}",
                        "All clear — no remaining integration risk".green()
                    );
                }

                println!();
                println!(
                    "  {} Seal the converged state:",
                    "All merges applied.".green().bold()
                );
                println!(
                    "    {}",
                    format!(
                        "writ seal -s \"converge-all: merged {} branch(es)\" --agent convergence-bot --status complete",
                        report.merge_order.len(),
                    )
                    .cyan()
                );
            } else {
                println!(
                    "  Run with --apply to merge: {}",
                    format!("writ converge-all --apply --strategy {strategy_str}").cyan()
                );
            }
        }
    }

    // Auto-resolve: apply highest-confidence suggestions for escalated conflicts.
    let auto_resolve_enabled =
        auto_resolve || repo.settings().convergence.auto_resolve.unwrap_or(false);

    if auto_resolve_enabled && effective_apply && !report.escalations.is_empty() {
        let min_conf = repo
            .settings()
            .convergence
            .auto_resolve_min_confidence
            .unwrap_or(0.85);

        let writ_dir = cwd.join(".writ");
        let logger = writ_core::security::SecurityEventLogger::new(&writ_dir);

        let mut resolved_count = 0usize;
        let mut remaining_escalations = Vec::new();

        for esc in &report.escalations {
            if let (Some(content), Some(conf)) = (&esc.suggested_content, esc.suggestion_confidence)
            {
                if conf >= min_conf {
                    let file_path = cwd.join(&esc.file_path);
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&file_path, content)?;
                    resolved_count += 1;

                    let _ = logger.emit_convergence_event(
                        "convergence_auto_resolved",
                        writ_core::security::Severity::Info,
                        &format!(
                            "auto-resolved '{}' (confidence: {:.0}%, action: {})",
                            esc.file_path,
                            conf * 100.0,
                            esc.recommended_action
                        ),
                    );

                    if format != "json" {
                        println!(
                            "  {} {} {} (confidence: {:.0}%)",
                            "AUTO-RESOLVED:".green().bold(),
                            "✓".green(),
                            esc.file_path,
                            conf * 100.0
                        );
                    }
                } else {
                    remaining_escalations.push(esc.clone());
                }
            } else {
                remaining_escalations.push(esc.clone());
            }
        }

        // Save unresolved escalations for `writ resolve`.
        if !remaining_escalations.is_empty() {
            let pending_dir = writ_dir.join("convergence");
            std::fs::create_dir_all(&pending_dir)?;
            let pending_path = pending_dir.join("pending.json");
            let json = serde_json::to_string_pretty(&remaining_escalations)?;
            std::fs::write(&pending_path, json)?;

            if format != "json" {
                println!(
                    "  {} {} escalation(s) saved for {}",
                    "PENDING:".yellow().bold(),
                    remaining_escalations.len(),
                    "`writ resolve`".cyan()
                );
            }
        } else {
            // Clean up pending file if all resolved.
            let pending_path = writ_dir.join("convergence/pending.json");
            let _ = std::fs::remove_file(&pending_path);
        }

        if format != "json" && resolved_count > 0 {
            println!(
                "  {} {} file(s) auto-resolved above {:.0}% confidence",
                "SUMMARY:".bold(),
                resolved_count,
                min_conf * 100.0
            );
        }
    } else if effective_apply && !report.escalations.is_empty() {
        // Not auto-resolving — save all escalations for `writ resolve`.
        let writ_dir = cwd.join(".writ");
        let pending_dir = writ_dir.join("convergence");
        std::fs::create_dir_all(&pending_dir)?;
        let pending_path = pending_dir.join("pending.json");
        let json = serde_json::to_string_pretty(&report.escalations)?;
        std::fs::write(&pending_path, json)?;
    }

    Ok(())
}

// -------------------------------------------------------------------
// Resolve command
// -------------------------------------------------------------------

fn cmd_resolve(
    cwd: &PathBuf,
    file: Option<String>,
    accept: Option<String>,
    all: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    let pending_path = writ_dir.join("convergence/pending.json");

    let escalations: Vec<writ_core::convergence::PipelineEscalation> = if pending_path.exists() {
        let data = std::fs::read_to_string(&pending_path)?;
        serde_json::from_str(&data)?
    } else {
        Vec::new()
    };

    if format == "json" {
        if all && accept.is_some() {
            // Resolve all in JSON mode.
            let accept = accept.as_deref().unwrap();
            for esc in &escalations {
                resolve_single(cwd, esc, accept)?;
            }
            let _ = std::fs::remove_file(&pending_path);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "resolved": escalations.len(),
                    "remaining": 0,
                    "accept": accept,
                }))?
            );
        } else if let Some(ref file) = file {
            let accept = accept.as_deref().unwrap_or("best");
            let esc = escalations
                .iter()
                .find(|e| e.file_path == *file)
                .ok_or_else(|| format!("'{}' is not in the pending escalation list", file))?;
            resolve_single(cwd, esc, accept)?;
            let remaining: Vec<_> = escalations
                .into_iter()
                .filter(|e| e.file_path != *file)
                .collect();
            if remaining.is_empty() {
                let _ = std::fs::remove_file(&pending_path);
            } else {
                let json = serde_json::to_string_pretty(&remaining)?;
                std::fs::write(&pending_path, json)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "resolved": file,
                    "accept": accept,
                    "remaining": remaining.len(),
                }))?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(&escalations)?);
        }
        return Ok(());
    }

    // Human-readable output.
    if escalations.is_empty() {
        println!(
            "No pending escalations. Run {} first.",
            "writ converge-all --apply".cyan()
        );
        return Ok(());
    }

    if all {
        let accept = accept
            .as_deref()
            .ok_or("--all requires --accept (left, right, both, or best)")?;
        for esc in &escalations {
            resolve_single(cwd, esc, accept)?;
            println!("  {} {} (--accept {})", "✓".green(), esc.file_path, accept);
        }
        let _ = std::fs::remove_file(&pending_path);
        println!(
            "\n{} {} file(s)",
            "resolved".green().bold(),
            escalations.len()
        );
    } else if let Some(ref file) = file {
        let accept = accept.as_deref().unwrap_or("best");
        let esc = escalations
            .iter()
            .find(|e| e.file_path == *file)
            .ok_or_else(|| format!("'{}' is not in the pending escalation list", file))?;
        resolve_single(cwd, esc, accept)?;
        let remaining: Vec<_> = escalations
            .into_iter()
            .filter(|e| e.file_path != *file)
            .collect();
        if remaining.is_empty() {
            let _ = std::fs::remove_file(&pending_path);
        } else {
            let json = serde_json::to_string_pretty(&remaining)?;
            std::fs::write(&pending_path, json)?;
        }
        println!("{} {}", "resolved".green().bold(), file);
        if !remaining.is_empty() {
            println!("  {} remaining escalation(s)", remaining.len());
        }
    } else {
        // List pending escalations.
        println!(
            "{} pending escalation(s):",
            escalations.len().to_string().bold()
        );
        println!();
        for esc in &escalations {
            let conf_info = esc
                .suggestion_confidence
                .map(|c| format!(" [confidence: {:.0}%]", c * 100.0))
                .unwrap_or_default();
            println!(
                "  {} {} ({}){}",
                "✗".red(),
                esc.file_path.red(),
                esc.reason,
                conf_info.dimmed()
            );
            println!("    {} {}", "recommended:".dimmed(), esc.recommended_action);
        }
        println!();
        println!("{}:", "Resolve with".bold());
        println!(
            "  {} to accept a specific resolution",
            "writ resolve <file> --accept left|right|both|best".cyan()
        );
        println!(
            "  {} to resolve all at once",
            "writ resolve --all --accept best".cyan()
        );
    }

    Ok(())
}

fn resolve_single(
    cwd: &PathBuf,
    esc: &writ_core::convergence::PipelineEscalation,
    accept: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = match accept {
        "left" => esc
            .left_content
            .as_ref()
            .ok_or("no left content available for this conflict")?
            .clone(),
        "right" => esc
            .right_content
            .as_ref()
            .ok_or("no right content available for this conflict")?
            .clone(),
        "both" => {
            let left = esc
                .left_content
                .as_ref()
                .ok_or("no left content available")?;
            let right = esc
                .right_content
                .as_ref()
                .ok_or("no right content available")?;
            format!("{}\n{}", left, right)
        }
        "best" => esc
            .suggested_content
            .as_ref()
            .ok_or(
                "no suggestion available for this conflict; use --accept left or --accept right",
            )?
            .clone(),
        other => {
            return Err(format!(
                "unknown accept value '{}' (expected left, right, both, best)",
                other
            )
            .into())
        }
    };

    let file_path = cwd.join(&esc.file_path);
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&file_path, content)?;
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
            println!(
                "imported git {} as seal {}",
                &result.git_commit[..12],
                &result.seal_id[..12]
            );
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
                    println!(
                        "  {} → {} — {}",
                        &e.seal_id[..12],
                        &e.git_commit[..12],
                        e.summary
                    );
                }
            }
        }
    }

    if pr_body && !result.exported.is_empty() {
        println!("\n--- PR Body ---\n");
        println!("## Agent Work Summary\n");
        println!(
            "Exported {} seal(s) from writ to branch `{}`.\n",
            result.seals_exported, result.branch
        );
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

fn cmd_bridge_status(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let status = repo.bridge_status()?;

    match format {
        "json" => println!("{}", serde_json::to_string_pretty(&status)?),
        _ => {
            if !status.initialized {
                println!("bridge not initialized — run `writ bridge import` first");
            } else {
                if let Some(ref imp) = status.last_import {
                    println!(
                        "last import: git {} → seal {}",
                        &imp.git_commit[..12],
                        &imp.seal_id[..12]
                    );
                    println!("  ref: {}", imp.git_ref);
                }
                if let Some(ref exp) = status.last_export {
                    println!(
                        "last export: seal {} → git {} (branch: {})",
                        &exp.seal_id[..12],
                        &exp.git_commit[..12],
                        exp.branch
                    );
                }
                println!("pending: {} seal(s) to export", status.pending_export_count);
            }
        }
    }

    Ok(())
}

fn cmd_push(cwd: &PathBuf, remote: &str, format: &str) -> Result<(), Box<dyn std::error::Error>> {
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

fn cmd_pull(cwd: &PathBuf, remote: &str, format: &str) -> Result<(), Box<dyn std::error::Error>> {
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
                        println!(
                            "    {} — field '{}': local='{}' remote='{}'",
                            c.spec_id, c.field, c.local_value, c.remote_value
                        );
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

fn cmd_remote_add(cwd: &PathBuf, name: &str, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.remote_add(name, path)?;
    println!("remote '{name}' added → {path}");
    Ok(())
}

fn cmd_remote_remove(cwd: &PathBuf, name: &str) -> Result<(), Box<dyn std::error::Error>> {
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

// ---------------------------------------------------------------------------
// writ verify
// ---------------------------------------------------------------------------

fn cmd_verify(
    cwd: &std::path::Path,
    chain: bool,
    all_chains: bool,
    seal_id: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    if let Some(id) = seal_id {
        return cmd_verify_seal(&repo, id, format);
    }

    if all_chains {
        return cmd_verify_all_chains(&repo, format);
    }

    if chain {
        return cmd_verify_chain(&repo, format);
    }

    // Default: verify the full chain
    cmd_verify_chain(&repo, format)
}

fn cmd_verify_seal(
    repo: &Repository,
    seal_id: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let full_id = repo.resolve_seal_id(seal_id)?;
    let seal = repo.load_seal(&full_id)?;
    let short_id = &seal.id[..12.min(seal.id.len())];

    // Pre-security seals: no crypto fields at all
    if !seal.is_secured() {
        match format {
            "json" => {
                let json = serde_json::json!({
                    "seal_id": seal.id,
                    "status": "pre-security",
                    "message": "seal created before cryptographic integrity was enabled"
                });
                println!("{}", serde_json::to_string_pretty(&json).unwrap());
            }
            _ => {
                println!("verify seal {short_id}");
                println!("  status: N/A (pre-security seal)");
            }
        }
        return Ok(());
    }

    let result = repo.verify_seal(&seal, None);

    // Determine signature status text
    let sig_status = match (seal.signature.is_some(), result.signature_valid) {
        (true, Some(true)) => ("OK", "verified".to_string()),
        (true, Some(false)) => ("FAIL", "signature verification failed".to_string()),
        (true, None) => ("N/A", "present (unverified — no key provided)".to_string()),
        (false, _) => ("N/A", "not present".to_string()),
    };

    let all_ok = result.content_hash_valid && result.chain_hash_valid;

    match format {
        "json" => {
            let json = serde_json::json!({
                "seal_id": seal.id,
                "valid": all_ok,
                "content_hash_valid": result.content_hash_valid,
                "chain_hash_valid": result.chain_hash_valid,
                "signature_present": result.signature_present,
                "signature_valid": result.signature_valid,
                "error": result.error,
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        _ => {
            println!("{} {}", "verify seal".bold(), short_id.yellow());
            let ch_display = if result.content_hash_valid {
                "OK".green()
            } else {
                "FAIL".red()
            };
            let cc_display = if result.chain_hash_valid {
                "OK".green()
            } else {
                "FAIL".red()
            };
            println!(
                "  content_hash: {} — {}",
                ch_display,
                seal.content_hash.as_deref().map_or("", |h| &h[..12])
            );
            println!(
                "  chain_hash:   {} — {}",
                cc_display,
                seal.chain_hash.as_deref().map_or("", |h| &h[..12])
            );
            let sig_display = match sig_status.0 {
                "OK" => "OK".green(),
                "FAIL" => "FAIL".red(),
                _ => "N/A".dimmed(),
            };
            println!("  signature:    {} — {}", sig_display, sig_status.1);
            if all_ok {
                println!("  result: {}", "VALID".green().bold());
            } else {
                println!(
                    "  result: {} — {}",
                    "INVALID".red().bold(),
                    result.error.as_deref().unwrap_or("unknown")
                );
            }
        }
    }

    Ok(())
}

fn cmd_verify_chain(repo: &Repository, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let spinner = if format != "json" {
        Some(make_spinner("verifying seal chain..."))
    } else {
        None
    };

    let result = repo.verify_chain(None)?;

    if let Some(sp) = spinner {
        sp.finish_and_clear();
    }

    if result.total_seals == 0 {
        match format {
            "json" => println!(r#"{{"valid":true,"seals_checked":0,"message":"empty chain"}}"#),
            _ => println!("no seals to verify"),
        }
        return Ok(());
    }

    match format {
        "json" => {
            let json = serde_json::json!({
                "valid": result.valid,
                "seals_checked": result.total_seals,
                "seals_verified": result.verified,
                "seals_unsecured": result.unsecured,
                "failures": result.failures.iter().map(|f| {
                    serde_json::json!({
                        "seal": &f.seal_id[..12.min(f.seal_id.len())],
                        "error": f.error,
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        _ => {
            println!(
                "{}: {} seals checked, {} verified, {} pre-security",
                "chain verification".bold(),
                result.total_seals,
                result.verified.to_string().green(),
                result.unsecured.to_string().dimmed()
            );
            if !result.failures.is_empty() {
                for f in &result.failures {
                    let short_id = &f.seal_id[..12.min(f.seal_id.len())];
                    println!(
                        "  {} {}: {}",
                        "FAIL".red().bold(),
                        short_id.yellow(),
                        f.error.as_deref().unwrap_or("unknown")
                    );
                }
                println!(
                    "  result: {} ({} error(s))",
                    "INVALID".red().bold(),
                    result.failures.len()
                );
            } else {
                println!("  result: {}", "VALID".green().bold());
            }
        }
    }

    Ok(())
}

fn cmd_verify_all_chains(
    repo: &Repository,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = repo.verify_all_chains(None)?;

    match format {
        "json" => {
            let json = serde_json::json!({
                "all_valid": result.all_valid,
                "head_chain": {
                    "valid": result.head_chain.valid,
                    "seals_checked": result.head_chain.total_seals,
                    "seals_verified": result.head_chain.verified,
                    "seals_unsecured": result.head_chain.unsecured,
                    "failures": result.head_chain.failures.iter().map(|f| {
                        serde_json::json!({
                            "seal": &f.seal_id[..12.min(f.seal_id.len())],
                            "error": f.error,
                        })
                    }).collect::<Vec<_>>(),
                },
                "spec_chains": result.spec_chains.iter().map(|sc| {
                    serde_json::json!({
                        "spec_id": sc.spec_id,
                        "valid": sc.chain.valid,
                        "seals_checked": sc.chain.total_seals,
                        "seals_verified": sc.chain.verified,
                        "seals_unsecured": sc.chain.unsecured,
                        "failures": sc.chain.failures.iter().map(|f| {
                            serde_json::json!({
                                "seal": &f.seal_id[..12.min(f.seal_id.len())],
                                "error": f.error,
                            })
                        }).collect::<Vec<_>>(),
                    })
                }).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        _ => {
            println!("=== HEAD chain ===");
            println!(
                "  {} seals checked, {} verified, {} pre-security",
                result.head_chain.total_seals,
                result.head_chain.verified,
                result.head_chain.unsecured
            );
            for f in &result.head_chain.failures {
                let short_id = &f.seal_id[..12.min(f.seal_id.len())];
                println!(
                    "  FAIL {short_id}: {}",
                    f.error.as_deref().unwrap_or("unknown")
                );
            }
            println!(
                "  result: {}",
                if result.head_chain.valid {
                    "VALID"
                } else {
                    "INVALID"
                }
            );

            if result.spec_chains.is_empty() {
                println!("\nno spec branches to verify");
            } else {
                for sc in &result.spec_chains {
                    println!("\n=== spec: {} ===", sc.spec_id);
                    println!(
                        "  {} seals checked, {} verified, {} pre-security",
                        sc.chain.total_seals, sc.chain.verified, sc.chain.unsecured
                    );
                    for f in &sc.chain.failures {
                        let short_id = &f.seal_id[..12.min(f.seal_id.len())];
                        println!(
                            "  FAIL {short_id}: {}",
                            f.error.as_deref().unwrap_or("unknown")
                        );
                    }
                    println!(
                        "  result: {}",
                        if sc.chain.valid { "VALID" } else { "INVALID" }
                    );
                }
            }

            println!(
                "\noverall: {}",
                if result.all_valid {
                    "ALL CHAINS VALID"
                } else {
                    "SOME CHAINS INVALID"
                }
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Agent commands (Sprint B)
// ---------------------------------------------------------------------------

fn cmd_agent_register(
    cwd: &PathBuf,
    name: &str,
    trust_level: &str,
    scope: Option<Vec<String>>,
    registered_by: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let trust = TrustLevel::from_str_loose(trust_level).ok_or_else(|| {
        format!(
            "invalid trust level '{trust_level}' — use full, standard, restricted, or untrusted"
        )
    })?;

    let scope_constraints = scope.unwrap_or_default();
    let agent = repo.register_agent(name, registered_by, trust, scope_constraints)?;

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&agent)?;
            println!("{json}");
        }
        _ => {
            println!("registered agent '{}'", agent.agent_id);
            println!("  trust_level: {:?}", agent.trust_level);
            println!("  public_key:  {}...", &agent.public_key[..16]);
            if agent.scope_constraints.is_empty() {
                println!("  scope:       unrestricted");
            } else {
                println!("  scope:       {}", agent.scope_constraints.join(", "));
            }
        }
    }

    Ok(())
}

fn cmd_agent_list(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let agents = repo.list_agents()?;

    if agents.is_empty() {
        match format {
            "json" => println!("[]"),
            _ => println!("no registered agents"),
        }
        return Ok(());
    }

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&agents)?;
            println!("{json}");
        }
        _ => {
            println!(
                "{:<20} {:<12} {:<10} {}",
                "AGENT", "TRUST", "STATUS", "SCOPE"
            );
            for agent in &agents {
                let scope = if agent.scope_constraints.is_empty() {
                    "unrestricted".to_string()
                } else {
                    agent.scope_constraints.join(", ")
                };
                println!(
                    "{:<20} {:<12} {:<10} {}",
                    agent.agent_id,
                    format!("{:?}", agent.trust_level).to_lowercase(),
                    format!("{:?}", agent.status).to_lowercase(),
                    scope
                );
            }
        }
    }

    Ok(())
}

fn cmd_agent_show(
    cwd: &PathBuf,
    name: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let agent = repo.load_agent(name)?;

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&agent)?;
            println!("{json}");
        }
        _ => {
            println!("agent: {}", agent.agent_id);
            println!("  status:        {:?}", agent.status);
            println!("  trust_level:   {:?}", agent.trust_level);
            println!("  public_key:    {}", agent.public_key);
            println!("  registered_at: {}", agent.registered_at.to_rfc3339());
            println!("  registered_by: {}", agent.registered_by);
            if agent.scope_constraints.is_empty() {
                println!("  scope:         unrestricted");
            } else {
                for s in &agent.scope_constraints {
                    println!("  scope:         {s}");
                }
            }
            if let Some(ref reason) = agent.revocation_reason {
                println!(
                    "  revoked_at:    {}",
                    agent.revoked_at.unwrap().to_rfc3339()
                );
                println!("  reason:        {reason}");
            }
        }
    }

    Ok(())
}

fn cmd_agent_revoke(
    cwd: &PathBuf,
    name: &str,
    reason: &str,
    compromise_timestamp: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    let ts = match compromise_timestamp {
        Some(s) => {
            let parsed = chrono::DateTime::parse_from_rfc3339(s)
                .map_err(|e| format!("invalid timestamp '{}': {}", s, e))?;
            Some(parsed.with_timezone(&chrono::Utc))
        }
        None => None,
    };

    let agent = repo.revoke_agent_with_compromise(name, reason, ts)?;

    println!("agent '{name}' revoked: {reason}");

    // Report flagged seals
    let flagged = repo.flagged_seal_ids()?;
    if !flagged.is_empty() {
        println!(
            "  {} seal(s) flagged as potentially compromised",
            flagged.len()
        );
        if let Some(t) = ts {
            println!("  compromise window: {} to now", t.to_rfc3339());
        } else if let Some(t) = agent.revoked_at {
            println!("  compromise window: {} (revocation time)", t.to_rfc3339());
        }
    }

    Ok(())
}

fn cmd_agent_suspend(cwd: &PathBuf, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.suspend_agent(name)?;
    println!("agent '{name}' suspended");
    Ok(())
}

fn cmd_agent_reactivate(cwd: &PathBuf, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    repo.reactivate_agent(name)?;
    println!("agent '{name}' reactivated");
    Ok(())
}

fn cmd_agent_scope(
    cwd: &PathBuf,
    name: &str,
    add: Option<String>,
    remove: Option<String>,
    list: bool,
    set: Option<Vec<String>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;

    if let Some(patterns) = set {
        let updated = repo.update_agent(
            name,
            AgentUpdate {
                scope_constraints: Some(patterns),
                ..Default::default()
            },
        )?;
        println!("scope for '{}' set to:", name);
        if updated.scope_constraints.is_empty() {
            println!("  unrestricted");
        } else {
            for s in &updated.scope_constraints {
                println!("  {s}");
            }
        }
        return Ok(());
    }

    if let Some(pattern) = add {
        let agent = repo.load_agent(name)?;
        let mut constraints = agent.scope_constraints.clone();
        if !constraints.contains(&pattern) {
            constraints.push(pattern.clone());
        }
        repo.update_agent(
            name,
            AgentUpdate {
                scope_constraints: Some(constraints),
                ..Default::default()
            },
        )?;
        println!("added '{pattern}' to scope for '{name}'");
        return Ok(());
    }

    if let Some(pattern) = remove {
        let agent = repo.load_agent(name)?;
        let constraints: Vec<String> = agent
            .scope_constraints
            .into_iter()
            .filter(|s| s != &pattern)
            .collect();
        repo.update_agent(
            name,
            AgentUpdate {
                scope_constraints: Some(constraints),
                ..Default::default()
            },
        )?;
        println!("removed '{pattern}' from scope for '{name}'");
        return Ok(());
    }

    // Default: list scope (also triggered by --list flag)
    let _ = list; // flag is consumed but default behavior is the same
    let agent = repo.load_agent(name)?;
    if agent.scope_constraints.is_empty() {
        println!("scope for '{}': unrestricted", name);
    } else {
        println!("scope for '{}':", name);
        for s in &agent.scope_constraints {
            println!("  {s}");
        }
    }

    Ok(())
}

// -------------------------------------------------------------------
// Security commands
// -------------------------------------------------------------------

fn cmd_security_events(
    cwd: &PathBuf,
    severity: Option<&str>,
    event_type: Option<&str>,
    limit: Option<usize>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    let severity_filter = match severity {
        Some("info") => Some(writ_core::security::Severity::Info),
        Some("warning") => Some(writ_core::security::Severity::Warning),
        Some("critical") => Some(writ_core::security::Severity::Critical),
        Some(other) => {
            eprintln!(
                "unknown severity '{}' — expected info, warning, or critical",
                other
            );
            std::process::exit(1);
        }
        None => None,
    };

    let logger = writ_core::security::SecurityEventLogger::new(&writ_dir);
    let mut events = logger.read_events(severity_filter.as_ref())?;

    // Apply event_type filter if specified.
    if let Some(et) = event_type {
        events.retain(|e| e.event_type == et);
    }

    let events: Vec<_> = if let Some(n) = limit {
        events.into_iter().rev().take(n).collect()
    } else {
        events.into_iter().rev().collect()
    };

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&events)?);
        }
        _ => {
            if events.is_empty() {
                println!("no security events recorded");
                if let Some(sev) = severity {
                    println!("  {}", format!("(filtered by severity: {})", sev).dimmed());
                }
                return Ok(());
            }

            println!(
                "{} security event(s){}:",
                events.len().to_string().bold(),
                severity
                    .map(|s| format!(" (severity: {})", s))
                    .unwrap_or_default()
            );
            println!();

            for event in &events {
                let sev_label = match event.severity {
                    writ_core::security::Severity::Info => "INFO".dimmed(),
                    writ_core::security::Severity::Warning => "WARN".yellow().bold(),
                    writ_core::security::Severity::Critical => "CRIT".red().bold(),
                };
                let agent = event
                    .agent_id
                    .as_deref()
                    .map(|a| format!(" agent={}", a.cyan()))
                    .unwrap_or_default();
                let ts = event
                    .timestamp
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string()
                    .dimmed();

                println!(
                    "[{}] {} {}{}",
                    sev_label,
                    ts,
                    event.event_type.cyan(),
                    agent
                );
                println!("  {}", event.details);
                println!();
            }
        }
    }

    Ok(())
}

// -------------------------------------------------------------------
// GC commands
// -------------------------------------------------------------------

fn cmd_gc_run(
    cwd: &PathBuf,
    dry_run: bool,
    yes: bool,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    let config = writ_core::gc::GcConfig::load(&writ_dir)?;
    let repo = Repository::open(cwd)?;
    let specs = repo.list_specs()?;
    let logger = writ_core::security::SecurityEventLogger::new(&writ_dir);
    let events = logger.read_events(None)?;

    let plan = writ_core::gc::GcPlan::generate(&writ_dir, &config, &specs, &events)?;

    if dry_run {
        match format {
            "json" => {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            }
            _ => {
                println!(
                    "{} {}",
                    "GC dry run".bold(),
                    format!("— {}", plan.summary.summary_line).dimmed()
                );
                println!();
                let usage_pct = plan.storage.usage_pct();
                let usage_colored = if usage_pct >= 90.0 {
                    format!("{:.1}%", usage_pct).red()
                } else if usage_pct >= 70.0 {
                    format!("{:.1}%", usage_pct).yellow()
                } else {
                    format!("{:.1}%", usage_pct).green()
                };
                println!(
                    "  {} {:.1} MB / {:.1} MB ({})",
                    "storage:".dimmed(),
                    plan.storage.total_bytes as f64 / 1_048_576.0,
                    plan.storage.budget_bytes as f64 / 1_048_576.0,
                    usage_colored
                );
                println!();

                if plan.actions.is_empty() {
                    println!("  {}", "nothing to clean".green());
                } else {
                    println!(
                        "  {} action(s) planned ({} transition(s), {} deletion(s), {} event(s) to clean):",
                        plan.summary.total_actions.to_string().bold(),
                        plan.summary.transitions,
                        plan.summary.deletions,
                        plan.summary.events_to_clean
                    );
                    println!();
                    for action in &plan.actions {
                        match action {
                            writ_core::gc::GcAction::TransitionSpec {
                                spec_id,
                                from,
                                to,
                                reason,
                            } => {
                                println!(
                                    "    {}  {}: {} -> {}",
                                    "transition".cyan(),
                                    spec_id.cyan(),
                                    from,
                                    to
                                );
                                println!("                {}", reason.dimmed());
                            }
                            writ_core::gc::GcAction::CleanSpec {
                                spec_id,
                                lifecycle_state,
                                reason,
                            } => {
                                println!(
                                    "    {}  {} ({})",
                                    "clean-spec".cyan(),
                                    spec_id.cyan(),
                                    lifecycle_state
                                );
                                println!("                {}", reason.dimmed());
                            }
                            writ_core::gc::GcAction::CleanSecurityEvents {
                                count,
                                severity,
                                reason,
                            } => {
                                println!(
                                    "    {}  {count} {severity} event(s)",
                                    "clean-events".cyan()
                                );
                                println!("                  {}", reason.dimmed());
                            }
                            writ_core::gc::GcAction::PruneObjects {
                                count,
                                total_bytes,
                                reason,
                            } => {
                                println!(
                                    "    {}  {count} object(s), {:.1} MB",
                                    "prune-objects".cyan(),
                                    *total_bytes as f64 / 1_048_576.0
                                );
                                println!("                   {}", reason.dimmed());
                            }
                            writ_core::gc::GcAction::RecompressObjects {
                                count,
                                estimated_savings_bytes,
                                reason,
                            } => {
                                println!(
                                    "    {}     {count} object(s), ~{:.1} MB savings",
                                    "recompress".cyan(),
                                    *estimated_savings_bytes as f64 / 1_048_576.0
                                );
                                println!("                   {}", reason.dimmed());
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    // Execute for real.
    if plan.actions.is_empty() {
        match format {
            "json" => println!("{}", serde_json::to_string_pretty(&plan)?),
            _ => println!("nothing to clean"),
        }
        return Ok(());
    }

    // Confirm unless --yes was passed.
    if !yes {
        eprintln!("{}", plan.summary.summary_line);
        let mut details = format!(
            "{} action(s): {} transition(s), {} deletion(s), {} event(s)",
            plan.summary.total_actions,
            plan.summary.transitions,
            plan.summary.deletions,
            plan.summary.events_to_clean
        );
        if plan.summary.objects_to_prune > 0 {
            details.push_str(&format!(
                ", {} object(s) to prune",
                plan.summary.objects_to_prune
            ));
        }
        if plan.summary.objects_to_recompress > 0 {
            details.push_str(&format!(
                ", {} object(s) to recompress",
                plan.summary.objects_to_recompress
            ));
        }
        eprintln!("{details}");
        eprint!("proceed? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("gc cancelled");
            return Ok(());
        }
    }

    let result = writ_core::gc::execute_plan(&writ_dir, &plan, &specs)?;

    // If events were marked for cleaning, actually clean the events file.
    if result.events_cleaned > 0 {
        let gc_config = writ_core::gc::GcConfig::load(&writ_dir)?;
        logger.clean_events(&gc_config.security_events)?;
    }

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&result.audit)?);
        }
        _ => {
            println!("{}:", "GC complete".green().bold());
            println!(
                "  {} {} action(s) in {}ms",
                "executed:".dimmed(),
                result.audit.actions_executed.to_string().bold(),
                result.audit.duration_ms
            );
            if !result.specs_cleaned.is_empty() {
                println!(
                    "  {} {}",
                    "cleaned specs:".green(),
                    result.specs_cleaned.join(", ").cyan()
                );
            }
            if !result.transitions_applied.is_empty() {
                for (id, from, to) in &result.transitions_applied {
                    println!(
                        "  {} {} ({from} -> {to})",
                        "transitioned:".green(),
                        id.cyan()
                    );
                }
            }
            if result.events_cleaned > 0 {
                println!("  {} {}", "cleaned events:".green(), result.events_cleaned);
            }
            if result.objects_pruned > 0 {
                println!(
                    "  {} {} object(s), {:.1} MB freed",
                    "pruned:".green(),
                    result.objects_pruned,
                    result.bytes_freed as f64 / 1_048_576.0
                );
            }
            if result.objects_recompressed > 0 {
                println!(
                    "  {} {} object(s), {:.1} MB saved",
                    "recompressed:".green(),
                    result.objects_recompressed,
                    result.recompression_savings as f64 / 1_048_576.0
                );
            }
            if result.audit.actions_skipped > 0 {
                println!(
                    "  {} {} (safety rules)",
                    "skipped:".yellow(),
                    result.audit.actions_skipped
                );
                for s in &result.audit.skipped_details {
                    println!("    {} {}: {}", "-".yellow(), s.action, s.reason);
                }
            }
        }
    }

    Ok(())
}

fn cmd_gc_status(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    let config = writ_core::gc::GcConfig::load(&writ_dir)?;
    let repo = Repository::open(cwd)?;
    let specs = repo.list_specs()?;
    let storage = writ_core::gc::StorageReport::scan(&writ_dir, config.budget_bytes)?;

    // Count specs by lifecycle state.
    let mut active = 0usize;
    let mut stale = 0usize;
    let mut completed = 0usize;
    let mut cancelled = 0usize;
    let mut archived = 0usize;

    for spec in &specs {
        match spec.lifecycle_state {
            writ_core::spec::LifecycleState::Active => active += 1,
            writ_core::spec::LifecycleState::Stale => stale += 1,
            writ_core::spec::LifecycleState::Completed => completed += 1,
            writ_core::spec::LifecycleState::Cancelled => cancelled += 1,
            writ_core::spec::LifecycleState::Archived => archived += 1,
        }
    }

    match format {
        "json" => {
            let status = serde_json::json!({
                "storage": storage,
                "usage_pct": storage.usage_pct(),
                "specs": {
                    "total": specs.len(),
                    "active": active,
                    "stale": stale,
                    "completed": completed,
                    "cancelled": cancelled,
                    "archived": archived,
                },
                "mode": config.mode,
                "budget_bytes": config.budget_bytes,
            });
            println!("{}", serde_json::to_string_pretty(&status)?);
        }
        _ => {
            println!("{}:", "GC status".bold());
            println!();
            let usage_pct = storage.usage_pct();
            let usage_colored = if usage_pct >= 90.0 {
                format!("{:.1}%", usage_pct).red()
            } else if usage_pct >= 70.0 {
                format!("{:.1}%", usage_pct).yellow()
            } else {
                format!("{:.1}%", usage_pct).green()
            };
            println!(
                "  {} {:.1} MB / {:.1} MB ({})",
                "storage:".dimmed(),
                storage.total_bytes as f64 / 1_048_576.0,
                storage.budget_bytes as f64 / 1_048_576.0,
                usage_colored
            );
            println!("  {} {:?}", "mode:".dimmed(), config.mode);
            println!();
            println!("  specs ({} total):", specs.len());
            println!("    {}    {}", "active:".green(), active);
            if stale > 0 {
                println!("    {}     {}", "stale:".yellow(), stale);
            }
            if completed > 0 {
                println!("    {} {}", "completed:".dimmed(), completed);
            }
            if cancelled > 0 {
                println!("    {} {}", "cancelled:".dimmed(), cancelled);
            }
            if archived > 0 {
                println!("    {}  {}", "archived:".dimmed(), archived);
            }

            // Show stale warnings.
            let stale_specs = repo.scan_stale_specs(&config)?;
            if !stale_specs.is_empty() {
                println!();
                println!("  {}:", "stale candidates".yellow());
                for (id, secs) in &stale_specs {
                    println!("    {}: inactive for {}h", id.yellow(), secs / 3600);
                }
            }

            if storage.usage_pct() >= config.warning_threshold_pct as f64 {
                println!();
                println!(
                    "  {} storage usage above {}% threshold",
                    "WARNING:".yellow().bold(),
                    config.warning_threshold_pct
                );
                println!("  run {} to free space", "`writ gc run`".cyan());
            }
        }
    }

    Ok(())
}

fn cmd_gc_storage(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(cwd)?;
    let storage = repo.storage_report()?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&storage)?);
        }
        _ => {
            println!("{}:", "Storage breakdown".bold());
            println!();
            println!(
                "  {} {:.1} MB",
                "total:          ".dimmed(),
                storage.total_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "seals:          ".dimmed(),
                storage.seal_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "working state:  ".dimmed(),
                storage.working_state_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "security events:".dimmed(),
                storage.security_event_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "keys:           ".dimmed(),
                storage.key_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "agents:         ".dimmed(),
                storage.agent_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "gc metadata:    ".dimmed(),
                storage.gc_bytes as f64 / 1_048_576.0
            );
            println!(
                "  {} {:.1} MB",
                "other:          ".dimmed(),
                storage.other_bytes as f64 / 1_048_576.0
            );
            // Compression stats (if available)
            if let Some(ref cs) = storage.compression {
                println!();
                println!(
                    "  {} {} compressed, {} raw, {} legacy",
                    "compression:".dimmed(),
                    cs.compressed_objects,
                    cs.raw_objects,
                    cs.legacy_objects
                );
                println!(
                    "    ratio: {} ({:.1} MB content in {:.1} MB on disk)",
                    format!("{:.1}x", cs.compression_ratio).cyan(),
                    cs.total_content_bytes as f64 / 1_048_576.0,
                    cs.total_disk_bytes as f64 / 1_048_576.0
                );
            }
            println!();
            if storage.budget_bytes == u64::MAX {
                println!("  {} unlimited (enterprise)", "budget:".dimmed());
            } else {
                let usage_pct = storage.usage_pct();
                let usage_colored = if usage_pct >= 90.0 {
                    format!("{:.1}% used", usage_pct).red()
                } else if usage_pct >= 70.0 {
                    format!("{:.1}% used", usage_pct).yellow()
                } else {
                    format!("{:.1}% used", usage_pct).green()
                };
                println!(
                    "  {} {:.1} MB ({})",
                    "budget:".dimmed(),
                    storage.budget_bytes as f64 / 1_048_576.0,
                    usage_colored
                );
            }
        }
    }

    Ok(())
}

fn cmd_gc_log(cwd: &PathBuf, limit: usize, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    let logger = writ_core::security::GcAuditLogger::new(&writ_dir);
    let records = logger.read_last(limit)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&records)?);
        }
        _ => {
            if records.is_empty() {
                println!("no GC audit records");
                return Ok(());
            }

            println!("{} GC audit record(s):", records.len().to_string().bold());
            println!();
            for record in &records {
                let ts = record
                    .executed_at
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string()
                    .dimmed();
                let trigger = format!("{:?}", record.triggered_by).cyan();
                println!(
                    "  {} ({}) — {}/{} executed, {} skipped, {}ms",
                    ts,
                    trigger,
                    record.actions_executed,
                    record.actions_planned,
                    record.actions_skipped,
                    record.duration_ms
                );
                if record.space_freed_bytes > 0 {
                    println!(
                        "    {} {:.1} MB",
                        "freed:".green(),
                        record.space_freed_bytes as f64 / 1_048_576.0
                    );
                }
                if !record.skipped_details.is_empty() {
                    for s in &record.skipped_details {
                        println!("    {} {} — {}", "skipped:".yellow(), s.action, s.reason);
                    }
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Config commands
// ---------------------------------------------------------------------------

fn cmd_config_set(
    cwd: &PathBuf,
    key: &str,
    value: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    if !writ_core::settings::WritSettings::is_valid_key(key) {
        return Err(format!("unknown setting '{key}'").into());
    }

    let mut settings = writ_core::settings::WritSettings::load(&writ_dir)?;
    settings.set(key, value)?;
    settings.save(&writ_dir)?;

    match format {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "key": key,
                    "value": value,
                    "status": "saved"
                }))?
            );
        }
        _ => {
            println!("{} {} = {}", "set".green(), key, value);
        }
    }

    Ok(())
}

fn cmd_config_get(
    cwd: &PathBuf,
    key: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    if !writ_core::settings::WritSettings::is_valid_key(key) {
        return Err(format!("unknown setting '{key}'").into());
    }

    let settings = writ_core::settings::WritSettings::load(&writ_dir)?;
    let value = settings.get(key);

    // Find the default for display purposes
    let key_meta = writ_core::settings::WritSettings::keys()
        .iter()
        .find(|k| k.key == key);
    let default_str = key_meta.map_or("(unknown)", |k| k.default_value);

    match format {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "key": key,
                    "value": value,
                    "default": default_str,
                    "source": if value.is_some() { "settings" } else { "default" }
                }))?
            );
        }
        _ => match value {
            Some(v) => println!("{}: {}", key, v),
            None => println!(
                "{}: {} (default: {})",
                key,
                "(not set)".dimmed(),
                default_str
            ),
        },
    }

    Ok(())
}

fn cmd_config_list(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    let settings = writ_core::settings::WritSettings::load(&writ_dir)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&settings)?);
        }
        _ => {
            println!(
                "{:<45} {:<15} {}",
                "KEY".bold(),
                "VALUE".bold(),
                "DEFAULT".bold()
            );
            for key_info in writ_core::settings::WritSettings::keys() {
                let value = settings.get(key_info.key);
                let val_display = match &value {
                    Some(v) => v.to_string(),
                    None => "(not set)".dimmed().to_string(),
                };
                println!(
                    "{:<45} {:<15} {}",
                    key_info.key,
                    val_display,
                    key_info.default_value.dimmed()
                );
            }
        }
    }

    Ok(())
}

fn cmd_config_unset(
    cwd: &PathBuf,
    key: &str,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    if !writ_dir.exists() {
        return Err("not a writ repository (no .writ directory)".into());
    }

    if !writ_core::settings::WritSettings::is_valid_key(key) {
        return Err(format!("unknown setting '{key}'").into());
    }

    let mut settings = writ_core::settings::WritSettings::load(&writ_dir)?;
    settings.unset(key)?;
    settings.save(&writ_dir)?;

    match format {
        "json" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "key": key,
                    "status": "unset"
                }))?
            );
        }
        _ => {
            println!("{} {} (reset to default)", "unset".green(), key);
        }
    }

    Ok(())
}
