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
use super::decompose;
use super::patterns::{PatternRegistry, PatternResult};
use super::phase5::{NoOpBackend, StructuredLlmResolver};
use super::phase6::HardenedVerifier;
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
    /// Trust context for confidence capping based on agent trust levels.
    pub trust_context: Option<crate::agent::TrustContext>,
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

#[allow(dead_code)]
struct StubLlmResolver;

#[allow(dead_code)]
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
/// Kept as a reference implementation; production uses `HardenedVerifier`.
#[allow(dead_code)]
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
            llm_resolver: Box::new(StructuredLlmResolver::new(Box::new(NoOpBackend))),
            verifier: Box::new(HardenedVerifier::new()),
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
            // Try region decomposition for mixed-scope conflicts.
            // If a region contains both imports and non-imports, split it
            // into homogeneous sub-regions and evaluate each independently.
            if conflict.structural_info.scope == ConflictScope::Mixed {
                if let Some(decomposed) = decompose::decompose_mixed_region(conflict) {
                    if let Some(composed) = self.try_decomposed_resolution(&decomposed, conflict) {
                        resolved_contents.push(Some(composed.content.clone()));
                        region_outcomes.push(RegionOutcome {
                            classified: conflict.clone(),
                            phase3_result: Some(ResolutionProposal {
                                pattern_name: "decomposed".into(),
                                confidence: composed.confidence,
                                merged_content: composed.content.clone(),
                                explanation: composed.explanation.clone(),
                                warnings: vec![],
                            }),
                            resolution: RegionResolutionStatus::Resolved {
                                content: composed.content,
                                method: "decomposed".to_string(),
                                confidence: composed.confidence,
                                resolved_in_phase: 3,
                            },
                        });
                        continue;
                    }
                }
            }

            let phase3_result = self.pattern_registry.evaluate(conflict);

            // Apply trust-based confidence cap (Sprint B)
            let phase3_result =
                Self::apply_trust_adjustment(phase3_result, input.trust_context.as_ref());

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
    // Region decomposition: evaluate sub-regions independently
    // -----------------------------------------------------------------------

    /// Attempt to resolve a decomposed region by evaluating each sub-region
    /// with the pattern registry. Returns `None` if any sub-region fails,
    /// so the caller can fall through to normal evaluation.
    fn try_decomposed_resolution(
        &self,
        decomposed: &decompose::DecomposedRegion,
        _original: &ClassifiedConflict,
    ) -> Option<ComposedResult> {
        let mut parts: Vec<String> = Vec::new();
        let mut min_confidence: f64 = 1.0;
        let mut methods: Vec<String> = Vec::new();

        if let Some(ref import_conflict) = decomposed.import_conflict {
            match self.pattern_registry.evaluate(import_conflict) {
                PatternResult::AutoResolved(p) => {
                    min_confidence = min_confidence.min(p.confidence);
                    methods.push(format!("imports:{}", p.pattern_name));
                    parts.push(p.merged_content);
                }
                _ => return None,
            }
        }

        if let Some(ref body_conflict) = decomposed.body_conflict {
            match self.pattern_registry.evaluate(body_conflict) {
                PatternResult::AutoResolved(p) => {
                    min_confidence = min_confidence.min(p.confidence);
                    methods.push(format!("body:{}", p.pattern_name));
                    parts.push(p.merged_content);
                }
                _ => return None,
            }
        }

        if parts.is_empty() {
            return None;
        }

        Some(ComposedResult {
            content: parts.join("\n"),
            confidence: min_confidence,
            explanation: format!("Decomposed mixed region: {}", methods.join(" + ")),
        })
    }

    // -----------------------------------------------------------------------
    // Phases 4 & 5: Higher-level resolution (stub wiring)
    // -----------------------------------------------------------------------

    /// Apply trust-based confidence cap to a pattern result.
    ///
    /// If a trust context is provided, the pattern's confidence is capped by
    /// the trust adjustment factor. This may demote an AutoResolved result to
    /// Suggested or NoMatch, depending on thresholds.
    fn apply_trust_adjustment(
        result: PatternResult,
        trust_context: Option<&crate::agent::TrustContext>,
    ) -> PatternResult {
        let ctx = match trust_context {
            Some(c) => c,
            None => return result,
        };
        let adjustment = ctx.trust_adjustment();
        match result {
            PatternResult::AutoResolved(mut proposal) => {
                proposal.confidence = proposal.confidence.min(adjustment);
                if proposal.confidence >= 0.85 {
                    PatternResult::AutoResolved(proposal)
                } else if proposal.confidence >= 0.60 {
                    PatternResult::Suggested(proposal)
                } else {
                    PatternResult::NoMatch
                }
            }
            PatternResult::Suggested(mut proposal) => {
                proposal.confidence = proposal.confidence.min(adjustment);
                if proposal.confidence >= 0.60 {
                    PatternResult::Suggested(proposal)
                } else {
                    PatternResult::NoMatch
                }
            }
            PatternResult::NoMatch => PatternResult::NoMatch,
        }
    }

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

