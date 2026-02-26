//! Core types for the v2 convergence pipeline.
//!
//! These types represent the structural building blocks that the new
//! six-phase pipeline operates on. They are designed to be
//! language-agnostic — the [`LanguageAnalyzer`](super::analyzers::LanguageAnalyzer)
//! trait produces them, and the [`Pattern`](super::patterns::Pattern) trait
//! consumes them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::traceability::{TraceabilityReport, TraceabilityVerdict};

// ---------------------------------------------------------------------------
// Structural Units — the atoms of language-aware diffing (Phase 1)
// ---------------------------------------------------------------------------

/// The kind of structural unit parsed from source code.
///
/// Language analyzers map source code into these categories so that
/// downstream phases can reason about structure rather than raw lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnitKind {
    /// An import/use/require statement.
    Import,
    /// A top-level definition (function, class, struct, interface, etc.).
    Definition,
    /// An executable statement (assignment, expression, call).
    Statement,
    /// A block of related code (if/else, loop, match, etc.).
    Block,
    /// A comment or docstring.
    Comment,
    /// Whitespace or blank lines (structural separators).
    Whitespace,
    /// Anything the analyzer doesn't specifically classify.
    Unknown,
}

/// A single structural unit parsed from source code.
///
/// This is the fundamental atom of the v2 convergence pipeline. Instead
/// of operating on raw lines, the pipeline operates on structural units
/// that carry semantic meaning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralUnit {
    /// What kind of structural element this is.
    pub kind: UnitKind,
    /// Optional name (e.g. function name, class name, import module).
    /// `None` for anonymous or unnamed units like statements or whitespace.
    pub name: Option<String>,
    /// Line span in the original source (0-indexed, inclusive start, exclusive end).
    pub span: (usize, usize),
    /// The raw text content of this unit.
    pub content: String,
    /// Nested units (e.g. methods inside a class, fields inside a struct).
    pub children: Vec<StructuralUnit>,
    /// Language-specific metadata (decorators, visibility, generic params, etc.).
    /// Stored as key-value pairs to stay language-agnostic at the type level.
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

impl StructuralUnit {
    /// Create a simple unit with no children or metadata.
    pub fn new(
        kind: UnitKind,
        name: Option<String>,
        span: (usize, usize),
        content: String,
    ) -> Self {
        Self {
            kind,
            name,
            span,
            content,
            children: Vec::new(),
            metadata: std::collections::HashMap::new(),
        }
    }

    /// Returns true if this unit has the given name.
    pub fn is_named(&self, name: &str) -> bool {
        self.name.as_deref() == Some(name)
    }
}

// ---------------------------------------------------------------------------
// Phase 1 output — Structural Diff
// ---------------------------------------------------------------------------

/// The result of Phase 1: a structured diff of a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralDiff {
    /// Path of the file being diffed.
    pub file_path: String,
    /// Which analyzer produced this diff (e.g. "python", "generic").
    pub analyzer_used: String,
    /// Conflict regions where left and right diverge from base.
    pub regions: Vec<StructuralConflictRegion>,
}

/// A conflict region expressed in terms of structural units.
///
/// This replaces the line-based `ConflictRegion` for the v2 pipeline
/// while the old type continues to work for backward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralConflictRegion {
    /// Structural units from the base version.
    pub base_units: Vec<StructuralUnit>,
    /// Structural units from agent A's version (left).
    pub left_units: Vec<StructuralUnit>,
    /// Structural units from agent B's version (right).
    pub right_units: Vec<StructuralUnit>,
    /// Line span in base file.
    pub base_span: (usize, usize),
    /// Line span in left file.
    pub left_span: (usize, usize),
    /// Line span in right file.
    pub right_span: (usize, usize),
}

// ---------------------------------------------------------------------------
// Phase 2 output — Classification
// ---------------------------------------------------------------------------

