//! Phase 4: Spec-Aware Resolution.
//!
//! Uses writ's first-class spec and seal objects to resolve conflicts that
//! Phase 3's deterministic patterns couldn't handle confidently, or to
//! confirm/reject low-confidence Phase 3 suggestions.
//!
//! This is the phase that makes writ fundamentally different from every
//! other VCS — no other system knows *why* agents made their changes.
//!
//! ## Resolution Rules (tried in order)
//!
//! 1. **Scope Authority** (0.90) — if one spec owns this file and the other
//!    doesn't, prefer the in-scope agent's changes.
//! 2. **Delete Confirmation** (0.85) — for DeleteVsModify, check which
//!    spec explicitly references the file to determine intent.
//! 3. **Intent Compatibility** (0.85) — if both seals describe non-conflicting
//!    intents and Phase 3 suggested a composition, confirm it.
//! 4. **Spec Priority** (stubbed) — reserved for future priority ordering.

use super::pipeline::{SpecContext, SpecResolver};
use super::types::*;

/// Confidence scores for each resolution rule.
const SCOPE_AUTHORITY_CONFIDENCE: f64 = 0.90;
const DELETE_CONFIRMATION_CONFIDENCE: f64 = 0.85;
const INTENT_COMPATIBILITY_CONFIDENCE: f64 = 0.85;

/// The spec-aware resolver implementing Phase 4 of the convergence pipeline.
///
/// Applies resolution rules that leverage spec metadata (file scope,
/// acceptance criteria, seal summaries) to resolve ambiguous conflicts.
pub struct SpecAwareResolver;

impl SpecResolver for SpecAwareResolver {
    fn resolve(
        &self,
        conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        spec_context: &SpecContext,
    ) -> Option<ResolutionProposal> {
        self.try_scope_authority(conflict, spec_context)
            .or_else(|| self.try_delete_confirmation(conflict, spec_context))
            .or_else(|| self.try_intent_compatibility(conflict, suggestion, spec_context))
            .or_else(|| self.try_spec_priority(conflict, spec_context))
    }
}

impl SpecAwareResolver {
    /// Rule 1: Scope Authority.
    ///
    /// If one spec's `file_scope` includes this file and the other's doesn't,
    /// the in-scope agent's changes take priority.
    fn try_scope_authority(
        &self,
        conflict: &ClassifiedConflict,
        ctx: &SpecContext,
    ) -> Option<ResolutionProposal> {
        let left_owns = file_in_scope(&ctx.file_path, &ctx.left_file_scope);
        let right_owns = file_in_scope(&ctx.file_path, &ctx.right_file_scope);

        match (left_owns, right_owns) {
            (true, false) => {
                let content = units_to_content(&conflict.region.left_units);
                Some(ResolutionProposal {
                    pattern_name: "spec_scope_authority".to_string(),
                    confidence: SCOPE_AUTHORITY_CONFIDENCE,
                    merged_content: content,
                    explanation: format!(
                        "Left spec owns '{}' (in file_scope); right spec does not",
                        ctx.file_path
                    ),
                    warnings: vec![],
                })
            }
            (false, true) => {
                let content = units_to_content(&conflict.region.right_units);
                Some(ResolutionProposal {
                    pattern_name: "spec_scope_authority".to_string(),
                    confidence: SCOPE_AUTHORITY_CONFIDENCE,
                    merged_content: content,
                    explanation: format!(
                        "Right spec owns '{}' (in file_scope); left spec does not",
                        ctx.file_path
                    ),
                    warnings: vec![],
                })
            }
            // Both own it or neither owns it — can't determine authority.
            _ => None,
        }
    }