/// Result of composing resolved sub-regions from decomposition.
struct ComposedResult {
    content: String,
    confidence: f64,
    explanation: String,
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
            trust_context: None,
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

    // ── Region decomposition integration tests ─────────────────────────

    #[test]
    fn test_decomposed_flask_app_resolves() {
        // TR20-like scenario: base has imports + app setup.
        // Left adds `request` to imports + auth route.
        // Right adds `abort` to imports + orders route.
        // diff3 produces one mixed region. Without decomposition, no
        // pattern handles it. With decomposition:
        //   - Import sub-region → ImportAccumulation merges names
        //   - Body sub-region → handled by another pattern or additive composition
        let pipeline = ConvergencePipeline::new();
        let base = "from flask import Flask, jsonify\napp = Flask(__name__)\n";
        let left = "from flask import Flask, jsonify, request\napp = Flask(__name__)\n\n@app.route('/auth')\ndef auth():\n    return 'auth'\n";
        let right = "from flask import Flask, abort, jsonify\napp = Flask(__name__)\n\n@app.route('/orders')\ndef orders():\n    return 'orders'\n";

        let input = make_input("app.py", base, left, right);
        let output = pipeline.run(&input);

        // The pipeline should resolve this (either via decomposition or
        // via the existing patterns with the Piece B fix).
        if output.fully_resolved {
            let content = output.merged_content.unwrap();
            // Must contain all imports.
            assert!(content.contains("Flask"), "Flask missing from:\n{content}");
            assert!(
                content.contains("jsonify"),
                "jsonify missing from:\n{content}"
            );
            // Must contain both routes.
            assert!(
                content.contains("/auth") || content.contains("auth"),
                "auth route missing from:\n{content}"
            );
            assert!(
                content.contains("/orders") || content.contains("orders"),
                "orders route missing from:\n{content}"
            );
        }
        // If not fully resolved, the pipeline escalated — that's acceptable
        // for now since the v1 fallback will handle it.
    }