/// Conflict type classification for Phase 2.
///
/// This extends the existing `ConflictClass` with safer defaults
/// and richer information for downstream phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictType {
    /// Both sides made identical changes or changed non-overlapping regions.
    Clean,
    /// Only agent A changed this region.
    LeftOnly,
    /// Only agent B changed this region.
    RightOnly,
    /// Base was empty/absent, both sides added content.
    BothInserted,
    /// One side deleted, the other modified. **Always requires review.**
    DeleteVsModify,
    /// Both sides changed the same region differently.
    BothModified,
    /// Both sides deleted the same region.
    BothDeleted,
}

/// Structural information about a conflict, derived from Phase 1's analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuralInfo {
    /// What kinds of structural units are on the left side.
    pub left_unit_kinds: Vec<UnitKind>,
    /// What kinds of structural units are on the right side.
    pub right_unit_kinds: Vec<UnitKind>,
    /// Do both sides define things with the same name?
    pub has_name_overlap: bool,
    /// The scope of the conflict (imports, definitions, etc.).
    pub scope: ConflictScope,
}

/// The structural scope of a conflict region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConflictScope {
    /// Conflict involves only import statements.
    Import,
    /// Conflict involves top-level definitions (functions, classes, etc.).
    Definition,
    /// Conflict is within a single function/method body.
    IntraFunction,
    /// Conflict spans multiple structural types.
    Mixed,
}

impl ConflictScope {
    /// Determine the conflict scope from the unit kinds on both sides.
    /// Whitespace should be filtered out before calling this.
    pub fn from_unit_kinds(left_kinds: &[UnitKind], right_kinds: &[UnitKind]) -> Self {
        let mut all_kinds: Vec<&UnitKind> = left_kinds.iter().chain(right_kinds.iter()).collect();
        all_kinds.dedup_by(|a, b| a == b);

        if all_kinds.is_empty() {
            return ConflictScope::Mixed;
        }

        if all_kinds.iter().all(|k| matches!(k, UnitKind::Import)) {
            return ConflictScope::Import;
        }
        if all_kinds.iter().all(|k| matches!(k, UnitKind::Definition)) {
            return ConflictScope::Definition;
        }
        if all_kinds
            .iter()
            .all(|k| matches!(k, UnitKind::Statement | UnitKind::Block))
        {
            return ConflictScope::IntraFunction;
        }

        ConflictScope::Mixed
    }
}

/// A fully classified conflict, ready for Phase 3+ resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifiedConflict {
    /// The structural conflict region from Phase 1.
    pub region: StructuralConflictRegion,
    /// The classified conflict type.
    pub conflict_type: ConflictType,
    /// Whether this conflict requires human/orchestrator review.
    /// `true` for DeleteVsModify (always) and other safety-critical cases.
    pub requires_review: bool,
    /// Structural information from the language analyzer.
    pub structural_info: StructuralInfo,
}

// ---------------------------------------------------------------------------
// Phase 3 output — Resolution Proposals
// ---------------------------------------------------------------------------

/// A proposed resolution from a pattern or higher phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionProposal {
    /// Which pattern (or phase) produced this proposal.
    pub pattern_name: String,
    /// Confidence score [0.0, 1.0].
    pub confidence: f64,
    /// The proposed merged content.
    pub merged_content: String,
    /// Human/agent-readable explanation of the resolution.
    pub explanation: String,
    /// Warnings the reviewer should know about.
    #[serde(default)]
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// Escalation
// ---------------------------------------------------------------------------

/// Why a conflict was escalated out of the deterministic pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EscalationReason {
    /// Phase 2 classified as DeleteVsModify.
    DeleteVsModify,
    /// No pattern matched above the suggest threshold.
    NoPatternMatch,
    /// Pattern matched but confidence was below auto-resolve threshold.
    LowConfidence,
    /// Spec-aware resolution found conflicting spec claims.
    ConflictingSpecs,
    /// LLM confidence was below threshold.
    LowLlmConfidence,
    /// LLM sanity check failed.
    LlmSanityCheckFailed,
    /// Verification (Phase 6) failed.
    VerificationFailed,
    /// Internal error — fail-safe: always escalate, never guess.
    InternalError(String),
}

