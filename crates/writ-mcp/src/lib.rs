//! writ MCP server — exposes writ CLI operations as native MCP tools.
//!
//! This is a thin translation layer. Each tool function calls the `writ` CLI
//! via `std::process::Command` and returns the output. No business logic lives
//! here — the CLI is the source of truth.

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::process::Command;

// ─── Parameter structs ───────────────────────────────────────────

/// Parameters for writ_context tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ContextParams {
    /// Optional spec ID or slug to scope context to a specific task.
    pub spec: Option<String>,
    /// Output format: 'toon' (token-optimized, default) or 'json'.
    pub format: Option<String>,
}

/// Parameters for writ_seal tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SealParams {
    /// Clear description of what you accomplished.
    pub summary: String,
    /// The spec ID or slug this work belongs to (required).
    pub spec: String,
    /// Your agent identity (defaults to 'claude-code').
    pub agent: Option<String>,
    /// Specific file paths to seal (default: all changes).
    pub paths: Option<Vec<String>>,
    /// Allow sealing with zero file changes (metadata-only seal).
    pub allow_empty: Option<bool>,
}

/// Parameters for writ_spec_add tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpecAddParams {
    /// Short lowercase hyphenated ID (e.g. 'auth-migration').
    pub id: String,
    /// Human-readable description of the task.
    pub title: String,
    /// Optional longer description.
    pub description: Option<String>,
}

/// Parameters for writ_spec_done tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpecDoneParams {
    /// The spec ID or slug to complete (auto-detected if only one active).
    pub id: Option<String>,
    /// Optional completion summary.
    pub summary: Option<String>,
}

/// Parameters for writ_status tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {
    /// Show only completed specs.
    pub completed: Option<bool>,
    /// Show only in-progress specs.
    pub active: Option<bool>,
    /// Filter by agent name.
    pub agent: Option<String>,
    /// Detail view of a specific spec.
    pub spec: Option<String>,
}

/// Parameters for writ_diff tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DiffParams {
    /// Show changes for a specific spec only.
    pub spec: Option<String>,
    /// Show changes from a specific agent only.
    pub agent: Option<String>,
    /// Summary only (file names and line counts).
    pub stat: Option<bool>,
}

/// Parameters for writ_log tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LogParams {
    /// Filter to seals for a specific spec.
    pub spec: Option<String>,
    /// Filter to seals from a specific agent.
    pub agent: Option<String>,
    /// Show complete history (default: recent only).
    pub all: Option<bool>,
    /// Maximum number of seals to show.
    pub limit: Option<u32>,
}

/// Parameters for writ_show tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// The seal ID to inspect (from writ log).
    pub seal_id: String,
    /// Include full diff output.
    pub diff: Option<bool>,
}

/// Parameters for writ_spec_status tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpecStatusParams {
    /// Filter by state: 'active', 'completed', 'committed'.
    pub state: Option<String>,
    /// Output format: 'human' (default) or 'json'.
    pub format: Option<String>,
}

/// Parameters for writ_spec_show tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpecShowParams {
    /// The spec ID or slug to inspect.
    pub id: String,
}

/// Parameters for writ_spec_reopen tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SpecReopenParams {
    /// The spec ID or slug to reopen.
    pub id: String,
}

/// Parameters for writ_finish tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FinishParams {
    /// Comma-separated spec IDs to include (default: all completed).
    pub specs: Option<String>,
    /// Custom commit message (default: auto-generated).
    pub message: Option<String>,
    /// Commit strategy: 'single', 'per-spec', or 'grouped'.
    pub strategy: Option<String>,
    /// Preview what would be committed without doing it.
    pub dry_run: Option<bool>,
}

/// Parameters for writ_summary tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SummaryParams {
    /// Output format: 'commit' for commit msg, 'pr' for PR description.
    pub format: Option<String>,
}

/// Parameters for writ_restore tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RestoreParams {
    /// The seal ID to restore to.
    pub seal_id: String,
}

/// Parameters for writ_converge tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ConvergeParams {
    /// Strategy: 'three-way-merge' (default), 'most-recent', 'most-complete'.
    pub strategy: Option<String>,
    /// Preview convergence result without applying.
    pub dry_run: Option<bool>,
}

/// Parameters for writ_verify tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct VerifyParams {
    /// Verify the main seal chain.
    pub chain: Option<bool>,
    /// Verify all chains (including diverged branches).
    pub all_chains: Option<bool>,
    /// Verify a specific seal by ID.
    pub seal: Option<String>,
}

/// Parameters for writ_doctor tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DoctorParams {
    /// Attempt to fix issues automatically.
    pub fix: Option<bool>,
}

/// Parameters for writ_workspace_create tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceCreateParams {
    /// Workspace name (lowercase, alphanumeric, hyphens).
    pub name: String,
    /// Directory for the workspace (default: .writ/ws/<name>/).
    pub path: Option<String>,
    /// Assign matching specs (glob or comma-separated IDs).
    pub specs: Option<String>,
    /// Create from another workspace's state instead of main.
    pub from: Option<String>,
}

