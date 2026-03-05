//! Output formatting — pluggable format system for CLI and SDK output.
//!
//! Defines the `OutputFormatter` trait and provides built-in formatters:
//! - `JsonFormatter` — pretty-printed JSON (default)
//! - `JsonCompactFormatter` — minified JSON, no whitespace
//! - `ToonFormatter` — Token-Oriented Object Notation (~40% token savings)
//!
//! Format resolution chain: CLI flag > project config > global config > default.

use crate::context::ContextOutput;
use crate::error::{WritError, WritResult};
use crate::seal::Seal;
use crate::spec::Spec;

/// Trait for output formatters. Each formatter converts writ data structures
/// into a string representation suitable for CLI output or SDK consumption.
pub trait OutputFormatter: Send + Sync {
    /// Human-readable format name (e.g., "json", "json-compact", "toon").
    fn name(&self) -> &str;

    /// Format a full context output.
    fn format_context(&self, context: &ContextOutput) -> WritResult<String>;

    /// Format a list of seals (e.g., `writ log` output).
    fn format_seal_log(&self, seals: &[Seal]) -> WritResult<String>;

    /// Format a list of specs (e.g., `writ spec list` output).
    fn format_spec_list(&self, specs: &[Spec]) -> WritResult<String>;
}

/// All known format names.
pub const FORMAT_NAMES: &[&str] = &["json", "json-compact", "toon"];

/// Check if a format name is valid.
pub fn is_valid_format(name: &str) -> bool {
    FORMAT_NAMES.contains(&name)
}

// ---------------------------------------------------------------------------
// JSON Formatter (pretty)
// ---------------------------------------------------------------------------

/// Pretty-printed JSON output. This is the default format for structured
/// commands like `writ context`, `writ log --format json`, etc.
pub struct JsonFormatter;

impl OutputFormatter for JsonFormatter {
    fn name(&self) -> &str {
        "json"
    }

    fn format_context(&self, context: &ContextOutput) -> WritResult<String> {
        Ok(serde_json::to_string_pretty(context)?)
    }

    fn format_seal_log(&self, seals: &[Seal]) -> WritResult<String> {
        Ok(serde_json::to_string_pretty(seals)?)
    }

    fn format_spec_list(&self, specs: &[Spec]) -> WritResult<String> {
        Ok(serde_json::to_string_pretty(specs)?)
    }
}

// ---------------------------------------------------------------------------
// JSON Compact Formatter
// ---------------------------------------------------------------------------

/// Minified JSON with no whitespace. Useful for piping to other tools
/// or when bandwidth/tokens matter but you need JSON compatibility.
pub struct JsonCompactFormatter;

impl OutputFormatter for JsonCompactFormatter {
    fn name(&self) -> &str {
        "json-compact"
    }

    fn format_context(&self, context: &ContextOutput) -> WritResult<String> {
        Ok(serde_json::to_string(context)?)
    }

    fn format_seal_log(&self, seals: &[Seal]) -> WritResult<String> {
        Ok(serde_json::to_string(seals)?)
    }

    fn format_spec_list(&self, specs: &[Spec]) -> WritResult<String> {
        Ok(serde_json::to_string(specs)?)
    }
}

// ---------------------------------------------------------------------------
// TOON Formatter
// ---------------------------------------------------------------------------

/// Token-Oriented Object Notation formatter. Uses tabular header format
/// for arrays of uniform structs, achieving ~40% token savings vs JSON.
///
/// Output includes a header comment with project name, format, and timestamp.
pub struct ToonFormatter {
    /// Optional project name for the header comment.
    pub project_name: Option<String>,
}

impl ToonFormatter {
    /// Create a new ToonFormatter with no project name.
    pub fn new() -> Self {
        Self { project_name: None }
    }

    /// Create a ToonFormatter with a project name for header comments.
    pub fn with_project(name: &str) -> Self {
        Self {
            project_name: Some(name.to_string()),
        }
    }

    /// Build the TOON header comment line.
    fn header_comment(&self, data_type: &str) -> String {
        let project = self.project_name.as_deref().unwrap_or("unknown");
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        format!(
            "# writ {} | project: {} | format: toon | timestamp: {}",
            data_type, project, timestamp
        )
    }

