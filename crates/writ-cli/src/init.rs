//! Interactive `writ init` flow.
//!
//! Implements the two-phase init flow from the writ-init-spec:
//! Phase 1: Global first-run setup (~/.writ/config)
//! Phase 2: Per-project setup (scan → prompts → config → files → summary)
//!
//! Non-interactive mode (`--yes`) skips all prompts and uses detected defaults.

use colored::Colorize;
use dialoguer::{Confirm, Input, Select};

use writ_core::config::{
    FrameworksConfig, GitConfig, GlobalConfig, InitDefaultValues, InitDefaults, OutputConfig,
    ProjectConfig, ProjectMeta, SecurityConfig, UserConfig, WorkflowConfig,
};
use writ_core::env_scan::{detect_user_name, EnvironmentScan};

/// Options passed from CLI flags to control init behavior.
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Accept all defaults without prompting.
    pub yes: bool,
    /// Only create .writ/ directory, no framework files.
    pub bare: bool,
    /// Skip git integration even if git repo detected.
    pub no_git: bool,
    /// Skip Claude Code integration.
    pub no_claude: bool,
    /// Skip Codex integration.
    pub no_codex: bool,
    /// Skip generic agent instructions.
    pub no_generic: bool,
    /// Explicit framework list (overrides defaults and --no-* flags).
    pub frameworks: Option<Vec<String>>,
    /// Explicit output format.
    pub format: Option<String>,
    /// Explicit project name.
    pub name: Option<String>,
    /// GC deployment profile.
    pub profile: String,
    /// Output display format: "human" or "json".
    pub output_format: String,
}

/// Result of the interactive init flow, consumed by cmd_init for execution.
#[derive(Debug, Clone)]
pub struct InitPlan {
    pub global_config: Option<GlobalConfig>,
    pub project_config: ProjectConfig,
    pub enable_git: bool,
    pub enable_claude: bool,
    pub enable_codex: bool,
    pub enable_generic: bool,
    pub project_name: String,
    pub scan: EnvironmentScan,
}

// ---------------------------------------------------------------------------
// Phase 1: Global first-run setup
// ---------------------------------------------------------------------------

/// Run global first-run setup if needed. Returns the global config.
/// Skips entirely if ~/.writ/config already exists.
pub fn maybe_global_setup(scan: &EnvironmentScan, opts: &InitOptions) -> GlobalConfig {
    // If global config exists, just load and return it
    if scan.global_config_exists {
        match GlobalConfig::load() {
            Ok(config) => return config,
            Err(e) => {
                eprintln!(
                    "{} could not load global config (~/.writ/config): {}",
                    "warning:".yellow().bold(),
                    e
                );
                eprintln!(
                    "  Using defaults. Fix the file or delete it to re-run first-time setup."
                );
                return GlobalConfig::default();
            }
        }
    }

    if opts.yes {
        // Non-interactive: use detected defaults
        let detected = detect_user_name();
        let config = GlobalConfig {
            user: Some(UserConfig {
                name: Some(detected.name),
            }),
            init: Some(InitDefaults {
                defaults: Some(InitDefaultValues {
                    frameworks: vec!["claude".into(), "codex".into(), "generic".into()],
                }),
            }),
            output: Some(OutputConfig {
                format: Some("toon".into()),
            }),
            workflow: Some(WorkflowConfig {
                commit_mode: Some("user".into()),
                commit_strategy: None,
                stale_timeout: None,
            }),
        };
        if let Err(e) = config.save() {
            eprintln!("warning: could not save global config: {}", e);
        }
        return config;
    }

    // Interactive global setup
    println!();
    println!(
        "{}",
        "Welcome to writ — AI-native version control for agentic development.".bold()
    );
    println!();
    println!("No global configuration found. Let's set that up first.");
    println!("(This only happens once. Settings apply to all projects unless overridden.)");
    println!();

    // Name detection
    let detected = detect_user_name();
    let name: String = Input::new()
        .with_prompt("Your name (for seal attribution)")
        .default(detected.name.clone())
        .interact_text()
        .unwrap_or(detected.name);

    // Output format selection
    println!();
    let format_options = &["toon", "json", "json-compact"];
    let format_descriptions = &[
        "Token-Oriented Object Notation (~40% fewer tokens)",
        "Standard JSON (maximum compatibility)",
        "Minified JSON",
    ];
    println!("Default output format for agent context:");
    for (i, (opt, desc)) in format_options.iter().zip(format_descriptions).enumerate() {
        println!("  ({}) {:<15} {}", i + 1, opt, desc);
    }

    let format_idx = Select::new()
        .with_prompt("Choose")
        .items(format_options)
        .default(0)
        .interact()
        .unwrap_or(0);
    let chosen_format = format_options[format_idx].to_string();

    println!(
        "  {} Agents will receive context in {} format by default.",
        "→".green(),
        chosen_format
    );

    // W.23: Workflow mode selection
    println!();
    let mode_options = &["user", "propose", "auto"];
    let mode_descriptions = &[
        "You run `writ finish` manually (recommended)",
        "Orchestrator proposes, you approve",
        "Fully autonomous (CI/pipelines)",
    ];
    println!("Default workflow mode (how completed work becomes git commits):");
    for (i, (opt, desc)) in mode_options.iter().zip(mode_descriptions).enumerate() {
        println!("  ({}) {:<10} {}", i + 1, opt, desc);
    }

    let mode_idx = Select::new()
        .with_prompt("Choose")
        .items(mode_options)
        .default(0)
        .interact()
        .unwrap_or(0);
    let chosen_mode = mode_options[mode_idx].to_string();

    println!("  {} Workflow: {} mode", "→".green(), chosen_mode);

    let config = GlobalConfig {
        user: Some(UserConfig { name: Some(name) }),
        init: Some(InitDefaults {
            defaults: Some(InitDefaultValues {
                frameworks: vec!["claude".into(), "codex".into(), "generic".into()],
            }),
        }),
        output: Some(OutputConfig {
            format: Some(chosen_format),
        }),
        workflow: Some(WorkflowConfig {
            commit_mode: Some(chosen_mode),
            commit_strategy: None,
            stale_timeout: None,
        }),
    };

    if let Err(e) = config.save() {
        eprintln!("warning: could not save global config: {}", e);
    } else {
        println!();
        println!(
            "Global config written to {}",
            scan.global_config_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.writ/config".into())
        );
    }

    config
}

