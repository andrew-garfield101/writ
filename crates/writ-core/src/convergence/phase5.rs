//! Phase 5: LLM-Assisted Conflict Resolution
//!
//! When Phases 3 and 4 cannot resolve a conflict, Phase 5 constructs a
//! structured prompt from the conflict context and asks an LLM to propose
//! a merge. The result goes through sanity checks before being accepted.
//!
//! The LLM backend is abstracted behind [`LlmBackend`] so any provider
//! (OpenAI, Anthropic, local model, etc.) can be plugged in.

use super::analyzers;
use super::pipeline::LlmResolver;
use super::types::{
    ClassifiedConflict, ConflictType, ResolutionProposal, StructuralUnit, UnitKind,
};

// ---------------------------------------------------------------------------
// LLM Backend Trait
// ---------------------------------------------------------------------------

/// Abstraction over the actual LLM API call.
///
/// Implementations should handle authentication, rate limiting, retries,
/// and timeout. The convergence engine only cares about the response text.
pub trait LlmBackend: Send + Sync {
    /// Send a prompt to the LLM and return the response text.
    /// Returns `None` if the call fails or times out.
    fn complete(&self, prompt: &str) -> Option<String>;

    /// The model name (for audit trail).
    fn model_name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Prompt Construction
// ---------------------------------------------------------------------------

/// Build a structured prompt for the LLM from a classified conflict.
pub fn build_merge_prompt(
    conflict: &ClassifiedConflict,
    suggestion: Option<&ResolutionProposal>,
    file_path: &str,
) -> String {
    let base_text = units_to_text(&conflict.region.base_units);
    let left_text = units_to_text(&conflict.region.left_units);
    let right_text = units_to_text(&conflict.region.right_units);

    let conflict_type_desc = match conflict.conflict_type {
        ConflictType::BothModified => "Both sides modified the same region",
        ConflictType::BothInserted => "Both sides inserted new content at the same location",
        ConflictType::DeleteVsModify => "One side deleted content that the other modified",
        ConflictType::LeftOnly => "Only the left side changed this region",
        ConflictType::RightOnly => "Only the right side changed this region",
        ConflictType::BothDeleted => "Both sides deleted this region",
        ConflictType::Clean => "No conflict (clean merge)",
    };

    let scope_desc = format!("{:?}", conflict.structural_info.scope);

    let suggestion_section = if let Some(s) = suggestion {
        format!(
            "\n## Phase 3 Suggestion (confidence: {:.0}%)\nPattern: {}\nExplanation: {}\nProposed content:\n```\n{}\n```\n",
            s.confidence * 100.0,
            s.pattern_name,
            s.explanation,
            s.merged_content,
        )
    } else {
        "\n## No Phase 3 suggestion available.\n".to_string()
    };

    format!(
        r#"You are a code merge assistant. Your task is to merge two conflicting versions of a code region into a single correct result.

## File
{file_path}

## Conflict Type
{conflict_type_desc}

## Scope
{scope_desc}

## Base Version (common ancestor)
```
{base_text}
```

## Left Version (Agent A)
```
{left_text}
```

## Right Version (Agent B)
```
{right_text}
```
{suggestion_section}
## Instructions
1. Merge the left and right versions into a single correct result.
2. Preserve ALL intentional changes from both sides.
3. Do NOT silently drop any content — if content was added by either side, include it.
4. If the changes are truly incompatible, respond with EXACTLY "INCOMPATIBLE" on the first line.
5. Otherwise, respond with ONLY the merged code — no explanation, no markdown fences, no commentary.
"#
    )
}

fn units_to_text(units: &[StructuralUnit]) -> String {
    if units.is_empty() {
        return "(empty)".to_string();
    }
    units
        .iter()
        .map(|u| u.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Response Parsing & Sanity Checks
// ---------------------------------------------------------------------------

/// Parse and validate the LLM's response.
///
/// Returns `None` if the response is invalid or fails sanity checks.
pub fn parse_llm_response(
    response: &str,
    conflict: &ClassifiedConflict,
    file_path: &str,
) -> Option<ResolutionProposal> {
    let trimmed = response.trim();

    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with("INCOMPATIBLE") {
        return None;
    }

    // Strip markdown fences if the LLM included them despite instructions.
    let content = strip_markdown_fences(trimmed);

    if content.is_empty() {
        return None;
    }

    let confidence = sanity_check(&content, conflict, file_path);

    if confidence < 0.01 {
        return None;
    }

    Some(ResolutionProposal {
        pattern_name: "llm-assisted".to_string(),
        confidence,
        merged_content: content,
        explanation: "Resolved by LLM-assisted merge".to_string(),
        warnings: vec![],
    })
}

fn strip_markdown_fences(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() >= 2
        && lines[0].starts_with("```")
        && lines.last().map(|l| l.trim()) == Some("```")
    {
        lines[1..lines.len() - 1].join("\n")
    } else {
        text.to_string()
    }
}

/// Sanity-check the LLM output against the conflict context.
///
/// Returns a confidence score (0.0–1.0). Higher means safer.
fn sanity_check(merged: &str, conflict: &ClassifiedConflict, file_path: &str) -> f64 {
    let mut score: f64 = 0.70;

    // Check 1: Content shouldn't be dramatically shorter than both sides.
    // This catches cases where the LLM dropped significant content.
    let left_len = conflict
        .region
        .left_units
        .iter()
        .map(|u| u.content.len())
        .sum::<usize>();
    let right_len = conflict
        .region
        .right_units
        .iter()
        .map(|u| u.content.len())
        .sum::<usize>();
    let merged_len = merged.len();
    let min_side = left_len.min(right_len);

    if min_side > 0 && merged_len < min_side / 3 {
        // Merged is less than 1/3 of the smaller side — suspicious.
        score -= 0.30;
    }

    // Check 2: For BothInserted, merged should be at least as long as
    // the longer side (both sides' content should be present).
    if conflict.conflict_type == ConflictType::BothInserted {
        let max_side = left_len.max(right_len);
        if max_side > 0 && merged_len < max_side {
            score -= 0.15;
        }
    }

    // Check 3: Verify the analyzer can parse the result.
    let analyzer = analyzers::analyzer_for_path(file_path);
    if analyzer.name() != "generic" {
        let units = analyzer.parse_structure(merged);
        let unknown_ratio = units.iter().filter(|u| u.kind == UnitKind::Unknown).count() as f64
            / units.len().max(1) as f64;
        if unknown_ratio > 0.5 {
            score -= 0.20;
        }
    }

    // Check 4: Key identifiers from both sides should survive.
    let left_names: Vec<&str> = conflict
        .region
        .left_units
        .iter()
        .filter(|u| u.kind == UnitKind::Definition || u.kind == UnitKind::Import)
        .filter_map(|u| u.name.as_deref())
        .collect();
    let right_names: Vec<&str> = conflict
        .region
        .right_units
        .iter()
        .filter(|u| u.kind == UnitKind::Definition || u.kind == UnitKind::Import)
        .filter_map(|u| u.name.as_deref())
        .collect();

    let missing_left = left_names.iter().filter(|n| !merged.contains(**n)).count();
    let missing_right = right_names.iter().filter(|n| !merged.contains(**n)).count();
    let total_names = left_names.len() + right_names.len();

    if total_names > 0 {
        let missing_ratio = (missing_left + missing_right) as f64 / total_names as f64;
        if missing_ratio > 0.5 {
            score -= 0.25;
        } else if missing_ratio > 0.0 {
            score -= 0.10;
        }
    }

    score.max(0.0).min(1.0)
}

// ---------------------------------------------------------------------------
// No-op backend (default when no real LLM is configured)
// ---------------------------------------------------------------------------

/// A no-op backend that always returns `None`.
///
/// Used as the default when no real LLM provider is configured.
/// The pipeline gate (`enable_phase5_llm`) prevents Phase 5 from running,
/// but this ensures the resolver is structurally real and ready to swap in
/// a real backend via `set_llm_resolver()`.
pub struct NoOpBackend;

impl LlmBackend for NoOpBackend {
    fn complete(&self, _prompt: &str) -> Option<String> {
        None
    }

    fn model_name(&self) -> &str {
        "none"
    }
}

// ---------------------------------------------------------------------------
// StructuredLlmResolver — Real implementation
// ---------------------------------------------------------------------------

/// LLM-assisted resolver that constructs prompts, calls an LLM backend,
/// and validates the response with sanity checks.
pub struct StructuredLlmResolver {
    backend: Box<dyn LlmBackend>,
}

impl StructuredLlmResolver {
    pub fn new(backend: Box<dyn LlmBackend>) -> Self {
        Self { backend }
    }
}

impl LlmResolver for StructuredLlmResolver {
    fn resolve(
        &self,
        conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        file_path: &str,
    ) -> Option<ResolutionProposal> {
        if conflict.conflict_type == ConflictType::Clean {
            return None;
        }

        let prompt = build_merge_prompt(conflict, suggestion, file_path);
        let response = self.backend.complete(&prompt)?;

        let mut proposal = parse_llm_response(&response, conflict, file_path)?;
        proposal.pattern_name = format!("llm-assisted({})", self.backend.model_name());
        Some(proposal)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::test_utils::helpers::make_unit;
    use crate::convergence::types::*;

    /// Phase5 conflict builder — populates unit_kinds from actual units
    /// and uses (0,1) spans (needed for prompt-building tests).
    fn make_conflict(
        conflict_type: ConflictType,
        base: Vec<StructuralUnit>,
        left: Vec<StructuralUnit>,
        right: Vec<StructuralUnit>,
    ) -> ClassifiedConflict {
        ClassifiedConflict {
            conflict_type,
            requires_review: conflict_type.always_requires_review(),
            structural_info: StructuralInfo {
                scope: ConflictScope::Mixed,
                left_unit_kinds: left.iter().map(|u| u.kind.clone()).collect(),
                right_unit_kinds: right.iter().map(|u| u.kind.clone()).collect(),
                has_name_overlap: false,
            },
            region: StructuralConflictRegion {
                base_units: base,
                left_units: left,
                right_units: right,
                base_span: (0, 1),
                left_span: (0, 1),
                right_span: (0, 1),
            },
        }
    }

    #[test]
    fn test_build_prompt_includes_all_sections() {
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![make_unit(
                UnitKind::Definition,
                Some("foo"),
                "def foo(): pass",
            )],
            vec![make_unit(
                UnitKind::Definition,
                Some("foo"),
                "def foo(): return 1",
            )],
            vec![make_unit(
                UnitKind::Definition,
                Some("foo"),
                "def foo(): return 2",
            )],
        );

        let prompt = build_merge_prompt(&conflict, None, "app.py");
        assert!(prompt.contains("app.py"));
        assert!(prompt.contains("Both sides modified"));
        assert!(prompt.contains("def foo(): pass"));
        assert!(prompt.contains("def foo(): return 1"));
        assert!(prompt.contains("def foo(): return 2"));
        assert!(prompt.contains("No Phase 3 suggestion"));
        assert!(prompt.contains("INCOMPATIBLE"));
    }

    #[test]
    fn test_build_prompt_includes_suggestion() {
        let conflict = make_conflict(
            ConflictType::BothInserted,
            vec![],
            vec![make_unit(UnitKind::Import, Some("os"), "import os")],
            vec![make_unit(UnitKind::Import, Some("sys"), "import sys")],
        );
        let suggestion = ResolutionProposal {
            pattern_name: "import_accumulation".into(),
            confidence: 0.75,
            merged_content: "import os\nimport sys".into(),
            explanation: "Accumulated imports".into(),
            warnings: vec![],
        };

        let prompt = build_merge_prompt(&conflict, Some(&suggestion), "app.py");
        assert!(prompt.contains("Phase 3 Suggestion"));
        assert!(prompt.contains("import_accumulation"));
        assert!(prompt.contains("75%"));
    }

    #[test]
    fn test_parse_incompatible_returns_none() {
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![],
            vec![make_unit(UnitKind::Statement, None, "x = 1")],
            vec![make_unit(UnitKind::Statement, None, "x = 2")],
        );
        assert!(parse_llm_response("INCOMPATIBLE", &conflict, "test.py").is_none());
    }

    #[test]
    fn test_parse_empty_returns_none() {
        let conflict = make_conflict(ConflictType::BothModified, vec![], vec![], vec![]);
        assert!(parse_llm_response("", &conflict, "test.py").is_none());
        assert!(parse_llm_response("   ", &conflict, "test.py").is_none());
    }

    #[test]
    fn test_parse_valid_response() {
        let conflict = make_conflict(
            ConflictType::BothInserted,
            vec![],
            vec![make_unit(UnitKind::Import, Some("os"), "import os")],
            vec![make_unit(UnitKind::Import, Some("sys"), "import sys")],
        );
        let result = parse_llm_response("import os\nimport sys", &conflict, "app.py");
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert_eq!(proposal.pattern_name, "llm-assisted");
        assert!(proposal.confidence > 0.5);
        assert!(proposal.merged_content.contains("import os"));
        assert!(proposal.merged_content.contains("import sys"));
    }

    #[test]
    fn test_parse_strips_markdown_fences() {
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![],
            vec![make_unit(UnitKind::Statement, None, "x = 1")],
            vec![make_unit(UnitKind::Statement, None, "x = 2")],
        );
        let result = parse_llm_response("```python\nx = 3\n```", &conflict, "test.py");
        assert!(result.is_some());
        assert_eq!(result.unwrap().merged_content, "x = 3");
    }

    #[test]
    fn test_sanity_check_penalizes_content_loss() {
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![],
            vec![make_unit(
                UnitKind::Definition,
                Some("UserModel"),
                "class UserModel:\n    name: str\n    email: str\n    age: int",
            )],
            vec![make_unit(
                UnitKind::Definition,
                Some("UserModel"),
                "class UserModel:\n    name: str\n    phone: str",
            )],
        );
        let short_merge = "pass";
        let score = sanity_check(short_merge, &conflict, "models.py");
        assert!(
            score < 0.5,
            "Drastically shorter merge should score low: {score}"
        );
    }

