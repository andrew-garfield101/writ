//! Non-Overlapping Definition Composition pattern.
//!
//! When both sides of a conflict add or modify definitions (classes,
//! functions, structs) with non-overlapping names, compose them all.
//! This is the language-agnostic generalization of Sprint 16's
//! "Definition-Level Composition" pattern.

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal, UnitKind};
use std::collections::HashSet;

/// Compose definitions when names don't collide.
pub struct NonOverlappingDefinitions;

impl Pattern for NonOverlappingDefinitions {
    fn name(&self) -> &str {
        "non_overlapping_definitions"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothInserted, ConflictType::BothModified]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        // At least one side must have definitions.
        let left_has_defs = conflict
            .region
            .left_units
            .iter()
            .any(|u| u.kind == UnitKind::Definition);
        let right_has_defs = conflict
            .region
            .right_units
            .iter()
            .any(|u| u.kind == UnitKind::Definition);

        (left_has_defs || right_has_defs) && !conflict.structural_info.has_name_overlap
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let (left_names, right_names) = conflict.region.definition_names();

        // Double-check for name collisions (structural_info may be stale).
        let left_set: HashSet<&str> = left_names.iter().map(|s| s.as_str()).collect();
        let right_set: HashSet<&str> = right_names.iter().map(|s| s.as_str()).collect();
        let overlap: Vec<&&str> = left_set.intersection(&right_set).collect();

        if !overlap.is_empty() {
            // Name collision — cannot resolve deterministically.
            return None;
        }

        // Compose: all left units, then all right units.
        // Non-definition units (imports, comments) from both sides are included.
        let mut parts: Vec<String> = Vec::new();

        for unit in &conflict.region.left_units {
            parts.push(unit.content.clone());
        }
        for unit in &conflict.region.right_units {
            // Avoid duplicating non-definition units that appear on both sides.
            if unit.kind != UnitKind::Definition {
                let already_present = conflict
                    .region
                    .left_units
                    .iter()
                    .any(|l| l.content.trim() == unit.content.trim() && l.kind == unit.kind);
                if already_present {
                    continue;
                }
            }
            parts.push(unit.content.clone());
        }

        let merged_content = parts.join("\n\n");
        let total_defs = left_names.len() + right_names.len();

        // Dynamic confidence: base 0.92, penalized by -0.02 per definition
        // beyond 3. Floor at 0.80 to stay well above suggest threshold.
        let penalty = if total_defs > 3 {
            (total_defs - 3) as f64 * 0.02
        } else {
            0.0
        };
        let confidence = (0.92 - penalty).max(0.80);

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence,
            merged_content,
            explanation: format!(
                "Composed {} non-overlapping definitions from both sides (left: [{}], right: [{}])",
                total_defs,
                left_names.join(", "),
                right_names.join(", "),
            ),
            warnings: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::test_utils::helpers;
    use crate::convergence::types::*;

    /// Class definition with span (0,3) — matches this pattern's test expectations.
    fn def_unit(name: &str, content: &str) -> StructuralUnit {
        helpers::def_unit_with_span(name, content, "class", (0, 3))
    }

    #[test]
    fn test_compose_non_overlapping_classes() {
        let pattern = NonOverlappingDefinitions;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![def_unit("User", "class User:\n    name: str")],
                right_units: vec![def_unit("Product", "class Product:\n    title: str")],
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

        assert!(pattern.matches(&conflict));
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("User"));
        assert!(proposal.merged_content.contains("Product"));
        // 2 definitions total (≤3): base confidence 0.92.
        assert!(
            (proposal.confidence - 0.92).abs() < f64::EPSILON,
            "expected 0.92, got {}",
            proposal.confidence
        );
    }

    #[test]
    fn test_name_collision_returns_none() {
        let pattern = NonOverlappingDefinitions;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![def_unit("User", "class User:\n    name: str")],
                right_units: vec![def_unit("User", "class User:\n    email: str")],
                base_span: (0, 0),
                left_span: (0, 3),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Definition],
                right_unit_kinds: vec![UnitKind::Definition],
                has_name_overlap: true,
                scope: ConflictScope::Definition,
            },
        };

        // has_name_overlap = true → matches() returns false.
        assert!(!pattern.matches(&conflict));
    }

    #[test]
    fn test_compose_functions_and_classes() {
        let pattern = NonOverlappingDefinitions;
        let mut func = StructuralUnit::new(
            UnitKind::Definition,
            Some("process".into()),
            (0, 3),
            "def process():\n    pass".into(),
        );
        func.metadata.insert("def_kind".into(), "function".into());

        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![func],
                right_units: vec![def_unit("Config", "class Config:\n    debug = True")],
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

        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("process"));
        assert!(proposal.merged_content.contains("Config"));
    }

    #[test]
    fn test_confidence_scales_with_definition_count() {
        let pattern = NonOverlappingDefinitions;
        // 2 defs: base 0.92
        let small = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![def_unit("A", "class A:\n    pass")],
                right_units: vec![def_unit("B", "class B:\n    pass")],
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
        let small_prop = pattern.resolve(&small).unwrap();
        assert!(
            (small_prop.confidence - 0.92).abs() < f64::EPSILON,
            "2 defs should be 0.92: {}",
            small_prop.confidence
        );

        // 6 defs: 0.92 - (6-3)*0.02 = 0.86
        let large = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![
                    def_unit("A", "class A:\n    pass"),
                    def_unit("B", "class B:\n    pass"),
                    def_unit("C", "class C:\n    pass"),
                ],
                right_units: vec![
                    def_unit("D", "class D:\n    pass"),
                    def_unit("E", "class E:\n    pass"),
                    def_unit("F", "class F:\n    pass"),
                ],
                base_span: (0, 0),
                left_span: (0, 9),
                right_span: (0, 9),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Definition; 3],
                right_unit_kinds: vec![UnitKind::Definition; 3],
                has_name_overlap: false,
                scope: ConflictScope::Definition,
            },
        };
        let large_prop = pattern.resolve(&large).unwrap();
        assert!(
            (large_prop.confidence - 0.86).abs() < f64::EPSILON,
            "6 defs should be 0.86: {}",
            large_prop.confidence
        );
        assert!(large_prop.confidence >= 0.80, "floor is 0.80");
    }

    #[test]
    fn test_no_definitions_does_not_match() {
        let pattern = NonOverlappingDefinitions;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![StructuralUnit::new(
                    UnitKind::Statement,
                    None,
                    (0, 1),
                    "x = 1".into(),
                )],
                right_units: vec![StructuralUnit::new(
                    UnitKind::Statement,
                    None,
                    (0, 1),
                    "y = 2".into(),
                )],
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Statement],
                right_unit_kinds: vec![UnitKind::Statement],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        assert!(!pattern.matches(&conflict));
    }
}
