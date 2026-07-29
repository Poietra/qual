//! `ChangeImpact` v0 projection over two immutable semantic snapshots.
//!
//! The comparison is deliberately conservative. Changed definitions and
//! files seed reverse traversal in both the base and target dependency
//! graphs, so deleted and renamed relationships remain visible from the base
//! graph while newly introduced relationships appear in the target graph.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::semantic::dependency::{
    DefinitionKind, DefinitionNode, DependencyEdge, DependencyNode, ReverseDependencyPath,
    SemanticDependencyGraph,
};
use crate::semantic::values::AllocationSite;
use crate::source::{FileId, SourceManager};
use crate::static_facts::{StaticFactsOutput, project_source_anchor};

const DOMAIN: &str = "change-impact-v0";

/// One fully analyzed source snapshot consumed by `ChangeImpact`.
pub struct SnapshotInput<'a> {
    /// Decoded and parsed source snapshot.
    pub sources: &'a SourceManager,
    /// Immutable raw bytes parallel to `sources.files()`.
    pub raw_sources: &'a [Vec<u8>],
    /// Semantic dependency graph enriched with lifecycle facts.
    pub graph: &'a SemanticDependencyGraph,
    /// `StaticFacts` projection for public IDs and snapshot metadata.
    pub static_facts: &'a StaticFactsOutput,
}

/// One completed `ChangeImpact` document and its canonical JSON.
#[derive(Debug)]
pub struct ChangeImpactOutput {
    /// Parsed public document.
    pub document: Value,
    /// Sorted pretty JSON with one trailing newline.
    pub json: String,
}

/// Compares base and target source snapshots and projects conservative impact
/// candidates through public `StaticFacts` IDs.
#[must_use]
pub fn compare(base: SnapshotInput<'_>, target: SnapshotInput<'_>) -> ChangeImpactOutput {
    assert_snapshot_alignment(&base);
    assert_snapshot_alignment(&target);
    let mut projector = ImpactProjector::new(base, target);
    let document = projector.build();
    let mut json = serde_json::to_string_pretty(&document)
        .expect("ChangeImpact projection contains only finite JSON values");
    json.push('\n');
    ChangeImpactOutput { document, json }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SnapshotSide {
    Base,
    Target,
}

impl SnapshotSide {
    const fn label(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Target => "target",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKind {
    Added,
    Removed,
    Modified,
}

impl ChangeKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
        }
    }
}

struct ImpactProjector<'a> {
    base: SnapshotInput<'a>,
    target: SnapshotInput<'a>,
    changed_files: Vec<Value>,
    changed_definitions: Vec<Value>,
    base_origins: BTreeSet<DependencyNode>,
    target_origins: BTreeSet<DependencyNode>,
    global_semantics_changed: bool,
}

impl<'a> ImpactProjector<'a> {
    fn new(base: SnapshotInput<'a>, target: SnapshotInput<'a>) -> Self {
        let (changed_files, mut base_origins, mut target_origins) = changed_files(&base, &target);
        let (changed_definitions, base_definition_origins, target_definition_origins) =
            changed_definitions(&base, &target);
        base_origins.extend(base_definition_origins);
        target_origins.extend(target_definition_origins);
        let global_semantics_changed = snapshot_semantics(&base) != snapshot_semantics(&target);
        if global_semantics_changed {
            seed_all_source_semantics(base.graph, &mut base_origins);
            seed_all_source_semantics(target.graph, &mut target_origins);
        }
        Self {
            base,
            target,
            changed_files,
            changed_definitions,
            base_origins,
            target_origins,
            global_semantics_changed,
        }
    }

