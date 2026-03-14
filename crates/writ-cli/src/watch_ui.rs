//! Terminal UI and daemon management for `writ watch`.
//!
//! This module handles the presentation layer for the watch system.
//! The core watch loop lives in `writ_core::watch`.
//! This module consumes `WatchEvent`s from the core loop and renders them.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use colored::Colorize;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent},
    terminal,
};

use writ_core::platform;
use writ_core::watch::{
    run_watch_loop, WatchConfig as CoreWatchConfig, WatchEvent, WatchEventKind, WatchSummary,
};

// ---------------------------------------------------------------------------
// Foreground terminal mode (MS.10)
// ---------------------------------------------------------------------------

/// Run `writ watch` in foreground terminal mode with real-time output.
///
/// Shows seal activity and convergence events as they happen.
/// Press 'q' to quit.
pub fn cmd_watch_foreground(
    cwd: &PathBuf,
    interval: u64,
    auto_converge: bool,
    max_retries: u32,
    log_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let repo = writ_core::Repository::open_from_dir(cwd)?;

    // Build core watch config with all resolved values.
    let core_config = CoreWatchConfig {
        interval_secs: interval,
        auto_converge,
        max_retries,
        log_file: Some(log_file.to_string()),
        ..Default::default()
    };

    // Channels for event communication and shutdown signaling.
    let (event_tx, event_rx) = mpsc::channel::<WatchEvent>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();

    let start_time = Instant::now();

    // Print startup banner.
    println!();
    println!(
        "  {} {}",
        "writ watch active".green().bold(),
        format!("— monitoring for new seals ({}s interval)...", interval).dimmed()
    );
    if !auto_converge {
        println!(
            "  {}",
            "auto-convergence disabled (watch and report only)".yellow()
        );
    }
    println!("  {}", "Press 'q' to stop.".dimmed());
    println!();

    // Enable raw terminal mode for single-key input.
    terminal::enable_raw_mode()?;

    // Use scoped threads so the watch loop can borrow `repo` without moving it.
    let result = std::thread::scope(|s| {
        // Spawn the core watch loop in a background thread.
        let watch_handle = s.spawn(|| run_watch_loop(&repo, &core_config, event_tx, stop_rx));

        // Run terminal UI in the main thread (blocks until 'q' pressed).
        let ui_result = run_terminal_loop(&event_rx, interval);

        // Send stop signal to the watch loop.
        let _ = stop_tx.send(());

        // Wait for the watch loop to finish and get the summary.
        let watch_summary = match watch_handle.join() {
            Ok(Ok(summary)) => summary,
            Ok(Err(e)) => {
                eprintln!("\r  {}: {}", "watch loop error".red(), e);
                WatchSummary::default()
            }
            Err(_) => {
                eprintln!("\r  {}", "watch loop thread panicked".red());
                WatchSummary::default()
            }
        };

        (ui_result, watch_summary)
    });

    // Always restore terminal mode.
    terminal::disable_raw_mode()?;

    let (ui_result, watch_summary) = result;

    // Print session summary with duration.
    let duration = start_time.elapsed();
    print_watch_summary(&watch_summary, duration);

    // Propagate any error from the UI loop.
    ui_result
}

/// Main terminal rendering loop. Checks for keyboard input and watch events.
fn run_terminal_loop(
    event_rx: &mpsc::Receiver<WatchEvent>,
    _interval: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let poll_timeout = Duration::from_millis(250);

    loop {
        // Check for keyboard input.
        if event::poll(poll_timeout)? {
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ => {}
                }
            }
        }

        // Drain all pending watch events.
        while let Ok(watch_event) = event_rx.try_recv() {
            render_watch_event(&watch_event);
        }
    }
}

