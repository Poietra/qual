//! Semantic dependency graph shared by incremental cache partitioning and
//! source-change impact analysis (DESIGN §8.4, §9).
//!
//! Edge direction is always **dependent → dependency**. For example, a
//! caller points to its callee and a Scene points to its `construct`
//! definition. Cache partitioning ignores direction and takes weakly
//! connected file components; change impact starts at a changed dependency
//! and walks the reverse index toward callers, Scenes, plays, and objects.
//!
//! Dynamic relationships never become guessed edges. They are retained as
//! [`DependencyUnknown`] frontiers with an owning node and source anchor.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rustpython_parser::ast::{self, Ranged};

use crate::frontend::imports::{ImportTarget, ImportedNames, import_from_names};
use crate::frontend::index::{BaseRef, ProjectIndex, QualifiedCall, QualifiedCallFacts};
use crate::frontend::names::Binding;
use crate::frontend::parser::{ModuleIdentity, module_identity};
use crate::semantic::interpreter::{DefMap, LifecycleFacts, PlayFact, SceneLifecycle, UpdaterHost};
use crate::semantic::values::{AllocationSite, ObjectId};
use crate::source::{FileId, SourceManager};

/// Kind of a project definition represented in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefinitionKind {
    /// A project class statement.
    Class,
    /// A module-level function or directly declared method.
    Callable,
}

/// Snapshot-local identity of one project definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefinitionNode {
    /// Fully qualified project name.
    pub qualified_name: String,
    /// Class versus callable.
    pub kind: DefinitionKind,
    /// Definition statement anchor.
    pub site: AllocationSite,
}

/// Snapshot-local identity of a play candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlayNode {
    /// Owning Scene qualified name.
    pub scene: String,
    /// Program-order ordinal inside the Scene lifecycle facts.
    pub ordinal: usize,
    /// Source call site (inside a helper when inlined).
    pub site: AllocationSite,
    /// Helper call sites, outermost first.
    pub call_path: Vec<AllocationSite>,
}

/// Snapshot-local identity of a reachable object candidate.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectNode {
    /// Owning Scene qualified name.
    pub scene: String,
    /// Abstract allocation identity, including bounded call context and
    /// cardinality.
    pub object: ObjectId,
}

/// A node in the semantic dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyNode {
    /// One project source file.
    File(FileId),
    /// One class, function, or method definition.
    Definition(DefinitionNode),
    /// One discovered Scene class.
    Scene(String),
    /// One play/wait candidate in a Scene execution.
    Play(PlayNode),
    /// One reachable object candidate in a Scene execution.
    Object(ObjectNode),
}

impl DependencyNode {
    /// The directly owned source file, when the node has one.
    #[must_use]
    pub const fn file(&self) -> Option<FileId> {
        match self {
            Self::File(file) => Some(*file),
            Self::Definition(definition) => Some(definition.site.file),
            Self::Play(play) => Some(play.site.file),
            Self::Object(object) => Some(object.object.site.file),
            Self::Scene(_) => None,
        }
    }
}

/// Why one node semantically depends on another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyReason {
    /// Two files resolve to the same module name and compete for ownership.
    ModuleCollision,
    /// A syntactic project import.
    Import,
    /// A final namespace/re-export binding to a project symbol.
    NamespaceBinding,
    /// A resolved project base class.
    BaseClass,
    /// A qualified project call target.
    Call,
    /// A definition is backed by a source file.
    DefinedIn,
    /// A Scene is represented by its class definition.
    SceneClass,
    /// A Scene lifecycle can execute this entry method.
    SceneEntrypoint,
    /// A play is emitted by this enclosing definition.
    PlayDefinition,
    /// An object is allocated by this enclosing definition.
    ObjectDefinition,
    /// A play/object candidate belongs to this Scene execution.
    SceneOwnership,
    /// An object may be targeted by an animation in this play.
    AnimationTarget,
    /// A starred play argument may contain an animation targeting any object
    /// in the owning Scene.
    StarredAnimationTarget,
    /// An object may host an updater registered by this Scene execution.
    UpdaterHost,
}

impl DependencyReason {
    /// Stable kebab-case label used by later JSON projections.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ModuleCollision => "module-collision",
            Self::Import => "import",
            Self::NamespaceBinding => "namespace-binding",
            Self::BaseClass => "base-class",
            Self::Call => "call",
            Self::DefinedIn => "defined-in",
            Self::SceneClass => "scene-class",
            Self::SceneEntrypoint => "scene-entrypoint",
            Self::PlayDefinition => "play-definition",
            Self::ObjectDefinition => "object-definition",
            Self::SceneOwnership => "scene-ownership",
            Self::AnimationTarget => "animation-target",
            Self::StarredAnimationTarget => "starred-animation-target",
            Self::UpdaterHost => "updater-host",
        }
    }

    const fn partitions_cache(self) -> bool {
        matches!(
            self,
            Self::ModuleCollision
                | Self::Import
                | Self::NamespaceBinding
                | Self::BaseClass
                | Self::Call
        )
    }
}