// ---------------------------------------------------------------------------
// Phase 2: Project setup (interactive flow)
// ---------------------------------------------------------------------------

/// Run the full interactive init flow and return a plan.
///
/// # Note
/// Interactive mode (the default) requires a TTY — `dialoguer` prompts will
/// fail or behave unexpectedly when stdin is piped. Use `--yes` for
/// non-interactive / CI environments.
pub fn plan_init(opts: &InitOptions) -> Result<InitPlan, Box<dyn std::error::Error>> {
    let cwd = std::env::current_dir()?;
    let scan = EnvironmentScan::scan(&cwd);

    // Phase 1: global setup (if needed)
    let global_config = maybe_global_setup(&scan, opts);
    let new_global = if !scan.global_config_exists {
        Some(global_config.clone())
    } else {
        None
    };

    // Phase 2 begins: display scan results (I.6)
    if opts.output_format != "json" {
        display_scan_results(&scan);
    }

    // Handle already-initialized case (I.6)
    if scan.writ_already_initialized {
        if opts.yes {
            // LE-4: Log reinit notice so CI logs show what happened
            if opts.output_format != "json" {
                eprintln!(
                    "{} Reinitializing (--yes, preserving seals)",
                    "notice:".cyan().bold()
                );
            }
        } else {
            let reinit = Confirm::new()
                .with_prompt("Writ already initialized. Reinitialize? (preserves existing seals)")
                .default(false)
                .interact()
                .unwrap_or(false);
            if !reinit {
                return Err("Init cancelled — writ already initialized.".into());
            }
        }
    }

    // Git integration (I.7)
    let enable_git = resolve_git(&scan, opts);

    // Agent framework selection (I.8)
    let (enable_claude, enable_codex, enable_generic) =
        resolve_frameworks(&scan, &global_config, opts);

    // Output format (I.9)
    let output_format = resolve_output_format(&global_config, opts);

    // W.24: Workflow mode (per-project override)
    let workflow_mode = resolve_workflow_mode(&global_config, opts);

    // Project name
    let project_name = opts
        .name
        .clone()
        .unwrap_or_else(|| scan.project_name.clone());

    // Build project config
    let project_config = ProjectConfig {
        project: Some(ProjectMeta {
            name: Some(project_name.clone()),
            initialized: Some(chrono::Utc::now()),
        }),
        git: Some(GitConfig {
            enabled: enable_git,
            baseline_ref: if enable_git {
                scan.git_head_full.clone()
            } else {
                None
            },
        }),
        frameworks: Some(FrameworksConfig {
            claude: Some(enable_claude),
            codex: Some(enable_codex),
            generic: Some(enable_generic),
            extra: Default::default(),
        }),
        output: Some(OutputConfig {
            format: Some(output_format.clone()),
        }),
        security: Some(SecurityConfig {
            scope_enforcement: true,
        }),
        workflow: Some(WorkflowConfig {
            commit_mode: Some(workflow_mode),
            commit_strategy: None,
            stale_timeout: None,
        }),
        auto: None,
    };

    // Summary and confirmation (I.10)
    if !opts.yes && opts.output_format != "json" {
        display_summary(
            &scan,
            &project_config,
            enable_claude,
            enable_codex,
            enable_generic,
        );

        let proceed = Confirm::new()
            .with_prompt("Proceed?")
            .default(true)
            .interact()
            .unwrap_or(true);
        if !proceed {
            return Err("Init cancelled by user.".into());
        }
    }

    Ok(InitPlan {
        global_config: new_global,
        project_config,
        enable_git,
        enable_claude,
        enable_codex,
        enable_generic,
        project_name,
        scan,
    })
}

