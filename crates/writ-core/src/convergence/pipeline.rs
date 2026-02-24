//! Convergence Pipeline — the v2 six-phase orchestrator.
//!
//! This module implements the core convergence pipeline that processes
//! a single file's conflict through all six phases:
//!
//! 1. **Structural Diff** — via [`phase1`](super::phase1)
//! 2. **Classification** — via [`phase2`](super::phase2)
//! 3. **Deterministic Resolution** — via [`PatternRegistry`](super::patterns::PatternRegistry)
//! 4. **Spec-Aware Resolution** — (stub, trait-based for future implementation)
//! 5. **LLM-Assisted Resolution** — (stub, trait-based for future implementation)
//! 6. **Verification** — structural re-parse validation
//!
//! The pipeline never silently loses work. If a conflict cannot be resolved
//! with sufficient confidence, it is escalated as an [`EscalationRecord`].

use std::time::Instant;

use super::analyzers;
use super::patterns::{PatternRegistry, PatternResult};
use super::types::*;

// ---------------------------------------------------------------------------
// Pipeline Configuration
// ---------------------------------------------------------------------------

/// Configuration for the convergence pipeline.
///
/// Controls thresholds, feature flags, and phase enablement.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Confidence thresholds for auto-resolve vs suggest vs ignore.
    pub thresholds: ConfidenceThresholds,
    /// Enable Phase 4 (spec-aware resolution). When false, conflicts that
    /// aren't resolved by Phase 3 go directly to Phase 5 or escalation.
    pub enable_phase4_spec_aware: bool,
    /// Enable Phase 5 (LLM-assisted resolution). When false, conflicts
    /// that aren't resolved by Phase 3/4 are escalated immediately.
    pub enable_phase5_llm: bool,
    /// Enable Phase 6 (post-merge verification). When false, resolved
    /// content is accepted without validation.
    pub enable_phase6_verification: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            thresholds: ConfidenceThresholds::default(),
            enable_phase4_spec_aware: false,
            enable_phase5_llm: false,
            enable_phase6_verification: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline Input
// ---------------------------------------------------------------------------

/// Everything the pipeline needs to process a single file's conflict.
#[derive(Debug, Clone)]
pub struct PipelineInput {
    /// Path of the file being merged.
    pub file_path: String,
    /// Base (common ancestor) content.
    pub base: String,
    /// Left side (agent A) content.
    pub left: String,
    /// Right side (agent B) content.
    pub right: String,
    /// Left agent/spec identifier.
    pub left_spec: String,
    /// Right agent/spec identifier.
    pub right_spec: String,
    /// Optional spec metadata for Phase 4.
    pub spec_context: Option<SpecContext>,
}

/// Spec metadata available for Phase 4 resolution.
#[derive(Debug, Clone)]
pub struct SpecContext {
    /// Files that the left spec is scoped to (if any).
    pub left_file_scope: Vec<String>,
    /// Files that the right spec is scoped to (if any).
    pub right_file_scope: Vec<String>,
    /// Left spec's description/acceptance criteria.
    pub left_description: String,
    /// Right spec's description/acceptance criteria.
    pub right_description: String,
    /// Semantic intent summary from the left agent's seal.
    pub left_seal_summary: String,
    /// Semantic intent summary from the right agent's seal.
    pub right_seal_summary: String,
    /// Left spec's acceptance criteria.
    pub left_acceptance_criteria: Vec<String>,
    /// Right spec's acceptance criteria.
    pub right_acceptance_criteria: Vec<String>,
    /// Left spec's design notes.
    pub left_design_notes: Vec<String>,
    /// Right spec's design notes.
    pub right_design_notes: Vec<String>,
    /// Path of the file being resolved (for scope matching).
    pub file_path: String,
}

// ---------------------------------------------------------------------------
// Pipeline Output
// ---------------------------------------------------------------------------

/// The complete result of running the convergence pipeline on one file.
#[derive(Debug, Clone)]
pub struct PipelineOutput {
    /// Path of the file that was processed.
    pub file_path: String,
    /// Which analyzer was used for structural analysis.
    pub analyzer_used: String,
    /// Whether all conflicts were fully resolved.
    pub fully_resolved: bool,
    /// The merged file content (if fully resolved).
    pub merged_content: Option<String>,
    /// Per-region audit trail (uses the shared `types::RegionOutcome`).
    pub resolutions: Vec<RegionOutcome>,
    /// Escalation records for conflicts that could not be auto-resolved.
    pub escalations: Vec<EscalationRecord>,
    /// Verification result (if Phase 6 ran).
    pub verification: Option<VerificationResult>,
    /// Per-phase timing for performance analysis.
    pub phase_timings: PhaseTimings,
}

