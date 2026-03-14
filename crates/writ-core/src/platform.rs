//! Cross-platform daemon process management and environment detection.
//!
//! Provides primitives for running `writ watch` as a background daemon:
//! - `daemonize()` — spawn a detached background process
//! - `send_signal()` — signal a running daemon (SIGTERM on Unix, TerminateProcess on Windows)
//! - `is_process_alive()` — check if a PID corresponds to a running process
//!
//! Also provides environment detection for the watch subsystem:
//! - `detect_watch_backend()` — determine which file-watching backend is available
//! - `detect_resource_limits()` — check system limits relevant to watching large repos

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{WritError, WritResult};

/// Signal types for cross-platform process signaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Graceful shutdown request (SIGTERM on Unix).
    Terminate,
    /// Check if process is alive (signal 0 on Unix).
    Probe,
}

/// Available file-watching backends, ordered by preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchBackend {
    /// macOS FSEvents — fast, kernel-level, low overhead.
    FSEvents,
    /// Linux inotify — fast, kernel-level, limited by max_user_watches.
    Inotify,
    /// Windows ReadDirectoryChangesW — native Win32 API.
    ReadDirectoryChanges,
    /// Polling fallback — works everywhere, slightly higher CPU.
    Polling,
}

impl std::fmt::Display for WatchBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchBackend::FSEvents => write!(f, "FSEvents (macOS native)"),
            WatchBackend::Inotify => write!(f, "inotify (Linux native)"),
            WatchBackend::ReadDirectoryChanges => {
                write!(f, "ReadDirectoryChangesW (Windows native)")
            }
            WatchBackend::Polling => write!(f, "polling (fallback)"),
        }
    }
}

/// System resource information relevant to file watching.
#[derive(Debug, Clone)]
pub struct ResourceLimits {
    /// Maximum number of open file descriptors (Unix) or handles (Windows).
    pub max_open_files: Option<u64>,
    /// Whether the limit is likely sufficient for watching a large repo.
    pub sufficient: bool,
    /// Warning message if limits are too low, None if OK.
    pub warning: Option<String>,
}

/// Status of a running daemon process.
#[derive(Debug, Clone)]
pub struct DaemonStatus {
    /// Process ID of the daemon.
    pub pid: u32,
    /// Whether the process is currently running.
    pub alive: bool,
}

/// Result of a daemonize operation.
#[derive(Debug, Clone)]
pub struct DaemonSpawnResult {
    /// PID of the spawned daemon process.
    pub pid: u32,
    /// Path to the PID file.
    pub pid_file: PathBuf,
}

// ---------------------------------------------------------------------------
// PID file management (cross-platform)
// ---------------------------------------------------------------------------

/// Default PID file path relative to the `.writ/` directory.
pub const PID_FILE_NAME: &str = "watch.pid";

/// Read the daemon PID from the PID file.
pub fn read_pid_file(writ_dir: &Path) -> WritResult<Option<u32>> {
    let pid_path = writ_dir.join(PID_FILE_NAME);
    if !pid_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&pid_path).map_err(WritError::Io)?;
    let pid: u32 = content
        .trim()
        .parse()
        .map_err(|e| WritError::Other(format!("invalid PID in {}: {}", pid_path.display(), e)))?;
    Ok(Some(pid))
}

/// Write a PID to the PID file.
pub fn write_pid_file(writ_dir: &Path, pid: u32) -> WritResult<()> {
    let pid_path = writ_dir.join(PID_FILE_NAME);
    crate::fsutil::atomic_write(&pid_path, pid.to_string().as_bytes())
}

/// Remove the PID file.
pub fn remove_pid_file(writ_dir: &Path) -> WritResult<()> {
    let pid_path = writ_dir.join(PID_FILE_NAME);
    if pid_path.exists() {
        fs::remove_file(&pid_path).map_err(WritError::Io)?;
    }
    Ok(())
}

