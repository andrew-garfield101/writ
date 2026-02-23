//! End-of-File Append pattern.
//!
//! When both sides preserved the entire base file and each appended
//! new content at the end, compose: base content + left append + right
//! append (ordered by which side has fewer additions first, to produce
//! deterministic output).

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal};

/// Compose when both sides append to end of file.
pub struct EofAppend;

impl Pattern for EofAppend {
    fn name(&self) -> &str {
        "eof_append"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothModified]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        // Need base content to verify it was preserved.
        !conflict.region.base_units.is_empty()
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let base_count = conflict.region.base_units.len();

        // Both sides must start with all base content (preserved as prefix).
        if conflict.region.left_units.len() < base_count
            || conflict.region.right_units.len() < base_count
        {
            return None;
        }

        // Check that both sides' first N units match the base.
        for (i, base_unit) in conflict.region.base_units.iter().enumerate() {
            let left_match = conflict
                .region
                .left_units
                .get(i)
                .map_or(false, |u| u.content.trim() == base_unit.content.trim());
            let right_match = conflict
                .region
                .right_units
                .get(i)
                .map_or(false, |u| u.content.trim() == base_unit.content.trim());

            if !left_match || !right_match {
                return None;
            }
        }

        // Extract appended content from each side.
        let left_appended: Vec<&str> = conflict.region.left_units[base_count..]
            .iter()
            .map(|u| u.content.as_str())
            .collect();
        let right_appended: Vec<&str> = conflict.region.right_units[base_count..]
            .iter()
            .map(|u| u.content.as_str())
            .collect();

        // At least one side must have appended something.
        if left_appended.is_empty() && right_appended.is_empty() {
            return None;
        }

        // Compose: base + left append + right append.
        let mut parts: Vec<String> = conflict
            .region
            .base_units
            .iter()
            .map(|u| u.content.clone())
            .collect();

        parts.extend(left_appended.iter().map(|s| s.to_string()));
        parts.extend(right_appended.iter().map(|s| s.to_string()));

        let merged_content = parts.join("\n");

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence: 0.90,
            merged_content,
            explanation: format!(
                "Both sides preserved base and appended content (left: {} lines, right: {} lines)",
                left_appended.len(),
                right_appended.len(),
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

    fn make_conflict(
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: base,
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
    fn test_both_append() {
        let pattern = EofAppend;
        let conflict = make_conflict(
            vec![unit("base1"), unit("base2")],
            vec![unit("base1"), unit("base2"), unit("left_new")],
            vec![unit("base1"), unit("base2"), unit("right_new")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("base1"));
        assert!(proposal.merged_content.contains("base2"));
        assert!(proposal.merged_content.contains("left_new"));
        assert!(proposal.merged_content.contains("right_new"));
        assert!((proposal.confidence - 0.90).abs() < f64::EPSILON);
    }

    #[test]
    fn test_left_only_append() {
        let pattern = EofAppend;
        let conflict = make_conflict(
            vec![unit("base")],
            vec![unit("base"), unit("left_new")],
            vec![unit("base")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("left_new"));
    }

    #[test]
    fn test_base_modified_returns_none() {
        let pattern = EofAppend;
        let conflict = make_conflict(
            vec![unit("base1"), unit("base2")],
            vec![unit("modified"), unit("base2"), unit("left_new")],
            vec![unit("base1"), unit("base2"), unit("right_new")],
        );
        // Left side changed base1 → modified — not an EOF append.
        assert!(pattern.resolve(&conflict).is_none());
    }

    #[test]
    fn test_empty_base_does_not_match() {
        let pattern = EofAppend;
        let conflict = make_conflict(vec![], vec![unit("left")], vec![unit("right")]);
        assert!(!pattern.matches(&conflict));
    }
}