    /// Encode a JSON value to TOON, prepending the header comment.
    fn encode_with_header(&self, value: &serde_json::Value, data_type: &str) -> WritResult<String> {
        let opts = toon_format::EncodeOptions::default();
        let toon = toon_format::encode(value, &opts)
            .map_err(|e| WritError::Other(format!("TOON encoding failed: {e}")))?;
        Ok(format!("{}\n{}", self.header_comment(data_type), toon))
    }
}

impl Default for ToonFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputFormatter for ToonFormatter {
    fn name(&self) -> &str {
        "toon"
    }

    fn format_context(&self, context: &ContextOutput) -> WritResult<String> {
        let val = serde_json::to_value(context)?;
        self.encode_with_header(&val, "context")
    }

    fn format_seal_log(&self, seals: &[Seal]) -> WritResult<String> {
        let val = serde_json::to_value(seals)?;
        self.encode_with_header(&val, "seal-log")
    }

    fn format_spec_list(&self, specs: &[Spec]) -> WritResult<String> {
        let val = serde_json::to_value(specs)?;
        self.encode_with_header(&val, "spec-list")
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a formatter by name. Returns `None` for unknown format names.
pub fn formatter_for(name: &str) -> Option<Box<dyn OutputFormatter>> {
    formatter_for_project(name, None)
}

/// Create a formatter by name with an optional project name for TOON headers.
pub fn formatter_for_project(
    name: &str,
    project_name: Option<&str>,
) -> Option<Box<dyn OutputFormatter>> {
    match name {
        "json" => Some(Box::new(JsonFormatter)),
        "json-compact" => Some(Box::new(JsonCompactFormatter)),
        "toon" => {
            let formatter = match project_name {
                Some(p) => ToonFormatter::with_project(p),
                None => ToonFormatter::new(),
            };
            Some(Box::new(formatter))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_names_constant() {
        assert!(FORMAT_NAMES.contains(&"json"));
        assert!(FORMAT_NAMES.contains(&"json-compact"));
        assert!(FORMAT_NAMES.contains(&"toon"));
    }

    #[test]
    fn test_is_valid_format() {
        assert!(is_valid_format("json"));
        assert!(is_valid_format("json-compact"));
        assert!(is_valid_format("toon"));
        assert!(!is_valid_format("xml"));
        assert!(!is_valid_format("human"));
    }

    #[test]
    fn test_json_formatter_name() {
        let f = JsonFormatter;
        assert_eq!(f.name(), "json");
    }

    #[test]
    fn test_json_compact_formatter_name() {
        let f = JsonCompactFormatter;
        assert_eq!(f.name(), "json-compact");
    }

    #[test]
    fn test_formatter_for_known() {
        assert!(formatter_for("json").is_some());
        assert!(formatter_for("json-compact").is_some());
    }

    #[test]
    fn test_formatter_for_toon() {
        assert!(formatter_for("toon").is_some());
    }

    #[test]
    fn test_formatter_for_unknown() {
        assert!(formatter_for("xml").is_none());
        assert!(formatter_for("human").is_none());
    }

    #[test]
    fn test_json_format_seal_log_empty() {
        let f = JsonFormatter;
        let result = f.format_seal_log(&[]).unwrap();
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_json_compact_format_seal_log_empty() {
        let f = JsonCompactFormatter;
        let result = f.format_seal_log(&[]).unwrap();
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_json_format_spec_list_empty() {
        let f = JsonFormatter;
        let result = f.format_spec_list(&[]).unwrap();
        assert_eq!(result, "[]");
    }

    // -- TOON formatter tests --

    #[test]
    fn test_toon_formatter_name() {
        let f = ToonFormatter::new();
        assert_eq!(f.name(), "toon");
    }

    #[test]
    fn test_toon_formatter_with_project() {
        let f = ToonFormatter::with_project("my-app");
        assert_eq!(f.project_name.as_deref(), Some("my-app"));
    }

    #[test]
    fn test_toon_header_comment() {
        let f = ToonFormatter::with_project("test-project");
        let header = f.header_comment("context");
        assert!(header
            .starts_with("# writ context | project: test-project | format: toon | timestamp:"));
    }

    #[test]
    fn test_toon_format_seal_log_empty() {
        let f = ToonFormatter::new();
        let result = f.format_seal_log(&[]).unwrap();
        assert!(result.contains("# writ seal-log"));
        assert!(result.contains("format: toon"));
    }

    #[test]
    fn test_toon_format_spec_list_empty() {
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&[]).unwrap();
        assert!(result.contains("# writ spec-list"));
    }

    fn make_test_spec(id: &str, title: &str, desc: &str) -> Spec {
        Spec::new(id.into(), title.into(), desc.into())
    }

    #[test]
    fn test_toon_encodes_specs() {
        let specs = vec![
            make_test_spec("auth", "Authentication", "JWT auth system"),
            make_test_spec("ui", "User Interface", "React frontend"),
        ];
        let f = ToonFormatter::with_project("test");
        let result = f.format_spec_list(&specs).unwrap();
        // Should have header comment
        assert!(result.starts_with("# writ spec-list"));
        // Should contain spec data
        assert!(result.contains("auth"));
        assert!(result.contains("Authentication"));
        assert!(result.contains("ui"));
    }

    #[test]
    fn test_toon_output_is_smaller_than_json() {
        let specs: Vec<Spec> = (0..10)
            .map(|i| {
                make_test_spec(
                    &format!("spec-{i}"),
                    &format!("Spec Number {i}"),
                    &format!("Description for spec {i}"),
                )
            })
            .collect();

        let json_f = JsonFormatter;
        let toon_f = ToonFormatter::new();

        let json_out = json_f.format_spec_list(&specs).unwrap();
        let toon_out = toon_f.format_spec_list(&specs).unwrap();

        // TOON should be meaningfully smaller than pretty JSON
        assert!(
            toon_out.len() < json_out.len(),
            "TOON ({} bytes) should be smaller than JSON ({} bytes)",
            toon_out.len(),
            json_out.len()
        );
    }

    #[test]
    fn test_formatter_for_project_passes_name_to_toon() {
        let f = formatter_for_project("toon", Some("my-app")).unwrap();
        let specs = vec![make_test_spec("s1", "Title", "Desc")];
        let output = f.format_spec_list(&specs).unwrap();
        assert!(
            output.contains("project: my-app"),
            "TOON header should contain project name, got: {}",
            output.lines().next().unwrap_or("")
        );
    }

    #[test]
    fn test_formatter_for_project_without_name() {
        let f = formatter_for_project("toon", None).unwrap();
        let specs = vec![make_test_spec("s1", "Title", "Desc")];
        let output = f.format_spec_list(&specs).unwrap();
        assert!(
            output.contains("project: unknown"),
            "TOON header should say unknown when no project name"
        );
    }

    #[test]
    fn test_formatter_for_project_json_ignores_name() {
        // JSON formatters don't use project name — should still work
        let f = formatter_for_project("json", Some("my-app")).unwrap();
        assert_eq!(f.name(), "json");
        let f2 = formatter_for_project("json-compact", Some("my-app")).unwrap();
        assert_eq!(f2.name(), "json-compact");
    }

    #[test]
    fn test_all_three_formatters_produce_output() {
        let specs = vec![make_test_spec("test", "Test", "A test spec")];
        for name in FORMAT_NAMES {
            let f = formatter_for(name).expect(&format!("formatter for {name}"));
            let result = f.format_spec_list(&specs);
            assert!(
                result.is_ok(),
                "formatter {name} failed: {:?}",
                result.err()
            );
            assert!(
                !result.unwrap().is_empty(),
                "formatter {name} produced empty output"
            );
        }
    }

    // --- Bri: T.3 — TOON serialization tests ---

    #[test]
    fn test_toon_single_spec_uses_header_format() {
        let specs = vec![make_test_spec("s1", "Solo", "One spec")];
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&specs).unwrap();
        assert!(result.contains("# writ spec-list"));
        assert!(result.contains("Solo"));
    }

    #[test]
    fn test_toon_empty_array_produces_header() {
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&[]).unwrap();
        assert!(result.starts_with("# writ spec-list"));
    }

    #[test]
    fn test_toon_empty_seal_log_has_header() {
        let f = ToonFormatter::new();
        let result = f.format_seal_log(&[]).unwrap();
        assert!(result.contains("# writ seal-log"));
    }

    #[test]
    fn test_toon_header_contains_timestamp() {
        let f = ToonFormatter::new();
        let header = f.header_comment("context");
        assert!(header.contains("timestamp:"));
        assert!(header.contains("T"));
        assert!(header.contains("Z"));
    }

    #[test]
    fn test_toon_default_project_unknown() {
        let f = ToonFormatter::new();
        let header = f.header_comment("context");
        assert!(header.contains("project: unknown"));
    }

    #[test]
    fn test_toon_named_project_in_header() {
        let f = ToonFormatter::with_project("my-cool-app");
        let header = f.header_comment("context");
        assert!(header.contains("project: my-cool-app"));
    }

    // --- Bri: T.4 — Round-trip consistency tests ---

    #[test]
    fn test_json_and_compact_identical_data() {
        let specs = vec![
            make_test_spec("auth", "Authentication", "JWT auth"),
            make_test_spec("ui", "Frontend", "React UI"),
        ];
        let json_out = JsonFormatter.format_spec_list(&specs).unwrap();
        let compact_out = JsonCompactFormatter.format_spec_list(&specs).unwrap();

        let json_val: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        let compact_val: serde_json::Value = serde_json::from_str(&compact_out).unwrap();
        assert_eq!(json_val, compact_val);
    }

    #[test]
    fn test_json_pretty_has_indentation() {
        let specs = vec![make_test_spec("s1", "Title", "Desc")];
        let result = JsonFormatter.format_spec_list(&specs).unwrap();
        assert!(result.contains('\n'));
        assert!(result.contains("  "));
    }

    #[test]
    fn test_json_compact_no_indentation() {
        let specs = vec![make_test_spec("s1", "Title", "Desc")];
        let result = JsonCompactFormatter.format_spec_list(&specs).unwrap();
        assert!(!result.contains("\n  "));
    }

    #[test]
    fn test_toon_contains_all_spec_ids() {
        let specs: Vec<Spec> = (0..5)
            .map(|i| make_test_spec(&format!("spec-{i}"), &format!("T{i}"), &format!("D{i}")))
            .collect();
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&specs).unwrap();
        for i in 0..5 {
            assert!(result.contains(&format!("spec-{i}")));
        }
    }

    // --- Bri: T.5 — TOON edge case tests ---

    #[test]
    fn test_toon_spec_with_comma_in_description() {
        let specs = vec![make_test_spec("s1", "Auth", "JWT, refresh tokens, MFA")];
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&specs).unwrap();
        assert!(result.contains("Auth"));
        assert!(!result.is_empty());
    }

    #[test]
    fn test_toon_spec_with_unicode() {
        let specs = vec![make_test_spec("s1", "国際化", "ユニコードテスト")];
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&specs).unwrap();
        assert!(result.contains("国際化"));
        assert!(result.contains("ユニコードテスト"));
    }