    fn build(&mut self) -> Value {
        let mut affected_scenes = BTreeMap::new();
        let mut affected_plays = BTreeMap::new();
        let mut affected_objects = BTreeMap::new();
        let mut reason_paths = Vec::new();
        let mut frontiers = Vec::new();

        self.project_side(
            SnapshotSide::Base,
            &self.base_origins,
            &mut affected_scenes,
            &mut affected_plays,
            &mut affected_objects,
            &mut reason_paths,
            &mut frontiers,
        );
        self.project_side(
            SnapshotSide::Target,
            &self.target_origins,
            &mut affected_scenes,
            &mut affected_plays,
            &mut affected_objects,
            &mut reason_paths,
            &mut frontiers,
        );
        if self.global_semantics_changed {
            frontiers.push(global_frontier(SnapshotSide::Base, &self.base));
            frontiers.push(global_frontier(SnapshotSide::Target, &self.target));
        }
        sort_dedup_values(&mut reason_paths);
        sort_dedup_values(&mut frontiers);
        let completeness = if frontiers.is_empty() {
            "complete"
        } else {
            "candidates"
        };
        json!({
            "schema_version": 0,
            "tool": {
                "name": "qual",
                "version": crate::VERSION,
                "semantic_build_hash": self.target.static_facts.document["tool"]["semantic_build_hash"],
            },
            "base_snapshot": snapshot_reference(&self.base),
            "target_snapshot": snapshot_reference(&self.target),
            "changed_files": self.changed_files,
            "changed_definitions": self.changed_definitions,
            "affected_scenes": affected_scenes.into_values().collect::<Vec<_>>(),
            "affected_plays": affected_plays.into_values().collect::<Vec<_>>(),
            "affected_objects": affected_objects.into_values().collect::<Vec<_>>(),
            "reason_paths": reason_paths,
            "unknown_frontiers": frontiers,
            "completeness": completeness,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the three typed candidate collections prevent entity kinds from being mixed"
    )]
    fn project_side(
        &self,
        side: SnapshotSide,
        origins: &BTreeSet<DependencyNode>,
        affected_scenes: &mut BTreeMap<(SnapshotSide, String), Value>,
        affected_plays: &mut BTreeMap<(SnapshotSide, String), Value>,
        affected_objects: &mut BTreeMap<(SnapshotSide, String), Value>,
        reason_paths: &mut Vec<Value>,
        frontiers: &mut Vec<Value>,
    ) {
        let snapshot = self.snapshot(side);
        let paths = snapshot.graph.reverse_paths(origins);
        let mut reached = origins.clone();
        for path in &paths {
            reached.insert(path.affected.clone());
            let Some((kind, id, candidate)) = affected_candidate(snapshot, side, &path.affected)
            else {
                continue;
            };
            let key = (side, id.clone());
            match kind {
                "scene" => {
                    affected_scenes.insert(key, candidate);
                }
                "play" => {
                    affected_plays.insert(key, candidate);
                }
                "object" => {
                    affected_objects.insert(key, candidate);
                }
                _ => unreachable!("affected_candidate returns only public entity kinds"),
            }
            if !path.edges.is_empty() {
                reason_paths.push(Self::reason_path(snapshot, side, path, kind, &id));
            }
        }
        for unknown in snapshot.graph.unknowns() {
            if reached.contains(&unknown.dependent) {
                frontiers.push(json!({
                    "snapshot": side.label(),
                    "dependent": node_reference(snapshot, &unknown.dependent),
                    "reasons": [{
                        "kind": unknown.kind.label(),
                        "anchor": anchor(snapshot, unknown.anchor),
                    }],
                }));
            }
        }
        for origin in origins {
            let DependencyNode::File(file) = origin else {
                continue;
            };
            let source = snapshot.sources.file(*file);
            if !source.is_parsed() {
                frontiers.push(json!({
                    "snapshot": side.label(),
                    "dependent": node_reference(snapshot, origin),
                    "reasons": [{
                        "kind": if source.encoding().label == "unknown" { "decode-error" } else { "parse-error" },
                        "anchor": anchor(snapshot, AllocationSite { file: *file, start: 0, end: 0 }),
                    }],
                }));
            }
        }
    }

    fn reason_path(
        snapshot: &SnapshotInput<'_>,
        side: SnapshotSide,
        path: &ReverseDependencyPath,
        kind: &str,
        id: &str,
    ) -> Value {
        let steps: Vec<Value> = path
            .edges
            .iter()
            .map(|edge| reason_step(snapshot, edge))
            .collect();
        json!({
            "snapshot": side.label(),
            "origin": node_reference(snapshot, &path.origin),
            "affected": { "kind": kind, "id": id },
            "steps": steps,
        })
    }

    const fn snapshot(&self, side: SnapshotSide) -> &SnapshotInput<'a> {
        match side {
            SnapshotSide::Base => &self.base,
            SnapshotSide::Target => &self.target,
        }
    }
}