/// Timing information for each pipeline phase.
#[derive(Debug, Clone, Default)]
pub struct PhaseTimings {
    pub phase1_structural_diff_ms: u64,
    pub phase2_classification_ms: u64,
    pub phase3_deterministic_ms: u64,
    pub phase4_spec_aware_ms: u64,
    pub phase5_llm_ms: u64,
    pub phase6_verification_ms: u64,
    pub total_ms: u64,
}

// ---------------------------------------------------------------------------
// Phase Traits (for future pluggability)
// ---------------------------------------------------------------------------

/// Phase 4: Spec-aware conflict resolution.
pub trait SpecResolver: Send + Sync {
    fn resolve(
        &self,
        conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        spec_context: &SpecContext,
    ) -> Option<ResolutionProposal>;
}

/// Phase 5: LLM-assisted conflict resolution.
pub trait LlmResolver: Send + Sync {
    fn resolve(
        &self,
        conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        file_path: &str,
    ) -> Option<ResolutionProposal>;
}

/// Phase 6: Post-merge verification.
pub trait Verifier: Send + Sync {
    fn verify(&self, merged_content: &str, file_path: &str) -> VerificationResult;
}

// ---------------------------------------------------------------------------
// Default Stub Implementations
// ---------------------------------------------------------------------------

struct StubLlmResolver;

impl LlmResolver for StubLlmResolver {
    fn resolve(
        &self,
        _conflict: &ClassifiedConflict,
        _suggestion: Option<&ResolutionProposal>,
        _file_path: &str,
    ) -> Option<ResolutionProposal> {
        None
    }
}

/// Basic Phase 6 verifier that checks the analyzer can re-parse the output.
struct BasicVerifier;

impl Verifier for BasicVerifier {
    fn verify(&self, merged_content: &str, file_path: &str) -> VerificationResult {
        let analyzer = analyzers::analyzer_for_path(file_path);
        let units = analyzer.parse_structure(merged_content);

        let has_unknowns = units.iter().any(|u| u.kind == UnitKind::Unknown);
        let lines_covered: usize = units
            .iter()
            .map(|u| u.span.1.saturating_sub(u.span.0))
            .sum();
        let total_lines = merged_content.lines().count();

        let coverage_ratio = if total_lines == 0 {
            1.0
        } else {
            lines_covered as f64 / total_lines as f64
        };

        let mut warnings = Vec::new();
        if has_unknowns && analyzer.name() != "generic" {
            warnings.push(format!(
                "Merged output contains unparseable regions (analyzer: {})",
                analyzer.name()
            ));
        }
        if coverage_ratio < 0.9 && total_lines > 0 {
            warnings.push(format!(
                "Analyzer only covers {:.0}% of merged output lines",
                coverage_ratio * 100.0
            ));
        }

        let verdict = if !warnings.is_empty() {
            VerificationVerdict::PassedWithWarnings
        } else {
            VerificationVerdict::Verified
        };

        VerificationResult {
            syntactic_valid: true,
            warnings,
            verdict,
        }
    }
}

// ---------------------------------------------------------------------------
// The Pipeline
// ---------------------------------------------------------------------------

/// The v2 convergence pipeline.
///
/// Processes a single file through all 6 phases, producing a fully
/// auditable resolution (or escalation) for every conflict region.
pub struct ConvergencePipeline {
    config: PipelineConfig,
    pattern_registry: PatternRegistry,
    spec_resolver: Box<dyn SpecResolver>,
    llm_resolver: Box<dyn LlmResolver>,
    verifier: Box<dyn Verifier>,
}

impl ConvergencePipeline {
    /// Create a pipeline with default configuration and stub resolvers.
    pub fn new() -> Self {
        Self::with_config(PipelineConfig::default())
    }

    /// Create a pipeline with custom configuration.
    pub fn with_config(config: PipelineConfig) -> Self {
        let pattern_registry = PatternRegistry::with_thresholds(config.thresholds.clone());
        Self {
            config,
            pattern_registry,
            spec_resolver: Box::new(super::phase4::SpecAwareResolver),
            llm_resolver: Box::new(StubLlmResolver),
            verifier: Box::new(BasicVerifier),
        }
    }

