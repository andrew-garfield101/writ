//! Rust language analyzer.
//!
//! Parses Rust source code into structural units with awareness of:
//! - Import statements (`use`, `mod`, `extern crate`)
//! - Top-level definitions (`fn`, `struct`, `enum`, `impl`, `trait`,
//!   `type`, `const`, `static`, `mod` with body)
//! - Attributes (`#[derive(...)]`, `#[cfg(...)]`, etc.)
//! - Comments (`//`, `///`, `//!`, `/* */`)
//! - Whitespace/blank lines

use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// Rust-specific code analyzer.
pub struct RustAnalyzer;

impl LanguageAnalyzer for RustAnalyzer {
    fn name(&self) -> &str {
        "rust"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        parse_rust_structure(source)
    }

    fn extract_imports(&self, source: &str) -> Vec<Import> {
        let units = self.parse_structure(source);
        units
            .into_iter()
            .filter(|u| u.kind == UnitKind::Import)
            .map(|u| {
                let (module, names) = parse_use_details(&u.content);
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
            let mut prev_blank = false;
            for line in s.lines() {
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    if !prev_blank {
                        result.push(String::new());
                    }
                    prev_blank = true;
                } else {
                    prev_blank = false;
                    // Normalize trailing commas in use groups.
                    let mut normalized = trimmed.trim_end_matches(',').to_string();
                    // Also handle trailing commas before closing braces:
                    // `{HashMap, HashSet,}` → `{HashMap, HashSet}`
                    normalized = normalized.replace(", }", " }").replace(",}", "}");
                    result.push(normalized);
                }
            }
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
            UnitKind::Import => false,
            UnitKind::Comment | UnitKind::Whitespace => false,
            // Rust top-level definitions are order-independent (no forward decl issues).
            UnitKind::Definition => false,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Structural parsing
// ---------------------------------------------------------------------------

/// Parse Rust source into structural units.
fn parse_rust_structure(source: &str) -> Vec<StructuralUnit> {
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
            // Scan for closing */
            let mut depth = 0i32;
            while i < lines.len() {
                let l = lines[i];
                for (j, _) in l.char_indices() {
                    if l[j..].starts_with("/*") {
                        depth += 1;
                    } else if l[j..].starts_with("*/") {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                i += 1;
                if depth == 0 {
                    break;
                }
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

        // Line comments: //, ///, //!
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

        // Attributes + definitions: #[...] followed by fn/struct/enum/etc.
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            let start = i;
            let mut attrs: Vec<String> = Vec::new();
            // Collect consecutive attribute lines.
            while i < lines.len() {
                let t = lines[i].trim();
                if t.starts_with("#[") || t.starts_with("#![") {
                    attrs.push(t.to_string());
                    i += 1;
                } else {
                    break;
                }
            }

            // Check if the next non-empty line is a definition.
            if i < lines.len() && is_rust_def_start(lines[i].trim()) {
                // Parse as definition with attributes.
                let (def_kind, name) = parse_rust_def_header(lines[i].trim());
                let def_end = find_brace_block_end(&lines, i);
                i = def_end;

                let content: String = lines[start..i].join("\n");
                let mut unit =
                    StructuralUnit::new(UnitKind::Definition, Some(name), (start, i), content);
                unit.metadata.insert("def_kind".into(), def_kind);
                if !attrs.is_empty() {
                    unit.metadata.insert("decorators".into(), attrs.join(", "));
                }
                units.push(unit);
            } else {
                // Standalone attributes (e.g., #![allow(...)]) → Statement.
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

        // Import: use, mod (without body), extern crate
        if is_rust_import(trimmed) {
            let start = i;
            // Multi-line use: use Foo::{...\n  ...\n};
            let end = find_semicolon_end(&lines, i);
            i = end;
            let content: String = lines[start..i].join("\n");
            let module = extract_use_module(lines[start].trim());
            units.push(StructuralUnit::new(
                UnitKind::Import,
                Some(module),
                (start, i),
                content,
            ));
            continue;
        }

        // Top-level definitions without attributes
        if is_rust_def_start(trimmed) {
            let start = i;
            let (def_kind, name) = parse_rust_def_header(trimmed);
            let def_end = find_brace_block_end(&lines, i);
            i = def_end;
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

/// Check if a trimmed line starts a Rust import.
fn is_rust_import(trimmed: &str) -> bool {
    trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("pub(crate) use ")
        || trimmed.starts_with("extern crate ")
        || (trimmed.starts_with("mod ") && !trimmed.contains('{'))
        || (trimmed.starts_with("pub mod ") && !trimmed.contains('{'))
        || (trimmed.starts_with("pub(crate) mod ") && !trimmed.contains('{'))
}

/// Check if a trimmed line starts a Rust top-level definition.
fn is_rust_def_start(trimmed: &str) -> bool {
    // Strip visibility prefix first.
    let after_vis = strip_visibility(trimmed);

    after_vis.starts_with("fn ")
        || after_vis.starts_with("async fn ")
        || after_vis.starts_with("const fn ")
        || after_vis.starts_with("unsafe fn ")
        || after_vis.starts_with("extern ")
        || after_vis.starts_with("struct ")
        || after_vis.starts_with("enum ")
        || after_vis.starts_with("impl ")
        || after_vis.starts_with("impl<")
        || after_vis.starts_with("trait ")
        || after_vis.starts_with("type ")
        || after_vis.starts_with("const ")
        || after_vis.starts_with("static ")
        || (after_vis.starts_with("mod ") && after_vis.contains('{'))
        || after_vis.starts_with("macro_rules!")
}

/// Strip `pub`, `pub(crate)`, `pub(super)` prefix from a trimmed line.
fn strip_visibility(trimmed: &str) -> &str {
    if trimmed.starts_with("pub(") {
        // pub(crate) or pub(super) etc.
        if let Some(close) = trimmed.find(") ") {
            return trimmed[close + 2..].trim_start();
        }
    }
    if trimmed.starts_with("pub ") {
        return &trimmed[4..];
    }
    trimmed
}

/// Parse a Rust definition header to extract kind and name.
fn parse_rust_def_header(trimmed: &str) -> (String, String) {
    let after_vis = strip_visibility(trimmed);

    if after_vis.starts_with("async fn ") {
        let name = extract_ident(&after_vis[9..]);
        return ("function".into(), name);
    }
    if after_vis.starts_with("const fn ") {
        let name = extract_ident(&after_vis[9..]);
        return ("function".into(), name);
    }
    if after_vis.starts_with("unsafe fn ") {
        let name = extract_ident(&after_vis[10..]);
        return ("function".into(), name);
    }
    if after_vis.starts_with("fn ") {
        let name = extract_ident(&after_vis[3..]);
        return ("function".into(), name);
    }
    if after_vis.starts_with("struct ") {
        let name = extract_ident(&after_vis[7..]);
        return ("struct".into(), name);
    }
    if after_vis.starts_with("enum ") {
        let name = extract_ident(&after_vis[5..]);
        return ("enum".into(), name);
    }
    if after_vis.starts_with("trait ") {
        let name = extract_ident(&after_vis[6..]);
        return ("trait".into(), name);
    }
    if after_vis.starts_with("type ") {
        let name = extract_ident(&after_vis[5..]);
        return ("type_alias".into(), name);
    }
    if after_vis.starts_with("const ") {
        let name = extract_ident(&after_vis[6..]);
        return ("const".into(), name);
    }
    if after_vis.starts_with("static ") {
        let name = extract_ident(&after_vis[7..]);
        return ("const".into(), name);
    }
    if after_vis.starts_with("impl<") || after_vis.starts_with("impl ") {
        // `impl Foo` or `impl<T> Foo for Bar`
        let rest = if after_vis.starts_with("impl<") {
            // Skip generics: find closing >
            let after_generics = skip_generics(&after_vis[4..]);
            after_generics.trim_start()
        } else {
            &after_vis[5..]
        };
        let name = extract_ident(rest);
        return ("impl".into(), name);
    }
    if after_vis.starts_with("mod ") {
        let name = extract_ident(&after_vis[4..]);
        return ("module".into(), name);
    }
    if after_vis.starts_with("macro_rules! ") {
        let name = extract_ident(&after_vis[13..]);
        return ("macro".into(), name);
    }
    if after_vis.starts_with("extern ") {
        return ("extern".into(), "extern".into());
    }

    ("unknown".into(), String::new())
}

/// Extract the first identifier from a string.
fn extract_ident(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Skip generic parameters (balanced <>).
fn skip_generics(s: &str) -> &str {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &s[i + 1..];
                }
            }
            _ => {}
        }
    }
    s
}

/// Find the end of a brace-delimited block starting at line `start`.
/// Handles definitions that end with `;` (no body) or `{ ... }`.
fn find_brace_block_end(lines: &[&str], start: usize) -> usize {
    let first_line = lines[start].trim();

    // No-body items: `type Foo = Bar;`, `const X: i32 = 5;`
    if first_line.ends_with(';') && !first_line.contains('{') {
        return start + 1;
    }

    let mut depth = 0i32;
    let mut found_open = false;
    let mut i = start;

    while i < lines.len() {
        for c in lines[i].chars() {
            match c {
                '{' => {
                    depth += 1;
                    found_open = true;
                }
                '}' => {
                    depth -= 1;
                    if found_open && depth == 0 {
                        return i + 1;
                    }
                }
                _ => {}
            }
        }
        i += 1;

        // If first line ends with `;` and we haven't found braces, single-line item.
        if !found_open && i > start + 1 && lines[i - 1].trim().ends_with(';') {
            return i;
        }
    }

    // If we never found a closing brace, consume to end.
    i
}

/// Find the end of a semicolon-terminated statement (possibly multi-line).
fn find_semicolon_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start;
    while i < lines.len() {
        for c in lines[i].chars() {
            match c {
                '{' | '(' => depth += 1,
                '}' | ')' => depth -= 1,
                ';' if depth == 0 => return i + 1,
                _ => {}
            }
        }
        i += 1;
    }
    i
}

/// Extract the module path from a `use` statement.
fn extract_use_module(trimmed: &str) -> String {
    let after_vis = strip_visibility(trimmed);
    let rest = if after_vis.starts_with("use ") {
        &after_vis[4..]
    } else if after_vis.starts_with("extern crate ") {
        &after_vis[13..]
    } else if after_vis.starts_with("mod ") {
        &after_vis[4..]
    } else {
        return String::new();
    };
    // Take until `;`, `{`, or `as`.
    rest.split(|c: char| c == ';' || c == '{' || c == ' ')
        .next()
        .unwrap_or("")
        .trim_end_matches(';')
        .to_string()
}

/// Parse details from a `use` statement.
fn parse_use_details(line: &str) -> (String, Vec<String>) {
    let trimmed = line.trim();
    let after_vis = strip_visibility(trimmed);

    if after_vis.starts_with("use ") {
        let rest = after_vis[4..].trim_end_matches(';').trim();
        // Check for grouped imports: `use crate::{A, B};`
        if let Some(brace_start) = rest.find('{') {
            let module = rest[..brace_start].trim_end_matches("::").to_string();
            let inner = rest[brace_start + 1..]
                .trim_end_matches('}')
                .trim_end_matches(';');
            let names: Vec<String> = inner
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return (module, names);
        }
        // Simple: `use std::io;` or `use std::io as io2;`
        let module = rest
            .split(|c: char| c == ' ' || c == ';')
            .next()
            .unwrap_or("")
            .to_string();
        return (module, vec![]);
    }
    if after_vis.starts_with("extern crate ") {
        let module = after_vis[13..]
            .trim_end_matches(';')
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
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
        assert_eq!(RustAnalyzer.name(), "rust");
    }

    #[test]
    fn test_parse_structure_imports() {
        let source = "use std::io;\nuse crate::foo::Bar;\nuse super::*;\n";
        let analyzer = RustAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 3, "should find 3 imports: {units:?}");
        assert_eq!(imports[0].name.as_deref(), Some("std::io"));
        assert_eq!(imports[1].name.as_deref(), Some("crate::foo::Bar"));
    }

    #[test]
    fn test_parse_structure_definitions() {
        let source = "\
fn main() {
    println!(\"hello\");
}

pub struct User {
    name: String,
    email: String,
}

impl User {
    fn new() -> Self {
        User { name: String::new(), email: String::new() }
    }
}
";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"main"), "should find main: {names:?}");
        assert!(names.contains(&"User"), "should find User: {names:?}");
        // impl block
        assert!(
            defs.iter().any(|d| d.def_kind == "impl"),
            "should find impl block: {defs:?}"
        );
    }

    #[test]
    fn test_parse_structure_covers_all_lines() {
        let source = "use std::io;\n\nfn main() {\n    println!(\"hello\");\n}\n";
        let analyzer = RustAnalyzer;
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
            "use std::collections::{HashMap, HashSet};\nuse crate::foo;\nextern crate serde;\n";
        let analyzer = RustAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 3);
        assert_eq!(imports[0].module, "std::collections");
        assert!(imports[0].names.contains(&"HashMap".to_string()));
        assert!(imports[0].names.contains(&"HashSet".to_string()));
        assert_eq!(imports[1].module, "crate::foo");
        assert_eq!(imports[2].module, "serde");
    }

    #[test]
    fn test_extract_definitions() {
        let source = "\
pub fn process(input: &str) -> String {
    input.to_uppercase()
}

enum Color {
    Red,
    Green,
    Blue,
}

trait Drawable {
    fn draw(&self);
}
";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 3, "should find 3 definitions: {defs:?}");
        assert_eq!(defs[0].name, "process");
        assert_eq!(defs[0].def_kind, "function");
        assert_eq!(defs[1].name, "Color");
        assert_eq!(defs[1].def_kind, "enum");
        assert_eq!(defs[2].name, "Drawable");
        assert_eq!(defs[2].def_kind, "trait");
    }

    #[test]
    fn test_semantic_equivalence() {
        let analyzer = RustAnalyzer;
        // Trailing whitespace difference.
        assert!(analyzer.are_semantically_equivalent("fn foo() {}", "fn foo() {}  "));
        // Trailing comma difference.
        assert!(analyzer.are_semantically_equivalent(
            "use std::collections::{HashMap, HashSet}",
            "use std::collections::{HashMap, HashSet,}"
        ));
        // Real difference.
        assert!(!analyzer.are_semantically_equivalent("fn foo() { 1 }", "fn foo() { 2 }"));
    }

    #[test]
    fn test_multiline_definition_spans() {
        let source = "\
fn complex_function(
    a: i32,
    b: i32,
) -> i32 {
    let result = a + b;
    result * 2
}
";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "complex_function");
        // Should span the entire function.
        assert_eq!(defs[0].span.0, 0, "should start at line 0");
        assert_eq!(
            defs[0].span.1,
            source.lines().count(),
            "should end at last line"
        );
    }

    #[test]
    fn test_attributes_attached_to_definition() {
        let source = "#[derive(Debug, Clone)]\n#[serde(rename_all = \"camelCase\")]\npub struct Config {\n    name: String,\n}\n";
        let analyzer = RustAnalyzer;
        let units = analyzer.parse_structure(source);
        let defs: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.as_deref(), Some("Config"));
        assert!(defs[0].metadata.contains_key("decorators"));
        let decorators = defs[0].metadata.get("decorators").unwrap();
        assert!(decorators.contains("#[derive(Debug, Clone)]"));
        assert!(
            decorators.contains("#[serde(rename_all = \"camelCase\")]"),
            "decorators: {decorators}"
        );
    }