fn assert_snapshot_alignment(snapshot: &SnapshotInput<'_>) {
    assert_eq!(
        snapshot.sources.files().len(),
        snapshot.raw_sources.len(),
        "raw source snapshot must align with SourceManager"
    );
}

fn changed_files(
    base: &SnapshotInput<'_>,
    target: &SnapshotInput<'_>,
) -> (
    Vec<Value>,
    BTreeSet<DependencyNode>,
    BTreeSet<DependencyNode>,
) {
    let base_files = file_snapshot(base);
    let target_files = file_snapshot(target);
    let paths: BTreeSet<&str> = base_files
        .keys()
        .chain(target_files.keys())
        .map(String::as_str)
        .collect();
    let mut changes = Vec::new();
    let mut base_origins = BTreeSet::new();
    let mut target_origins = BTreeSet::new();
    for path in paths {
        let base_file = base_files.get(path);
        let target_file = target_files.get(path);
        let kind = match (base_file, target_file) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            (Some((_, base_hash)), Some((_, target_hash))) if base_hash != target_hash => {
                ChangeKind::Modified
            }
            _ => continue,
        };
        if let Some((file, _)) = base_file {
            base_origins.insert(DependencyNode::File(*file));
        }
        if let Some((file, _)) = target_file {
            target_origins.insert(DependencyNode::File(*file));
        }
        let mut change = Map::new();
        change.insert("path".to_owned(), Value::String(path.to_owned()));
        change.insert("change".to_owned(), Value::String(kind.label().to_owned()));
        if let Some((_, hash)) = base_file {
            change.insert(
                "base_raw_content_hash".to_owned(),
                Value::String(hash.clone()),
            );
        }
        if let Some((_, hash)) = target_file {
            change.insert(
                "target_raw_content_hash".to_owned(),
                Value::String(hash.clone()),
            );
        }
        changes.push(Value::Object(change));
    }
    (changes, base_origins, target_origins)
}

fn changed_definitions(
    base: &SnapshotInput<'_>,
    target: &SnapshotInput<'_>,
) -> (
    Vec<Value>,
    BTreeSet<DependencyNode>,
    BTreeSet<DependencyNode>,
) {
    let base_definitions = definitions(base.graph);
    let target_definitions = definitions(target.graph);
    let names: BTreeSet<&str> = base_definitions
        .keys()
        .chain(target_definitions.keys())
        .map(String::as_str)
        .collect();
    let mut changes = Vec::new();
    let mut base_origins = BTreeSet::new();
    let mut target_origins = BTreeSet::new();
    for name in names {
        let base_definition = base_definitions.get(name);
        let target_definition = target_definitions.get(name);
        let kind = match (base_definition, target_definition) {
            (None, Some(_)) => ChangeKind::Added,
            (Some(_), None) => ChangeKind::Removed,
            (Some(base_definition), Some(target_definition))
                if definition_fingerprint(base, base_definition)
                    != definition_fingerprint(target, target_definition) =>
            {
                ChangeKind::Modified
            }
            _ => continue,
        };
        if let Some(definition) = base_definition {
            base_origins.insert(DependencyNode::Definition((*definition).clone()));
        }
        if let Some(definition) = target_definition {
            target_origins.insert(DependencyNode::Definition((*definition).clone()));
        }
        let definition_kind = target_definition
            .or(base_definition)
            .map_or("callable", |definition| {
                definition_kind_name(definition.kind)
            });
        let mut change = Map::new();
        change.insert("qualified_name".to_owned(), Value::String(name.to_owned()));
        change.insert("kind".to_owned(), Value::String(definition_kind.to_owned()));
        change.insert("change".to_owned(), Value::String(kind.label().to_owned()));
        if let Some(definition) = base_definition {
            change.insert("base_anchor".to_owned(), anchor(base, definition.site));
        }
        if let Some(definition) = target_definition {
            change.insert("target_anchor".to_owned(), anchor(target, definition.site));
        }
        changes.push(Value::Object(change));
    }
    (changes, base_origins, target_origins)
}

