//! Python language analyzer.
//!
//! Parses Python source code into structural units with awareness of:
//! - Import statements (`import X`, `from X import Y`)
//! - Top-level definitions (`class`, `def`)
//! - Decorators (`@decorator`)
//! - Comments and docstrings
//! - Whitespace/blank lines
//!
//! This analyzer powers the convergence engine's ability to reason about
//! Python code at the definition level rather than the line level —
//! preventing content loss when agents add non-overlapping classes or
//! functions to the same file.

use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// Python-specific code analyzer.
pub struct PythonAnalyzer;

impl LanguageAnalyzer for PythonAnalyzer {
    fn name(&self) -> &str {
        "python"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        parse_python_structure(source)
    }

    fn extract_imports(&self, source: &str) -> Vec<Import> {
        let units = self.parse_structure(source);
        units
            .into_iter()
            .filter(|u| u.kind == UnitKind::Import)
            .map(|u| {
                let (module, names) = parse_import_details(&u.content);
                Import {
                    module,
                    names,
                    raw: u.content,
                }
            })
            .collect()
    }

    fn extract_definitions(&self, source: &str) -> Vec<Definition> {
        let units = self.parse_structure(source);
        units
            .into_iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .map(|u| Definition {
                name: u.name.clone().unwrap_or_default(),
                def_kind: u
                    .metadata
                    .get("def_kind")
                    .cloned()
                    .unwrap_or_else(|| "unknown".into()),
                span: u.span,
                content: u.content,
            })
            .collect()
    }

    fn are_semantically_equivalent(&self, a: &str, b: &str) -> bool {
        // Normalize: strip trailing whitespace per line, collapse multiple
        // blank lines to one, strip leading/trailing blank lines.
        let normalize = |s: &str| -> String {
            let lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
            // Collapse runs of blank lines to a single blank line.
            let mut result: Vec<&str> = Vec::new();
            let mut prev_blank = false;
            for line in &lines {
                if line.is_empty() {
                    if !prev_blank {
                        result.push(line);
                    }
                    prev_blank = true;
                } else {
                    prev_blank = false;
                    result.push(line);
                }
            }
            // Trim leading/trailing blank lines.
            while result.first().map_or(false, |l| l.is_empty()) {
                result.remove(0);
            }
            while result.last().map_or(false, |l| l.is_empty()) {
                result.pop();
            }
            result.join("\n")
        };
        normalize(a) == normalize(b)
    }

