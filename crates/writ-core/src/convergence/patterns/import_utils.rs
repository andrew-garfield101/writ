//! Language-aware import utilities for the pattern resolution engine.
//!
//! Provides parsing and reconstruction of import statements across all
//! supported languages, using metadata populated by language analyzers.
//! Falls back to content-based detection when metadata is absent.

use crate::convergence::types::StructuralUnit;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Detected language of an import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportLang {
    Python,
    Rust,
    Go,
    TypeScript,
    JavaScript,
    Unknown,
}

/// Parsed representation of an import statement, language-agnostic.
#[derive(Debug, Clone)]
pub struct ParsedImport {
    /// Language of the import.
    pub lang: ImportLang,
    /// Module or package path (e.g. "flask", "std::collections", "react").
    pub module: String,
    /// Imported names (e.g. ["Flask", "jsonify"]). Empty for bare imports.
    /// For Go grouped imports, these are the package paths.
    pub names: Vec<String>,
    /// Original raw content of the import statement.
    pub raw: String,
}

// ---------------------------------------------------------------------------
// Language Detection
// ---------------------------------------------------------------------------

/// Detect the language of an import unit from metadata, falling back
/// to content-based detection.
pub fn detect_lang(unit: &StructuralUnit) -> ImportLang {
    if let Some(lang) = unit.metadata.get("import_lang") {
        return match lang.as_str() {
            "python" => ImportLang::Python,
            "rust" => ImportLang::Rust,
            "go" => ImportLang::Go,
            "typescript" => ImportLang::TypeScript,
            "javascript" => ImportLang::JavaScript,
            _ => ImportLang::Unknown,
        };
    }
    detect_lang_from_content(&unit.content)
}