/// One directed dependency edge (`dependent` → `dependency`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DependencyEdge {
    /// Node whose meaning may change when `dependency` changes.
    pub dependent: DependencyNode,
    /// Node providing the consumed meaning.
    pub dependency: DependencyNode,
    /// Semantic reason for the edge.
    pub reason: DependencyReason,
    /// Source operation establishing the dependency, when available.
    pub anchor: Option<AllocationSite>,
}

/// Why a dependency edge could not be resolved conservatively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyUnknownKind {
    /// A call target was dynamic or had no resolved candidate.
    DynamicCallTarget,
    /// A base-class expression could not be resolved.
    UnresolvedBase,
    /// A relative import target could not be resolved.
    UnresolvedImport,
    /// A lifecycle entity could not be attributed to a project definition.
    UnavailableDefinition,
    /// A starred `Scene.play` argument hides animation identities and targets.
    StarArguments,
}

impl DependencyUnknownKind {
    /// Stable kebab-case label used by later JSON projections.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DynamicCallTarget => "dynamic-call-target",
            Self::UnresolvedBase => "unresolved-base",
            Self::UnresolvedImport => "unresolved-import",
            Self::UnavailableDefinition => "unavailable-definition",
            Self::StarArguments => "star-arguments",
        }
    }
}

/// A structured precision frontier in the dependency graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DependencyUnknown {
    /// Node whose dependency could not be completed.
    pub dependent: DependencyNode,
    /// Why resolution stopped.
    pub kind: DependencyUnknownKind,
    /// Source expression at which resolution stopped.
    pub anchor: AllocationSite,
}

/// One deterministic shortest reverse path from a changed dependency to an
/// affected dependent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReverseDependencyPath {
    /// Starting changed node.
    pub origin: DependencyNode,
    /// Reached dependent node.
    pub affected: DependencyNode,
    /// Edges in reverse-traversal order. Each stored edge itself retains the
    /// canonical dependent → dependency direction.
    pub edges: Vec<DependencyEdge>,
}

/// Project semantic dependencies with deterministic forward and reverse
/// indexes over one source snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticDependencyGraph {
    nodes: BTreeSet<DependencyNode>,
    edges: BTreeSet<DependencyEdge>,
    forward: BTreeMap<DependencyNode, Vec<DependencyEdge>>,
    reverse: BTreeMap<DependencyNode, Vec<DependencyEdge>>,
    definitions: BTreeMap<String, DefinitionNode>,
    unknowns: BTreeSet<DependencyUnknown>,
}

impl SemanticDependencyGraph {
    /// Builds file/definition/Scene dependencies from project frontend facts.
    /// This stage is sufficient for cache partitioning and runs before the
    /// lifecycle interpreter.
    #[must_use]
    pub fn from_frontend(
        sources: &SourceManager,
        source_roots: &[String],
        index: &ProjectIndex,
        calls: &QualifiedCallFacts,
    ) -> Self {
        let mut graph = Self::default();
        graph.register_frontend_nodes(sources, index);
        graph.add_file_dependencies(sources, source_roots, index, calls);
        graph.add_definition_dependencies(index, calls);
        graph.rebuild_indexes();
        graph
    }

    /// Adds Scene execution, play, object, animation-target, and updater-host
    /// dependencies after lifecycle analysis.
    pub fn attach_lifecycle(
        &mut self,
        lifecycle: &LifecycleFacts,
        sources: &SourceManager,
        index: &ProjectIndex,
    ) {
        let defs = DefMap::build(sources, index);
        for scene in &lifecycle.scenes {
            self.attach_scene(scene, &defs);
        }
        self.rebuild_indexes();
    }

    /// Every graph node in deterministic order.
    pub fn nodes(&self) -> impl Iterator<Item = &DependencyNode> {
        self.nodes.iter()
    }

    /// Every canonical edge in deterministic order.
    pub fn edges(&self) -> impl Iterator<Item = &DependencyEdge> {
        self.edges.iter()
    }

    /// Structured unresolved dependency frontiers in deterministic order.
    pub fn unknowns(&self) -> impl Iterator<Item = &DependencyUnknown> {
        self.unknowns.iter()
    }

    /// Outgoing edges (`node` depends on each returned dependency).
    #[must_use]
    pub fn dependencies_of(&self, node: &DependencyNode) -> &[DependencyEdge] {
        self.forward.get(node).map_or(&[], Vec::as_slice)
    }

    /// Incoming edges (each returned dependent may be affected by `node`).
    #[must_use]
    pub fn dependents_of(&self, node: &DependencyNode) -> &[DependencyEdge] {
        self.reverse.get(node).map_or(&[], Vec::as_slice)
    }

    /// Looks up a project definition by its fully qualified name.
    #[must_use]
    pub fn definition(&self, qualified_name: &str) -> Option<&DefinitionNode> {
        self.definitions.get(qualified_name)
    }