    /// Rule 2: Delete Confirmation.
    ///
    /// For DeleteVsModify conflicts, checks which spec explicitly references
    /// the file to determine whether deletion or modification was intentional.
    fn try_delete_confirmation(
        &self,
        conflict: &ClassifiedConflict,
        ctx: &SpecContext,
    ) -> Option<ResolutionProposal> {
        if conflict.conflict_type != ConflictType::DeleteVsModify {
            return None;
        }

        // Determine which side deleted and which modified.
        let left_deleted = conflict.region.left_units.is_empty();
        let right_deleted = conflict.region.right_units.is_empty();

        if !left_deleted && !right_deleted {
            return None;
        }

        let (deleter_refs, modifier_refs) = if left_deleted {
            // Left deleted, right modified.
            let del_refs = spec_references_file(
                &ctx.left_acceptance_criteria,
                &ctx.left_design_notes,
                &ctx.left_seal_summary,
                &ctx.file_path,
            );
            let mod_refs = spec_references_file(
                &ctx.right_acceptance_criteria,
                &ctx.right_design_notes,
                &ctx.right_seal_summary,
                &ctx.file_path,
            );
            (del_refs, mod_refs)
        } else {
            // Right deleted, left modified.
            let del_refs = spec_references_file(
                &ctx.right_acceptance_criteria,
                &ctx.right_design_notes,
                &ctx.right_seal_summary,
                &ctx.file_path,
            );
            let mod_refs = spec_references_file(
                &ctx.left_acceptance_criteria,
                &ctx.left_design_notes,
                &ctx.left_seal_summary,
                &ctx.file_path,
            );
            (del_refs, mod_refs)
        };

        match (deleter_refs, modifier_refs) {
            // Both reference — genuine conflict, escalate.
            (true, true) => None,
            // Only deleter references — intentional removal.
            (true, false) => Some(ResolutionProposal {
                pattern_name: "spec_delete_confirmation".to_string(),
                confidence: DELETE_CONFIRMATION_CONFIDENCE,
                merged_content: String::new(),
                explanation: format!(
                    "Deleting agent's spec explicitly references '{}'; \
                     modifying agent's does not — deletion is intentional",
                    ctx.file_path
                ),
                warnings: vec![],
            }),
            // Only modifier references — deletion was incidental, keep modification.
            (false, true) => {
                let content = if left_deleted {
                    units_to_content(&conflict.region.right_units)
                } else {
                    units_to_content(&conflict.region.left_units)
                };
                Some(ResolutionProposal {
                    pattern_name: "spec_delete_confirmation".to_string(),
                    confidence: DELETE_CONFIRMATION_CONFIDENCE,
                    merged_content: content,
                    explanation: format!(
                        "Modifying agent's spec explicitly references '{}'; \
                         deleting agent's does not — keeping modification",
                        ctx.file_path
                    ),
                    warnings: vec![],
                })
            }
            // Neither references — unclear intent, escalate.
            (false, false) => None,
        }
    }

    /// Rule 3: Intent Compatibility.
    ///
    /// If both seals describe non-conflicting intents and Phase 3 suggested
    /// a composition, confirm the suggestion with bumped confidence.
    fn try_intent_compatibility(
        &self,
        _conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        ctx: &SpecContext,
    ) -> Option<ResolutionProposal> {
        // Only fires when there's a Phase 3 suggestion to confirm.
        let suggestion = suggestion?;

        // Skip if either seal summary is empty — no intent to analyze.
        if ctx.left_seal_summary.is_empty() || ctx.right_seal_summary.is_empty() {
            return None;
        }

        // Check for conflicting intents.
        if intents_conflict(&ctx.left_seal_summary, &ctx.right_seal_summary) {
            return None;
        }

        // Non-conflicting intents — confirm the Phase 3 suggestion.
        Some(ResolutionProposal {
            pattern_name: "spec_intent_compatibility".to_string(),
            confidence: INTENT_COMPATIBILITY_CONFIDENCE,
            merged_content: suggestion.merged_content.clone(),
            explanation: format!(
                "Both agents' intents are compatible (left: '{}', right: '{}'); \
                 confirming Phase 3 suggestion '{}'",
                truncate(&ctx.left_seal_summary, 60),
                truncate(&ctx.right_seal_summary, 60),
                suggestion.pattern_name,
            ),
            warnings: suggestion.warnings.clone(),
        })
    }