/// A record of an escalated conflict with all gathered context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EscalationRecord {
    /// File path where the conflict occurred.
    pub file_path: String,
    /// The conflict type.
    pub conflict_type: ConflictType,
    /// Base content of the conflict region.
    pub base_content: String,
    /// Left (agent A) content.
    pub left_content: String,
    /// Right (agent B) content.
    pub right_content: String,
    /// Left agent identifier.
    pub left_agent: String,
    /// Right agent identifier.
    pub right_agent: String,
    /// Phase 3 suggestion, if any.
    pub phase3_suggestion: Option<ResolutionProposal>,
    /// Why this conflict was escalated.
    pub reason: EscalationReason,
    /// Best-guess recommendation, even if low confidence.
    pub recommended_action: String,
}

// ---------------------------------------------------------------------------
// Phase 6 — Verification
// ---------------------------------------------------------------------------

/// The outcome of Phase 6 verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationVerdict {
    /// All checks passed.
    Verified,
    /// Checks passed but with warnings.
    PassedWithWarnings,
    /// Verification failed — convergence should be rejected.
    Failed,
}

/// Result of verifying a merged file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Does the merged file parse as valid syntax?
    pub syntactic_valid: bool,
    /// Warnings from verification.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Overall verdict.
    pub verdict: VerificationVerdict,
}

// ---------------------------------------------------------------------------
// Confidence thresholds (configurable defaults)
// ---------------------------------------------------------------------------

/// Confidence thresholds that govern the pipeline's behavior.
///
/// These are the defaults from the spec. In production, they'll be
/// loaded from `writ.toml` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceThresholds {
    /// Patterns scoring above this are auto-applied.
    pub auto_resolve: f64,
    /// Patterns scoring between suggest and auto_resolve are forwarded
    /// to Phase 4/5 as suggestions.
    pub suggest: f64,
}

impl Default for ConfidenceThresholds {
    fn default() -> Self {
        Self {
            auto_resolve: 0.85,
            suggest: 0.60,
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience impls
// ---------------------------------------------------------------------------

impl ConflictType {
    /// Returns `true` if this conflict type always requires human review.
    pub fn always_requires_review(&self) -> bool {
        matches!(self, ConflictType::DeleteVsModify)
    }
}

impl StructuralConflictRegion {
    /// Returns `true` if the base side of this region is empty.
    pub fn base_is_empty(&self) -> bool {
        self.base_units.is_empty()
    }

    /// Collect all unique definition names from both sides.
    pub fn definition_names(&self) -> (Vec<String>, Vec<String>) {
        let left_names: Vec<String> = self
            .left_units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .filter_map(|u| u.name.clone())
            .collect();
        let right_names: Vec<String> = self
            .right_units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .filter_map(|u| u.name.clone())
            .collect();
        (left_names, right_names)
    }

    /// Returns `true` if the conflict is exclusively about imports.
    pub fn is_import_only(&self) -> bool {
        let all_left = self.left_units.iter().all(|u| {
            matches!(
                u.kind,
                UnitKind::Import | UnitKind::Whitespace | UnitKind::Comment
            )
        });
        let all_right = self.right_units.iter().all(|u| {
            matches!(
                u.kind,
                UnitKind::Import | UnitKind::Whitespace | UnitKind::Comment
            )
        });
        all_left && all_right && (!self.left_units.is_empty() || !self.right_units.is_empty())
    }
}

// ---------------------------------------------------------------------------
// Pipeline contract types (Phase-to-Phase communication)
// ---------------------------------------------------------------------------

/// Phase 1 result for a single file.
///
/// Wraps the diff3 output: either a clean merge (no conflicts) or a
/// structural diff with conflict regions annotated by the language analyzer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Phase1Result {
    /// diff3 found no conflicts — file merges cleanly.
    Clean(String),
    /// Conflicts found — structural analysis applied to each region.
    Conflicts(StructuralDiff),
}

/// Phase 2 result: all conflicts in a file classified and ready for Phase 3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase2Result {
    /// File path being processed.
    pub file_path: String,
    /// Which analyzer was used.
    pub analyzer_used: String,
    /// Each conflict region, classified with type and structural info.
    pub classified_conflicts: Vec<ClassifiedConflict>,
}

/// What happened to a single conflict region after pipeline processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RegionResolutionStatus {
    /// Resolved by a pattern or phase.
    Resolved {
        /// The merged content for this region.
        content: String,
        /// Which method resolved it (pattern name or phase description).
        method: String,
        /// Confidence score of the resolution.
        confidence: f64,
        /// Which phase resolved it (3 = deterministic, 4 = spec-aware, 5 = LLM).
        resolved_in_phase: u8,
    },
    /// Escalated — could not be auto-resolved.
    Escalated {
        /// Why this region was escalated.
        reason: EscalationReason,
        /// Best-guess recommendation for the reviewer.
        recommendation: String,
    },
}