/// Render a single watch event to the terminal.
fn render_watch_event(event: &WatchEvent) {
    let ts = event
        .timestamp
        .with_timezone(&chrono::Local)
        .format("%-I:%M:%S %p");

    // Raw mode swallows \n — use \r\n for proper line breaks.
    match &event.kind {
        WatchEventKind::SealDetected {
            seal_id,
            spec,
            agent,
            files,
        } => {
            let short_id = if seal_id.len() > 8 {
                &seal_id[..8]
            } else {
                seal_id
            };
            let count = files.len();
            print!(
                "\r  {} seal {} ({}, {}): {} file{}\r\n",
                format!("[{}]", ts).dimmed(),
                short_id,
                agent,
                spec,
                count,
                if count == 1 { "" } else { "s" },
            );
        }

        WatchEventKind::OverlapDetected { files, specs } => {
            print!(
                "\r  {} {} file{} overlap detected between specs: {}\r\n",
                format!("[{}]", ts).dimmed(),
                files.len(),
                if files.len() == 1 { "" } else { "s" },
                specs.join(", ").yellow(),
            );
        }

        WatchEventKind::ConvergenceStarted { files } => {
            print!(
                "\r  {}          → auto-converging {}\r\n",
                " ".repeat(12),
                files.join(", "),
            );
            let _ = std::io::stdout().flush();
        }

        WatchEventKind::ConvergenceCompleted {
            files: _,
            merged_count,
        } => {
            print!(
                "\r  {}          → {}\r\n",
                " ".repeat(12),
                format!(
                    "convergence complete: {} file{} merged",
                    merged_count,
                    if *merged_count == 1 { "" } else { "s" }
                )
                .green(),
            );
        }

        WatchEventKind::ConvergenceFailed { files: _, reason } => {
            print!(
                "\r  {}          → {}: {}\r\n",
                " ".repeat(12),
                "convergence failed".red(),
                reason,
            );
        }

        WatchEventKind::ConflictDetected {
            file,
            agents,
            reason,
        } => {
            print!(
                "\r  {}          → {} {} ({})\r\n",
                " ".repeat(12),
                "CONFLICT".yellow().bold(),
                file,
                reason,
            );
            if !agents.is_empty() {
                print!(
                    "\r  {}            agents: {}\r\n",
                    " ".repeat(12),
                    agents.join(", "),
                );
            }
        }
    }
    let _ = std::io::stdout().flush();
}

/// Print the session summary after watch stops.
fn print_watch_summary(summary: &WatchSummary, duration: Duration) {
    println!();
    println!("  {}", "writ watch stopped.".dimmed());

    let duration_str = format_duration(duration);
    println!("    Seals detected:  {}", summary.seals_detected);
    println!("    Cycles:          {}", summary.cycles);
    if summary.convergences_succeeded > 0 || summary.convergences_triggered > 0 {
        println!(
            "    Convergences:    {} triggered, {} succeeded, {} failed",
            summary.convergences_triggered,
            summary.convergences_succeeded,
            summary.convergences_failed,
        );
    }
    if summary.overlaps_detected > 0 {
        println!("    Overlaps:        {}", summary.overlaps_detected);
    }
    if summary.conflicts_detected > 0 {
        println!(
            "    Conflicts:       {} {}",
            summary.conflicts_detected,
            "(run `writ status` to see details)".dimmed(),
        );
    }
    println!("    Duration:        {}", duration_str);
    println!();
}

/// Format a Duration as a human-readable string (e.g., "2h 15m", "45s").
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{}h {}m", hours, mins)
    }
}

// ---------------------------------------------------------------------------
// Daemon mode (MS.17) — scaffolding, depends on Lee's MS.19
// ---------------------------------------------------------------------------

/// Start watch as a background daemon process.
pub fn cmd_watch_daemon(
    cwd: &PathBuf,
    interval: u64,
    auto_converge: bool,
    _max_retries: u32,
    log_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");

    // Check for existing daemon using platform primitives.
    if let Some(status) = platform::check_daemon(&writ_dir)? {
        if status.alive {
            eprintln!(
                "{} writ watch daemon is already running (PID {})",
                "error:".red().bold(),
                status.pid
            );
            eprintln!("  Run `writ watch --stop` to stop it first.");
            std::process::exit(1);
        }
        // Stale PID — check_daemon already cleaned it up.
    }

    // Build the daemon command args to re-invoke ourselves in foreground mode.
    let exe = std::env::current_exe()?;
    let log_path = writ_dir.join(log_file);
    let mut args = vec![
        "watch".to_string(),
        "--interval".to_string(),
        interval.to_string(),
        "--log-file".to_string(),
        log_file.to_string(),
    ];
    if !auto_converge {
        args.push("--no-auto-converge".to_string());
    }

    let result = platform::daemonize(
        &exe,
        &args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &log_path,
        &writ_dir,
    )?;

    println!(
        "  {} writ watch daemon started (PID {})",
        "started.".green().bold(),
        result.pid
    );
    println!("  PID file: {}", result.pid_file.display());
    println!("  Log file: {}", log_path.display());
    println!("  Run `writ watch --stop` to stop.");

    Ok(())
}

