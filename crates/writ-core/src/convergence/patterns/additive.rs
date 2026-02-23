//! Additive Composition pattern.
//!
//! When both sides of a BothModified conflict preserved all base content
//! and each added unique content, compose: base + left additions + right
//! additions.

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal};
use std::collections::HashSet;

/// Compose base + left additions + right additions.
pub struct AdditiveComposition;

impl Pattern for AdditiveComposition {
    fn name(&self) -> &str {
        "additive_composition"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothModified]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        // Need non-empty base content to determine additions.
        !conflict.region.base_units.is_empty()
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let base_content: HashSet<String> = conflict
            .region
            .base_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Check that both sides preserve all base content.
        let left_content: Vec<String> = conflict
            .region
            .left_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let right_content: Vec<String> = conflict
            .region
            .right_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let left_set: HashSet<&str> = left_content.iter().map(|s| s.as_str()).collect();
        let right_set: HashSet<&str> = right_content.iter().map(|s| s.as_str()).collect();

        // Both sides must contain all base content.
        for base_line in &base_content {
            if !left_set.contains(base_line.as_str()) || !right_set.contains(base_line.as_str()) {
                return None;
            }
        }

        // Extract additions from each side (content not in base).
        let left_additions: Vec<&str> = left_content
            .iter()
            .filter(|c| !base_content.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();
        let right_additions: Vec<&str> = right_content
            .iter()
            .filter(|c| !base_content.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();

        // At least one side must have additions for this to be useful.
        if left_additions.is_empty() && right_additions.is_empty() {
            return None;
        }

        // Compose: base + left additions + right additions (deduplicated).
        let mut merged_parts: Vec<String> = Vec::new();
        for unit in &conflict.region.base_units {
            merged_parts.push(unit.content.clone());
        }

        let mut seen_additions: HashSet<String> = HashSet::new();
        for addition in &left_additions {
            if seen_additions.insert(addition.to_string()) {
                merged_parts.push(addition.to_string());
            }
        }
        for addition in &right_additions {
            if seen_additions.insert(addition.to_string()) {
                merged_parts.push(addition.to_string());
            }
        }

        let merged_content = merged_parts.join("\n");

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence: 0.85,
            merged_content,
            explanation: format!(
                "Both sides preserved base content; composed {} left additions + {} right additions",
                left_additions.len(),
                right_additions.len(),
            ),
            warnings: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::*;

    fn unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Unknown, None, (0, 1), content.into())
    }

    #[test]
    fn test_additive_composition_both_add() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("line1"), unit("line2")],
                left_units: vec![unit("line1"), unit("line2"), unit("left_new")],
                right_units: vec![unit("line1"), unit("line2"), unit("right_new")],
                base_span: (0, 2),
                left_span: (0, 3),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("line1"));
        assert!(proposal.merged_content.contains("line2"));
        assert!(proposal.merged_content.contains("left_new"));
        assert!(proposal.merged_content.contains("right_new"));
        assert!((proposal.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_returns_none_when_base_missing_from_side() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("line1"), unit("line2")],
                left_units: vec![unit("line1"), unit("replacement")],
                right_units: vec![unit("line1"), unit("line2"), unit("right_new")],
                base_span: (0, 2),
                left_span: (0, 2),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        // Left side dropped "line2" — not additive.
        assert!(pattern.resolve(&conflict).is_none());
    }

    #[test]
    fn test_deduplicates_shared_additions() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("base")],
                left_units: vec![unit("base"), unit("shared_new")],
                right_units: vec![unit("base"), unit("shared_new")],
                base_span: (0, 1),
                left_span: (0, 2),
                right_span: (0, 2),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        let count = proposal.merged_content.matches("shared_new").count();
        assert_eq!(count, 1, "should deduplicate: {}", proposal.merged_content);
    }

    #[test]
    fn test_empty_base_does_not_match() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![unit("left")],
                right_units: vec![unit("right")],
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        assert!(!pattern.matches(&conflict));
    }
}