    fn ordering_matters(&self, unit_kind: &UnitKind) -> bool {
        match unit_kind {
            // Python import ordering is a style preference (PEP 8), not semantic.
            UnitKind::Import => false,
            // Comments and whitespace order doesn't affect semantics.
            UnitKind::Comment | UnitKind::Whitespace => false,
            // Top-level definitions in Python are order-independent (mostly).
            // Forward references work in Python, and class/function definitions
            // at module level can be in any order.
            UnitKind::Definition => false,
            // Statements ARE order-dependent.
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Structural parsing
// ---------------------------------------------------------------------------

/// Parse Python source into structural units.
///
/// This is a heuristic parser — it doesn't build a full AST, but it
/// correctly identifies top-level constructs by indentation level.
fn parse_python_structure(source: &str) -> Vec<StructuralUnit> {
    let lines: Vec<&str> = source.lines().collect();
    let mut units: Vec<StructuralUnit> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Blank lines → Whitespace
        if trimmed.is_empty() {
            let start = i;
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }
            let content: String = lines[start..i].join("\n");
            units.push(StructuralUnit::new(
                UnitKind::Whitespace,
                None,
                (start, i),
                content,
            ));
            continue;
        }

        // Comments (lines starting with #, not inside a string)
        if trimmed.starts_with('#') {
            let start = i;
            while i < lines.len() && lines[i].trim().starts_with('#') {
                i += 1;
            }
            let content: String = lines[start..i].join("\n");
            units.push(StructuralUnit::new(
                UnitKind::Comment,
                None,
                (start, i),
                content,
            ));
            continue;
        }

        // Import lines (at any indentation, but typically top-level)
        if is_python_import(trimmed) {
            let start = i;
            let content = line.to_string();
            let module = extract_import_module(trimmed);
            i += 1;
            units.push(StructuralUnit::new(
                UnitKind::Import,
                Some(module),
                (start, i),
                content,
            ));
            continue;
        }

        // Decorators + class/def definitions
        if trimmed.starts_with('@') || is_top_level_def(line) {
            let start = i;

            // Collect decorators.
            let mut decorators: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('@') {
                decorators.push(lines[i].trim().to_string());
                i += 1;
            }

            // Now we should be at a class/def line (if decorators were present).
            if i < lines.len() && is_top_level_def(lines[i]) {
                let def_line = lines[i];
                let (def_kind, name) = parse_def_header(def_line.trim());
                i += 1;

                // Collect the body: all following lines with indentation > 0
                // (or blank lines within the body).
                let body_end = find_block_end(&lines, i);
                i = body_end;

                let content: String = lines[start..i].join("\n");
                let mut unit =
                    StructuralUnit::new(UnitKind::Definition, Some(name), (start, i), content);
                unit.metadata.insert("def_kind".into(), def_kind);
                if !decorators.is_empty() {
                    unit.metadata
                        .insert("decorators".into(), decorators.join(", "));
                }
                units.push(unit);
            } else {
                // Decorators without a following def/class — treat as statements.
                let content: String = lines[start..i].join("\n");
                units.push(StructuralUnit::new(
                    UnitKind::Statement,
                    None,
                    (start, i),
                    content,
                ));
            }
            continue;
        }

        // Everything else → Statement (executable code at top level)
        let start = i;
        i += 1;
        units.push(StructuralUnit::new(
            UnitKind::Statement,
            None,
            (start, i),
            line.to_string(),
        ));
    }

    units
}

/// Check if a trimmed line is a Python import statement.
fn is_python_import(trimmed: &str) -> bool {
    trimmed.starts_with("import ") || (trimmed.starts_with("from ") && trimmed.contains(" import "))
}

/// Check if a line is a top-level class or def definition.
///
/// Top-level means the line starts at column 0 (no leading whitespace).
fn is_top_level_def(line: &str) -> bool {
    let trimmed = line.trim();
    (trimmed.starts_with("def ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("async def "))
        && !line.starts_with(' ')
        && !line.starts_with('\t')
}

/// Parse a def/class header to extract the kind and name.
fn parse_def_header(trimmed: &str) -> (String, String) {
    if trimmed.starts_with("class ") {
        let rest = &trimmed[6..];
        let name = rest
            .split(|c: char| c == '(' || c == ':' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_string();
        ("class".into(), name)
    } else if trimmed.starts_with("async def ") {
        let rest = &trimmed[10..];
        let name = rest
            .split(|c: char| c == '(' || c == ':' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_string();
        ("async_function".into(), name)
    } else if trimmed.starts_with("def ") {
        let rest = &trimmed[4..];
        let name = rest
            .split(|c: char| c == '(' || c == ':' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_string();
        ("function".into(), name)
    } else {
        ("unknown".into(), String::new())
    }
}

/// Find the end of an indented block (the first non-empty line at indent 0).
fn find_block_end(lines: &[&str], start: usize) -> usize {
    let mut i = start;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            // Blank lines inside a block are fine — keep going.
            i += 1;
            continue;
        }
        // If the line has no leading whitespace, we've left the block.
        if !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        i += 1;
    }
    // Trim trailing blank lines from the block (they belong to the
    // gap between definitions, not to this definition).
    while i > start && i > 0 && lines[i - 1].trim().is_empty() {
        i -= 1;
    }
    i
}

/// Extract the module name from a Python import line.
fn extract_import_module(trimmed: &str) -> String {
    if trimmed.starts_with("from ") {
        let rest = &trimmed[5..];
        rest.split_whitespace().next().unwrap_or("").to_string()
    } else if trimmed.starts_with("import ") {
        let rest = &trimmed[7..];
        // Handle `import X as Y` and `import X, Y`.
        rest.split(|c: char| c == ',' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    }
}

/// Parse import details (module and names) from an import line.
fn parse_import_details(line: &str) -> (String, Vec<String>) {
    let trimmed = line.trim();
    if trimmed.starts_with("from ") {
        let rest = &trimmed[5..];
        if let Some(import_idx) = rest.find(" import ") {
            let module = rest[..import_idx].trim().to_string();
            let names_part = rest[import_idx + 8..].trim();
            if names_part == "*" {
                return (module, vec!["*".into()]);
            }
            let names: Vec<String> = names_part
                .trim_start_matches('(')
                .trim_end_matches(')')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return (module, names);
        }
    } else if trimmed.starts_with("import ") {
        let module = trimmed[7..].trim().to_string();
        return (module, vec![]);
    }
    (String::new(), vec![])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert_eq!(PythonAnalyzer.name(), "python");
    }

    #[test]
    fn test_parse_simple_script() {
        let source = "import os\nfrom sys import argv\n\ndef main():\n    print('hello')\n\nclass User:\n    name: str\n";
        let analyzer = PythonAnalyzer;
        let units = analyzer.parse_structure(source);

        let kinds: Vec<&UnitKind> = units.iter().map(|u| &u.kind).collect();
        assert!(
            kinds.contains(&&UnitKind::Import),
            "should have imports: {kinds:?}"
        );
        assert!(
            kinds.contains(&&UnitKind::Definition),
            "should have definitions: {kinds:?}"
        );

        // Check that definitions are named.
        let defs: Vec<&StructuralUnit> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .collect();
        let def_names: Vec<&str> = defs.iter().filter_map(|u| u.name.as_deref()).collect();
        assert!(
            def_names.contains(&"main"),
            "should find main: {def_names:?}"
        );
        assert!(
            def_names.contains(&"User"),
            "should find User: {def_names:?}"
        );
    }

    #[test]
    fn test_parse_imports() {
        let source = "import os\nfrom flask import Flask, jsonify\nfrom . import utils\n";
        let analyzer = PythonAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].module, "os");
        assert_eq!(imports[1].module, "flask");
        assert_eq!(imports[1].names, vec!["Flask", "jsonify"]);
        assert_eq!(imports[2].module, ".");
    }

    #[test]
    fn test_parse_class_with_decorator() {
        let source = "@dataclass\nclass User:\n    name: str\n    email: str\n";
        let analyzer = PythonAnalyzer;
        let units = analyzer.parse_structure(source);
        let defs: Vec<&StructuralUnit> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.as_deref(), Some("User"));
        assert_eq!(
            defs[0].metadata.get("def_kind").map(|s| s.as_str()),
            Some("class")
        );
        assert!(defs[0].metadata.contains_key("decorators"));
        assert!(defs[0].content.contains("@dataclass"));
    }

    #[test]
    fn test_parse_async_def() {
        let source = "async def fetch_data():\n    return await get()\n";
        let analyzer = PythonAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "fetch_data");
        assert_eq!(defs[0].def_kind, "async_function");
    }