    #[test]
    fn test_toon_spec_with_empty_description() {
        let specs = vec![make_test_spec("s1", "Title", "")];
        let f = ToonFormatter::new();
        let result = f.format_spec_list(&specs).unwrap();
        assert!(result.contains("Title"));
    }

    #[test]
    fn test_toon_50_specs_still_smaller_than_json() {
        let specs: Vec<Spec> = (0..50)
            .map(|i| {
                make_test_spec(
                    &format!("spec-{:04}", i),
                    &format!("Feature Number {} longer title", i),
                    &format!("Description for feature {} with details", i),
                )
            })
            .collect();

        let json_out = JsonFormatter.format_spec_list(&specs).unwrap();
        let toon_out = ToonFormatter::new().format_spec_list(&specs).unwrap();

        assert!(
            toon_out.len() < json_out.len(),
            "TOON ({} bytes) should be smaller than JSON ({} bytes)",
            toon_out.len(),
            json_out.len()
        );
    }

    // --- Bri: T.6 — Format resolution chain (factory level) ---

    #[test]
    fn test_formatter_for_empty_string() {
        assert!(formatter_for("").is_none());
    }

    #[test]
    fn test_formatter_for_case_sensitive() {
        assert!(formatter_for("JSON").is_none());
        assert!(formatter_for("TOON").is_none());
    }

