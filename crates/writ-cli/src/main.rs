//! writ CLI — the human (and agent) interface to writ.

mod init;
mod watch_ui;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use colored::Colorize;
use writ_core::agent::{AgentUpdate, TrustLevel};
use writ_core::config::{self, GlobalConfig, ProjectConfig};
use writ_core::context::{ContextFilter, ContextScope};
use writ_core::diff::LineOp;
use writ_core::format;
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
    /// Initialize writ in this project: detect git, import baseline, install hooks.
    Init {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,

        /// Deployment profile for GC configuration.
        /// Values: raspberry-pi, development (default), production, enterprise.
        #[arg(long, default_value = "development")]
        profile: String,

        /// Create a spec during init (convenience shortcut).
        /// Example: --spec auth --title "Authentication" --description "JWT auth system"
        #[arg(long)]
        spec: Option<String>,

        /// Title for the spec created with --spec. Defaults to the spec ID.
        #[arg(long, requires = "spec")]
        title: Option<String>,

        /// Description for the spec created with --spec.
        #[arg(long, requires = "spec")]
        description: Option<String>,

        /// Accept all defaults without prompting (CI-safe).
        #[arg(long, short = 'y')]
        yes: bool,

        /// Create only .writ/ directory, no framework integration files.
        #[arg(long)]
        bare: bool,

        /// Skip git integration even if git repo detected.
        #[arg(long)]
        no_git: bool,

        /// Skip Claude Code integration.
        #[arg(long)]
        no_claude: bool,

        /// Skip Codex / OpenAI integration.
        #[arg(long)]
        no_codex: bool,

        /// Skip generic agent instructions.
        #[arg(long)]
        no_generic: bool,

        /// Comma-separated list of frameworks to enable: claude,codex,generic.
        #[arg(long, value_delimiter = ',')]
        frameworks: Option<Vec<String>>,

        /// Set output format for this project: toon, json, json-compact.
        #[arg(long = "output-format")]
        output_fmt: Option<String>,

        /// Set project name (default: auto-detected from manifest or directory name).
        #[arg(long)]
        name: Option<String>,

        /// Re-prompt for project-level settings even if global config exists.
        #[arg(long)]
        reconfigure: bool,
    },

    /// Remove writ from this project (inverse of init).
    Uninit {
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

    /// Deprecated: use `writ uninit` instead.
    #[command(hide = true)]
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

    /// Deprecated: use `writ init` instead.
    #[command(hide = true)]
    Install {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,

        /// Deployment profile for GC configuration.
        #[arg(long, default_value = "development")]
        profile: String,

        /// Create a spec during install.
        #[arg(long)]
        spec: Option<String>,

        /// Title for the spec created with --spec.
        #[arg(long, requires = "spec")]
        title: Option<String>,

        /// Description for the spec created with --spec.
        #[arg(long, requires = "spec")]
        description: Option<String>,
    },

    /// Create and manage tasks (spec + workspace in one command).
    ///
    /// Each task gets its own workspace directory at `workspaces/<id>/` where
    /// an agent can work without interfering with other agents.
    ///
    /// Usage:
    ///   writ task "description"     Create a new task
    ///   writ task list              List all active tasks
    #[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
    Task {
        /// Task description (used as spec title and prompt suggestion).
        title: Option<String>,

        /// Override the auto-derived spec/workspace ID.
        #[arg(long)]
        id: Option<String>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,

        #[command(subcommand)]
        action: Option<TaskCommands>,
    },

    /// Create specs in batch from a list of task descriptions.
    /// Accepts inline arguments, -f file (one task per line), or stdin.
    Plan {
        /// Task descriptions (inline). Each becomes a spec.
        tasks: Vec<String>,

        /// Read tasks from a file (one per line).
        #[arg(short, long)]
        file: Option<String>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show working directory state.
    State {
        /// Output format: "human" (default), "json", "json-compact", or "brief".
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

        /// Output format: "human" (default), "json", "json-compact", or "brief".
        #[arg(long)]
        format: Option<String>,
    },

    /// Show seal history.
    Log {
        /// Output format: "human" (default), "json", "json-compact", or "brief".
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

        /// Output format: "human" (default), "json", "json-compact", or "brief".
        #[arg(long)]
        format: Option<String>,

        /// Filter diff to files changed by a specific spec.
        #[arg(long)]
        spec: Option<String>,

        /// Filter diff to files changed by a specific agent.
        #[arg(long)]
        agent: Option<String>,

        /// Only show changes from completed specs.
        #[arg(long)]
        completed: bool,

        /// Include in-progress specs (used with --completed to show all).
        #[arg(long)]
        all: bool,

        /// Show diff for a single file only.
        #[arg(long)]
        file: Option<String>,

        /// Summary only: file names and line counts, no diff content.
        #[arg(long)]
        stat: bool,

        /// Show only file names that changed (no line counts, no diff content).
        #[arg(long)]
        name_only: bool,

        /// Show changes from completed specs not yet committed to git.
        #[arg(long)]
        cached: bool,
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

        /// Output format: "json" (default), "json-compact", "human", or "brief".
        /// Note: context defaults to "json" unlike other commands.
        #[arg(long)]
        format: Option<String>,
    },

    /// Human-readable summary of all work done in this writ session.
    /// Designed for the round-trip: writ init -> agents work -> writ summary -> git commit.
    Summary {
        /// Output format: "human" (default from settings), "json", "commit", or "pr".
        /// "commit" outputs a concise one-line commit message.
        /// "pr" outputs a detailed PR description with full spec/agent breakdown.
        #[arg(long)]
        format: Option<String>,
    },

    /// Fleet-aware project status: agent activity, spec progress, commit readiness.
    /// High-level porcelain view — complements `writ state` (low-level plumbing).
    Status {
        /// Show all completed specs in detail.
        #[arg(long)]
        completed: bool,

        /// Show all in-progress specs in detail.
        #[arg(long)]
        active: bool,

        /// Filter by agent name.
        #[arg(long)]
        agent: Option<String>,

        /// Detail view of one spec.
        #[arg(long)]
        spec: Option<String>,

        /// Live-updating view (refresh every 5 seconds by default).
        #[arg(long)]
        watch: bool,

        /// Refresh interval in seconds for --watch mode.
        #[arg(long, default_value = "5")]
        interval: u64,

        /// Output format: "human" (default), "json", "json-compact", "toon".
        #[arg(long)]
        format: Option<String>,
    },

    /// Commit completed spec work to git.
    /// Shows completed specs, generates commit message(s), and creates git commit(s).
    Finish {
        /// Use the full PR-style description as the commit body instead of a one-liner.
        #[arg(long)]
        full: bool,

        /// Dry run: show what would be committed without actually committing.
        #[arg(long)]
        dry_run: bool,

        /// Skip confirmation prompts (for scripts and auto mode).
        #[arg(long, short)]
        yes: bool,

        /// Commit strategy: single (default), per-spec, grouped.
        #[arg(long, default_value = "single")]
        strategy: String,

        /// Create a proposal instead of committing directly (propose mode).
        #[arg(long)]
        propose: bool,

        /// List pending proposals.
        #[arg(long)]
        proposals: bool,

        /// Accept a pending proposal by ID (e.g. prop-20260306-143000).
        #[arg(long)]
        accept: Option<String>,

        /// Reject a pending proposal by ID.
        #[arg(long)]
        reject: Option<String>,

        /// Auto mode: commit immediately without prompts, using project auto config.
        #[arg(long)]
        auto: bool,

        /// Auto-clean workspaces after committing (skip cleanup prompt).
        #[arg(long, conflicts_with = "no_cleanup")]
        cleanup: bool,

        /// Skip workspace cleanup entirely after committing.
        #[arg(long, conflicts_with = "cleanup")]
        no_cleanup: bool,
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

        /// Output format: "json" (default), "json-compact", "human", or "brief".
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
        /// Output format: "json" (default), "json-compact", or "human".
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

    /// Converge work from parallel workspaces back into the main workspace.
    ConvergeWorkspaces {
        /// Workspace names to merge into main.
        workspaces: Vec<String>,

        /// Output format: "json" (default) or "human".
        #[arg(long)]
        format: Option<String>,

        /// Merge strategy for irreconcilable conflicts: "three-way-merge" (default),
        /// "most-recent", or "escalate".
        #[arg(long, default_value = "three-way-merge")]
        strategy: String,

        /// Dry run: show what would be merged without applying.
        #[arg(long)]
        dry_run: bool,
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

    /// Start the MCP (Model Context Protocol) server for native AI agent integration.
    #[command(name = "mcp-serve")]
    McpServe,

    /// Write .mcp.json config so Claude Code discovers writ's MCP tools.
    #[command(name = "mcp-install")]
    McpInstall {
        /// Write to Claude Desktop config instead of project .mcp.json.
        #[arg(long)]
        desktop: bool,
    },

    /// Manage isolated parallel workspaces.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCommands,
    },

    /// Monitor for new seals and auto-converge overlapping changes in real-time.
    ///
    /// Runs as a foreground terminal process by default, showing live seal activity
    /// and convergence events. Use --daemon for background mode.
    Watch {
        /// Polling interval in seconds (default: 5, or from config).
        #[arg(long)]
        interval: Option<u64>,

        /// Disable auto-convergence (watch and report only, don't merge).
        #[arg(long)]
        no_auto_converge: bool,

        /// Run as a background daemon process.
        #[arg(long)]
        daemon: bool,

        /// Stop a running daemon.
        #[arg(long)]
        stop: bool,

        /// Show daemon status and recent activity.
        #[arg(long, name = "watch_status")]
        status: bool,
    },

    /// Check repository health and schema version.
    Doctor {
        /// Output as JSON instead of human-readable.
        #[arg(long)]
        json: bool,

        /// Attempt to fix problems (reserved for future use).
        #[arg(long)]
        fix: bool,
    },
}

#[derive(Subcommand)]
enum TaskCommands {
    /// List all active tasks and their status.
    List {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
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

    /// Mark a spec as done — all work is complete.
    /// Creates a final seal and transitions the spec to complete status.
    /// If only one active spec exists, the ID is auto-detected.
    Done {
        /// Spec ID to complete (auto-detected if only one active spec).
        id: Option<String>,

        /// Completion summary describing what was accomplished.
        #[arg(short, long)]
        summary: Option<String>,

        /// Agent ID for the final seal.
        #[arg(long)]
        agent: Option<String>,
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

    /// Reopen a completed spec, returning it to active state.
    /// Seal history is preserved. Any agent can claim the spec.
    Reopen {
        /// Spec ID to reopen.
        id: String,
    },

    /// Claim a spec for an agent. Prevents other agents from working on it.
    /// On first seal with --spec, the spec is auto-claimed.
    Claim {
        /// Spec ID to claim.
        id: String,

        /// Agent ID claiming this spec (auto-detected if omitted).
        #[arg(long)]
        agent: Option<String>,
    },

    /// Assign a spec to a workspace. Scopes the spec to that workspace's context.
    Assign {
        /// Spec ID to assign.
        id: String,

        /// Workspace name to assign the spec to.
        #[arg(long)]
        workspace: String,
    },

    /// Unassign a spec from its workspace, making it globally visible.
    Unassign {
        /// Spec ID to unassign.
        id: String,
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
enum WorkspaceCommands {
    /// Create a new isolated parallel workspace.
    Create {
        /// Workspace name (lowercase, alphanumeric, hyphens).
        name: String,

        /// Directory for the workspace (default: .writ/ws/<name>/).
        #[arg(long)]
        path: Option<PathBuf>,

        /// Assign matching specs (glob or comma-separated IDs).
        #[arg(long)]
        specs: Option<String>,

        /// Create from another workspace's state instead of main.
        #[arg(long)]
        from: Option<String>,
    },

    /// List all workspaces.
    List {
        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Show workspace status and details.
    Status {
        /// Workspace name (default: current workspace).
        name: Option<String>,

        /// Output format: "human" (default) or "json".
        #[arg(long, default_value = "human")]
        format: String,
    },

    /// Delete a workspace. Cannot delete "main".
    Delete {
        /// Name of the workspace to delete.
        name: String,
        /// Keep the parallel working directory files.
        #[arg(long)]
        keep_files: bool,
        /// Skip confirmation prompt.
        #[arg(long)]
        force: bool,
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
        Commands::Init {
            format,
            profile,
            spec,
            title,
            description,
            yes,
            bare,
            no_git,
            no_claude,
            no_codex,
            no_generic,
            frameworks,
            output_fmt,
            name,
            reconfigure,
        } => {
            let opts = init::InitOptions {
                yes,
                bare,
                no_git,
                no_claude,
                no_codex,
                no_generic,
                frameworks,
                format: output_fmt,
                name,
                reconfigure,
                profile: profile.clone(),
                output_format: format.clone(),
            };
            cmd_init(&cwd, &format, &profile, spec, title, description, opts)
        }
        Commands::Uninit {
            force,
            keep_writignore,
            format,
        } => cmd_uninit(&cwd, force, keep_writignore, &format),
        Commands::Uninstall {
            force,
            keep_writignore,
            format,
        } => {
            eprintln!(
                "{} `writ uninstall` is deprecated — use `writ uninit` instead",
                "notice:".yellow().bold()
            );
            cmd_uninit(&cwd, force, keep_writignore, &format)
        }
        Commands::Install {
            format,
            profile,
            spec,
            title,
            description,
        } => {
            eprintln!(
                "{} `writ install` is deprecated — use `writ init` instead",
                "notice:".yellow().bold()
            );
            let opts = init::InitOptions {
                yes: true, // deprecated path: non-interactive for backward compat
                profile: profile.clone(),
                output_format: format.clone(),
                ..Default::default()
            };
            cmd_init(&cwd, &format, &profile, spec, title, description, opts)
        }
        Commands::Task {
            title,
            id,
            format,
            action,
        } => match action {
            Some(TaskCommands::List { format }) => cmd_task_list(&cwd, &format),
            None => match title {
                Some(title) => cmd_task_create(&cwd, &title, id, &format),
                None => {
                    eprintln!("error: task description is required");
                    eprintln!("  usage: writ task \"description\"");
                    eprintln!("  or:    writ task list");
                    std::process::exit(1);
                }
            },
        },
        Commands::Plan {
            tasks,
            file,
            format,
        } => cmd_plan(&cwd, tasks, file, &format),
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
        Commands::Diff {
            from,
            to,
            format,
            spec,
            agent,
            completed,
            all,
            file,
            stat,
            name_only,
            cached,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_diff(
                &cwd, from, to, &format, spec, agent, completed, all, file, stat, name_only, cached,
            )
        }
        Commands::Context {
            spec,
            for_agent,
            seal_limit,
            status,
            agent,
            format,
        } => {
            let format = if format.is_none() && std::io::stdout().is_terminal() {
                // No explicit --format and stdout is a terminal → human-readable
                "human".to_string()
            } else {
                resolve_format(format.as_deref(), &cwd, "json")
            };
            cmd_context(&cwd, spec, for_agent, seal_limit, status, agent, &format)
        }
        Commands::Summary { format } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_summary(&cwd, &format)
        }
        Commands::Status {
            completed,
            active,
            agent,
            spec,
            watch,
            interval,
            format,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "human");
            cmd_status(
                &cwd, completed, active, agent, spec, watch, interval, &format,
            )
        }
        Commands::Finish {
            full,
            dry_run,
            yes,
            strategy,
            propose,
            proposals,
            accept,
            reject,
            auto,
            cleanup,
            no_cleanup,
        } => {
            if proposals {
                cmd_finish_proposals(&cwd)
            } else if let Some(id) = accept {
                cmd_finish_accept(&cwd, &id, &strategy)
            } else if let Some(id) = reject {
                cmd_finish_reject(&cwd, &id)
            } else if propose {
                cmd_finish_propose(&cwd, full, &strategy)
            } else if auto {
                cmd_finish_auto(&cwd, &strategy)
            } else {
                cmd_finish(&cwd, full, dry_run, yes, cleanup, no_cleanup, &strategy)
            }
        }
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
        Commands::ConvergeWorkspaces {
            workspaces,
            format,
            strategy,
            dry_run,
        } => {
            let format = resolve_format(format.as_deref(), &cwd, "json");
            cmd_converge_workspaces(&cwd, &workspaces, &format, &strategy, dry_run)
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
            SpecCommands::Done { id, summary, agent } => {
                cmd_spec_done(&cwd, id.as_deref(), summary, agent.as_deref())
            }
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
            SpecCommands::Reopen { id } => cmd_spec_reopen(&cwd, &id),
            SpecCommands::Claim { id, agent } => cmd_spec_claim(&cwd, &id, agent.as_deref()),
            SpecCommands::Assign { id, workspace } => cmd_spec_assign(&cwd, &id, &workspace),
            SpecCommands::Unassign { id } => cmd_spec_unassign(&cwd, &id),
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
        Commands::Workspace { action } => match action {
            WorkspaceCommands::Create {
                name,
                path,
                specs,
                from,
            } => cmd_workspace_create(
                &cwd,
                &name,
                path.as_deref(),
                specs.as_deref(),
                from.as_deref(),
            ),
            WorkspaceCommands::List { format } => cmd_workspace_list(&cwd, &format),
            WorkspaceCommands::Status { name, format } => {
                cmd_workspace_status(&cwd, name.as_deref(), &format)
            }
            WorkspaceCommands::Delete {
                name,
                keep_files,
                force,
            } => cmd_workspace_delete(&cwd, &name, keep_files, force),
        },
        Commands::McpServe => cmd_mcp_serve(&cwd),
        Commands::McpInstall { desktop } => cmd_mcp_install(&cwd, desktop),
        Commands::Watch {
            interval,
            no_auto_converge,
            daemon,
            stop,
            status,
        } => cmd_watch(&cwd, interval, no_auto_converge, daemon, stop, status),
        Commands::Doctor { json, fix } => cmd_doctor(&cwd, json, fix),
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

/// Resolve the effective output format using the full config resolution chain:
/// CLI flag > WRIT_FORMAT env var > project config > global config > default.
fn resolve_format(explicit: Option<&str>, cwd: &PathBuf, fallback: &str) -> String {
    if let Some(f) = explicit {
        return f.to_string();
    }

    // Check WRIT_FORMAT environment variable
    if let Ok(env_fmt) = std::env::var("WRIT_FORMAT") {
        if !env_fmt.is_empty() {
            if format::is_valid_format(&env_fmt)
                || matches!(env_fmt.as_str(), "human" | "brief" | "commit" | "pr")
            {
                return env_fmt;
            }
            eprintln!(
                "warning: WRIT_FORMAT='{}' is not a recognized format, ignoring",
                env_fmt
            );
        }
    }

    // Load project config (from .writ/config.toml or migrated settings.json)
    let project = Repository::open_from_dir(cwd)
        .ok()
        .and_then(|r| ProjectConfig::load(r.writ_dir()).ok())
        .unwrap_or_default();

    // Load global config (~/.writ/config)
    let global = GlobalConfig::load().unwrap_or_default();

    config::resolve_output_format(None, &project, &global, fallback)
}

/// Resolve the project name from config, falling back to directory name.
fn resolve_project_name(cwd: &PathBuf) -> Option<String> {
    Repository::open_from_dir(cwd)
        .ok()
        .and_then(|r| {
            ProjectConfig::load(r.writ_dir())
                .ok()
                .and_then(|c| c.project.and_then(|p| p.name))
        })
        .or_else(|| {
            cwd.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
}

/// Create a formatter by name, injecting project context for TOON headers.
fn make_formatter(name: &str, cwd: &PathBuf) -> Option<Box<dyn format::OutputFormatter>> {
    let project_name = resolve_project_name(cwd);
    format::formatter_for_project(name, project_name.as_deref())
}

/// Resolve the effective agent ID for seals.
///
/// Priority: explicit --agent flag > settings.default_agent > env var auto-detect > "human".
/// Auto-detect checks for known agent framework env vars (Claude Code, Codex, etc.)
/// and generates a session-specific ID like "claude-code-a3f2" so each agent instance
/// is uniquely identifiable even without passing --agent.
fn resolve_agent(explicit: Option<&str>, cwd: &PathBuf) -> String {
    if let Some(a) = explicit {
        return a.to_string();
    }
    if let Some(configured) = Repository::open_from_dir(cwd)
        .ok()
        .and_then(|r| r.settings().default_agent.clone())
    {
        return configured;
    }
    // Auto-detect agent identity from environment variables.
    if let Some(detected) = detect_agent_from_env() {
        return detected;
    }
    "human".to_string()
}

/// Detect agent identity from known framework environment variables.
/// Returns a session-specific ID like "claude-code-a3f2" for uniqueness.
fn detect_agent_from_env() -> Option<String> {
    // Explicit writ agent ID takes priority.
    if let Ok(agent_id) = std::env::var("WRIT_AGENT_ID") {
        return Some(agent_id);
    }

    // Claude Code: prefer session ID for unique suffix, fall back to PID.
    for var in &[
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "ANTHROPIC_SESSION_ID",
    ] {
        if let Ok(session_id) = std::env::var(var) {
            let suffix = short_hash(&session_id);
            return Some(format!("claude-code-{}", suffix));
        }
    }
    // CLAUDECODE=1 is set by Claude Code even when session ID vars are absent.
    if std::env::var("CLAUDECODE").is_ok() {
        let suffix = short_hash(&std::process::id().to_string());
        return Some(format!("claude-code-{}", suffix));
    }

    // Codex sets CODEX_SESSION or similar.
    for var in &["CODEX_SESSION", "CODEX_SESSION_ID"] {
        if let Ok(session_id) = std::env::var(var) {
            let suffix = short_hash(&session_id);
            return Some(format!("codex-{}", suffix));
        }
    }

    None
}

/// Generate a short 4-character hash suffix from a string for agent ID uniqueness.
fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:04x}", hasher.finish() & 0xFFFF)
}

/// Check if `writ context` was run recently by reading the `.writ/.context_token` file.
/// Returns true if the token exists and is less than 4 hours old.
fn check_context_token(cwd: &PathBuf) -> bool {
    let token_path = cwd.join(".writ").join(".context_token");
    match std::fs::read_to_string(&token_path) {
        Ok(ts) => {
            if let Ok(token_time) = ts.trim().parse::<i64>() {
                let now = chrono::Utc::now().timestamp();
                now - token_time < 4 * 3600
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

/// Resolve the effective convergence strategy.
fn resolve_strategy(explicit: Option<&str>, cwd: &PathBuf) -> String {
    if let Some(s) = explicit {
        return s.to_string();
    }
    Repository::open_from_dir(cwd)
        .ok()
        .and_then(|r| r.settings().convergence.strategy.clone())
        .unwrap_or_else(|| "escalate".to_string())
}

/// Return an actionable hint for a given error, or None if no hint is needed.
fn error_hint(err: &dyn std::error::Error) -> Option<String> {
    let msg = err.to_string();

    if msg.contains("not a writ repository") {
        return Some("run `writ init` to create one".into());
    }
    if msg.contains(".writ/ already exists") {
        return Some(
            "this directory already has writ initialized — use `writ uninit` first to start fresh"
                .into(),
        );
    }
    if msg.contains("no changes to seal") {
        return Some(
            "your working directory matches the last seal. This can happen if:\n  \
             — another agent in the same directory sealed your files first\n  \
             — your changes were already captured by a previous seal\n  \
             Use `--allow-empty` to create a metadata-only seal."
                .into(),
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
        return Some("run `git init` first, then `writ init`".into());
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
    // WV.8: When a spec already exists and we're inside a workspace, hint about using it.
    if msg.contains("already exists") && msg.contains("spec '") {
        // Extract spec ID from "spec 'foo' already exists"
        if let Some(start) = msg.find("spec '").map(|i| i + 6) {
            if let Some(end) = msg[start..].find('\'').map(|i| i + start) {
                let spec_id = &msg[start..end];
                // Check if we're in a workspace that has this spec assigned
                if let Ok(cwd) = std::env::current_dir() {
                    let ws_pointer = cwd.join(".writ-workspace");
                    if ws_pointer.exists() {
                        return Some(format!(
                            "this spec is assigned to your workspace — use it with:\n  \
                             writ seal -s \"your summary\" --spec {spec_id}"
                        ));
                    }
                }
            }
        }
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

fn cmd_init(
    cwd: &PathBuf,
    format: &str,
    profile: &str,
    spec_id: Option<String>,
    spec_title: Option<String>,
    spec_description: Option<String>,
    opts: init::InitOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Init guard: detect if we're inside a parallel workspace directory.
    let pointer_path = cwd.join(".writ-workspace");
    if pointer_path.exists() {
        if let Ok(pointer) = writ_core::workspace::parse_workspace_pointer(&pointer_path) {
            eprintln!(
                "error: This directory is a writ workspace (parent: {}).",
                pointer.parent
            );
            eprintln!("  Writ commands work here automatically — no init needed.");
            eprintln!(
                "  To reinitialize the parent project: cd {} && writ init",
                std::path::Path::new(&pointer.parent)
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| pointer.parent.clone())
            );
            std::process::exit(1);
        }
    }

    // Phase 1+2: Interactive flow collects user preferences via prompts.
    let plan = init::plan_init(&opts)?;

    // Execute: create .writ/, import baseline.
    let result = Repository::init_project(cwd)?;

    // Save GC config from the selected profile.
    let gc_config = writ_core::gc::GcConfig::from_profile(profile)?;
    gc_config.save(&cwd.join(".writ"))?;

    // Save the project config from the interactive flow.
    plan.project_config.save(&cwd.join(".writ"))?;

    // MS.30: Append commented [watch] section to config.toml for discoverability.
    append_watch_config_comment(&cwd.join(".writ").join("config.toml"));

    // Install framework hooks based on user selections (not just auto-detection).
    // LE-7: Collect and display hook errors instead of silently discarding them.
    if !opts.bare {
        let mut hook_warnings: Vec<String> = Vec::new();
        if plan.enable_claude {
            if let Err(e) = writ_core::hooks::hook_claude_code(cwd) {
                hook_warnings.push(format!("Claude Code hook: {}", e));
            }
        }
        if plan.enable_codex {
            if let Err(e) = writ_core::hooks::hook_codex(cwd) {
                hook_warnings.push(format!("Codex hook: {}", e));
            }
        }
        if plan.enable_generic {
            if let Err(e) = writ_core::hooks::hook_generic(cwd) {
                hook_warnings.push(format!("Generic hook: {}", e));
            }
        }
        if let Err(e) = writ_core::hooks::append_gitignore(cwd) {
            hook_warnings.push(format!(".gitignore: {}", e));
        }
        // MCP.6: Generate .mcp.json when Claude Code is enabled.
        if plan.enable_claude {
            if let Err(e) = generate_mcp_json(cwd) {
                hook_warnings.push(format!(".mcp.json: {}", e));
            }
        }
        for warning in &hook_warnings {
            eprintln!("{} {}", "warning:".yellow().bold(), warning);
        }
    }

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            println!();
            if result.initialized {
                println!("{} Initialized .writ/", "✓".green());
            } else {
                println!("{} Reinitialized .writ/ (seals preserved)", "✓".green());
            }

            if result.git_imported {
                let seal_short = result
                    .imported_seal_id
                    .as_deref()
                    .map(|s| &s[..12.min(s.len())])
                    .unwrap_or("?");
                let files = result.imported_files.unwrap_or(0);
                let branch = result.git_branch.as_deref().unwrap_or("(detached)");
                let head = result.git_head_short.as_deref().unwrap_or("unknown");

                if result.reimported {
                    println!(
                        "{} Re-imported git baseline ({} @ {}, {} files, seal {})",
                        "✓".green(),
                        branch,
                        head,
                        files,
                        seal_short
                    );
                } else {
                    println!(
                        "{} Imported git baseline ({} @ {}, {} files, seal {})",
                        "✓".green(),
                        branch,
                        head,
                        files,
                        seal_short
                    );
                }
            } else if result.already_imported {
                println!("{} Git baseline already synced", "✓".green());
            }

            if plan.enable_claude {
                println!("{} Claude Code integration configured", "✓".green());
                println!(
                    "  {} Agent permissions: Bash(writ *), mcp__writ__*",
                    "→".green()
                );
                println!(
                    "  {} Agent directive: writ usage instruction added",
                    "→".green()
                );
                println!(
                    "  {} Prompt hook: writ context injected at conversation start",
                    "→".green()
                );
                if cwd.join(".mcp.json").exists() {
                    println!(
                        "  {} MCP server: .mcp.json (native tool integration)",
                        "→".green()
                    );
                }
                println!(
                    "  {} Tip: run {} for auto-convergence when multiple agents work in the same directory",
                    "→".green(),
                    "writ watch".bold()
                );
            }
            if plan.enable_codex {
                println!("{} Codex / OpenAI integration configured", "✓".green());
            }
            if plan.enable_generic {
                println!("{} Generic agent instructions created", "✓".green());
            }

            if result.writignore_created {
                println!("{} Created .writignore", "✓".green());
            }

            let output_fmt = plan.project_config.output_format().unwrap_or("json");
            println!("{} Output format: {}", "✓".green(), output_fmt);

            // W.25: Show workflow mode in init output.
            let commit_mode = plan.project_config.commit_mode().unwrap_or("user");
            let mode_desc = match commit_mode {
                "user" => "run `writ finish` to promote completed work to git",
                "auto" => "fully autonomous commits",
                _ => "",
            };
            println!(
                "{} Workflow: {} mode ({})",
                "✓".green(),
                commit_mode,
                mode_desc
            );

            if let Some(ref err) = result.import_error {
                eprintln!("{} Import error: {}", "✗".red(), err);
            }

            println!();
            println!(
                "{}",
                "Ready. Agents in this directory will use writ automatically.".bold()
            );
        }
    }

    // Create a spec if --spec was provided.
    if let Some(ref id) = spec_id {
        let repo = Repository::open_from_dir(cwd)?;
        let title = spec_title.as_deref().unwrap_or(id);
        let desc = spec_description.as_deref().unwrap_or("");
        repo.add_spec(&Spec::new(id.clone(), title.to_string(), desc.to_string()))?;
        if format != "json" {
            println!("spec: created '{}' ({})", id, title);
        }
    }

    Ok(())
}

fn cmd_uninit(
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
        let repo = Repository::open_from_dir(cwd).ok();
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
            eprintln!("uninit cancelled");
            return Ok(());
        }
    }

    let result = Repository::uninstall(cwd, keep_writignore)?;
    let mcp_removed = remove_mcp_json(cwd);

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

            if mcp_removed {
                println!("removed .mcp.json");
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
            println!("to reinitialize: writ init");
        }
    }

    Ok(())
}

fn cmd_state(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let state = repo.state()?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&state)?);
        }
        "json-compact" => {
            println!("{}", serde_json::to_string(&state)?);
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

fn cmd_task_create(
    cwd: &PathBuf,
    title: &str,
    id: Option<String>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // WV.5: Warn if running from inside an existing workspace.
    let ws_pointer = cwd.join(".writ-workspace");
    if ws_pointer.exists() {
        eprintln!(
            "{} you're inside an existing workspace.",
            "warning:".yellow().bold()
        );
        eprintln!("  Creating a task here would create a nested workspace.");
        eprintln!("  Run `writ task` from the main project directory instead.");
        eprintln!();
        // In non-interactive (no TTY), error out.
        if !std::io::stdin().is_terminal() {
            return Err("refusing to create nested workspace in non-interactive mode".into());
        }
        eprint!("  Continue anyway? [y/N]: ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("  Cancelled.");
            return Ok(());
        }
    }

    let repo = Repository::open_from_dir(cwd)?;
    let result = repo.create_task(title.to_string(), id)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        _ => {
            println!(
                "{} {}",
                "task created:".green().bold(),
                result.spec_id.cyan()
            );
            println!(
                "  {} {}",
                "workspace:".dimmed(),
                result.workspace_path.display()
            );
            println!();
            println!("  Launch an agent in this workspace:");
            println!(
                "    {}",
                format!("cd {}", result.workspace_path.display()).cyan()
            );
            println!();
            println!(
                "  {} \"{}\"",
                "Suggested prompt:".dimmed(),
                result.suggested_prompt
            );
            println!("  {}", "Or provide your own prompt for the agent.".dimmed());
        }
    }

    Ok(())
}

fn cmd_plan(
    cwd: &PathBuf,
    inline_tasks: Vec<String>,
    file: Option<String>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;
    use std::io::{self, BufRead};

    // Collect tasks from inline args, file, or stdin.
    let tasks: Vec<String> = if !inline_tasks.is_empty() {
        inline_tasks
    } else if let Some(path) = file {
        let content = std::fs::read_to_string(&path)?;
        content
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    } else {
        // Read from stdin.
        let stdin = io::stdin();
        stdin
            .lock()
            .lines()
            .map(|l| l.unwrap_or_default().trim().to_string())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    };

    if tasks.is_empty() {
        eprintln!("error: no tasks provided");
        eprintln!("  usage: writ plan \"Task A\" \"Task B\"");
        eprintln!("  or:    writ plan -f tasks.txt");
        eprintln!("  or:    cat tasks.txt | writ plan");
        std::process::exit(1);
    }

    let repo = Repository::open_from_dir(cwd)?;
    let results = repo.plan(tasks)?;

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&results)?;
            println!("{json}");
        }
        _ => {
            println!("\n  {} specs created:", results.len().to_string().bold());
            for r in &results {
                println!("    {}  \"{}\"", r.spec_id.cyan(), r.title);
            }
            println!();
            println!(
                "  Next: launch your agents. They discover specs via {}.",
                "writ context".cyan()
            );
            println!(
                "  Run {} to enable automatic convergence.",
                "writ watch".cyan()
            );
        }
    }

    Ok(())
}

fn cmd_task_list(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    let repo = Repository::open_from_dir(cwd)?;
    let all_specs = repo.list_specs()?;
    let task_specs: Vec<_> = all_specs.iter().filter(|s| s.workspace.is_some()).collect();

    match format {
        "json" => {
            let tasks: Vec<serde_json::Value> = task_specs
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "title": s.title,
                        "status": format!("{:?}", s.status).to_lowercase(),
                        "workspace": s.workspace,
                        "seal_count": s.sealed_by.len(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&tasks)?);
        }
        _ => {
            if task_specs.is_empty() {
                println!("No active tasks.");
                println!();
                println!(
                    "  {} Create one with: {}",
                    "→".dimmed(),
                    "writ task \"description\"".bold()
                );
                return Ok(());
            }

            for spec in &task_specs {
                let status_str = format!("{:?}", spec.status).to_lowercase();
                let seal_count = spec.sealed_by.len();
                let seal_str = if seal_count == 1 {
                    "1 seal".to_string()
                } else {
                    format!("{} seals", seal_count)
                };

                if spec.status == writ_core::spec::SpecStatus::Complete {
                    println!("  {:<24} {}   ({})", spec.id, "complete".green(), seal_str);
                } else {
                    println!("  {:<24} {:<10} {}", spec.id, seal_str, status_str.yellow());
                }
            }

            let task_complete = task_specs
                .iter()
                .filter(|s| s.status == writ_core::spec::SpecStatus::Complete)
                .count();
            let task_in_progress = task_specs.len() - task_complete;
            println!();
            println!(
                "  {} complete / {} in progress",
                task_complete, task_in_progress
            );
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
    let is_agent = detect_agent_from_env().is_some();

    // C.13: Seal enforcement — agents must link seals to specs.
    if spec_id.is_none() {
        if is_agent {
            eprintln!("{} No active spec for this seal.", "ERROR:".red().bold());
            eprintln!("  Run: writ spec add --id <id> --title \"<title>\"");
            eprintln!("  Then retry your seal.");
            eprintln!();
            eprintln!("  Agents must link seals to specs for tracking and coordination.");
            process::exit(1);
        } else {
            eprintln!(
                "{} This seal has no spec. Run `writ spec add` for tracking.",
                "warning:".yellow().bold()
            );
        }
    }

    // C.14: Context token check — warn if writ context wasn't run recently.
    if is_agent && !check_context_token(cwd) {
        eprintln!(
            "{} No `writ context` run detected this session.",
            "warning:".yellow().bold()
        );
        eprintln!("  Run `writ context` first to get project state.");
        eprintln!("  Seal saved, but your work may conflict with other agents.");
    }

    let mut repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
    let mut seals = match (&spec, all) {
        (Some(spec_id), _) => repo.spec_log(spec_id)?,
        (None, true) => repo.log_all()?,
        (None, false) => repo.log()?,
    };

    if let Some(n) = limit {
        seals.truncate(n);
    }

    match format {
        fmt @ ("json" | "json-compact") => {
            if let Some(formatter) = make_formatter(fmt, cwd) {
                println!("{}", formatter.format_seal_log(&seals)?);
            }
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
    spec_filter: Option<String>,
    agent_filter: Option<String>,
    completed: bool,
    all: bool,
    file_filter: Option<String>,
    stat: bool,
    name_only: bool,
    cached: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;

    let mut diff_output = match (&from, &to) {
        (Some(f), Some(t)) => repo.diff_seals(f, t)?,
        (None, None) => repo.diff()?,
        _ => {
            return Err("must provide both --from and --to, or neither".into());
        }
    };

    // --cached: filter to files from completed-but-uncommitted specs.
    // This shows what `writ finish` would commit.
    let effective_completed = completed || cached;

    // Build a set of allowed file paths based on filtering flags.
    let has_filter = spec_filter.is_some()
        || agent_filter.is_some()
        || effective_completed
        || file_filter.is_some();

    if has_filter {
        let allowed_paths: std::collections::HashSet<String> = if let Some(ref path) = file_filter {
            // Single file filter — just that path.
            std::iter::once(path.clone()).collect()
        } else {
            // Collect file paths from seals matching the filter criteria.
            collect_filtered_paths(
                &repo,
                spec_filter.as_deref(),
                agent_filter.as_deref(),
                effective_completed,
                all,
            )?
        };

        diff_output
            .files
            .retain(|f| allowed_paths.contains(&f.path));

        // Recompute totals after filtering.
        diff_output.files_changed = diff_output.files.len();
        diff_output.total_additions = diff_output.files.iter().map(|f| f.additions).sum();
        diff_output.total_deletions = diff_output.files.iter().map(|f| f.deletions).sum();

        // Update description with filter info.
        if let Some(ref id) = spec_filter {
            diff_output.description = format!("{} (spec: {})", diff_output.description, id);
        }
        if let Some(ref name) = agent_filter {
            diff_output.description = format!("{} (agent: {})", diff_output.description, name);
        }
    }

    // Use name-only or stat mode if requested.
    let effective_format = if name_only {
        "name-only"
    } else if stat {
        "stat"
    } else {
        format
    };

    match effective_format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&diff_output)?);
        }
        "json-compact" => {
            println!("{}", serde_json::to_string(&diff_output)?);
        }
        "name-only" => {
            if diff_output.files.is_empty() {
                println!("no changes");
            } else {
                for f in &diff_output.files {
                    println!("{}", f.path);
                }
            }
        }
        "stat" | "brief" => {
            if diff_output.files.is_empty() {
                println!("no changes");
            } else {
                for f in &diff_output.files {
                    let marker = match f.change_type {
                        ChangeType::Added => "+",
                        ChangeType::Modified => "~",
                        ChangeType::Deleted => "-",
                    };
                    println!("  {marker} {} (+{}, -{})", f.path, f.additions, f.deletions);
                }
                println!(
                    "\n{} file(s) changed, {} addition(s), {} deletion(s)",
                    diff_output.files_changed,
                    diff_output.total_additions,
                    diff_output.total_deletions,
                );
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

/// Collect file paths from seals matching the given filter criteria.
/// Used by cmd_diff to filter diff output by spec, agent, or completion status.
fn collect_filtered_paths(
    repo: &Repository,
    spec_filter: Option<&str>,
    agent_filter: Option<&str>,
    completed_only: bool,
    include_all: bool,
) -> Result<std::collections::HashSet<String>, Box<dyn std::error::Error>> {
    use writ_core::spec::SpecStatus;

    let mut paths = std::collections::HashSet::new();

    if let Some(spec_id) = spec_filter {
        // Get all seals for this specific spec.
        if let Ok(seals) = repo.spec_log(spec_id) {
            for seal in &seals {
                for change in &seal.changes {
                    paths.insert(change.path.clone());
                }
            }
        }
        return Ok(paths);
    }

    if agent_filter.is_some() || completed_only {
        // Get all seals across all branches.
        let seals = repo.log_all()?;

        // Build set of completed spec IDs for filtering.
        let completed_specs: std::collections::HashSet<String> = if completed_only && !include_all {
            repo.list_specs()?
                .iter()
                .filter(|s| s.status == SpecStatus::Complete)
                .map(|s| s.id.clone())
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        for seal in &seals {
            // Agent filter: skip seals from other agents.
            if let Some(agent_name) = agent_filter {
                if seal.agent.id != agent_name {
                    continue;
                }
            }

            // Completed filter: skip seals not linked to completed specs.
            if completed_only && !include_all {
                match &seal.spec_id {
                    Some(sid) if completed_specs.contains(sid) => {}
                    _ => continue,
                }
            }

            for change in &seal.changes {
                paths.insert(change.path.clone());
            }
        }
    }

    Ok(paths)
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
    let repo = Repository::open_from_dir(cwd)?;

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

    // Auto-set workspace filter when repo is in a non-main workspace
    let ws = if repo.active_workspace() != "main" {
        Some(repo.active_workspace().to_string())
    } else {
        None
    };

    let filter = ContextFilter {
        status: filter_status,
        agent,
        workspace: ws,
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
            // ── Header ──────────────────────────────────────────────────
            let project_name = ctx
                .workspace
                .as_deref()
                .filter(|w| *w != "main")
                .unwrap_or("project");
            let spec_count = ctx.all_specs.as_ref().map(|s| s.len()).unwrap_or(0);
            println!();
            print!(
                "  {} {}",
                "writ context".green().bold(),
                format!("| {} | {} tracked", project_name, ctx.tracked_files).dimmed(),
            );
            if spec_count > 0 {
                print!(" | {} spec{}", spec_count, if spec_count == 1 { "" } else { "s" });
            }
            println!();

            // ── Session complete banner ─────────────────────────────────
            if let Some(ref ss) = ctx.session_summary {
                println!();
                println!(
                    "  {} {}",
                    "SESSION COMPLETE".green().bold(),
                    ss.headline,
                );
                println!(
                    "  {} file(s) changed. {}",
                    ss.files_changed, ss.message
                );
            }

            // ── Active spec (spec-scoped view) ─────────────────────────
            if let Some(ref spec) = ctx.active_spec {
                println!();
                println!(
                    "  {} {} {}",
                    "spec:".bold(),
                    spec.id.cyan(),
                    format!("({})", format!("{:?}", spec.status).to_lowercase()).dimmed(),
                );
                if !spec.title.is_empty() {
                    println!("        {}", spec.title);
                }
            }

            // ── Specs table ─────────────────────────────────────────────
            if let Some(ref specs) = ctx.all_specs {
                if !specs.is_empty() && ctx.active_spec.is_none() {
                    println!();
                    println!(
                        "  {}",
                        "Specs".bold(),
                    );
                    println!(
                        "  {}",
                        "─".repeat(60).dimmed(),
                    );
                    for spec in specs {
                        let status_str = format!("{:?}", spec.status).to_lowercase();
                        let colored_status = match spec.status {
                            writ_core::spec::SpecStatus::Complete => status_str.green(),
                            writ_core::spec::SpecStatus::Blocked => status_str.red(),
                            writ_core::spec::SpecStatus::InProgress => status_str.yellow(),
                            writ_core::spec::SpecStatus::Pending => status_str.dimmed(),
                        };
                        let claimed = spec
                            .claimed_by
                            .as_deref()
                            .unwrap_or("unclaimed");
                        let seal_count = spec.sealed_by.len();
                        let seal_info = if seal_count > 0 {
                            format!("{} seal{}", seal_count, if seal_count == 1 { "" } else { "s" })
                        } else {
                            String::new()
                        };
                        println!(
                            "  {:<30} {:<14} {:<16} {}",
                            spec.id,
                            colored_status,
                            claimed.dimmed(),
                            seal_info.dimmed(),
                        );
                    }
                }
            }

            // ── Unclaimed specs ─────────────────────────────────────────
            if !ctx.unclaimed_specs.is_empty() {
                println!();
                println!(
                    "  {} {}",
                    "Unclaimed".bold(),
                    format!("({} available)", ctx.unclaimed_specs.len()).dimmed(),
                );
                for us in &ctx.unclaimed_specs {
                    println!(
                        "    {} {}",
                        us.id.cyan(),
                        us.title.dimmed(),
                    );
                }
            }

            // ── Working state ───────────────────────────────────────────
            println!();
            if ctx.working_state.clean {
                println!(
                    "  {} {}",
                    "Working state:".bold(),
                    "clean".green(),
                );
            } else {
                let new_count = ctx.working_state.new_files.len();
                let mod_count = ctx.working_state.modified_files.len();
                let del_count = ctx.working_state.deleted_files.len();
                let mut parts = Vec::new();
                if new_count > 0 {
                    parts.push(format!("+{new_count} new").green().to_string());
                }
                if mod_count > 0 {
                    parts.push(format!("~{mod_count} modified").yellow().to_string());
                }
                if del_count > 0 {
                    parts.push(format!("-{del_count} deleted").red().to_string());
                }
                println!(
                    "  {} {}",
                    "Working state:".bold(),
                    parts.join(", "),
                );
            }

            // ── Pending changes summary ─────────────────────────────────
            if let Some(ref pc) = ctx.pending_changes {
                if pc.files_changed > 0 {
                    println!(
                        "  {} {} file(s) changed ({}, {})",
                        "Pending:".bold(),
                        pc.files_changed,
                        format!("+{}", pc.total_additions).green(),
                        format!("-{}", pc.total_deletions).red(),
                    );
                }
            }

            // ── Seal nudge ──────────────────────────────────────────────
            if let Some(ref nudge) = ctx.seal_nudge {
                println!(
                    "  {} {}",
                    "Nudge:".yellow().bold(),
                    nudge.message,
                );
            }

            // ── Recent seals ────────────────────────────────────────────
            if !ctx.recent_seals.is_empty() {
                println!();
                println!(
                    "  {}",
                    "Recent Seals".bold(),
                );
                println!(
                    "  {}",
                    "─".repeat(60).dimmed(),
                );
                for s in &ctx.recent_seals {
                    let spec_part = s
                        .spec_id
                        .as_deref()
                        .map(|id| format!("  {}", id.dimmed()))
                        .unwrap_or_default();
                    println!(
                        "  {}  {}  {}  {} file{}{}",
                        s.id[..8.min(s.id.len())].yellow(),
                        s.agent.cyan(),
                        s.summary,
                        s.files_changed,
                        if s.files_changed == 1 { "" } else { "s" },
                        spec_part,
                    );
                }
            }

            // ── Integration risk (only if not low) ──────────────────────
            if !ctx.integration_risk.is_low() {
                println!();
                println!(
                    "  {} {} (score: {})",
                    "Risk:".bold(),
                    color_risk_level(&ctx.integration_risk.level),
                    ctx.integration_risk.score,
                );
                for f in &ctx.integration_risk.factors {
                    println!("    {} {f}", "-".dimmed());
                }
            }

            // ── Convergence warning ─────────────────────────────────────
            if ctx.convergence_recommended {
                println!();
                println!(
                    "  {} Diverged branches detected. Run {}.",
                    "CONVERGE:".yellow().bold(),
                    "writ converge-all".cyan(),
                );
            }

            // ── Stale specs ─────────────────────────────────────────────
            if !ctx.stale_specs.is_empty() {
                println!();
                println!(
                    "  {} {}",
                    "Stale:".yellow().bold(),
                    format!("{} spec(s) inactive", ctx.stale_specs.len()).dimmed(),
                );
            }

            // ── Scope violations ────────────────────────────────────────
            if !ctx.file_scope_violations.is_empty() {
                println!();
                println!(
                    "  {} {} violation(s)",
                    "Scope:".red().bold(),
                    ctx.file_scope_violations.len(),
                );
            }

            // ── Recommended action ──────────────────────────────────────
            if let Some(ref action) = ctx.recommended_action {
                println!();
                println!(
                    "  {} {}",
                    "Next:".bold(),
                    action.message,
                );
            }

            println!();
            print_diverged_branch_warnings(&repo);
        }
        other => {
            if let Some(formatter) = make_formatter(other, cwd) {
                println!("{}", formatter.format_context(&ctx)?);
            } else {
                // Fall back to pretty JSON for unknown formats (backward compat)
                println!("{}", serde_json::to_string_pretty(&ctx)?);
            }
        }
    }

    // C.14: Write context token so `writ seal` can verify context was run.
    let token_path = cwd.join(".writ").join(".context_token");
    let _ = std::fs::write(&token_path, chrono::Utc::now().timestamp().to_string());

    Ok(())
}

fn cmd_summary(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
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

fn cmd_status(
    cwd: &PathBuf,
    show_completed: bool,
    show_active: bool,
    filter_agent: Option<String>,
    filter_spec: Option<String>,
    watch: bool,
    interval: u64,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // W.2: Watch mode — live-updating terminal view.
    if watch && format == "human" {
        return cmd_status_watch(
            cwd,
            show_completed,
            show_active,
            filter_agent,
            filter_spec,
            interval,
        );
    }

    let repo = Repository::open_from_dir(cwd)?;
    let status = repo.status()?;

    // Machine-readable output (W.3).
    match format {
        "json" | "json-compact" | "toon" => {
            if let Some(formatter) = make_formatter(format, cwd) {
                println!("{}", formatter.format_status(&status)?);
            } else {
                // Fallback to pretty JSON if unknown format somehow.
                println!("{}", serde_json::to_string_pretty(&status)?);
            }
            return Ok(());
        }
        _ => {}
    }

    // Human-readable display with adaptive scaling.
    use writ_core::status::SpecBrief;

    // Apply filters.
    let filter_briefs = |briefs: &[SpecBrief]| -> Vec<SpecBrief> {
        briefs
            .iter()
            .filter(|b| {
                if let Some(ref agent) = filter_agent {
                    if b.agent != *agent {
                        return false;
                    }
                }
                if let Some(ref spec) = filter_spec {
                    if b.id != *spec {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    };

    let completed = filter_briefs(&status.specs_completed);
    let in_progress = filter_briefs(&status.specs_in_progress);
    let stale = filter_briefs(&status.stale_specs);

    let total_specs = completed.len() + in_progress.len();

    // Header.
    let now = chrono::Local::now();
    println!();
    println!(
        "{}",
        format!(
            "── writ status ── {} ── {} ────────────",
            status.project_name,
            now.format("%-I:%M %p")
        )
        .dimmed()
    );
    println!();

    // Agent summary.
    if status.agents.total > 0 {
        if status.agents.active > 0 {
            println!(
                "  {}  {:>3} agent{}   {} spec{} in progress",
                "Active".green().bold(),
                status.agents.active,
                if status.agents.active == 1 { "" } else { "s" },
                in_progress.len(),
                if in_progress.len() == 1 { "" } else { "s" }
            );
        }
        if status.agents.done > 0 {
            println!(
                "  {}    {:>3} agent{}   {} spec{} completed (not committed)",
                "Done".cyan().bold(),
                status.agents.done,
                if status.agents.done == 1 { "" } else { "s" },
                completed.len(),
                if completed.len() == 1 { "" } else { "s" }
            );
        }
        if status.agents.idle > 0 {
            println!(
                "  {}    {:>3} agent{}",
                "Idle".dimmed().bold(),
                status.agents.idle,
                if status.agents.idle == 1 { "" } else { "s" }
            );
        }
    } else {
        println!("  No agent activity yet.");
    }

    // Adaptive scaling for spec display.
    let show_all_completed = show_completed || total_specs <= 15;
    let show_all_active = show_active || total_specs <= 5;
    let max_completed = if show_all_completed {
        completed.len()
    } else if total_specs <= 50 {
        5
    } else {
        0
    };
    let max_active = if show_all_active {
        in_progress.len()
    } else {
        0
    };

    // Completed specs.
    if !completed.is_empty() {
        println!();
        println!(
            "{}",
            "── Completed (ready to finish) ─────────────────────".dimmed()
        );
        for brief in completed.iter().take(max_completed) {
            print_spec_brief(brief);
        }
        if max_completed < completed.len() {
            println!(
                "  {} more completed spec{} (use --completed to see all)",
                completed.len() - max_completed,
                if completed.len() - max_completed == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }

        println!();
        println!(
            "  {} spec{} complete · {} file{} changed · run {} when ready",
            completed.len(),
            if completed.len() == 1 { "" } else { "s" },
            status.total_files_changed,
            if status.total_files_changed == 1 {
                ""
            } else {
                "s"
            },
            "`writ finish`".bold()
        );
    }

    // In-progress specs.
    if !in_progress.is_empty() {
        println!();
        println!(
            "{}",
            "── In Progress ─────────────────────────────────────".dimmed()
        );
        if max_active > 0 {
            for brief in in_progress.iter().take(max_active) {
                print_spec_brief(brief);
            }
            if max_active < in_progress.len() {
                println!(
                    "  ... {} more (use --active to see all)",
                    in_progress.len() - max_active
                );
            }
        }
        println!(
            "  {} spec{} across {} agent{}",
            in_progress.len(),
            if in_progress.len() == 1 { "" } else { "s" },
            status.agents.active,
            if status.agents.active == 1 { "" } else { "s" }
        );
    }

    // Stale specs.
    if !stale.is_empty() {
        println!();
        println!(
            "{}",
            "── Stale ───────────────────────────────────────────".dimmed()
        );
        for brief in &stale {
            let age = chrono::Utc::now()
                .signed_duration_since(brief.last_activity)
                .num_minutes();
            let age_str = if age >= 60 {
                format!("last seal {}h ago", age / 60)
            } else {
                format!("last seal {}m ago", age)
            };
            println!(
                "  {} {}  {:<30} {:<8} {}",
                "⚠".yellow(),
                brief.id,
                brief.title,
                brief.agent,
                age_str.dimmed()
            );
        }
    }

    // WV.7: Tasks section — specs with associated workspaces.
    let all_specs = repo.list_specs()?;
    let task_specs: Vec<_> = all_specs.iter().filter(|s| s.workspace.is_some()).collect();

    if !task_specs.is_empty() {
        println!();
        println!(
            "{}",
            "── Tasks ───────────────────────────────────────────".dimmed()
        );

        for spec in &task_specs {
            let status_str = format!("{:?}", spec.status).to_lowercase();
            let seal_count = spec.sealed_by.len();
            let seal_str = if seal_count == 1 {
                "1 seal".to_string()
            } else {
                format!("{} seals", seal_count)
            };

            let display = if spec.status == writ_core::spec::SpecStatus::Complete {
                format!("  {:<24} {}   ({})", spec.id, "complete".green(), seal_str)
            } else {
                format!("  {:<24} {:<10} {}", spec.id, seal_str, status_str.yellow())
            };
            println!("{display}");
        }

        let task_complete = task_specs
            .iter()
            .filter(|s| s.status == writ_core::spec::SpecStatus::Complete)
            .count();
        let task_in_progress = task_specs.len() - task_complete;
        println!();
        println!(
            "  {} complete / {} in progress",
            task_complete, task_in_progress
        );
        if task_complete > 0 {
            println!(
                "  Run {} when ready, or {} to converge and commit.",
                "`writ converge-all`".bold(),
                "`writ finish`".bold()
            );
        }
    }

    // Workspace overview (WS.18): show when multiple workspaces exist.
    let workspaces = repo.list_workspaces()?;
    if workspaces.len() > 1 {
        println!();
        println!(
            "{}",
            "── Workspaces ──────────────────────────────────────".dimmed()
        );
        for ws in &workspaces {
            let path_str = if ws.is_main {
                ".".to_string()
            } else {
                ws.path.display().to_string()
            };

            // Count specs by status for this workspace.
            let ws_specs: Vec<_> = all_specs
                .iter()
                .filter(|s| {
                    if ws.is_main {
                        s.workspace.is_none() || s.workspace.as_deref() == Some("main")
                    } else {
                        s.workspace.as_deref() == Some(&ws.name)
                    }
                })
                .collect();
            let ws_complete = ws_specs
                .iter()
                .filter(|s| s.status == writ_core::spec::SpecStatus::Complete)
                .count();
            let ws_in_progress = ws_specs
                .iter()
                .filter(|s| s.status == writ_core::spec::SpecStatus::InProgress)
                .count();

            let suffix = if ws.is_main {
                "  base workspace".to_string()
            } else if ws_complete == ws_specs.len() && !ws_specs.is_empty() {
                format!("  {} complete ← ready to converge", ws_complete)
            } else {
                let mut parts = Vec::new();
                if ws_complete > 0 {
                    parts.push(format!("{} complete", ws_complete));
                }
                if ws_in_progress > 0 {
                    parts.push(format!("{} in-progress", ws_in_progress));
                }
                if parts.is_empty() {
                    String::new()
                } else {
                    format!("  {}", parts.join(", "))
                }
            };

            println!(
                "  {:<20} {:<25} {} specs{}",
                ws.name,
                path_str,
                ws_specs.len(),
                suffix
            );
        }
    }

    // Empty state.
    if completed.is_empty() && in_progress.is_empty() {
        println!();
        println!(
            "  No specs yet. Create one with {}.",
            "`writ spec add`".bold()
        );
    }

    println!();

    Ok(())
}

/// W.2: Watch mode — live-updating terminal view with keyboard shortcuts.
fn cmd_status_watch(
    cwd: &PathBuf,
    show_completed: bool,
    show_active: bool,
    filter_agent: Option<String>,
    filter_spec: Option<String>,
    interval: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEvent},
        terminal::{self, ClearType},
    };
    use std::io::Write;
    use std::time::Duration;

    terminal::enable_raw_mode()?;

    let result = (|| -> Result<Option<&str>, Box<dyn std::error::Error>> {
        loop {
            // Clear screen and move cursor to top.
            let mut stdout = std::io::stdout();
            crossterm::execute!(
                stdout,
                terminal::Clear(ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            )?;

            // Render status.
            let repo = Repository::open_from_dir(cwd)?;
            let status = repo.status()?;

            let now = chrono::Local::now();
            println!(
                "{}",
                format!(
                    "── writ status ── {} ── LIVE ({}s) ── {} ────────────",
                    status.project_name,
                    interval,
                    now.format("%-I:%M:%S %p")
                )
                .dimmed()
            );
            println!();

            // Agent summary.
            if status.agents.total > 0 {
                if status.agents.active > 0 {
                    println!(
                        "  {}  {:>3} agent{}   {} spec{} in progress",
                        "Active".green().bold(),
                        status.agents.active,
                        if status.agents.active == 1 { "" } else { "s" },
                        status.specs_in_progress.len(),
                        if status.specs_in_progress.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    );
                }
                if status.agents.done > 0 {
                    println!(
                        "  {}    {:>3} agent{}   {} spec{} completed",
                        "Done".cyan().bold(),
                        status.agents.done,
                        if status.agents.done == 1 { "" } else { "s" },
                        status.specs_completed.len(),
                        if status.specs_completed.len() == 1 {
                            ""
                        } else {
                            "s"
                        }
                    );
                }
            } else {
                println!("  No agent activity yet.");
            }

            // Completed specs.
            let completed: Vec<_> = status
                .specs_completed
                .iter()
                .filter(|b| {
                    filter_agent.as_ref().map_or(true, |a| b.agent == *a)
                        && filter_spec.as_ref().map_or(true, |s| b.id == *s)
                })
                .collect();

            if !completed.is_empty() {
                println!();
                println!(
                    "{}",
                    "── Completed ───────────────────────────────────────".dimmed()
                );
                let max = if show_completed {
                    completed.len()
                } else {
                    5.min(completed.len())
                };
                for brief in completed.iter().take(max) {
                    print_spec_brief(brief);
                }
                if max < completed.len() {
                    println!("  ... {} more", completed.len() - max);
                }
            }

            // In-progress specs.
            let active: Vec<_> = status
                .specs_in_progress
                .iter()
                .filter(|b| {
                    filter_agent.as_ref().map_or(true, |a| b.agent == *a)
                        && filter_spec.as_ref().map_or(true, |s| b.id == *s)
                })
                .collect();

            if !active.is_empty() {
                println!();
                println!(
                    "{}",
                    "── In Progress ─────────────────────────────────────".dimmed()
                );
                let max = if show_active {
                    active.len()
                } else {
                    5.min(active.len())
                };
                for brief in active.iter().take(max) {
                    print_spec_brief(brief);
                }
                if max < active.len() {
                    println!("  ... {} more", active.len() - max);
                }
            }

            // Stale.
            if !status.stale_specs.is_empty() {
                println!();
                println!(
                    "{}",
                    "── Stale ───────────────────────────────────────────".dimmed()
                );
                for brief in &status.stale_specs {
                    println!(
                        "  {} {}  {}  (possibly stale)",
                        "⚠".yellow(),
                        brief.id,
                        brief.title
                    );
                }
            }

            println!();
            println!("  {}", "Press q to quit · f to finish · d to diff".dimmed());
            stdout.flush()?;

            // Poll for keyboard input until interval expires.
            let poll_timeout = Duration::from_secs(interval);
            if event::poll(poll_timeout)? {
                if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                    match code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                        KeyCode::Char('f') => return Ok(Some("finish")),
                        KeyCode::Char('d') => return Ok(Some("diff")),
                        _ => {}
                    }
                }
            }
        }
    })();

    terminal::disable_raw_mode()?;

    // Handle post-watch command dispatch.
    match result {
        Ok(Some("finish")) => {
            println!();
            cmd_finish(cwd, false, false, false, false, false, "single")?;
        }
        Ok(Some("diff")) => {
            println!();
            let format = resolve_format(None, cwd, "human");
            cmd_diff(
                cwd, None, None, &format, None, None, false, false, None, false, false, false,
            )?;
        }
        Ok(_) => {} // quit
        Err(e) => return Err(e),
    }

    Ok(())
}

/// Print a single SpecBrief line for status display.
fn print_spec_brief(brief: &writ_core::status::SpecBrief) {
    let age = chrono::Utc::now()
        .signed_duration_since(brief.last_activity)
        .num_minutes();
    let age_str = if age < 1 {
        "just now".to_string()
    } else if age < 60 {
        format!("{}m ago", age)
    } else {
        format!("{}h ago", age / 60)
    };

    println!(
        "  {}  {:<30} {:<8} {} seal{}   {}",
        brief.id,
        brief.title,
        brief.agent,
        brief.seal_count,
        if brief.seal_count == 1 { " " } else { "s" },
        age_str.dimmed()
    );
}

fn cmd_finish(
    cwd: &PathBuf,
    full: bool,
    dry_run: bool,
    yes: bool,
    cleanup: bool,
    no_cleanup: bool,
    strategy: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;
    use writ_core::git_ops::{Git2Ops, GitOps};

    let repo = Repository::open_from_dir(cwd)?;

    // WV.6: Auto-converge outstanding workspaces before finishing.
    let workspaces = repo.list_workspaces()?;
    let non_main_ws: Vec<_> = workspaces.iter().filter(|ws| !ws.is_main).collect();
    if !non_main_ws.is_empty() {
        let ws_names: Vec<String> = non_main_ws.iter().map(|ws| ws.name.clone()).collect();
        println!(
            "  {} {} {} workspace{}...",
            "→".dimmed(),
            if dry_run {
                "Would converge"
            } else {
                "Converging"
            },
            ws_names.len(),
            if ws_names.len() == 1 { "" } else { "s" }
        );
        let report = repo.converge_workspaces(&ws_names, "three-way-merge", dry_run)?;
        if !report.escalations.is_empty() {
            eprintln!();
            eprintln!(
                "{} {} escalation{} need resolution:",
                "error:".red().bold(),
                report.escalations.len(),
                if report.escalations.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
            for esc in &report.escalations {
                eprintln!("  {} {}", "·".red(), esc.file_path);
            }
            eprintln!();
            eprintln!(
                "  Resolve escalations before finishing. Run {} to see details.",
                "`writ converge-all --dry-run`".bold()
            );
            return Err("convergence has unresolved escalations".into());
        }
        if report.is_clean {
            println!(
                "  {} Converged {} file{} cleanly.",
                "✓".green(),
                report.total_auto_merged,
                if report.total_auto_merged == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }
    }

    // Gather completed specs for display
    let specs = repo.list_specs()?;
    let committable: Vec<_> = specs.iter().filter(|s| s.is_committable()).collect();
    let in_progress: Vec<_> = specs
        .iter()
        .filter(|s| {
            matches!(
                s.status,
                writ_core::spec::SpecStatus::InProgress | writ_core::spec::SpecStatus::Pending
            )
        })
        .collect();

    if committable.is_empty() {
        // Fall back to legacy behavior: commit whatever is in the working tree
        let summary = repo.summary()?;
        if summary.files_to_stage.is_empty() {
            println!("Nothing to commit — no completed specs and no changes.");
            println!();
            println!(
                "  {} Use `writ spec done <id>` to mark a spec as complete.",
                "→".dimmed()
            );
            println!(
                "  {} Use `writ status` to see current progress.",
                "→".dimmed()
            );
            return Ok(());
        }

        // Legacy path: commit all changes with summary message
        let commit_message = if full {
            summary.commit_message.clone()
        } else {
            let files = summary.files_changed.len();
            format!("{} ({} files)", summary.headline, files)
        };

        return finish_legacy(cwd, &commit_message, &summary.files_to_stage, dry_run, full);
    }

    // Show what we're about to commit
    println!();
    println!("{}", "Completed specs ready to commit:".bold());
    for s in &committable {
        let summary_hint = s.completion_summary.as_deref().unwrap_or("(no summary)");
        println!("  {} {} — {}", "✓".green(), s.id.cyan(), summary_hint);
    }

    if !in_progress.is_empty() {
        println!();
        println!("{}", "In-progress specs (not included):".dimmed());
        for s in &in_progress {
            println!("  {} {}", "·".dimmed(), s.id.dimmed());
        }
    }

    // Generate commit message
    let summary = repo.summary()?;
    let commit_message = if full {
        summary.commit_message.clone()
    } else {
        // Build a message from completed spec summaries
        let mut msg = summary.headline.clone();
        if committable.len() > 1 {
            msg = format!("{} ({} specs)", msg, committable.len());
        }
        msg
    };

    println!();
    println!("{}:", "Commit message".bold());
    println!("  {}", commit_message.lines().next().unwrap_or(""));
    if full {
        for line in commit_message.lines().skip(1) {
            println!("  {}", line);
        }
    }
    println!("Strategy: {}", strategy);

    if dry_run {
        println!();
        println!("{}", "DRY RUN — no changes made.".yellow().bold());
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

    // Confirm unless --yes
    if !yes {
        print!("\nProceed? [Y/n] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_lowercase();
        if input == "n" || input == "no" {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Open git repo via GitOps
    let git = Git2Ops::open(cwd)?;

    match strategy {
        "single" => {
            // Single commit: stage all, commit once, mark all specs
            git.stage_all()?;

            if !git.has_staged_changes()? {
                println!("Nothing to commit — working tree clean.");
                return Ok(());
            }

            let hash = git.commit(&commit_message)?;
            let short_hash = &hash[..std::cmp::min(8, hash.len())];

            // Mark all committable specs
            for s in &committable {
                let _ = repo.mark_spec_committed(&s.id, &hash);
            }

            println!();
            println!(
                "  {} Committed {} — {}",
                "✓".green().bold(),
                short_hash.cyan(),
                commit_message.lines().next().unwrap_or("")
            );
            println!(
                "  {} {} spec(s) marked as committed.",
                "✓".green().bold(),
                committable.len()
            );
        }
        "per-spec" => {
            // Per-spec commits: sort by completed_at, one commit per spec.
            // Uses file_scope for isolation when available; falls back to
            // stage_all for the first spec if no file_scope is set.
            let mut sorted: Vec<_> = committable.clone();
            sorted.sort_by_key(|s| s.completed_at);

            let mut staged_all = false;
            for s in &sorted {
                if !s.file_scope.is_empty() {
                    // Stage only this spec's files
                    let paths: Vec<&str> = s.file_scope.iter().map(|p| p.as_str()).collect();
                    git.stage_files(&paths)?;
                } else if !staged_all {
                    // No file_scope — stage everything on the first pass
                    git.stage_all()?;
                    staged_all = true;
                }

                if !git.has_staged_changes()? {
                    continue;
                }

                let msg = s.completion_summary.as_deref().unwrap_or(&s.title);
                let spec_msg = format!("{}: {}", s.id, msg);

                let hash = git.commit(&spec_msg)?;
                let short = &hash[..std::cmp::min(8, hash.len())];
                let _ = repo.mark_spec_committed(&s.id, &hash);

                println!("  {} {} — {} ({})", "✓".green(), short.cyan(), s.id, msg);
            }
        }
        "grouped" => {
            // Grouped commits: auto-detect logical groupings by directory prefix.
            // Specs sharing a common directory prefix are committed together.
            let groups = compute_spec_groups(&committable);

            if groups.len() == 1 {
                println!(
                    "  {} All specs share the same area — committing as single group.",
                    "→".dimmed()
                );
            } else {
                println!(
                    "  {} {} groups detected by directory prefix:",
                    "→".dimmed(),
                    groups.len()
                );
                for (i, group) in groups.iter().enumerate() {
                    let spec_ids: Vec<&str> = group.specs.iter().map(|s| s.id.as_str()).collect();
                    println!(
                        "    Group {}: \"{}\" ({})",
                        i + 1,
                        group.label,
                        spec_ids.join(", ")
                    );
                }
                println!();
            }

            for group in &groups {
                // Collect all files from specs in this group
                let files: Vec<&str> = group
                    .specs
                    .iter()
                    .flat_map(|s| s.file_scope.iter().map(|f| f.as_str()))
                    .collect();

                if !files.is_empty() {
                    git.stage_files(&files)?;
                } else {
                    // No file_scope on any spec in the group — stage all
                    git.stage_all()?;
                }

                if !git.has_staged_changes()? {
                    continue;
                }

                // Build commit message from group specs
                let summaries: Vec<String> = group
                    .specs
                    .iter()
                    .map(|s| {
                        let msg = s.completion_summary.as_deref().unwrap_or(&s.title);
                        format!("{}: {}", s.id, msg)
                    })
                    .collect();
                let group_msg = if summaries.len() == 1 {
                    summaries[0].clone()
                } else {
                    format!("{}\n\n{}", group.label, summaries.join("\n"))
                };

                let hash = git.commit(&group_msg)?;
                let short = &hash[..std::cmp::min(8, hash.len())];

                for s in &group.specs {
                    let _ = repo.mark_spec_committed(&s.id, &hash);
                }

                let spec_ids: Vec<&str> = group.specs.iter().map(|s| s.id.as_str()).collect();
                println!(
                    "  {} {} — {} ({})",
                    "✓".green(),
                    short.cyan(),
                    group.label,
                    spec_ids.join(", ")
                );
            }
        }
        other => {
            eprintln!(
                "error: unknown strategy '{}'. Use: single, per-spec, grouped",
                other
            );
            std::process::exit(1);
        }
    }

    // WV.6: Workspace cleanup after successful commit.
    if !non_main_ws.is_empty() {
        println!();
        println!(
            "  {} {} task{} committed.",
            "✓".green().bold(),
            non_main_ws.len(),
            if non_main_ws.len() == 1 { "" } else { "s" }
        );

        let should_cleanup = if no_cleanup {
            false
        } else if cleanup || yes {
            true
        } else {
            use std::io::IsTerminal;
            if std::io::stdin().is_terminal() {
                print!("\n  Clean up workspaces? [Y/n]: ");
                std::io::Write::flush(&mut std::io::stdout())?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                let input = input.trim().to_lowercase();
                input.is_empty() || input == "y" || input == "yes"
            } else {
                false
            }
        };

        if should_cleanup {
            for ws in &non_main_ws {
                if ws.path.exists() {
                    if let Err(e) = std::fs::remove_dir_all(&ws.path) {
                        eprintln!("  warning: could not remove {}: {}", ws.path.display(), e);
                    } else {
                        println!("  removed {}/", ws.path.display());
                    }
                }
                let _ = repo.delete_workspace(&ws.name, true);
            }
        }
    }

    println!();
    println!("  {} Run `git push` when ready.", "→".dimmed());

    Ok(())
}

/// A group of specs sharing a common directory prefix for grouped commit strategy.
struct SpecGroup<'a> {
    /// Human-readable label for this group (the common directory prefix or fallback).
    label: String,
    /// Specs in this group.
    specs: Vec<&'a writ_core::spec::Spec>,
}

/// Compute logical groupings of specs by common directory prefix of their file_scope.
///
/// Algorithm:
/// 1. For each spec, compute the common directory prefix of its changed files.
/// 2. Group specs sharing the same prefix.
/// 3. Specs with empty file_scope go into a "misc" catch-all group.
fn compute_spec_groups<'a>(specs: &[&'a writ_core::spec::Spec]) -> Vec<SpecGroup<'a>> {
    use std::collections::BTreeMap;

    let mut prefix_groups: BTreeMap<String, Vec<&'a writ_core::spec::Spec>> = BTreeMap::new();

    for spec in specs {
        let prefix = common_directory_prefix(&spec.file_scope);
        prefix_groups.entry(prefix).or_default().push(spec);
    }

    prefix_groups
        .into_iter()
        .map(|(prefix, group_specs)| {
            let label = if prefix.is_empty() {
                "misc".to_string()
            } else {
                prefix
            };
            SpecGroup {
                label,
                specs: group_specs,
            }
        })
        .collect()
}

/// Compute the common directory prefix for a list of file paths.
///
/// Examples:
/// - `["src/storage/zstd.rs", "src/storage/compress.rs"]` → `"src/storage/"`
/// - `["src/a.rs", "tests/b.rs"]` → `""` (no common prefix beyond root)
/// - `["crates/writ-py/src/lib.rs"]` → `"crates/writ-py/src/"`
/// - `[]` → `""`
fn common_directory_prefix(paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }

    // Extract directory portions of each path
    let dirs: Vec<&str> = paths
        .iter()
        .map(|p| {
            match p.rfind('/') {
                Some(i) => &p[..=i], // include trailing slash
                None => "",          // file in root
            }
        })
        .collect();

    if dirs.is_empty() {
        return String::new();
    }

    // Find common prefix across all directory strings
    let first = dirs[0];
    let mut common_len = first.len();

    for dir in &dirs[1..] {
        common_len = first
            .chars()
            .zip(dir.chars())
            .take(common_len)
            .take_while(|(a, b)| a == b)
            .count();
        if common_len == 0 {
            return String::new();
        }
    }

    let prefix = &first[..common_len];

    // Trim to last '/' boundary so we don't split mid-directory
    match prefix.rfind('/') {
        Some(i) => prefix[..=i].to_string(),
        None => String::new(),
    }
}

/// Legacy finish path: no spec awareness, just stage and commit.
fn finish_legacy(
    cwd: &PathBuf,
    commit_message: &str,
    files_to_stage: &[String],
    dry_run: bool,
    full: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

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
        println!("Files that would be staged ({}):", files_to_stage.len());
        for f in files_to_stage {
            println!("  {f}");
        }
        return Ok(());
    }

    // Use GitOps for the actual commit
    use writ_core::git_ops::{Git2Ops, GitOps};
    let git = Git2Ops::open(cwd)?;
    git.stage_all()?;

    if !git.has_staged_changes()? {
        println!("Nothing to commit — working tree clean.");
        return Ok(());
    }

    let hash = git.commit(commit_message)?;
    let short = &hash[..std::cmp::min(8, hash.len())];

    println!(
        "  {} Committed {} — {}",
        "✓".green().bold(),
        short.cyan(),
        commit_message.lines().next().unwrap_or("")
    );

    Ok(())
}

/// Create a proposal instead of committing directly.
fn cmd_finish_propose(
    cwd: &PathBuf,
    full: bool,
    strategy: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    let repo = Repository::open_from_dir(cwd)?;
    let specs = repo.list_specs()?;
    let committable: Vec<_> = specs.iter().filter(|s| s.is_committable()).collect();

    if committable.is_empty() {
        eprintln!("No completed specs to propose.");
        eprintln!("  {} Use `writ spec done <id>` first.", "→".dimmed());
        std::process::exit(1);
    }

    // Generate commit message
    let summary = repo.summary()?;
    let message = if full {
        summary.commit_message.clone()
    } else {
        let mut msg = summary.headline.clone();
        if committable.len() > 1 {
            msg = format!("{} ({} specs)", msg, committable.len());
        }
        msg
    };

    let spec_ids: Vec<String> = committable.iter().map(|s| s.id.clone()).collect();
    let proposal = repo.create_proposal(spec_ids, message, "cli".into(), strategy.into())?;

    println!();
    println!(
        "  {} Proposal {} created.",
        "✓".green().bold(),
        proposal.id.cyan()
    );
    println!("  Specs: {}", proposal.spec_ids.join(", "));
    println!(
        "  Message: {}",
        proposal.message.lines().next().unwrap_or("")
    );
    println!();
    println!("  {} Review: `writ finish --proposals`", "→".dimmed());
    println!(
        "  {} Accept: `writ finish --accept {}`",
        "→".dimmed(),
        proposal.id
    );

    Ok(())
}

/// List pending proposals.
fn cmd_finish_proposals(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    let repo = Repository::open_from_dir(cwd)?;
    let proposals = repo.list_proposals()?;
    let pending: Vec<_> = proposals.iter().filter(|p| p.is_pending()).collect();

    if pending.is_empty() {
        println!("No pending proposals.");
        println!(
            "  {} Use `writ finish --propose` to create one.",
            "→".dimmed()
        );
        return Ok(());
    }

    println!();
    println!("{}", "Pending proposals:".bold());
    for p in &pending {
        println!();
        println!("  {} {}", "ID:".dimmed(), p.id.cyan());
        println!("  {} {}", "Specs:".dimmed(), p.spec_ids.join(", "));
        println!(
            "  {} {}",
            "Message:".dimmed(),
            p.message.lines().next().unwrap_or("")
        );
        println!(
            "  {} {} ({})",
            "By:".dimmed(),
            p.proposed_by,
            p.created_at.format("%Y-%m-%d %H:%M")
        );
    }
    println!();
    println!("  {} Accept: `writ finish --accept <id>`", "→".dimmed());
    println!("  {} Reject: `writ finish --reject <id>`", "→".dimmed());

    Ok(())
}

/// Accept a proposal: commit and mark accepted.
fn cmd_finish_accept(
    cwd: &PathBuf,
    proposal_id: &str,
    strategy: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;
    use writ_core::git_ops::{Git2Ops, GitOps};

    let repo = Repository::open_from_dir(cwd)?;
    let proposal = repo.accept_proposal(proposal_id)?;

    // Execute the commit
    let git = Git2Ops::open(cwd)?;
    git.stage_all()?;

    if !git.has_staged_changes()? {
        println!("Nothing to commit — working tree clean.");
        return Ok(());
    }

    let hash = git.commit(&proposal.message)?;
    let short = &hash[..std::cmp::min(8, hash.len())];

    // Update proposal with actual hash
    let _ = repo.update_proposal_hash(proposal_id, &hash);

    // Mark specs as committed
    for spec_id in &proposal.spec_ids {
        let _ = repo.mark_spec_committed(spec_id, &hash);
    }

    println!();
    println!(
        "  {} Proposal {} accepted.",
        "✓".green().bold(),
        proposal_id.cyan()
    );
    println!(
        "  {} Committed {} — {}",
        "✓".green().bold(),
        short.cyan(),
        proposal.message.lines().next().unwrap_or("")
    );
    println!(
        "  {} {} spec(s) marked as committed.",
        "✓".green().bold(),
        proposal.spec_ids.len()
    );
    println!();
    println!("  {} Run `git push` when ready.", "→".dimmed());

    Ok(())
}

/// Reject a proposal.
fn cmd_finish_reject(cwd: &PathBuf, proposal_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    let repo = Repository::open_from_dir(cwd)?;
    let proposal = repo.reject_proposal(proposal_id)?;

    println!();
    println!(
        "  {} Proposal {} rejected.",
        "✗".red().bold(),
        proposal_id.cyan()
    );
    println!("  Specs remain completed — create a new proposal when ready.");

    Ok(())
}

/// Auto mode: commit without prompts, with safety rails from project config.
fn cmd_finish_auto(cwd: &PathBuf, strategy: &str) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;
    use writ_core::config::ProjectConfig;
    use writ_core::git_ops::{Git2Ops, GitOps};

    let repo = Repository::open_from_dir(cwd)?;

    // Load auto config
    let project_config = ProjectConfig::load(repo.writ_dir()).unwrap_or_default();
    let auto_config = project_config.auto.clone().unwrap_or_default();

    // Safety: warn if targeting main/master
    let git = Git2Ops::open(cwd)?;
    if let Some(ref target_branch) = auto_config.branch {
        if target_branch == "main" || target_branch == "master" {
            eprintln!(
                "{}",
                "WARNING: auto mode targeting main/master branch. This is not recommended."
                    .yellow()
                    .bold()
            );
        }
        // Checkout target branch
        git.checkout_or_create_branch(target_branch)?;
    } else {
        // No branch configured — check current branch
        if let Ok(Some(ref branch)) = git.current_branch() {
            if branch == "main" || branch == "master" {
                eprintln!(
                    "{}",
                    "WARNING: auto mode on main/master. Set [auto] branch in config.toml."
                        .yellow()
                        .bold()
                );
            }
        }
    }

    // Safety: run verify command if configured
    if let Some(ref cmd) = auto_config.verify_command {
        eprintln!("Running verify command: {}", cmd);
        let output = std::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(cwd)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "{}",
                "Auto-commit BLOCKED: verify command failed.".red().bold()
            );
            eprintln!("  Command: {}", cmd);
            eprintln!("  Exit code: {}", output.status);
            if !stderr.is_empty() {
                eprintln!("  stderr: {}", stderr.trim());
            }
            std::process::exit(1);
        }
        eprintln!("  {} Verify command passed.", "✓".green());
    }

    // Gather completed specs
    let specs = repo.list_specs()?;
    let committable: Vec<_> = specs.iter().filter(|s| s.is_committable()).collect();

    if committable.is_empty() {
        eprintln!("Auto mode: no completed specs to commit.");
        std::process::exit(0);
    }

    // Apply max_specs_per_commit batching
    let max_per_commit = auto_config.max_specs_per_commit.unwrap_or(u32::MAX) as usize;
    let batches: Vec<Vec<_>> = committable
        .chunks(max_per_commit)
        .map(|chunk| chunk.to_vec())
        .collect();

    let summary = repo.summary()?;
    let mut total_committed = 0;

    for (i, batch) in batches.iter().enumerate() {
        git.stage_all()?;
        if !git.has_staged_changes()? {
            continue;
        }

        // Generate message for this batch
        let message = if batches.len() == 1 {
            summary.headline.clone()
        } else {
            format!("{} (batch {}/{})", summary.headline, i + 1, batches.len())
        };

        let hash = git.commit(&message)?;
        let short = &hash[..std::cmp::min(8, hash.len())];

        // Mark specs committed
        for s in batch {
            let _ = repo.mark_spec_committed(&s.id, &hash);
        }

        total_committed += batch.len();
        eprintln!(
            "  {} {} — {} ({} specs)",
            "✓".green(),
            short,
            message.lines().next().unwrap_or(""),
            batch.len()
        );

        // Log as security event (audit trail)
        let logger = writ_core::security::SecurityEventLogger::new(repo.writ_dir());
        let event = writ_core::security::SecurityEvent {
            timestamp: chrono::Utc::now(),
            severity: writ_core::security::Severity::Info,
            event_type: "auto_commit".to_string(),
            agent_id: None,
            details: format!("Auto-committed {} specs: {}", batch.len(), hash),
        };
        let _ = logger.emit_event(&event);
    }

    // Notification
    let notify = auto_config.notify.as_deref().unwrap_or("log");
    match notify {
        "stdout" => {
            println!(
                "AUTO: committed {} spec(s) in {} batch(es).",
                total_committed,
                batches.len()
            );
        }
        "none" => {}
        _ => {
            // "log" is default — already logged via security events above
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
    let seal = repo.get_seal(seal_id)?;

    match format {
        "json" | "json-compact" => {
            let compact = format == "json-compact";
            if show_diff {
                let diff = repo.diff_seal(seal_id)?;
                let combined = serde_json::json!({
                    "seal": seal,
                    "diff": diff,
                });
                if compact {
                    println!("{}", serde_json::to_string(&combined)?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&combined)?);
                }
            } else if compact {
                println!("{}", serde_json::to_string(&seal)?);
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
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

    let repo = Repository::open_from_dir(cwd)?;
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

    if format == "json" || format == "json-compact" {
        if let Some(formatter) = make_formatter(format, cwd) {
            println!("{}", formatter.format_spec_list(&specs)?);
        }
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

fn cmd_spec_done(
    cwd: &PathBuf,
    id: Option<&str>,
    summary: Option<String>,
    agent: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use colored::Colorize;

    let repo = Repository::open_from_dir(cwd)?;

    // Auto-detect spec ID if not provided
    let spec_id = match id {
        Some(id) => id.to_string(),
        None => {
            let specs = repo.list_specs()?;
            let active: Vec<_> = specs
                .iter()
                .filter(|s| {
                    matches!(
                        s.status,
                        writ_core::spec::SpecStatus::InProgress
                            | writ_core::spec::SpecStatus::Pending
                    )
                })
                .collect();

            match active.len() {
                0 => {
                    eprintln!("error: no active specs to complete.");
                    eprintln!("hint: use `writ spec add` to create a spec first.");
                    std::process::exit(1);
                }
                1 => {
                    let id = active[0].id.clone();
                    println!("Auto-detected active spec: {} \"{}\"", id, active[0].title);
                    id
                }
                n => {
                    eprintln!("error: {} active specs found. Please specify which one:", n);
                    for s in &active {
                        eprintln!("  writ spec done {} -s \"summary\"", s.id);
                    }
                    std::process::exit(1);
                }
            }
        }
    };

    // Create final seal — use resolve_agent for auto-detect support
    let agent_id = resolve_agent(agent, cwd);
    let seal_summary = summary.as_deref().unwrap_or("Spec completed").to_string();

    let seal_agent = AgentIdentity {
        id: agent_id.to_string(),
        agent_type: if agent_id == "human" {
            AgentType::Human
        } else {
            AgentType::Agent
        },
    };
    let verification = Verification {
        tests_passed: None,
        tests_failed: None,
        linted: false,
    };
    let _seal = repo.seal(
        seal_agent,
        seal_summary,
        Some(spec_id.clone()),
        TaskStatus::Complete,
        verification,
        true, // allow empty — spec done is a metadata operation, changes may already be sealed
    )?;

    // Mark spec as done
    let spec = repo.mark_spec_done(&spec_id, summary)?;

    let seal_count = repo.spec_log(&spec_id).map(|l| l.len()).unwrap_or(0);

    println!();
    println!(
        "  {} Spec \"{}\" marked as done.",
        "✓".green().bold(),
        spec.title
    );
    if let Some(ref s) = spec.completion_summary {
        println!("  Summary: {}", s);
    }
    println!("  Seals: {}", seal_count);
    println!();
    println!(
        "  {} Run `writ status` to see all completed specs.",
        "→".dimmed()
    );
    println!(
        "  {} Run `writ finish` to commit completed work to git.",
        "→".dimmed()
    );

    Ok(())
}

fn cmd_spec_cancel(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    repo.cancel_spec(id)?;
    println!("spec '{}' cancelled", id);
    Ok(())
}

fn cmd_spec_complete(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    repo.complete_spec(id)?;
    println!("spec '{}' lifecycle completed", id);
    Ok(())
}

fn cmd_spec_reopen(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;

    // Load spec for display info before reopening.
    let spec = repo.load_spec(id)?;
    let title = spec.title.clone();
    let seal_count = repo.spec_log(id).map(|l| l.len()).unwrap_or(0);

    repo.reopen_spec(id)?;

    println!("Spec {} \"{}\" reopened.", id, title);
    println!("Status: completed → active");
    println!("Any agent can now claim and continue this spec.");
    println!();
    println!(
        "Previous work preserved in seal chain ({} seals).",
        seal_count
    );

    Ok(())
}

fn cmd_spec_claim(
    cwd: &PathBuf,
    id: &str,
    agent: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let agent_id = agent.unwrap_or("claude-code");
    repo.spec_claim(id, agent_id)?;
    println!("Claimed spec '{}' for agent '{}'.", id, agent_id);
    Ok(())
}

fn cmd_spec_assign(
    cwd: &PathBuf,
    id: &str,
    workspace: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let spec = repo.load_spec(id)?;
    let old_ws = spec.workspace.as_deref().unwrap_or("(global)");

    repo.assign_spec_to_workspace(id, workspace)?;

    println!(
        "Spec {} \"{}\" assigned to workspace '{}'.",
        id, spec.title, workspace
    );
    if old_ws != "(global)" && old_ws != workspace {
        println!("  (was: '{}')", old_ws);
    }

    Ok(())
}

fn cmd_spec_unassign(cwd: &PathBuf, id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let spec = repo.load_spec(id)?;

    if spec.workspace.is_none() {
        println!(
            "Spec {} \"{}\" is already globally visible.",
            id, spec.title
        );
        return Ok(());
    }

    let old_ws = spec.workspace.as_deref().unwrap_or("unknown");
    repo.unassign_spec_from_workspace(id)?;

    println!(
        "Spec {} \"{}\" unassigned from workspace '{}'. Now globally visible.",
        id, spec.title, old_ws
    );

    Ok(())
}

fn cmd_converge(
    cwd: &PathBuf,
    left_spec: &str,
    right_spec: &str,
    format: &str,
    apply: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let report = repo.converge(left_spec, right_spec)?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "json-compact" => {
            println!("{}", serde_json::to_string(&report)?);
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

    let repo = Repository::open_from_dir(cwd)?;

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

    let spinner = if format != "json" && format != "json-compact" {
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
            "json-compact" => println!("{}", serde_json::to_string(&report)?),
            _ => println!("No diverged branches — nothing to converge."),
        }
        return Ok(());
    }

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        "json-compact" => {
            println!("{}", serde_json::to_string(&report)?);
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
// Converge Workspaces command
// -------------------------------------------------------------------

fn cmd_converge_workspaces(
    cwd: &PathBuf,
    workspaces: &[String],
    format: &str,
    strategy: &str,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;

    if workspaces.is_empty() {
        // If no workspaces specified, auto-detect all non-main workspaces.
        let all_ws = repo.list_workspaces()?;
        let non_main: Vec<String> = all_ws
            .iter()
            .filter(|ws| !ws.is_main)
            .map(|ws| ws.name.clone())
            .collect();

        if non_main.is_empty() {
            if format == "json" {
                println!(
                    "{{\"status\":\"no_workspaces\",\"message\":\"No non-main workspaces found\"}}"
                );
            } else {
                println!("No non-main workspaces found to converge.");
            }
            return Ok(());
        }

        let report = repo.converge_workspaces(&non_main, strategy, dry_run)?;
        print_converge_workspaces_report(&report, format, dry_run);
        return Ok(());
    }

    let report = repo.converge_workspaces(workspaces, strategy, dry_run)?;
    print_converge_workspaces_report(&report, format, dry_run);
    Ok(())
}

fn print_converge_workspaces_report(
    report: &writ_core::convergence::ConvergeAllReport,
    format: &str,
    dry_run: bool,
) {
    if format == "json" {
        println!(
            "{}",
            serde_json::to_string_pretty(report).unwrap_or_default()
        );
        return;
    }

    // Human-readable output.
    use colored::Colorize;

    let prefix = if dry_run { "[dry-run] " } else { "" };

    println!(
        "{}workspace convergence: {} workspace(s) into main",
        prefix,
        report.merge_order.len()
    );
    println!(
        "  strategy: {}, clean: {}, applied: {}",
        report.strategy.cyan(),
        if report.is_clean {
            "yes".green().to_string()
        } else {
            "no".red().to_string()
        },
        report.applied
    );

    if !report.merges.is_empty() {
        println!("\n  merge steps:");
        for step in &report.merges {
            let status = if step.clean {
                "clean".green().to_string()
            } else {
                "conflicts".red().to_string()
            };
            println!(
                "    {} <- {}: {} auto-merged, {} conflicts [{}]",
                step.left_spec, step.right_spec, step.auto_merged, step.conflicts, status
            );
            if let Some(ref err) = step.error {
                println!("      error: {}", err.red());
            }
        }
    }

    if !report.files_changed.is_empty() {
        println!("\n  files changed: {}", report.files_changed.len());
        for f in &report.files_changed {
            println!("    {}", f);
        }
    }

    if !report.warnings.is_empty() {
        println!("\n  warnings:");
        for w in &report.warnings {
            println!("    {}", w.yellow());
        }
    }

    if !report.escalations.is_empty() {
        println!("\n  escalations: {}", report.escalations.len());
        for esc in &report.escalations {
            println!("    {}: {}", esc.file_path, esc.reason);
        }
    }

    if report.applied {
        println!(
            "\n  {}",
            "Changes applied to main workspace. Seal the result with:".green()
        );
        println!(
            "    writ seal -s \"converged {} workspace(s)\" --agent convergence-bot",
            report.merge_order.len()
        );
    }
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
        if all && accept.is_none() {
            return Err("--all requires --accept (e.g. --accept best)".into());
        }
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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
    repo.remote_add(name, path)?;
    println!("remote '{name}' added → {path}");
    Ok(())
}

fn cmd_remote_remove(cwd: &PathBuf, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    repo.remote_remove(name)?;
    println!("remote '{name}' removed");
    Ok(())
}

fn cmd_remote_list(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
    repo.suspend_agent(name)?;
    println!("agent '{name}' suspended");
    Ok(())
}

fn cmd_agent_reactivate(cwd: &PathBuf, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;

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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
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
    let repo = Repository::open_from_dir(cwd)?;
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

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

/// Generate .mcp.json for Claude Code MCP discovery.
/// Only writes if file doesn't exist or content differs (idempotent).
/// MS.30: Append a commented `[watch]` section to config.toml so users
/// can discover and customize watch daemon settings.
fn append_watch_config_comment(config_path: &std::path::Path) {
    let comment = r#"

# Watch daemon configuration (uncomment to customize)
# Run `writ watch` to start the convergence daemon.
# [watch]
# interval = 5              # seconds between polls (min: 1)
# auto_converge = true       # auto-converge overlapping seals
# max_retries = 3            # max convergence retries (min: 1)
# log_file = ".writ/watch.log"  # log file, relative to project root
"#;
    if let Ok(existing) = std::fs::read_to_string(config_path) {
        if !existing.contains("[watch]") {
            let _ = std::fs::write(config_path, format!("{}{}", existing, comment));
        }
    }
}

fn generate_mcp_json(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mcp_path = cwd.join(".mcp.json");
    let config = serde_json::json!({
        "mcpServers": {
            "writ": {
                "command": "writ",
                "args": ["mcp-serve"]
            }
        }
    });
    let content = format!("{}\n", serde_json::to_string_pretty(&config)?);

    if mcp_path.exists() {
        let existing = std::fs::read_to_string(&mcp_path)?;
        if existing == content {
            return Ok(());
        }
    }

    std::fs::write(&mcp_path, content)?;
    Ok(())
}

/// Remove .mcp.json if it was generated by writ.
fn remove_mcp_json(cwd: &PathBuf) -> bool {
    let mcp_path = cwd.join(".mcp.json");
    if !mcp_path.exists() {
        return false;
    }
    // Only remove if it contains our writ server entry
    if let Ok(content) = std::fs::read_to_string(&mcp_path) {
        if content.contains("\"writ\"") && content.contains("mcp-serve") {
            let _ = std::fs::remove_file(&mcp_path);
            return true;
        }
    }
    false
}

fn cmd_mcp_serve(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let writ_binary = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "writ".to_string());

    let project_dir = cwd.to_string_lossy().to_string();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(writ_mcp::run_mcp_server(writ_binary, project_dir))?;

    Ok(())
}

fn cmd_mcp_install(cwd: &PathBuf, desktop: bool) -> Result<(), Box<dyn std::error::Error>> {
    if desktop {
        let config_path = dirs_mcp_desktop_config();
        write_mcp_desktop_config(&config_path)?;
        println!(
            "{} Updated Claude Desktop config at {}",
            "✓".green(),
            config_path.display()
        );
        return Ok(());
    }

    // Default: write project-level .mcp.json
    let mcp_path = cwd.join(".mcp.json");
    let config = serde_json::json!({
        "mcpServers": {
            "writ": {
                "command": "writ",
                "args": ["mcp-serve"]
            }
        }
    });

    let content = serde_json::to_string_pretty(&config)?;
    std::fs::write(&mcp_path, format!("{}\n", content))?;

    println!(
        "{} Generated {} (MCP server for Claude Code)",
        "✓".green(),
        mcp_path.display()
    );
    println!("  Commit this file so collaborators get MCP tools automatically.");

    Ok(())
}

/// Path to Claude Desktop's config file.
fn dirs_mcp_desktop_config() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Claude")
            .join("claude_desktop_config.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux / other: XDG-style
        let config = std::env::var("XDG_CONFIG_HOME")
            .or_else(|_| std::env::var("HOME").map(|h| format!("{}/.config", h)))
            .unwrap_or_else(|_| ".config".to_string());
        PathBuf::from(config)
            .join("Claude")
            .join("claude_desktop_config.json")
    }
}

/// Merge writ server entry into Claude Desktop config (preserving existing servers).
fn write_mcp_desktop_config(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let mut config: serde_json::Value = if path.exists() {
        let existing = std::fs::read_to_string(path)?;
        serde_json::from_str(&existing)?
    } else {
        serde_json::json!({})
    };

    let servers = config
        .as_object_mut()
        .ok_or("Desktop config is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    servers
        .as_object_mut()
        .ok_or("mcpServers is not a JSON object")?
        .insert(
            "writ".to_string(),
            serde_json::json!({
                "command": "writ",
                "args": ["mcp-serve"]
            }),
        );

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(&config)?;
    std::fs::write(path, format!("{}\n", content))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

fn cmd_workspace_create(
    cwd: &PathBuf,
    name: &str,
    path: Option<&Path>,
    specs: Option<&str>,
    from: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let info = repo.create_workspace(name, path, from)?;

    println!(
        "{} Created workspace '{}' at {}",
        "✓".green(),
        info.name,
        info.path.display()
    );

    // Assign specs if --specs was provided.
    if let Some(spec_pattern) = specs {
        let all_specs = repo.list_specs()?;
        let mut assigned = 0;

        if spec_pattern.contains('*') {
            // Glob matching.
            let pattern = spec_pattern.replace('*', "");
            for spec in &all_specs {
                if spec_pattern.starts_with('*') && spec.id.ends_with(&pattern)
                    || spec_pattern.ends_with('*') && spec.id.starts_with(&pattern)
                    || spec.id.contains(&pattern)
                {
                    repo.assign_spec_to_workspace(&spec.id, name)?;
                    assigned += 1;
                }
            }
        } else {
            // Comma-separated exact match.
            for id in spec_pattern.split(',') {
                let id = id.trim();
                if !id.is_empty() {
                    match repo.assign_spec_to_workspace(id, name) {
                        Ok(()) => assigned += 1,
                        Err(e) => eprintln!("  {}: {}", "warning".yellow(), e),
                    }
                }
            }
        }

        if assigned > 0 {
            println!(
                "{} Assigned {} spec{} to workspace '{}'",
                "✓".green(),
                assigned,
                if assigned == 1 { "" } else { "s" },
                name
            );
        } else {
            eprintln!(
                "  {}: no specs matched pattern '{}'",
                "warning".yellow(),
                spec_pattern
            );
        }
    }

    Ok(())
}

fn cmd_workspace_status(
    cwd: &PathBuf,
    name: Option<&str>,
    format: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let workspaces = repo.list_workspaces()?;

    let ws_name = name.unwrap_or(repo.active_workspace());

    let ws = workspaces
        .iter()
        .find(|w| w.name == ws_name)
        .ok_or_else(|| {
            Box::<dyn std::error::Error>::from(format!("workspace '{}' not found", ws_name))
        })?;

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&ws)?);
        return Ok(());
    }

    let path_str = if ws.is_main {
        ".".to_string()
    } else {
        ws.path.display().to_string()
    };

    println!("Workspace: {}", ws.name.cyan().bold());
    println!("  Path:       {}", path_str);
    println!("  Specs:      {}", ws.spec_count);
    println!(
        "  HEAD seal:  {}",
        ws.head_seal.as_deref().unwrap_or("(none)")
    );
    if ws.is_main {
        println!("  Type:       base workspace");
    }

    // Show assigned specs.
    let all_specs = repo.list_specs()?;
    let assigned: Vec<_> = all_specs
        .iter()
        .filter(|s| {
            s.workspace.as_deref() == Some(ws_name) || (ws.is_main && s.workspace.is_none())
        })
        .collect();

    if !assigned.is_empty() {
        println!();
        println!("  Specs:");
        for spec in &assigned {
            let status = format!("{:?}", spec.status).to_lowercase();
            println!("    {} — {} [{}]", spec.id, spec.title, status);
        }
    }

    Ok(())
}

fn cmd_workspace_list(cwd: &PathBuf, format: &str) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let workspaces = repo.list_workspaces()?;

    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&workspaces)?);
        return Ok(());
    }

    if workspaces.is_empty() {
        println!("No workspaces found.");
        return Ok(());
    }

    for ws in &workspaces {
        let path_str = if ws.is_main {
            ".".to_string()
        } else {
            ws.path.display().to_string()
        };

        let suffix = if ws.is_main {
            "  base workspace".to_string()
        } else {
            String::new()
        };

        println!(
            "  {:<20} {:<25} {} specs{}",
            ws.name, path_str, ws.spec_count, suffix
        );
    }

    Ok(())
}

fn cmd_workspace_delete(
    cwd: &PathBuf,
    name: &str,
    keep_files: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;

    if !force {
        eprintln!(
            "{}",
            format!("About to delete workspace '{name}'. This removes workspace state.").yellow()
        );
        if !keep_files {
            eprintln!(
                "{}",
                "  The parallel working directory will also be removed.".yellow()
            );
        }
        eprint!("Continue? [y/N] ");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    repo.delete_workspace(name, keep_files)?;
    println!("Workspace '{}' deleted.", name);
    Ok(())
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

fn cmd_doctor(cwd: &PathBuf, json: bool, fix: bool) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open_from_dir(cwd)?;
    let report = writ_core::migrate::DoctorReport::run(repo.writ_dir());

    if fix {
        eprintln!(
            "{} --fix is reserved for a future release. Showing report only.",
            "note:".yellow()
        );
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Human-readable output
    for check in &report.checks {
        let icon = match check.status {
            writ_core::migrate::CheckStatus::Pass => "✓".green().to_string(),
            writ_core::migrate::CheckStatus::Fail => "✗".red().to_string(),
            writ_core::migrate::CheckStatus::Warning => "!".yellow().to_string(),
        };
        println!("  {} {} — {}", icon, check.name, check.message);
    }

    println!();
    let failed_str = report.failed.to_string();
    let warn_str = report.warnings.to_string();
    println!(
        "  {} passed, {} failed, {} warnings",
        report.passed.to_string().green(),
        if report.failed > 0 {
            failed_str.red().to_string()
        } else {
            failed_str
        },
        if report.warnings > 0 {
            warn_str.yellow().to_string()
        } else {
            warn_str
        },
    );

    if !report.is_healthy() {
        std::process::exit(1);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Watch command
// ---------------------------------------------------------------------------

fn cmd_watch(
    cwd: &PathBuf,
    interval: Option<u64>,
    no_auto_converge: bool,
    daemon: bool,
    stop: bool,
    status: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Verify writ is initialized.
    let _repo = Repository::open_from_dir(cwd)?;

    // Resolve watch config: CLI flags > project config > global config > defaults.
    // Uses Haris's config resolution chain (MS.18).
    let project_config = config::ProjectConfig::load(cwd).unwrap_or_default();
    let global_config = config::GlobalConfig::load().unwrap_or_default();
    let resolved = config::resolve_watch_config(&project_config, &global_config)?;

    // CLI flags override resolved config.
    let effective_interval = interval.unwrap_or(resolved.interval);
    let auto_converge = if no_auto_converge {
        false
    } else {
        resolved.auto_converge
    };

    if stop {
        // MS.17: Daemon stop — requires Lee's platform primitives (MS.19).
        return watch_ui::cmd_watch_stop(cwd);
    }

    if status {
        // MS.17: Daemon status — requires Lee's platform primitives (MS.19).
        return watch_ui::cmd_watch_status(cwd);
    }

    // Wire resolved max_retries and log_file through to the watch loop (Amis review fix).
    let effective_max_retries = resolved.max_retries;
    let effective_log_file = resolved.log_file.clone();

    if daemon {
        // MS.17: Daemon start — requires Lee's platform primitives (MS.19).
        return watch_ui::cmd_watch_daemon(
            cwd,
            effective_interval,
            auto_converge,
            effective_max_retries,
            &effective_log_file,
        );
    }

    // Foreground terminal mode (default).
    watch_ui::cmd_watch_foreground(
        cwd,
        effective_interval,
        auto_converge,
        effective_max_retries,
        &effective_log_file,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- common_directory_prefix tests --

    #[test]
    fn test_common_prefix_same_directory() {
        let paths = vec![
            "src/storage/zstd.rs".to_string(),
            "src/storage/compress.rs".to_string(),
            "src/storage/object_store.rs".to_string(),
        ];
        assert_eq!(common_directory_prefix(&paths), "src/storage/");
    }

    #[test]
    fn test_common_prefix_no_overlap() {
        let paths = vec!["src/a.rs".to_string(), "tests/b.rs".to_string()];
        assert_eq!(common_directory_prefix(&paths), "");
    }

    #[test]
    fn test_common_prefix_single_file() {
        let paths = vec!["crates/writ-py/src/lib.rs".to_string()];
        assert_eq!(common_directory_prefix(&paths), "crates/writ-py/src/");
    }

    #[test]
    fn test_common_prefix_empty_list() {
        let paths: Vec<String> = vec![];
        assert_eq!(common_directory_prefix(&paths), "");
    }

    #[test]
    fn test_common_prefix_root_files() {
        let paths = vec!["Cargo.toml".to_string(), "README.md".to_string()];
        assert_eq!(common_directory_prefix(&paths), "");
    }

    #[test]
    fn test_common_prefix_partial_directory_match() {
        // "src/convergence/" and "src/config/" share "src/" not "src/con"
        let paths = vec![
            "src/convergence/phase3.rs".to_string(),
            "src/config/settings.rs".to_string(),
        ];
        assert_eq!(common_directory_prefix(&paths), "src/");
    }

    #[test]
    fn test_common_prefix_nested_match() {
        let paths = vec![
            "crates/writ-core/src/repo.rs".to_string(),
            "crates/writ-core/src/spec.rs".to_string(),
            "crates/writ-core/src/config.rs".to_string(),
        ];
        assert_eq!(common_directory_prefix(&paths), "crates/writ-core/src/");
    }

    #[test]
    fn test_common_prefix_mixed_depth() {
        let paths = vec![
            "crates/writ-py/src/lib.rs".to_string(),
            "crates/writ-py/tests/test_api.py".to_string(),
        ];
        assert_eq!(common_directory_prefix(&paths), "crates/writ-py/");
    }

    // -- compute_spec_groups tests --

    fn make_test_spec(id: &str, files: Vec<&str>) -> writ_core::spec::Spec {
        writ_core::spec::Spec {
            id: id.to_string(),
            title: format!("Test spec {}", id),
            description: String::new(),
            status: writ_core::spec::SpecStatus::Complete,
            depends_on: vec![],
            file_scope: files.into_iter().map(|f| f.to_string()).collect(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            sealed_by: vec![],
            acceptance_criteria: vec![],
            design_notes: vec![],
            tech_stack: vec![],
            lifecycle_state: writ_core::spec::LifecycleState::Active,
            last_activity: chrono::Utc::now(),
            completion_summary: None,
            commit_state: writ_core::spec::CommitState::Uncommitted,
            completed_at: None,
            commit_hash: None,
            committed_at: None,
            workspace: None,
            claimed_by: None,
            genesis_tree: None,
        }
    }

    #[test]
    fn test_group_specs_by_directory() {
        let s1 = make_test_spec(
            "S-001",
            vec!["src/storage/zstd.rs", "src/storage/compress.rs"],
        );
        let s2 = make_test_spec("S-002", vec!["src/storage/object_store.rs"]);
        let s3 = make_test_spec("S-003", vec!["crates/writ-py/src/lib.rs"]);

        let specs: Vec<&writ_core::spec::Spec> = vec![&s1, &s2, &s3];
        let groups = compute_spec_groups(&specs);

        assert_eq!(groups.len(), 2);
        // BTreeMap sorts by key, so "crates/writ-py/src/" comes before "src/storage/"
        assert_eq!(groups[0].label, "crates/writ-py/src/");
        assert_eq!(groups[0].specs.len(), 1);
        assert_eq!(groups[1].label, "src/storage/");
        assert_eq!(groups[1].specs.len(), 2);
    }

    #[test]
    fn test_group_specs_all_same_area() {
        let s1 = make_test_spec("S-001", vec!["src/repo.rs"]);
        let s2 = make_test_spec("S-002", vec!["src/config.rs"]);

        let specs: Vec<&writ_core::spec::Spec> = vec![&s1, &s2];
        let groups = compute_spec_groups(&specs);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].label, "src/");
        assert_eq!(groups[0].specs.len(), 2);
    }

    #[test]
    fn test_group_specs_empty_file_scope_goes_to_misc() {
        let s1 = make_test_spec("S-001", vec!["src/repo.rs"]);
        let s2 = make_test_spec("S-002", vec![]);

        let specs: Vec<&writ_core::spec::Spec> = vec![&s1, &s2];
        let groups = compute_spec_groups(&specs);

        assert_eq!(groups.len(), 2);
        // "" sorts before "src/" in BTreeMap
        assert_eq!(groups[0].label, "misc");
        assert_eq!(groups[0].specs.len(), 1);
        assert_eq!(groups[0].specs[0].id, "S-002");
    }

    #[test]
    fn test_group_specs_cross_directory_spec_gets_own_group() {
        // A spec touching files across directories gets grouped by the
        // common prefix of its own files
        let s1 = make_test_spec("S-001", vec!["src/a.rs", "tests/b.rs"]);
        let s2 = make_test_spec("S-002", vec!["src/c.rs", "src/d.rs"]);

        let specs: Vec<&writ_core::spec::Spec> = vec![&s1, &s2];
        let groups = compute_spec_groups(&specs);

        assert_eq!(groups.len(), 2);
        // S-001 has no common prefix → "misc"
        assert_eq!(groups[0].label, "misc");
        assert_eq!(groups[0].specs[0].id, "S-001");
        assert_eq!(groups[1].label, "src/");
        assert_eq!(groups[1].specs[0].id, "S-002");
    }
}
