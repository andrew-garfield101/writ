//! Import Accumulation pattern.
//!
//! When both sides of a conflict add or modify import statements,
//! accumulate (union) all imports. Deduplicate exact matches, flag
//! conflicting imports (same name, different source) for review.

use super::import_utils;
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
        let mut all_parsed: Vec<import_utils::ParsedImport> = Vec::new();

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
                all_parsed.push(import_utils::parse_import(unit));
            }
        }

        if merged_lines.is_empty() {
            return None;
        }

        // Language-aware conflict detection: same name from different modules.
        let name_conflicts = import_utils::detect_name_conflicts(&all_parsed);
        let has_conflicts = !name_conflicts.is_empty();

        // Dynamic confidence: base 0.95 (no conflicts) or 0.60 (conflicts),
        // penalized by -0.02 per import beyond 10 (complexity penalty).
        // Floor: 0.60 (conflict case stays at suggest threshold).
        let base_confidence = if has_conflicts { 0.60 } else { 0.95 };
        let import_count = merged_lines.len();
        let penalty = if import_count > 10 {
            (import_count - 10) as f64 * 0.02
        } else {
            0.0
        };
        let confidence = (base_confidence - penalty).max(0.60);
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

    /// Helper: create an import unit with metadata.
    fn import_with_meta(
        name: &str,
        content: &str,
        lang: &str,
        module: &str,
        names: &str,
    ) -> StructuralUnit {
        let mut unit = import_unit(name, content);
        unit.metadata.insert("import_lang".into(), lang.into());
        unit.metadata.insert("import_module".into(), module.into());
        if !names.is_empty() {
            unit.metadata.insert("import_names".into(), names.into());
        }
        unit
    }

    #[test]
    fn test_rust_use_dedup() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![
                import_with_meta("std::io", "use std::io;", "rust", "std::io", ""),
                import_with_meta(
                    "std::collections",
                    "use std::collections::HashMap;",
                    "rust",
                    "std::collections",
                    "HashMap",
                ),
            ],
            vec![
                import_with_meta("std::io", "use std::io;", "rust", "std::io", ""),
                import_with_meta(
                    "serde",
                    "use serde::{Serialize, Deserialize};",
                    "rust",
                    "serde",
                    "Deserialize, Serialize",
                ),
            ],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        // std::io should appear only once (deduplicated).
        let io_count = proposal.merged_content.matches("use std::io").count();
        assert_eq!(
            io_count, 1,
            "should deduplicate: {}",
            proposal.merged_content
        );
        assert!(proposal.merged_content.contains("HashMap"));
        assert!(proposal.merged_content.contains("Serialize"));
        assert!((proposal.confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_go_import_dedup() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_with_meta("fmt", "import \"fmt\"", "go", "fmt", "")],
            vec![import_with_meta("os", "import \"os\"", "go", "os", "")],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("\"fmt\""));
        assert!(proposal.merged_content.contains("\"os\""));
        assert!((proposal.confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_ts_named_import_dedup() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_with_meta(
                "react",
                "import { useState } from 'react';",
                "typescript",
                "react",
                "useState",
            )],
            vec![import_with_meta(
                "axios",
                "import axios from 'axios';",
                "typescript",
                "axios",
                "",
            )],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("useState"));
        assert!(proposal.merged_content.contains("axios"));
    }

    #[test]
    fn test_rust_conflict_detection() {
        let pattern = ImportAccumulation;
        let conflict = make_import_conflict(
            vec![import_with_meta(
                "serde",
                "use serde::Serialize;",
                "rust",
                "serde",
                "Serialize",
            )],
            vec![import_with_meta(
                "other_serde",
                "use other_serde::Serialize;",
                "rust",
                "other_serde",
                "Serialize",
            )],
        );
        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(
            (proposal.confidence - 0.60).abs() < f64::EPSILON,
            "same name from different modules should lower confidence"
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