    /// File-to-file edges that define dependency-closed cache partitions.
    /// Lifecycle ownership edges never participate in this view.
    pub fn cache_file_edges(&self) -> impl Iterator<Item = (FileId, FileId)> + '_ {
        self.edges.iter().filter_map(|edge| {
            if !edge.reason.partitions_cache() {
                return None;
            }
            match (&edge.dependent, &edge.dependency) {
                (DependencyNode::File(dependent), DependencyNode::File(dependency)) => {
                    Some((*dependent, *dependency))
                }
                _ => None,
            }
        })
    }

    /// Deterministic shortest reverse paths for every `(origin, affected)`
    /// pair. Ties for one pair follow the stable edge order; paths from one
    /// changed definition never hide paths from another changed definition.
    #[must_use]
    pub fn reverse_paths(&self, origins: &BTreeSet<DependencyNode>) -> Vec<ReverseDependencyPath> {
        let mut all = Vec::new();
        for origin in origins {
            all.extend(self.reverse_paths_from(origin).into_values());
        }
        all.sort();
        all
    }

    /// Deterministic shortest paths from one changed dependency.
    #[must_use]
    pub fn reverse_paths_from(
        &self,
        origin: &DependencyNode,
    ) -> BTreeMap<DependencyNode, ReverseDependencyPath> {
        let mut paths = BTreeMap::new();
        let mut queue = VecDeque::new();
        paths.insert(
            origin.clone(),
            ReverseDependencyPath {
                origin: origin.clone(),
                affected: origin.clone(),
                edges: Vec::new(),
            },
        );
        queue.push_back(origin.clone());
        while let Some(current) = queue.pop_front() {
            let current_path = paths[&current].clone();
            for edge in self.dependents_of(&current) {
                let affected = edge.dependent.clone();
                if paths.contains_key(&affected) {
                    continue;
                }
                let mut edges = current_path.edges.clone();
                edges.push(edge.clone());
                paths.insert(
                    affected.clone(),
                    ReverseDependencyPath {
                        origin: current_path.origin.clone(),
                        affected: affected.clone(),
                        edges,
                    },
                );
                queue.push_back(affected);
            }
        }
        paths
    }

    fn register_frontend_nodes(&mut self, sources: &SourceManager, index: &ProjectIndex) {
        self.nodes.extend(
            sources
                .files()
                .iter()
                .map(|source| DependencyNode::File(source.id())),
        );
        let defs = DefMap::build(sources, index);
        for (name, def) in &defs.defs {
            self.register_definition(&DefinitionNode {
                qualified_name: name.clone(),
                kind: DefinitionKind::Callable,
                site: AllocationSite::new(def.file, def.range),
            });
        }
        for class in index.classes.values() {
            self.register_definition(&DefinitionNode {
                qualified_name: class.qualified_name.clone(),
                kind: DefinitionKind::Class,
                site: AllocationSite::new(class.file, class.range),
            });
        }
        for scene in &index.scene_classes {
            self.nodes.insert(DependencyNode::Scene(scene.clone()));
        }
    }

    fn register_definition(&mut self, definition: &DefinitionNode) {
        self.nodes
            .insert(DependencyNode::Definition(definition.clone()));
        self.definitions
            .insert(definition.qualified_name.clone(), definition.clone());
        self.add_edge(DependencyEdge {
            dependent: DependencyNode::Definition(definition.clone()),
            dependency: DependencyNode::File(definition.site.file),
            reason: DependencyReason::DefinedIn,
            anchor: Some(definition.site),
        });
    }

    fn add_file_dependencies(
        &mut self,
        sources: &SourceManager,
        source_roots: &[String],
        index: &ProjectIndex,
        calls: &QualifiedCallFacts,
    ) {
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
        let mut first_by_module: BTreeMap<&str, FileId> = BTreeMap::new();
        for (file, identity) in &identities {
            if let Some(first) = first_by_module.get(identity.name.as_str()) {
                self.add_file_edge(*file, *first, DependencyReason::ModuleCollision, None);
                self.add_file_edge(*first, *file, DependencyReason::ModuleCollision, None);
            } else {
                first_by_module.insert(identity.name.as_str(), *file);
            }
        }

        let owners = module_owners(index);
        for file in sources.files() {
            let (Some(module), Some(identity)) = (file.ast(), identities.get(&file.id())) else {
                continue;
            };
            let mut imports = Vec::new();
            collect_import_targets(
                &module.body,
                identity,
                &mut imports,
                &mut self.unknowns,
                file.id(),
            );
            for (target, anchor) in imports {
                for owner in module_prefix_owners(&target, &owners) {
                    self.add_file_edge(file.id(), owner, DependencyReason::Import, Some(anchor));
                }
            }
        }

        for record in index.modules.values() {
            for binding in record.namespace.values() {
                let target = match binding {
                    Binding::ImportedModule(target)
                    | Binding::ImportedSymbol(target)
                    | Binding::LocalClass(target)
                    | Binding::LocalFunction(target) => Some(target.as_str()),
                    Binding::LocalVar(_) | Binding::Unknown => None,
                };
                if let Some(owner) = target.and_then(|target| qualified_owner(target, index)) {
                    self.add_file_edge(
                        record.file,
                        owner,
                        DependencyReason::NamespaceBinding,
                        None,
                    );
                }
            }
        }
        for class in index.classes.values() {
            for (base, range) in class.bases.iter().zip(&class.base_ranges) {
                if let BaseRef::Resolved(target) = base {
                    if let Some(owner) = qualified_owner(target, index) {
                        self.add_file_edge(
                            class.file,
                            owner,
                            DependencyReason::BaseClass,
                            Some(AllocationSite::new(class.file, *range)),
                        );
                    }
                }
            }
        }
        for call in &calls.calls {
            for candidate in &call.candidates {
                if let Some(owner) = qualified_owner(candidate, index) {
                    self.add_file_edge(
                        call.file,
                        owner,
                        DependencyReason::Call,
                        Some(AllocationSite::new(call.file, call.call_range)),
                    );
                }
            }
        }
    }

    fn add_definition_dependencies(&mut self, index: &ProjectIndex, calls: &QualifiedCallFacts) {
        let definition_names: BTreeSet<&str> =
            self.definitions.keys().map(String::as_str).collect();
        let mut pending_edges = Vec::new();
        let mut pending_unknowns = Vec::new();
        for call in &calls.calls {
            let dependent = caller_definition(call, &definition_names)
                .and_then(|name| self.definitions.get(&name).cloned())
                .map_or(DependencyNode::File(call.file), DependencyNode::Definition);
            let anchor = AllocationSite::new(call.file, call.call_range);
            let mut project_target = false;
            for candidate in &call.candidates {
                if let Some(definition) = self.definitions.get(candidate) {
                    project_target = true;
                    pending_edges.push(DependencyEdge {
                        dependent: dependent.clone(),
                        dependency: DependencyNode::Definition(definition.clone()),
                        reason: DependencyReason::Call,
                        anchor: Some(anchor),
                    });
                }
            }
            if !project_target && call_is_dynamic(call) {
                pending_unknowns.push(DependencyUnknown {
                    dependent,
                    kind: DependencyUnknownKind::DynamicCallTarget,
                    anchor,
                });
            }
        }
        for edge in pending_edges {
            self.add_edge(edge);
        }
        self.unknowns.extend(pending_unknowns);

        let classes: Vec<_> = index.classes.values().collect();
        for class in classes {
            let Some(class_node) = self.definitions.get(&class.qualified_name).cloned() else {
                continue;
            };
            for (base, range) in class.bases.iter().zip(&class.base_ranges) {
                let anchor = AllocationSite::new(class.file, *range);
                match base {
                    BaseRef::Resolved(target) => {
                        if let Some(base_node) = self.definitions.get(target).cloned() {
                            self.add_edge(DependencyEdge {
                                dependent: DependencyNode::Definition(class_node.clone()),
                                dependency: DependencyNode::Definition(base_node),
                                reason: DependencyReason::BaseClass,
                                anchor: Some(anchor),
                            });
                        }
                    }
                    BaseRef::Unresolved(_) => {
                        self.unknowns.insert(DependencyUnknown {
                            dependent: DependencyNode::Definition(class_node.clone()),
                            kind: DependencyUnknownKind::UnresolvedBase,
                            anchor,
                        });
                    }
                }
            }
        }

        let scenes: Vec<String> = index.scene_classes.iter().cloned().collect();
        for scene in scenes {
            let scene_node = DependencyNode::Scene(scene.clone());
            if let Some(class_node) = self.definitions.get(&scene).cloned() {
                self.add_edge(DependencyEdge {
                    dependent: scene_node.clone(),
                    dependency: DependencyNode::Definition(class_node),
                    reason: DependencyReason::SceneClass,
                    anchor: None,
                });
            }
            for method in lifecycle_entry_definitions(&scene, index, &self.definitions) {
                self.add_edge(DependencyEdge {
                    dependent: scene_node.clone(),
                    dependency: DependencyNode::Definition(method),
                    reason: DependencyReason::SceneEntrypoint,
                    anchor: None,
                });
            }
        }
    }

    fn attach_scene(&mut self, scene: &SceneLifecycle, defs: &DefMap<'_>) {
        let scene_node = DependencyNode::Scene(scene.qualified_name.clone());
        self.nodes.insert(scene_node.clone());
        let mut play_nodes = Vec::new();
        for (ordinal, play) in scene.plays.iter().enumerate() {
            let play_node = DependencyNode::Play(PlayNode {
                scene: scene.qualified_name.clone(),
                ordinal,
                site: play.site,
                call_path: play.call_path.clone(),
            });
            self.nodes.insert(play_node.clone());
            self.add_edge(DependencyEdge {
                dependent: play_node.clone(),
                dependency: scene_node.clone(),
                reason: DependencyReason::SceneOwnership,
                anchor: Some(play.site),
            });
            if let Some(definition) = enclosing_definition(play.site, defs, &self.definitions) {
                self.add_edge(DependencyEdge {
                    dependent: play_node.clone(),
                    dependency: DependencyNode::Definition(definition),
                    reason: DependencyReason::PlayDefinition,
                    anchor: Some(play.site),
                });
            } else {
                self.unknowns.insert(DependencyUnknown {
                    dependent: play_node.clone(),
                    kind: DependencyUnknownKind::UnavailableDefinition,
                    anchor: play.site,
                });
            }
            if play.star_args {
                self.unknowns.insert(DependencyUnknown {
                    dependent: play_node.clone(),
                    kind: DependencyUnknownKind::StarArguments,
                    anchor: play.site,
                });
            }
            play_nodes.push(play_node);
        }

        let objects = reachable_objects(scene);
        let mut object_nodes = BTreeMap::new();
        for object in objects {
            let object_node = DependencyNode::Object(ObjectNode {
                scene: scene.qualified_name.clone(),
                object: object.clone(),
            });
            self.nodes.insert(object_node.clone());
            self.add_edge(DependencyEdge {
                dependent: object_node.clone(),
                dependency: scene_node.clone(),
                reason: DependencyReason::SceneOwnership,
                anchor: Some(object.site),
            });
            if let Some(definition) = enclosing_definition(object.site, defs, &self.definitions) {
                self.add_edge(DependencyEdge {
                    dependent: object_node.clone(),
                    dependency: DependencyNode::Definition(definition),
                    reason: DependencyReason::ObjectDefinition,
                    anchor: Some(object.site),
                });
            }
            object_nodes.insert(object, object_node);
        }
        for (ordinal, play) in scene.plays.iter().enumerate() {
            self.attach_starred_play_targets(play, &play_nodes[ordinal], &object_nodes);
            for animation in &play.animations {
                if let Some(state) = &animation.state {
                    for target in &state.targets {
                        if let Some(object_node) = object_nodes.get(target) {
                            self.add_edge(DependencyEdge {
                                dependent: object_node.clone(),
                                dependency: play_nodes[ordinal].clone(),
                                reason: DependencyReason::AnimationTarget,
                                anchor: Some(animation.site),
                            });
                        }
                    }
                }
                if let Some(target) = &animation.replacement_target {
                    if let Some(object_node) = object_nodes.get(target) {
                        self.add_edge(DependencyEdge {
                            dependent: object_node.clone(),
                            dependency: play_nodes[ordinal].clone(),
                            reason: DependencyReason::AnimationTarget,
                            anchor: Some(animation.site),
                        });
                    }
                }
            }
        }
        self.attach_updater_hosts(scene, defs, &object_nodes);
    }

    fn attach_starred_play_targets(
        &mut self,
        play: &PlayFact,
        play_node: &DependencyNode,
        object_nodes: &BTreeMap<ObjectId, DependencyNode>,
    ) {
        if !play.star_args {
            return;
        }
        for object_node in object_nodes.values() {
            self.add_edge(DependencyEdge {
                dependent: object_node.clone(),
                dependency: play_node.clone(),
                reason: DependencyReason::StarredAnimationTarget,
                anchor: Some(play.site),
            });
        }
    }

    fn attach_updater_hosts(
        &mut self,
        scene: &SceneLifecycle,
        defs: &DefMap<'_>,
        object_nodes: &BTreeMap<ObjectId, DependencyNode>,
    ) {
        for updater in &scene.updaters {
            if let UpdaterHost::Mobject(host) = &updater.host {
                if let Some(object_node) = object_nodes.get(host) {
                    if let Some(definition) =
                        enclosing_definition(updater.site, defs, &self.definitions)
                    {
                        self.add_edge(DependencyEdge {
                            dependent: object_node.clone(),
                            dependency: DependencyNode::Definition(definition),
                            reason: DependencyReason::UpdaterHost,
                            anchor: Some(updater.site),
                        });
                    } else {
                        self.unknowns.insert(DependencyUnknown {
                            dependent: object_node.clone(),
                            kind: DependencyUnknownKind::UnavailableDefinition,
                            anchor: updater.site,
                        });
                    }
                }
            }
        }
    }

    fn add_file_edge(
        &mut self,
        dependent: FileId,
        dependency: FileId,
        reason: DependencyReason,
        anchor: Option<AllocationSite>,
    ) {
        self.add_edge(DependencyEdge {
            dependent: DependencyNode::File(dependent),
            dependency: DependencyNode::File(dependency),
            reason,
            anchor,
        });
    }

    fn add_edge(&mut self, edge: DependencyEdge) {
        self.nodes.insert(edge.dependent.clone());
        self.nodes.insert(edge.dependency.clone());
        self.edges.insert(edge);
    }

    fn rebuild_indexes(&mut self) {
        self.forward.clear();
        self.reverse.clear();
        for edge in &self.edges {
            self.forward
                .entry(edge.dependent.clone())
                .or_default()
                .push(edge.clone());
            self.reverse
                .entry(edge.dependency.clone())
                .or_default()
                .push(edge.clone());
        }
    }
}

