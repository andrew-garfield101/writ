//! Property-based tests for the convergence pipeline.
//!
//! These tests use `proptest` to generate random inputs and verify universal
//! invariants that must hold for ALL convergence inputs.
//!
//! Invariants implemented:
//! 1. **No Panic** — patterns never panic on any input combination
//! 2. **Confidence Bounds** — confidence is always in [0.0, 1.0]
//!
//! Deferred:
//! - **Content Provenance** — "every structural unit in merged output
//!   originated from at least one input." Deferred until the structural unit
//!   comparison logic is well-defined (substring matching produces false
//!   positives on valid import accumulation merges).

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::convergence::patterns::PatternRegistry;
    use crate::convergence::test_utils::helpers;
    use crate::convergence::types::*;

    // ── Arbitrary generators ───────────────────────────────────────

    /// Generate an arbitrary UnitKind.
    fn arb_unit_kind() -> impl Strategy<Value = UnitKind> {
        prop_oneof![
            Just(UnitKind::Import),
            Just(UnitKind::Definition),
            Just(UnitKind::Statement),
            Just(UnitKind::Block),
            Just(UnitKind::Comment),
            Just(UnitKind::Whitespace),
            Just(UnitKind::Unknown),
        ]
    }

    /// Generate an arbitrary StructuralUnit with random kind and content.
    fn arb_unit() -> impl Strategy<Value = StructuralUnit> {
        (arb_unit_kind(), "[a-zA-Z0-9_ ]{0,80}")
            .prop_map(|(kind, content)| StructuralUnit::new(kind, None, (0, 1), content))
    }

    /// Generate an arbitrary named StructuralUnit.
    fn arb_named_unit() -> impl Strategy<Value = StructuralUnit> {
        (arb_unit_kind(), "[a-z_]{1,20}", "[a-zA-Z0-9_ ]{0,80}").prop_map(
            |(kind, name, content)| StructuralUnit::new(kind, Some(name), (0, 1), content),
        )
    }

    /// Generate a unit that might or might not have a name.
    fn arb_any_unit() -> impl Strategy<Value = StructuralUnit> {
        prop_oneof![arb_unit(), arb_named_unit(),]
    }

    /// Generate an arbitrary ConflictType.
    fn arb_conflict_type() -> impl Strategy<Value = ConflictType> {
        prop_oneof![
            Just(ConflictType::BothModified),
            Just(ConflictType::BothInserted),
            Just(ConflictType::LeftOnly),
            Just(ConflictType::RightOnly),
            Just(ConflictType::DeleteVsModify),
            Just(ConflictType::BothDeleted),
            Just(ConflictType::Clean),
        ]
    }

    /// Generate an arbitrary ConflictScope.
    fn arb_scope() -> impl Strategy<Value = ConflictScope> {
        prop_oneof![
            Just(ConflictScope::Import),
            Just(ConflictScope::Definition),
            Just(ConflictScope::IntraFunction),
            Just(ConflictScope::Mixed),
        ]
    }

    /// Generate an arbitrary ClassifiedConflict.
    fn arb_conflict() -> impl Strategy<Value = ClassifiedConflict> {
        (
            arb_conflict_type(),
            prop::collection::vec(arb_any_unit(), 0..8),
            prop::collection::vec(arb_any_unit(), 0..8),
            prop::collection::vec(arb_any_unit(), 0..8),
            arb_scope(),
        )
            .prop_map(|(ct, base, left, right, scope)| {
                helpers::make_full_conflict(ct, base, left, right, scope)
            })
    }

    // ── Invariant 1: Patterns Never Panic ──────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn pattern_never_panics(conflict in arb_conflict()) {
            let registry = PatternRegistry::new();
            // Must not panic — result can be any variant, but no panic.
            let _ = registry.evaluate(&conflict);
        }
    }

    // ── Invariant 2: Confidence Always in [0.0, 1.0] ──────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn confidence_always_valid(conflict in arb_conflict()) {
            use crate::convergence::patterns::PatternResult;

            let registry = PatternRegistry::new();
            let result = registry.evaluate(&conflict);
            let maybe_proposal = match &result {
                PatternResult::AutoResolved(p) => Some(p),
                PatternResult::Suggested(p) => Some(p),
                PatternResult::NoMatch => None,
            };
            if let Some(proposal) = maybe_proposal {
                prop_assert!(
                    proposal.confidence >= 0.0,
                    "Confidence below 0: {}",
                    proposal.confidence
                );
                prop_assert!(
                    proposal.confidence <= 1.0,
                    "Confidence above 1: {}",
                    proposal.confidence
                );
            }
        }
    }

    // ── Invariant 3: diff3 Never Panics ────────────────────────────

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn diff3_never_panics(
            base in "[ -~\n]{0,200}",
            left in "[ -~\n]{0,200}",
            right in "[ -~\n]{0,200}",
        ) {
            let _ = crate::convergence::three_way_merge(&base, &left, &right);
        }
    }
}