/// Check if a daemon is currently running. Returns its status if a PID file exists.
pub fn check_daemon(writ_dir: &Path) -> WritResult<Option<DaemonStatus>> {
    match read_pid_file(writ_dir)? {
        Some(pid) => {
            let alive = is_process_alive(pid);
            if !alive {
                // Stale PID file — process died without cleanup.
                remove_pid_file(writ_dir)?;
            }
            Ok(Some(DaemonStatus { pid, alive }))
        }
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Platform-specific: Unix (macOS + Linux)
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// Spawn a detached daemon process running the given executable with args.
    ///
    /// The daemon:
    /// - Runs in a new session (setsid)
    /// - Redirects stdout/stderr to the log file
    /// - Closes stdin
    /// - Is detached from the parent terminal
    pub fn daemonize(
        executable: &Path,
        args: &[&str],
        log_file: &Path,
        writ_dir: &Path,
    ) -> WritResult<DaemonSpawnResult> {
        // Ensure log file parent directory exists.
        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent).map_err(WritError::Io)?;
        }

        // Open log file for stdout/stderr redirection.
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .map_err(|e| {
                WritError::Other(format!(
                    "cannot open log file {}: {}",
                    log_file.display(),
                    e
                ))
            })?;
        let log_err = log.try_clone().map_err(WritError::Io)?;

        let child = unsafe {
            Command::new(executable)
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(log)
                .stderr(log_err)
                .pre_exec(|| {
                    // Create a new session so the daemon is detached from the terminal.
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
        }
        .map_err(|e| WritError::Other(format!("failed to spawn daemon process: {}", e)))?;

        let pid = child.id();
        let pid_file = writ_dir.join(PID_FILE_NAME);
        write_pid_file(writ_dir, pid)?;

        Ok(DaemonSpawnResult { pid, pid_file })
    }

    /// Send a signal to a process.
    pub fn send_signal(pid: u32, signal: Signal) -> WritResult<()> {
        let sig = match signal {
            Signal::Terminate => libc::SIGTERM,
            Signal::Probe => 0,
        };
        let ret = unsafe { libc::kill(pid as libc::pid_t, sig) };
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            return Err(WritError::Other(format!(
                "failed to send signal to PID {}: {}",
                pid, err
            )));
        }
        Ok(())
    }

    /// Check if a process with the given PID is alive.
    pub fn is_process_alive(pid: u32) -> bool {
        // kill(pid, 0) checks existence without sending a signal.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Detect system resource limits on Unix.
    pub fn detect_resource_limits() -> ResourceLimits {
        let mut limits = ResourceLimits {
            max_open_files: None,
            sufficient: true,
            warning: None,
        };

        let mut rlim: libc::rlimit = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
        if ret == 0 {
            let soft = rlim.rlim_cur;
            limits.max_open_files = Some(soft);

            // A reasonable threshold: watching large repos may need many open files.
            // 256 is a common low default; 1024+ is generally safe.
            if soft < 512 {
                limits.sufficient = false;
                limits.warning = Some(format!(
                    "open file limit is {} (low for large repos). \
                     Consider raising with `ulimit -n 4096`.",
                    soft
                ));
            }
        }

        limits
    }
}

#[cfg(unix)]
pub use unix_impl::{
    daemonize, detect_resource_limits as detect_resource_limits_impl, is_process_alive, send_signal,
};

// ---------------------------------------------------------------------------
// Platform-specific: Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::process::Command;

    /// Spawn a detached daemon process on Windows.
    pub fn daemonize(
        executable: &Path,
        args: &[&str],
        log_file: &Path,
        writ_dir: &Path,
    ) -> WritResult<DaemonSpawnResult> {
        use std::os::windows::process::CommandExt;

        // Ensure log file parent directory exists.
        if let Some(parent) = log_file.parent() {
            fs::create_dir_all(parent).map_err(WritError::Io)?;
        }

        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .map_err(|e| {
                WritError::Other(format!(
                    "cannot open log file {}: {}",
                    log_file.display(),
                    e
                ))
            })?;
        let log_err = log.try_clone().map_err(WritError::Io)?;

        // DETACHED_PROCESS (0x00000008) | CREATE_NO_WINDOW (0x08000000)
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let child = Command::new(executable)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(log_err)
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| WritError::Other(format!("failed to spawn daemon process: {}", e)))?;

        let pid = child.id();
        let pid_file = writ_dir.join(PID_FILE_NAME);
        write_pid_file(writ_dir, pid)?;

        Ok(DaemonSpawnResult { pid, pid_file })
    }

    /// Send a signal to a process on Windows.
    /// Windows does not have Unix-style signals. Terminate uses TerminateProcess.
    pub fn send_signal(pid: u32, signal: Signal) -> WritResult<()> {
        match signal {
            Signal::Probe => {
                // Check if alive — delegate to is_process_alive.
                if !is_process_alive(pid) {
                    return Err(WritError::Other(format!("process {} is not running", pid)));
                }
                Ok(())
            }
            Signal::Terminate => {
                // Use taskkill as a portable approach.
                let output = std::process::Command::new("taskkill")
                    .args(&["/PID", &pid.to_string(), "/F"])
                    .output()
                    .map_err(|e| {
                        WritError::Other(format!("failed to terminate PID {}: {}", pid, e))
                    })?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(WritError::Other(format!(
                        "failed to terminate PID {}: {}",
                        pid,
                        stderr.trim()
                    )));
                }
                Ok(())
            }
        }
    }

    /// Check if a process with the given PID is alive on Windows.
    pub fn is_process_alive(pid: u32) -> bool {
        // Use tasklist to check if the PID exists.
        let output = std::process::Command::new("tasklist")
            .args(&["/FI", &format!("PID eq {}", pid), "/NH"])
            .output();
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }

    /// Detect system resource limits on Windows.
    pub fn detect_resource_limits() -> ResourceLimits {
        // Windows handle limits are typically very high (16M+).
        ResourceLimits {
            max_open_files: None, // Not easily queryable on Windows.
            sufficient: true,
            warning: None,
        }
    }
}