fn file_snapshot(snapshot: &SnapshotInput<'_>) -> BTreeMap<String, (FileId, String)> {
    snapshot
        .sources
        .files()
        .iter()
        .map(|source| {
            (
                source.relative_path().to_owned(),
                (
                    source.id(),
                    sha256(&snapshot.raw_sources[source.id().index()]),
                ),
            )
        })
        .collect()
}

fn definitions(graph: &SemanticDependencyGraph) -> BTreeMap<String, &DefinitionNode> {
    graph
        .nodes()
        .filter_map(|node| match node {
            DependencyNode::Definition(definition) => {
                Some((definition.qualified_name.clone(), definition))
            }
            _ => None,
        })
        .collect()
}

fn definition_fingerprint(snapshot: &SnapshotInput<'_>, definition: &DefinitionNode) -> String {
    let source = snapshot.sources.file(definition.site.file);
    let start = usize::try_from(definition.site.start)
        .unwrap_or(usize::MAX)
        .min(source.text().len());
    let end = usize::try_from(definition.site.end)
        .unwrap_or(usize::MAX)
        .min(source.text().len())
        .max(start);
    let kind = definition_kind_name(definition.kind);
    sha256(
        &serde_json::to_vec(&json!([
            DOMAIN,
            "definition",
            kind,
            source.relative_path(),
            &source.text()[start..end],
        ]))
        .expect("definition fingerprint serializes"),
    )
}

const fn definition_kind_name(kind: DefinitionKind) -> &'static str {
    match kind {
        DefinitionKind::Class => "class",
        DefinitionKind::Callable => "callable",
    }
}

fn seed_all_source_semantics(
    graph: &SemanticDependencyGraph,
    origins: &mut BTreeSet<DependencyNode>,
) {
    origins.extend(
        graph
            .nodes()
            .filter(|node| {
                matches!(
                    node,
                    DependencyNode::File(_) | DependencyNode::Definition(_)
                )
            })
            .cloned(),
    );
}

fn snapshot_semantics(snapshot: &SnapshotInput<'_>) -> Value {
    json!([
        snapshot.static_facts.document["snapshot"]["semantic_config_hash"],
        snapshot.static_facts.document["knowledge_profile"],
    ])
}

fn snapshot_reference(snapshot: &SnapshotInput<'_>) -> Value {
    json!({
        "id": snapshot.static_facts.document["snapshot"]["id"],
        "source_manifest_hash": snapshot.static_facts.document["snapshot"]["source_manifest_hash"],
        "semantic_config_hash": snapshot.static_facts.document["snapshot"]["semantic_config_hash"],
        "knowledge_profile": snapshot.static_facts.document["knowledge_profile"],
    })
}

fn affected_candidate(
    snapshot: &SnapshotInput<'_>,
    side: SnapshotSide,
    node: &DependencyNode,
) -> Option<(&'static str, String, Value)> {
    let id = snapshot.static_facts.index.id_for_node(node)?;
    let (kind, array, anchor_field) = match node {
        DependencyNode::Scene(_) => ("scene", "scenes", "definition_anchor"),
        DependencyNode::Play(_) => ("play", "plays", "call_anchor"),
        DependencyNode::Object(_) => ("object", "objects", "allocation_anchor"),
        DependencyNode::File(_) | DependencyNode::Definition(_) => return None,
    };
    let record = find_record(&snapshot.static_facts.document[array], id)?;
    let mut candidate = Map::new();
    candidate.insert(
        "snapshot".to_owned(),
        Value::String(side.label().to_owned()),
    );
    candidate.insert("id".to_owned(), Value::String(id.to_owned()));
    if let Some(scene_id) = record.get("scene_id") {
        candidate.insert("scene_id".to_owned(), scene_id.clone());
    }
    if let Some(qualified_name) = record.get("qualified_name") {
        candidate.insert("qualified_name".to_owned(), qualified_name.clone());
    }
    candidate.insert("anchor".to_owned(), record[anchor_field].clone());
    Some((kind, id.to_owned(), Value::Object(candidate)))
}

