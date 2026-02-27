//! Phase 2: Classification Engine.
//!
//! Takes the structural diff output from Phase 1 and classifies each
//! conflict region into a [`ConflictType`] with [`StructuralInfo`].
//! The classified conflicts are then ready for Phase 3's pattern matching.

use super::analyzers::{self, LanguageAnalyzer};
use super::types::{
    ClassifiedConflict, ConflictScope, ConflictType, Phase2Result, StructuralConflictRegion,
    StructuralDiff, StructuralInfo, UnitKind,
};

/// Run Phase 2: classify all conflict regions from a Phase 1 structural diff.
pub fn run(diff: &StructuralDiff) -> Phase2Result {
    let analyzer = analyzers::analyzer_for_path(&diff.file_path);
    let classified_conflicts = diff
        .regions
        .iter()
        .map(|region| classify_region(region, analyzer.as_ref()))
        .collect();

    Phase2Result {
        file_path: diff.file_path.clone(),
        analyzer_used: diff.analyzer_used.clone(),
        classified_conflicts,
    }
}

/// Classify a single conflict region.
///
/// Determines the [`ConflictType`] by comparing base, left, and right
/// content using the analyzer's semantic equivalence. Derives
/// [`StructuralInfo`] from the structural units.
pub fn classify_region(
    region: &StructuralConflictRegion,
    analyzer: &dyn LanguageAnalyzer,
) -> ClassifiedConflict {
    let conflict_type = determine_conflict_type(region, analyzer);
    let structural_info = derive_structural_info(region);

    ClassifiedConflict {
        region: region.clone(),
        conflict_type,
        requires_review: conflict_type.always_requires_review(),
        structural_info,
    }
}

/// Determine the conflict type by comparing the three sides.
fn determine_conflict_type(
    region: &StructuralConflictRegion,
    analyzer: &dyn LanguageAnalyzer,
) -> ConflictType {
    let base_empty = region.base_units.is_empty();
    let left_empty = region.left_units.is_empty();
    let right_empty = region.right_units.is_empty();

    // All empty — trivially clean.
    if base_empty && left_empty && right_empty {
        return ConflictType::Clean;
    }

    // Both sides deleted base content.
    if !base_empty && left_empty && right_empty {
        return ConflictType::BothDeleted;
    }

    // One side deleted, other modified (or base unchanged).
    // Check deletion cases before semantic equivalence to avoid
    // empty-string comparisons masking real deletions.
    if !base_empty && left_empty && !right_empty {
        return ConflictType::DeleteVsModify;
    }
    if !base_empty && !left_empty && right_empty {
        return ConflictType::DeleteVsModify;
    }

    let left_content = units_to_content(&region.left_units);
    let right_content = units_to_content(&region.right_units);
    let base_content = units_to_content(&region.base_units);

    // Both sides made identical changes.
    if analyzer.are_semantically_equivalent(&left_content, &right_content) {
        return ConflictType::Clean;
    }

    // Only right changed (base == left).
    if !base_empty && analyzer.are_semantically_equivalent(&base_content, &left_content) {
        return ConflictType::RightOnly;
    }

    // Only left changed (base == right).
    if !base_empty && analyzer.are_semantically_equivalent(&base_content, &right_content) {
        return ConflictType::LeftOnly;
    }

    // Base empty — both sides inserted.
    if base_empty {
        return ConflictType::BothInserted;
    }

    // Both sides changed base content differently.
    ConflictType::BothModified
}

/// Derive structural information from a conflict region's units.
fn derive_structural_info(region: &StructuralConflictRegion) -> StructuralInfo {
    let left_unit_kinds: Vec<UnitKind> = region
        .left_units
        .iter()
        .map(|u| u.kind.clone())
        .filter(|k| !matches!(k, UnitKind::Whitespace))
        .collect();

    let right_unit_kinds: Vec<UnitKind> = region
        .right_units
        .iter()
        .map(|u| u.kind.clone())
        .filter(|k| !matches!(k, UnitKind::Whitespace))
        .collect();

    // Check for definition name overlap.
    let (left_def_names, right_def_names) = region.definition_names();
    let has_name_overlap = left_def_names
        .iter()
        .any(|name| right_def_names.contains(name));

    // Determine scope from unit kinds.
    let scope = ConflictScope::from_unit_kinds(&left_unit_kinds, &right_unit_kinds);

    StructuralInfo {
        left_unit_kinds,
        right_unit_kinds,
        has_name_overlap,
        scope,
    }
}

