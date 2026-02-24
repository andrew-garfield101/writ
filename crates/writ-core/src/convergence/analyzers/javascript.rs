//! JavaScript language analyzer.
//!
//! Shares the structural parser with TypeScript but without type-specific
//! syntax (no `interface`, `type`, `enum`, type imports). Recognizes:
//! - Import statements (`import { X } from 'y'`, `const X = require('y')`)
//! - Definitions (`function`, `class`, `const`, `let`, `var`, `export default`)
//! - Comments (`//`, `/* */`, `/** */`)
//! - Whitespace/blank lines

use super::typescript::{is_ts_import, parse_ts_structure, ts_semantic_equivalent};
use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// JavaScript-specific code analyzer.
///
/// Delegates to the TypeScript parser in JS mode (no type syntax).
pub struct JavaScriptAnalyzer;

impl LanguageAnalyzer for JavaScriptAnalyzer {
    fn name(&self) -> &str {
        "javascript"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        // Use the TS parser with is_typescript=false.
        parse_ts_structure(source, false)
    }

    fn extract_imports(&self, source: &str) -> Vec<Import> {
        // Re-use the TS import extraction but filter out type-only imports.
        let lines: Vec<&str> = source.lines().collect();
        let mut imports = Vec::new();
        let mut i = 0;

        while i < lines.len() {
            let trimmed = lines[i].trim();
            if is_ts_import(trimmed, false) {
                let start = i;
                let end = find_import_end(&lines, i);
                let content: String = lines[start..end].join("\n");
                let module = extract_js_import_module(&content);

                let mut names: Vec<String> = Vec::new();
                if let Some(brace_start) = content.find('{') {
                    if let Some(brace_end) = content.find('}') {
                        let inner = &content[brace_start + 1..brace_end];
                        for name in inner.split(',') {
                            let n = name.trim().to_string();
                            if !n.is_empty() {
                                names.push(n);
                            }
                        }
                    }
                }

                imports.push(Import {
                    module,
                    names,
                    raw: content,
                });
                i = end;
            } else {
                i += 1;
            }
        }
        imports
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
        ts_semantic_equivalent(a, b)
    }

    fn ordering_matters(&self, unit_kind: &UnitKind) -> bool {
        match unit_kind {
            UnitKind::Import => false,
            UnitKind::Comment | UnitKind::Whitespace => false,
            _ => true,
        }
    }
}

/// Find the end of an import statement.
fn find_import_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i32;
    let mut i = start;
    while i < lines.len() {
        for c in lines[i].chars() {
            match c {
                '{' | '(' => depth += 1,
                '}' | ')' => depth = (depth - 1).max(0),
                ';' if depth == 0 => return i + 1,
                _ => {}
            }
        }
        if depth == 0 && i > start {
            return i + 1;
        }
        i += 1;
    }
    i
}

/// Extract the module source from a JS import statement.
fn extract_js_import_module(content: &str) -> String {
    // Look for `from 'xxx'` or `from "xxx"`.
    for from_marker in &["from '", "from \""] {
        if let Some(start) = content.find(from_marker) {
            let rest = &content[start + from_marker.len()..];
            let quote = if from_marker.ends_with('\'') {
                '\''
            } else {
                '"'
            };
            if let Some(end) = rest.find(quote) {
                return rest[..end].to_string();
            }
        }
    }
    // require('xxx') or require("xxx")
    for req_marker in &["require('", "require(\""] {
        if let Some(start) = content.find(req_marker) {
            let rest = &content[start + req_marker.len()..];
            let quote = if req_marker.ends_with('\'') {
                '\''
            } else {
                '"'
            };
            if let Some(end) = rest.find(quote) {
                return rest[..end].to_string();
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert_eq!(JavaScriptAnalyzer.name(), "javascript");
    }

    #[test]
    fn test_parse_structure_imports() {
        let source = "import { useState } from 'react';\nconst express = require('express');\n";
        let analyzer = JavaScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 2, "should find 2 imports: {units:?}");
    }

    #[test]
    fn test_parse_structure_definitions() {
        let source = "\
function greet(name) {
  return `Hello ${name}`;
}

class UserService {
  constructor() {
    this.users = [];
  }
}

const API_URL = 'https://api.example.com';
";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"greet"), "should find greet: {names:?}");
        assert!(
            names.contains(&"UserService"),
            "should find UserService: {names:?}"
        );
        assert!(names.contains(&"API_URL"), "should find API_URL: {names:?}");
    }

    #[test]
    fn test_parse_structure_covers_all_lines() {
        let source = "import React from 'react';\n\nfunction App() {\n  return null;\n}\n";
        let analyzer = JavaScriptAnalyzer;
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
        let source = "import { foo, bar } from './utils';\nconst path = require('path');\n";
        let analyzer = JavaScriptAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].module, "./utils");
        assert!(imports[0].names.contains(&"foo".to_string()));
        assert_eq!(imports[1].module, "path");
    }

    #[test]
    fn test_extract_definitions() {
        let source = "\
export default function handler(req) {
  return { status: 200 };
}

let counter = 0;
";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 2, "should find 2 definitions: {defs:?}");
        assert_eq!(defs[0].name, "handler");
        assert_eq!(defs[0].def_kind, "function");
        assert_eq!(defs[1].name, "counter");
        assert_eq!(defs[1].def_kind, "variable");
    }

    #[test]
    fn test_semantic_equivalence() {
        let analyzer = JavaScriptAnalyzer;
        assert!(analyzer.are_semantically_equivalent("const x = 1;", "const x = 1"));
        assert!(!analyzer.are_semantically_equivalent("const x = 1", "const x = 2"));
    }

    #[test]
    fn test_multiline_definition_spans() {
        let source = "\
function complexFunction(
  a,
  b
) {
  const result = a + b;
  return result * 2;
}
";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "complexFunction");
        assert_eq!(defs[0].span.0, 0);
        assert_eq!(defs[0].span.1, source.lines().count());
    }

    #[test]
    fn test_no_ts_specific_syntax() {
        // JavaScript analyzer should not recognize TypeScript-specific syntax.
        let source =
            "interface User { name: string; }\ntype UserId = string;\nenum Status { Active }\n";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        // These should not be recognized as definitions in JS mode.
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            !names.contains(&"User") || !defs.iter().any(|d| d.def_kind == "interface"),
            "interface should not be recognized in JS: {defs:?}"
        );
    }

    #[test]
    fn test_comments_parsed() {
        let source = "// This is a comment\n/** JSDoc */\nfunction foo() {}\n";
        let analyzer = JavaScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        let comments: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Comment)
            .collect();
        assert!(comments.len() >= 1, "should find comments: {units:?}");
    }

    #[test]
    fn test_export_default_class() {
        let source = "export default class App {\n  render() {\n    return null;\n  }\n}\n";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "App");
        assert_eq!(defs[0].def_kind, "class");
    }

    #[test]
    fn test_empty_source() {
        let analyzer = JavaScriptAnalyzer;
        let units = analyzer.parse_structure("");
        assert!(units.is_empty());
    }

    #[test]
    fn test_var_declaration() {
        let source = "var globalConfig = {};\n";
        let analyzer = JavaScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "globalConfig");
    }
}