    #[test]
    fn test_is_valid_format_rejects_old_names() {
        assert!(!is_valid_format("human"));
        assert!(!is_valid_format("brief"));
        assert!(!is_valid_format("commit"));
        assert!(!is_valid_format("pr"));
    }

    #[test]
    fn test_all_format_names_are_valid() {
        for name in FORMAT_NAMES {
            assert!(is_valid_format(name));
        }
    }

    #[test]
    fn test_all_format_names_have_formatters() {
        for name in FORMAT_NAMES {
            assert!(formatter_for(name).is_some(), "No formatter for '{}'", name);
        }
    }

    #[test]
    fn test_formatter_names_match_factory_key() {
        for name in FORMAT_NAMES {
            let f = formatter_for(name).unwrap();
            assert_eq!(f.name(), *name);
        }
    }

    #[test]
    fn test_toon_default_impl() {
        let f = ToonFormatter::default();
        assert!(f.project_name.is_none());
        assert_eq!(f.name(), "toon");
    }

    // -----------------------------------------------------------------------
    // F.14 — Token efficiency benchmarks
    //
    // These tests generate realistic writ data (seals, specs, full context)
    // and measure byte-size savings across JSON, JSON-compact, and TOON.
    // Assertions enforce minimum savings thresholds so regressions are caught.
    // -----------------------------------------------------------------------

