//! Conservative project dependency components for incremental cache shards.
//!
//! Diagnostics produced while interpreting one Scene may be anchored in a
//! helper's source file. Sharding by primary path alone would therefore be
//! unsound. We instead join every file connected by a statically resolved
//! project import, qualified call, base class, or module-name collision and
//! cache the resulting weakly connected component atomically (DESIGN §9).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast;

use crate::frontend::imports::{ImportTarget, ImportedNames, import_from_names};
use crate::frontend::index::{BaseRef, ProjectIndex, QualifiedCallFacts};
use crate::frontend::names::Binding;
use crate::frontend::parser::{ModuleIdentity, module_identity};
use crate::source::{FileId, SourceManager};

/// One dependency-closed cache shard, deterministically ordered by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisComponent {
    /// Files whose diagnostics and method summaries are cached atomically.
    pub files: BTreeSet<FileId>,
    /// Project-relative paths parallel to `files`, sorted lexicographically.
    pub paths: Vec<String>,
}

/// Builds weak dependency components over every source file, including files
/// that failed to parse (those remain isolated unless they collide with a
/// parsed module identity).
pub(crate) fn build(
    sources: &SourceManager,
    source_roots: &[String],
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
) -> Vec<AnalysisComponent> {
    let mut union = UnionFind::new(sources.files().len());
    let identities: BTreeMap<FileId, ModuleIdentity> = sources
        .files()
        .iter()
        .map(|file| {
            (
                file.id(),
                module_identity(file.relative_path(), source_roots),
            )
        })
        .collect();

    // Colliding module names share one resolver namespace: the first file
    // wins, but changing either file or the sorted project layout can affect
    // which facts are visible.
    let mut first_by_module: BTreeMap<&str, FileId> = BTreeMap::new();
    for (file, identity) in &identities {
        if let Some(first) = first_by_module.get(identity.name.as_str()) {
            union.join(file.index(), first.index());
        } else {
            first_by_module.insert(identity.name.as_str(), *file);
        }
    }

    let owner_by_module: BTreeMap<&str, FileId> = index
        .modules
        .iter()
        .map(|(name, record)| (name.as_str(), record.file))
        .collect();

    // Every syntactic import is collected recursively, including imports in
    // functions, classes, and branches. Only project modules create edges;
    // external modules cannot make one project shard depend on another.
    for file in sources.files() {
        let (Some(module), Some(identity)) = (file.ast(), identities.get(&file.id())) else {
            continue;
        };
        let mut targets = BTreeSet::new();
        collect_import_targets(&module.body, identity, &mut targets);
        for target in targets {
            connect_module_prefixes(file.id(), &target, &owner_by_module, &mut union);
        }
    }

    // Final namespace bindings capture re-export chains and module aliases
    // that the fixpoint resolver made concrete.
    for record in index.modules.values() {
        for binding in record.namespace.values() {
            let target = match binding {
                Binding::ImportedModule(target)
                | Binding::ImportedSymbol(target)
                | Binding::LocalClass(target)
                | Binding::LocalFunction(target) => Some(target.as_str()),
                Binding::LocalVar(_) | Binding::Unknown => None,
            };
            if let Some(target) = target {
                connect_qualified(record.file, target, index, &mut union);
            }
        }
    }

    // Resolved project bases affect MRO, Scene discovery, and every summary
    // inherited through that chain.
    for class in index.classes.values() {
        for base in &class.bases {
            if let BaseRef::Resolved(target) = base {
                connect_qualified(class.file, target, index, &mut union);
            }
        }
    }

    // Qualified calls include alias.attr chains such as `import pkg;
    // pkg.helpers.run()`, for which the plain import target alone may be an
    // implied package without its own source file.
    for call in &calls.calls {
        for candidate in &call.candidates {
            connect_qualified(call.file, candidate, index, &mut union);
        }
    }

    let mut grouped: BTreeMap<usize, Vec<FileId>> = BTreeMap::new();
    for file in sources.files() {
        grouped
            .entry(union.root(file.id().index()))
            .or_default()
            .push(file.id());
    }
    let mut components: Vec<AnalysisComponent> = grouped
        .into_values()
        .map(|mut files| {
            files.sort_by(|left, right| {
                sources
                    .file(*left)
                    .relative_path()
                    .cmp(sources.file(*right).relative_path())
            });
            let paths = files
                .iter()
                .map(|file| sources.file(*file).relative_path().to_owned())
                .collect();
            AnalysisComponent {
                files: files.into_iter().collect(),
                paths,
            }
        })
        .collect();
    components.sort_by(|left, right| left.paths.cmp(&right.paths));
    components
}

fn connect_qualified(source: FileId, qualified: &str, index: &ProjectIndex, union: &mut UnionFind) {
    let owner = index
        .modules
        .iter()
        .filter(|(module, _)| {
            qualified == module.as_str()
                || qualified
                    .strip_prefix(module.as_str())
                    .is_some_and(|tail| tail.starts_with('.'))
        })
        .max_by_key(|(module, _)| module.len())
        .map(|(_, record)| record.file);
    if let Some(owner) = owner {
        union.join(source.index(), owner.index());
    }
}

fn connect_module_prefixes(
    source: FileId,
    target: &str,
    owner_by_module: &BTreeMap<&str, FileId>,
    union: &mut UnionFind,
) {
    let mut end = target.len();
    loop {
        let prefix = &target[..end];
        if let Some(owner) = owner_by_module.get(prefix) {
            union.join(source.index(), owner.index());
        }
        let Some(dot) = prefix.rfind('.') else {
            break;
        };
        end = dot;
    }
}