fn module_owners(index: &ProjectIndex) -> BTreeMap<&str, FileId> {
    index
        .modules
        .iter()
        .map(|(name, record)| (name.as_str(), record.file))
        .collect()
}

fn module_prefix_owners(target: &str, owners: &BTreeMap<&str, FileId>) -> BTreeSet<FileId> {
    let mut result = BTreeSet::new();
    let mut end = target.len();
    loop {
        let prefix = &target[..end];
        if let Some(owner) = owners.get(prefix) {
            result.insert(*owner);
        }
        let Some(dot) = prefix.rfind('.') else {
            break;
        };
        end = dot;
    }
    result
}

fn qualified_owner(qualified: &str, index: &ProjectIndex) -> Option<FileId> {
    index
        .modules
        .iter()
        .filter(|(module, _)| {
            qualified == module.as_str()
                || qualified
                    .strip_prefix(module.as_str())
                    .is_some_and(|tail| tail.starts_with('.'))
        })
        .max_by_key(|(module, _)| module.len())
        .map(|(_, record)| record.file)
}

fn caller_definition(call: &QualifiedCall, definitions: &BTreeSet<&str>) -> Option<String> {
    let function = call.context.function.as_deref()?;
    let base = call
        .context
        .class_name
        .as_deref()
        .unwrap_or(&call.context.module);
    let mut path: Vec<&str> = function.split('.').collect();
    while !path.is_empty() {
        let candidate = format!("{base}.{}", path.join("."));
        if definitions.contains(candidate.as_str()) {
            return Some(candidate);
        }
        path.pop();
    }
    None
}