    use crate::context::{ContextOutput, IntegrationRisk, SealSummary, WorkingStateSummary};

    /// Build N realistic seals with file changes and summaries.
    fn make_benchmark_seals(n: usize) -> Vec<crate::seal::Seal> {
        use crate::seal::{
            AgentIdentity, AgentType, ChangeType, FileChange, TaskStatus, Verification,
        };
        (0..n)
            .map(|i| {
                let agent_id = match i % 3 {
                    0 => "cc-opus",
                    1 => "amis-sonnet",
                    _ => "bri-haiku",
                };
                let spec = match i % 4 {
                    0 => Some("auth".into()),
                    1 => Some("convergence".into()),
                    2 => Some("format".into()),
                    _ => None,
                };
                let changes: Vec<FileChange> = (0..((i % 5) + 1))
                    .map(|j| FileChange {
                        path: format!("src/module_{}/file_{}.rs", i % 8, j),
                        change_type: if j == 0 {
                            ChangeType::Added
                        } else {
                            ChangeType::Modified
                        },
                        old_hash: if j == 0 {
                            None
                        } else {
                            Some(format!("old_{:08x}", i * 100 + j))
                        },
                        new_hash: Some(format!("new_{:08x}", i * 100 + j)),
                    })
                    .collect();
                Seal::new(
                    if i > 0 {
                        Some(format!("parent_seal_{:04x}", i - 1))
                    } else {
                        None
                    },
                    format!("tree_hash_{:08x}", i * 31337),
                    AgentIdentity {
                        id: agent_id.into(),
                        agent_type: AgentType::Agent,
                    },
                    spec,
                    if i == n - 1 {
                        TaskStatus::Complete
                    } else {
                        TaskStatus::InProgress
                    },
                    changes,
                    Verification {
                        tests_passed: Some((i * 10 + 100) as u32),
                        tests_failed: Some(0),
                        linted: true,
                    },
                    format!(
                        "Implemented feature {} — updated {} files with new logic",
                        i,
                        (i % 5) + 1
                    ),
                    vec![],
                    None,
                )
            })
            .collect()
    }

    /// Build N realistic seal summaries (as used in context output).
    fn make_benchmark_seal_summaries(n: usize) -> Vec<SealSummary> {
        (0..n)
            .map(|i| {
                let agent = match i % 3 {
                    0 => "cc-opus",
                    1 => "amis-sonnet",
                    _ => "bri-haiku",
                };
                SealSummary {
                    id: format!("{:012x}", i * 0xDEAD + 0xBEEF),
                    timestamp: format!("2026-03-04T{:02}:{:02}:00Z", 10 + (i / 60), i % 60),
                    agent: agent.into(),
                    summary: format!("Updated module {} with feature implementation", i),
                    files_changed: (i % 5) + 1,
                    spec_id: match i % 4 {
                        0 => Some("auth".into()),
                        1 => Some("convergence".into()),
                        2 => Some("format".into()),
                        _ => None,
                    },
                    status: "in-progress".into(),
                    verification: None,
                    changed_paths: (0..((i % 3) + 1))
                        .map(|j| format!("src/mod_{}/file_{}.rs", i % 6, j))
                        .collect(),
                }
            })
            .collect()
    }

