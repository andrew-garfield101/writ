//! Region Decomposition — splits mixed conflict regions into homogeneous
//! sub-regions so each can be handled by a specialized pattern.
//!
//! When diff3 produces a single monolithic conflict region containing imports,
//! definitions, and statements, no single v2 pattern can handle it. This module
//! decomposes such regions into an **import sub-region** and a **body sub-region**,
//! each of which can be evaluated independently by the pattern registry.

use super::types::{
    ClassifiedConflict, ConflictScope, ConflictType, StructuralConflictRegion, StructuralInfo,
    StructuralUnit, UnitKind,
};

/// Result of decomposing a mixed conflict region.
#[derive(Debug, Clone)]
pub struct DecomposedRegion {
    /// Import-only sub-conflict (None if no imports on any side).
    pub import_conflict: Option<ClassifiedConflict>,
    /// Body sub-conflict — everything that isn't imports (None if only imports).
    pub body_conflict: Option<ClassifiedConflict>,
}

/// Attempt to decompose a mixed-scope conflict region into homogeneous
/// sub-regions (imports vs. body). Returns `None` if decomposition is
/// not applicable or wouldn't be useful.
pub fn decompose_mixed_region(conflict: &ClassifiedConflict) -> Option<DecomposedRegion> {
    // Only decompose Mixed-scope regions.
    if conflict.structural_info.scope != ConflictScope::Mixed {
        return None;
    }

    let region = &conflict.region;

    // Partition each side into imports and body.
    let (base_imports, base_body) = partition_units(&region.base_units);
    let (left_imports, left_body) = partition_units(&region.left_units);
    let (right_imports, right_body) = partition_units(&region.right_units);

    let has_imports =
        !base_imports.is_empty() || !left_imports.is_empty() || !right_imports.is_empty();
    let has_body = !base_body.is_empty() || !left_body.is_empty() || !right_body.is_empty();

    // Decomposition is only useful if we have BOTH imports and body.
    // If only one, the original region evaluation handles it fine.
    if !has_imports || !has_body {
        return None;
    }

    let import_conflict = build_sub_conflict(
        &base_imports,
        &left_imports,
        &right_imports,
        &region.base_span,
        conflict.conflict_type,
    );

    let body_conflict = build_sub_conflict(
        &base_body,
        &left_body,
        &right_body,
        &region.base_span,
        conflict.conflict_type,
    );

    Some(DecomposedRegion {
        import_conflict: if has_imports {
            Some(import_conflict)
        } else {
            None
        },
        body_conflict: if has_body { Some(body_conflict) } else { None },
    })
}

/// Partition structural units into imports and body (everything else).
///
/// Import units go to the imports bucket. Whitespace/Comment units that
/// are adjacent to (immediately before or after) an import unit also go
/// to the imports bucket to preserve formatting. Everything else goes
/// to body.
fn partition_units(units: &[StructuralUnit]) -> (Vec<StructuralUnit>, Vec<StructuralUnit>) {
    if units.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // First pass: mark which units are imports.
    let is_import: Vec<bool> = units.iter().map(|u| u.kind == UnitKind::Import).collect();

    // Second pass: mark whitespace/comments adjacent to imports as import-associated.
    let mut in_import_section: Vec<bool> = is_import.clone();
    for i in 0..units.len() {
        if matches!(units[i].kind, UnitKind::Whitespace | UnitKind::Comment) {
            // Check if previous or next non-whitespace/comment unit is an import.
            let prev_is_import = (0..i)
                .rev()
                .find(|&j| !matches!(units[j].kind, UnitKind::Whitespace | UnitKind::Comment))
                .map_or(false, |j| is_import[j]);

            let next_is_import = ((i + 1)..units.len())
                .find(|&j| !matches!(units[j].kind, UnitKind::Whitespace | UnitKind::Comment))
                .map_or(false, |j| is_import[j]);

            // Only associate with imports if BOTH neighbors are imports,
            // or if we're at the start/end of the file adjacent to imports.
            // This is conservative — whitespace between imports and body
            // goes to body to avoid pulling non-import context into the
            // import sub-region.
            if prev_is_import && next_is_import {
                in_import_section[i] = true;
            } else if prev_is_import
                && (i + 1..units.len())
                    .all(|j| matches!(units[j].kind, UnitKind::Whitespace | UnitKind::Comment))
            {
                // Trailing whitespace after last import — keep with imports.
                in_import_section[i] = true;
            }
        }
    }

    let mut imports = Vec::new();
    let mut body = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        if in_import_section[i] {
            imports.push(unit.clone());
        } else {
            body.push(unit.clone());
        }
    }

    (imports, body)
}