#[cfg(windows)]
pub use windows_impl::{
    daemonize, detect_resource_limits as detect_resource_limits_impl, is_process_alive, send_signal,
};

// ---------------------------------------------------------------------------
// Cross-platform API
// ---------------------------------------------------------------------------

/// Detect the best available file-watching backend for this platform.
pub fn detect_watch_backend() -> WatchBackend {
    #[cfg(target_os = "macos")]
    {
        WatchBackend::FSEvents
    }
    #[cfg(target_os = "linux")]
    {
        // Check if inotify is available by probing /proc/sys/fs/inotify.
        if Path::new("/proc/sys/fs/inotify/max_user_watches").exists() {
            WatchBackend::Inotify
        } else {
            WatchBackend::Polling
        }
    }
    #[cfg(target_os = "windows")]
    {
        WatchBackend::ReadDirectoryChanges
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        WatchBackend::Polling
    }
}

/// Detect system resource limits for the watch subsystem.
pub fn detect_resource_limits() -> ResourceLimits {
    detect_resource_limits_impl()
}

/// Full environment report for the watch subsystem.
#[derive(Debug, Clone)]
pub struct WatchEnvironment {
    /// Best available file-watching backend.
    pub backend: WatchBackend,
    /// System resource limits.
    pub resources: ResourceLimits,
    /// Platform identifier.
    pub platform: &'static str,
    /// Any warnings about the environment.
    pub warnings: Vec<String>,
}

/// Detect the full watch environment and produce warnings for any issues.
pub fn detect_watch_environment() -> WatchEnvironment {
    let backend = detect_watch_backend();
    let resources = detect_resource_limits();
    let mut warnings = Vec::new();

    if let Some(ref w) = resources.warning {
        warnings.push(w.clone());
    }

    if backend == WatchBackend::Polling {
        warnings.push(
            "using polling backend (no native filesystem events). \
             Watch loop will use slightly more CPU."
                .to_string(),
        );
    }

    let platform = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    WatchEnvironment {
        backend,
        resources,
        platform,
        warnings,
    }
}