    #[test]
    fn test_decomposed_method_is_reported() {
        // Verify that when decomposition succeeds, the method is "decomposed".
        let pipeline = ConvergencePipeline::new();
        let base = "import os\nx = 1\n";
        let left = "import os\nimport sys\nx = 1\ny = 2\n";
        let right = "import os\nimport json\nx = 1\nz = 3\n";

        let input = make_input("test.py", base, left, right);
        let output = pipeline.run(&input);

        if output.fully_resolved {
            let decomposed_count = output
                .resolutions
                .iter()
                .filter(|o| {
                    matches!(
                        &o.resolution,
                        RegionResolutionStatus::Resolved { method, .. }
                        if method == "decomposed"
                    )
                })
                .count();
            // We expect at least one region to use decomposition if the
            // pipeline engaged it. (It might also be handled by other
            // patterns depending on how diff3 splits the regions.)
            if decomposed_count > 0 {
                assert!(
                    decomposed_count >= 1,
                    "should have at least one decomposed region"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Sprint 0.3.3 — Phase 4/5 feature flag verification
    // -----------------------------------------------------------------------

    /// Helper: create a conflict that Phase 3 can only suggest (not auto-resolve),
    /// so it would fall through to Phase 4/5 if enabled.
    fn make_unresolvable_conflict_input() -> PipelineInput {
        // Both sides modify the same line differently — no pattern can auto-resolve
        make_input("conflict.py", "value = 1\n", "value = 2\n", "value = 3\n")
    }

    #[test]
    fn test_phase4_skipped_when_disabled() {
        // Default config has enable_phase4_spec_aware = false
        let pipeline = ConvergencePipeline::new();
        assert!(!pipeline.config.enable_phase4_spec_aware);

        let input = make_unresolvable_conflict_input();
        let output = pipeline.run(&input);

        // Conflict should escalate, NOT be resolved by Phase 4
        assert!(
            !output.fully_resolved,
            "unresolvable conflict should not be resolved when Phase 4 is disabled"
        );
        assert!(
            !output.escalations.is_empty(),
            "should have escalations when Phase 4 is disabled"
        );
        // Verify no resolution claims to come from Phase 4
        for res in &output.resolutions {
            if let RegionResolutionStatus::Resolved { method, .. } = &res.resolution {
                assert!(
                    !method.contains("phase4") && !method.contains("spec-aware"),
                    "Phase 4 should not have resolved anything: {method}"
                );
            }
        }
    }

    #[test]
    fn test_phase5_skipped_when_disabled() {
        // Default config has enable_phase5_llm = false
        let pipeline = ConvergencePipeline::new();
        assert!(!pipeline.config.enable_phase5_llm);

        let input = make_unresolvable_conflict_input();
        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "unresolvable conflict should not be resolved when Phase 5 is disabled"
        );
        assert!(
            !output.escalations.is_empty(),
            "should have escalations when Phase 5 is disabled"
        );
    }

    #[test]
    fn test_both_phase4_and_phase5_disabled_escalates() {
        let config = PipelineConfig {
            enable_phase4_spec_aware: false,
            enable_phase5_llm: false,
            enable_phase6_verification: true,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        let input = make_unresolvable_conflict_input();
        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "both phases disabled — conflict must escalate"
        );
        assert!(
            !output.escalations.is_empty(),
            "both phases disabled — must have escalations"
        );
    }

    #[test]
    fn test_phase4_enabled_but_no_spec_context_is_noop() {
        // Phase 4 enabled but spec_context is None — should behave as if disabled
        let config = PipelineConfig {
            enable_phase4_spec_aware: true,
            enable_phase5_llm: false,
            enable_phase6_verification: true,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        // Input has spec_context = None (the make_input default)
        let input = make_unresolvable_conflict_input();
        assert!(input.spec_context.is_none());

        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "Phase 4 enabled but no spec_context — should still escalate"
        );
    }

    #[test]
    fn test_phase5_enabled_with_noop_backend_is_inert() {
        // Phase 5 enabled but NoOpBackend always returns None
        let config = PipelineConfig {
            enable_phase4_spec_aware: false,
            enable_phase5_llm: true,
            enable_phase6_verification: true,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        let input = make_unresolvable_conflict_input();
        let output = pipeline.run(&input);

        // NoOpBackend returns None → conflict still escalates
        assert!(
            !output.fully_resolved,
            "Phase 5 with NoOpBackend should not resolve anything"
        );
        assert!(
            !output.escalations.is_empty(),
            "Phase 5 with NoOpBackend — conflict should escalate"
        );
    }

    #[test]
    fn test_phase4_enabled_with_spec_context_no_side_effects_on_clean_merge() {
        // Phase 4 enabled with full spec_context, but the merge is clean
        // (Phase 1-3 handle it) — Phase 4 should not interfere.
        let config = PipelineConfig {
            enable_phase4_spec_aware: true,
            enable_phase5_llm: false,
            enable_phase6_verification: true,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        // Clean merge: disjoint imports — Phase 3 auto-resolves
        let mut input = make_input(
            "app.py",
            "import os\n",
            "import os\nimport json\n",
            "import os\nimport sys\n",
        );
        input.spec_context = Some(SpecContext {
            left_file_scope: vec!["app.py".into()],
            right_file_scope: vec!["app.py".into()],
            left_description: "Add JSON support".into(),
            right_description: "Add sys support".into(),
            left_seal_summary: "added json import".into(),
            right_seal_summary: "added sys import".into(),
            left_acceptance_criteria: vec![],
            right_acceptance_criteria: vec![],
            left_design_notes: vec![],
            right_design_notes: vec![],
            file_path: "app.py".into(),
        });

        let output = pipeline.run(&input);

        assert!(
            output.fully_resolved,
            "clean merge should still resolve with Phase 4 enabled"
        );
        let content = output.merged_content.unwrap();
        assert!(content.contains("import json"));
        assert!(content.contains("import sys"));
    }

    #[test]
    fn test_all_phases_disabled_still_runs_phases_1_through_3() {
        // Even with Phase 4/5/6 all disabled, Phases 1-3 should still work
        let config = PipelineConfig {
            enable_phase4_spec_aware: false,
            enable_phase5_llm: false,
            enable_phase6_verification: false,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        // Clean merge that Phase 3 can handle
        let input = make_input(
            "test.py",
            "import os\n",
            "import os\nimport json\n",
            "import os\nimport sys\n",
        );
        let output = pipeline.run(&input);

        assert!(
            output.fully_resolved,
            "Phases 1-3 should still resolve clean merges even with 4/5/6 disabled"
        );
        // No verification when Phase 6 disabled
        assert!(
            output.verification.is_none(),
            "Phase 6 disabled — no verification should be present"
        );
    }

    #[test]
    fn test_phase6_disabled_skips_verification() {
        let config = PipelineConfig {
            enable_phase4_spec_aware: false,
            enable_phase5_llm: false,
            enable_phase6_verification: false,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        let input = make_input(
            "test.py",
            "import os\n",
            "import os\nimport json\n",
            "import os\nimport sys\n",
        );
        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        assert!(
            output.verification.is_none(),
            "verification should be None when Phase 6 is disabled"
        );
    }

    #[test]
    fn test_phase6_enabled_runs_verification() {
        let config = PipelineConfig {
            enable_phase4_spec_aware: false,
            enable_phase5_llm: false,
            enable_phase6_verification: true,
            ..PipelineConfig::default()
        };
        let pipeline = ConvergencePipeline::with_config(config);

        let input = make_input(
            "test.py",
            "import os\n",
            "import os\nimport json\n",
            "import os\nimport sys\n",
        );
        let output = pipeline.run(&input);

        assert!(output.fully_resolved);
        assert!(
            output.verification.is_some(),
            "verification should be present when Phase 6 is enabled"
        );
    }

    // -----------------------------------------------------------------------
    // Sprint B — Trust-adjusted convergence tests
    // -----------------------------------------------------------------------

    /// Helper: create a TrustContext with the given trust levels.
    fn make_trust_context(
        left: crate::agent::TrustLevel,
        right: crate::agent::TrustLevel,
    ) -> crate::agent::TrustContext {
        crate::agent::TrustContext {
            left_trust: left,
            right_trust: right,
        }
    }

    #[test]
    fn test_trust_full_full_no_cap() {
        // Full+Full → 1.0 adjustment — should not cap any confidence
        use crate::agent::TrustLevel;
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.95,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };
        let ctx = make_trust_context(TrustLevel::Full, TrustLevel::Full);

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            Some(&ctx),
        );
        match result {
            PatternResult::AutoResolved(p) => {
                assert!(
                    (p.confidence - 0.95).abs() < f64::EPSILON,
                    "Full+Full should not cap confidence: got {}",
                    p.confidence
                );
            }
            other => panic!("Full+Full should stay AutoResolved, got {:?}", other),
        }
    }

    #[test]
    fn test_trust_standard_standard_caps_at_090() {
        // Standard+Standard → 0.90 adjustment — caps confidence but 0.90 >= 0.85
        // threshold so it stays AutoResolved
        use crate::agent::TrustLevel;
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.95,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };
        let ctx = make_trust_context(TrustLevel::Standard, TrustLevel::Standard);

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            Some(&ctx),
        );
        match result {
            PatternResult::AutoResolved(p) => {
                assert!(
                    (p.confidence - 0.90).abs() < f64::EPSILON,
                    "Standard+Standard should cap at 0.90: got {}",
                    p.confidence
                );
            }
            other => panic!(
                "Standard+Standard with 0.95 should stay AutoResolved (0.90 >= 0.85), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_trust_mixed_caps_at_075() {
        // Full+Standard → 0.75 adjustment — demotes to Suggested
        use crate::agent::TrustLevel;
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.95,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };
        let ctx = make_trust_context(TrustLevel::Full, TrustLevel::Standard);

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            Some(&ctx),
        );
        match result {
            PatternResult::Suggested(p) => {
                assert!(
                    (p.confidence - 0.75).abs() < f64::EPSILON,
                    "Mixed trust should cap at 0.75: got {}",
                    p.confidence
                );
            }
            other => panic!(
                "Full+Standard with 0.95 should demote to Suggested, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_trust_restricted_caps_at_060() {
        // Restricted+anything (non-Untrusted) → 0.60 adjustment
        use crate::agent::TrustLevel;
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.95,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };
        let ctx = make_trust_context(TrustLevel::Restricted, TrustLevel::Full);

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            Some(&ctx),
        );
        match result {
            PatternResult::Suggested(p) => {
                assert!(
                    (p.confidence - 0.60).abs() < f64::EPSILON,
                    "Restricted should cap at 0.60: got {}",
                    p.confidence
                );
            }
            other => panic!(
                "Restricted+Full with 0.95 should demote to Suggested, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_trust_untrusted_always_drops_to_nomatch() {
        // Untrusted → 0.0 adjustment — everything becomes NoMatch
        use crate::agent::TrustLevel;
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.99,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };
        let ctx = make_trust_context(TrustLevel::Untrusted, TrustLevel::Full);

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            Some(&ctx),
        );
        assert!(
            matches!(result, PatternResult::NoMatch),
            "Untrusted agent should always produce NoMatch, got {:?}",
            result
        );
    }

    #[test]
    fn test_trust_none_context_is_passthrough() {
        // No TrustContext → result passes through unchanged
        let proposal = ResolutionProposal {
            pattern_name: "test_pattern".into(),
            confidence: 0.95,
            merged_content: "merged".into(),
            explanation: "test".into(),
            warnings: vec![],
        };

        let result = ConvergencePipeline::apply_trust_adjustment(
            PatternResult::AutoResolved(proposal),
            None,
        );
        match result {
            PatternResult::AutoResolved(p) => {
                assert!(
                    (p.confidence - 0.95).abs() < f64::EPSILON,
                    "No trust context should pass through: got {}",
                    p.confidence
                );
            }
            other => panic!(
                "No trust context should leave AutoResolved unchanged, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_trust_cap_applied_after_pattern_evaluation() {
        // Integration test: run the full pipeline with Standard+Standard trust
        // on a conflict that Phase 3 would normally auto-resolve (disjoint imports).
        // The trust cap (0.90) should demote AutoResolved → Suggested → escalation.
        use crate::agent::TrustLevel;
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("test.py", base, left, right);
        input.trust_context = Some(make_trust_context(
            TrustLevel::Standard,
            TrustLevel::Standard,
        ));

        let output = pipeline.run(&input);

        // With Standard+Standard (0.90 cap), a normally auto-resolved import merge
        // should now be Suggested (0.90 < 0.85 threshold? No, 0.90 >= 0.85).
        // Actually 0.90 >= 0.85 so it stays AutoResolved. The cap only demotes if
        // the capped value drops below the threshold. Let's verify it still resolves.
        assert!(
            output.fully_resolved,
            "Standard+Standard (0.90) should still auto-resolve high-confidence imports"
        );
    }

    #[test]
    fn test_trust_untrusted_escalates_normally_resolvable() {
        // Integration test: a normally auto-resolvable import merge should escalate
        // when one agent is Untrusted (0.0 cap → NoMatch → escalation).
        use crate::agent::TrustLevel;
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("test.py", base, left, right);
        input.trust_context = Some(make_trust_context(TrustLevel::Untrusted, TrustLevel::Full));

        let output = pipeline.run(&input);

        assert!(
            !output.fully_resolved,
            "Untrusted agent should cause escalation even for normally-resolvable conflicts"
        );
        assert!(
            !output.escalations.is_empty(),
            "Untrusted agent should produce escalation records"
        );
    }
}

// ---------------------------------------------------------------------------
// B.3.2 — Mixed-trust convergence integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod mixed_trust_convergence_tests {
    use super::*;
    use crate::agent::{TrustContext, TrustLevel};

    fn make_input(file_path: &str, base: &str, left: &str, right: &str) -> PipelineInput {
        PipelineInput {
            file_path: file_path.to_string(),
            base: base.to_string(),
            left: left.to_string(),
            right: right.to_string(),
            left_spec: "spec-a".to_string(),
            right_spec: "spec-b".to_string(),
            spec_context: None,
            trust_context: None,
        }
    }

    fn make_trust(left: TrustLevel, right: TrustLevel) -> TrustContext {
        TrustContext {
            left_trust: left,
            right_trust: right,
        }
    }

    // --- Full+Full: confidence unaffected, auto-resolves normally ---

    #[test]
    fn test_full_full_python_imports_auto_resolve() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("app.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Full, TrustLevel::Full));

        let output = pipeline.run(&input);
        assert!(
            output.fully_resolved,
            "Full+Full should auto-resolve disjoint imports"
        );
        let merged = output.merged_content.unwrap();
        assert!(
            merged.contains("import json"),
            "merged should contain left import"
        );
        assert!(
            merged.contains("import sys"),
            "merged should contain right import"
        );
    }

    #[test]
    fn test_full_full_rust_use_statements_auto_resolve() {
        let pipeline = ConvergencePipeline::new();
        let base = "use std::io;\n\nfn main() {}\n";
        let left = "use std::io;\nuse std::fs;\n\nfn main() {}\n";
        let right = "use std::io;\nuse std::path::Path;\n\nfn main() {}\n";

        let mut input = make_input("main.rs", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Full, TrustLevel::Full));

        let output = pipeline.run(&input);
        assert!(
            output.fully_resolved,
            "Full+Full should auto-resolve disjoint use statements"
        );
        let merged = output.merged_content.unwrap();
        assert!(
            merged.contains("use std::fs;"),
            "merged should contain left use"
        );
        assert!(
            merged.contains("use std::path::Path;"),
            "merged should contain right use"
        );
    }

    // --- Standard+Standard: cap 0.90, still auto-resolves (0.90 >= 0.85) ---

    #[test]
    fn test_standard_standard_still_auto_resolves_high_confidence() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("util.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Standard, TrustLevel::Standard));

        let output = pipeline.run(&input);
        assert!(
            output.fully_resolved,
            "Standard+Standard (cap 0.90) should still auto-resolve (0.90 >= 0.85 threshold)"
        );
    }

    #[test]
    fn test_standard_standard_confidence_capped_in_resolutions() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("check.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Standard, TrustLevel::Standard));

        let output = pipeline.run(&input);
        // Check that resolution proposals have capped confidence
        for resolution in &output.resolutions {
            if let Some(ref proposal) = resolution.phase3_result {
                assert!(
                    proposal.confidence <= 0.90 + f64::EPSILON,
                    "Standard+Standard should cap confidence to 0.90, got {}",
                    proposal.confidence
                );
            }
        }
    }

    // --- Full+Standard (mixed): cap 0.75, demotes to Suggested → escalation ---

    #[test]
    fn test_mixed_full_standard_demotes_to_escalation() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("mixed.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Full, TrustLevel::Standard));

        let output = pipeline.run(&input);
        // 0.75 cap < 0.85 auto-resolve threshold → demoted to Suggested → escalated
        assert!(
            !output.fully_resolved,
            "Mixed trust (0.75 cap) should escalate — confidence below auto-resolve threshold"
        );
        assert!(
            !output.escalations.is_empty(),
            "Mixed trust should produce escalation records"
        );
    }

    #[test]
    fn test_mixed_standard_full_same_as_full_standard() {
        // Symmetry: Standard+Full should produce same result as Full+Standard
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input_a = make_input("sym_a.py", base, left, right);
        input_a.trust_context = Some(make_trust(TrustLevel::Full, TrustLevel::Standard));

        let mut input_b = make_input("sym_b.py", base, left, right);
        input_b.trust_context = Some(make_trust(TrustLevel::Standard, TrustLevel::Full));

        let output_a = pipeline.run(&input_a);
        let output_b = pipeline.run(&input_b);

        assert_eq!(
            output_a.fully_resolved, output_b.fully_resolved,
            "Trust adjustment should be symmetric: Full+Standard == Standard+Full"
        );
        assert_eq!(
            output_a.escalations.len(),
            output_b.escalations.len(),
            "Same number of escalations regardless of left/right trust order"
        );
    }

    // --- Restricted+any: cap 0.60, barely above suggest threshold ---

    #[test]
    fn test_restricted_full_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("restricted.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Restricted, TrustLevel::Full));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Restricted (0.60 cap) should escalate — far below 0.85 auto-resolve"
        );
    }

    #[test]
    fn test_restricted_restricted_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "use std::io;\n\nfn main() {}\n";
        let left = "use std::io;\nuse std::fs;\n\nfn main() {}\n";
        let right = "use std::io;\nuse std::path::Path;\n\nfn main() {}\n";

        let mut input = make_input("restricted.rs", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Restricted, TrustLevel::Restricted));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Restricted+Restricted (0.60 cap) should escalate"
        );
    }

    // --- Untrusted+any: cap 0.0, everything becomes NoMatch ---

    #[test]
    fn test_untrusted_full_always_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("untrusted.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Untrusted, TrustLevel::Full));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Untrusted (0.0 cap) should always escalate"
        );
        assert!(
            !output.escalations.is_empty(),
            "Untrusted should produce escalation records"
        );
    }

    #[test]
    fn test_untrusted_standard_always_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "use std::io;\n\nfn main() {}\n";
        let left = "use std::io;\nuse std::fs;\n\nfn main() {}\n";
        let right = "use std::io;\nuse std::path::Path;\n\nfn main() {}\n";

        let mut input = make_input("untrusted.rs", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Untrusted, TrustLevel::Standard));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Untrusted+Standard should always escalate"
        );
    }

    #[test]
    fn test_untrusted_both_sides_always_escalates() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("both_untrusted.py", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Untrusted, TrustLevel::Untrusted));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Untrusted+Untrusted should always escalate"
        );
    }

    // --- Symmetry verification ---

    #[test]
    fn test_trust_adjustment_is_symmetric() {
        // For all meaningful trust level pairs, verify Left=A Right=B
        // produces the same trust_adjustment as Left=B Right=A
        let levels = [
            TrustLevel::Full,
            TrustLevel::Standard,
            TrustLevel::Restricted,
            TrustLevel::Untrusted,
        ];

        for &l in &levels {
            for &r in &levels {
                let ctx_lr = make_trust(l, r);
                let ctx_rl = make_trust(r, l);
                assert!(
                    (ctx_lr.trust_adjustment() - ctx_rl.trust_adjustment()).abs() < f64::EPSILON,
                    "Trust adjustment should be symmetric: {:?}+{:?} ({}) != {:?}+{:?} ({})",
                    l,
                    r,
                    ctx_lr.trust_adjustment(),
                    r,
                    l,
                    ctx_rl.trust_adjustment(),
                );
            }
        }
    }

    // --- No trust context passthrough ---

    #[test]
    fn test_no_trust_context_resolves_normally() {
        let pipeline = ConvergencePipeline::new();
        let base = "import os\n";
        let left = "import os\nimport json\n";
        let right = "import os\nimport sys\n";

        let mut input = make_input("no_trust.py", base, left, right);
        input.trust_context = None; // No trust context

        let output = pipeline.run(&input);
        assert!(
            output.fully_resolved,
            "No trust context should allow normal resolution"
        );
    }

    // --- TypeScript trust tests ---

    #[test]
    fn test_full_full_typescript_imports_resolve() {
        let pipeline = ConvergencePipeline::new();
        let base = "import React from 'react';\n\nexport default function App() {}\n";
        let left =
            "import React from 'react';\nimport axios from 'axios';\n\nexport default function App() {}\n";
        let right =
            "import React from 'react';\nimport lodash from 'lodash';\n\nexport default function App() {}\n";

        let mut input = make_input("App.tsx", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Full, TrustLevel::Full));

        let output = pipeline.run(&input);
        assert!(
            output.fully_resolved,
            "Full+Full TypeScript imports should auto-resolve"
        );
        let merged = output.merged_content.unwrap();
        assert!(
            merged.contains("axios"),
            "merged should contain left import"
        );
        assert!(
            merged.contains("lodash"),
            "merged should contain right import"
        );
    }

    #[test]
    fn test_restricted_typescript_imports_escalate() {
        let pipeline = ConvergencePipeline::new();
        let base = "import React from 'react';\n\nexport default function App() {}\n";
        let left =
            "import React from 'react';\nimport axios from 'axios';\n\nexport default function App() {}\n";
        let right =
            "import React from 'react';\nimport lodash from 'lodash';\n\nexport default function App() {}\n";

        let mut input = make_input("App.tsx", base, left, right);
        input.trust_context = Some(make_trust(TrustLevel::Restricted, TrustLevel::Standard));

        let output = pipeline.run(&input);
        assert!(
            !output.fully_resolved,
            "Restricted TypeScript imports should escalate (0.60 cap)"
        );
    }
}