/// Build a ClassifiedConflict from sub-region units.
fn build_sub_conflict(
    base_units: &[StructuralUnit],
    left_units: &[StructuralUnit],
    right_units: &[StructuralUnit],
    base_span: &(usize, usize),
    parent_conflict_type: ConflictType,
) -> ClassifiedConflict {
    let region = StructuralConflictRegion {
        base_units: base_units.to_vec(),
        left_units: left_units.to_vec(),
        right_units: right_units.to_vec(),
        base_span: *base_span,
        left_span: (0, left_units.len()),
        right_span: (0, right_units.len()),
    };

    // Derive structural info from the sub-region's units.
    let left_unit_kinds: Vec<UnitKind> = left_units
        .iter()
        .map(|u| u.kind.clone())
        .filter(|k| !matches!(k, UnitKind::Whitespace))
        .collect();
    let right_unit_kinds: Vec<UnitKind> = right_units
        .iter()
        .map(|u| u.kind.clone())
        .filter(|k| !matches!(k, UnitKind::Whitespace))
        .collect();

    let (left_def_names, right_def_names) = region.definition_names();
    let has_name_overlap = left_def_names
        .iter()
        .any(|name| right_def_names.contains(name));

    let scope = ConflictScope::from_unit_kinds(&left_unit_kinds, &right_unit_kinds);

    let structural_info = StructuralInfo {
        left_unit_kinds,
        right_unit_kinds,
        has_name_overlap,
        scope,
    };

    ClassifiedConflict {
        region,
        conflict_type: parent_conflict_type,
        requires_review: false,
        structural_info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Import, None, (0, 1), content.into())
    }

    fn def_unit(name: &str, content: &str) -> StructuralUnit {
        StructuralUnit::new(
            UnitKind::Definition,
            Some(name.into()),
            (0, 1),
            content.into(),
        )
    }

    fn stmt_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Statement, None, (0, 1), content.into())
    }

    fn ws_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Whitespace, None, (0, 1), content.into())
    }

    fn make_conflict(
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
        scope: ConflictScope,
        conflict_type: ConflictType,
    ) -> ClassifiedConflict {
        let region = StructuralConflictRegion {
            base_units: base,
            left_units: left,
            right_units: right,
            base_span: (0, 0),
            left_span: (0, 0),
            right_span: (0, 0),
        };
        let (left_def_names, right_def_names) = region.definition_names();
        let has_name_overlap = left_def_names
            .iter()
            .any(|name| right_def_names.contains(name));
        ClassifiedConflict {
            region,
            conflict_type,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap,
                scope,
            },
        }
    }

    // ── Decomposition logic tests ──────────────────────────────────────

    #[test]
    fn test_decompose_mixed_region_splits_imports_from_body() {
        let conflict = make_conflict(
            vec![import_unit("import os"), stmt_unit("x = 1")],
            vec![
                import_unit("import os"),
                import_unit("import sys"),
                stmt_unit("x = 1"),
                def_unit("foo", "def foo(): pass"),
            ],
            vec![
                import_unit("import os"),
                import_unit("import json"),
                stmt_unit("x = 1"),
                def_unit("bar", "def bar(): pass"),
            ],
            ConflictScope::Mixed,
            ConflictType::BothModified,
        );

        let result = decompose_mixed_region(&conflict).unwrap();

        // Import sub-conflict should have import units only.
        let import_c = result.import_conflict.unwrap();
        assert!(import_c.region.left_units.iter().all(|u| matches!(
            u.kind,
            UnitKind::Import | UnitKind::Whitespace | UnitKind::Comment
        )));
        assert_eq!(import_c.structural_info.scope, ConflictScope::Import);

        // Body sub-conflict should have non-import units.
        let body_c = result.body_conflict.unwrap();
        assert!(body_c
            .region
            .left_units
            .iter()
            .all(|u| !matches!(u.kind, UnitKind::Import)));
    }

    #[test]
    fn test_decompose_non_mixed_returns_none() {
        // Import-only scope — no decomposition needed.
        let conflict = make_conflict(
            vec![],
            vec![import_unit("import os")],
            vec![import_unit("import sys")],
            ConflictScope::Import,
            ConflictType::BothInserted,
        );
        assert!(decompose_mixed_region(&conflict).is_none());
    }

    #[test]
    fn test_decompose_empty_imports_returns_none() {
        // Mixed scope but no actual import units — decomposition not useful.
        let conflict = make_conflict(
            vec![stmt_unit("x = 1")],
            vec![stmt_unit("x = 1"), def_unit("foo", "def foo(): pass")],
            vec![stmt_unit("x = 1"), def_unit("bar", "def bar(): pass")],
            ConflictScope::Mixed,
            ConflictType::BothModified,
        );
        // This has Statement + Definition = Mixed scope, but no Import units.
        assert!(decompose_mixed_region(&conflict).is_none());
    }

    #[test]
    fn test_decompose_preserves_conflict_type() {
        let conflict = make_conflict(
            vec![],
            vec![import_unit("import os"), def_unit("foo", "def foo(): pass")],
            vec![
                import_unit("import sys"),
                def_unit("bar", "def bar(): pass"),
            ],
            ConflictScope::Mixed,
            ConflictType::BothInserted,
        );

        let result = decompose_mixed_region(&conflict).unwrap();
        assert_eq!(
            result.import_conflict.unwrap().conflict_type,
            ConflictType::BothInserted
        );
        assert_eq!(
            result.body_conflict.unwrap().conflict_type,
            ConflictType::BothInserted
        );
    }

    #[test]
    fn test_decompose_whitespace_between_imports_stays_with_imports() {
        let conflict = make_conflict(
            vec![],
            vec![
                import_unit("import os"),
                ws_unit(""),
                import_unit("import sys"),
                ws_unit(""),
                def_unit("main", "def main(): pass"),
            ],
            vec![
                import_unit("import json"),
                def_unit("run", "def run(): pass"),
            ],
            ConflictScope::Mixed,
            ConflictType::BothInserted,
        );

        let result = decompose_mixed_region(&conflict).unwrap();
        let import_c = result.import_conflict.unwrap();
        // Whitespace between imports should be in the import sub-region.
        assert!(import_c.region.left_units.len() >= 3); // os, ws, sys
    }

    #[test]
    fn test_decompose_recomputes_body_scope() {
        let conflict = make_conflict(
            vec![],
            vec![import_unit("import os"), def_unit("foo", "def foo(): pass")],
            vec![
                import_unit("import sys"),
                def_unit("bar", "def bar(): pass"),
            ],
            ConflictScope::Mixed,
            ConflictType::BothInserted,
        );

        let result = decompose_mixed_region(&conflict).unwrap();
        let body_c = result.body_conflict.unwrap();
        // Body contains only definitions → scope should be Definition.
        assert_eq!(body_c.structural_info.scope, ConflictScope::Definition);
    }

    // ── Partition tests ────────────────────────────────────────────────

    #[test]
    fn test_partition_separates_imports_from_defs() {
        let units = vec![
            import_unit("import os"),
            import_unit("from flask import Flask"),
            stmt_unit("app = Flask(__name__)"),
            def_unit("index", "@app.route('/')"),
        ];
        let (imports, body) = partition_units(&units);
        assert_eq!(imports.len(), 2);
        assert_eq!(body.len(), 2);
        assert!(imports.iter().all(|u| u.kind == UnitKind::Import));
    }

    #[test]
    fn test_partition_empty_input() {
        let (imports, body) = partition_units(&[]);
        assert!(imports.is_empty());
        assert!(body.is_empty());
    }

    #[test]
    fn test_partition_all_imports() {
        let units = vec![import_unit("import os"), import_unit("import sys")];
        let (imports, body) = partition_units(&units);
        assert_eq!(imports.len(), 2);
        assert!(body.is_empty());
    }

    #[test]
    fn test_partition_no_imports() {
        let units = vec![stmt_unit("x = 1"), def_unit("foo", "def foo(): pass")];
        let (imports, body) = partition_units(&units);
        assert!(imports.is_empty());
        assert_eq!(body.len(), 2);
    }

    // ── Edge cases ─────────────────────────────────────────────────────

    #[test]
    fn test_decompose_both_inserted_mixed() {
        let conflict = make_conflict(
            vec![], // empty base = BothInserted
            vec![
                import_unit("from flask import Flask, jsonify"),
                stmt_unit("app = Flask(__name__)"),
                def_unit("register", "@app.route('/register')"),
            ],
            vec![
                import_unit("from flask import Flask, abort"),
                stmt_unit("app = Flask(__name__)"),
                def_unit("checkout", "@app.route('/checkout')"),
            ],
            ConflictScope::Mixed,
            ConflictType::BothInserted,
        );

        let result = decompose_mixed_region(&conflict).unwrap();
        assert!(result.import_conflict.is_some());
        assert!(result.body_conflict.is_some());

        let import_c = result.import_conflict.unwrap();
        assert_eq!(import_c.region.left_units.len(), 1); // just the import
        assert_eq!(import_c.region.right_units.len(), 1);

        let body_c = result.body_conflict.unwrap();
        assert_eq!(body_c.region.left_units.len(), 2); // stmt + def
        assert_eq!(body_c.region.right_units.len(), 2);
    }

    #[test]
    fn test_decompose_definition_only_scope_returns_none() {
        // Pure definition scope — not Mixed, so no decomposition.
        let conflict = make_conflict(
            vec![],
            vec![def_unit("foo", "def foo(): pass")],
            vec![def_unit("bar", "def bar(): pass")],
            ConflictScope::Definition,
            ConflictType::BothInserted,
        );
        assert!(decompose_mixed_region(&conflict).is_none());
    }
}
