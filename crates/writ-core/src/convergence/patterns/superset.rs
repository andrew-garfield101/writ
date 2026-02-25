//! Superset Containment pattern.
//!
//! When one side's content is a strict superset of the other's
//! (contains everything the other has, plus more), propose the
//! superset. In multi-agent workflows this is the common case:
//! a later agent built on an earlier agent's work. Confidence
//! is 0.88 (above auto-resolve) with a review warning.

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal};
use std::collections::HashSet;

/// Propose the superset when one side contains everything the other has.
pub struct SupersetContainment;

impl Pattern for SupersetContainment {
    fn name(&self) -> &str {
        "superset_containment"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothModified, ConflictType::BothInserted]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        // Both sides must have content.
        !conflict.region.left_units.is_empty() && !conflict.region.right_units.is_empty()
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let left_lines: HashSet<String> = conflict
            .region
            .left_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let right_lines: HashSet<String> = conflict
            .region
            .right_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let left_is_superset = right_lines.iter().all(|r| left_lines.contains(r))
            && left_lines.len() > right_lines.len();
        let right_is_superset = left_lines.iter().all(|l| right_lines.contains(l))
            && right_lines.len() > left_lines.len();

        if left_is_superset {
            let content: String = conflict
                .region
                .left_units
                .iter()
                .map(|u| u.content.as_str())
                .collect::<Vec<&str>>()
                .join("\n");
            Some(ResolutionProposal {
                pattern_name: self.name().into(),
                confidence: 0.88,
                merged_content: content,
                explanation: "Left side is a strict superset of right side".into(),
                warnings: vec![
                    "Right side may have intentionally removed content — review recommended".into(),
                ],
            })
        } else if right_is_superset {
            let content: String = conflict
                .region
                .right_units
                .iter()
                .map(|u| u.content.as_str())
                .collect::<Vec<&str>>()
                .join("\n");
            Some(ResolutionProposal {
                pattern_name: self.name().into(),
                confidence: 0.88,
                merged_content: content,
                explanation: "Right side is a strict superset of left side".into(),
                warnings: vec![
                    "Left side may have intentionally removed content — review recommended".into(),
                ],
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::*;

    fn unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Unknown, None, (0, 1), content.into())
    }

    fn make_conflict(left: Vec<StructuralUnit>, right: Vec<StructuralUnit>) -> ClassifiedConflict {
        ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: left,
                right_units: right,
                base_span: (0, 0),
                left_span: (0, 0),
                right_span: (0, 0),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        }
    }

    #[test]
    fn test_left_superset() {
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("line1"), unit("line2"), unit("line3")],
            vec![unit("line1"), unit("line2")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("line3"));
        assert!((proposal.confidence - 0.88).abs() < f64::EPSILON);
        assert!(proposal.explanation.contains("Left"));
    }

    #[test]
    fn test_right_superset() {
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("line1")],
            vec![unit("line1"), unit("line2"), unit("line3")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("line3"));
        assert!(proposal.explanation.contains("Right"));
    }

    #[test]
    fn test_neither_superset() {
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("line1"), unit("line2")],
            vec![unit("line1"), unit("line3")],
        );
        assert!(pattern.resolve(&conflict).is_none());
    }

    #[test]
    fn test_equal_sets_not_superset() {
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("line1"), unit("line2")],
            vec![unit("line1"), unit("line2")],
        );
        assert!(pattern.resolve(&conflict).is_none());
    }

    #[test]
    fn test_has_warning() {
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("a"), unit("b"), unit("c")],
            vec![unit("a"), unit("b")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(!proposal.warnings.is_empty());
    }
}
