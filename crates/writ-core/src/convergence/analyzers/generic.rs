//! Generic (line-based) language analyzer.
//!
//! The `GenericAnalyzer` treats every line as a `StructuralUnit` of kind
//! `Unknown`. This is the universal fallback — it preserves full diff3
//! functionality for any file type, just without structural awareness.

use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// Line-based analyzer that works for any file type.
///
/// Every non-empty line becomes its own `Unknown` structural unit.
/// Blank lines become `Whitespace` units. No semantic parsing is
/// performed.
pub struct GenericAnalyzer;

impl LanguageAnalyzer for GenericAnalyzer {
    fn name(&self) -> &str {
        "generic"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        source
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let kind = if line.trim().is_empty() {
                    UnitKind::Whitespace
                } else {
                    UnitKind::Unknown
                };
                StructuralUnit::new(kind, None, (i, i + 1), line.to_string())
            })
            .collect()
    }

    fn extract_imports(&self, _source: &str) -> Vec<Import> {
        // Generic analyzer can't identify imports.
        Vec::new()
    }

    fn extract_definitions(&self, _source: &str) -> Vec<Definition> {
        // Generic analyzer can't identify definitions.
        Vec::new()
    }

    fn are_semantically_equivalent(&self, a: &str, b: &str) -> bool {
        // For generic files, only exact match counts.
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_structure_simple() {
        let analyzer = GenericAnalyzer;
        let source = "line one\nline two\n\nline four\n";
        let units = analyzer.parse_structure(source);
        assert_eq!(units.len(), 4);
        assert_eq!(units[0].kind, UnitKind::Unknown);
        assert_eq!(units[0].content, "line one");
        assert_eq!(units[0].span, (0, 1));
        assert_eq!(units[2].kind, UnitKind::Whitespace);
    }

    #[test]
    fn test_parse_structure_empty_source() {
        let analyzer = GenericAnalyzer;
        let units = analyzer.parse_structure("");
        // `"".lines()` yields nothing in Rust — empty source produces no units.
        assert!(units.is_empty());
    }

    #[test]
    fn test_parse_structure_covers_all_lines() {
        let analyzer = GenericAnalyzer;
        let source = "a\nb\nc";
        let units = analyzer.parse_structure(source);
        assert_eq!(units.len(), 3);
        // Verify spans are contiguous.
        for (i, unit) in units.iter().enumerate() {
            assert_eq!(unit.span.0, i);
            assert_eq!(unit.span.1, i + 1);
        }
    }

    #[test]
    fn test_no_imports_or_definitions() {
        let analyzer = GenericAnalyzer;
        assert!(analyzer.extract_imports("import foo").is_empty());
        assert!(analyzer.extract_definitions("def foo(): pass").is_empty());
    }

    #[test]
    fn test_semantic_equivalence_exact_only() {
        let analyzer = GenericAnalyzer;
        assert!(analyzer.are_semantically_equivalent("hello", "hello"));
        assert!(!analyzer.are_semantically_equivalent("hello", "hello "));
        assert!(!analyzer.are_semantically_equivalent("hello", "Hello"));
    }

    #[test]
    fn test_name() {
        assert_eq!(GenericAnalyzer.name(), "generic");
    }
}