    /// Replace the Phase 4 spec resolver.
    pub fn set_spec_resolver(&mut self, resolver: Box<dyn SpecResolver>) {
        self.spec_resolver = resolver;
    }

    /// Replace the Phase 5 LLM resolver.
    pub fn set_llm_resolver(&mut self, resolver: Box<dyn LlmResolver>) {
        self.llm_resolver = resolver;
    }

    /// Replace the Phase 6 verifier.
    pub fn set_verifier(&mut self, verifier: Box<dyn Verifier>) {
        self.verifier = verifier;
    }

    /// Run the full 6-phase pipeline on a single file.
    pub fn run(&self, input: &PipelineInput) -> PipelineOutput {
        let pipeline_start = Instant::now();
        let mut timings = PhaseTimings::default();

        // === Phase 1: Structural Diff (via phase1 module) ===
        // phase1::run() calls diff3 internally and lifts to structural regions.
        let phase1_start = Instant::now();
        let phase1_result =
            super::phase1::run(&input.file_path, &input.base, &input.left, &input.right);
        timings.phase1_structural_diff_ms = phase1_start.elapsed().as_millis() as u64;

        let structural_diff = match phase1_result {
            Phase1Result::Clean(content) => {
                timings.total_ms = pipeline_start.elapsed().as_millis() as u64;
                return PipelineOutput {
                    file_path: input.file_path.clone(),
                    analyzer_used: analyzers::analyzer_for_path(&input.file_path)
                        .name()
                        .to_string(),
                    fully_resolved: true,
                    merged_content: Some(content),
                    resolutions: vec![],
                    escalations: vec![],
                    verification: None,
                    phase_timings: timings,
                };
            }
            Phase1Result::Conflicts(diff) => diff,
        };

        let analyzer_used = structural_diff.analyzer_used.clone();

        // === Phase 2: Classification (via phase2 module) ===
        let phase2_start = Instant::now();
        let phase2_result = super::phase2::run(&structural_diff);
        timings.phase2_classification_ms = phase2_start.elapsed().as_millis() as u64;

        // === Phases 3-5: Resolution ===
        let phase3_start = Instant::now();
        let mut region_outcomes: Vec<RegionOutcome> =
            Vec::with_capacity(phase2_result.classified_conflicts.len());
        let mut escalations: Vec<EscalationRecord> = Vec::new();
        let mut resolved_contents: Vec<Option<String>> =
            Vec::with_capacity(phase2_result.classified_conflicts.len());

        for conflict in &phase2_result.classified_conflicts {
            let phase3_result = self.pattern_registry.evaluate(conflict);

            match phase3_result {
                PatternResult::AutoResolved(proposal) => {
                    resolved_contents.push(Some(proposal.merged_content.clone()));
                    region_outcomes.push(RegionOutcome {
                        classified: conflict.clone(),
                        phase3_result: Some(proposal.clone()),
                        resolution: RegionResolutionStatus::Resolved {
                            content: proposal.merged_content.clone(),
                            method: proposal.pattern_name.clone(),
                            confidence: proposal.confidence,
                            resolved_in_phase: 3,
                        },
                    });
                }
                PatternResult::Suggested(suggestion) => {
                    if let Some(resolved) =
                        self.try_phase4_and_5(conflict, Some(&suggestion), input)
                    {
                        resolved_contents.push(Some(resolved.proposal.merged_content.clone()));
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: Some(suggestion),
                            resolution: RegionResolutionStatus::Resolved {
                                content: resolved.proposal.merged_content.clone(),
                                method: resolved.proposal.pattern_name.clone(),
                                confidence: resolved.proposal.confidence,
                                resolved_in_phase: resolved.resolved_by,
                            },
                        });
                    } else {
                        let esc = self.build_escalation(
                            input,
                            conflict,
                            Some(suggestion.clone()),
                            EscalationReason::LowConfidence,
                        );
                        escalations.push(esc);
                        resolved_contents.push(None);
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: Some(suggestion),
                            resolution: RegionResolutionStatus::Escalated {
                                reason: EscalationReason::LowConfidence,
                                recommendation:
                                    "Pattern matched with low confidence — review suggestion"
                                        .to_string(),
                            },
                        });
                    }
                }
                PatternResult::NoMatch => {
                    if conflict.requires_review {
                        let esc = self.build_escalation(
                            input,
                            conflict,
                            None,
                            EscalationReason::DeleteVsModify,
                        );
                        escalations.push(esc);
                        resolved_contents.push(None);
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: None,
                            resolution: RegionResolutionStatus::Escalated {
                                reason: EscalationReason::DeleteVsModify,
                                recommendation:
                                    "Review whether deletion or modification is correct"
                                        .to_string(),
                            },
                        });
                        continue;
                    }

                    if let Some(resolved) = self.try_phase4_and_5(conflict, None, input) {
                        resolved_contents.push(Some(resolved.proposal.merged_content.clone()));
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: None,
                            resolution: RegionResolutionStatus::Resolved {
                                content: resolved.proposal.merged_content.clone(),
                                method: resolved.proposal.pattern_name.clone(),
                                confidence: resolved.proposal.confidence,
                                resolved_in_phase: resolved.resolved_by,
                            },
                        });
                    } else {
                        let esc = self.build_escalation(
                            input,
                            conflict,
                            None,
                            EscalationReason::NoPatternMatch,
                        );
                        escalations.push(esc);
                        resolved_contents.push(None);
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: None,
                            resolution: RegionResolutionStatus::Escalated {
                                reason: EscalationReason::NoPatternMatch,
                                recommendation:
                                    "Manual review required — no deterministic pattern matched"
                                        .to_string(),
                            },
                        });
                    }
                }
            }
        }
        timings.phase3_deterministic_ms = phase3_start.elapsed().as_millis() as u64;

        // === Assemble merged content ===
        // Re-run diff3 once here to get the line regions needed for rebuild.
        // (Phase 1 ran diff3 internally; a future optimization can share the result.)
        let fully_resolved = escalations.is_empty();
        let merged_content = if fully_resolved {
            let line_regions = match super::three_way_merge(&input.base, &input.left, &input.right)
            {
                super::FileMergeResult::Clean(content) => {
                    return PipelineOutput {
                        file_path: input.file_path.clone(),
                        analyzer_used: analyzer_used.clone(),
                        fully_resolved: true,
                        merged_content: Some(content),
                        resolutions: region_outcomes,
                        escalations,
                        verification: None,
                        phase_timings: timings,
                    }
                }
                super::FileMergeResult::Conflict(regions) => regions,
            };
            Some(self.assemble_merged_content(input, &line_regions, &resolved_contents))
        } else {
            None
        };

        // === Phase 6: Verification ===
        let verification = if self.config.enable_phase6_verification {
            if let Some(ref content) = merged_content {
                let phase6_start = Instant::now();
                let result = self.verifier.verify(content, &input.file_path);
                timings.phase6_verification_ms = phase6_start.elapsed().as_millis() as u64;

                if result.verdict == VerificationVerdict::Failed {
                    escalations.push(EscalationRecord {
                        file_path: input.file_path.clone(),
                        conflict_type: ConflictType::BothModified,
                        base_content: input.base.clone(),
                        left_content: input.left.clone(),
                        right_content: input.right.clone(),
                        left_agent: input.left_spec.clone(),
                        right_agent: input.right_spec.clone(),
                        phase3_suggestion: None,
                        reason: EscalationReason::VerificationFailed,
                        recommended_action: format!(
                            "Verification failed: {}",
                            result.warnings.join("; ")
                        ),
                    });
                    timings.total_ms = pipeline_start.elapsed().as_millis() as u64;
                    return PipelineOutput {
                        file_path: input.file_path.clone(),
                        analyzer_used: analyzer_used.clone(),
                        fully_resolved: false,
                        merged_content: None,
                        resolutions: region_outcomes,
                        escalations,
                        verification: Some(result),
                        phase_timings: timings,
                    };
                }
                Some(result)
            } else {
                None
            }
        } else {
            None
        };

        timings.total_ms = pipeline_start.elapsed().as_millis() as u64;

        PipelineOutput {
            file_path: input.file_path.clone(),
            analyzer_used,
            fully_resolved,
            merged_content,
            resolutions: region_outcomes,
            escalations,
            verification,
            phase_timings: timings,
        }
    }

    // -----------------------------------------------------------------------
    // Phases 4 & 5: Higher-level resolution (stub wiring)
    // -----------------------------------------------------------------------

    fn try_phase4_and_5(
        &self,
        conflict: &ClassifiedConflict,
        suggestion: Option<&ResolutionProposal>,
        input: &PipelineInput,
    ) -> Option<HigherPhaseResult> {
        if self.config.enable_phase4_spec_aware {
            if let Some(ref ctx) = input.spec_context {
                if let Some(proposal) = self.spec_resolver.resolve(conflict, suggestion, ctx) {
                    if proposal.confidence >= self.config.thresholds.auto_resolve {
                        return Some(HigherPhaseResult {
                            resolved_by: 4,
                            proposal,
                        });
                    }
                }
            }
        }

        if self.config.enable_phase5_llm {
            if let Some(proposal) =
                self.llm_resolver
                    .resolve(conflict, suggestion, &input.file_path)
            {
                if proposal.confidence >= self.config.thresholds.auto_resolve {
                    return Some(HigherPhaseResult {
                        resolved_by: 5,
                        proposal,
                    });
                }
            }
        }

        None
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn build_escalation(
        &self,
        input: &PipelineInput,
        conflict: &ClassifiedConflict,
        suggestion: Option<ResolutionProposal>,
        reason: EscalationReason,
    ) -> EscalationRecord {
        let base_content: String = conflict
            .region
            .base_units
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let left_content: String = conflict
            .region
            .left_units
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let right_content: String = conflict
            .region
            .right_units
            .iter()
            .map(|u| u.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let recommended_action = match &reason {
            EscalationReason::DeleteVsModify => {
                "Review whether deletion or modification is correct".to_string()
            }
            EscalationReason::NoPatternMatch => {
                "Manual review required — no deterministic pattern matched".to_string()
            }
            EscalationReason::LowConfidence => {
                "Pattern matched with low confidence — review suggestion".to_string()
            }
            EscalationReason::VerificationFailed => "Merged output failed verification".to_string(),
            EscalationReason::InternalError(msg) => {
                format!("Internal error: {msg}")
            }
            EscalationReason::ConflictingSpecs => {
                "Spec-aware resolution found conflicting spec claims".to_string()
            }
            EscalationReason::LowLlmConfidence => "LLM confidence was below threshold".to_string(),
            EscalationReason::LlmSanityCheckFailed => "LLM sanity check failed".to_string(),
        };

        EscalationRecord {
            file_path: input.file_path.clone(),
            conflict_type: conflict.conflict_type,
            base_content,
            left_content,
            right_content,
            left_agent: input.left_spec.clone(),
            right_agent: input.right_spec.clone(),
            phase3_suggestion: suggestion,
            reason,
            recommended_action,
        }
    }

    /// Assemble the final merged content from resolved regions.
    ///
    /// Accepts pre-computed line regions from diff3 to avoid redundant calls.
    fn assemble_merged_content(
        &self,
        input: &PipelineInput,
        line_regions: &[super::ConflictRegion],
        resolved_contents: &[Option<String>],
    ) -> String {
        let resolutions: Vec<super::RegionResolution> = resolved_contents
            .iter()
            .map(|content_opt| {
                let lines = match content_opt {
                    Some(content) => content.lines().map(|l| l.to_string()).collect(),
                    None => vec![],
                };
                super::RegionResolution {
                    lines,
                    class: super::ConflictClass::BothModified,
                    method: "v2-pipeline".to_string(),
                    confidence: content_opt.as_ref().map(|_| 1.0).unwrap_or(0.0),
                }
            })
            .collect();

        super::rebuild_with_resolutions(
            &input.base,
            &input.left,
            &input.right,
            line_regions,
            &resolutions,
        )
    }
}

impl Default for ConvergencePipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

struct HigherPhaseResult {
    resolved_by: u8,
    proposal: ResolutionProposal,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_input(file_path: &str, base: &str, left: &str, right: &str) -> PipelineInput {
        PipelineInput {
            file_path: file_path.to_string(),
            base: base.to_string(),
            left: left.to_string(),
            right: right.to_string(),
            left_spec: "spec-a".to_string(),
            right_spec: "spec-b".to_string(),
            spec_context: None,
        }
    }

    #[test]
    fn test_pipeline_clean_merge_no_conflicts() {
        let pipeline = ConvergencePipeline::new();
        let input = make_input("test.py", "base\n", "base\nfoo\n", "base\n");

        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        assert!(output.merged_content.is_some());
        assert!(output.escalations.is_empty());
    }

    #[test]
    fn test_pipeline_identical_changes_clean() {
        let pipeline = ConvergencePipeline::new();
        let input = make_input("test.py", "old\n", "new\n", "new\n");

        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        assert_eq!(output.merged_content.as_deref(), Some("new\n"));
        assert!(output.escalations.is_empty());
    }

    #[test]
    fn test_pipeline_disjoint_imports_auto_resolved() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(
            output.fully_resolved,
            "disjoint imports should auto-resolve"
        );
        let content = output.merged_content.unwrap();
        assert!(
            content.contains("import json"),
            "should contain json import"
        );
        assert!(content.contains("import sys"), "should contain sys import");
        assert!(content.contains("import os"), "should contain os import");
    }

    #[test]
    fn test_pipeline_delete_vs_modify_always_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "line1\noriginal\nline3\n";
        let left = "line1\nline3\n";
        let right = "line1\nmodified\nline3\n";

        let input = make_input("test.txt", base, left, right);
        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "DeleteVsModify must not auto-resolve"
        );
        assert!(
            !output.escalations.is_empty(),
            "should produce escalation records"
        );
        assert_eq!(
            output.escalations[0].reason,
            EscalationReason::DeleteVsModify
        );
    }

    #[test]
    fn test_pipeline_both_modified_no_pattern_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "value = 1\n";
        let left = "value = 2\n";
        let right = "value = 3\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "conflicting scalar changes should escalate"
        );
        assert!(!output.escalations.is_empty());
    }

    #[test]
    fn test_pipeline_non_overlapping_definitions_auto_resolved() {
        let pipeline = ConvergencePipeline::new();
        let base = "class Base:\n    pass\n";
        let left = "class Base:\n    pass\nclass Inventory:\n    name = 'item'\n";
        let right = "class Base:\n    pass\nclass Order:\n    total = 0\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(
            output.fully_resolved,
            "non-overlapping definitions should auto-resolve: escalations = {:?}",
            output.escalations
        );
        let content = output.merged_content.unwrap();
        assert!(content.contains("Inventory"), "should contain Inventory");
        assert!(content.contains("Order"), "should contain Order");
    }

    #[test]
    fn test_pipeline_timings_populated() {
        let pipeline = ConvergencePipeline::new();
        let input = make_input("test.py", "a\n", "b\n", "c\n");
        let output = pipeline.run(&input);

        assert!(
            output.phase_timings.total_ms < 5000,
            "should complete quickly"
        );
    }

    #[test]
    fn test_pipeline_verification_runs_on_resolved() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        assert!(
            output.verification.is_some(),
            "verification should run on resolved content"
        );
    }

    #[test]
    fn test_pipeline_config_defaults() {
        let config = PipelineConfig::default();
        assert!(!config.enable_phase4_spec_aware);
        assert!(!config.enable_phase5_llm);
        assert!(config.enable_phase6_verification);
    }

    #[test]
    fn test_pipeline_uses_phase1_module() {
        let pipeline = ConvergencePipeline::new();
        let input = make_input(
            "models.py",
            "import os\n",
            "import os\nimport json\n",
            "import os\nimport sys\n",
        );
        let output = pipeline.run(&input);

        assert_eq!(
            output.analyzer_used, "python",
            "should dispatch to Python analyzer"
        );
    }

    #[test]
    fn test_pipeline_region_outcome_uses_types_rs_format() {
        let pipeline = ConvergencePipeline::new();
        let base = "value = 1\n";
        let left = "value = 2\n";
        let right = "value = 3\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(!output.resolutions.is_empty());
        let outcome = &output.resolutions[0];
        assert!(
            matches!(outcome.resolution, RegionResolutionStatus::Escalated { .. }),
            "unresolvable conflict should produce Escalated status"
        );
        assert!(
            matches!(
                outcome.classified.conflict_type,
                ConflictType::BothModified | ConflictType::BothInserted
            ),
            "expected BothModified or BothInserted, got {:?}",
            outcome.classified.conflict_type
        );
    }

    #[test]
    fn test_pipeline_auto_resolved_region_outcome() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        let resolved_outcomes: Vec<_> = output
            .resolutions
            .iter()
            .filter(|o| matches!(o.resolution, RegionResolutionStatus::Resolved { .. }))
            .collect();
        assert!(
            !resolved_outcomes.is_empty(),
            "should have at least one Resolved outcome"
        );

        if let RegionResolutionStatus::Resolved {
            resolved_in_phase,
            confidence,
            ..
        } = &resolved_outcomes[0].resolution
        {
            assert_eq!(*resolved_in_phase, 3, "should be resolved in Phase 3");
            assert!(*confidence >= 0.85, "should have high confidence");
        }
    }
}