// ---------------------------------------------------------------------------
// I.6: Display scan results
// ---------------------------------------------------------------------------

fn display_scan_results(scan: &EnvironmentScan) {
    println!();
    println!(
        "Initializing writ in {}",
        scan.path.display().to_string().bold()
    );
    println!();
    println!("Scanning environment...");

    // Git
    if scan.git_detected {
        let display = scan.git_display().unwrap_or_default();
        println!("  {} Git repository detected ({})", "✓".green(), display);
        if scan.git_dirty {
            println!(
                "  {} {} uncommitted change(s)",
                "⚠".yellow(),
                scan.git_dirty_count
            );
        }
    } else {
        println!("  {} No git repository detected", "·".dimmed());
    }

    // Writ state
    if scan.writ_already_initialized {
        println!(
            "  {} Writ already initialized in this directory",
            "⚠".yellow()
        );
        if scan.writ_has_legacy_settings {
            println!(
                "  {} Legacy settings.json found (will migrate to config.toml)",
                "·".dimmed()
            );
        }
    }

    // Frameworks
    if scan.claude_detected() {
        let indicators: Vec<&str> = [
            if scan.claude_md_exists {
                Some("CLAUDE.md")
            } else {
                None
            },
            if scan.claude_dir_exists {
                Some(".claude/")
            } else {
                None
            },
        ]
        .iter()
        .filter_map(|x| *x)
        .collect();
        println!(
            "  {} Claude Code detected ({})",
            "✓".green(),
            indicators.join(", ")
        );
    } else {
        println!("  {} No CLAUDE.md found — will create", "·".dimmed());
    }

    if scan.codex_detected() {
        println!("  {} Codex detected", "✓".green());
    } else {
        println!("  {} No AGENTS.md found — will create", "·".dimmed());
    }

    // Project name
    println!(
        "  {} Project: {} (from {})",
        "·".dimmed(),
        scan.project_name.bold(),
        scan.project_name_source
    );
}

// ---------------------------------------------------------------------------
// I.7: Git integration prompt
// ---------------------------------------------------------------------------

