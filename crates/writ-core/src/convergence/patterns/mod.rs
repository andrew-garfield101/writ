//! Pattern Registry for deterministic conflict resolution (Phase 3).
//!
//! Patterns are evaluated in parallel (all that match are scored), not
//! sequentially. The highest-confidence pattern wins. This replaces
//! the Sprint 16 sequential pattern matching with a more robust system
//! that always picks the best available resolution.
//!
//! ## Core Patterns
//!
//! - [`ImportAccumulation`](imports::ImportAccumulation) — Union of imports from both sides
//! - [`NonOverlappingDefinitions`](definitions::NonOverlappingDefinitions) — Compose when names don't collide
//! - [`AdditiveComposition`](additive::AdditiveComposition) — Base + left additions + right additions
//! - [`SupersetContainment`](superset::SupersetContainment) — One side contains everything the other has
//! - [`EofAppend`](eof_append::EofAppend) — Both sides append to end of file

pub mod additive;
pub mod definitions;
pub mod eof_append;
pub mod imports;
pub mod superset;

use super::types::{ClassifiedConflict, ConfidenceThresholds, ConflictType, ResolutionProposal};

// ---------------------------------------------------------------------------
// Pattern trait
// ---------------------------------------------------------------------------

/// A conflict resolution pattern.
///
/// Patterns are pure functions: given a classified conflict, they either
/// produce a resolution proposal with a confidence score, or return `None`
/// if the pattern doesn't apply.
pub trait Pattern: Send + Sync {
    /// Human-readable name for audit trail and debugging.
    fn name(&self) -> &str;

    /// Which conflict types this pattern can resolve.
    fn applies_to(&self) -> &[ConflictType];

    /// Check if this pattern's preconditions are met.
    fn matches(&self, conflict: &ClassifiedConflict) -> bool;

    /// Attempt to resolve the conflict and produce a proposal.
    ///
    /// Returns `None` if the pattern can't resolve this specific conflict
    /// even though preconditions were met (e.g. name collision detected
    /// during resolution attempt).
    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal>;
}

// ---------------------------------------------------------------------------
// Pattern Registry
// ---------------------------------------------------------------------------

/// Registry that evaluates all matching patterns and picks the best one.
///
/// All patterns that match a conflict are evaluated (conceptually in
/// parallel — in practice sequentially since patterns are fast). The
/// highest-confidence proposal wins.
pub struct PatternRegistry {
    patterns: Vec<Box<dyn Pattern>>,
    thresholds: ConfidenceThresholds,
}

/// The result of running the pattern registry on a single conflict.
#[derive(Debug, Clone)]
pub enum PatternResult {
    /// A pattern resolved the conflict with high confidence (≥ auto_resolve).
    AutoResolved(ResolutionProposal),
    /// A pattern produced a suggestion but below auto-resolve threshold.
    /// Pass to Phase 4/5 for confirmation.
    Suggested(ResolutionProposal),
    /// No pattern matched or all scores were below suggest threshold.
    NoMatch,
}

impl PatternRegistry {
    /// Create a new registry with default patterns and thresholds.
    pub fn new() -> Self {
        Self::with_thresholds(ConfidenceThresholds::default())
    }

    /// Create a registry with custom confidence thresholds.
    pub fn with_thresholds(thresholds: ConfidenceThresholds) -> Self {
        let patterns: Vec<Box<dyn Pattern>> = vec![
            Box::new(imports::ImportAccumulation),
            Box::new(definitions::NonOverlappingDefinitions),
            Box::new(additive::AdditiveComposition),
            Box::new(superset::SupersetContainment),
            Box::new(eof_append::EofAppend),
        ];
        Self {
            patterns,
            thresholds,
        }
    }

    /// Register an additional pattern.
    pub fn add_pattern(&mut self, pattern: Box<dyn Pattern>) {
        self.patterns.push(pattern);
    }

    /// Evaluate all matching patterns and return the best result.
    ///
    /// All patterns whose `applies_to` includes the conflict type AND
    /// whose `matches` returns `true` are evaluated. The highest-confidence
    /// proposal wins, subject to threshold filtering.
    pub fn evaluate(&self, conflict: &ClassifiedConflict) -> PatternResult {
        // Conflicts that always require review skip pattern resolution.
        if conflict.requires_review {
            return PatternResult::NoMatch;
        }

        let mut best: Option<ResolutionProposal> = None;

        for pattern in &self.patterns {
            // Check if pattern applies to this conflict type.
            if !pattern.applies_to().contains(&conflict.conflict_type) {
                continue;
            }

            // Check preconditions.
            if !pattern.matches(conflict) {
                continue;
            }

            // Attempt resolution.
            if let Some(proposal) = pattern.resolve(conflict) {
                let dominated = best
                    .as_ref()
                    .map_or(false, |b| b.confidence >= proposal.confidence);
                if !dominated {
                    best = Some(proposal);
                }
            }
        }

        match best {
            Some(proposal) if proposal.confidence >= self.thresholds.auto_resolve => {
                PatternResult::AutoResolved(proposal)
            }
            Some(proposal) if proposal.confidence >= self.thresholds.suggest => {
                PatternResult::Suggested(proposal)
            }
            _ => PatternResult::NoMatch,
        }
    }
}