    /// Rule 4: Spec Priority (stub).
    ///
    /// Reserved for future priority ordering on specs. Currently returns
    /// `None` — specs don't have priority fields yet.
    fn try_spec_priority(
        &self,
        _conflict: &ClassifiedConflict,
        _ctx: &SpecContext,
    ) -> Option<ResolutionProposal> {
        // Stub: specs don't have priority fields yet.
        // When Spec gains a `priority` field, implement:
        // - Compare left vs right priority
        // - If significant difference, prefer higher-priority spec's content
        // - Confidence: 0.85
        None
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Check whether a file path matches any entry in a scope list.
///
/// Replicates the matching logic from `repo.rs::check_file_scope`:
/// - Directory scopes (ending with `/`): prefix match
/// - Glob patterns (containing `*`): glob match
/// - Exact paths: exact match or directory prefix
fn file_in_scope(file_path: &str, scope: &[String]) -> bool {
    if scope.is_empty() {
        return false;
    }
    scope.iter().any(|entry| {
        if entry.ends_with('/') {
            file_path.starts_with(entry) || file_path.starts_with(&entry[..entry.len() - 1])
        } else if entry.contains('*') {
            crate::ignore::glob_match(entry, file_path)
        } else {
            file_path == entry || file_path.starts_with(&format!("{entry}/"))
        }
    })
}

/// Check if a spec's metadata (acceptance criteria, design notes, seal summary)
/// references a particular file path.
///
/// Looks for the file's name or path segments in the text content. This is
/// a heuristic — it checks for substring matches of the file name and
/// path components.
fn spec_references_file(
    acceptance_criteria: &[String],
    design_notes: &[String],
    seal_summary: &str,
    file_path: &str,
) -> bool {
    // Extract the file name for matching (e.g., "models.py" from "src/models.py").
    let file_name = file_path.rsplit('/').next().unwrap_or(file_path);

    // Also try the stem (e.g., "models" from "models.py") for broader matching.
    let file_stem = file_name.rsplit('.').last().unwrap_or(file_name);

    let texts: Vec<&str> = acceptance_criteria
        .iter()
        .map(|s| s.as_str())
        .chain(design_notes.iter().map(|s| s.as_str()))
        .chain(std::iter::once(seal_summary))
        .collect();

    let file_path_lower = file_path.to_lowercase();
    let file_name_lower = file_name.to_lowercase();
    let file_stem_lower = file_stem.to_lowercase();

    texts.iter().any(|text| {
        let lower = text.to_lowercase();
        lower.contains(&file_path_lower)
            || lower.contains(&file_name_lower)
            || (file_stem_lower.len() > 2 && lower.contains(&file_stem_lower))
    })
}

/// Check whether two seal summaries describe conflicting intents.
///
/// Looks for negation patterns where one summary contradicts the other.
/// This is deliberately conservative — it only flags conflicts when
/// explicit removal/deletion language targets the other side's subject.
fn intents_conflict(left_summary: &str, right_summary: &str) -> bool {
    let negation_keywords = [
        "remove", "delete", "drop", "revert", "undo", "disable", "strip",
    ];

    let left_lower = left_summary.to_lowercase();
    let right_lower = right_summary.to_lowercase();

    // Extract key terms from each summary (words > 3 chars, skip common words).
    let left_terms = extract_key_terms(&left_lower);
    let right_terms = extract_key_terms(&right_lower);

    // Check if left's summary negates something right is working on.
    for keyword in &negation_keywords {
        if left_lower.contains(keyword) {
            // Check if any of right's key terms appear near the negation in left.
            for term in &right_terms {
                if left_lower.contains(term.as_str()) {
                    return true;
                }
            }
        }
        if right_lower.contains(keyword) {
            for term in &left_terms {
                if right_lower.contains(term.as_str()) {
                    return true;
                }
            }
        }
    }

    false
}

/// Extract meaningful terms from a summary for conflict detection.
fn extract_key_terms(text: &str) -> Vec<String> {
    let stop_words = [
        "the", "and", "for", "with", "from", "that", "this", "into", "added", "updated", "changed",
        "modified", "code", "file", "module", "function", "class", "method",
    ];

    text.split_whitespace()
        .filter(|w| w.len() > 3)
        .filter(|w| !stop_words.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Join structural unit contents into a single string.
fn units_to_content(units: &[StructuralUnit]) -> String {
    units
        .iter()
        .map(|u| u.content.as_str())
        .collect::<Vec<&str>>()
        .join("\n")
}

/// Truncate a string for display in explanations.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find the largest valid char boundary at or before max_len
        // to avoid panicking on multi-byte UTF-8 characters.
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::test_utils::helpers::{
        make_typed_conflict as make_conflict, typed_unit as unit,
    };
    use crate::convergence::types::UnitKind;

    /// Helper: build a spec context with defaults.
    fn make_context(
        file_path: &str,
        left_scope: Vec<String>,
        right_scope: Vec<String>,
    ) -> SpecContext {
        SpecContext {
            left_file_scope: left_scope,
            right_file_scope: right_scope,
            left_description: String::new(),
            right_description: String::new(),
            left_seal_summary: String::new(),
            right_seal_summary: String::new(),
            left_acceptance_criteria: vec![],
            right_acceptance_criteria: vec![],
            left_design_notes: vec![],
            right_design_notes: vec![],
            file_path: file_path.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Rule 1: Scope Authority
    // -----------------------------------------------------------------------

    #[test]
    fn test_scope_authority_left_in_scope() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left change")],
            vec![unit(UnitKind::Unknown, "right change")],
        );
        let ctx = make_context(
            "models.py",
            vec!["models.py".to_string()],
            vec!["views.py".to_string()],
        );

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some(), "should resolve via scope authority");
        let proposal = result.unwrap();
        assert_eq!(proposal.pattern_name, "spec_scope_authority");
        assert!((proposal.confidence - 0.90).abs() < f64::EPSILON);
        assert_eq!(proposal.merged_content, "left change");
    }

    #[test]
    fn test_scope_authority_right_in_scope() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left change")],
            vec![unit(UnitKind::Unknown, "right change")],
        );
        let ctx = make_context(
            "models.py",
            vec!["views.py".to_string()],
            vec!["models.py".to_string()],
        );

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert_eq!(proposal.merged_content, "right change");
    }