    #[test]
    fn test_sanity_check_rewards_complete_merge() {
        let conflict = make_conflict(
            ConflictType::BothInserted,
            vec![],
            vec![make_unit(UnitKind::Import, Some("json"), "import json")],
            vec![make_unit(UnitKind::Import, Some("sys"), "import sys")],
        );
        let good_merge = "import json\nimport sys";
        let score = sanity_check(good_merge, &conflict, "app.py");
        assert!(score >= 0.6, "Complete merge should score well: {score}");
    }

    #[test]
    fn test_sanity_check_detects_missing_names() {
        let conflict = make_conflict(
            ConflictType::BothInserted,
            vec![],
            vec![
                make_unit(UnitKind::Definition, Some("foo"), "def foo(): pass"),
                make_unit(UnitKind::Definition, Some("bar"), "def bar(): pass"),
            ],
            vec![make_unit(
                UnitKind::Definition,
                Some("baz"),
                "def baz(): pass",
            )],
        );
        let missing_merge = "def baz(): pass";
        let score = sanity_check(missing_merge, &conflict, "app.py");
        assert!(
            score < 0.65,
            "Missing names from left should lower score: {score}"
        );
    }

    struct MockBackend {
        response: Option<String>,
    }

    impl LlmBackend for MockBackend {
        fn complete(&self, _prompt: &str) -> Option<String> {
            self.response.clone()
        }
        fn model_name(&self) -> &str {
            "mock-model"
        }
    }