/// Parameters for writ_workspace_list tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceListParams {
    /// Output format: 'human' (default) or 'json'.
    pub format: Option<String>,
}

/// Parameters for writ_workspace_status tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceStatusParams {
    /// Workspace name (default: current workspace).
    pub name: Option<String>,
    /// Output format: 'human' (default) or 'json'.
    pub format: Option<String>,
}

/// Parameters for writ_workspace_delete tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceDeleteParams {
    /// Name of the workspace to delete.
    pub name: String,
    /// Keep the parallel working directory files.
    pub keep_files: Option<bool>,
}

/// Parameters for writ_task tool.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskParams {
    /// Task description (used as spec title and prompt suggestion).
    pub title: String,
    /// Override the auto-derived spec/workspace ID.
    pub id: Option<String>,
}

// ─── Server ──────────────────────────────────────────────────────

/// The writ MCP server. Exposes writ CLI operations as native MCP tools.
#[derive(Debug, Clone)]
pub struct WritMcpServer {
    tool_router: ToolRouter<Self>,
    writ_binary: String,
    project_dir: String,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WritMcpServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::V_2025_06_18,
            capabilities: rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: rmcp::model::Implementation {
                name: "writ".to_string(),
                title: Some("writ MCP Server".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: Some("AI-native version control for agentic development".to_string()),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "writ is an AI-native version control system. \
                 Start with writ_context to see project state, \
                 create specs with writ_spec_add, \
                 checkpoint work with writ_seal, \
                 and mark tasks complete with writ_spec_done."
                    .to_string(),
            ),
        }
    }
}

#[tool_router(router = tool_router)]
impl WritMcpServer {
    /// Create a new server instance.
    pub fn new(writ_binary: String, project_dir: String) -> Self {
        Self {
            tool_router: Self::tool_router(),
            writ_binary,
            project_dir,
        }
    }

    // ─── Core Workflow ───────────────────────────────────────────