/// Stop a running daemon process gracefully.
///
/// 1. Read PID from `.writ/watch.pid`
/// 2. Send SIGTERM (Unix) or TerminateProcess (Windows)
/// 3. Wait briefly for graceful shutdown
/// 4. Remove PID file
pub fn stop_daemon(writ_dir: &Path) -> WritResult<u32> {
    let pid = read_pid_file(writ_dir)?
        .ok_or_else(|| WritError::Other("no daemon running (no PID file found)".to_string()))?;

    if !is_process_alive(pid) {
        // Stale PID file — just clean up.
        remove_pid_file(writ_dir)?;
        return Err(WritError::Other(format!(
            "daemon (PID {}) is not running (stale PID file removed)",
            pid
        )));
    }

    send_signal(pid, Signal::Terminate)?;

    // Wait up to 5 seconds for graceful shutdown.
    for _ in 0..50 {
        if !is_process_alive(pid) {
            remove_pid_file(writ_dir)?;
            return Ok(pid);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Process didn't stop — remove PID file anyway but warn.
    remove_pid_file(writ_dir)?;
    Err(WritError::Other(format!(
        "daemon (PID {}) did not stop within 5 seconds. \
         PID file removed; process may still be running.",
        pid
    )))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_watch_backend_returns_valid_backend() {
        let backend = detect_watch_backend();
        // On macOS (our primary dev platform), expect FSEvents.
        #[cfg(target_os = "macos")]
        assert_eq!(backend, WatchBackend::FSEvents);
        #[cfg(target_os = "linux")]
        assert!(backend == WatchBackend::Inotify || backend == WatchBackend::Polling);
        // Just ensure it returns something valid on any platform.
        let display = format!("{}", backend);
        assert!(!display.is_empty());
    }

    #[test]
    fn test_detect_resource_limits() {
        let limits = detect_resource_limits();
        // On Unix, we should get a value.
        #[cfg(unix)]
        assert!(limits.max_open_files.is_some());
    }

    #[test]
    fn test_detect_watch_environment() {
        let env = detect_watch_environment();
        assert!(!env.platform.is_empty());
        // Backend should be valid.
        let _ = format!("{}", env.backend);
    }

    #[test]
    fn test_pid_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let writ_dir = dir.path();

        // No PID file initially.
        assert!(read_pid_file(writ_dir).unwrap().is_none());

        // Write a PID.
        write_pid_file(writ_dir, 12345).unwrap();
        assert_eq!(read_pid_file(writ_dir).unwrap(), Some(12345));

        // Remove it.
        remove_pid_file(writ_dir).unwrap();
        assert!(read_pid_file(writ_dir).unwrap().is_none());
    }

    #[test]
    fn test_remove_nonexistent_pid_file_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        // Should not error.
        remove_pid_file(dir.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_is_process_alive_current_process() {
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[cfg(unix)]
    #[test]
    fn test_is_process_alive_nonexistent() {
        // PID 99999999 is almost certainly not running.
        assert!(!is_process_alive(99_999_999));
    }

    #[cfg(unix)]
    #[test]
    fn test_send_signal_probe_current_process() {
        let pid = std::process::id();
        // Probing our own process should succeed.
        assert!(send_signal(pid, Signal::Probe).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn test_send_signal_probe_nonexistent() {
        assert!(send_signal(99_999_999, Signal::Probe).is_err());
    }

    #[test]
    fn test_check_daemon_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_daemon(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_check_daemon_stale_pid() {
        let dir = tempfile::tempdir().unwrap();
        // Write a PID that doesn't exist.
        write_pid_file(dir.path(), 99_999_999).unwrap();
        let status = check_daemon(dir.path()).unwrap().unwrap();
        assert!(!status.alive);
        // PID file should be cleaned up.
        assert!(read_pid_file(dir.path()).unwrap().is_none());
    }

    #[test]
    fn test_stop_daemon_no_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let result = stop_daemon(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_watch_backend_display() {
        assert_eq!(
            format!("{}", WatchBackend::FSEvents),
            "FSEvents (macOS native)"
        );
        assert_eq!(
            format!("{}", WatchBackend::Inotify),
            "inotify (Linux native)"
        );
        assert_eq!(format!("{}", WatchBackend::Polling), "polling (fallback)");
    }
}