    #[test]
    fn test_structured_resolver_calls_backend() {
        let backend = MockBackend {
            response: Some("import os\nimport sys".to_string()),
        };
        let resolver = StructuredLlmResolver::new(Box::new(backend));
        let conflict = make_conflict(
            ConflictType::BothInserted,
            vec![],
            vec![make_unit(UnitKind::Import, Some("os"), "import os")],
            vec![make_unit(UnitKind::Import, Some("sys"), "import sys")],
        );
        let result = resolver.resolve(&conflict, None, "app.py");
        assert!(result.is_some());
        let proposal = result.unwrap();
        assert!(proposal.pattern_name.contains("mock-model"));
        assert!(proposal.merged_content.contains("import os"));
    }

    #[test]
    fn test_structured_resolver_returns_none_on_backend_failure() {
        let backend = MockBackend { response: None };
        let resolver = StructuredLlmResolver::new(Box::new(backend));
        let conflict = make_conflict(ConflictType::BothModified, vec![], vec![], vec![]);
        assert!(resolver.resolve(&conflict, None, "test.py").is_none());
    }

    #[test]
    fn test_structured_resolver_skips_clean_conflicts() {
        let backend = MockBackend {
            response: Some("should not be called".to_string()),
        };
        let resolver = StructuredLlmResolver::new(Box::new(backend));
        let conflict = make_conflict(ConflictType::Clean, vec![], vec![], vec![]);
        assert!(resolver.resolve(&conflict, None, "test.py").is_none());
    }

    #[test]
    fn test_structured_resolver_rejects_incompatible() {
        let backend = MockBackend {
            response: Some("INCOMPATIBLE - these changes conflict".to_string()),
        };
        let resolver = StructuredLlmResolver::new(Box::new(backend));
        let conflict = make_conflict(
            ConflictType::BothModified,
            vec![],
            vec![make_unit(UnitKind::Statement, None, "x = 1")],
            vec![make_unit(UnitKind::Statement, None, "x = 2")],
        );
        assert!(resolver.resolve(&conflict, None, "test.py").is_none());
    }
}