    /// Get structured project state — specs, seals, files, agent activity.
    /// Run this FIRST at the start of every task to understand the project.
    #[tool(
        name = "writ_context",
        description = "Get structured project state — specs, seals, files, agent activity. Run this FIRST at the start of every task."
    )]
    async fn writ_context(
        &self,
        Parameters(params): Parameters<ContextParams>,
    ) -> Result<CallToolResult, McpError> {
        let fmt = params.format.unwrap_or_else(|| "toon".to_string());
        let mut args = vec!["context".to_string(), "--format".to_string(), fmt];
        if let Some(s) = params.spec {
            args.extend(["--spec".to_string(), s]);
        }
        self.run_writ_owned(&args)
    }

    /// Create a checkpoint (seal) of your current work.
    /// Run after each meaningful unit of progress.
    #[tool(
        name = "writ_seal",
        description = "Create a checkpoint (seal) of your current work. Run after each meaningful unit of progress. Seals are immutable snapshots."
    )]
    async fn writ_seal(
        &self,
        Parameters(params): Parameters<SealParams>,
    ) -> Result<CallToolResult, McpError> {
        let agent_id = params.agent.unwrap_or_else(|| "claude-code".to_string());
        let mut args = vec![
            "seal".to_string(),
            "-s".to_string(),
            params.summary,
            "--spec".to_string(),
            params.spec,
            "--agent".to_string(),
            agent_id,
        ];
        if let Some(p) = params.paths {
            args.extend(["--paths".to_string(), p.join(",")]);
        }
        if params.allow_empty.unwrap_or(false) {
            args.push("--allow-empty".to_string());
        }
        self.run_writ_owned(&args)
    }

    /// Create a new task spec before starting work.
    #[tool(
        name = "writ_spec_add",
        description = "Create a new task spec before starting work. Every task should have a spec for tracking and attribution. Returns the new spec plus a project state summary."
    )]
    async fn writ_spec_add(
        &self,
        Parameters(params): Parameters<SpecAddParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = params.id.clone();

        // Check for unclaimed specs before creating a new one.
        // This nudges agents to claim existing specs from `writ plan` instead
        // of creating duplicates.
        let unclaimed_warning = self.check_unclaimed_specs();

        let mut args = vec![
            "spec".to_string(),
            "add".to_string(),
            "--id".to_string(),
            params.id,
            "--title".to_string(),
            params.title,
        ];
        if let Some(d) = params.description {
            args.extend(["--description".to_string(), d]);
        }
        let add_result = self.run_writ_owned(&args)?;

        // If spec add failed, return the error as-is.
        if add_result.is_error.unwrap_or(false) {
            return Ok(add_result);
        }

        // Append project state summary so the agent has context without a
        // separate writ_context call.
        let add_text = add_result
            .content
            .first()
            .and_then(|c| {
                let v = serde_json::to_value(c).ok()?;
                v.get("text")?.as_str().map(|s| s.to_string())
            })
            .unwrap_or_default();

        let context_result = self.run_writ(&["spec", "status", "--format", "toon"])?;
        let context_text = context_result
            .content
            .first()
            .and_then(|c| {
                let v = serde_json::to_value(c).ok()?;
                v.get("text")?.as_str().map(|s| s.to_string())
            })
            .unwrap_or_default();

        let mut combined = format!(
            "{}\n\nspec created: {}\n\nproject specs:\n{}",
            add_text.trim(),
            spec_id,
            context_text.trim(),
        );

        // Append unclaimed spec warning if any exist
        if let Some(warning) = unclaimed_warning {
            combined = format!("{}\n\n{}", combined, warning);
        }

        Ok(CallToolResult::success(vec![Content::text(combined)]))
    }

    /// Mark a task spec as complete.
    #[tool(
        name = "writ_spec_done",
        description = "Mark a task spec as complete. Creates a final seal and transitions the spec to completed status."
    )]
    async fn writ_spec_done(
        &self,
        Parameters(params): Parameters<SpecDoneParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["spec".to_string(), "done".to_string()];
        if let Some(i) = params.id {
            args.push(i);
        }
        if let Some(s) = params.summary {
            args.extend(["-s".to_string(), s]);
        }
        self.run_writ_owned(&args)
    }

    // ─── Status & Review ─────────────────────────────────────────

    /// View project overview.
    #[tool(
        name = "writ_status",
        description = "View project overview — agent activity, spec progress, commit readiness."
    )]
    async fn writ_status(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["status".to_string()];
        if params.completed.unwrap_or(false) {
            args.push("--completed".to_string());
        }
        if params.active.unwrap_or(false) {
            args.push("--active".to_string());
        }
        if let Some(a) = params.agent {
            args.extend(["--agent".to_string(), a]);
        }
        if let Some(s) = params.spec {
            args.extend(["--spec".to_string(), s]);
        }
        self.run_writ_owned(&args)
    }

    /// Preview file changes.
    #[tool(
        name = "writ_diff",
        description = "Preview file changes across completed or in-progress specs."
    )]
    async fn writ_diff(
        &self,
        Parameters(params): Parameters<DiffParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["diff".to_string()];
        if let Some(s) = params.spec {
            args.extend(["--spec".to_string(), s]);
        }
        if let Some(a) = params.agent {
            args.extend(["--agent".to_string(), a]);
        }
        if params.stat.unwrap_or(false) {
            args.push("--stat".to_string());
        }
        self.run_writ_owned(&args)
    }

    /// View seal history.
    #[tool(name = "writ_log", description = "View seal history for the project.")]
    async fn writ_log(
        &self,
        Parameters(params): Parameters<LogParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["log".to_string()];
        if let Some(s) = params.spec {
            args.extend(["--spec".to_string(), s]);
        }
        if let Some(a) = params.agent {
            args.extend(["--agent".to_string(), a]);
        }
        if params.all.unwrap_or(false) {
            args.push("--all".to_string());
        }
        if let Some(l) = params.limit {
            args.extend(["--limit".to_string(), l.to_string()]);
        }
        self.run_writ_owned(&args)
    }

    /// Inspect a specific seal.
    #[tool(
        name = "writ_show",
        description = "Inspect a specific seal — metadata, changes, and optional diff."
    )]
    async fn writ_show(
        &self,
        Parameters(params): Parameters<ShowParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["show".to_string(), params.seal_id];
        if params.diff.unwrap_or(false) {
            args.push("--diff".to_string());
        }
        self.run_writ_owned(&args)
    }

    // ─── Spec Management ─────────────────────────────────────────

    /// View all specs.
    #[tool(
        name = "writ_spec_status",
        description = "View all specs and their current state."
    )]
    async fn writ_spec_status(
        &self,
        Parameters(params): Parameters<SpecStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["spec".to_string(), "status".to_string()];
        if let Some(s) = params.state {
            args.extend(["--state".to_string(), s]);
        }
        if let Some(f) = params.format {
            args.extend(["--format".to_string(), f]);
        }
        self.run_writ_owned(&args)
    }

    /// Show spec details.
    #[tool(
        name = "writ_spec_show",
        description = "Show details of a single spec — status, seals, files, acceptance criteria."
    )]
    async fn writ_spec_show(
        &self,
        Parameters(params): Parameters<SpecShowParams>,
    ) -> Result<CallToolResult, McpError> {
        self.run_writ(&["spec", "show", &params.id])
    }

    /// Reopen a completed spec.
    #[tool(
        name = "writ_spec_reopen",
        description = "Reopen a completed spec, returning it to active state. Seal history is preserved."
    )]
    async fn writ_spec_reopen(
        &self,
        Parameters(params): Parameters<SpecReopenParams>,
    ) -> Result<CallToolResult, McpError> {
        self.run_writ(&["spec", "reopen", &params.id])
    }

    // ─── Round-Trip ──────────────────────────────────────────────

    /// Promote completed work to git.
    #[tool(
        name = "writ_finish",
        description = "Promote completed work to a git commit. NOTE: In multi-agent environments, this is typically run by the user or orchestrator, not by individual agents."
    )]
    async fn writ_finish(
        &self,
        Parameters(params): Parameters<FinishParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["finish".to_string(), "--yes".to_string()];
        if let Some(s) = params.specs {
            args.extend(["--specs".to_string(), s]);
        }
        if let Some(m) = params.message {
            args.extend(["--message".to_string(), m]);
        }
        if let Some(s) = params.strategy {
            args.extend(["--strategy".to_string(), s]);
        }
        if params.dry_run.unwrap_or(false) {
            args.push("--dry-run".to_string());
        }
        self.run_writ_owned(&args)
    }

    /// Generate commit messages or PR descriptions.
    #[tool(
        name = "writ_summary",
        description = "Generate commit messages or PR descriptions from seal history."
    )]
    async fn writ_summary(
        &self,
        Parameters(params): Parameters<SummaryParams>,
    ) -> Result<CallToolResult, McpError> {
        let fmt = params.format.unwrap_or_else(|| "commit".to_string());
        self.run_writ(&["summary", "--format", &fmt])
    }

    // ─── Recovery ────────────────────────────────────────────────

    /// Restore to a previous seal.
    #[tool(
        name = "writ_restore",
        description = "Restore working directory to a previous seal's state. Every seal is an immutable snapshot. Use writ_log to find the seal ID."
    )]
    async fn writ_restore(
        &self,
        Parameters(params): Parameters<RestoreParams>,
    ) -> Result<CallToolResult, McpError> {
        self.run_writ(&["restore", &params.seal_id, "--force"])
    }

    // ─── Convergence ─────────────────────────────────────────────

    /// Run multi-branch convergence.
    #[tool(
        name = "writ_converge",
        description = "Run multi-branch convergence across divergent agent work. Use after multiple agents have completed work on overlapping files."
    )]
    async fn writ_converge(
        &self,
        Parameters(params): Parameters<ConvergeParams>,
    ) -> Result<CallToolResult, McpError> {
        let strat = params
            .strategy
            .unwrap_or_else(|| "three-way-merge".to_string());
        let mut args = vec!["converge-all".to_string(), "--strategy".to_string(), strat];
        if params.dry_run.unwrap_or(false) {
            args.push("--dry-run".to_string());
        } else {
            args.push("--apply".to_string());
        }
        self.run_writ_owned(&args)
    }

    // ─── Diagnostics ─────────────────────────────────────────────

    /// Verify seal chain integrity.
    #[tool(
        name = "writ_verify",
        description = "Verify seal chain integrity and signatures."
    )]
    async fn writ_verify(
        &self,
        Parameters(params): Parameters<VerifyParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["verify".to_string()];
        if params.chain.unwrap_or(false) {
            args.push("--chain".to_string());
        }
        if params.all_chains.unwrap_or(false) {
            args.push("--all-chains".to_string());
        }
        if let Some(s) = params.seal {
            args.extend(["--seal".to_string(), s]);
        }
        // Default to --chain if no specific flag provided.
        if args.len() == 1 {
            args.push("--chain".to_string());
        }
        self.run_writ_owned(&args)
    }

    /// Check repository health.
    #[tool(
        name = "writ_doctor",
        description = "Check repository health and schema version."
    )]
    async fn writ_doctor(
        &self,
        Parameters(params): Parameters<DoctorParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["doctor".to_string()];
        if params.fix.unwrap_or(false) {
            args.push("--fix".to_string());
        }
        self.run_writ_owned(&args)
    }

    // ─── Workspaces ─────────────────────────────────────────────

    /// Create a new isolated parallel workspace.
    #[tool(
        name = "writ_workspace_create",
        description = "Create a new isolated parallel workspace with its own index, HEAD, and working directory. Shares object store and specs with other workspaces."
    )]
    async fn writ_workspace_create(
        &self,
        Parameters(params): Parameters<WorkspaceCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["workspace".to_string(), "create".to_string(), params.name];
        if let Some(p) = params.path {
            args.extend(["--path".to_string(), p]);
        }
        if let Some(s) = params.specs {
            args.extend(["--specs".to_string(), s]);
        }
        if let Some(f) = params.from {
            args.extend(["--from".to_string(), f]);
        }
        self.run_writ_owned(&args)
    }

    /// List all workspaces.
    #[tool(
        name = "writ_workspace_list",
        description = "List all workspaces — names, paths, spec counts, and active status."
    )]
    async fn writ_workspace_list(
        &self,
        Parameters(params): Parameters<WorkspaceListParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["workspace".to_string(), "list".to_string()];
        if let Some(f) = params.format {
            args.extend(["--format".to_string(), f]);
        }
        self.run_writ_owned(&args)
    }

    /// Show workspace status and details.
    #[tool(
        name = "writ_workspace_status",
        description = "Show workspace details — assigned specs, HEAD seal, working directory path."
    )]
    async fn writ_workspace_status(
        &self,
        Parameters(params): Parameters<WorkspaceStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["workspace".to_string(), "status".to_string()];
        if let Some(n) = params.name {
            args.push(n);
        }
        if let Some(f) = params.format {
            args.extend(["--format".to_string(), f]);
        }
        self.run_writ_owned(&args)
    }

    /// Delete a workspace.
    #[tool(
        name = "writ_workspace_delete",
        description = "Delete a workspace and optionally its working directory. Cannot delete the main workspace."
    )]
    async fn writ_workspace_delete(
        &self,
        Parameters(params): Parameters<WorkspaceDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec![
            "workspace".to_string(),
            "delete".to_string(),
            params.name,
            "--force".to_string(),
        ];
        if params.keep_files.unwrap_or(false) {
            args.push("--keep-files".to_string());
        }
        self.run_writ_owned(&args)
    }

    // ─── Task (one-shot spec + workspace) ───────────────────────

    /// Create a task — a spec and isolated workspace in one command.
    #[tool(
        name = "writ_task",
        description = "Create a task: spec + workspace + gitignore in one shot. Returns spec ID, workspace path, and a suggested agent prompt."
    )]
    async fn writ_task(
        &self,
        Parameters(params): Parameters<TaskParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut args = vec!["task".to_string(), params.title];
        if let Some(id) = params.id {
            args.extend(["--id".to_string(), id]);
        }
        self.run_writ_owned(&args)
    }
}