/// Per-region audit trail — tracks a conflict through the entire pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionOutcome {
    /// The classified conflict from Phase 2.
    pub classified: ClassifiedConflict,
    /// Phase 3 proposal, if a pattern matched (even if not auto-resolved).
    pub phase3_result: Option<ResolutionProposal>,
    /// Final resolution status after all phases.
    pub resolution: RegionResolutionStatus,
}

/// Complete pipeline result for a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineFileResult {
    /// File path that was processed.
    pub file_path: String,
    /// Which analyzer was used for structural analysis.
    pub analyzer_used: String,
    /// Fully merged content, if all regions were resolved.
    pub merged_content: Option<String>,
    /// Per-region audit trail.
    pub region_outcomes: Vec<RegionOutcome>,
    /// Escalation records for regions that could not be auto-resolved.
    pub escalations: Vec<EscalationRecord>,
    /// True if every conflict region was resolved.
    pub fully_resolved: bool,
}

// ---------------------------------------------------------------------------
// Convergence Seal Record — reproducibility and auditability metadata
// ---------------------------------------------------------------------------

/// A record capturing the full context of a convergence operation.
///
/// Created alongside a convergence seal. Enables reproducibility auditing:
/// given the same inputs and config, deterministic phases should produce
/// identical output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConvergenceSealRecord {
    /// The seal ID this record is attached to.
    pub seal_id: String,
    /// When the convergence was performed.
    pub timestamp: DateTime<Utc>,
    /// The spec used as the merge base.
    pub base_spec: String,
    /// Spec IDs that participated in the convergence (including base).
    pub participating_specs: Vec<String>,
    /// BLAKE3 hashes of input seals for reproducibility.
    pub input_seal_hashes: Vec<String>,
    /// Convergence engine version string.
    pub pipeline_version: String,
    /// Pattern name → version mapping for deterministic replay.
    pub pattern_versions: HashMap<String, String>,
    /// BLAKE3 hash of serialized pipeline configuration.
    pub configuration_hash: String,
    /// Per-file traceability reports (only for merged files).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub traceability_reports: Vec<TraceabilityReport>,
    /// True if ALL files passed traceability validation.
    pub traceability_passed: bool,
    /// Number of files auto-merged (no conflicts).
    pub files_auto_merged: usize,
    /// Number of files auto-resolved (conflicts resolved by patterns/pipeline).
    pub files_auto_resolved: usize,
    /// Number of files with unresolved conflicts.
    pub files_escalated: usize,
    /// Whether the convergence was degraded (content loss possible).
    pub degraded: bool,
    /// Files changed by convergence.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files_changed: Vec<String>,
    /// Quality score from the convergence quality report (0-100).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<u32>,
}