fn collect_import_targets(
    statements: &[ast::Stmt],
    identity: &ModuleIdentity,
    targets: &mut BTreeSet<String>,
) {
    for statement in statements {
        match statement {
            ast::Stmt::Import(import) => {
                targets.extend(import.names.iter().map(|alias| alias.name.to_string()));
            }
            ast::Stmt::ImportFrom(import) => match import_from_names(import, identity) {
                ImportedNames::Bindings(bindings) => {
                    for binding in bindings {
                        match binding.target {
                            ImportTarget::Module(module) => {
                                targets.insert(module);
                            }
                            ImportTarget::Symbol { module, name } => {
                                targets.insert(format!("{module}.{name}"));
                                targets.insert(module);
                            }
                            ImportTarget::Unknown => {}
                        }
                    }
                }
                ImportedNames::Star {
                    module: Some(module),
                    ..
                } => {
                    targets.insert(module);
                }
                ImportedNames::Star { module: None, .. } => {}
            },
            ast::Stmt::FunctionDef(def) => {
                collect_import_targets(&def.body, identity, targets);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                collect_import_targets(&def.body, identity, targets);
            }
            ast::Stmt::ClassDef(def) => {
                collect_import_targets(&def.body, identity, targets);
            }
            ast::Stmt::For(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
            }
            ast::Stmt::AsyncFor(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
            }
            ast::Stmt::While(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
            }
            ast::Stmt::If(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
            }
            ast::Stmt::With(inner) => {
                collect_import_targets(&inner.body, identity, targets);
            }
            ast::Stmt::AsyncWith(inner) => {
                collect_import_targets(&inner.body, identity, targets);
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    collect_import_targets(&case.body, identity, targets);
                }
            }
            ast::Stmt::Try(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
                collect_import_targets(&inner.finalbody, identity, targets);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_import_targets(&handler.body, identity, targets);
                }
            }
            ast::Stmt::TryStar(inner) => {
                collect_import_targets(&inner.body, identity, targets);
                collect_import_targets(&inner.orelse, identity, targets);
                collect_import_targets(&inner.finalbody, identity, targets);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_import_targets(&handler.body, identity, targets);
                }
            }
            ast::Stmt::Return(_)
            | ast::Stmt::Delete(_)
            | ast::Stmt::Assign(_)
            | ast::Stmt::TypeAlias(_)
            | ast::Stmt::AugAssign(_)
            | ast::Stmt::AnnAssign(_)
            | ast::Stmt::Raise(_)
            | ast::Stmt::Assert(_)
            | ast::Stmt::Global(_)
            | ast::Stmt::Nonlocal(_)
            | ast::Stmt::Expr(_)
            | ast::Stmt::Pass(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
        }
    }
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn root(&mut self, node: usize) -> usize {
        let parent = self.parent[node];
        if parent == node {
            node
        } else {
            let root = self.root(parent);
            self.parent[node] = root;
            root
        }
    }

    fn join(&mut self, left: usize, right: usize) {
        let left = self.root(left);
        let right = self.root(right);
        if left != right {
            let (first, second) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            self.parent[second] = first;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::frontend::{ManimSurface, index};

    fn components(files: &[(&str, &str)], roots: &[&str]) -> Vec<Vec<String>> {
        let mut sources = SourceManager::new("/project");
        for (path, source) in files {
            sources.load_bytes(&Path::new("/project").join(path), source.as_bytes());
        }
        let roots: Vec<String> = roots.iter().map(|root| (*root).to_owned()).collect();
        let frontend = index::analyze(&sources, &roots, &ManimSurface::default());
        build(&sources, &roots, &frontend.index, &frontend.calls)
            .into_iter()
            .map(|component| component.paths)
            .collect()
    }

    #[test]
    fn nested_project_import_joins_only_its_dependency() {
        let actual = components(
            &[
                (
                    "a.py",
                    "def run():\n    from b import helper\n    helper()\n",
                ),
                ("b.py", "def helper():\n    pass\n"),
                ("c.py", "value = 1\n"),
            ],
            &["."],
        );
        assert_eq!(
            actual,
            vec![
                vec!["a.py".to_owned(), "b.py".to_owned()],
                vec!["c.py".to_owned()]
            ]
        );
    }

    #[test]
    fn qualified_call_through_implied_package_joins_the_real_module() {
        let actual = components(
            &[
                ("scene.py", "import pkg\npkg.helpers.run()\n"),
                ("pkg/helpers.py", "def run():\n    pass\n"),
                ("other.py", "value = 1\n"),
            ],
            &["."],
        );
        assert_eq!(
            actual,
            vec![
                vec!["other.py".to_owned()],
                vec!["pkg/helpers.py".to_owned(), "scene.py".to_owned()]
            ]
        );
    }

    #[test]
    fn colliding_module_identities_share_one_shard() {
        let actual = components(
            &[
                ("src/pkg/mod.py", "value = 1\n"),
                ("lib/pkg/mod.py", "value = 2\n"),
            ],
            &["src", "lib"],
        );
        assert_eq!(
            actual,
            vec![vec![
                "lib/pkg/mod.py".to_owned(),
                "src/pkg/mod.py".to_owned()
            ]]
        );
    }
}
