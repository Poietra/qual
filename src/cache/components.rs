//! Dependency-closed cache shards derived from the semantic dependency graph.
//!
//! The cache owns only weak-component partitioning. Import/call/base/collision
//! discovery belongs to [`SemanticDependencyGraph`], whose directed edges are
//! also consumed by source-change impact analysis (DESIGN §8.4, §9).

use std::collections::{BTreeMap, BTreeSet};

use crate::semantic::dependency::SemanticDependencyGraph;
use crate::source::{FileId, SourceManager};

/// One dependency-closed cache shard, deterministically ordered by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalysisComponent {
    /// Files whose diagnostics and method summaries are cached atomically.
    pub files: BTreeSet<FileId>,
    /// Project-relative paths parallel to `files`, sorted lexicographically.
    pub paths: Vec<String>,
}

/// Builds weak file components from the graph's cache-partition view.
///
/// Files that failed to parse remain nodes and therefore isolated unless a
/// module-name collision connects them to another file.
pub(crate) fn build(
    sources: &SourceManager,
    graph: &SemanticDependencyGraph,
) -> Vec<AnalysisComponent> {
    let mut union = UnionFind::new(sources.files().len());
    for (dependent, dependency) in graph.cache_file_edges() {
        union.join(dependent.index(), dependency.index());
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
        let graph = SemanticDependencyGraph::from_frontend(
            &sources,
            &roots,
            &frontend.index,
            &frontend.calls,
        );
        build(&sources, &graph)
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
