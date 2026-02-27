//! Shared test helpers for the convergence pipeline.
//!
//! Centralizes unit constructors, conflict builders, and assertion helpers
//! used across phase, pattern, and decomposition test modules.
//! Only compiled in test mode.

#[cfg(test)]
pub mod helpers {
    use crate::convergence::types::*;

    // ── Unit Constructors ──────────────────────────────────────────

    /// Generic content unit with no semantic meaning.
    pub fn unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Unknown, None, (0, 1), content.into())
    }

    /// Unit with explicit kind (for phase2/phase4 style tests).
    pub fn typed_unit(kind: UnitKind, content: &str) -> StructuralUnit {
        StructuralUnit::new(kind, None, (0, 1), content.into())
    }

    /// Unit with kind and name (for definition/import tests).
    pub fn named_unit(kind: UnitKind, name: &str, content: &str) -> StructuralUnit {
        StructuralUnit::new(kind, Some(name.into()), (0, 1), content.into())
    }

    /// Unit with kind, optional name, and content (most flexible).
    pub fn make_unit(kind: UnitKind, name: Option<&str>, content: &str) -> StructuralUnit {
        StructuralUnit::new(kind, name.map(|s| s.to_string()), (0, 1), content.into())
    }

    /// Import unit without a name.
    pub fn import_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Import, None, (0, 1), content.into())
    }

    /// Import unit with a name (e.g. module name).
    pub fn named_import(name: &str, content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Import, Some(name.into()), (0, 1), content.into())
    }

    /// Import unit with metadata (language, module, names).
    pub fn import_with_meta(
        name: &str,
        content: &str,
        lang: &str,
        module: &str,
        names: &str,
    ) -> StructuralUnit {
        let mut u = named_import(name, content);
        u.metadata.insert("import_lang".into(), lang.into());
        u.metadata.insert("import_module".into(), module.into());
        if !names.is_empty() {
            u.metadata.insert("import_names".into(), names.into());
        }
        u
    }

    /// Definition unit with default span (0, 1) and "function" def_kind.
    pub fn def_unit(name: &str, content: &str) -> StructuralUnit {
        let mut u = StructuralUnit::new(
            UnitKind::Definition,
            Some(name.into()),
            (0, 1),
            content.into(),
        );
        u.metadata.insert("def_kind".into(), "function".into());
        u
    }

    /// Definition unit with explicit def_kind (class, struct, trait, etc.).
    pub fn def_unit_typed(name: &str, content: &str, kind: &str) -> StructuralUnit {
        let mut u = StructuralUnit::new(
            UnitKind::Definition,
            Some(name.into()),
            (0, 1),
            content.into(),
        );
        u.metadata.insert("def_kind".into(), kind.into());
        u
    }

    /// Definition unit with explicit span and def_kind.
    pub fn def_unit_with_span(
        name: &str,
        content: &str,
        kind: &str,
        span: (usize, usize),
    ) -> StructuralUnit {
        let mut u = StructuralUnit::new(
            UnitKind::Definition,
            Some(name.into()),
            span,
            content.into(),
        );
        u.metadata.insert("def_kind".into(), kind.into());
        u
    }

    /// Statement unit.
    pub fn stmt_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Statement, None, (0, 1), content.into())
    }

    /// Whitespace unit.
    pub fn ws_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Whitespace, None, (0, 1), content.into())
    }

    // ── Conflict Region Constructor ────────────────────────────────

    /// Build a bare StructuralConflictRegion (for phase2-level tests).
    pub fn make_region(
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> StructuralConflictRegion {
        StructuralConflictRegion {
            base_units: base,
            left_units: left,
            right_units: right,
            base_span: (0, 0),
            left_span: (0, 0),
            right_span: (0, 0),
        }
    }

    // ── Conflict Constructors ──────────────────────────────────────

    /// Two-sided conflict with empty base, BothModified type, Mixed scope.
    /// The simplest conflict for pattern tests.
    pub fn make_conflict(
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: make_region(vec![], left, right),
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

    /// Three-sided conflict with explicit base, BothModified type, Mixed scope.
    pub fn make_conflict_with_base(
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: make_region(base, left, right),
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

    /// Import-only conflict (BothInserted, Import scope).
    pub fn make_import_conflict(
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            region: make_region(vec![], left, right),
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

    /// Full-control conflict builder with explicit type, base, left, right.
    /// Sets scope to Mixed and computes has_name_overlap from definition names.
    pub fn make_typed_conflict(
        conflict_type: ConflictType,
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        let region = make_region(base, left, right);
        let (left_defs, right_defs) = region.definition_names();
        let has_name_overlap = left_defs.iter().any(|n| right_defs.contains(n));
        ClassifiedConflict {
            region,
            conflict_type,
            requires_review: conflict_type.always_requires_review(),
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap,
                scope: ConflictScope::Mixed,
            },
        }
    }

    /// Conflict with explicit type, scope, and name overlap control.
    pub fn make_full_conflict(
        conflict_type: ConflictType,
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
        scope: ConflictScope,
    ) -> ClassifiedConflict {
        let region = make_region(base, left, right);
        let (left_defs, right_defs) = region.definition_names();
        let has_name_overlap = left_defs.iter().any(|n| right_defs.contains(n));
        let left_unit_kinds: Vec<UnitKind> =
            region.left_units.iter().map(|u| u.kind.clone()).collect();
        let right_unit_kinds: Vec<UnitKind> =
            region.right_units.iter().map(|u| u.kind.clone()).collect();
        ClassifiedConflict {
            region,
            conflict_type,
            requires_review: conflict_type.always_requires_review(),
            structural_info: StructuralInfo {
                left_unit_kinds,
                right_unit_kinds,
                has_name_overlap,
                scope,
            },
        }
    }

    // ── Content Helpers ────────────────────────────────────────────

    /// Join structural units into a single string (newline-separated).
    pub fn units_to_content(units: &[StructuralUnit]) -> String {
        units
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<&str>>()
            .join("\n")
    }

    /// Join structural units into a display string, returning "(empty)" for empty input.
    pub fn units_to_text(units: &[StructuralUnit]) -> String {
        if units.is_empty() {
            return "(empty)".to_string();
        }
        units
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ── Assertion Helpers ──────────────────────────────────────────

    /// Assert confidence is within expected range.
    pub fn assert_confidence(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() < tolerance,
            "Confidence {actual} not within {tolerance} of {expected}"
        );
    }

    /// Assert merged content contains all expected definition names.
    pub fn assert_definitions_preserved(merged: &str, names: &[&str]) {
        for name in names {
            assert!(
                merged.contains(name),
                "Definition '{name}' missing from merged output:\n{merged}"
            );
        }
    }

    /// Assert a resolution proposal was produced with confidence above threshold.
    pub fn assert_resolved_above(proposals: &[ResolutionProposal], threshold: f64) {
        assert!(
            !proposals.is_empty(),
            "Expected at least one resolution proposal, got none"
        );
        let best = proposals
            .iter()
            .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap())
            .unwrap();
        assert!(
            best.confidence >= threshold,
            "Best confidence {:.3} is below threshold {threshold}",
            best.confidence
        );
    }

    /// Assert no proposals were produced (pattern should not match).
    pub fn assert_no_resolution(proposals: &[ResolutionProposal]) {
        assert!(
            proposals.is_empty(),
            "Expected no resolution proposals, got {}: {:?}",
            proposals.len(),
            proposals
                .iter()
                .map(|p| &p.pattern_name)
                .collect::<Vec<_>>()
        );
    }
}