// ─── CLI Bridge ──────────────────────────────────────────────────

impl WritMcpServer {
    /// Run a writ CLI command with borrowed string args.
    pub fn run_writ(&self, args: &[&str]) -> Result<CallToolResult, McpError> {
        let output = Command::new(&self.writ_binary)
            .args(args)
            .current_dir(&self.project_dir)
            .output()
            .map_err(|e| McpError::internal_error(format!("Failed to run writ: {}", e), None))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let msg = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            return Ok(CallToolResult::error(vec![Content::text(msg)]));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(CallToolResult::success(vec![Content::text(
            stdout.trim().to_string(),
        )]))
    }

    /// Run a writ CLI command with owned string args.
    pub fn run_writ_owned(&self, args: &[String]) -> Result<CallToolResult, McpError> {
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run_writ(&refs)
    }

    /// Check for unclaimed specs and return a warning message if any exist.
    /// Best-effort — returns None on any error.
    fn check_unclaimed_specs(&self) -> Option<String> {
        let output = Command::new(&self.writ_binary)
            .args(["spec", "status", "--format", "json"])
            .current_dir(&self.project_dir)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let specs: Vec<serde_json::Value> = serde_json::from_str(stdout.trim()).ok()?;

        let unclaimed: Vec<&str> = specs
            .iter()
            .filter(|s| {
                let status = s.get("status").and_then(|v| v.as_str()).unwrap_or("");
                let claimed = s.get("claimed_by").and_then(|v| v.as_str());
                (status == "pending" || status == "in-progress") && claimed.is_none()
            })
            .filter_map(|s| s.get("id").and_then(|v| v.as_str()))
            .collect();

        if unclaimed.is_empty() {
            return None;
        }

        Some(format!(
            "WARNING: {} unclaimed spec(s) already exist: [{}]. \
             Did you mean to use `writ spec claim <id>` instead of creating a new spec?",
            unclaimed.len(),
            unclaimed.join(", ")
        ))
    }
}

