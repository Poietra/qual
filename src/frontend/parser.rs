//! Higher-level parse facade over [`crate::source::SourceManager`].
//!
//! Exposes every successfully parsed module together with its dotted module
//! identity derived from the configured source roots (DESIGN §5.3).

use rustpython_parser::ast;

use crate::source::{FileId, SourceFile, SourceManager};

/// Dotted module identity of one source file inside the project tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleIdentity {
    /// Dotted module name, e.g. `my_scenes.base`.
    pub name: String,
    /// Whether the file is a package `__init__.py`.
    pub is_package: bool,
}

/// One successfully parsed module of the project.
#[derive(Debug, Clone, Copy)]
pub struct ParsedModule<'a> {
    /// Handle of the underlying file.
    pub file: FileId,
    /// The decoded source file (spans, slices, tokens).
    pub source: &'a SourceFile,
    /// Parsed module body.
    pub ast: &'a ast::ModModule,
}

/// Returns every parsed file of `sources` in load order.
///
/// Files that failed to decode or parse are skipped; they already carry an
/// `MLC000` diagnostic and take no part in name resolution.
#[must_use]
pub fn parsed_modules(sources: &SourceManager) -> Vec<ParsedModule<'_>> {
    sources
        .files()
        .iter()
        .filter_map(|source| {
            source.ast().map(|ast| ParsedModule {
                file: source.id(),
                source,
                ast,
            })
        })
        .collect()
}

/// Derives the dotted module identity of a project-relative POSIX path.
///
/// Each source root is tried in configuration order; the first root that is a
/// prefix of the path wins. A root of `"."` or `""` matches every path. When
/// no root matches, the whole relative path is used so that every file still
/// has a deterministic identity.
#[must_use]
pub fn module_identity(relative_path: &str, source_roots: &[String]) -> ModuleIdentity {
    let tail = source_roots
        .iter()
        .find_map(|root| strip_root(relative_path, root))
        .unwrap_or(relative_path);
    identity_from_tail(tail)
}

fn strip_root<'a>(relative_path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_matches('/');
    if root.is_empty() || root == "." {
        return Some(relative_path);
    }
    relative_path
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix('/'))
}

fn identity_from_tail(tail: &str) -> ModuleIdentity {
    let stem = tail.strip_suffix(".py").unwrap_or(tail);
    let (stem, is_package) = match stem.strip_suffix("/__init__") {
        Some(package) => (package, true),
        None => (stem, stem == "__init__"),
    };
    let name = if stem.is_empty() || stem == "__init__" {
        // A root-level `__init__.py` has no addressable package name; keep a
        // deterministic placeholder instead of an empty string.
        "__init__".to_owned()
    } else {
        stem.replace('/', ".")
    };
    ModuleIdentity { name, is_package }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(list: &[&str]) -> Vec<String> {
        list.iter().map(|root| (*root).to_owned()).collect()
    }

    #[test]
    fn plain_module_under_dot_root() {
        let identity = module_identity("scenes/demo.py", &roots(&["."]));
        assert_eq!(identity.name, "scenes.demo");
        assert!(!identity.is_package);
    }

    #[test]
    fn package_init_becomes_package_name() {
        let identity = module_identity("my_scenes/__init__.py", &roots(&["."]));
        assert_eq!(identity.name, "my_scenes");
        assert!(identity.is_package);
    }

    #[test]
    fn named_source_root_is_stripped() {
        let identity = module_identity("src/pkg/mod.py", &roots(&["src", "."]));
        assert_eq!(identity.name, "pkg.mod");
    }

    #[test]
    fn first_matching_root_wins() {
        let identity = module_identity("src/pkg/mod.py", &roots(&[".", "src"]));
        assert_eq!(identity.name, "src.pkg.mod");
    }

    #[test]
    fn unmatched_root_falls_back_to_full_path() {
        let identity = module_identity("elsewhere/mod.py", &roots(&["src"]));
        assert_eq!(identity.name, "elsewhere.mod");
    }
}