    #[test]
    fn test_comments_parsed() {
        let source = "// This is a comment\n/// Doc comment\nfn foo() {}\n";
        let analyzer = RustAnalyzer;
        let units = analyzer.parse_structure(source);
        assert_eq!(units[0].kind, UnitKind::Comment);
        assert_eq!(units[0].span, (0, 2)); // Both comment lines grouped.
    }

    #[test]
    fn test_const_and_static() {
        let source = "const MAX: i32 = 100;\nstatic GLOBAL: &str = \"hello\";\n";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 2, "should find const and static: {defs:?}");
        assert_eq!(defs[0].name, "MAX");
        assert_eq!(defs[1].name, "GLOBAL");
    }

    #[test]
    fn test_mod_declaration_is_import() {
        let source = "mod tests;\npub mod utils;\n";
        let analyzer = RustAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(
            imports.len(),
            2,
            "mod declarations are imports: {imports:?}"
        );
    }

    #[test]
    fn test_mod_with_body_is_definition() {
        let source = "mod tests {\n    fn test_one() {}\n}\n";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "tests");
        assert_eq!(defs[0].def_kind, "module");
    }

    #[test]
    fn test_async_fn() {
        let source = "pub async fn fetch_data() -> Result<()> {\n    Ok(())\n}\n";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "fetch_data");
        assert_eq!(defs[0].def_kind, "function");
    }

    #[test]
    fn test_empty_source() {
        let analyzer = RustAnalyzer;
        let units = analyzer.parse_structure("");
        assert!(units.is_empty());
    }

    #[test]
    fn test_type_alias() {
        let source = "type Result<T> = std::result::Result<T, Error>;\n";
        let analyzer = RustAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "Result");
        assert_eq!(defs[0].def_kind, "type_alias");
    }
}
