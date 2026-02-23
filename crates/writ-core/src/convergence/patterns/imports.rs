//! Import Accumulation pattern.
//!
//! When both sides of a conflict add or modify import statements,
//! accumulate (union) all imports. Deduplicate exact matches, flag
//! conflicting imports (same name, different source) for review.

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal, UnitKind};
use std::collections::HashSet;

/// Union of all imports from both sides with deduplication.
pub struct ImportAccumulation;

impl Pattern for ImportAccumulation {
    fn name(&self) -> &str {
        "import_accumulation"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothInserted, ConflictType::BothModified]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        conflict.region.is_import_only()
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged_lines: Vec<String> = Vec::new();
        let mut has_conflicts = false;

        // Collect all import lines from both sides, deduplicating.
        for unit in conflict
            .region
            .left_units
            .iter()
            .chain(conflict.region.right_units.iter())
        {
            if unit.kind != UnitKind::Import {
                continue;
            }
            let normalized = unit.content.trim().to_string();
            if seen.insert(normalized.clone()) {
                merged_lines.push(unit.content.clone());
            }
        }

        // Check for conflicting imports (same name, different source).
        // This is a simplified check — name extracted from the import line.
        let mut name_to_source: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for line in &merged_lines {
            let trimmed = line.trim();
            // Extract imported names for conflict detection.
            if trimmed.starts_with("from ") {
                if let Some(after) = trimmed.splitn(2, " import ").nth(1) {
                    let module = trimmed[5..].split_whitespace().next().unwrap_or("");
                    for name in after.split(',') {
                        let name = name.trim().to_string();
                        if name != "*" && !name.is_empty() {
                            if let Some(prev_source) = name_to_source.get(&name) {
                                if prev_source != module {
                                    has_conflicts = true;
                                }
                            } else {
                                name_to_source.insert(name, module.to_string());
                            }
                        }
                    }
                }
            }
        }

        if merged_lines.is_empty() {
            return None;
        }

        let confidence = if has_conflicts { 0.60 } else { 0.95 };
        let merged_content = merged_lines.join("\n");

        let mut warnings = Vec::new();
        if has_conflicts {
            warnings.push(
                "Conflicting imports detected — same name imported from different modules"
                    .to_string(),
            );
        }

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence,
            merged_content,
            explanation: format!(
                "Union of {} import statements from both sides (deduplicated)",
                merged_lines.len()
            ),
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::*;

    fn import_unit(name: &str, content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Import, Some(name.into()), (0, 1), content.into())
    }

    fn make_import_conflict(
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: left,
                right_units: right,
                base_span: (0, 0),
                left_span: (0, 0),
                right_span: (0, 0),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Import],
                has_name_overlap: false,
                scope: ConflictScope::Import,
            },
        }
    }

    #[test]
    fn test_union_disjoint_imports() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_unit("os", "import os")],
            vec![import_unit("sys", "import sys")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("import os"));
        assert!(proposal.merged_content.contains("import sys"));
        assert!((proposal.confidence - 0.95).abs() < f64::EPSILON);
        assert!(proposal.warnings.is_empty());
    }

    #[test]
    fn test_deduplicates_identical_imports() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_unit("os", "import os")],
            vec![import_unit("os", "import os")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        // Should appear only once.
        let count = proposal.merged_content.matches("import os").count();
        assert_eq!(count, 1, "should deduplicate: {}", proposal.merged_content);
    }

    #[test]
    fn test_conflicting_imports_lowers_confidence() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_unit("models", "from auth.models import User")],
            vec![import_unit("models", "from core.models import User")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(
            (proposal.confidence - 0.60).abs() < f64::EPSILON,
            "conflicting imports should lower confidence to 0.60"
        );
        assert!(!proposal.warnings.is_empty());
    }

    #[test]
    fn test_does_not_match_mixed_content() {
        let pattern = ImportAccumulation;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![import_unit("os", "import os")],
                right_units: vec![StructuralUnit::new(
                    UnitKind::Definition,
                    Some("main".into()),
                    (0, 2),
                    "def main(): pass".into(),
                )],
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 2),
            },
            conflict_type: ConflictType::BothInserted,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Definition],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };
        assert!(!pattern.matches(&conflict));
    }
}
