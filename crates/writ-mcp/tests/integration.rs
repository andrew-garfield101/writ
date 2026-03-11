//! MCP.7 integration tests for writ-mcp server.
//!
//! Tests the public API: server construction, info, and CLI bridge behavior.
//! Arg-building tests for individual tools live in the crate's inline test module
//! (they need access to private tool methods).

use rmcp::ServerHandler;
use std::os::unix::fs::PermissionsExt;
use writ_mcp::WritMcpServer;

// ─── Helpers ─────────────────────────────────────────────────────

fn mock_echo_binary(dir: &std::path::Path) -> String {
    let script = dir.join("mock-writ");
    std::fs::write(&script, "#!/bin/bash\necho \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_string()
}

fn mock_failing_binary(dir: &std::path::Path, msg: &str, code: i32) -> String {
    let script = dir.join("mock-writ-fail");
    let content = format!("#!/bin/bash\necho '{}' >&2\nexit {}\n", msg, code);
    std::fs::write(&script, content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_string()
}

fn mock_stderr_and_stdout_binary(dir: &std::path::Path) -> String {
    let script = dir.join("mock-writ-both");
    let content = "#!/bin/bash\necho 'stdout output'\necho 'stderr output' >&2\nexit 1\n";
    std::fs::write(&script, content).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_str().unwrap().to_string()
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    let val = serde_json::to_value(&result.content).unwrap();
    val.as_array().unwrap()[0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

// ─── Server construction ─────────────────────────────────────────

#[test]
fn test_server_info_has_correct_name() {
    let server = WritMcpServer::new("writ".to_string(), ".".to_string());
    let info = server.get_info();
    assert_eq!(info.server_info.name, "writ");
    assert_eq!(info.server_info.title.as_deref(), Some("writ MCP Server"));
}

#[test]
fn test_server_info_has_version() {
    let server = WritMcpServer::new("writ".to_string(), ".".to_string());
    let info = server.get_info();
    assert!(
        !info.server_info.version.is_empty(),
        "Server must report a version"
    );
}

#[test]
fn test_server_info_has_instructions() {
    let server = WritMcpServer::new("writ".to_string(), ".".to_string());
    let info = server.get_info();
    let instructions = info.instructions.expect("Instructions must be present");
    assert!(instructions.contains("writ_context"));
    assert!(instructions.contains("writ_seal"));
    assert!(instructions.contains("writ_spec_add"));
    assert!(instructions.contains("writ_spec_done"));
}

#[test]
fn test_server_info_has_tool_capabilities() {
    let server = WritMcpServer::new("writ".to_string(), ".".to_string());
    let info = server.get_info();
    assert!(
        info.capabilities.tools.is_some(),
        "Server must advertise tool capabilities"
    );
}

// ─── Bridge: success path ────────────────────────────────────────

#[test]
fn test_bridge_echo_returns_trimmed_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let binary = mock_echo_binary(dir.path());
    let server = WritMcpServer::new(binary, dir.path().to_str().unwrap().to_string());
    let result = server.run_writ(&["context", "--format", "toon"]).unwrap();
    assert!(!result.is_error.unwrap_or(false));
    let text = result_text(&result);
    assert_eq!(text, "context --format toon");
}

#[test]
fn test_bridge_owned_matches_borrowed() {
    let dir = tempfile::tempdir().unwrap();
    let binary = mock_echo_binary(dir.path());
    let server = WritMcpServer::new(binary, dir.path().to_str().unwrap().to_string());

    let borrowed = server.run_writ(&["log", "--all"]).unwrap();
    let owned = server
        .run_writ_owned(&["log".to_string(), "--all".to_string()])
        .unwrap();

    assert_eq!(result_text(&borrowed), result_text(&owned));
}

// ─── Bridge: error paths ─────────────────────────────────────────

#[test]
fn test_bridge_missing_binary_is_err() {
    let server = WritMcpServer::new("/nonexistent/writ".to_string(), "/tmp".to_string());
    let result = server.run_writ(&["context"]);
    assert!(result.is_err(), "Missing binary should return Err");
}

#[test]
fn test_bridge_nonzero_exit_returns_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let binary = mock_failing_binary(dir.path(), "error: bad input", 1);
    let server = WritMcpServer::new(binary, dir.path().to_str().unwrap().to_string());
    let result = server.run_writ(&["seal"]).unwrap();
    assert!(
        result.is_error.unwrap_or(false),
        "Non-zero exit should set is_error"
    );
}

#[test]
fn test_bridge_prefers_stderr_over_stdout_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let binary = mock_stderr_and_stdout_binary(dir.path());
    let server = WritMcpServer::new(binary, dir.path().to_str().unwrap().to_string());
    let result = server.run_writ(&["fail"]).unwrap();
    let text = result_text(&result);
    assert!(
        text.contains("stderr output"),
        "Should prefer stderr: got '{}'",
        text
    );
}

#[test]
fn test_bridge_uses_stdout_when_stderr_empty() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mock-writ-stdout-err");
    std::fs::write(
        &script,
        "#!/bin/bash\necho 'stdout error message'\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = WritMcpServer::new(
        script.to_str().unwrap().to_string(),
        dir.path().to_str().unwrap().to_string(),
    );
    let result = server.run_writ(&["fail"]).unwrap();
    let text = result_text(&result);
    assert!(
        text.contains("stdout error message"),
        "Should fall back to stdout when stderr empty: got '{}'",
        text
    );
}

#[test]
fn test_bridge_various_exit_codes() {
    for code in &[1, 2, 127] {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("mock-writ-exit");
        let content = format!("#!/bin/bash\necho 'exit {}' >&2\nexit {}\n", code, code);
        std::fs::write(&script, content).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let server = WritMcpServer::new(
            script.to_str().unwrap().to_string(),
            dir.path().to_str().unwrap().to_string(),
        );
        let result = server.run_writ(&["test"]).unwrap();
        assert!(
            result.is_error.unwrap_or(false),
            "Exit code {} should set is_error",
            code
        );
    }
}

// ─── Bridge: working directory ───────────────────────────────────

#[test]
fn test_bridge_runs_in_project_dir() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("mock-writ-pwd");
    std::fs::write(&script, "#!/bin/bash\npwd\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = WritMcpServer::new(
        script.to_str().unwrap().to_string(),
        dir.path().to_str().unwrap().to_string(),
    );
    let result = server.run_writ(&[]).unwrap();
    let text = result_text(&result);
    // Resolve symlinks (macOS /private/var/folders/... vs /var/folders/...)
    let expected = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let actual = std::fs::canonicalize(std::path::Path::new(&text))
        .unwrap_or_else(|_| std::path::PathBuf::from(&text))
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(actual, expected);
}