    #[test]
    fn test_scope_authority_both_in_scope() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let ctx = make_context(
            "models.py",
            vec!["models.py".to_string()],
            vec!["models.py".to_string()],
        );

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_none(), "both in scope → no resolution");
    }

    #[test]
    fn test_scope_authority_neither_in_scope() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let ctx = make_context(
            "models.py",
            vec!["views.py".to_string()],
            vec!["routes.py".to_string()],
        );

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_none(), "neither in scope → no resolution");
    }

    #[test]
    fn test_scope_authority_glob_matching() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left change")],
            vec![unit(UnitKind::Unknown, "right change")],
        );
        let ctx = make_context(
            "src/models.py",
            vec!["src/*.py".to_string()],
            vec!["tests/".to_string()],
        );

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some(), "glob should match src/models.py");
        assert_eq!(result.unwrap().merged_content, "left change");
    }

    // -----------------------------------------------------------------------
    // Rule 2: Delete Confirmation
    // -----------------------------------------------------------------------

    #[test]
    fn test_delete_confirmation_deleter_spec_owns() {
        let resolver = SpecAwareResolver;
        // Left deleted, right modified.
        let conflict = make_conflict(
            ConflictType::DeleteVsModify,
            vec![unit(UnitKind::Unknown, "original")],
            vec![], // left deleted
            vec![unit(UnitKind::Unknown, "modified")],
        );
        let mut ctx = make_context("legacy.py", vec![], vec![]);
        ctx.left_acceptance_criteria = vec!["Remove legacy.py endpoint".to_string()];

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert_eq!(proposal.pattern_name, "spec_delete_confirmation");
        assert!(
            proposal.merged_content.is_empty(),
            "should resolve as deletion"
        );
    }

    #[test]
    fn test_delete_confirmation_modifier_spec_owns() {
        let resolver = SpecAwareResolver;
        // Left deleted, right modified.
        let conflict = make_conflict(
            ConflictType::DeleteVsModify,
            vec![unit(UnitKind::Unknown, "original")],
            vec![], // left deleted
            vec![unit(UnitKind::Unknown, "modified content")],
        );
        let mut ctx = make_context("legacy.py", vec![], vec![]);
        ctx.right_acceptance_criteria = vec!["Update legacy.py with new auth".to_string()];

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert_eq!(proposal.merged_content, "modified content");
    }

    #[test]
    fn test_delete_confirmation_both_reference() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::DeleteVsModify,
            vec![unit(UnitKind::Unknown, "original")],
            vec![],
            vec![unit(UnitKind::Unknown, "modified")],
        );
        let mut ctx = make_context("legacy.py", vec![], vec![]);
        ctx.left_acceptance_criteria = vec!["Remove legacy.py".to_string()];
        ctx.right_acceptance_criteria = vec!["Update legacy.py".to_string()];

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_none(), "both reference → escalate");
    }

    #[test]
    fn test_delete_confirmation_neither_reference() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::DeleteVsModify,
            vec![unit(UnitKind::Unknown, "original")],
            vec![],
            vec![unit(UnitKind::Unknown, "modified")],
        );
        let ctx = make_context("legacy.py", vec![], vec![]);

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_none(), "neither reference → escalate");
    }

    #[test]
    fn test_delete_confirmation_only_on_delete_vs_modify() {
        let resolver = SpecAwareResolver;
        // BothModified — delete confirmation should NOT fire.
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let mut ctx = make_context("models.py", vec![], vec![]);
        ctx.left_acceptance_criteria = vec!["Remove models.py".to_string()];

        // With no scope authority match, delete confirmation should skip.
        let result = resolver.try_delete_confirmation(&conflict, &ctx);
        assert!(result.is_none(), "should not fire on BothModified");
    }

    // -----------------------------------------------------------------------
    // Rule 3: Intent Compatibility
    // -----------------------------------------------------------------------

    #[test]
    fn test_intent_compatibility_confirms_suggestion() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Import, "import os")],
            vec![
                unit(UnitKind::Import, "import os"),
                unit(UnitKind::Import, "import json"),
            ],
            vec![
                unit(UnitKind::Import, "import os"),
                unit(UnitKind::Import, "import sys"),
            ],
        );
        let suggestion = ResolutionProposal {
            pattern_name: "additive_import_accumulation".to_string(),
            confidence: 0.70,
            merged_content: "import os\nimport json\nimport sys".to_string(),
            explanation: "merged imports".to_string(),
            warnings: vec![],
        };
        let mut ctx = make_context("models.py", vec![], vec![]);
        ctx.left_seal_summary = "Added JSON parsing to models".to_string();
        ctx.right_seal_summary = "Added system utilities to models".to_string();

        let result = resolver.try_intent_compatibility(&conflict, Some(&suggestion), &ctx);
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert_eq!(proposal.pattern_name, "spec_intent_compatibility");
        assert!((proposal.confidence - 0.85).abs() < f64::EPSILON);
        assert_eq!(proposal.merged_content, suggestion.merged_content);
    }

    #[test]
    fn test_intent_compatibility_no_suggestion() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let mut ctx = make_context("models.py", vec![], vec![]);
        ctx.left_seal_summary = "Added auth".to_string();
        ctx.right_seal_summary = "Added logging".to_string();

        let result = resolver.try_intent_compatibility(&conflict, None, &ctx);
        assert!(result.is_none(), "no suggestion → no resolution");
    }

    #[test]
    fn test_intent_compatibility_conflicting_summaries() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let suggestion = ResolutionProposal {
            pattern_name: "some_pattern".to_string(),
            confidence: 0.70,
            merged_content: "merged".to_string(),
            explanation: "test".to_string(),
            warnings: vec![],
        };
        let mut ctx = make_context("auth.py", vec![], vec![]);
        ctx.left_seal_summary = "Added token validation to auth".to_string();
        ctx.right_seal_summary = "Remove token validation from auth".to_string();

        let result = resolver.try_intent_compatibility(&conflict, Some(&suggestion), &ctx);
        assert!(
            result.is_none(),
            "conflicting intents should not confirm suggestion"
        );
    }

    // -----------------------------------------------------------------------
    // Integration
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_tries_rules_in_order() {
        let resolver = SpecAwareResolver;
        // Set up a DeleteVsModify where scope authority should fire BEFORE
        // delete confirmation, because left owns the file.
        let conflict = make_conflict(
            ConflictType::DeleteVsModify,
            vec![unit(UnitKind::Unknown, "original")],
            vec![], // left deleted
            vec![unit(UnitKind::Unknown, "right modified")],
        );
        let mut ctx = make_context(
            "models.py",
            vec!["views.py".to_string()],  // left does NOT own
            vec!["models.py".to_string()], // right owns
        );
        ctx.right_acceptance_criteria = vec!["Update models.py".to_string()];

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_some());
        // Scope authority fires first (right owns the file).
        assert_eq!(
            result.unwrap().pattern_name,
            "spec_scope_authority",
            "scope authority should fire before delete confirmation"
        );
    }

    #[test]
    fn test_resolve_returns_none_when_no_rules_match() {
        let resolver = SpecAwareResolver;
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![unit(UnitKind::Unknown, "base")],
            vec![unit(UnitKind::Unknown, "left")],
            vec![unit(UnitKind::Unknown, "right")],
        );
        let ctx = make_context("unknown.py", vec![], vec![]);

        let result = resolver.resolve(&conflict, None, &ctx);
        assert!(result.is_none(), "no rules match → None");
    }

    // -----------------------------------------------------------------------
    // Helper function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_file_in_scope_exact_match() {
        assert!(file_in_scope("models.py", &["models.py".to_string()]));
        assert!(!file_in_scope("views.py", &["models.py".to_string()]));
    }

    #[test]
    fn test_file_in_scope_directory_prefix() {
        assert!(file_in_scope("src/models.py", &["src/".to_string()]));
        assert!(!file_in_scope(
            "tests/test_models.py",
            &["src/".to_string()]
        ));
    }

    #[test]
    fn test_file_in_scope_empty_scope() {
        assert!(!file_in_scope("models.py", &[]));
    }

    #[test]
    fn test_spec_references_file_by_name() {
        assert!(spec_references_file(
            &["Update models.py with new fields".to_string()],
            &[],
            "",
            "src/models.py",
        ));
    }

    #[test]
    fn test_spec_references_file_by_stem() {
        assert!(spec_references_file(
            &[],
            &["Refactor the models module".to_string()],
            "",
            "src/models.py",
        ));
    }

    #[test]
    fn test_spec_references_file_in_seal_summary() {
        assert!(spec_references_file(
            &[],
            &[],
            "Updated models.py with auth changes",
            "src/models.py",
        ));
    }

    #[test]
    fn test_spec_does_not_reference_unrelated_file() {
        assert!(!spec_references_file(
            &["Add new views".to_string()],
            &["Frontend routing".to_string()],
            "Added login page",
            "src/models.py",
        ));
    }

    #[test]
    fn test_intents_conflict_negation() {
        assert!(intents_conflict(
            "Added token validation to auth",
            "Remove token validation from auth",
        ));
    }

    #[test]
    fn test_intents_compatible() {
        assert!(!intents_conflict(
            "Added token validation to auth module",
            "Added rate limiting wrapper to auth module",
        ));
    }

    #[test]
    fn test_truncate_ascii() {
        assert_eq!(truncate("hello world", 5), "hello");
        assert_eq!(truncate("short", 100), "short");
    }

    #[test]
    fn test_truncate_multibyte_utf8() {
        // Each emoji is 4 bytes. "Hi 👋🌍" = 3 ASCII + 4 + 4 = 11 bytes.
        let s = "Hi 👋🌍";
        // Truncating at 5 lands inside the first emoji (bytes 3..7).
        // Should back up to byte 3 (the space boundary).
        let result = truncate(s, 5);
        assert_eq!(result, "Hi ");
        // Truncating at 7 should capture "Hi 👋" (exactly on boundary).
        let result = truncate(s, 7);
        assert_eq!(result, "Hi 👋");
    }
}