    /// Build a realistic ContextOutput with specs, seals, files, and risk.
    fn make_benchmark_context(n_specs: usize, n_seals: usize, n_files: usize) -> ContextOutput {
        let specs: Vec<Spec> = (0..n_specs)
            .map(|i| {
                make_test_spec(
                    &format!("spec-{i}"),
                    &format!("Feature {i}: implementation task"),
                    &format!("Detailed description for feature {i} covering requirements and acceptance criteria"),
                )
            })
            .collect();

        ContextOutput {
            writ_version: "0.1.0".into(),
            active_spec: specs.first().cloned(),
            all_specs: Some(specs),
            working_state: WorkingStateSummary {
                clean: false,
                new_files: (0..3).map(|i| format!("src/new_{i}.rs")).collect(),
                modified_files: (0..5).map(|i| format!("src/mod_{i}.rs")).collect(),
                deleted_files: vec!["src/old_module.rs".into()],
                tracked_count: n_files,
            },
            recent_seals: make_benchmark_seal_summaries(n_seals),
            pending_changes: None,
            seal_nudge: None,
            file_scope: (0..n_files)
                .map(|i| format!("src/module_{}/file_{}.rs", i / 10, i % 10))
                .collect(),
            tracked_files: n_files,
            dependency_status: None,
            spec_progress: None,
            agent_activity: vec![],
            diverged_branches: vec![],
            convergence_recommended: false,
            file_scope_violations: vec![],
            file_contention: vec![],
            integration_risk: IntegrationRisk {
                level: "low".into(),
                factors: vec![],
                score: 5,
            },
            session_complete: false,
            session_summary: None,
            recommended_action: None,
            chain_integrity: None,
            stale_specs: vec![],
            available_operations: vec![
                "seal".into(),
                "context".into(),
                "log".into(),
                "converge".into(),
                "verify".into(),
            ],
        }
    }

    /// Measure byte sizes across all three formats and return (json, compact, toon).
    fn measure_sizes<F>(format_fn: F) -> (usize, usize, usize)
    where
        F: Fn(&dyn OutputFormatter) -> WritResult<String>,
    {
        let json_size = format_fn(&JsonFormatter).unwrap().len();
        let compact_size = format_fn(&JsonCompactFormatter).unwrap().len();
        let toon_size = format_fn(&ToonFormatter::with_project("benchmark"))
            .unwrap()
            .len();
        (json_size, compact_size, toon_size)
    }

    fn savings_pct(baseline: usize, candidate: usize) -> f64 {
        ((baseline as f64 - candidate as f64) / baseline as f64) * 100.0
    }

    #[test]
    fn benchmark_seal_log_token_savings() {
        let seals = make_benchmark_seals(20);
        let (json, compact, toon) = measure_sizes(|f| f.format_seal_log(&seals));

        let toon_vs_json = savings_pct(json, toon);
        let compact_vs_json = savings_pct(json, compact);

        eprintln!("--- Seal Log (20 seals) ---");
        eprintln!("  JSON pretty:  {json:>6} bytes");
        eprintln!("  JSON compact: {compact:>6} bytes ({compact_vs_json:.0}% smaller)");
        eprintln!("  TOON:         {toon:>6} bytes ({toon_vs_json:.0}% smaller)");

        // TOON must beat pretty JSON on tabular data
        assert!(
            toon < json,
            "TOON ({toon}) should be smaller than JSON ({json})"
        );
        // Seal logs are highly tabular — expect significant savings
        assert!(
            toon_vs_json > 20.0,
            "TOON should save >20% vs JSON on seal logs, got {toon_vs_json:.1}%"
        );
    }