/// Fallback: detect language from import content syntax.
fn detect_lang_from_content(content: &str) -> ImportLang {
    let trimmed = content.trim();

    // Rust: use/pub use/extern crate/mod
    if trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("pub(crate) use ")
        || trimmed.starts_with("extern crate ")
    {
        return ImportLang::Rust;
    }

    // Go: import "pkg" or import ( ... )
    if trimmed.starts_with("import \"")
        || trimmed.starts_with("import `")
        || trimmed.starts_with("import (")
        || trimmed == "import("
    {
        return ImportLang::Go;
    }

    // TS/JS: import { X } from 'y' or require()
    if (trimmed.starts_with("import ") || trimmed.starts_with("import{"))
        && (trimmed.contains(" from '")
            || trimmed.contains(" from \"")
            || trimmed.contains("require("))
    {
        return ImportLang::TypeScript;
    }
    if trimmed.contains("require(")
        && (trimmed.starts_with("const ")
            || trimmed.starts_with("let ")
            || trimmed.starts_with("var "))
    {
        return ImportLang::TypeScript;
    }
    if trimmed.starts_with("export {") && trimmed.contains(" from ") {
        return ImportLang::TypeScript;
    }
    if trimmed.starts_with("export * from ") {
        return ImportLang::TypeScript;
    }

    // Python: from X import Y or import X
    if trimmed.starts_with("from ") && trimmed.contains(" import ") {
        return ImportLang::Python;
    }
    if trimmed.starts_with("import ") {
        return ImportLang::Python;
    }

    ImportLang::Unknown
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parse an import unit into a [`ParsedImport`] using metadata when available,
/// falling back to content-based parsing.
pub fn parse_import(unit: &StructuralUnit) -> ParsedImport {
    let lang = detect_lang(unit);

    // Try metadata first (populated by analyzers during Phase 1).
    // Fall back to content-based module extraction if metadata is absent.
    let module = unit
        .metadata
        .get("import_module")
        .cloned()
        .unwrap_or_else(|| {
            parse_module_from_content(&unit.content, &lang)
                .unwrap_or_else(|| unit.name.clone().unwrap_or_default())
        });

    let names = if let Some(names_str) = unit.metadata.get("import_names") {
        if names_str.is_empty() {
            Vec::new()
        } else {
            names_str
                .split(", ")
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
    } else {
        // Fallback: parse from content.
        parse_names_from_content(&unit.content, &lang)
    };

    ParsedImport {
        lang,
        module,
        names,
        raw: unit.content.clone(),
    }
}

/// Parse import details from raw content string (no unit required).
/// Uses content-based language detection and parsing.
pub fn parse_import_content(content: &str) -> ParsedImport {
    let lang = detect_lang_from_content(content);
    let module = parse_module_from_content(content, &lang).unwrap_or_default();
    let names = parse_names_from_content(content, &lang);

    ParsedImport {
        lang,
        module,
        names,
        raw: content.to_string(),
    }
}

/// Fallback module extraction from raw content, per language.
fn parse_module_from_content(content: &str, lang: &ImportLang) -> Option<String> {
    let trimmed = content.trim();
    match lang {
        ImportLang::Python => {
            if trimmed.starts_with("from ") {
                let rest = &trimmed[5..];
                rest.split_whitespace().next().map(|s| s.to_string())
            } else if trimmed.starts_with("import ") {
                let rest = &trimmed[7..];
                rest.split(|c: char| c == ',' || c.is_whitespace())
                    .next()
                    .map(|s| s.to_string())
            } else {
                None
            }
        }
        ImportLang::Rust => {
            // Strip visibility prefix.
            let after_vis = if trimmed.starts_with("pub(crate) ") {
                &trimmed[11..]
            } else if trimmed.starts_with("pub ") {
                &trimmed[4..]
            } else {
                trimmed
            };
            if after_vis.starts_with("use ") {
                let rest = after_vis[4..].trim_end_matches(';').trim();
                if let Some(brace_start) = rest.find('{') {
                    Some(rest[..brace_start].trim_end_matches("::").to_string())
                } else {
                    Some(
                        rest.split(|c: char| c == ' ' || c == ';')
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    )
                }
            } else {
                None
            }
        }
        ImportLang::Go => {
            if trimmed.contains('(') {
                Some("(grouped)".to_string())
            } else if let Some(start) = trimmed.find('"') {
                let after = &trimmed[start + 1..];
                after.find('"').map(|end| after[..end].to_string())
            } else {
                None
            }
        }
        ImportLang::TypeScript | ImportLang::JavaScript => {
            // Extract from: from 'module' or from "module" or require('module')
            for pattern in &["from '", "from \"", "require('", "require(\""] {
                if let Some(idx) = trimmed.find(pattern) {
                    let after = &trimmed[idx + pattern.len()..];
                    let quote = if pattern.contains('\'') { '\'' } else { '"' };
                    if let Some(end) = after.find(quote) {
                        return Some(after[..end].to_string());
                    }
                }
            }
            None
        }
        ImportLang::Unknown => None,
    }
}

/// Fallback name extraction from raw content, per language.
fn parse_names_from_content(content: &str, lang: &ImportLang) -> Vec<String> {
    let trimmed = content.trim();
    match lang {
        ImportLang::Python => parse_python_names(trimmed),
        ImportLang::Rust => parse_rust_names(trimmed),
        ImportLang::Go => parse_go_names(trimmed),
        ImportLang::TypeScript | ImportLang::JavaScript => parse_ts_names(trimmed),
        ImportLang::Unknown => Vec::new(),
    }
}

/// Extract imported names from Python `from X import a, b` or `import a, b`.
fn parse_python_names(content: &str) -> Vec<String> {
    let names_part = if let Some(idx) = content.find(" import ") {
        &content[idx + 8..]
    } else {
        return Vec::new();
    };

    // Strip parentheses if present: from X import (a, b)
    let names_str = names_part
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')');

    names_str
        .split(',')
        .map(|s| {
            let s = s.trim();
            // Handle `name as alias` — keep the original name.
            if let Some(idx) = s.find(" as ") {
                s[..idx].trim().to_string()
            } else {
                s.to_string()
            }
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract imported names from Rust `use path::{A, B};`.
fn parse_rust_names(content: &str) -> Vec<String> {
    if let Some(brace_start) = content.find('{') {
        if let Some(brace_end) = content.rfind('}') {
            let inner = &content[brace_start + 1..brace_end];
            return inner
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && *s != "self")
                .collect();
        }
    }
    Vec::new()
}

/// Extract package paths from Go `import ( "fmt" "os" )`.
fn parse_go_names(content: &str) -> Vec<String> {
    if content.contains('(') {
        // Grouped import — extract quoted strings.
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                    || (trimmed.starts_with('`') && trimmed.ends_with('`'))
                {
                    Some(trimmed[1..trimmed.len() - 1].to_string())
                } else {
                    // Handle lines like: "fmt" // comment
                    let quote_start = trimmed.find('"').or_else(|| trimmed.find('`'));
                    let quote_char = quote_start.map(|i| trimmed.as_bytes()[i] as char);
                    if let (Some(start), Some(qc)) = (quote_start, quote_char) {
                        let after = &trimmed[start + 1..];
                        if let Some(end) = after.find(qc) {
                            return Some(after[..end].to_string());
                        }
                    }
                    None
                }
            })
            .collect()
    } else {
        // Single import — extract the quoted path.
        let trimmed = content.trim();
        let quote_start = trimmed.find('"').or_else(|| trimmed.find('`'));
        let quote_char = quote_start.map(|i| trimmed.as_bytes()[i] as char);
        if let (Some(start), Some(qc)) = (quote_start, quote_char) {
            let after = &trimmed[start + 1..];
            if let Some(end) = after.find(qc) {
                return vec![after[..end].to_string()];
            }
        }
        Vec::new()
    }
}

/// Extract named imports from TS/JS `import { a, b } from 'x'`.
fn parse_ts_names(content: &str) -> Vec<String> {
    if let Some(brace_start) = content.find('{') {
        if let Some(brace_end) = content.find('}') {
            let inner = &content[brace_start + 1..brace_end];
            return inner
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
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Preservation Check
// ---------------------------------------------------------------------------

/// Check if a side's import preserves all names from the base import
/// (possibly extending it with additional names).
pub fn import_is_preserved(base: &ParsedImport, side: &ParsedImport) -> bool {
    // Direct module match.
    if base.module == side.module {
        // If base has no parsed names, fall back to raw content comparison.
        if base.names.is_empty() {
            return base.raw.trim() == side.raw.trim();
        }
        // All base names must be present in side.
        let side_set: HashSet<&String> = side.names.iter().collect();
        return base.names.iter().all(|n| side_set.contains(n));
    }

    // Rust-specific: `use path::Item;` (no names, module=path::Item) extended
    // to `use path::{Item, Other};` (names=[Item, Other], module=path).
    // Check if base module = side_module::leaf and leaf is in side names.
    if base.lang == ImportLang::Rust && base.names.is_empty() && !side.names.is_empty() {
        if let Some(last_sep) = base.module.rfind("::") {
            let parent = &base.module[..last_sep];
            let leaf = &base.module[last_sep + 2..];
            if parent == side.module && side.names.iter().any(|n| n == leaf) {
                return true;
            }
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Merging
// ---------------------------------------------------------------------------

/// Merge three versions of the same-module import into one with the union
/// of all imported names.
pub fn merge_same_module(base: &ParsedImport, left: &ParsedImport, right: &ParsedImport) -> String {
    let mut all_names: HashSet<String> = HashSet::new();

    // For Rust: if base has no names but module is `path::Item`,
    // extract the leaf name so it's included in the union.
    if base.names.is_empty() && base.lang == ImportLang::Rust {
        if let Some(last_sep) = base.module.rfind("::") {
            let leaf = &base.module[last_sep + 2..];
            all_names.insert(leaf.to_string());
        }
    }
    for name in &base.names {
        all_names.insert(name.clone());
    }
    for name in &left.names {
        all_names.insert(name.clone());
    }
    for name in &right.names {
        all_names.insert(name.clone());
    }
    let mut sorted: Vec<String> = all_names.into_iter().collect();
    sorted.sort();

    // Pick the most specific module path for reconstruction.
    // Prefer the grouped form (e.g. left/right with names) over
    // the base's leaf-in-path form.
    let module = if !left.names.is_empty() {
        &left.module
    } else if !right.names.is_empty() {
        &right.module
    } else {
        &base.module
    };

    // Use left's raw content as the template for reconstruction.
    reconstruct_import(&left.lang, module, &sorted, &left.raw)
}

/// Reconstruct an import statement for the given language.
pub fn reconstruct_import(
    lang: &ImportLang,
    module: &str,
    names: &[String],
    original: &str,
) -> String {
    match lang {
        ImportLang::Python => reconstruct_python(module, names, original),
        ImportLang::Rust => reconstruct_rust(module, names, original),
        ImportLang::Go => reconstruct_go(names, original),
        ImportLang::TypeScript | ImportLang::JavaScript => {
            reconstruct_ts_js(module, names, original)
        }
        ImportLang::Unknown => original.trim().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Per-language Reconstruction
// ---------------------------------------------------------------------------

fn reconstruct_python(module: &str, names: &[String], original: &str) -> String {
    if names.is_empty() {
        // Bare import: `import os`
        return original.trim().to_string();
    }
    let trimmed = original.trim();
    if trimmed.starts_with("from ") {
        format!("from {} import {}", module, names.join(", "))
    } else {
        format!("import {}", names.join(", "))
    }
}

fn reconstruct_rust(module: &str, names: &[String], original: &str) -> String {
    let trimmed = original.trim();

    // Detect visibility prefix.
    let vis_prefix = if trimmed.starts_with("pub(crate) ") {
        "pub(crate) "
    } else if trimmed.starts_with("pub ") {
        "pub "
    } else {
        ""
    };

    if names.is_empty() {
        // Whole module: `use std::io;`
        return trimmed.to_string();
    }

    if names.len() == 1 {
        // Check if original used braces for a single name.
        if original.contains('{') {
            format!("{}use {}::{{{}}};", vis_prefix, module, names[0])
        } else {
            format!("{}use {}::{};", vis_prefix, module, names[0])
        }
    } else {
        format!("{}use {}::{{{}}};", vis_prefix, module, names.join(", "))
    }
}

fn reconstruct_go(names: &[String], original: &str) -> String {
    if names.is_empty() {
        return original.trim().to_string();
    }
    if names.len() == 1 {
        return format!("import \"{}\"", names[0]);
    }
    // Grouped import block.
    let mut lines = vec!["import (".to_string()];
    for name in names {
        lines.push(format!("\t\"{}\"", name));
    }
    lines.push(")".to_string());
    lines.join("\n")
}

fn reconstruct_ts_js(module: &str, names: &[String], original: &str) -> String {
    if names.is_empty() {
        // Default or side-effect import — preserve as-is.
        return original.trim().to_string();
    }

    // Detect quote style from original.
    let quote = if original.contains('\'') { '\'' } else { '"' };

    // Detect type-only import.
    let trimmed = original.trim();
    let type_prefix = if trimmed.starts_with("import type ") {
        "type "
    } else {
        ""
    };

    format!(
        "import {}{{ {} }} from {}{}{};",
        type_prefix,
        names.join(", "),
        quote,
        module,
        quote,
    )
}

// ---------------------------------------------------------------------------
// Conflict Detection
// ---------------------------------------------------------------------------

/// Detect conflicting imports: same name imported from different modules.
/// Returns tuples of (name, module1, module2) for each conflict.
pub fn detect_name_conflicts(imports: &[ParsedImport]) -> Vec<(String, String, String)> {
    let mut name_to_module: HashMap<String, String> = HashMap::new();
    let mut conflicts = Vec::new();

    for imp in imports {
        for name in &imp.names {
            if let Some(prev_module) = name_to_module.get(name) {
                if prev_module != &imp.module {
                    conflicts.push((name.clone(), prev_module.clone(), imp.module.clone()));
                }
            } else {
                name_to_module.insert(name.clone(), imp.module.clone());
            }
        }
    }
    conflicts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::test_utils::helpers::{self, import_unit as bare_import};

    /// Import with metadata — uses module as both name and module key.
    fn import_with_meta(content: &str, lang: &str, module: &str, names: &str) -> StructuralUnit {
        helpers::import_with_meta(module, content, lang, module, names)
    }

    // ── Language detection ─────────────────────────────────────────────

    #[test]
    fn test_detect_lang_from_metadata() {
        for (lang_str, expected) in [
            ("python", ImportLang::Python),
            ("rust", ImportLang::Rust),
            ("go", ImportLang::Go),
            ("typescript", ImportLang::TypeScript),
            ("javascript", ImportLang::JavaScript),
        ] {
            let unit = import_with_meta("content", lang_str, "mod", "");
            assert_eq!(detect_lang(&unit), expected);
        }
    }

    #[test]
    fn test_detect_lang_fallback_python() {
        assert_eq!(
            detect_lang(&bare_import("from flask import Flask")),
            ImportLang::Python
        );
        assert_eq!(detect_lang(&bare_import("import os")), ImportLang::Python);
    }

    #[test]
    fn test_detect_lang_fallback_rust() {
        assert_eq!(detect_lang(&bare_import("use std::io;")), ImportLang::Rust);
        assert_eq!(
            detect_lang(&bare_import("pub use crate::foo;")),
            ImportLang::Rust
        );
        assert_eq!(
            detect_lang(&bare_import("extern crate serde;")),
            ImportLang::Rust
        );
    }

    #[test]
    fn test_detect_lang_fallback_go() {
        assert_eq!(detect_lang(&bare_import("import \"fmt\"")), ImportLang::Go);
        assert_eq!(
            detect_lang(&bare_import("import (\n\t\"fmt\"\n\t\"os\"\n)")),
            ImportLang::Go
        );
    }

    #[test]
    fn test_detect_lang_fallback_ts() {
        assert_eq!(
            detect_lang(&bare_import("import { useState } from 'react';")),
            ImportLang::TypeScript
        );
        assert_eq!(
            detect_lang(&bare_import("const express = require('express');")),
            ImportLang::TypeScript
        );
    }

    // ── Parsing ───────────────────────────────────────────────────────

    #[test]
    fn test_parse_import_python_with_metadata() {
        let unit = import_with_meta(
            "from flask import Flask, jsonify",
            "python",
            "flask",
            "Flask, jsonify",
        );
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Python);
        assert_eq!(parsed.module, "flask");
        assert_eq!(parsed.names, vec!["Flask", "jsonify"]);
    }

    #[test]
    fn test_parse_import_python_fallback() {
        let unit = bare_import("from flask import Flask, jsonify, request");
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Python);
        assert_eq!(parsed.names, vec!["Flask", "jsonify", "request"]);
    }

    #[test]
    fn test_parse_import_rust_with_metadata() {
        let unit = import_with_meta(
            "use std::collections::{HashMap, HashSet};",
            "rust",
            "std::collections",
            "HashMap, HashSet",
        );
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Rust);
        assert_eq!(parsed.module, "std::collections");
        assert_eq!(parsed.names, vec!["HashMap", "HashSet"]);
    }

    #[test]
    fn test_parse_import_rust_fallback() {
        let unit = bare_import("use std::collections::{HashMap, HashSet};");
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Rust);
        assert_eq!(parsed.names, vec!["HashMap", "HashSet"]);
    }

    #[test]
    fn test_parse_import_go_with_metadata() {
        let unit = import_with_meta(
            "import (\n\t\"fmt\"\n\t\"os\"\n)",
            "go",
            "(grouped)",
            "fmt, os",
        );
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Go);
        assert_eq!(parsed.names, vec!["fmt", "os"]);
    }

    #[test]
    fn test_parse_import_go_fallback() {
        let unit = bare_import("import (\n\t\"fmt\"\n\t\"os\"\n)");
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::Go);
        assert_eq!(parsed.names, vec!["fmt", "os"]);
    }

    #[test]
    fn test_parse_import_ts_with_metadata() {
        let unit = import_with_meta(
            "import { useState, useEffect } from 'react';",
            "typescript",
            "react",
            "useEffect, useState",
        );
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::TypeScript);
        assert_eq!(parsed.names, vec!["useEffect", "useState"]);
    }

    #[test]
    fn test_parse_import_ts_fallback() {
        let unit = bare_import("import { useState, useEffect } from 'react';");
        let parsed = parse_import(&unit);
        assert_eq!(parsed.lang, ImportLang::TypeScript);
        assert_eq!(parsed.names, vec!["useState", "useEffect"]);
    }

    // ── Preservation ──────────────────────────────────────────────────

    #[test]
    fn test_import_is_preserved_python_extension() {
        let base = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "jsonify".into()],
            raw: "from flask import Flask, jsonify".into(),
        };
        let side = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "jsonify".into(), "request".into()],
            raw: "from flask import Flask, jsonify, request".into(),
        };
        assert!(import_is_preserved(&base, &side));
    }

    #[test]
    fn test_import_is_preserved_name_removed() {
        let base = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "jsonify".into()],
            raw: "from flask import Flask, jsonify".into(),
        };
        let side = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into()],
            raw: "from flask import Flask".into(),
        };
        assert!(!import_is_preserved(&base, &side));
    }

    #[test]
    fn test_import_is_preserved_module_mismatch() {
        let base = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into()],
            raw: "from flask import Flask".into(),
        };
        let side = ParsedImport {
            lang: ImportLang::Python,
            module: "django".into(),
            names: vec!["Flask".into()],
            raw: "from django import Flask".into(),
        };
        assert!(!import_is_preserved(&base, &side));
    }

    #[test]
    fn test_import_is_preserved_rust() {
        let base = ParsedImport {
            lang: ImportLang::Rust,
            module: "std::collections".into(),
            names: vec!["HashMap".into()],
            raw: "use std::collections::HashMap;".into(),
        };
        let side = ParsedImport {
            lang: ImportLang::Rust,
            module: "std::collections".into(),
            names: vec!["HashMap".into(), "HashSet".into()],
            raw: "use std::collections::{HashMap, HashSet};".into(),
        };
        assert!(import_is_preserved(&base, &side));
    }

    // ── Reconstruction ────────────────────────────────────────────────

    #[test]
    fn test_reconstruct_python() {
        let result = reconstruct_import(
            &ImportLang::Python,
            "flask",
            &["Flask".into(), "abort".into(), "jsonify".into()],
            "from flask import Flask, jsonify",
        );
        assert_eq!(result, "from flask import Flask, abort, jsonify");
    }

    #[test]
    fn test_reconstruct_rust() {
        let result = reconstruct_import(
            &ImportLang::Rust,
            "std::collections",
            &["BTreeMap".into(), "HashMap".into(), "HashSet".into()],
            "use std::collections::{HashMap, HashSet};",
        );
        assert_eq!(
            result,
            "use std::collections::{BTreeMap, HashMap, HashSet};"
        );
    }

    #[test]
    fn test_reconstruct_rust_pub() {
        let result = reconstruct_import(
            &ImportLang::Rust,
            "crate::types",
            &["Config".into(), "Settings".into()],
            "pub use crate::types::Config;",
        );
        assert_eq!(result, "pub use crate::types::{Config, Settings};");
    }

    #[test]
    fn test_reconstruct_go_single() {
        let result = reconstruct_import(&ImportLang::Go, "fmt", &["fmt".into()], "import \"fmt\"");
        assert_eq!(result, "import \"fmt\"");
    }

    #[test]
    fn test_reconstruct_go_grouped() {
        let result = reconstruct_import(
            &ImportLang::Go,
            "(grouped)",
            &["fmt".into(), "net/http".into(), "os".into()],
            "import (\n\t\"fmt\"\n\t\"os\"\n)",
        );
        assert_eq!(result, "import (\n\t\"fmt\"\n\t\"net/http\"\n\t\"os\"\n)");
    }

    #[test]
    fn test_reconstruct_ts() {
        let result = reconstruct_import(
            &ImportLang::TypeScript,
            "react",
            &["useCallback".into(), "useEffect".into(), "useState".into()],
            "import { useState } from 'react';",
        );
        assert_eq!(
            result,
            "import { useCallback, useEffect, useState } from 'react';"
        );
    }

    #[test]
    fn test_reconstruct_ts_double_quotes() {
        let result = reconstruct_import(
            &ImportLang::TypeScript,
            "react",
            &["useState".into()],
            "import { useState } from \"react\";",
        );
        assert_eq!(result, "import { useState } from \"react\";");
    }

    #[test]
    fn test_reconstruct_ts_type_import() {
        let result = reconstruct_import(
            &ImportLang::TypeScript,
            "react",
            &["FC".into(), "ReactNode".into()],
            "import type { FC } from 'react';",
        );
        assert_eq!(result, "import type { FC, ReactNode } from 'react';");
    }

    // ── Merging ───────────────────────────────────────────────────────

    #[test]
    fn test_merge_same_module_python() {
        let base = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "jsonify".into()],
            raw: "from flask import Flask, jsonify".into(),
        };
        let left = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "jsonify".into(), "request".into()],
            raw: "from flask import Flask, jsonify, request".into(),
        };
        let right = ParsedImport {
            lang: ImportLang::Python,
            module: "flask".into(),
            names: vec!["Flask".into(), "abort".into(), "jsonify".into()],
            raw: "from flask import Flask, abort, jsonify".into(),
        };
        let result = merge_same_module(&base, &left, &right);
        assert_eq!(result, "from flask import Flask, abort, jsonify, request");
    }

    #[test]
    fn test_merge_same_module_rust() {
        let base = ParsedImport {
            lang: ImportLang::Rust,
            module: "std::collections".into(),
            names: vec!["HashMap".into()],
            raw: "use std::collections::HashMap;".into(),
        };
        let left = ParsedImport {
            lang: ImportLang::Rust,
            module: "std::collections".into(),
            names: vec!["HashMap".into(), "HashSet".into()],
            raw: "use std::collections::{HashMap, HashSet};".into(),
        };
        let right = ParsedImport {
            lang: ImportLang::Rust,
            module: "std::collections".into(),
            names: vec!["BTreeMap".into(), "HashMap".into()],
            raw: "use std::collections::{BTreeMap, HashMap};".into(),
        };
        let result = merge_same_module(&base, &left, &right);
        assert_eq!(
            result,
            "use std::collections::{BTreeMap, HashMap, HashSet};"
        );
    }

    #[test]
    fn test_merge_same_module_ts() {
        let base = ParsedImport {
            lang: ImportLang::TypeScript,
            module: "react".into(),
            names: vec!["useState".into()],
            raw: "import { useState } from 'react';".into(),
        };
        let left = ParsedImport {
            lang: ImportLang::TypeScript,
            module: "react".into(),
            names: vec!["useEffect".into(), "useState".into()],
            raw: "import { useState, useEffect } from 'react';".into(),
        };
        let right = ParsedImport {
            lang: ImportLang::TypeScript,
            module: "react".into(),
            names: vec!["useMemo".into(), "useState".into()],
            raw: "import { useState, useMemo } from 'react';".into(),
        };
        let result = merge_same_module(&base, &left, &right);
        assert_eq!(
            result,
            "import { useEffect, useMemo, useState } from 'react';"
        );
    }

    // ── Conflict detection ────────────────────────────────────────────

    #[test]
    fn test_detect_name_conflicts_found() {
        let imports = vec![
            ParsedImport {
                lang: ImportLang::Python,
                module: "auth.models".into(),
                names: vec!["User".into()],
                raw: "from auth.models import User".into(),
            },
            ParsedImport {
                lang: ImportLang::Python,
                module: "core.models".into(),
                names: vec!["User".into()],
                raw: "from core.models import User".into(),
            },
        ];
        let conflicts = detect_name_conflicts(&imports);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, "User");
    }

    #[test]
    fn test_detect_name_conflicts_none() {
        let imports = vec![
            ParsedImport {
                lang: ImportLang::Python,
                module: "os".into(),
                names: vec!["path".into()],
                raw: "from os import path".into(),
            },
            ParsedImport {
                lang: ImportLang::Python,
                module: "sys".into(),
                names: vec!["argv".into()],
                raw: "from sys import argv".into(),
            },
        ];
        let conflicts = detect_name_conflicts(&imports);
        assert!(conflicts.is_empty());
    }

    // ── Python alias handling ─────────────────────────────────────────

    #[test]
    fn test_python_alias_extraction() {
        let unit = bare_import("from datetime import datetime as dt, timedelta");
        let parsed = parse_import(&unit);
        assert_eq!(parsed.names, vec!["datetime", "timedelta"]);
    }
}
