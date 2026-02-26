//! Superset Containment pattern.
//!
//! When one side's content is a strict superset of the other's
//! (contains everything the other has, plus more), propose the
//! superset. In multi-agent workflows this is the common case:
//! a later agent built on an earlier agent's work.
//!
//! Dynamic confidence: base 0.82, penalized by -0.02 per unit
//! beyond 3 in the difference. Always a suggestion (below auto-resolve
//! threshold of 0.85) since the non-superset side may have
//! intentionally removed content.

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

        let (superset_side, subset_side, side_label, other_label) = if left_is_superset {
            (&conflict.region.left_units, &right_lines, "Left", "Right")
        } else if right_is_superset {
            (&conflict.region.right_units, &left_lines, "Right", "Left")
        } else {
            return None;
        };

        let content: String = superset_side
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n");

        // Dynamic confidence: base 0.82, penalized by -0.02 per extra unit
        // beyond 3. Floor at 0.75 to stay above suggest threshold.
        let diff_count = superset_side.len().saturating_sub(subset_side.len());
        let penalty = if diff_count > 3 {
            (diff_count - 3) as f64 * 0.02
        } else {
            0.0
        };
        let confidence = (0.82 - penalty).max(0.75);

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence,
            merged_content: content,
            explanation: format!("{side_label} side is a strict superset of {other_label} side"),
            warnings: vec![format!(
                "{other_label} side may have intentionally removed content — review recommended"
            )],
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
        // Base confidence 0.82 (diff=1, below penalty threshold of 3).
        assert!(
            (proposal.confidence - 0.82).abs() < f64::EPSILON,
            "expected 0.82, got {}",
            proposal.confidence
        );
        // Must be below auto_resolve (0.85) — this is a suggestion, not auto-applied.
        assert!(proposal.confidence < 0.85);
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

    #[test]
    fn test_superset_below_auto_resolve() {
        // SupersetContainment should ALWAYS be below auto_resolve (0.85)
        // because it always warns about potential intentional removal.
        let pattern = SupersetContainment;
        let conflict = make_conflict(
            vec![unit("a"), unit("b"), unit("c"), unit("d")],
            vec![unit("a"), unit("b"), unit("c")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(
            proposal.confidence < 0.85,
            "superset should be suggestion, not auto-resolve: {}",
            proposal.confidence
        );
    }

    #[test]
    fn test_confidence_scales_with_diff_size() {
        let pattern = SupersetContainment;
        // Small diff (1 extra): base 0.82
        let small = make_conflict(
            vec![unit("a"), unit("b"), unit("c")],
            vec![unit("a"), unit("b")],
        );
        let small_prop = pattern.resolve(&small).unwrap();

        // Large diff (7 extra): 0.82 - (7-3)*0.02 = 0.74, clamped to 0.75
        let large = make_conflict(
            vec![
                unit("a"),
                unit("b"),
                unit("c"),
                unit("d"),
                unit("e"),
                unit("f"),
                unit("g"),
                unit("h"),
                unit("i"),
            ],
            vec![unit("a"), unit("b")],
        );
        let large_prop = pattern.resolve(&large).unwrap();

        assert!(
            small_prop.confidence > large_prop.confidence,
            "larger diff should have lower confidence: small={}, large={}",
            small_prop.confidence,
            large_prop.confidence
        );
        assert!(
            large_prop.confidence >= 0.75,
            "confidence should never drop below 0.75: {}",
            large_prop.confidence
        );
    }
}