fn call_is_dynamic(call: &QualifiedCall) -> bool {
    if !call.candidates.is_empty() {
        return false;
    }
    let Some(dotted) = &call.callee_dotted else {
        return true;
    };
    !matches!(
        dotted.join(".").as_str(),
        "abs"
            | "all"
            | "any"
            | "bool"
            | "dict"
            | "enumerate"
            | "float"
            | "int"
            | "len"
            | "list"
            | "max"
            | "min"
            | "range"
            | "set"
            | "str"
            | "sum"
            | "tuple"
            | "zip"
    )
}

fn lifecycle_entry_definitions(
    scene: &str,
    index: &ProjectIndex,
    definitions: &BTreeMap<String, DefinitionNode>,
) -> Vec<DefinitionNode> {
    const ENTRIES: [&str; 4] = ["__init__", "setup", "construct", "tear_down"];
    let mut result = BTreeSet::new();
    let mut queue = vec![scene.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(class_name) = queue.pop() {
        if !visited.insert(class_name.clone()) {
            continue;
        }
        for method in ENTRIES {
            if let Some(definition) = definitions.get(&format!("{class_name}.{method}")) {
                result.insert(definition.clone());
            }
        }
        if let Some(class) = index.classes.get(&class_name) {
            for base in &class.bases {
                if let BaseRef::Resolved(base) = base {
                    if index.classes.contains_key(base) {
                        queue.push(base.clone());
                    }
                }
            }
        }
    }
    result.into_iter().collect()
}

fn enclosing_definition(
    site: AllocationSite,
    defs: &DefMap<'_>,
    definitions: &BTreeMap<String, DefinitionNode>,
) -> Option<DefinitionNode> {
    defs.defs
        .iter()
        .filter(|(_, def)| {
            def.file == site.file
                && u32::from(def.range.start()) <= site.start
                && site.end <= u32::from(def.range.end())
        })
        .min_by_key(|(_, def)| u32::from(def.range.end()) - u32::from(def.range.start()))
        .and_then(|(name, _)| definitions.get(name).cloned())
}

fn reachable_objects(scene: &SceneLifecycle) -> BTreeSet<ObjectId> {
    let mut objects = BTreeSet::new();
    for snapshot in &scene.snapshots {
        objects.extend(snapshot.heap.objects.keys().cloned());
        objects.extend(snapshot.heap.copy_of.keys().cloned());
        objects.extend(
            snapshot
                .heap
                .copy_of
                .values()
                .map(|copy| copy.original.clone()),
        );
    }
    objects.extend(scene.final_heap.objects.keys().cloned());
    objects.extend(scene.final_heap.copy_of.keys().cloned());
    objects.extend(
        scene
            .final_heap
            .copy_of
            .values()
            .map(|copy| copy.original.clone()),
    );
    for play in &scene.plays {
        for animation in &play.animations {
            if let Some(state) = &animation.state {
                objects.extend(state.targets.iter().cloned());
            }
            objects.extend(animation.replacement_target.iter().cloned());
        }
    }
    for updater in &scene.updaters {
        if let UpdaterHost::Mobject(host) = &updater.host {
            objects.insert(host.clone());
        }
    }
    objects.remove(&scene.scene_id);
    objects
}

#[allow(
    clippy::too_many_lines,
    reason = "an exhaustive Python statement walk is clearer as one recursive match"
)]
fn collect_import_targets(
    statements: &[ast::Stmt],
    identity: &ModuleIdentity,
    targets: &mut Vec<(String, AllocationSite)>,
    unknowns: &mut BTreeSet<DependencyUnknown>,
    file: FileId,
) {
    for statement in statements {
        let anchor = AllocationSite::new(file, statement.range());
        match statement {
            ast::Stmt::Import(import) => {
                targets.extend(
                    import
                        .names
                        .iter()
                        .map(|alias| (alias.name.to_string(), anchor)),
                );
            }
            ast::Stmt::ImportFrom(import) => match import_from_names(import, identity) {
                ImportedNames::Bindings(bindings) => {
                    for binding in bindings {
                        match binding.target {
                            ImportTarget::Module(module) => targets.push((module, anchor)),
                            ImportTarget::Symbol { module, name } => {
                                targets.push((format!("{module}.{name}"), anchor));
                                targets.push((module, anchor));
                            }
                            ImportTarget::Unknown => {
                                unknowns.insert(DependencyUnknown {
                                    dependent: DependencyNode::File(file),
                                    kind: DependencyUnknownKind::UnresolvedImport,
                                    anchor,
                                });
                            }
                        }
                    }
                }
                ImportedNames::Star {
                    module: Some(module),
                    ..
                } => targets.push((module, anchor)),
                ImportedNames::Star { module: None, .. } => {
                    unknowns.insert(DependencyUnknown {
                        dependent: DependencyNode::File(file),
                        kind: DependencyUnknownKind::UnresolvedImport,
                        anchor,
                    });
                }
            },
            ast::Stmt::FunctionDef(def) => {
                collect_import_targets(&def.body, identity, targets, unknowns, file);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                collect_import_targets(&def.body, identity, targets, unknowns, file);
            }
            ast::Stmt::ClassDef(def) => {
                collect_import_targets(&def.body, identity, targets, unknowns, file);
            }
            ast::Stmt::For(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
                collect_import_targets(&inner.orelse, identity, targets, unknowns, file);
            }
            ast::Stmt::AsyncFor(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
                collect_import_targets(&inner.orelse, identity, targets, unknowns, file);
            }
            ast::Stmt::While(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
                collect_import_targets(&inner.orelse, identity, targets, unknowns, file);
            }
            ast::Stmt::If(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
                collect_import_targets(&inner.orelse, identity, targets, unknowns, file);
            }
            ast::Stmt::With(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
            }
            ast::Stmt::AsyncWith(inner) => {
                collect_import_targets(&inner.body, identity, targets, unknowns, file);
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    collect_import_targets(&case.body, identity, targets, unknowns, file);
                }
            }
            ast::Stmt::Try(inner) => {
                collect_try_import_targets(
                    &inner.body,
                    &inner.orelse,
                    &inner.finalbody,
                    &inner.handlers,
                    identity,
                    targets,
                    unknowns,
                    file,
                );
            }
            ast::Stmt::TryStar(inner) => {
                collect_try_import_targets(
                    &inner.body,
                    &inner.orelse,
                    &inner.finalbody,
                    &inner.handlers,
                    identity,
                    targets,
                    unknowns,
                    file,
                );
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

#[allow(
    clippy::too_many_arguments,
    reason = "the helper mirrors all four Python try-statement child collections"
)]
fn collect_try_import_targets(
    body: &[ast::Stmt],
    orelse: &[ast::Stmt],
    finalbody: &[ast::Stmt],
    handlers: &[ast::ExceptHandler],
    identity: &ModuleIdentity,
    targets: &mut Vec<(String, AllocationSite)>,
    unknowns: &mut BTreeSet<DependencyUnknown>,
    file: FileId,
) {
    collect_import_targets(body, identity, targets, unknowns, file);
    collect_import_targets(orelse, identity, targets, unknowns, file);
    collect_import_targets(finalbody, identity, targets, unknowns, file);
    for handler in handlers {
        let ast::ExceptHandler::ExceptHandler(handler) = handler;
        collect_import_targets(&handler.body, identity, targets, unknowns, file);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::application::manim_surface;
    use crate::frontend::index;
    use crate::knowledge;
    use crate::semantic::interpreter;

    fn analyzed_graph(files: &[(&str, &str)]) -> SemanticDependencyGraph {
        let mut sources = SourceManager::new("/project");
        for (path, source) in files {
            sources.load_bytes(&Path::new("/project").join(path), source.as_bytes());
        }
        let profile = knowledge::load("upstream_0_20").unwrap();
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
        graph
    }

    #[test]
    fn shared_helper_reaches_every_caller_scene_play_and_target() {
        let graph = analyzed_graph(&[
            (
                "base.py",
                "from manim import FadeIn, Scene\n\nclass Base(Scene):\n    def show(self, mob):\n        self.play(FadeIn(mob))\n",
            ),
            (
                "a.py",
                "from manim import Square\nfrom base import Base\n\nclass A(Base):\n    def construct(self):\n        square = Square()\n        self.show(square)\n",
            ),
            (
                "b.py",
                "from manim import Circle\nfrom base import Base\n\nclass B(Base):\n    def construct(self):\n        circle = Circle()\n        self.show(circle)\n",
            ),
        ]);
        let helper = DependencyNode::Definition(
            graph
                .definition("base.Base.show")
                .expect("helper definition")
                .clone(),
        );
        let paths = graph.reverse_paths_from(&helper);
        let scenes: BTreeSet<&str> = paths
            .keys()
            .filter_map(|node| match node {
                DependencyNode::Scene(scene) => Some(scene.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(scenes, BTreeSet::from(["a.A", "b.B"]));

        let plays: Vec<&ReverseDependencyPath> = paths
            .iter()
            .filter_map(|(node, path)| matches!(node, DependencyNode::Play(_)).then_some(path))
            .collect();
        assert_eq!(plays.len(), 2);
        assert!(plays.iter().all(|path| {
            path.edges
                .iter()
                .map(|edge| edge.reason)
                .eq([DependencyReason::PlayDefinition])
        }));

        let affected_objects: Vec<&ObjectNode> = paths
            .keys()
            .filter_map(|node| match node {
                DependencyNode::Object(object) => Some(object),
                _ => None,
            })
            .collect();
        assert_eq!(
            affected_objects
                .iter()
                .map(|object| object.scene.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["a.A", "b.B"])
        );
        assert_eq!(
            graph
                .edges()
                .filter(|edge| edge.reason == DependencyReason::AnimationTarget)
                .count(),
            2
        );

        for scene in ["a.A", "b.B"] {
            let path = &paths[&DependencyNode::Scene(scene.to_owned())];
            assert_eq!(
                path.edges
                    .iter()
                    .map(|edge| edge.reason.label())
                    .collect::<Vec<_>>(),
                ["call", "scene-entrypoint"]
            );
        }
    }

    #[test]
    fn dynamic_calls_are_frontiers_not_guessed_edges() {
        let graph = analyzed_graph(&[(
            "scene.py",
            "from manim import Scene\n\nclass Demo(Scene):\n    def construct(self):\n        factory = getattr(self, unknown_name())\n        factory()\n",
        )]);
        let construct = DependencyNode::Definition(
            graph
                .definition("scene.Demo.construct")
                .expect("construct definition")
                .clone(),
        );
        assert!(graph.unknowns().any(|unknown| {
            unknown.dependent == construct
                && unknown.kind == DependencyUnknownKind::DynamicCallTarget
        }));
        assert!(graph.dependencies_of(&construct).iter().all(|edge| {
            edge.reason != DependencyReason::Call
                || matches!(edge.dependency, DependencyNode::Definition(_))
        }));
    }

    #[test]
    fn starred_play_arguments_frontier_widens_targets_to_every_scene_object() {
        let graph = analyzed_graph(&[(
            "scene.py",
            "from manim import Circle, Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square()\n        circle = Circle()\n        animations = []\n        self.play(*animations)\n",
        )]);
        let play = graph
            .nodes()
            .find(|node| matches!(node, DependencyNode::Play(_)))
            .cloned()
            .expect("play node");
        assert!(graph.unknowns().any(|unknown| {
            unknown.dependent == play && unknown.kind == DependencyUnknownKind::StarArguments
        }));
        let objects: Vec<DependencyNode> = graph
            .nodes()
            .filter(|node| matches!(node, DependencyNode::Object(_)))
            .cloned()
            .collect();
        assert_eq!(objects.len(), 2);
        assert!(objects.iter().all(|object| {
            graph.dependencies_of(object).iter().any(|edge| {
                edge.dependency == play && edge.reason == DependencyReason::StarredAnimationTarget
            })
        }));
    }

    #[test]
    fn updater_host_depends_on_the_registering_helper() {
        let graph = analyzed_graph(&[
            (
                "base.py",
                "from manim import Scene\n\nclass Base(Scene):\n    def bind(self, mob):\n        mob.add_updater(lambda current, dt: current.rotate(dt))\n",
            ),
            (
                "scene.py",
                "from manim import Square\nfrom base import Base\n\nclass Demo(Base):\n    def construct(self):\n        square = Square()\n        self.bind(square)\n",
            ),
        ]);
        let helper = DependencyNode::Definition(
            graph
                .definition("base.Base.bind")
                .expect("helper definition")
                .clone(),
        );
        let updater_hosts: Vec<&ObjectNode> = graph
            .dependents_of(&helper)
            .iter()
            .filter_map(|edge| match (&edge.dependent, edge.reason) {
                (DependencyNode::Object(object), DependencyReason::UpdaterHost) => Some(object),
                _ => None,
            })
            .collect();
        assert!(!updater_hosts.is_empty());
        assert!(
            updater_hosts
                .iter()
                .all(|object| object.scene == "scene.Demo")
        );
    }

    #[test]
    fn forward_and_reverse_indexes_retain_the_same_edge() {
        let graph = analyzed_graph(&[
            ("helper.py", "def helper():\n    pass\n"),
            (
                "caller.py",
                "from helper import helper\n\ndef caller():\n    helper()\n",
            ),
        ]);
        let caller = DependencyNode::Definition(graph.definition("caller.caller").unwrap().clone());
        let helper = DependencyNode::Definition(graph.definition("helper.helper").unwrap().clone());
        let forward = graph
            .dependencies_of(&caller)
            .iter()
            .find(|edge| edge.dependency == helper)
            .expect("caller to helper edge");
        let reverse = graph
            .dependents_of(&helper)
            .iter()
            .find(|edge| edge.dependent == caller)
            .expect("helper to caller reverse edge");
        assert_eq!(forward, reverse);
        assert_eq!(forward.reason, DependencyReason::Call);
        assert!(forward.anchor.is_some());
    }
}
