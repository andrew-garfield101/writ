//! Language analyzers for structural code analysis.
//!
//! The [`LanguageAnalyzer`] trait defines how the convergence engine
//! understands source code. Each supported language provides an
//! implementation that can parse structure, extract imports and
//! definitions, and compare semantic equivalence.
//!
//! When no language-specific analyzer exists, the [`GenericAnalyzer`]
//! treats each line as an `Unknown` structural unit, preserving full
//! diff3 functionality.

pub mod generic;
pub mod go;
pub mod javascript;
pub mod python;
pub mod rust_lang;
pub mod typescript;

use super::types::{StructuralUnit, UnitKind};

// ---------------------------------------------------------------------------
// LanguageAnalyzer trait
// ---------------------------------------------------------------------------

/// A parsed import statement, language-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The module/package being imported from (e.g. "os", "flask", "std::io").
    pub module: String,
    /// Individual names imported (empty for bare imports like `import os`).
    pub names: Vec<String>,
    /// The raw source text of the import statement.
    pub raw: String,
}

/// A parsed top-level definition, language-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Definition {
    /// The name of the definition (function, class, struct, etc.).
    pub name: String,
    /// What kind of definition ("function", "class", "struct", "interface", etc.).
    pub def_kind: String,
    /// Line span in source (0-indexed, start inclusive, end exclusive).
    pub span: (usize, usize),
    /// The raw source text of the definition.
    pub content: String,
}

/// Trait for language-specific code analysis.
///
/// Implementations parse source code into [`StructuralUnit`]s and provide
/// language-aware operations that the convergence pipeline uses to make
/// better merge decisions.
///
/// # Contract
///
/// - `parse_structure()` must be total — it never fails, falling back to
///   `Unknown` units for anything it can't parse.
/// - `extract_imports()` and `extract_definitions()` are convenience
///   methods that can be derived from `parse_structure()` output but
///   may be optimized independently.
/// - `are_semantically_equivalent()` should be conservative — return
///   `false` if unsure. False negatives (unnecessary conflict) are
///   safe; false positives (missed conflict) are dangerous.
pub trait LanguageAnalyzer {
    /// The name of this analyzer (e.g. "python", "generic").
    fn name(&self) -> &str;

    /// Parse source code into a flat list of structural units.
    ///
    /// The returned units should cover the entire source — no gaps.
    /// Lines that can't be classified should be `UnitKind::Unknown`.
    fn parse_structure(&self, source: &str) -> Vec<StructuralUnit>;

    /// Extract import statements from source code.
    fn extract_imports(&self, source: &str) -> Vec<Import> {
        self.parse_structure(source)
            .into_iter()
            .filter(|u| u.kind == UnitKind::Import)
            .map(|u| Import {
                module: u.name.unwrap_or_default(),
                names: Vec::new(),
                raw: u.content,
            })
            .collect()
    }

    /// Extract top-level definitions from source code.
    fn extract_definitions(&self, source: &str) -> Vec<Definition> {
        self.parse_structure(source)
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

    /// Check if two code fragments are semantically equivalent.
    ///
    /// This allows the engine to detect that whitespace-only or
    /// formatting-only changes are not real conflicts. Be conservative:
    /// return `false` if unsure.
    fn are_semantically_equivalent(&self, a: &str, b: &str) -> bool {
        // Default: exact string match. Language-specific analyzers
        // can be smarter (ignore whitespace, normalize formatting, etc.).
        a == b
    }

    /// Does ordering matter for this kind of structural unit?
    ///
    /// For imports, ordering usually doesn't matter (PEP 8 is a style
    /// preference). For statements and definitions, ordering usually
    /// matters. This informs the pattern registry.
    fn ordering_matters(&self, unit_kind: &UnitKind) -> bool {
        match unit_kind {
            UnitKind::Import => false,
            UnitKind::Comment | UnitKind::Whitespace => false,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Analyzer dispatch
// ---------------------------------------------------------------------------

/// Select the appropriate analyzer for a file path.
///
/// Returns a boxed `LanguageAnalyzer` based on the file extension.
/// Falls back to `GenericAnalyzer` for unknown file types.
pub fn analyzer_for_path(path: &str) -> Box<dyn LanguageAnalyzer> {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "py" | "pyi" => Box::new(python::PythonAnalyzer),
        "rs" => Box::new(rust_lang::RustAnalyzer),
        "ts" | "tsx" => Box::new(typescript::TypeScriptAnalyzer),
        "js" | "jsx" | "mjs" | "cjs" => Box::new(javascript::JavaScriptAnalyzer),
        "go" => Box::new(go::GoAnalyzer),
        _ => Box::new(generic::GenericAnalyzer),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyzer_dispatch_python() {
        let analyzer = analyzer_for_path("models.py");
        assert_eq!(analyzer.name(), "python");
    }

    #[test]
    fn test_analyzer_dispatch_pyi() {
        let analyzer = analyzer_for_path("types.pyi");
        assert_eq!(analyzer.name(), "python");
    }

    #[test]
    fn test_analyzer_dispatch_unknown_falls_back_to_generic() {
        let analyzer = analyzer_for_path("config.yaml");
        assert_eq!(analyzer.name(), "generic");
    }

    #[test]
    fn test_analyzer_dispatch_rust() {
        let analyzer = analyzer_for_path("main.rs");
        assert_eq!(analyzer.name(), "rust");
    }

    #[test]
    fn test_analyzer_dispatch_go() {
        let analyzer = analyzer_for_path("main.go");
        assert_eq!(analyzer.name(), "go");
    }

    #[test]
    fn test_analyzer_dispatch_typescript() {
        let analyzer = analyzer_for_path("app.ts");
        assert_eq!(analyzer.name(), "typescript");
    }

    #[test]
    fn test_analyzer_dispatch_tsx() {
        let analyzer = analyzer_for_path("App.tsx");
        assert_eq!(analyzer.name(), "typescript");
    }

    #[test]
    fn test_analyzer_dispatch_javascript() {
        let analyzer = analyzer_for_path("index.js");
        assert_eq!(analyzer.name(), "javascript");
    }

    #[test]
    fn test_analyzer_dispatch_jsx() {
        let analyzer = analyzer_for_path("App.jsx");
        assert_eq!(analyzer.name(), "javascript");
    }

    #[test]
    fn test_analyzer_dispatch_mjs() {
        let analyzer = analyzer_for_path("module.mjs");
        assert_eq!(analyzer.name(), "javascript");
    }

    #[test]
    fn test_analyzer_dispatch_cjs() {
        let analyzer = analyzer_for_path("config.cjs");
        assert_eq!(analyzer.name(), "javascript");
    }

    #[test]
    fn test_ordering_matters_defaults() {
        let analyzer = generic::GenericAnalyzer;
        assert!(!analyzer.ordering_matters(&UnitKind::Import));
        assert!(analyzer.ordering_matters(&UnitKind::Definition));
        assert!(analyzer.ordering_matters(&UnitKind::Statement));
        assert!(!analyzer.ordering_matters(&UnitKind::Whitespace));
    }
}
