//! Direct relative-import extraction, used by incremental mode to hash a
//! test file TOGETHER with the local modules it imports: a change to a
//! helper, fixture, or setup module a test imports must invalidate cached
//! verdicts even though the test file's own bytes are unchanged.
//!
//! One hop of static/dynamic relative imports covers the dominant class
//! (colocated helpers and fixtures). Package-name imports are versioned by
//! the lockfile, which callers hash separately.

use camino::{Utf8Path, Utf8PathBuf};
use oxc_allocator::Allocator;
use oxc_ast::ast::{Argument, Expression};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// Relative import specifiers (`./x`, `../y`) of a module, in source order.
pub fn direct_relative_imports(path: &Utf8Path, source: &str) -> Vec<String> {
    let Ok(source_type) = SourceType::from_path(path.as_std_path()) else {
        return Vec::new();
    };
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    let mut collector = ImportCollector { specifiers: Vec::new() };
    collector.visit_program(&parsed.program);
    collector.specifiers
}

/// Resolve a relative specifier against the importer's directory, trying the
/// TS/JS extension conventions. Returns a path relative to the same root the
/// importer is relative to, or None when nothing exists on disk.
pub fn resolve_relative_import(
    root: &Utf8Path,
    importer: &Utf8Path,
    specifier: &str,
) -> Option<Utf8PathBuf> {
    let base = importer.parent()?;
    let joined = normalize(&base.join(specifier));
    const SUFFIXES: &[&str] = &["", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs"];
    for suffix in SUFFIXES {
        let candidate = Utf8PathBuf::from(format!("{joined}{suffix}"));
        if root.join(&candidate).is_file() {
            return Some(candidate);
        }
    }
    for index in ["index.ts", "index.tsx", "index.js"] {
        let candidate = joined.join(index);
        if root.join(&candidate).is_file() {
            return Some(candidate);
        }
    }
    None
}

fn normalize(path: &Utf8Path) -> Utf8PathBuf {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        match component.as_str() {
            "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.iter().collect()
}

struct ImportCollector {
    specifiers: Vec<String>,
}

impl ImportCollector {
    fn add(&mut self, specifier: &str) {
        if specifier.starts_with("./") || specifier.starts_with("../") {
            self.specifiers.push(specifier.to_string());
        }
    }
}

impl<'a> Visit<'a> for ImportCollector {
    fn visit_import_declaration(&mut self, decl: &oxc_ast::ast::ImportDeclaration<'a>) {
        self.add(decl.source.value.as_str());
    }

    fn visit_export_from_declaration(&mut self, decl: &oxc_ast::ast::ExportFromDeclaration<'a>) {
        self.add(decl.source.value.as_str());
    }

    fn visit_export_all_declaration(&mut self, decl: &oxc_ast::ast::ExportAllDeclaration<'a>) {
        self.add(decl.source.value.as_str());
    }

    fn visit_import_expression(&mut self, expr: &oxc_ast::ast::ImportExpression<'a>) {
        if let Expression::StringLiteral(lit) = &expr.source {
            self.add(lit.value.as_str());
        }
        walk::walk_import_expression(self, expr);
    }

    fn visit_call_expression(&mut self, expr: &oxc_ast::ast::CallExpression<'a>) {
        // require("./x")
        if let Expression::Identifier(ident) = &expr.callee {
            if ident.name == "require" {
                if let Some(Argument::StringLiteral(lit)) = expr.arguments.first() {
                    self.add(lit.value.as_str());
                }
            }
        }
        walk::walk_call_expression(self, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_relative_specifiers_only() {
        let source = r#"
import { a } from "./helper.ts";
import { b } from "../shared/util";
import { c } from "vitest";
export { d } from "./re-export";
export * from "../all";
const e = await import("./dynamic");
const f = require("./cjs");
"#;
        let specs = direct_relative_imports(Utf8Path::new("src/a.test.ts"), source);
        assert_eq!(
            specs,
            vec!["./helper.ts", "../shared/util", "./re-export", "../all", "./dynamic", "./cjs"]
        );
    }

    #[test]
    fn resolves_with_extension_conventions() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src/shared")).unwrap();
        std::fs::write(root.join("src/helper.ts"), "x").unwrap();
        std::fs::write(root.join("src/shared/index.ts"), "x").unwrap();

        let importer = Utf8Path::new("src/a.test.ts");
        assert_eq!(
            resolve_relative_import(root, importer, "./helper").unwrap(),
            Utf8PathBuf::from("src/helper.ts")
        );
        assert_eq!(
            resolve_relative_import(root, importer, "./shared").unwrap(),
            Utf8PathBuf::from("src/shared/index.ts")
        );
        assert!(resolve_relative_import(root, importer, "./missing").is_none());
    }
}
