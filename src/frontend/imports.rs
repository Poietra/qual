//! Per-module import model (DESIGN §5.3).
//!
//! Turns `import` / `from ... import` statements into explicit binding
//! targets. Relative imports are resolved against the dotted module tree
//! derived from the configured source roots. Anything that cannot be
//! resolved — a relative import escaping the project tree, a `from x import *`
//! whose source is unknown — is an explicit [`ImportTarget::Unknown`] /
//! unresolved-star marker, never a guess.

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use super::parser::ModuleIdentity;

/// What one imported local name refers to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportTarget {
    /// A module object, e.g. `import manim as mn` binds `mn` to `manim`.
    Module(String),
    /// One symbol of a module, e.g. `from manim import Square as Sq`.
    Symbol {
        /// Absolute dotted module the symbol is imported from.
        module: String,
        /// Name of the symbol inside that module.
        name: String,
    },
    /// The source module could not be resolved (e.g. a relative import that
    /// escapes the project tree). Never treated as any concrete symbol.
    Unknown,
}

/// One local name introduced by an import statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBinding {
    /// Local name bound in the importing module.
    pub name: String,
    /// Resolved target of the binding.
    pub target: ImportTarget,
    /// Byte range of the `alias` clause that created the binding.
    pub range: TextRange,
}

/// Names introduced by a single `from ... import ...` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportedNames {
    /// Explicit names (possibly aliased).
    Bindings(Vec<ImportBinding>),
    /// A star import. `module` is `None` when the source module could not be
    /// resolved; consumers must widen the namespace to Unknown in that case.
    Star {
        /// Absolute dotted source module, when resolvable.
        module: Option<String>,
        /// Byte range of the whole statement.
        range: TextRange,
    },
}

/// Bindings of a plain `import a.b.c [as x]` statement.
///
/// Without an alias, Python binds only the first path segment (`a`); with an
/// alias the full dotted module is bound to the alias name.
#[must_use]
pub fn import_bindings(stmt: &ast::StmtImport) -> Vec<ImportBinding> {
    stmt.names
        .iter()
        .map(|alias| {
            let dotted = alias.name.as_str();
            if let Some(asname) = &alias.asname {
                ImportBinding {
                    name: asname.to_string(),
                    target: ImportTarget::Module(dotted.to_owned()),
                    range: alias.range(),
                }
            } else {
                let first = dotted.split('.').next().unwrap_or(dotted);
                ImportBinding {
                    name: first.to_owned(),
                    target: ImportTarget::Module(first.to_owned()),
                    range: alias.range(),
                }
            }
        })
        .collect()
}

/// Names of a `from ... import ...` statement with relative levels resolved
/// against `current`.
#[must_use]
pub fn import_from_names(stmt: &ast::StmtImportFrom, current: &ModuleIdentity) -> ImportedNames {
    let level = stmt.level.map_or(0, |level| level.to_usize());
    let written = stmt.module.as_ref().map(ast::Identifier::as_str);
    let source = if level == 0 {
        written.map(str::to_owned)
    } else {
        resolve_relative_module(current, level, written)
    };

    if stmt.names.iter().any(|alias| alias.name.as_str() == "*") {
        return ImportedNames::Star {
            module: source,
            range: stmt.range(),
        };
    }

    let bindings = stmt
        .names
        .iter()
        .map(|alias| {
            let local = alias
                .asname
                .as_ref()
                .unwrap_or(&alias.name)
                .as_str()
                .to_owned();
            let target = match &source {
                Some(module) => ImportTarget::Symbol {
                    module: module.clone(),
                    name: alias.name.as_str().to_owned(),
                },
                None => ImportTarget::Unknown,
            };
            ImportBinding {
                name: local,
                target,
                range: alias.range(),
            }
        })
        .collect();
    ImportedNames::Bindings(bindings)
}

/// Resolves a relative import (`level >= 1`) against the current module.
///
/// Returns `None` when the import escapes the top of the project module tree;
/// callers must record the result as Unknown, not guess.
#[must_use]
pub fn resolve_relative_module(
    current: &ModuleIdentity,
    level: usize,
    tail: Option<&str>,
) -> Option<String> {
    let mut components: Vec<&str> = current.name.split('.').collect();
    // Level 1 means "the current package": for a plain module that is its
    // parent package, for a package `__init__` it is the package itself.
    if !current.is_package {
        components.pop();
    }
    for _ in 1..level {
        components.pop()?;
    }
    if components.is_empty() {
        return None;
    }
    let base = components.join(".");
    match tail {
        Some(tail) => Some(format!("{base}.{tail}")),
        None => Some(base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::{Mode, ast::Mod, parse};

    fn body(source: &str) -> Vec<ast::Stmt> {
        match parse(source, Mode::Module, "<test>").expect("parse") {
            Mod::Module(module) => module.body,
            _ => unreachable!(),
        }
    }

    fn module(name: &str, is_package: bool) -> ModuleIdentity {
        ModuleIdentity {
            name: name.to_owned(),
            is_package,
        }
    }

    #[test]
    fn plain_import_binds_first_segment() {
        let stmts = body("import manim.utils.color\n");
        let ast::Stmt::Import(import) = &stmts[0] else {
            panic!("expected import");
        };
        let bindings = import_bindings(import);
        assert_eq!(bindings[0].name, "manim");
        assert_eq!(bindings[0].target, ImportTarget::Module("manim".to_owned()));
    }

    #[test]
    fn aliased_import_binds_full_module() {
        let stmts = body("import manim as mn\n");
        let ast::Stmt::Import(import) = &stmts[0] else {
            panic!("expected import");
        };
        let bindings = import_bindings(import);
        assert_eq!(bindings[0].name, "mn");
        assert_eq!(bindings[0].target, ImportTarget::Module("manim".to_owned()));
    }

    #[test]
    fn relative_import_resolves_against_package() {
        let current = module("my_scenes.scenes", false);
        assert_eq!(
            resolve_relative_module(&current, 1, Some("base")),
            Some("my_scenes.base".to_owned())
        );
        assert_eq!(
            resolve_relative_module(&current, 1, None),
            Some("my_scenes".to_owned())
        );
        assert_eq!(resolve_relative_module(&current, 2, Some("base")), None);
    }

    #[test]
    fn relative_import_from_package_init_stays_in_package() {
        let current = module("my_scenes", true);
        assert_eq!(
            resolve_relative_module(&current, 1, Some("base")),
            Some("my_scenes.base".to_owned())
        );
    }

    #[test]
    fn unresolvable_relative_import_is_unknown() {
        let stmts = body("from ..outside import Thing\n");
        let ast::Stmt::ImportFrom(import) = &stmts[0] else {
            panic!("expected import-from");
        };
        let names = import_from_names(import, &module("top", false));
        let ImportedNames::Bindings(bindings) = names else {
            panic!("expected bindings");
        };
        assert_eq!(bindings[0].target, ImportTarget::Unknown);
    }

    #[test]
    fn star_import_is_reported_as_star() {
        let stmts = body("from manim import *\n");
        let ast::Stmt::ImportFrom(import) = &stmts[0] else {
            panic!("expected import-from");
        };
        let names = import_from_names(import, &module("scene", false));
        assert!(matches!(
            names,
            ImportedNames::Star {
                module: Some(module),
                ..
            } if module == "manim"
        ));
    }
}