fn find_record<'a>(records: &'a Value, id: &str) -> Option<&'a Map<String, Value>> {
    records.as_array()?.iter().find_map(|record| {
        (record["id"].as_str() == Some(id))
            .then(|| record.as_object())
            .flatten()
    })
}

fn node_reference(snapshot: &SnapshotInput<'_>, node: &DependencyNode) -> Value {
    match node {
        DependencyNode::File(file) => json!({
            "kind": "file",
            "path": snapshot.sources.file(*file).relative_path(),
        }),
        DependencyNode::Definition(definition) => json!({
            "kind": "definition",
            "qualified_name": definition.qualified_name,
            "anchor": anchor(snapshot, definition.site),
        }),
        DependencyNode::Scene(_) | DependencyNode::Play(_) | DependencyNode::Object(_) => {
            let kind = match node {
                DependencyNode::Scene(_) => "scene",
                DependencyNode::Play(_) => "play",
                DependencyNode::Object(_) => "object",
                _ => unreachable!(),
            };
            snapshot.static_facts.index.id_for_node(node).map_or_else(
                || json!({ "kind": kind }),
                |id| json!({ "kind": kind, "id": id }),
            )
        }
    }
}

fn reason_step(snapshot: &SnapshotInput<'_>, edge: &DependencyEdge) -> Value {
    let mut step = Map::new();
    step.insert(
        "kind".to_owned(),
        Value::String(edge.reason.label().to_owned()),
    );
    if let Some(site) = edge.anchor {
        step.insert("anchor".to_owned(), anchor(snapshot, site));
    }
    Value::Object(step)
}

fn global_frontier(side: SnapshotSide, snapshot: &SnapshotInput<'_>) -> Value {
    json!({
        "snapshot": side.label(),
        "dependent": { "kind": "snapshot" },
        "reasons": [{
            "kind": "semantic-config-changed",
            "detail": format!(
                "{} snapshot uses semantic config/profile {}",
                side.label(),
                snapshot.static_facts.document["snapshot"]["semantic_config_hash"].as_str().unwrap_or("unknown"),
            ),
        }],
    })
}

fn anchor(snapshot: &SnapshotInput<'_>, site: AllocationSite) -> Value {
    project_source_anchor(snapshot.sources, snapshot.raw_sources, site)
}