/// Join structural unit contents into a single string for comparison.
fn units_to_content(units: &[super::types::StructuralUnit]) -> String {
    units
        .iter()
        .map(|u| u.content.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::analyzers::generic::GenericAnalyzer;
    use crate::convergence::test_utils::helpers::{make_region, named_unit, typed_unit as unit};

    #[test]
    fn test_classify_clean_identical_changes() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "original")],
            vec![unit(UnitKind::Unknown, "changed")],
            vec![unit(UnitKind::Unknown, "changed")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::Clean);
    }

    #[test]
    fn test_classify_right_only() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "right changed")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::RightOnly);
    }

    #[test]
    fn test_classify_left_only() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left changed")],
            vec![unit(UnitKind::Unknown, "base")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::LeftOnly);
    }

    #[test]
    fn test_classify_both_inserted() {
        let region = make_region(
            vec![],
            vec![unit(UnitKind::Unknown, "left insert")],
            vec![unit(UnitKind::Unknown, "right insert")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::BothInserted);
    }

    #[test]
    fn test_classify_both_deleted() {
        let region = make_region(vec![unit(UnitKind::Unknown, "was here")], vec![], vec![]);
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::BothDeleted);
    }

    #[test]
    fn test_classify_delete_vs_modify_left_deleted() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "base content")],
            vec![],
            vec![unit(UnitKind::Unknown, "right modified")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::DeleteVsModify);
        assert!(result.requires_review);
    }

    #[test]
    fn test_classify_delete_vs_modify_right_deleted() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "base content")],
            vec![unit(UnitKind::Unknown, "left modified")],
            vec![],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::DeleteVsModify);
        assert!(result.requires_review);
    }

    #[test]
    fn test_classify_both_modified() {
        let region = make_region(
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left version")],
            vec![unit(UnitKind::Unknown, "right version")],
        );
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::BothModified);
    }

    #[test]
    fn test_structural_info_import_scope() {
        let region = make_region(
            vec![],
            vec![unit(UnitKind::Import, "import os")],
            vec![unit(UnitKind::Import, "import sys")],
        );
        let info = derive_structural_info(&region);
        assert_eq!(info.scope, ConflictScope::Import);
    }

    #[test]
    fn test_structural_info_definition_scope() {
        let region = make_region(
            vec![],
            vec![named_unit(UnitKind::Definition, "User", "class User: pass")],
            vec![named_unit(
                UnitKind::Definition,
                "Product",
                "class Product: pass",
            )],
        );
        let info = derive_structural_info(&region);
        assert_eq!(info.scope, ConflictScope::Definition);
    }

    #[test]
    fn test_structural_info_mixed_scope() {
        let region = make_region(
            vec![],
            vec![
                unit(UnitKind::Import, "import os"),
                named_unit(UnitKind::Definition, "main", "def main(): pass"),
            ],
            vec![unit(UnitKind::Import, "import sys")],
        );
        let info = derive_structural_info(&region);
        assert_eq!(info.scope, ConflictScope::Mixed);
    }

    #[test]
    fn test_structural_info_name_overlap() {
        let region = make_region(
            vec![],
            vec![named_unit(UnitKind::Definition, "User", "class User: ...")],
            vec![named_unit(UnitKind::Definition, "User", "class User: ...")],
        );
        let info = derive_structural_info(&region);
        assert!(info.has_name_overlap);
    }

    #[test]
    fn test_structural_info_no_name_overlap() {
        let region = make_region(
            vec![],
            vec![named_unit(UnitKind::Definition, "User", "class User: ...")],
            vec![named_unit(
                UnitKind::Definition,
                "Product",
                "class Product: ...",
            )],
        );
        let info = derive_structural_info(&region);
        assert!(!info.has_name_overlap);
    }

    #[test]
    fn test_semantic_equivalence_used() {
        // Python analyzer normalizes trailing whitespace per line and
        // collapses blank lines. Verify that classification uses semantic
        // equivalence rather than raw string comparison.
        use crate::convergence::analyzers::python::PythonAnalyzer;

        let region = make_region(
            vec![unit(UnitKind::Import, "original")],
            vec![unit(UnitKind::Unknown, "import os  ")], // trailing spaces
            vec![unit(UnitKind::Unknown, "import os")],
        );
        let result = classify_region(&region, &PythonAnalyzer);
        // Python semantic equivalence trims trailing whitespace.
        assert_eq!(
            result.conflict_type,
            ConflictType::Clean,
            "semantically equivalent content should classify as Clean"
        );
    }

    #[test]
    fn test_run_produces_phase2_result() {
        let diff = StructuralDiff {
            file_path: "test.py".into(),
            analyzer_used: "python".into(),
            regions: vec![make_region(
                vec![unit(UnitKind::Unknown, "base")],
                vec![unit(UnitKind::Unknown, "left")],
                vec![unit(UnitKind::Unknown, "right")],
            )],
        };
        let result = run(&diff);
        assert_eq!(result.file_path, "test.py");
        assert_eq!(result.classified_conflicts.len(), 1);
        assert_eq!(
            result.classified_conflicts[0].conflict_type,
            ConflictType::BothModified
        );
    }

    #[test]
    fn test_run_empty_diff() {
        let diff = StructuralDiff {
            file_path: "empty.py".into(),
            analyzer_used: "python".into(),
            regions: vec![],
        };
        let result = run(&diff);
        assert!(result.classified_conflicts.is_empty());
    }

    #[test]
    fn test_all_empty_is_clean() {
        let region = make_region(vec![], vec![], vec![]);
        let result = classify_region(&region, &GenericAnalyzer);
        assert_eq!(result.conflict_type, ConflictType::Clean);
    }

    #[test]
    fn test_intra_function_scope() {
        let region = make_region(
            vec![],
            vec![unit(UnitKind::Statement, "x = 1")],
            vec![unit(UnitKind::Statement, "x = 2")],
        );
        let info = derive_structural_info(&region);
        assert_eq!(info.scope, ConflictScope::IntraFunction);
    }
}
