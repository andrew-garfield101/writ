//! Core types for the v2 convergence pipeline.
//!
//! These types represent the structural building blocks that the new
//! six-phase pipeline operates on. They are designed to be
//! language-agnostic — the [`LanguageAnalyzer`](super::analyzers::LanguageAnalyzer)
//! trait produces them, and the [`Pattern`](super::patterns::Pattern) trait
//! consumes them.

use serde::{Deserialize, Serialize};

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
}