fn sort_dedup_values(values: &mut Vec<Value>) {
    values.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
    values.dedup();
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::application::manim_surface;
    use crate::config::loader::{self, ResolutionInput};
    use crate::frontend::index;
    use crate::knowledge;
    use crate::semantic::interpreter;
    use crate::static_facts::{ProjectionInput, project};

    struct OwnedSnapshot {
        sources: SourceManager,
        raw: Vec<Vec<u8>>,
        graph: SemanticDependencyGraph,
        facts: StaticFactsOutput,
    }

    impl OwnedSnapshot {
        fn input(&self) -> SnapshotInput<'_> {
            SnapshotInput {
                sources: &self.sources,
                raw_sources: &self.raw,
                graph: &self.graph,
                static_facts: &self.facts,
            }
        }
    }

    fn snapshot(files: &[(&str, &str)]) -> OwnedSnapshot {
        let mut sources = SourceManager::new("/project");
        let mut raw = Vec::new();
        for (path, source) in files {
            raw.push(source.as_bytes().to_vec());
            sources.load_bytes(&Path::new("/project").join(path), source.as_bytes());
        }
        let profile = knowledge::load("upstream_0_20").unwrap();
        let config = loader::resolve(&ResolutionInput {
            project_root: "/project".into(),
            ..ResolutionInput::default()
        })
        .unwrap();
        let frontend = index::analyze(&sources, &[".".to_owned()], &manim_surface(&profile));
        let lifecycle =
            interpreter::analyze(&sources, &frontend.index, &frontend.calls, Some(&profile));
        let mut graph = SemanticDependencyGraph::from_frontend(
            &sources,
            &[".".to_owned()],
            &frontend.index,
            &frontend.calls,
        );
        graph.attach_lifecycle(&lifecycle, &sources, &frontend.index);
        let facts = project(ProjectionInput {
            sources: &sources,
            raw_sources: &raw,
            config: &config,
            knowledge: &profile,
            index: &frontend.index,
            calls: &frontend.calls,
            lifecycle: &lifecycle,
        });
        OwnedSnapshot {
            sources,
            raw,
            graph,
            facts,
        }
    }

    #[test]
    fn removed_helper_uses_the_base_graph_to_reach_every_caller_scene() {
        let base = snapshot(&[
            ("helper.py", "def move():\n    return 1\n"),
            (
                "a.py",
                "from manim import Scene\nfrom helper import move\nclass A(Scene):\n    def construct(self):\n        move()\n",
            ),
            (
                "b.py",
                "from manim import Scene\nfrom helper import move\nclass B(Scene):\n    def construct(self):\n        move()\n",
            ),
        ]);
        let target = snapshot(&[
            ("helper.py", "value = 1\n"),
            (
                "a.py",
                "from manim import Scene\nfrom helper import move\nclass A(Scene):\n    def construct(self):\n        move()\n",
            ),
            (
                "b.py",
                "from manim import Scene\nfrom helper import move\nclass B(Scene):\n    def construct(self):\n        move()\n",
            ),
        ]);
        let output = compare(base.input(), target.input());
        let removed = output.document["changed_definitions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|definition| {
                definition["qualified_name"] == "helper.move" && definition["change"] == "removed"
            });
        assert!(removed);
        let base_scenes: BTreeSet<&str> = output.document["affected_scenes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|scene| scene["snapshot"] == "base")
            .filter_map(|scene| scene["qualified_name"].as_str())
            .collect();
        assert_eq!(base_scenes, BTreeSet::from(["a.A", "b.B"]));
    }

    #[test]
    fn dynamic_frontier_makes_completeness_candidates() {
        let base = snapshot(&[("scene.py", "def run(fn):\n    fn()\n")]);
        let target = snapshot(&[("scene.py", "def run(fn):\n    return fn()\n")]);
        let output = compare(base.input(), target.input());
        assert_eq!(output.document["completeness"], "candidates");
        assert!(
            output.document["unknown_frontiers"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|frontier| frontier["reasons"].as_array().unwrap())
                .any(|reason| reason["kind"] == "dynamic-call-target")
        );
    }

    #[test]
    fn renamed_helper_is_removed_in_base_and_added_in_target() {
        let base = snapshot(&[
            ("helper.py", "def move():\n    return 1\n"),
            (
                "scene.py",
                "from manim import Scene\nfrom helper import move\nclass Demo(Scene):\n    def construct(self):\n        move()\n",
            ),
        ]);
        let target = snapshot(&[
            ("helper.py", "def shift():\n    return 1\n"),
            (
                "scene.py",
                "from manim import Scene\nfrom helper import shift\nclass Demo(Scene):\n    def construct(self):\n        shift()\n",
            ),
        ]);
        let output = compare(base.input(), target.input());
        let definitions = output.document["changed_definitions"].as_array().unwrap();
        assert!(definitions.iter().any(|definition| {
            definition["qualified_name"] == "helper.move" && definition["change"] == "removed"
        }));
        assert!(definitions.iter().any(|definition| {
            definition["qualified_name"] == "helper.shift" && definition["change"] == "added"
        }));
        assert!(output.document["affected_scenes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|scene| scene["snapshot"] == "base"
                && scene["qualified_name"] == "scene.Demo"));
        assert!(
            output.document["affected_scenes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|scene| scene["snapshot"] == "target"
                    && scene["qualified_name"] == "scene.Demo")
        );
    }

    #[test]
    fn identical_snapshots_have_no_impact_and_byte_stable_output() {
        let base = snapshot(&[("scene.py", "value = 1\n")]);
        let target = snapshot(&[("scene.py", "value = 1\n")]);
        let first = compare(base.input(), target.input());
        let second = compare(base.input(), target.input());
        assert_eq!(first.json, second.json);
        assert_eq!(first.document["completeness"], "complete");
        assert!(
            first.document["changed_files"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            first.document["reason_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