fn resolve_git(scan: &EnvironmentScan, opts: &InitOptions) -> bool {
    if opts.no_git {
        return false;
    }

    if opts.yes {
        return scan.git_detected;
    }

    if scan.git_detected {
        // Git present — ask whether to use it
        println!();
        println!(
            "{}",
            "── Git Integration ─────────────────────────────────".dimmed()
        );
        let display = scan.git_display().unwrap_or_default();
        println!("Git repository found ({}).", display);

        Confirm::new()
            .with_prompt("Use git alongside writ for agentic development?")
            .default(true)
            .interact()
            .unwrap_or(true)
    } else {
        // No git — offer to initialize
        println!();
        println!(
            "{}",
            "── Git Integration ─────────────────────────────────".dimmed()
        );
        println!("No git repository detected.");
        println!("Writ works standalone but works best alongside git for pushing to remotes.");

        let options = &[
            "Initialize git here too (runs git init)",
            "Use writ without git",
        ];
        let choice = Select::new()
            .with_prompt("Choose")
            .items(options)
            .default(0)
            .interact()
            .unwrap_or(1);

        if choice == 0 {
            // Run git init
            match std::process::Command::new("git")
                .arg("init")
                .current_dir(&scan.path)
                .output()
            {
                Ok(output) if output.status.success() => {
                    println!("  {} git init complete", "✓".green());
                    true
                }
                Ok(output) => {
                    eprintln!(
                        "  {} git init failed: {}",
                        "✗".red(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                    false
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!(
                        "  {} git is not installed or not on PATH. Install git and try again.",
                        "✗".red()
                    );
                    false
                }
                Err(e) => {
                    eprintln!("  {} failed to run git: {}", "✗".red(), e);
                    false
                }
            }
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// I.8: Agent framework selection prompt
// ---------------------------------------------------------------------------

fn resolve_frameworks(
    _scan: &EnvironmentScan,
    global: &GlobalConfig,
    opts: &InitOptions,
) -> (bool, bool, bool) {
    if opts.bare {
        return (false, false, false);
    }

    // Explicit --frameworks flag overrides everything
    if let Some(ref fw_list) = opts.frameworks {
        return (
            fw_list.iter().any(|f| f == "claude"),
            fw_list.iter().any(|f| f == "codex"),
            fw_list.iter().any(|f| f == "generic"),
        );
    }

    // Apply --no-* flags
    let default_claude = !opts.no_claude;
    let default_codex = !opts.no_codex;
    let default_generic = !opts.no_generic;

    if opts.yes {
        return (default_claude, default_codex, default_generic);
    }

    // Get defaults from global config
    let global_frameworks = global
        .init
        .as_ref()
        .and_then(|i| i.defaults.as_ref())
        .map(|d| &d.frameworks);

    let mut claude = global_frameworks
        .map(|f| f.iter().any(|x| x == "claude"))
        .unwrap_or(true)
        && default_claude;
    let mut codex = global_frameworks
        .map(|f| f.iter().any(|x| x == "codex"))
        .unwrap_or(true)
        && default_codex;
    let mut generic = global_frameworks
        .map(|f| f.iter().any(|x| x == "generic"))
        .unwrap_or(true)
        && default_generic;

    println!();
    println!(
        "{}",
        "── Agent Frameworks ────────────────────────────────".dimmed()
    );
    println!("Configure writ for which agent frameworks?");

    loop {
        println!("  [{}] 1. Claude Code", if claude { "✓" } else { " " });
        println!(
            "  [{}] 2. Codex / OpenAI agents",
            if codex { "✓" } else { " " }
        );
        println!(
            "  [{}] 3. Generic / custom agents",
            if generic { "✓" } else { " " }
        );

        let input: String = Input::new()
            .with_prompt("Press enter to accept, or type number to toggle")
            .default(String::new())
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        let trimmed = input.trim();
        if trimmed.is_empty() {
            break;
        }

        for ch in trimmed.chars() {
            match ch {
                '1' => claude = !claude,
                '2' => codex = !codex,
                '3' => generic = !generic,
                _ => {}
            }
        }
    }

    (claude, codex, generic)
}

// ---------------------------------------------------------------------------
// I.9: Output format selection prompt
// ---------------------------------------------------------------------------

fn resolve_output_format(global: &GlobalConfig, opts: &InitOptions) -> String {
    // Explicit --format flag
    if let Some(ref fmt) = opts.format {
        return fmt.clone();
    }

    if opts.yes {
        // Use global config or default to toon
        return global.output_format().unwrap_or("toon").to_string();
    }

    let global_format = global.output_format();

    if let Some(fmt) = global_format {
        // Global format exists — offer to override
        println!();
        println!(
            "{}",
            "── Output Format ───────────────────────────────────".dimmed()
        );
        println!(
            "Context output format for agents: {} (from global config)",
            fmt.bold()
        );

        let input: String = Input::new()
            .with_prompt("Override for this project? [enter to keep / type format name]")
            .default(String::new())
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        let trimmed = input.trim();
        if trimmed.is_empty() {
            fmt.to_string()
        } else if writ_core::format::is_valid_format(trimmed) {
            trimmed.to_string()
        } else {
            eprintln!("Unknown format '{}', keeping '{}'", trimmed, fmt);
            fmt.to_string()
        }
    } else {
        // No global format — show full selection
        println!();
        println!(
            "{}",
            "── Output Format ───────────────────────────────────".dimmed()
        );
        println!("writ can output context in multiple formats. For LLM agents, TOON uses ~40%");
        println!("fewer tokens than JSON with identical information.");

        let options = &[
            "toon          Token-Oriented Object Notation (recommended for agents)",
            "json          Standard JSON (maximum compatibility)",
            "json-compact  Minified JSON",
        ];

        let choice = Select::new()
            .with_prompt("Choose")
            .items(options)
            .default(0)
            .interact()
            .unwrap_or(0);

        match choice {
            0 => "toon".into(),
            1 => "json".into(),
            2 => "json-compact".into(),
            _ => "toon".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// W.24: Workflow mode prompt (per-project override)
// ---------------------------------------------------------------------------

fn resolve_workflow_mode(global: &GlobalConfig, opts: &InitOptions) -> String {
    if opts.yes {
        // Use global config or default to "user".
        return global.commit_mode().unwrap_or("user").to_string();
    }

    let global_mode = global.commit_mode();

    if let Some(mode) = global_mode {
        // Global mode exists — offer to override.
        println!();
        println!(
            "{}",
            "── Workflow Mode ───────────────────────────────────".dimmed()
        );
        println!(
            "How should completed work become git commits? {} (from global config)",
            mode.bold()
        );

        let input: String = Input::new()
            .with_prompt("Override for this project? [enter to keep / type mode name]")
            .default(String::new())
            .allow_empty(true)
            .interact_text()
            .unwrap_or_default();

        let trimmed = input.trim();
        if trimmed.is_empty() {
            mode.to_string()
        } else if writ_core::config::is_valid_commit_mode(trimmed) {
            trimmed.to_string()
        } else {
            eprintln!("Unknown mode '{}', keeping '{}'", trimmed, mode);
            mode.to_string()
        }
    } else {
        // No global mode — show full selection.
        println!();
        println!(
            "{}",
            "── Workflow Mode ───────────────────────────────────".dimmed()
        );
        println!("How should completed work become git commits?");

        let options = &[
            "user      You run `writ finish` manually (recommended)",
            "propose   Orchestrator proposes, you approve",
            "auto      Fully autonomous (CI/pipelines)",
        ];

        let choice = Select::new()
            .with_prompt("Choose")
            .items(options)
            .default(0)
            .interact()
            .unwrap_or(0);

        match choice {
            0 => "user".into(),
            1 => "propose".into(),
            2 => "auto".into(),
            _ => "user".into(),
        }
    }
}

// ---------------------------------------------------------------------------
// I.10: Summary display and confirmation
// ---------------------------------------------------------------------------

fn display_summary(
    scan: &EnvironmentScan,
    config: &ProjectConfig,
    claude: bool,
    codex: bool,
    generic: bool,
) {
    println!();
    println!(
        "{}",
        "── Summary ─────────────────────────────────────────".dimmed()
    );
    println!("Will create:");
    println!("  {:<40} writ state directory", ".writ/");
    println!("  {:<40} project configuration", ".writ/config.toml");

    if generic {
        println!(
            "  {:<40} generic agent instructions",
            ".writ/AGENT_INSTRUCTIONS.md"
        );
    }

    if claude {
        println!(
            "  {:<40} Claude Code slash command",
            ".claude/commands/writ-seal.md"
        );
        println!(
            "  {:<40} Claude Code slash command",
            ".claude/commands/writ-context.md"
        );

        if scan.claude_md_exists {
            println!("  {:<40} append writ section (file exists)", "CLAUDE.md");
        } else {
            println!("  {:<40} agent workflow (new file)", "CLAUDE.md");
        }
    }

    if codex {
        if scan.agents_md_exists {
            println!("  {:<40} append writ section (file exists)", "AGENTS.md");
        } else {
            println!("  {:<40} codex workflow (new file)", "AGENTS.md");
        }
    }

    if scan.git_detected || config.git.as_ref().map(|g| g.enabled).unwrap_or(false) {
        println!("  {:<40} append .writ/ entry", ".gitignore");
    }

    let format = config.output_format().unwrap_or("json");
    println!();
    println!("Output format: {} (token-optimized)", format.bold());

    // W.25: Show workflow mode in summary.
    let mode = config.commit_mode().unwrap_or("user");
    let mode_desc = match mode {
        "user" => "run `writ finish` to promote completed work to git",
        "propose" => "orchestrator proposes, you approve",
        "auto" => "fully autonomous commits",
        _ => "",
    };
    println!("Workflow: {} mode ({})", mode.bold(), mode_desc);
    println!();
}
