//! Additive Composition pattern.
//!
//! When both sides of a BothModified conflict preserved all base content
//! (or additively extended it, e.g. import line extensions) and each added
//! unique content, compose: base + left additions + right additions.
//!
//! Import lines receive special treatment: extending
//! `from flask import Flask, jsonify` to
//! `from flask import Flask, jsonify, request` counts as preserving
//! the base, and the merged result unions all imported names.

use super::Pattern;
use crate::convergence::types::{ClassifiedConflict, ConflictType, ResolutionProposal, UnitKind};
use std::collections::HashSet;

/// Compose base + left additions + right additions.
pub struct AdditiveComposition;

/// Extract imported names from an import line.
/// Handles Python `from X import a, b, c` and `import a, b`.
fn extract_import_names(content: &str) -> HashSet<String> {
    let trimmed = content.trim();
    if let Some(after) = trimmed.split(" import ").nth(1) {
        // `from X import a, b, c` or `import a, b`
        after
            .split(',')
            .map(|s| {
                let s = s.trim();
                // Handle `name as alias` — use the original name for matching.
                s.split(" as ").next().unwrap_or(s).trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        // Single `import X` without commas — the whole line is the name.
        HashSet::new()
    }
}

/// Extract the module source from an import line.
/// `from flask import ...` → `flask`
/// `import os` → `os`
fn extract_import_module(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix("from ") {
        rest.split_whitespace().next().map(|s| s.to_string())
    } else if let Some(rest) = trimmed.strip_prefix("import ") {
        rest.split(',').next().and_then(|s| {
            let s = s.trim();
            s.split(" as ").next().map(|n| n.trim().to_string())
        })
    } else {
        None
    }
}

/// Check if a side's import line preserves all names from a base import line
/// (possibly extending it with more names). Returns true if all base names
/// are present in the side's line.
fn import_is_preserved(base_content: &str, side_content: &str) -> bool {
    let base_module = extract_import_module(base_content);
    let side_module = extract_import_module(side_content);

    // Modules must match.
    if base_module != side_module || base_module.is_none() {
        return false;
    }

    let base_names = extract_import_names(base_content);
    let side_names = extract_import_names(side_content);

    // All base names must be present in the side.
    base_names.iter().all(|n| side_names.contains(n))
}

/// Merge import names from base, left, and right into a single import line.
/// Returns a combined import line with the union of all names.
fn merge_import_line(base: &str, left: &str, right: &str) -> String {
    let module = extract_import_module(base).unwrap_or_default();
    let prefix = base.trim();

    // Determine the import style: `from X import ...` or `import X, Y`.
    let is_from_import = prefix.starts_with("from ");

    // Collect all names from all three versions.
    let base_names = extract_import_names(base);
    let left_names = extract_import_names(left);
    let right_names = extract_import_names(right);

    let mut all_names: Vec<String> = base_names
        .iter()
        .chain(left_names.iter())
        .chain(right_names.iter())
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    all_names.sort();

    if is_from_import {
        format!("from {} import {}", module, all_names.join(", "))
    } else {
        format!("import {}", all_names.join(", "))
    }
}

impl Pattern for AdditiveComposition {
    fn name(&self) -> &str {
        "additive_composition"
    }

    fn applies_to(&self) -> &[ConflictType] {
        &[ConflictType::BothModified]
    }

    fn matches(&self, conflict: &ClassifiedConflict) -> bool {
        // Need non-empty base content to determine additions.
        !conflict.region.base_units.is_empty()
    }

    fn resolve(&self, conflict: &ClassifiedConflict) -> Option<ResolutionProposal> {
        let base_units = &conflict.region.base_units;
        let left_units = &conflict.region.left_units;
        let right_units = &conflict.region.right_units;

        let left_content: Vec<String> = left_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let right_content: Vec<String> = right_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let left_set: HashSet<&str> = left_content.iter().map(|s| s.as_str()).collect();
        let right_set: HashSet<&str> = right_content.iter().map(|s| s.as_str()).collect();

        // Track which base imports were "extended" (preserved with additional names)
        // so we can merge them properly in the composition step.
        let mut left_import_extensions: Vec<(usize, String)> = Vec::new(); // (base_idx, side_content)
        let mut right_import_extensions: Vec<(usize, String)> = Vec::new();

        // Both sides must contain all base content — either exactly or as
        // an additive extension (for imports).
        for (base_idx, base_unit) in base_units.iter().enumerate() {
            let base_trimmed = base_unit.content.trim().to_string();
            if base_trimmed.is_empty() {
                continue;
            }

            let left_exact = left_set.contains(base_trimmed.as_str());
            let right_exact = right_set.contains(base_trimmed.as_str());

            if left_exact && right_exact {
                // Both sides have this base line exactly — fully preserved.
                continue;
            }

            // For import units, check if the base import was extended.
            if base_unit.kind == UnitKind::Import {
                let left_ok = left_exact
                    || left_content
                        .iter()
                        .any(|lc| import_is_preserved(&base_trimmed, lc));
                let right_ok = right_exact
                    || right_content
                        .iter()
                        .any(|rc| import_is_preserved(&base_trimmed, rc));

                if left_ok && right_ok {
                    // Record extensions for merging.
                    if !left_exact {
                        if let Some(ext) = left_content
                            .iter()
                            .find(|lc| import_is_preserved(&base_trimmed, lc))
                        {
                            left_import_extensions.push((base_idx, ext.clone()));
                        }
                    }
                    if !right_exact {
                        if let Some(ext) = right_content
                            .iter()
                            .find(|rc| import_is_preserved(&base_trimmed, rc))
                        {
                            right_import_extensions.push((base_idx, ext.clone()));
                        }
                    }
                    continue;
                }
            }

            // Not preserved (exact or extended) — pattern doesn't apply.
            return None;
        }

        // Build the set of base content (including extended import lines)
        // for addition detection.
        let base_content: HashSet<String> = base_units
            .iter()
            .map(|u| u.content.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        // Extended import lines should also be excluded from "additions".
        let mut extended_lines: HashSet<String> = HashSet::new();
        for (_, ext) in &left_import_extensions {
            extended_lines.insert(ext.clone());
        }
        for (_, ext) in &right_import_extensions {
            extended_lines.insert(ext.clone());
        }

        // Extract additions from each side (content not in base and not
        // an import extension of a base line).
        let left_additions: Vec<&str> = left_content
            .iter()
            .filter(|c| !base_content.contains(c.as_str()) && !extended_lines.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();
        let right_additions: Vec<&str> = right_content
            .iter()
            .filter(|c| !base_content.contains(c.as_str()) && !extended_lines.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();

        // At least one side must have additions or import extensions for this to be useful.
        if left_additions.is_empty()
            && right_additions.is_empty()
            && left_import_extensions.is_empty()
            && right_import_extensions.is_empty()
        {
            return None;
        }

        // Build the set of base indices that have import extensions.
        let left_ext_map: std::collections::HashMap<usize, &str> = left_import_extensions
            .iter()
            .map(|(idx, content)| (*idx, content.as_str()))
            .collect();
        let right_ext_map: std::collections::HashMap<usize, &str> = right_import_extensions
            .iter()
            .map(|(idx, content)| (*idx, content.as_str()))
            .collect();

        // Compose: base (with merged imports) + left additions + right additions.
        let mut merged_parts: Vec<String> = Vec::new();
        for (idx, unit) in base_units.iter().enumerate() {
            let has_left_ext = left_ext_map.contains_key(&idx);
            let has_right_ext = right_ext_map.contains_key(&idx);

            if has_left_ext || has_right_ext {
                // This base import was extended — merge all names.
                let left_ver = left_ext_map
                    .get(&idx)
                    .copied()
                    .unwrap_or(unit.content.trim());
                let right_ver = right_ext_map
                    .get(&idx)
                    .copied()
                    .unwrap_or(unit.content.trim());
                merged_parts.push(merge_import_line(unit.content.trim(), left_ver, right_ver));
            } else {
                merged_parts.push(unit.content.clone());
            }
        }

        let mut seen_additions: HashSet<String> = HashSet::new();
        for addition in &left_additions {
            if seen_additions.insert(addition.to_string()) {
                merged_parts.push(addition.to_string());
            }
        }
        for addition in &right_additions {
            if seen_additions.insert(addition.to_string()) {
                merged_parts.push(addition.to_string());
            }
        }

        let merged_content = merged_parts.join("\n");

        let import_ext_count = left_import_extensions.len() + right_import_extensions.len();
        let explanation = if import_ext_count > 0 {
            format!(
                "Both sides preserved base content (including {} import extension(s)); composed {} left additions + {} right additions",
                import_ext_count,
                left_additions.len(),
                right_additions.len(),
            )
        } else {
            format!(
                "Both sides preserved base content; composed {} left additions + {} right additions",
                left_additions.len(),
                right_additions.len(),
            )
        };

        Some(ResolutionProposal {
            pattern_name: self.name().into(),
            confidence: 0.85,
            merged_content,
            explanation,
            warnings: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::types::*;

    fn unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Unknown, None, (0, 1), content.into())
    }

    fn import_unit(content: &str) -> StructuralUnit {
        StructuralUnit::new(UnitKind::Import, None, (0, 1), content.into())
    }

    #[test]
    fn test_additive_composition_both_add() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("line1"), unit("line2")],
                left_units: vec![unit("line1"), unit("line2"), unit("left_new")],
                right_units: vec![unit("line1"), unit("line2"), unit("right_new")],
                base_span: (0, 2),
                left_span: (0, 3),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        assert!(proposal.merged_content.contains("line1"));
        assert!(proposal.merged_content.contains("line2"));
        assert!(proposal.merged_content.contains("left_new"));
        assert!(proposal.merged_content.contains("right_new"));
        assert!((proposal.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_returns_none_when_base_missing_from_side() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("line1"), unit("line2")],
                left_units: vec![unit("line1"), unit("replacement")],
                right_units: vec![unit("line1"), unit("line2"), unit("right_new")],
                base_span: (0, 2),
                left_span: (0, 2),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        // Left side dropped "line2" — not additive.
        assert!(pattern.resolve(&conflict).is_none());
    }

    #[test]
    fn test_deduplicates_shared_additions() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![unit("base")],
                left_units: vec![unit("base"), unit("shared_new")],
                right_units: vec![unit("base"), unit("shared_new")],
                base_span: (0, 1),
                left_span: (0, 2),
                right_span: (0, 2),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Unknown],
                right_unit_kinds: vec![UnitKind::Unknown],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        let count = proposal.merged_content.matches("shared_new").count();
        assert_eq!(count, 1, "should deduplicate: {}", proposal.merged_content);
    }

    #[test]
    fn test_empty_base_does_not_match() {
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![],
                left_units: vec![unit("left")],
                right_units: vec![unit("right")],
                base_span: (0, 0),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![],
                right_unit_kinds: vec![],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        assert!(!pattern.matches(&conflict));
    }

    // ── Import extension tests ─────────────────────────────────────────

    #[test]
    fn test_import_extension_recognized_as_additive() {
        // Base: `from flask import Flask, jsonify`
        // Left: `from flask import Flask, jsonify, request` (added request)
        // Right: `from flask import Flask, abort, jsonify` (added abort)
        // Both preserved all base names and added more.
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![import_unit("from flask import Flask, jsonify")],
                left_units: vec![import_unit("from flask import Flask, jsonify, request")],
                right_units: vec![import_unit("from flask import Flask, abort, jsonify")],
                base_span: (0, 1),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Import],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        // Should contain all names: Flask, abort, jsonify, request.
        assert!(
            proposal.merged_content.contains("Flask"),
            "Flask missing from: {}",
            proposal.merged_content
        );
        assert!(
            proposal.merged_content.contains("jsonify"),
            "jsonify missing from: {}",
            proposal.merged_content
        );
        assert!(
            proposal.merged_content.contains("request"),
            "request missing from: {}",
            proposal.merged_content
        );
        assert!(
            proposal.merged_content.contains("abort"),
            "abort missing from: {}",
            proposal.merged_content
        );
        // Should NOT duplicate any name.
        assert_eq!(
            proposal.merged_content.matches("Flask").count(),
            1,
            "Flask duplicated in: {}",
            proposal.merged_content
        );
    }

    #[test]
    fn test_import_extension_with_new_definitions() {
        // Base: `from flask import Flask, jsonify` + `app = Flask(__name__)`
        // Left extends import + adds a route
        // Right extends import differently + adds a different route
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![
                    import_unit("from flask import Flask, jsonify"),
                    unit("app = Flask(__name__)"),
                ],
                left_units: vec![
                    import_unit("from flask import Flask, jsonify, request"),
                    unit("app = Flask(__name__)"),
                    unit("@app.route('/auth')"),
                ],
                right_units: vec![
                    import_unit("from flask import Flask, abort, jsonify"),
                    unit("app = Flask(__name__)"),
                    unit("@app.route('/orders')"),
                ],
                base_span: (0, 2),
                left_span: (0, 3),
                right_span: (0, 3),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import, UnitKind::Statement, UnitKind::Definition],
                right_unit_kinds: vec![UnitKind::Import, UnitKind::Statement, UnitKind::Definition],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        let proposal = pattern.resolve(&conflict).unwrap();
        // Merged import should have all names.
        assert!(proposal.merged_content.contains("Flask"));
        assert!(proposal.merged_content.contains("abort"));
        assert!(proposal.merged_content.contains("jsonify"));
        assert!(proposal.merged_content.contains("request"));
        // Base statement preserved.
        assert!(proposal.merged_content.contains("app = Flask(__name__)"));
        // Both additions present.
        assert!(proposal.merged_content.contains("/auth"));
        assert!(proposal.merged_content.contains("/orders"));
    }

    #[test]
    fn test_import_removal_still_fails() {
        // If a side removes a name from the base import, it's NOT additive.
        let pattern = AdditiveComposition;
        let conflict = ClassifiedConflict {
            region: StructuralConflictRegion {
                base_units: vec![import_unit("from flask import Flask, jsonify")],
                left_units: vec![import_unit("from flask import Flask")], // removed jsonify!
                right_units: vec![import_unit("from flask import Flask, jsonify, request")],
                base_span: (0, 1),
                left_span: (0, 1),
                right_span: (0, 1),
            },
            conflict_type: ConflictType::BothModified,
            requires_review: false,
            structural_info: StructuralInfo {
                left_unit_kinds: vec![UnitKind::Import],
                right_unit_kinds: vec![UnitKind::Import],
                has_name_overlap: false,
                scope: ConflictScope::Mixed,
            },
        };

        // Left side removed jsonify — that's a destructive change, not additive.
        assert!(pattern.resolve(&conflict).is_none());
    }

    // ── Helper function tests ──────────────────────────────────────────

    #[test]
    fn test_extract_import_names_from_import() {
        let names = extract_import_names("from flask import Flask, jsonify, request");
        assert!(names.contains("Flask"));
        assert!(names.contains("jsonify"));
        assert!(names.contains("request"));
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn test_extract_import_names_with_alias() {
        let names = extract_import_names("from datetime import datetime as dt, timedelta");
        assert!(names.contains("datetime"));
        assert!(names.contains("timedelta"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn test_extract_import_module() {
        assert_eq!(
            extract_import_module("from flask import Flask"),
            Some("flask".into())
        );
        assert_eq!(extract_import_module("import os"), Some("os".into()));
        assert_eq!(
            extract_import_module("from flask_cors import CORS"),
            Some("flask_cors".into())
        );
    }

    #[test]
    fn test_import_is_preserved_extension() {
        assert!(import_is_preserved(
            "from flask import Flask, jsonify",
            "from flask import Flask, jsonify, request"
        ));
    }

    #[test]
    fn test_import_is_preserved_removal_fails() {
        assert!(!import_is_preserved(
            "from flask import Flask, jsonify",
            "from flask import Flask"
        ));
    }

    #[test]
    fn test_import_is_preserved_different_module_fails() {
        assert!(!import_is_preserved(
            "from flask import Flask",
            "from django import Flask"
        ));
    }

    #[test]
    fn test_merge_import_line() {
        let merged = merge_import_line(
            "from flask import Flask, jsonify",
            "from flask import Flask, jsonify, request",
            "from flask import Flask, abort, jsonify",
        );
        assert!(merged.contains("Flask"));
        assert!(merged.contains("abort"));
        assert!(merged.contains("jsonify"));
        assert!(merged.contains("request"));
        assert!(merged.starts_with("from flask import"));
    }
}