    #[test]
    fn benchmark_spec_list_token_savings() {
        let specs: Vec<Spec> = (0..15)
            .map(|i| {
                make_test_spec(
                    &format!("spec-{i}"),
                    &format!("Feature {i}: implementation task with details"),
                    &format!(
                        "Acceptance criteria for spec {i} covering edge cases and integration"
                    ),
                )
            })
            .collect();
        let (json, compact, toon) = measure_sizes(|f| f.format_spec_list(&specs));

        let toon_vs_json = savings_pct(json, toon);
        let compact_vs_json = savings_pct(json, compact);

        eprintln!("--- Spec List (15 specs) ---");
        eprintln!("  JSON pretty:  {json:>6} bytes");
        eprintln!("  JSON compact: {compact:>6} bytes ({compact_vs_json:.0}% smaller)");
        eprintln!("  TOON:         {toon:>6} bytes ({toon_vs_json:.0}% smaller)");

        assert!(
            toon < json,
            "TOON ({toon}) should be smaller than JSON ({json})"
        );
        // Spec lists have short, uniform fields — savings are modest but positive
        assert!(
            toon_vs_json > 5.0,
            "TOON should save >5% vs JSON on spec lists, got {toon_vs_json:.1}%"
        );
    }

    #[test]
    fn benchmark_full_context_token_savings() {
        let ctx = make_benchmark_context(5, 10, 40);
        let (json, compact, toon) = measure_sizes(|f| f.format_context(&ctx));

        let toon_vs_json = savings_pct(json, toon);
        let compact_vs_json = savings_pct(json, compact);

        eprintln!("--- Full Context (5 specs, 10 seals, 40 files) ---");
        eprintln!("  JSON pretty:  {json:>6} bytes");
        eprintln!("  JSON compact: {compact:>6} bytes ({compact_vs_json:.0}% smaller)");
        eprintln!("  TOON:         {toon:>6} bytes ({toon_vs_json:.0}% smaller)");

        assert!(
            toon < json,
            "TOON ({toon}) should be smaller than JSON ({json})"
        );
        // Full context is mixed (nested + tabular), so savings are less dramatic
        assert!(
            toon_vs_json > 10.0,
            "TOON should save >10% vs JSON on full context, got {toon_vs_json:.1}%"
        );
    }

    #[test]
    fn benchmark_summary_report() {
        // Generate all three data sets and produce a summary for easy reading.
        let seals = make_benchmark_seals(20);
        let specs: Vec<Spec> = (0..15)
            .map(|i| {
                make_test_spec(
                    &format!("spec-{i}"),
                    &format!("Feature {i}: implementation task"),
                    &format!("Description for spec {i}"),
                )
            })
            .collect();
        let ctx = make_benchmark_context(5, 10, 40);

        let seal_sizes = measure_sizes(|f| f.format_seal_log(&seals));
        let spec_sizes = measure_sizes(|f| f.format_spec_list(&specs));
        let ctx_sizes = measure_sizes(|f| f.format_context(&ctx));

        eprintln!();
        eprintln!("╔══════════════════════════════════════════════════════════╗");
        eprintln!("║          F.14 Token Efficiency Benchmark Report         ║");
        eprintln!("╠══════════════════════════════════════════════════════════╣");
        eprintln!("║ Data Type      │ JSON    │ Compact │ TOON    │ Savings  ║");
        eprintln!("╠════════════════╪═════════╪═════════╪═════════╪══════════╣");
        eprintln!(
            "║ Seal log (20)  │ {:>6}B │ {:>6}B │ {:>6}B │ {:>5.1}%   ║",
            seal_sizes.0,
            seal_sizes.1,
            seal_sizes.2,
            savings_pct(seal_sizes.0, seal_sizes.2)
        );
        eprintln!(
            "║ Spec list (15) │ {:>6}B │ {:>6}B │ {:>6}B │ {:>5.1}%   ║",
            spec_sizes.0,
            spec_sizes.1,
            spec_sizes.2,
            savings_pct(spec_sizes.0, spec_sizes.2)
        );
        eprintln!(
            "║ Full ctx (5/10)│ {:>6}B │ {:>6}B │ {:>6}B │ {:>5.1}%   ║",
            ctx_sizes.0,
            ctx_sizes.1,
            ctx_sizes.2,
            savings_pct(ctx_sizes.0, ctx_sizes.2)
        );
        eprintln!("╚══════════════════════════════════════════════════════════╝");
        eprintln!("  (Savings = TOON vs JSON pretty, by byte count)");
        eprintln!();

        // All three data types: TOON must be smaller than JSON
        assert!(seal_sizes.2 < seal_sizes.0);
        assert!(spec_sizes.2 < spec_sizes.0);
        assert!(ctx_sizes.2 < ctx_sizes.0);
    }
}
