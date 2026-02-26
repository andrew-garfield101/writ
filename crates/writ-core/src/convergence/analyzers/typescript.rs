//! TypeScript language analyzer.
//!
//! Parses TypeScript source code into structural units with awareness of:
//! - Import statements (`import { X } from 'y'`, `import X from 'y'`,
//!   `import type { X } from 'y'`, `require()`)
//! - Definitions (`function`, `class`, `interface`, `type`, `enum`,
//!   `const`, `let`, `export default`, `export const`)
//! - Decorators (`@Component(...)`)
//! - Comments (`//`, `/* */`, `/** */` JSDoc)
//! - Whitespace/blank lines
//!
//! The JavaScript analyzer extends this with a reduced feature set
//! (no type imports, interface, enum, etc.).

use super::{Definition, Import, LanguageAnalyzer};
use crate::convergence::types::{StructuralUnit, UnitKind};

/// TypeScript-specific code analyzer.
pub struct TypeScriptAnalyzer;

impl LanguageAnalyzer for TypeScriptAnalyzer {
    fn name(&self) -> &str {
        "typescript"
    }

    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit> {
        parse_ts_structure(source, true)
    }

    fn extract_imports(&self, source: &str) -> Vec<Import> {
        extract_ts_imports(source)
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

// ---------------------------------------------------------------------------
// Shared parsing logic (used by both TypeScript and JavaScript analyzers)
// ---------------------------------------------------------------------------

/// Parse TypeScript/JavaScript source into structural units.
///
/// When `is_typescript` is true, recognizes `interface`, `type`, `enum`,
/// and `import type` syntax. When false, behaves as JavaScript.
pub(crate) fn parse_ts_structure(source: &str, is_typescript: bool) -> Vec<StructuralUnit> {
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

        // Block comments: /* */ and /** */ (JSDoc)
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

        // Decorators (TypeScript): @Component(...) etc.
        if is_typescript && trimmed.starts_with('@') {
            let start = i;
            let mut decorators: Vec<String> = Vec::new();
            while i < lines.len() && lines[i].trim().starts_with('@') {
                decorators.push(lines[i].trim().to_string());
                i += 1;
            }
            // Check if followed by a definition.
            if i < lines.len() && is_ts_def_start(lines[i].trim(), is_typescript) {
                let (def_kind, name) = parse_ts_def_header(lines[i].trim(), is_typescript);
                let end = find_js_block_end(&lines, i);
                i = end;
                let content: String = lines[start..i].join("\n");
                let mut unit =
                    StructuralUnit::new(UnitKind::Definition, Some(name), (start, i), content);
                unit.metadata.insert("def_kind".into(), def_kind);
                unit.metadata
                    .insert("decorators".into(), decorators.join(", "));
                units.push(unit);
            } else {
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

        // Import/require/export-from statements
        if is_ts_import(trimmed, is_typescript) {
            let start = i;
            // Multi-line imports: import {\n  X,\n  Y\n} from 'z';
            let end = find_semicolon_or_newline_end(&lines, i);
            i = end;
            let content: String = lines[start..i].join("\n");
            let module = extract_ts_import_module(&content);
            let lang_name = if is_typescript {
                "typescript"
            } else {
                "javascript"
            };
            let mut unit = StructuralUnit::new(
                UnitKind::Import,
                Some(module.clone()),
                (start, i),
                content.clone(),
            );
            unit.metadata.insert("import_lang".into(), lang_name.into());
            unit.metadata.insert("import_module".into(), module);
            // Extract named imports from { ... } block.
            if let Some(brace_start) = content.find('{') {
                if let Some(brace_end) = content.find('}') {
                    let inner = &content[brace_start + 1..brace_end];
                    let mut names: Vec<String> = inner
                        .split(',')
                        .map(|s| {
                            let s = s.trim();
                            // Handle `name as alias`.
                            if let Some(idx) = s.find(" as ") {
                                s[..idx].trim().to_string()
                            } else {
                                s.to_string()
                            }
                        })
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !names.is_empty() {
                        names.sort();
                        unit.metadata
                            .insert("import_names".into(), names.join(", "));
                    }
                }
            }
            units.push(unit);
            continue;
        }

        // Top-level definitions
        if is_ts_def_start(trimmed, is_typescript) {
            let start = i;
            let (def_kind, name) = parse_ts_def_header(trimmed, is_typescript);
            let end = find_js_block_end(&lines, i);
            i = end;
            let content: String = lines[start..i].join("\n");
            let mut unit =
                StructuralUnit::new(UnitKind::Definition, Some(name), (start, i), content);
            unit.metadata.insert("def_kind".into(), def_kind);
            if trimmed.starts_with("export ") {
                unit.metadata.insert("visibility".into(), "export".into());
            }
            if trimmed.contains("async ") {
                unit.metadata.insert("async".into(), "true".into());
            }
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

/// Check if a trimmed line is a TS/JS import statement.
pub(crate) fn is_ts_import(trimmed: &str, is_typescript: bool) -> bool {
    if trimmed.starts_with("import ") || trimmed.starts_with("import{") {
        // Exclude `import()` dynamic imports which are expressions.
        if trimmed.starts_with("import(") {
            return false;
        }
        return true;
    }
    if is_typescript && trimmed.starts_with("import type ") {
        return true;
    }
    // require() pattern
    if trimmed.contains("require(")
        && (trimmed.starts_with("const ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("var "))
    {
        return true;
    }
    // Re-exports: export { X } from 'y'
    if trimmed.starts_with("export {") && trimmed.contains("from ") {
        return true;
    }
    if trimmed.starts_with("export * from ") {
        return true;
    }
    false
}

/// Check if a trimmed line starts a TS/JS definition.
pub(crate) fn is_ts_def_start(trimmed: &str, is_typescript: bool) -> bool {
    // Strip export/export default/declare prefix.
    let after_prefix = strip_ts_prefix(trimmed);

    after_prefix.starts_with("function ")
        || after_prefix.starts_with("function*(")
        || after_prefix.starts_with("async function ")
        || after_prefix.starts_with("class ")
        || after_prefix.starts_with("abstract class ")
        || after_prefix.starts_with("const ")
        || after_prefix.starts_with("let ")
        || after_prefix.starts_with("var ")
        || (is_typescript && after_prefix.starts_with("interface "))
        || (is_typescript && after_prefix.starts_with("type "))
        || (is_typescript && after_prefix.starts_with("enum "))
        || (is_typescript && after_prefix.starts_with("namespace "))
}

/// Strip `export`, `export default`, `declare` prefixes.
fn strip_ts_prefix(trimmed: &str) -> &str {
    let mut rest = trimmed;
    if rest.starts_with("export default ") {
        rest = &rest[15..];
    } else if rest.starts_with("export ") {
        rest = &rest[7..];
    }
    if rest.starts_with("declare ") {
        rest = &rest[8..];
    }
    rest
}

/// Parse a TS/JS definition header to extract kind and name.
pub(crate) fn parse_ts_def_header(trimmed: &str, is_typescript: bool) -> (String, String) {
    let after_prefix = strip_ts_prefix(trimmed);

    if after_prefix.starts_with("async function ") {
        let name = extract_js_ident(&after_prefix[15..]);
        return ("function".into(), name);
    }
    if after_prefix.starts_with("function*") {
        let rest = after_prefix[9..].trim_start_matches(|c: char| c == '(' || c == ' ');
        let name = extract_js_ident(rest);
        return ("function".into(), name);
    }
    if after_prefix.starts_with("function ") {
        let name = extract_js_ident(&after_prefix[9..]);
        return ("function".into(), name);
    }
    if after_prefix.starts_with("abstract class ") {
        let name = extract_js_ident(&after_prefix[15..]);
        return ("class".into(), name);
    }
    if after_prefix.starts_with("class ") {
        let name = extract_js_ident(&after_prefix[6..]);
        return ("class".into(), name);
    }
    if is_typescript && after_prefix.starts_with("interface ") {
        let name = extract_js_ident(&after_prefix[10..]);
        return ("interface".into(), name);
    }
    if is_typescript && after_prefix.starts_with("type ") {
        let name = extract_js_ident(&after_prefix[5..]);
        return ("type_alias".into(), name);
    }
    if is_typescript && after_prefix.starts_with("enum ") {
        let name = extract_js_ident(&after_prefix[5..]);
        return ("enum".into(), name);
    }
    if is_typescript && after_prefix.starts_with("namespace ") {
        let name = extract_js_ident(&after_prefix[10..]);
        return ("namespace".into(), name);
    }
    if after_prefix.starts_with("const ") {
        let name = extract_js_ident(&after_prefix[6..]);
        return ("variable".into(), name);
    }
    if after_prefix.starts_with("let ") {
        let name = extract_js_ident(&after_prefix[4..]);
        return ("variable".into(), name);
    }
    if after_prefix.starts_with("var ") {
        let name = extract_js_ident(&after_prefix[4..]);
        return ("variable".into(), name);
    }

    ("unknown".into(), String::new())
}

/// Extract JS/TS identifier.
fn extract_js_ident(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Extract the module source from an import statement.
fn extract_ts_import_module(content: &str) -> String {
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

/// Extract imports from TS/JS source.
fn extract_ts_imports(source: &str) -> Vec<Import> {
    let lines: Vec<&str> = source.lines().collect();
    let mut imports = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        if is_ts_import(trimmed, true) {
            let start = i;
            let end = find_semicolon_or_newline_end(&lines, i);
            let content: String = lines[start..end].join("\n");
            let module = extract_ts_import_module(&content);

            // Extract named imports.
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

/// Find the end of a JS/TS block (brace-delimited or single-line ending with `;`).
pub(crate) fn find_js_block_end(lines: &[&str], start: usize) -> usize {
    let first_line = lines[start].trim();

    // Single-line declarations without braces.
    if (first_line.ends_with(';') || first_line.ends_with(',')) && !first_line.contains('{') {
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

        // If we haven't found braces and line ends with `;`, it's a single statement.
        if !found_open && i > start + 1 && lines[i - 1].trim().ends_with(';') {
            return i;
        }
    }
    i
}

/// Find the end of a semicolon-terminated import (possibly multi-line).
fn find_semicolon_or_newline_end(lines: &[&str], start: usize) -> usize {
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
        // If line ends without `;` and braces are balanced, single-line import.
        if depth == 0 && i > start {
            return i + 1;
        }
        i += 1;
    }
    i
}

/// Semantic equivalence for TS/JS.
pub(crate) fn ts_semantic_equivalent(a: &str, b: &str) -> bool {
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
                // Normalize: strip trailing semicolons and commas from the line.
                let mut normalized = trimmed.trim_end_matches(';').to_string();
                normalized = normalized.trim_end_matches(',').to_string();
                // Also normalize trailing commas before closing braces/parens:
                // `{ a, b, }` → `{ a, b }`
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert_eq!(TypeScriptAnalyzer.name(), "typescript");
    }

    #[test]
    fn test_parse_structure_imports() {
        let source = "import { useState, useEffect } from 'react';\nimport type { FC } from 'react';\nimport axios from 'axios';\n";
        let analyzer = TypeScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 3, "should find 3 imports: {units:?}");
    }

    #[test]
    fn test_parse_structure_definitions() {
        let source = "\
export function greet(name: string): string {
  return `Hello ${name}`;
}

export class UserService {
  private users: User[] = [];

  add(user: User): void {
    this.users.push(user);
  }
}

interface User {
  name: string;
  email: string;
}

type UserId = string;

enum Status {
  Active,
  Inactive,
}
";
        let analyzer = TypeScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"greet"), "should find greet: {names:?}");
        assert!(
            names.contains(&"UserService"),
            "should find UserService: {names:?}"
        );
        assert!(names.contains(&"User"), "should find User: {names:?}");
        assert!(names.contains(&"UserId"), "should find UserId: {names:?}");
        assert!(names.contains(&"Status"), "should find Status: {names:?}");
    }

    #[test]
    fn test_parse_structure_covers_all_lines() {
        let source = "import { useState } from 'react';\n\nfunction App() {\n  return null;\n}\n";
        let analyzer = TypeScriptAnalyzer;
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
        let source = "import { useState, useEffect } from 'react';\nimport axios from 'axios';\n";
        let analyzer = TypeScriptAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].module, "react");
        assert!(imports[0].names.contains(&"useState".to_string()));
        assert!(imports[0].names.contains(&"useEffect".to_string()));
        assert_eq!(imports[1].module, "axios");
    }

    #[test]
    fn test_extract_definitions() {
        let source = "\
export const API_URL = 'https://api.example.com';

export default function handler(req: Request) {
  return new Response('ok');
}
";
        let analyzer = TypeScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 2, "should find 2 definitions: {defs:?}");
        assert_eq!(defs[0].name, "API_URL");
        assert_eq!(defs[0].def_kind, "variable");
        assert_eq!(defs[1].name, "handler");
        assert_eq!(defs[1].def_kind, "function");
    }

    #[test]
    fn test_semantic_equivalence() {
        let analyzer = TypeScriptAnalyzer;
        // Trailing semicolons.
        assert!(analyzer.are_semantically_equivalent("const x = 1;", "const x = 1"));
        // Trailing commas.
        assert!(analyzer
            .are_semantically_equivalent("import { a, b, } from 'x'", "import { a, b } from 'x'"));
        // Real difference.
        assert!(!analyzer.are_semantically_equivalent("const x = 1", "const x = 2"));
    }

    #[test]
    fn test_multiline_definition_spans() {
        let source = "\
export class ComplexService {
  private cache: Map<string, unknown> = new Map();

  async fetch(url: string): Promise<unknown> {
    if (this.cache.has(url)) {
      return this.cache.get(url);
    }
    const result = await fetch(url);
    this.cache.set(url, result);
    return result;
  }
}
";
        let analyzer = TypeScriptAnalyzer;
        let defs = analyzer.extract_definitions(source);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "ComplexService");
        assert_eq!(defs[0].span.0, 0);
        assert_eq!(defs[0].span.1, source.lines().count());
    }

    #[test]
    fn test_decorator_attached_to_class() {
        let source = "@Component({\n  selector: 'app-root'\n})\n@Injectable()\nexport class AppComponent {\n  title = 'app';\n}\n";
        let analyzer = TypeScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        let defs: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Definition)
            .collect();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.as_deref(), Some("AppComponent"));
        assert!(defs[0].metadata.contains_key("decorators"));
    }

    #[test]
    fn test_require_as_import() {
        let source = "const express = require('express');\nconst path = require('path');\n";
        let analyzer = TypeScriptAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(
            imports.len(),
            2,
            "require() should be treated as import: {imports:?}"
        );
        assert_eq!(imports[0].module, "express");
        assert_eq!(imports[1].module, "path");
    }

    #[test]
    fn test_jsdoc_comment() {
        let source = "/**\n * Process input data.\n * @param data - The input data.\n */\nfunction process(data: unknown) {}\n";
        let analyzer = TypeScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        assert_eq!(units[0].kind, UnitKind::Comment);
        assert!(units[0].content.contains("@param"));
    }

    #[test]
    fn test_export_star_is_import() {
        let source = "export * from './utils';\nexport { foo } from './bar';\n";
        let analyzer = TypeScriptAnalyzer;
        let imports = analyzer.extract_imports(source);
        assert_eq!(
            imports.len(),
            2,
            "re-exports should be imports: {imports:?}"
        );
    }

    #[test]
    fn test_empty_source() {
        let analyzer = TypeScriptAnalyzer;
        let units = analyzer.parse_structure("");
        assert!(units.is_empty());
    }

    #[test]
    fn test_import_metadata_populated() {
        let source = "import { useState, useEffect } from 'react';\nimport axios from 'axios';\n";
        let analyzer = TypeScriptAnalyzer;
        let units = analyzer.parse_structure(source);
        let imports: Vec<_> = units
            .iter()
            .filter(|u| u.kind == UnitKind::Import)
            .collect();
        assert_eq!(imports.len(), 2);

        // Named import
        assert_eq!(
            imports[0].metadata.get("import_lang").unwrap(),
            "typescript"
        );
        assert_eq!(imports[0].metadata.get("import_module").unwrap(), "react");
        assert_eq!(
            imports[0].metadata.get("import_names").unwrap(),
            "useEffect, useState"
        );

        // Default import (no names)
        assert_eq!(
            imports[1].metadata.get("import_lang").unwrap(),
            "typescript"
        );
        assert_eq!(imports[1].metadata.get("import_module").unwrap(), "axios");
        assert!(imports[1].metadata.get("import_names").is_none());
    }
}