impl Default for PatternRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::*;

    fn make_conflict(
        conflict_type: ConflictType,
        left_units: Vec<StructuralUnit>,
        right_units: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units,
                right_units,
                base_span: (0, 0),
                left_span: (0, 0),
                right_span: (0, 0),
            },
            conflict_type,
            requires_review: conflict_type.always_requires_review(),
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        }
    }

    #[test]
    fn test_registry_creation() {
        let registry = PatternRegistry::new();
        assert_eq!(registry.patterns.len(), 5);
    }

    #[test]
    fn test_delete_vs_modify_always_skipped() {
        let registry = PatternRegistry::new();
        let conflict = make_conflict(ConflictType::DeleteVsModify, vec![], vec![]);
        match registry.evaluate(&conflict) {
            PatternResult::NoMatch => {} // expected
            other => panic!("DeleteVsModify should be NoMatch, got: {other:?}"),
        }
    }

    #[test]
    fn test_non_overlapping_definitions_resolves() {
        let registry = PatternRegistry::new();

        let left = vec![StructuralUnit::new(
            UnitKind::Definition,
            Some("User".into()),
            (0, 3),
            "class User:\n    name: str\n    email: str".into(),
        )];
        let right = vec![StructuralUnit::new(
            UnitKind::Definition,
            Some("Product".into()),
            (0, 3),
            "class Product:\n    title: str\n    price: float".into(),
        )];

        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: left,
                right_units: right,
                base_span: (0, 0),
                left_span: (0, 3),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Definition],
                right_unit_kinds: vec![UnitKind::Definition],
                has_name_overlap: false,
                scope: ConflictScope::Definition,
            },
        };

        match registry.evaluate(&conflict) {
            PatternResult::AutoResolved(proposal) => {
                assert_eq!(proposal.pattern_name, "non_overlapping_definitions");
                assert!(proposal.confidence >= 0.85);
                assert!(proposal.merged_content.contains("User"));
                assert!(proposal.merged_content.contains("Product"));
            }
            other => panic!("Expected AutoResolved, got: {other:?}"),
        }
    }

    #[test]
    fn test_import_accumulation_resolves() {
        let registry = PatternRegistry::new();

        let left = vec![StructuralUnit::new(
            UnitKind::Import,
            Some("os".into()),
            (0, 1),
            "import os".into(),
        )];
        let right = vec![StructuralUnit::new(
            UnitKind::Import,
            Some("sys".into()),
            (0, 1),
            "import sys".into(),
        )];

        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: left,
                right_units: right,
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Import],
                has_name_overlap: false,
                scope: ConflictScope::Import,
            },
        };

        match registry.evaluate(&conflict) {
            PatternResult::AutoResolved(proposal) => {
                assert_eq!(proposal.pattern_name, "import_accumulation");
                assert!(proposal.merged_content.contains("import os"));
                assert!(proposal.merged_content.contains("import sys"));
            }
            other => panic!("Expected AutoResolved, got: {other:?}"),
        }
    }

    #[test]
    fn test_highest_confidence_wins() {
        // When multiple patterns match, the highest confidence should win.
        let registry = PatternRegistry::new();

        // Both sides are imports AND could be seen as additive composition.
        // Import accumulation (0.95) should beat additive (0.85).
        let left = vec![StructuralUnit::new(
            UnitKind::Import,
            Some("os".into()),
            (0, 1),
            "import os".into(),
        )];
        let right = vec![StructuralUnit::new(
            UnitKind::Import,
            Some("sys".into()),
            (0, 1),
            "import sys".into(),
        )];

        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: left,
                right_units: right,
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Import],
                has_name_overlap: false,
                scope: ConflictScope::Import,
            },
        };

        match registry.evaluate(&conflict) {
            PatternResult::AutoResolved(proposal) => {
                assert_eq!(
                    proposal.pattern_name, "import_accumulation",
                    "import_accumulation (0.95) should beat other patterns"
                );
            }
            other => panic!("Expected AutoResolved, got: {other:?}"),
        }
    }
}
