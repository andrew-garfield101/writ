//! Go language analyzer.
//!
//! Parses Go source code into structural units with awareness of:
//! - Import statements (`import "fmt"`, `import ( ... )` blocks)
//! - Top-level definitions (`func`, `type X struct`, `type X interface`,
//!   `var`, `const`, `const ( ... )` blocks)
//! - Comments (`//`, `/* */`)
//! - Whitespace/blank lines

use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// Go-specific code analyzer.
pub struct GoAnalyzer;

impl LanguageAnalyzer for GoAnalyzer {
    fn name(&self) -> &str {
        "go"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        parse_go_structure(source)
    }

    fn extract_imports(&self, source: &str) -> Vec<Import> {
        let units = self.parse_structure(source);
        units
            .into_iter()
            .filter(|u| u.kind == UnitKind::Import)
            .map(|u| {
                let (module, names) = parse_go_import_details(&u.content);
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
        let normalize = |s: &str| -> String {
            let mut result: Vec<String> = Vec::new();
            for line in s.lines() {
                let trimmed = line.trim_end();
                // Skip blank lines entirely — in Go, blank lines in import
                // groups and between definitions are stylistic, not semantic.
                if trimmed.is_empty() {
                    continue;
                }
                result.push(trimmed.to_string());
            }
            result.join("\n")
        };
        normalize(a) == normalize(b)
    }

    fn ordering_matters(&self, unit_kind: &UnitKind) -> bool {
        match unit_kind {
            UnitKind::Import => false,
            UnitKind::Comment | UnitKind::Whitespace => false,
            // Go top-level definitions are order-independent.
            UnitKind::Definition => false,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Structural parsing
// ---------------------------------------------------------------------------

/// Parse Go source into structural units.
fn parse_go_structure(source: &str) -> Vec<StructuralUnit> {
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

        // Block comments: /* ... */
        if trimmed.starts_with("/*") {
            let start = i;
            while i < lines.len() {
                if lines[i].contains("*/") {
                    i += 1;
                    break;
                }
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

        // Line comments: //
        if trimmed.starts_with("//") {
            let start = i;
            while i < lines.len() && lines[i].trim().starts_with("//") {
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

        // Package declaration → Statement
        if trimmed.starts_with("package ") {
            let start = i;
            i += 1;
            units.push(StructuralUnit::new(
                UnitKind::Statement,
                None,
                (start, i),
                line.to_string(),
            ));
            continue;
        }

        // Import: `import "fmt"` or `import ( ... )`
        if trimmed.starts_with("import ") || trimmed == "import(" || trimmed == "import (" {
            let start = i;
            if trimmed.contains('(') {
                // Grouped import block.
                let end = find_paren_end(&lines, i);
                i = end;
            } else {
                // Single-line import.
                i += 1;
            }
            let content: String = lines[start..i].join("\n");
            let module = extract_go_module(lines[start].trim());
            let mut unit = StructuralUnit::new(
                UnitKind::Import,
                Some(module.clone()),
                (start, i),
                content.clone(),
            );
            unit.metadata.insert("import_lang".into(), "go".into());
            let (parsed_module, names) = parse_go_import_details(&content);
            unit.metadata.insert(
                "import_module".into(),
                if parsed_module.is_empty() {
                    module
                } else {
                    parsed_module
                },
            );
            if !names.is_empty() {
                let mut sorted = names;
                sorted.sort();
                unit.metadata
                    .insert("import_names".into(), sorted.join(", "));
            }
            units.push(unit);
            continue;
        }

        // Definitions
        if is_go_def_start(trimmed) {
            let start = i;
            let (def_kind, name) = parse_go_def_header(trimmed);
            let end = if trimmed.contains('{') || trimmed.contains('(') {
                find_go_block_end(&lines, i)
            } else {
                i + 1
            };
            i = end;
            let content: String = lines[start..i].join("\n");
            let mut unit =
                StructuralUnit::new(UnitKind::Definition, Some(name), (start, i), content);
            unit.metadata.insert("def_kind".into(), def_kind);
            units.push(unit);
            continue;
        }

        // Everything else → Statement
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

/// Check if a trimmed line starts a Go definition.
fn is_go_def_start(trimmed: &str) -> bool {
    trimmed.starts_with("func ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("var ")
        || trimmed.starts_with("const ")
        || trimmed == "var("
        || trimmed == "var ("
        || trimmed == "const("
        || trimmed == "const ("
}

/// Parse a Go definition header.
fn parse_go_def_header(trimmed: &str) -> (String, String) {
    if trimmed.starts_with("func ") {
        let rest = &trimmed[5..];
        // Method receiver: `func (r *Receiver) Name(...)`
        let after_receiver = if rest.starts_with('(') {
            if let Some(close) = rest.find(") ") {
                &rest[close + 2..]
            } else {
                rest
            }
        } else {
            rest
        };
        let name = extract_go_ident(after_receiver);
        return ("function".into(), name);
    }
    if trimmed.starts_with("type ") {
        let rest = &trimmed[5..];
        let name = extract_go_ident(rest);
        // Determine if it's struct, interface, or type alias.
        let kind = if rest.contains("struct") {
            "struct"
        } else if rest.contains("interface") {
            "interface"
        } else {
            "type_alias"
        };
        return (kind.into(), name);
    }
    if trimmed.starts_with("var ") {
        let rest = &trimmed[4..];
        let name = extract_go_ident(rest);
        return ("variable".into(), name);
    }
    if trimmed.starts_with("const ") {
        let rest = &trimmed[6..];
        let name = extract_go_ident(rest);
        return ("const".into(), name);
    }
    // Grouped var/const blocks.
    if trimmed == "var(" || trimmed == "var (" {
        return ("variable".into(), "var_block".into());
    }
    if trimmed == "const(" || trimmed == "const (" {
        return ("const".into(), "const_block".into());
    }

    ("unknown".into(), String::new())
}

/// Extract Go identifier from the start of a string.
fn extract_go_ident(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Find the end of a parenthesized block `( ... )`.
fn find_paren_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start;
    while i < lines.len() {
        for c in lines[i].chars() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    i
}

/// Find the end of a Go block (brace-delimited or paren-delimited).
///
/// For function/struct/interface definitions, the block ends at the
/// closing `}`. Parens in function signatures are tracked but don't
/// terminate the block.
fn find_go_block_end(lines: &[&str], start: usize) -> usize {
    let first_line = lines[start].trim();

    // Paren-only blocks (const/var groups).
    let is_paren_block = first_line == "const ("
        || first_line == "const("
        || first_line == "var ("
        || first_line == "var(";

    if is_paren_block {
        return find_paren_end(lines, start);
    }

    // Brace-delimited blocks: track braces, ignore parens for termination.
    let mut brace_depth = 0i32;
    let mut found_brace = false;
    let mut i = start;

    while i < lines.len() {
        for c in lines[i].chars() {
            match c {
                '{' => {
                    brace_depth += 1;
                    found_brace = true;
                }
                '}' => {
                    brace_depth -= 1;
                    if found_brace && brace_depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    i
}

/// Extract module name from a Go import line.
fn extract_go_module(trimmed: &str) -> String {
    if trimmed.starts_with("import \"") || trimmed.starts_with("import `") {
        let rest = &trimmed[7..];
        return rest
            .trim_matches(|c: char| c == '"' || c == '`' || c == ' ')
            .to_string();
    }
    if trimmed.starts_with("import (") || trimmed == "import(" {
        return "(grouped)".into();
    }
    String::new()
}

/// Parse import details from Go import text.
fn parse_go_import_details(content: &str) -> (String, Vec<String>) {
    let trimmed = content.trim();

    // Single-line: import "fmt"
    if trimmed.starts_with("import \"") || trimmed.starts_with("import `") {
        let module = trimmed[7..]
            .trim_end()
            .trim_matches(|c: char| c == '"' || c == '`')
            .to_string();
        return (module, vec![]);
    }

    // Grouped import block: extract all quoted paths.
    if trimmed.starts_with("import") && trimmed.contains('(') {
        let mut names: Vec<String> = Vec::new();
        for line in content.lines() {
            let t = line.trim();
            // Extract quoted strings: "fmt", "os", "github.com/..."
            if let Some(start) = t.find('"') {
                if let Some(end) = t[start + 1..].find('"') {
                    let pkg = &t[start + 1..start + 1 + end];
                    if !pkg.is_empty() {
                        names.push(pkg.to_string());
                    }
                }
            }
        }
        let module = "(grouped)".to_string();
        return (module, names);
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
        assert_eq!(GoAnalyzer.name(), "go");
    }

    #[test]
    fn test_parse_structure_imports() {
        let source = "package main\n\nimport \"fmt\"\nimport \"os\"\n";
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 2, "should find 2 imports: {units:?}");
    }

    #[test]
    fn test_parse_structure_grouped_import() {
        let source = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n";
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 1, "grouped import is one unit: {units:?}");
        assert!(imports[0].content.contains("fmt"));
        assert!(imports[0].content.contains("os"));
    }

    #[test]
    fn test_parse_structure_definitions() {
        let source = "\
package main

func main() {
\tfmt.Println(\"hello\")
}

type User struct {
\tName  string
\tEmail string
}

type Handler interface {
\tHandle() error
}
";
        let analyzer = GoAnalyzer;
        let defs = analyzer.extract_definitions(source);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"main"), "should find main: {names:?}");
        assert!(names.contains(&"User"), "should find User: {names:?}");
        assert!(names.contains(&"Handler"), "should find Handler: {names:?}");
    }

    #[test]
    fn test_parse_structure_covers_all_lines() {
        let source = "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(\"hi\")\n}\n";
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure(source);
        let line_count = source.lines().count();
        let mut covered = vec![false; line_count];
        for unit in &units {
            for idx in unit.span.0..unit.span.1 {
                if idx < line_count {
                    covered[idx] = true;
                }
            }
        }
        assert!(
            covered.iter().all(|&c| c),
            "all lines should be covered: {covered:?}"
        );
    }

    #[test]
    fn test_extract_imports() {
        let source =
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\t\"github.com/pkg/errors\"\n)\n";
        let analyzer = GoAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 1); // One grouped import unit.
        assert!(imports[0].names.contains(&"fmt".to_string()));
        assert!(imports[0].names.contains(&"os".to_string()));
        assert!(imports[0]
            .names
            .contains(&"github.com/pkg/errors".to_string()));
    }

    #[test]
    fn test_extract_definitions() {
        let source = "\
func Add(a, b int) int {
\treturn a + b
}

var Version = \"1.0\"

const MaxRetries = 3
";
        let analyzer = GoAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 3, "should find 3 definitions: {defs:?}");
        assert_eq!(defs[0].name, "Add");
        assert_eq!(defs[0].def_kind, "function");
        assert_eq!(defs[1].name, "Version");
        assert_eq!(defs[1].def_kind, "variable");
        assert_eq!(defs[2].name, "MaxRetries");
        assert_eq!(defs[2].def_kind, "const");
    }

    #[test]
    fn test_semantic_equivalence() {
        let analyzer = GoAnalyzer;
        // Trailing whitespace.
        assert!(analyzer.are_semantically_equivalent("func foo() {}", "func foo() {}  "));
        // Blank line differences in import groups.
        assert!(analyzer.are_semantically_equivalent(
            "import (\n\t\"fmt\"\n\n\t\"os\"\n)",
            "import (\n\t\"fmt\"\n\t\"os\"\n)"
        ));
        // Real difference.
        assert!(!analyzer
            .are_semantically_equivalent("func foo() { return 1 }", "func foo() { return 2 }"));
    }

    #[test]
    fn test_multiline_definition_spans() {
        let source = "\
func ComplexFunction(
\ta int,
\tb int,
) int {
\tresult := a + b
\treturn result * 2
}
";
        let analyzer = GoAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "ComplexFunction");
        assert_eq!(defs[0].span.0, 0);
        assert_eq!(defs[0].span.1, source.lines().count());
    }

    #[test]
    fn test_method_receiver() {
        let source = "func (u *User) String() string {\n\treturn u.Name\n}\n";
        let analyzer = GoAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "String");
        assert_eq!(defs[0].def_kind, "function");
    }

    #[test]
    fn test_comments_parsed() {
        let source = "// Package main\n// provides the entry point.\npackage main\n";
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure(source);
        assert_eq!(units[0].kind, UnitKind::Comment);
        assert_eq!(units[0].span, (0, 2));
    }

    #[test]
    fn test_const_block() {
        let source = "const (\n\tA = 1\n\tB = 2\n\tC = 3\n)\n";
        let analyzer = GoAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1, "const block is one definition: {defs:?}");
        assert_eq!(defs[0].def_kind, "const");
    }

    #[test]
    fn test_empty_source() {
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure("");
        assert!(units.is_empty());
    }

    #[test]
    fn test_import_metadata_populated() {
        let source = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nimport \"net/http\"\n";
        let analyzer = GoAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);

        // Grouped import
        assert_eq!(imports[0].metadata.get("import_lang").unwrap(), "go");
        assert_eq!(
            imports[0].metadata.get("import_module").unwrap(),
            "(grouped)"
        );
        assert_eq!(imports[0].metadata.get("import_names").unwrap(), "fmt, os");

        // Single import
        assert_eq!(imports[1].metadata.get("import_lang").unwrap(), "go");
        assert_eq!(
            imports[1].metadata.get("import_module").unwrap(),
            "net/http"
        );
        assert!(imports[1].metadata.get("import_names").is_none());
    }
}