// ─── Public entry point ──────────────────────────────────────────

/// Start the writ MCP server on stdio transport.
///
/// Called by `writ mcp-serve` CLI subcommand.
pub async fn run_mcp_server(
    writ_binary: String,
    project_dir: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = WritMcpServer::new(writ_binary, project_dir);
    let service = server
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| format!("MCP server error: {}", e))?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    // ─── Helpers ─────────────────────────────────────────────────

    /// Creates a shell script that echoes all arguments to stdout.
    fn mock_echo_binary(dir: &std::path::Path) -> String {
        let script = dir.join("mock-writ");
        std::fs::write(&script, "#!/bin/bash\necho \"$@\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_str().unwrap().to_string()
    }

    /// Creates a shell script that outputs to stderr and exits non-zero.
    fn mock_failing_binary(dir: &std::path::Path, msg: &str) -> String {
        let script = dir.join("mock-writ-fail");
        let content = format!("#!/bin/bash\necho '{}' >&2\nexit 1\n", msg);
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_str().unwrap().to_string()
    }

    /// Extracts the text string from a successful CallToolResult.
    fn result_text(result: &CallToolResult) -> String {
        let val = serde_json::to_value(&result.content).unwrap();
        val.as_array().unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Creates a server with a mock echo binary in the given temp dir.
    fn echo_server(dir: &std::path::Path) -> WritMcpServer {
        let binary = mock_echo_binary(dir);
        WritMcpServer::new(binary, dir.to_str().unwrap().to_string())
    }

    // ─── Existing tests (server construction, info, tools) ──────

    #[test]
    fn test_server_construction() {
        let server = WritMcpServer::new("/usr/bin/writ".to_string(), "/tmp".to_string());
        assert_eq!(server.writ_binary, "/usr/bin/writ");
        assert_eq!(server.project_dir, "/tmp");
    }

    #[test]
    fn test_server_info() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let info = server.get_info();
        assert_eq!(info.server_info.name, "writ");
        assert!(info.instructions.is_some());
        let instructions = info.instructions.unwrap();
        assert!(
            instructions.contains("writ_context"),
            "Instructions should mention writ_context"
        );
        assert!(
            instructions.contains("writ_seal"),
            "Instructions should mention writ_seal"
        );
    }

    #[test]
    fn test_tool_count() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        assert_eq!(tools.len(), 22, "Expected 22 tools, got {}", tools.len());
    }

    #[test]
    fn test_tool_names() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

        let expected = vec![
            "writ_context",
            "writ_converge",
            "writ_diff",
            "writ_doctor",
            "writ_finish",
            "writ_log",
            "writ_restore",
            "writ_seal",
            "writ_show",
            "writ_spec_add",
            "writ_spec_done",
            "writ_spec_reopen",
            "writ_spec_show",
            "writ_spec_status",
            "writ_status",
            "writ_summary",
            "writ_verify",
            "writ_workspace_create",
            "writ_workspace_list",
            "writ_workspace_status",
            "writ_workspace_delete",
            "writ_task",
        ];

        for name in &expected {
            assert!(
                names.contains(name),
                "Missing tool: {}. Found: {:?}",
                name,
                names
            );
        }
    }

    // ─── Schema validation ──────────────────────────────────────

    #[test]
    fn test_seal_requires_summary_and_spec_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let seal_tool = tools.iter().find(|t| t.name == "writ_seal").unwrap();
        let schema = &seal_tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let required_names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            required_names.contains(&"summary"),
            "seal schema must require 'summary'"
        );
        assert!(
            required_names.contains(&"spec"),
            "seal schema must require 'spec'"
        );
    }

    #[test]
    fn test_spec_add_requires_id_and_title_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_spec_add").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"id"), "spec_add must require 'id'");
        assert!(names.contains(&"title"), "spec_add must require 'title'");
    }

    #[test]
    fn test_show_requires_seal_id_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_show").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"seal_id"), "show must require 'seal_id'");
    }

    #[test]
    fn test_restore_requires_seal_id_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_restore").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"seal_id"), "restore must require 'seal_id'");
    }

    #[test]
    fn test_spec_show_requires_id_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_spec_show").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"id"), "spec_show must require 'id'");
    }

    #[test]
    fn test_spec_reopen_requires_id_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_spec_reopen").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"id"), "spec_reopen must require 'id'");
    }

    #[test]
    fn test_all_tools_have_descriptions() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        for tool in &tools {
            assert!(
                tool.description.is_some(),
                "Tool {} must have a description",
                tool.name
            );
            let desc = tool.description.as_ref().unwrap();
            assert!(
                !desc.is_empty(),
                "Tool {} description must not be empty",
                tool.name
            );
        }
    }

    // ─── Bridge error handling ──────────────────────────────────

    #[test]
    fn test_bridge_missing_binary_returns_error() {
        let server = WritMcpServer::new("/nonexistent/writ".to_string(), "/tmp".to_string());
        let result = server.run_writ(&["context"]);
        assert!(result.is_err(), "Missing binary should return Err");
    }

    #[test]
    fn test_bridge_nonzero_exit_returns_error_content() {
        let dir = tempfile::tempdir().unwrap();
        let binary = mock_failing_binary(dir.path(), "error: something failed");
        let server = WritMcpServer::new(binary, dir.path().to_str().unwrap().to_string());
        let result = server.run_writ(&["context"]).unwrap();
        assert!(
            result.is_error.unwrap_or(false),
            "Non-zero exit should set is_error"
        );
        let text = result_text(&result);
        assert!(
            text.contains("something failed"),
            "Error text should contain stderr: got '{}'",
            text
        );
    }

    #[test]
    fn test_bridge_success_returns_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server.run_writ(&["context", "--format", "toon"]).unwrap();
        assert!(
            !result.is_error.unwrap_or(false),
            "Success should not set is_error"
        );
        let text = result_text(&result);
        assert_eq!(text, "context --format toon");
    }

    // ─── Arg building: Core Workflow ────────────────────────────

    #[tokio::test]
    async fn test_context_default_format() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_context(Parameters(ContextParams {
                spec: None,
                format: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "context --format toon");
    }

    #[tokio::test]
    async fn test_context_with_spec_and_json() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_context(Parameters(ContextParams {
                spec: Some("my-feat".to_string()),
                format: Some("json".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "context --format json --spec my-feat");
    }

    #[tokio::test]
    async fn test_seal_default_agent() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_seal(Parameters(SealParams {
                summary: "did stuff".to_string(),
                spec: "feat-1".to_string(),
                agent: None,
                paths: None,
                allow_empty: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "seal -s did stuff --spec feat-1 --agent claude-code");
    }

    #[tokio::test]
    async fn test_seal_custom_agent_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_seal(Parameters(SealParams {
                summary: "added routes".to_string(),
                spec: "api-1".to_string(),
                agent: Some("backend-dev".to_string()),
                paths: Some(vec!["src/app.py".to_string(), "src/models.py".to_string()]),
                allow_empty: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(
            text,
            "seal -s added routes --spec api-1 --agent backend-dev --paths src/app.py,src/models.py"
        );
    }

    #[tokio::test]
    async fn test_seal_allow_empty() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_seal(Parameters(SealParams {
                summary: "metadata update".to_string(),
                spec: "feat-1".to_string(),
                agent: None,
                paths: None,
                allow_empty: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(
            text,
            "seal -s metadata update --spec feat-1 --agent claude-code --allow-empty"
        );
    }

    #[tokio::test]
    async fn test_spec_add_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_add(Parameters(SpecAddParams {
                id: "auth-flow".to_string(),
                title: "Add authentication".to_string(),
                description: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(
            text.contains("spec add --id auth-flow --title Add authentication"),
            "should contain spec add command, got: {}",
            text
        );
        assert!(
            text.contains("spec created: auth-flow"),
            "should contain spec created confirmation, got: {}",
            text
        );
        assert!(
            text.contains("project specs:"),
            "should contain project specs summary, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_spec_add_with_description() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_add(Parameters(SpecAddParams {
                id: "auth-flow".to_string(),
                title: "Add auth".to_string(),
                description: Some("OAuth2 flow".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert!(
            text.contains("spec add --id auth-flow --title Add auth --description OAuth2 flow"),
            "should contain spec add command with description, got: {}",
            text
        );
        assert!(
            text.contains("spec created: auth-flow"),
            "should contain spec created confirmation, got: {}",
            text
        );
    }

    #[tokio::test]
    async fn test_spec_done_auto_detect() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_done(Parameters(SpecDoneParams {
                id: None,
                summary: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec done");
    }

    #[tokio::test]
    async fn test_spec_done_with_id_and_summary() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_done(Parameters(SpecDoneParams {
                id: Some("feat-1".to_string()),
                summary: Some("all tests pass".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec done feat-1 -s all tests pass");
    }

    // ─── Arg building: Status & Review ──────────────────────────

    #[tokio::test]
    async fn test_status_no_filters() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_status(Parameters(StatusParams {
                completed: None,
                active: None,
                agent: None,
                spec: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "status");
    }

    #[tokio::test]
    async fn test_status_with_filters() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_status(Parameters(StatusParams {
                completed: Some(true),
                active: Some(false),
                agent: Some("backend".to_string()),
                spec: Some("feat-1".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "status --completed --agent backend --spec feat-1");
    }

    #[tokio::test]
    async fn test_diff_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_diff(Parameters(DiffParams {
                spec: None,
                agent: None,
                stat: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "diff");
    }

    #[tokio::test]
    async fn test_diff_with_all_params() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_diff(Parameters(DiffParams {
                spec: Some("feat-1".to_string()),
                agent: Some("dev-1".to_string()),
                stat: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "diff --spec feat-1 --agent dev-1 --stat");
    }

    #[tokio::test]
    async fn test_log_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_log(Parameters(LogParams {
                spec: None,
                agent: None,
                all: None,
                limit: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "log");
    }

    #[tokio::test]
    async fn test_log_with_all_params() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_log(Parameters(LogParams {
                spec: Some("feat-1".to_string()),
                agent: Some("dev-1".to_string()),
                all: Some(true),
                limit: Some(5),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "log --spec feat-1 --agent dev-1 --all --limit 5");
    }

    #[tokio::test]
    async fn test_show_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_show(Parameters(ShowParams {
                seal_id: "abc123".to_string(),
                diff: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "show abc123");
    }

    #[tokio::test]
    async fn test_show_with_diff() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_show(Parameters(ShowParams {
                seal_id: "abc123".to_string(),
                diff: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "show abc123 --diff");
    }

    // ─── Arg building: Spec Management ──────────────────────────

    #[tokio::test]
    async fn test_spec_status_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_status(Parameters(SpecStatusParams {
                state: None,
                format: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec status");
    }

    #[tokio::test]
    async fn test_spec_status_with_state_and_format() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_status(Parameters(SpecStatusParams {
                state: Some("active".to_string()),
                format: Some("json".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec status --state active --format json");
    }

    #[tokio::test]
    async fn test_spec_show_args() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_show(Parameters(SpecShowParams {
                id: "my-spec".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec show my-spec");
    }

    #[tokio::test]
    async fn test_spec_reopen_args() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_spec_reopen(Parameters(SpecReopenParams {
                id: "done-spec".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "spec reopen done-spec");
    }

    // ─── Arg building: Round-Trip ───────────────────────────────

    #[tokio::test]
    async fn test_finish_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_finish(Parameters(FinishParams {
                specs: None,
                message: None,
                strategy: None,
                dry_run: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "finish --yes");
    }

    #[tokio::test]
    async fn test_finish_with_all_params() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_finish(Parameters(FinishParams {
                specs: Some("feat-1,feat-2".to_string()),
                message: Some("release v1".to_string()),
                strategy: Some("per-spec".to_string()),
                dry_run: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(
            text,
            "finish --yes --specs feat-1,feat-2 --message release v1 --strategy per-spec --dry-run"
        );
    }

    #[tokio::test]
    async fn test_summary_default_format() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_summary(Parameters(SummaryParams { format: None }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "summary --format commit");
    }

    #[tokio::test]
    async fn test_summary_pr_format() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_summary(Parameters(SummaryParams {
                format: Some("pr".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "summary --format pr");
    }

    // ─── Arg building: Recovery ─────────────────────────────────

    #[tokio::test]
    async fn test_restore_includes_force() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_restore(Parameters(RestoreParams {
                seal_id: "seal-xyz".to_string(),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "restore seal-xyz --force");
    }

    // ─── Arg building: Convergence ──────────────────────────────

    #[tokio::test]
    async fn test_converge_default_strategy_apply() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_converge(Parameters(ConvergeParams {
                strategy: None,
                dry_run: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "converge-all --strategy three-way-merge --apply");
    }

    #[tokio::test]
    async fn test_converge_dry_run() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_converge(Parameters(ConvergeParams {
                strategy: Some("most-complete".to_string()),
                dry_run: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "converge-all --strategy most-complete --dry-run");
    }

    // ─── Arg building: Diagnostics ──────────────────────────────

    #[tokio::test]
    async fn test_verify_defaults_to_chain() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_verify(Parameters(VerifyParams {
                chain: None,
                all_chains: None,
                seal: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "verify --chain");
    }

    #[tokio::test]
    async fn test_verify_all_chains() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_verify(Parameters(VerifyParams {
                chain: None,
                all_chains: Some(true),
                seal: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "verify --all-chains");
    }

    #[tokio::test]
    async fn test_verify_specific_seal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_verify(Parameters(VerifyParams {
                chain: None,
                all_chains: None,
                seal: Some("seal-abc".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "verify --seal seal-abc");
    }

    #[tokio::test]
    async fn test_doctor_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_doctor(Parameters(DoctorParams { fix: None }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "doctor");
    }

    #[tokio::test]
    async fn test_doctor_with_fix() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_doctor(Parameters(DoctorParams { fix: Some(true) }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "doctor --fix");
    }

    // ─── Arg building: Workspaces ──────────────────────────────

    #[tokio::test]
    async fn test_workspace_create_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_create(Parameters(WorkspaceCreateParams {
                name: "auth-team".to_string(),
                path: None,
                specs: None,
                from: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace create auth-team");
    }

    #[tokio::test]
    async fn test_workspace_create_with_all_params() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_create(Parameters(WorkspaceCreateParams {
                name: "payments".to_string(),
                path: Some("/tmp/ws-payments".to_string()),
                specs: Some("pay-*,billing".to_string()),
                from: Some("auth-team".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(
            text,
            "workspace create payments --path /tmp/ws-payments --specs pay-*,billing --from auth-team"
        );
    }

    #[tokio::test]
    async fn test_workspace_list_default() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_list(Parameters(WorkspaceListParams { format: None }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace list");
    }

    #[tokio::test]
    async fn test_workspace_list_json() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_list(Parameters(WorkspaceListParams {
                format: Some("json".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace list --format json");
    }

    #[tokio::test]
    async fn test_workspace_status_default() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_status(Parameters(WorkspaceStatusParams {
                name: None,
                format: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace status");
    }

    #[tokio::test]
    async fn test_workspace_status_with_name_and_format() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_status(Parameters(WorkspaceStatusParams {
                name: Some("auth-team".to_string()),
                format: Some("json".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace status auth-team --format json");
    }

    #[tokio::test]
    async fn test_workspace_delete_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_delete(Parameters(WorkspaceDeleteParams {
                name: "old-ws".to_string(),
                keep_files: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace delete old-ws --force");
    }

    #[tokio::test]
    async fn test_workspace_delete_keep_files() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_workspace_delete(Parameters(WorkspaceDeleteParams {
                name: "old-ws".to_string(),
                keep_files: Some(true),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "workspace delete old-ws --force --keep-files");
    }

    // ─── Schema validation: Workspaces ─────────────────────────

    #[test]
    fn test_workspace_create_requires_name_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "writ_workspace_create")
            .unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            names.contains(&"name"),
            "workspace_create must require 'name'"
        );
    }

    #[test]
    fn test_workspace_delete_requires_name_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == "writ_workspace_delete")
            .unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(
            names.contains(&"name"),
            "workspace_delete must require 'name'"
        );
    }

    // ─── Arg-building: Task ────────────────────────────────────

    #[tokio::test]
    async fn test_task_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_task(Parameters(TaskParams {
                title: "Add login flow".to_string(),
                id: None,
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "task Add login flow");
    }

    #[tokio::test]
    async fn test_task_with_id() {
        let dir = tempfile::tempdir().unwrap();
        let server = echo_server(dir.path());
        let result = server
            .writ_task(Parameters(TaskParams {
                title: "Add login flow".to_string(),
                id: Some("login-v2".to_string()),
            }))
            .await
            .unwrap();
        let text = result_text(&result);
        assert_eq!(text, "task Add login flow --id login-v2");
    }

    // ─── Schema validation: Task ───────────────────────────────

    #[test]
    fn test_task_requires_title_in_schema() {
        let server = WritMcpServer::new("writ".to_string(), ".".to_string());
        let tools = server.tool_router.list_all();
        let tool = tools.iter().find(|t| t.name == "writ_task").unwrap();
        let schema = &tool.input_schema;
        let required = schema.get("required").unwrap().as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"title"), "task must require 'title'");
    }
}