impl ConvergenceSealRecord {
    /// Create a new record from a converge_all report and traceability results.
    pub fn from_report(
        seal_id: &str,
        report: &super::ConvergeAllReport,
        traceability_reports: Vec<TraceabilityReport>,
        input_seal_hashes: Vec<String>,
        pipeline_version: &str,
        pattern_versions: HashMap<String, String>,
        configuration_hash: &str,
    ) -> Self {
        let traceability_passed = traceability_reports
            .iter()
            .all(|r| r.verdict == TraceabilityVerdict::Pass);

        let quality_score = report.quality_report.as_ref().map(|qr| qr.quality_score);

        let mut participating_specs = vec![report.base_spec.clone()];
        participating_specs.extend(report.merge_order.iter().cloned());

        ConvergenceSealRecord {
            seal_id: seal_id.to_string(),
            timestamp: Utc::now(),
            base_spec: report.base_spec.clone(),
            participating_specs,
            input_seal_hashes,
            pipeline_version: pipeline_version.to_string(),
            pattern_versions,
            configuration_hash: configuration_hash.to_string(),
            traceability_reports,
            traceability_passed,
            files_auto_merged: report.total_auto_merged,
            files_auto_resolved: report.total_resolutions,
            files_escalated: report
                .total_conflicts
                .saturating_sub(report.total_resolutions),
            degraded: report.degraded,
            files_changed: report.files_changed.clone(),
            quality_score,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_structural_unit_new() {
        let unit = StructuralUnit::new(
            UnitKind::Definition,
            Some("MyClass".into()),
            (0, 10),
            "class MyClass:\n    pass\n".into(),
        );
        assert_eq!(unit.kind, UnitKind::Definition);
        assert!(unit.is_named("MyClass"));
        assert!(!unit.is_named("Other"));
        assert!(unit.children.is_empty());
        assert!(unit.metadata.is_empty());
    }

    #[test]
    fn test_conflict_type_requires_review() {
        assert!(ConflictType::DeleteVsModify.always_requires_review());
        assert!(!ConflictType::BothModified.always_requires_review());
        assert!(!ConflictType::Clean.always_requires_review());
    }

    #[test]
    fn test_structural_conflict_region_base_is_empty() {
        let region = StructuralConflictRegion {
            base_units: vec![],
            left_units: vec![StructuralUnit::new(
                UnitKind::Definition,
                Some("foo".into()),
                (0, 2),
                "def foo(): pass".into(),
            )],
            right_units: vec![],
            base_span: (0, 0),
            left_span: (0, 2),
            right_span: (0, 0),
        };
        assert!(region.base_is_empty());
    }

    #[test]
    fn test_definition_names_extraction() {
        let region = StructuralConflictRegion {
            base_units: vec![],
            left_units: vec![
                StructuralUnit::new(
                    UnitKind::Definition,
                    Some("User".into()),
                    (0, 5),
                    "class User: ...".into(),
                ),
                StructuralUnit::new(UnitKind::Import, None, (6, 6), "import os".into()),
            ],
            right_units: vec![StructuralUnit::new(
                UnitKind::Definition,
                Some("Product".into()),
                (0, 5),
                "class Product: ...".into(),
            )],
            base_span: (0, 0),
            left_span: (0, 7),
            right_span: (0, 5),
        };
        let (left, right) = region.definition_names();
        assert_eq!(left, vec!["User"]);
        assert_eq!(right, vec!["Product"]);
    }

    #[test]
    fn test_import_only_region() {
        let region = StructuralConflictRegion {
            base_units: vec![],
            left_units: vec![StructuralUnit::new(
                UnitKind::Import,
                Some("os".into()),
                (0, 1),
                "import os".into(),
            )],
            right_units: vec![StructuralUnit::new(
                UnitKind::Import,
                Some("sys".into()),
                (0, 1),
                "import sys".into(),
            )],
            base_span: (0, 0),
            left_span: (0, 1),
            right_span: (0, 1),
        };
        assert!(region.is_import_only());
    }

    #[test]
    fn test_mixed_region_is_not_import_only() {
        let region = StructuralConflictRegion {
            base_units: vec![],
            left_units: vec![
                StructuralUnit::new(UnitKind::Import, None, (0, 1), "import os".into()),
                StructuralUnit::new(
                    UnitKind::Definition,
                    Some("main".into()),
                    (2, 4),
                    "def main(): ...".into(),
                ),
            ],
            right_units: vec![],
            base_span: (0, 0),
            left_span: (0, 4),
            right_span: (0, 0),
        };
        assert!(!region.is_import_only());
    }

    #[test]
    fn test_confidence_thresholds_defaults() {
        let t = ConfidenceThresholds::default();
        assert!((t.auto_resolve - 0.85).abs() < f64::EPSILON);
        assert!((t.suggest - 0.60).abs() < f64::EPSILON);
    }

    #[test]
    fn test_resolution_proposal_serialization() {
        let proposal = ResolutionProposal {
            pattern_name: "additive_composition".into(),
            confidence: 0.85,
            merged_content: "merged code".into(),
            explanation: "Both sides add non-overlapping content".into(),
            warnings: vec![],
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let decoded: ResolutionProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.pattern_name, "additive_composition");
        assert!((decoded.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_escalation_record_construction() {
        let record = EscalationRecord {
            file_path: "models.py".into(),
            conflict_type: ConflictType::BothModified,
            base_content: "class User: pass".into(),
            left_content: "class User:\n    name: str".into(),
            right_content: "class User:\n    email: str".into(),
            left_agent: "agent-a".into(),
            right_agent: "agent-b".into(),
            phase3_suggestion: None,
            reason: EscalationReason::NoPatternMatch,
            recommended_action: "Manual review required".into(),
        };
        assert_eq!(record.reason, EscalationReason::NoPatternMatch);
    }

    #[test]
    fn test_phase1_result_serialization() {
        let clean = Phase1Result::Clean("merged content".into());
        let json = serde_json::to_string(&clean).unwrap();
        let decoded: Phase1Result = serde_json::from_str(&json).unwrap();
        match decoded {
            Phase1Result::Clean(s) => assert_eq!(s, "merged content"),
            _ => panic!("expected Clean"),
        }
    }

    #[test]
    fn test_region_resolution_status_variants() {
        let resolved = RegionResolutionStatus::Resolved {
            content: "merged".into(),
            method: "import_accumulation".into(),
            confidence: 0.95,
            resolved_in_phase: 3,
        };
        let json = serde_json::to_string(&resolved).unwrap();
        let decoded: RegionResolutionStatus = serde_json::from_str(&json).unwrap();
        match decoded {
            RegionResolutionStatus::Resolved {
                confidence,
                resolved_in_phase,
                ..
            } => {
                assert!((confidence - 0.95).abs() < f64::EPSILON);
                assert_eq!(resolved_in_phase, 3);
            }
            _ => panic!("expected Resolved"),
        }

        let escalated = RegionResolutionStatus::Escalated {
            reason: EscalationReason::DeleteVsModify,
            recommendation: "Review deletion".into(),
        };
        let json = serde_json::to_string(&escalated).unwrap();
        let decoded: RegionResolutionStatus = serde_json::from_str(&json).unwrap();
        match decoded {
            RegionResolutionStatus::Escalated { reason, .. } => {
                assert_eq!(reason, EscalationReason::DeleteVsModify);
            }
            _ => panic!("expected Escalated"),
        }
    }

    #[test]
    fn test_pipeline_file_result_construction() {
        let result = PipelineFileResult {
            file_path: "models.py".into(),
            analyzer_used: "python".into(),
            merged_content: Some("class User: pass".into()),
            region_outcomes: vec![],
            escalations: vec![],
            fully_resolved: true,
        };
        assert!(result.fully_resolved);
        assert!(result.merged_content.is_some());
        assert!(result.escalations.is_empty());
    }

    // -- ConvergenceSealRecord tests --

    fn make_minimal_report() -> crate::convergence::ConvergeAllReport {
        crate::convergence::ConvergeAllReport {
            base_spec: "spec-base".to_string(),
            merge_order: vec!["spec-a".to_string(), "spec-b".to_string()],
            merges: vec![],
            strategy: "escalate".to_string(),
            total_auto_merged: 5,
            total_conflicts: 3,
            total_resolutions: 2,
            is_clean: false,
            degraded: false,
            applied: true,
            warnings: vec![],
            escalations: vec![],
            quality_report: None,
            files_changed: vec!["src/main.rs".to_string()],
            convergence_record: None,
        }
    }

    #[test]
    fn test_convergence_seal_record_construction() {
        let report = make_minimal_report();
        let record = ConvergenceSealRecord::from_report(
            "seal-123",
            &report,
            vec![],
            vec!["hash-a".to_string(), "hash-b".to_string()],
            "0.1.0",
            HashMap::new(),
            "config-hash-abc",
        );

        assert_eq!(record.seal_id, "seal-123");
        assert_eq!(record.base_spec, "spec-base");
        assert_eq!(
            record.participating_specs,
            vec!["spec-base", "spec-a", "spec-b"]
        );
        assert_eq!(record.input_seal_hashes.len(), 2);
        assert_eq!(record.pipeline_version, "0.1.0");
        assert_eq!(record.configuration_hash, "config-hash-abc");
        assert_eq!(record.files_auto_merged, 5);
        assert_eq!(record.files_auto_resolved, 2);
        assert_eq!(record.files_escalated, 1);
        assert!(!record.degraded);
        assert!(record.traceability_passed); // no reports = all pass
    }

    #[test]
    fn test_convergence_seal_record_serialization_roundtrip() {
        let report = make_minimal_report();
        let mut patterns = HashMap::new();
        patterns.insert("additive".to_string(), "1.0".to_string());
        let record = ConvergenceSealRecord::from_report(
            "seal-456",
            &report,
            vec![],
            vec!["h1".to_string()],
            "0.2.0",
            patterns,
            "cfg-hash",
        );

        let json = serde_json::to_string_pretty(&record).unwrap();
        let parsed: ConvergenceSealRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.seal_id, "seal-456");
        assert_eq!(parsed.pipeline_version, "0.2.0");
        assert_eq!(
            parsed.pattern_versions.get("additive"),
            Some(&"1.0".to_string())
        );
    }

    #[test]
    fn test_convergence_seal_record_traceability_passed_false() {
        use crate::convergence::traceability::{TraceabilityReport, TraceabilityVerdict};

        let report = make_minimal_report();
        let failing_trace = TraceabilityReport {
            file_path: "src/main.rs".to_string(),
            verdict: TraceabilityVerdict::Fail,
            novel_units: vec![],
            untraced_lines: vec![],
            lines_checked: 10,
            lines_passed: 8,
            threshold: 0,
            summary: "TIER 2 FAIL".to_string(),
        };

        let record = ConvergenceSealRecord::from_report(
            "seal-789",
            &report,
            vec![failing_trace],
            vec![],
            "0.1.0",
            HashMap::new(),
            "cfg",
        );

        assert!(
            !record.traceability_passed,
            "Should be false when any report fails"
        );
    }

    #[test]
    fn test_convergence_seal_record_config_hash_deterministic() {
        let report = make_minimal_report();
        let r1 = ConvergenceSealRecord::from_report(
            "s",
            &report,
            vec![],
            vec![],
            "v",
            HashMap::new(),
            "hash-A",
        );
        let r2 = ConvergenceSealRecord::from_report(
            "s",
            &report,
            vec![],
            vec![],
            "v",
            HashMap::new(),
            "hash-A",
        );
        assert_eq!(r1.configuration_hash, r2.configuration_hash);
        assert_eq!(r1.configuration_hash, "hash-A");
    }

    #[test]
    fn test_convergence_seal_record_has_all_sprint_doc_fields() {
        let report = make_minimal_report();
        let mut patterns = HashMap::new();
        patterns.insert("imports".to_string(), "2.0".to_string());

        let record = ConvergenceSealRecord::from_report(
            "seal-x",
            &report,
            vec![],
            vec!["hash-1".to_string()],
            "0.3.0",
            patterns,
            "config-hash-xyz",
        );

        // Sprint doc required fields: input_seal_hashes, pipeline_version,
        // pattern_versions, configuration_hash
        assert!(!record.input_seal_hashes.is_empty());
        assert!(!record.pipeline_version.is_empty());
        assert!(!record.pattern_versions.is_empty());
        assert!(!record.configuration_hash.is_empty());
    }
}