    #[test]
    fn test_parse_covers_all_lines() {
        let source = "import os\n\ndef main():\n    pass\n";
        let analyzer = PythonAnalyzer;
        let units = analyzer.parse_structure(source);
        // Every line should be accounted for — no gaps.
        let mut covered = vec![false; source.lines().count()];
        for unit in &units {
            for line_idx in unit.span.0..unit.span.1 {
                covered[line_idx] = true;
            }
        }
        assert!(
            covered.iter().all(|&c| c),
            "all lines should be covered: {covered:?}"
        );
    }

    #[test]
    fn test_extract_definitions_non_overlapping() {
        // This is the TR19 scenario: two agents add different classes.
        let source = "class User:\n    name: str\n\nclass Product:\n    title: str\n";
        let analyzer = PythonAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "User");
        assert_eq!(defs[1].name, "Product");
        // Spans should not overlap.
        assert!(
            defs[0].span.1 <= defs[1].span.0,
            "User span {:?} should end before Product span {:?}",
            defs[0].span,
            defs[1].span
        );
    }

    #[test]
    fn test_semantic_equivalence_whitespace_differences() {
        let analyzer = PythonAnalyzer;
        let a = "def foo():\n    pass\n\n\n";
        let b = "def foo():\n    pass\n";
        assert!(analyzer.are_semantically_equivalent(a, b));
    }

    #[test]
    fn test_semantic_equivalence_real_difference() {
        let analyzer = PythonAnalyzer;
        let a = "def foo():\n    return 1\n";
        let b = "def foo():\n    return 2\n";
        assert!(!analyzer.are_semantically_equivalent(a, b));
    }

    #[test]
    fn test_ordering_matters_for_python() {
        let analyzer = PythonAnalyzer;
        // Imports and definitions can be reordered in Python.
        assert!(!analyzer.ordering_matters(&UnitKind::Import));
        assert!(!analyzer.ordering_matters(&UnitKind::Definition));
        // Statements must stay in order.
        assert!(analyzer.ordering_matters(&UnitKind::Statement));
    }

    #[test]
    fn test_multiple_decorators() {
        let source = "@app.route('/api')\n@login_required\ndef handler():\n    return 'ok'\n";
        let analyzer = PythonAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "handler");
        assert!(defs[0].content.contains("@app.route"));
        assert!(defs[0].content.contains("@login_required"));
    }

    #[test]
    fn test_comments_parsed_correctly() {
        let source = "# This is a comment\n# Another comment\nimport os\n";
        let analyzer = PythonAnalyzer;
        let units = analyzer.parse_structure(source);
        assert_eq!(units[0].kind, UnitKind::Comment);
        assert_eq!(units[0].span, (0, 2)); // Both comment lines grouped.
        assert_eq!(units[1].kind, UnitKind::Import);
    }

    #[test]
    fn test_star_import() {
        let source = "from typing import *\n";
        let analyzer = PythonAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].module, "typing");
        assert_eq!(imports[0].names, vec!["*"]);
    }

    #[test]
    fn test_empty_source() {
        let analyzer = PythonAnalyzer;
        let units = analyzer.parse_structure("");
        // `"".lines()` yields nothing in Rust — empty source produces no units.
        assert!(units.is_empty());
    }

    #[test]
    fn test_class_with_methods() {
        let source = "class User:\n    def __init__(self):\n        self.name = ''\n\n    def greet(self):\n        return f'Hi {self.name}'\n";
        let analyzer = PythonAnalyzer;
        let defs = analyzer.extract_definitions(source);
        // The whole class (including methods) is one Definition unit.
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "User");
        assert!(defs[0].content.contains("__init__"));
        assert!(defs[0].content.contains("greet"));
    }
}
