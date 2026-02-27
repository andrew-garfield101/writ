//! Integration tests using the ScenarioBuilder API.
//!
//! These tests exercise full convergence workflows with real I/O:
//! init → baseline → seal → diverge → converge → assert.

mod convergence_scenarios;

use convergence_scenarios::ScenarioBuilder;

// ── Disjoint Files (No Conflict) ───────────────────────────────────

#[test]
fn test_disjoint_files_merge_cleanly() {
    let result = ScenarioBuilder::new()
        .baseline("base.py", "# base\n")
        .agent("agent-a", "spec-a")
        .writes("module_a.py", "def feature_a(): pass\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("module_b.py", "def feature_b(): pass\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("module_a.py", "feature_a");
    result.assert_file_contains("module_b.py", "feature_b");
    result.assert_not_degraded();
}

// ── Non-Overlapping Definitions (Pattern 2) ────────────────────────

#[test]
fn test_non_overlapping_definitions_composed() {
    let result = ScenarioBuilder::new()
        .baseline("models.py", "class BaseModel:\n    pass\n")
        .agent("inventory-dev", "inventory")
        .writes(
            "models.py",
            indoc(
                "from dataclasses import dataclass\n\
                 \n\
                 @dataclass\n\
                 class Product:\n\
                     name: str\n\
                     price: float\n\
                 \n\
                 @dataclass\n\
                 class Inventory:\n\
                     products: list\n",
            ),
        )
        .seal()
        .agent("auth-dev", "auth")
        .writes(
            "models.py",
            indoc(
                "from dataclasses import dataclass\n\
                 \n\
                 @dataclass\n\
                 class User:\n\
                     username: str\n\
                     email: str\n\
                 \n\
                 @dataclass\n\
                 class Session:\n\
                     user_id: str\n\
                     token: str\n",
            ),
        )
        .seal()
        .converge()
        .expect_success();

    // All four classes should be in the merged output
    result.assert_definitions_preserved("models.py", &["Product", "Inventory", "User", "Session"]);
    result.assert_not_degraded();
}

// ── EOF Append (Pattern 5) ─────────────────────────────────────────

#[test]
fn test_eof_append_both_sides() {
    let result = ScenarioBuilder::new()
        .baseline("config.py", "DEBUG = True\nPORT = 8080\n")
        .agent("agent-a", "spec-a")
        .writes(
            "config.py",
            "DEBUG = True\nPORT = 8080\nDB_HOST = 'localhost'\n",
        )
        .seal()
        .agent("agent-b", "spec-b")
        .writes("config.py", "DEBUG = True\nPORT = 8080\nCACHE_TTL = 300\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("config.py", "DB_HOST");
    result.assert_file_contains("config.py", "CACHE_TTL");
    result.assert_file_contains("config.py", "DEBUG = True");
    result.assert_not_degraded();
}

// ── Same Region Different Changes (Conflict) ──────────────────────

#[test]
fn test_same_line_conflict_escalates() {
    let result = ScenarioBuilder::new()
        .baseline("shared.py", "line1\noriginal\nline3\n")
        .agent("agent-a", "spec-a")
        .writes("shared.py", "line1\nleft_change\nline3\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("shared.py", "line1\nright_change\nline3\n")
        .seal()
        .converge()
        .expect_escalation();

    result.assert_escalated("shared.py");
}

// ── Three Agents, Mixed Files ──────────────────────────────────────

#[test]
fn test_three_agents_disjoint_files() {
    let result = ScenarioBuilder::new()
        .baseline("base.txt", "base\n")
        .agent("agent-a", "spec-a")
        .writes("a.py", "def a(): pass\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("b.py", "def b(): pass\n")
        .seal()
        .agent("agent-c", "spec-c")
        .writes("c.py", "def c(): pass\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("a.py", "def a");
    result.assert_file_contains("b.py", "def b");
    result.assert_file_contains("c.py", "def c");
    result.assert_not_degraded();
}

// ── Import Accumulation (Pattern 1) ────────────────────────────────

#[test]
fn test_import_accumulation_python() {
    let result = ScenarioBuilder::new()
        .baseline("app.py", "import os\n\ndef main(): pass\n")
        .agent("agent-a", "spec-a")
        .writes("app.py", "import os\nimport sys\n\ndef main(): pass\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("app.py", "import os\nimport json\n\ndef main(): pass\n")
        .seal()
        .converge()
        .expect_success();

    let content = result.file_content("app.py");
    assert!(
        content.contains("import sys") || content.contains("sys"),
        "Expected sys import in merged output: {content}"
    );
    assert!(
        content.contains("import json") || content.contains("json"),
        "Expected json import in merged output: {content}"
    );
    result.assert_not_degraded();
}

// ── Agent Writes New File + Modifies Existing ──────────────────────

#[test]
fn test_mixed_new_and_modified_files() {
    let result = ScenarioBuilder::new()
        .baseline("existing.py", "# original\n")
        .agent("agent-a", "spec-a")
        .writes("existing.py", "# modified by A\n")
        .writes("new_a.py", "# new file from A\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("new_b.py", "# new file from B\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("new_a.py", "new file from A");
    result.assert_file_contains("new_b.py", "new file from B");
    result.assert_not_degraded();
}

// ── Delete vs Existing (One Agent Deletes) ─────────────────────────

#[test]
fn test_one_agent_deletes_file_other_ignores() {
    let result = ScenarioBuilder::new()
        .baseline("keep.py", "keep this\n")
        .baseline("remove.py", "remove this\n")
        .agent("agent-a", "spec-a")
        .deletes("remove.py")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("new.py", "new content\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("new.py", "new content");
    result.assert_not_degraded();
}

// ── Identical Changes (Both Same) ──────────────────────────────────

#[test]
fn test_identical_changes_merge_cleanly() {
    let result = ScenarioBuilder::new()
        .baseline("shared.py", "original\n")
        .agent("agent-a", "spec-a")
        .writes("shared.py", "identical change\n")
        .seal()
        .agent("agent-b", "spec-b")
        .writes("shared.py", "identical change\n")
        .seal()
        .converge()
        .expect_success();

    result.assert_file_contains("shared.py", "identical change");
    result.assert_not_degraded();
}

// ── Helpers ────────────────────────────────────────────────────────

/// Identity function — just for readability in multi-line content.
fn indoc(s: &str) -> &str {
    s
}