/// Stop a running watch daemon.
pub fn cmd_watch_stop(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");

    match platform::stop_daemon(&writ_dir) {
        Ok(pid) => {
            println!(
                "  {} writ watch daemon stopped (PID {}).",
                "stopped.".green(),
                pid
            );
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("no daemon running") {
                println!("  {} no writ watch daemon is running.", "note:".dimmed());
            } else if msg.contains("not running") {
                // Stale PID — stop_daemon already cleaned it up.
                println!("  {} {}", "note:".dimmed(), msg);
            } else if msg.contains("did not stop") {
                eprintln!("{} {}", "warning:".yellow().bold(), msg);
            } else {
                eprintln!("{} {}", "error:".red().bold(), msg);
            }
        }
    }

    Ok(())
}

/// Show the status of a running watch daemon.
pub fn cmd_watch_status(cwd: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let writ_dir = cwd.join(".writ");
    let log_file = writ_dir.join("watch.log");

    match platform::check_daemon(&writ_dir)? {
        Some(status) if status.alive => {
            println!(
                "  {} writ watch daemon is running (PID {})",
                "active".green().bold(),
                status.pid
            );

            // Show recent log entries if available.
            if log_file.exists() {
                let content = std::fs::read_to_string(&log_file).unwrap_or_default();
                let lines: Vec<&str> = content.lines().collect();
                let recent: Vec<&&str> = lines.iter().rev().take(10).collect();

                if !recent.is_empty() {
                    println!();
                    println!("  {}", "Recent activity:".dimmed());
                    for line in recent.iter().rev() {
                        println!("    {}", line);
                    }
                }
            }
        }
        Some(status) => {
            // Stale — check_daemon already cleaned the PID file.
            println!(
                "  {} daemon is not running (stale PID {} cleaned up).",
                "status:".dimmed(),
                status.pid
            );
        }
        None => {
            println!("  {} no writ watch daemon is running.", "status:".dimmed());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 5s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(Duration::from_secs(8100)), "2h 15m");
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
    }

    #[test]
    fn test_watch_summary_default() {
        let summary = WatchSummary::default();
        assert_eq!(summary.seals_detected, 0);
        assert_eq!(summary.convergences_succeeded, 0);
        assert_eq!(summary.convergences_failed, 0);
        assert_eq!(summary.conflicts_detected, 0);
        assert_eq!(summary.cycles, 0);
    }

    #[test]
    fn test_render_seal_detected_event() {
        let event = WatchEvent {
            timestamp: chrono::Utc::now(),
            kind: WatchEventKind::SealDetected {
                seal_id: "s-00410b3f".to_string(),
                spec: "auth-feature".to_string(),
                agent: "agent-1".to_string(),
                files: vec![
                    "src/main.rs".to_string(),
                    "src/lib.rs".to_string(),
                    "Cargo.toml".to_string(),
                ],
            },
        };
        // Should not panic — renders to stdout.
        render_watch_event(&event);
    }

    #[test]
    fn test_render_convergence_completed_event() {
        let event = WatchEvent {
            timestamp: chrono::Utc::now(),
            kind: WatchEventKind::ConvergenceCompleted {
                files: vec!["src/auth.ts".to_string()],
                merged_count: 1,
            },
        };
        render_watch_event(&event);
    }

    #[test]
    fn test_render_conflict_detected_event() {
        let event = WatchEvent {
            timestamp: chrono::Utc::now(),
            kind: WatchEventKind::ConflictDetected {
                file: "src/auth.ts".to_string(),
                agents: vec!["agent-1".to_string(), "agent-2".to_string()],
                reason: "competing rewrites of loginWithGoogle()".to_string(),
            },
        };
        render_watch_event(&event);
    }
}
