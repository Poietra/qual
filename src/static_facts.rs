//! `StaticFacts` v0 public semantic projection (RFC 0001).
//!
//! This module deliberately builds a new JSON document instead of serializing
//! analyzer structs. Internal file/object/play handles never cross the public
//! boundary. Every source-backed value is re-anchored against the immutable
//! raw-byte snapshot used for parsing, and every unknown carries provenance.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::model::ResolvedConfig;
use crate::frontend::cfg::{CfgStmt, ControlFlowGraph};
use crate::frontend::index::{LiteralFact, ProjectIndex, QualifiedCall, QualifiedCallFacts};
use crate::frontend::statements::each_statement;
use crate::knowledge::KnowledgeProfile;
use crate::render_order::{DisplayOrder, OrderUnknownReason, RenderOrderInputs};
use crate::semantic::dependency::DependencyNode;
use crate::semantic::heap::AbstractHeap;
use crate::semantic::interpreter::{
    FallbackReason, LifecycleFacts, PlayFact, PlayKind, SceneLifecycle, UpdaterHost,
};
use crate::semantic::state::{CallbackRef, MobjectState, PlayGroupId, WriteChannel};
use crate::semantic::values::{
    AllocationSite, Cardinality, CopyKind, KindSet, Num, NumLit, ObjectId, Presence, Truth,
};
use crate::source::{FileId, NewlineStyle, SourceManager};

const DOMAIN: &str = "static-facts-v0";
const ALWAYS_REDRAW: &str = "manim.animation.updaters.mobject_update_utils.always_redraw";
const PATH_IO_METHODS: [&str; 5] = [
    "open",
    "read_bytes",
    "read_text",
    "write_bytes",
    "write_text",
];
const OBJECT_CALL_CONTEXT_DEPTH: usize = 2;
const HELPER_CALL_PATH_DEPTH: usize = 32;

/// Inputs to the stable external projection.
///
/// `raw_sources` is indexed exactly like [`SourceManager::files`]. Keeping it
/// separate prevents internal source handles or decoded-text assumptions from
/// leaking into raw-content hashes.
pub struct ProjectionInput<'a> {
    pub sources: &'a SourceManager,
    pub raw_sources: &'a [Vec<u8>],
    pub config: &'a ResolvedConfig,
    pub knowledge: &'a KnowledgeProfile,
    pub index: &'a ProjectIndex,
    pub calls: &'a QualifiedCallFacts,
    pub lifecycle: &'a LifecycleFacts,
}

/// One completed `StaticFacts` document and its canonical pretty JSON.
#[derive(Debug)]
pub struct StaticFactsOutput {
    pub document: Value,
    pub json: String,
    pub(crate) index: ProjectionIndex,
}

/// Projects the complete fact stack into `StaticFacts` v0.
#[must_use]
pub fn project(input: ProjectionInput<'_>) -> StaticFactsOutput {
    let projector = Projector::new(input);
    let document = projector.build();
    let index = projector.projection_index();
    let mut rendered = serde_json::to_string_pretty(&document)
        .expect("StaticFacts projection contains only finite JSON values");
    rendered.push('\n');
    StaticFactsOutput {
        document,
        json: rendered,
        index,
    }
}

/// Internal bridge from snapshot-local semantic graph nodes to the public IDs
/// emitted in the document. The handles in the map are never serialized.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectionIndex {
    scenes: BTreeMap<String, String>,
    plays: BTreeMap<(String, usize), String>,
    objects: BTreeMap<(String, ObjectId), String>,
}

impl ProjectionIndex {
    pub(crate) fn id_for_node(&self, node: &DependencyNode) -> Option<&str> {
        match node {
            DependencyNode::Scene(scene) => self.scenes.get(scene).map(String::as_str),
            DependencyNode::Play(play) => self
                .plays
                .get(&(play.scene.clone(), play.ordinal))
                .map(String::as_str),
            DependencyNode::Object(object) => self
                .objects
                .get(&(object.scene.clone(), object.object.clone()))
                .map(String::as_str),
            DependencyNode::File(_) | DependencyNode::Definition(_) => None,
        }
    }

    pub(crate) fn object_location(&self, public_id: &str) -> Option<(&str, &ObjectId)> {
        self.objects
            .iter()
            .find_map(|((scene, object), id)| (id == public_id).then_some((scene.as_str(), object)))
    }
}

#[derive(Debug, Clone)]
struct FileMeta {
    raw_hash: String,
    decoded_hash: String,
}

#[derive(Debug)]
struct SceneIds {
    scene: String,
    objects: BTreeMap<ObjectId, String>,
    plays: Vec<String>,
    animations: Vec<Vec<String>>,
    updaters: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ProjectedField {
    AnimationArguments,
    Duration,
    ExecutionCertainty,
    Repetitions,
}

#[derive(Debug, Default)]
struct ProjectionProvenance {
    reasons: BTreeMap<(String, PlayGroupId, ProjectedField), BTreeSet<&'static str>>,
}

impl ProjectionProvenance {
    fn collect(input: &ProjectionInput<'_>) -> Self {
        let mut sidecar = Self::default();
        for scene in &input.lifecycle.scenes {
            for play in &scene.plays {
                if play.star_args {
                    sidecar.insert(
                        scene,
                        play,
                        ProjectedField::AnimationArguments,
                        "star-arguments",
                    );
                }
                if play.certainty == Presence::Maybe
                    && !scene.summary_derived_plays.contains(&play.play_group)
                {
                    for reason in play_control_flow_reasons(input, play) {
                        sidecar.insert(scene, play, ProjectedField::ExecutionCertainty, reason);
                    }
                }
                if matches!(play.duration, Num::Unknown) {
                    for reason in play_duration_unknown_reasons(input, play) {
                        sidecar.insert(scene, play, ProjectedField::Duration, reason);
                    }
                }
                if matches!(
                    play.repetitions,
                    Num::Unknown | Num::Interval { lo: None, .. }
                ) {
                    for reason in play_control_flow_reasons(input, play) {
                        if reason == "loop-widening" {
                            sidecar.insert(scene, play, ProjectedField::Repetitions, reason);
                        }
                    }
                }
            }
        }
        sidecar
    }

    fn insert(
        &mut self,
        scene: &SceneLifecycle,
        play: &PlayFact,
        field: ProjectedField,
        reason: &'static str,
    ) {
        self.reasons
            .entry((scene.qualified_name.clone(), play.play_group, field))
            .or_default()
            .insert(reason);
    }

    fn reason(
        &self,
        scene: &SceneLifecycle,
        play: &PlayFact,
        field: ProjectedField,
    ) -> Vec<&'static str> {
        self.reasons
            .get(&(scene.qualified_name.clone(), play.play_group, field))
            .map_or_else(
                || vec!["unsupported-semantics"],
                |reasons| reasons.iter().copied().collect(),
            )
    }
}

fn play_duration_unknown_reasons(
    input: &ProjectionInput<'_>,
    play: &PlayFact,
) -> BTreeSet<&'static str> {
    let mut reasons = BTreeSet::new();
    if play.star_args {
        reasons.insert("star-arguments");
    }
    let calls: Vec<&QualifiedCall> = input
        .calls
        .calls
        .iter()
        .filter(|call| {
            call.file == play.site.file
                && u32::from(call.call_range.start()) == play.site.start
                && u32::from(call.call_range.end()) == play.site.end
        })
        .collect();
    if let [call] = calls.as_slice() {
        collect_duration_call_reasons(call, &mut reasons);
    }
    for animation in &play.animations {
        for call in input.calls.calls.iter().filter(|call| {
            call.file == animation.site.file
                && u32::from(call.call_range.start()) == animation.site.start
                && u32::from(call.call_range.end()) == animation.site.end
        }) {
            collect_duration_call_reasons(call, &mut reasons);
        }
    }
    if play
        .animations
        .iter()
        .any(|animation| animation.state.is_none() || animation.convertible != Truth::Yes)
    {
        reasons.insert("unknown-animation-target");
    }
    reasons
}

fn collect_duration_call_reasons(call: &QualifiedCall, reasons: &mut BTreeSet<&'static str>) {
    if call.keyword("run_time").is_some_and(|argument| {
        !matches!(
            argument.literal,
            Some(LiteralFact::Int(_) | LiteralFact::Float(_))
        )
    }) {
        reasons.insert("non-literal-expression");
    }
    if call.has_star_star_kwargs {
        reasons.insert("star-arguments");
    }
}

fn play_control_flow_reasons(
    input: &ProjectionInput<'_>,
    play: &PlayFact,
) -> BTreeSet<&'static str> {
    let mut reasons = BTreeSet::new();
    for site in std::iter::once(&play.site).chain(play.call_path.iter()) {
        let Some(block) = enclosing_cfg_block(input.sources, site) else {
            continue;
        };
        if block.loop_depth > 0 {
            reasons.insert("loop-widening");
        }
        if block.cond_depth > block.loop_depth {
            reasons.insert("branch-join");
        }
    }
    reasons
}

fn enclosing_cfg_block<'a>(
    sources: &'a SourceManager,
    site: &AllocationSite,
) -> Option<crate::frontend::cfg::BasicBlock<'a>> {
    let module = sources.file(site.file).ast()?;
    let mut body = None;
    let mut body_span = u32::MAX;
    each_statement(&module.body, &mut |statement| {
        let range = statement.range();
        if !range_contains_site(range, site) {
            return;
        }
        let candidate = match statement {
            ast::Stmt::FunctionDef(def) => Some(def.body.as_slice()),
            ast::Stmt::AsyncFunctionDef(def) => Some(def.body.as_slice()),
            _ => None,
        };
        let span = u32::from(range.end()) - u32::from(range.start());
        if candidate.is_some() && span < body_span {
            body = candidate;
            body_span = span;
        }
    });
    let cfg = ControlFlowGraph::build(body?);
    cfg.blocks.into_iter().find(|block| {
        block
            .stmts
            .iter()
            .any(|statement| range_contains_site(cfg_statement_range(statement), site))
    })
}

fn cfg_statement_range(statement: &CfgStmt<'_>) -> TextRange {
    match statement {
        CfgStmt::Stmt(statement) => statement.range(),
        CfgStmt::Eval(expression) | CfgStmt::LoopTarget(expression) => expression.range(),
        CfgStmt::WithEnter(item) => item.context_expr.range(),
        CfgStmt::PatternBind(pattern) => pattern.range(),
    }
}

fn range_contains_site(range: TextRange, site: &AllocationSite) -> bool {
    u32::from(range.start()) <= site.start && site.end <= u32::from(range.end())
}

struct Projector<'a> {
    input: ProjectionInput<'a>,
    files: BTreeMap<FileId, FileMeta>,
    semantic_build_hash: String,
    semantic_config_hash: String,
    source_manifest_hash: String,
    snapshot_id: String,
    ids: Vec<SceneIds>,
    provenance: ProjectionProvenance,
}

#[allow(
    clippy::unused_self,
    reason = "projection helpers remain methods to keep the public-contract namespace cohesive"
)]
impl<'a> Projector<'a> {
    fn new(input: ProjectionInput<'a>) -> Self {
        assert_eq!(
            input.sources.files().len(),
            input.raw_sources.len(),
            "raw source snapshot must align with SourceManager"
        );
        let files: BTreeMap<FileId, FileMeta> = input
            .sources
            .files()
            .iter()
            .zip(input.raw_sources)
            .map(|(source, raw)| {
                (
                    source.id(),
                    FileMeta {
                        raw_hash: sha256(raw),
                        decoded_hash: sha256(source.text().as_bytes()),
                    },
                )
            })
            .collect();
        let semantic_build_hash = build_hash();
        let semantic_config_hash = semantic_config_hash(input.config);
        let source_manifest = source_manifest(input.sources, &files);
        let source_manifest_hash = hash_value(&source_manifest);
        let snapshot_preimage = json!([
            DOMAIN,
            "snapshot",
            crate::VERSION,
            semantic_build_hash,
            input.config.target_python,
            semantic_config_hash,
            [
                input.knowledge.name,
                input.knowledge.schema_version,
                input.knowledge.source_digest
            ],
            source_manifest,
        ]);
        let snapshot_id = format!("snapshot:sf0:{}", hex_hash(&snapshot_preimage));
        let ids = build_ids(&input, &files, &snapshot_id);
        let provenance = ProjectionProvenance::collect(&input);
        Self {
            input,
            files,
            semantic_build_hash,
            semantic_config_hash,
            source_manifest_hash,
            snapshot_id,
            ids,
            provenance,
        }
    }

    fn build(&self) -> Value {
        let mut scenes: Vec<Value> = self
            .input
            .lifecycle
            .scenes
            .iter()
            .zip(&self.ids)
            .map(|(scene, ids)| self.scene_fact(scene, ids))
            .collect();
        sort_by_id(&mut scenes);
        let mut objects = Vec::new();
        let mut plays = Vec::new();
        let mut animations = Vec::new();
        let mut updaters = Vec::new();
        for (scene, ids) in self.input.lifecycle.scenes.iter().zip(&self.ids) {
            objects.extend(self.object_facts(scene, ids));
            for (ordinal, play) in scene.plays.iter().enumerate() {
                plays.push(self.play_fact(scene, ids, ordinal, play));
                animations.extend(self.animation_facts(scene, ids, ordinal, play));
            }
            updaters.extend(self.updater_facts(scene, ids));
        }
        sort_by_id(&mut objects);
        sort_by_id(&mut plays);
        sort_by_id(&mut animations);
        sort_by_id(&mut updaters);
        let renderer_risks = self.renderer_risks();
        let coverage = self.coverage(&[
            scenes.as_slice(),
            objects.as_slice(),
            plays.as_slice(),
            animations.as_slice(),
            updaters.as_slice(),
        ]);
        json!({
            "schema_version": 0,
            "tool": {
                "name": "manim-lint",
                "version": crate::VERSION,
                "semantic_build_hash": self.semantic_build_hash,
            },
            "snapshot": {
                "id": self.snapshot_id,
                "source_manifest_hash": self.source_manifest_hash,
                "semantic_config_hash": self.semantic_config_hash,
                "target_python": self.input.config.target_python,
                "object_call_context_depth": OBJECT_CALL_CONTEXT_DEPTH,
                "helper_call_path_depth": HELPER_CALL_PATH_DEPTH,
            },
            "knowledge_profile": {
                "name": self.input.knowledge.name,
                "schema_version": self.input.knowledge.schema_version,
                "source_digest": self.input.knowledge.source_digest,
            },
            "profiles": self.profile_facts(),
            "files": self.file_facts(),
            "scenes": scenes,
            "objects": objects,
            "plays": plays,
            "animations": animations,
            "updaters": updaters,
            "renderer_risks": renderer_risks,
            "coverage": coverage,
        })
    }

    fn projection_index(&self) -> ProjectionIndex {
        let mut index = ProjectionIndex::default();
        for (scene, ids) in self.input.lifecycle.scenes.iter().zip(&self.ids) {
            index
                .scenes
                .insert(scene.qualified_name.clone(), ids.scene.clone());
            for (ordinal, id) in ids.plays.iter().enumerate() {
                index
                    .plays
                    .insert((scene.qualified_name.clone(), ordinal), id.clone());
            }
            for (object, id) in &ids.objects {
                index
                    .objects
                    .insert((scene.qualified_name.clone(), object.clone()), id.clone());
            }
        }
        index
    }

    fn profile_facts(&self) -> Vec<Value> {
        let mut profiles: Vec<Value> = self
            .input
            .config
            .active_profiles
            .iter()
            .map(|profile| {
                json!({
                    "name": profile.name,
                    "renderer": profile.renderer,
                    "config_hash": hash_serializable(profile),
                })
            })
            .collect();
        profiles.sort_by(|a, b| string_field(a, "name").cmp(string_field(b, "name")));
        profiles
    }

    fn file_facts(&self) -> Vec<Value> {
        let mut files: Vec<Value> = self
            .input
            .sources
            .files()
            .iter()
            .map(|source| {
                let meta = &self.files[&source.id()];
                let analysis = if source.is_parsed() {
                    json!({ "status": "analyzed" })
                } else {
                    let reason = if source.encoding().label == "unknown" {
                        "decode-error"
                    } else {
                        "parse-error"
                    };
                    unknown_status(reason, Some(self.zero_anchor(source.id())), None)
                };
                json!({
                    "path": source.relative_path(),
                    "raw_content_hash": meta.raw_hash,
                    "decoded_utf8_hash": meta.decoded_hash,
                    "encoding": source.encoding().label,
                    "byte_order_mark": source.encoding().byte_order_mark,
                    "newline": newline_name(source.newline()),
                    "utf8_byte_length": source.text().len(),
                    "analysis": analysis,
                })
            })
            .collect();
        files.sort_by(|a, b| string_field(a, "path").cmp(string_field(b, "path")));
        files
    }

    fn scene_fact(&self, scene: &SceneLifecycle, ids: &SceneIds) -> Value {
        let definition = self
            .input
            .index
            .classes
            .get(&scene.qualified_name)
            .map_or_else(
                || self.zero_anchor(scene.file),
                |class| self.range_anchor(class.file, class.range),
            );
        let constructor = if scene.constructor_state_unknown {
            unknown_status(
                "unavailable-definition",
                Some(definition.clone()),
                Some(vec![ids.scene.clone()]),
            )
        } else {
            known_truth(true)
        };
        let mut object_ids: Vec<String> = ids.objects.values().cloned().collect();
        object_ids.sort();
        let mut play_ids = ids.plays.clone();
        play_ids.sort();
        let mut updater_ids = ids.updaters.clone();
        updater_ids.sort();
        json!({
            "id": ids.scene,
            "qualified_name": scene.qualified_name,
            "definition_anchor": definition,
            "constructor_state_complete": constructor,
            "object_ids": object_ids,
            "play_ids": play_ids,
            "updater_ids": updater_ids,
        })
    }

    fn object_facts(&self, scene: &SceneLifecycle, ids: &SceneIds) -> Vec<Value> {
        let relations = self.object_relations(scene, ids);
        ids.objects
            .iter()
            .map(|(object, public_id)| {
                let anchor = self.anchor(object.site);
                let state = scene.final_heap.object(object);
                let call_context: Vec<Value> = object
                    .context
                    .frames()
                    .iter()
                    .map(|site| self.anchor(*site))
                    .collect();
                let kind = state.map_or_else(
                    || {
                        unknown_candidates(
                            Vec::<String>::new(),
                            "untracked-value",
                            anchor.clone(),
                            vec![public_id.clone()],
                        )
                    },
                    |state| self.kind_fact(&state.kind, &anchor, public_id),
                );
                let bindings = json!({
                    "status": "known",
                    "values": self.object_bindings(scene, object),
                });
                let final_membership = state.map_or_else(
                    || self.unknown_membership_state(&anchor, public_id),
                    |state| self.membership_state(state, &anchor, public_id),
                );
                json!({
                    "id": public_id,
                    "scene_id": ids.scene,
                    "allocation_anchor": anchor,
                    "call_context": call_context,
                    "cardinality": cardinality_name(object.cardinality),
                    "kind_candidates": kind,
                    "binding_candidates": bindings,
                    "relations": relations.get(object).cloned().unwrap_or_default(),
                    "final_membership": final_membership,
                })
            })
            .collect()
    }

    fn object_bindings(&self, scene: &SceneLifecycle, object: &ObjectId) -> Vec<String> {
        let mut names = BTreeSet::new();
        for snapshot in &scene.snapshots {
            for (name, bound) in &snapshot.object_bindings {
                if snapshot.heap.resolve(bound) == snapshot.heap.resolve(object) {
                    names.insert(name.clone());
                }
            }
        }
        names.into_iter().collect()
    }

    fn object_relations(
        &self,
        scene: &SceneLifecycle,
        ids: &SceneIds,
    ) -> BTreeMap<ObjectId, Vec<Value>> {
        let mut edges: BTreeMap<ObjectId, BTreeMap<&'static str, BTreeSet<ObjectId>>> =
            BTreeMap::new();
        for heap in scene
            .snapshots
            .iter()
            .map(|snapshot| &snapshot.heap)
            .chain(std::iter::once(&scene.final_heap))
        {
            for object in ids.objects.keys() {
                if let Some(state) = heap.object(object) {
                    for parent in &state.parents {
                        edges
                            .entry(object.clone())
                            .or_default()
                            .entry("parent")
                            .or_default()
                            .insert(heap.resolve(parent));
                    }
                    for child in &state.children {
                        edges
                            .entry(object.clone())
                            .or_default()
                            .entry("child")
                            .or_default()
                            .insert(heap.resolve(child));
                    }
                }
            }
            for (copy, provenance) in &heap.copy_of {
                let kind = match provenance.kind {
                    CopyKind::Copy | CopyKind::DeepCopy => "copy-of",
                    CopyKind::GenerateTarget => "generated-target-of",
                    CopyKind::AnimationStartingCopy => "animation-starting-copy-of",
                    CopyKind::AnimationTargetCopy => "animation-target-copy-of",
                };
                edges
                    .entry(copy.clone())
                    .or_default()
                    .entry(kind)
                    .or_default()
                    .insert(heap.resolve(&provenance.original));
            }
        }
        for play in &scene.plays {
            for played in &play.animations {
                let (Some(state), Some(replacement)) =
                    (&played.state, played.replacement_target.as_ref())
                else {
                    continue;
                };
                if state.replacement != Truth::Yes {
                    continue;
                }
                let replacement = scene.final_heap.resolve(replacement);
                for target in &state.targets {
                    let target = scene.final_heap.resolve(target);
                    edges
                        .entry(target.clone())
                        .or_default()
                        .entry("replaced-by")
                        .or_default()
                        .insert(replacement.clone());
                    edges
                        .entry(replacement.clone())
                        .or_default()
                        .entry("replaces")
                        .or_default()
                        .insert(target);
                }
            }
        }
        edges
            .into_iter()
            .map(|(object, by_kind)| {
                let mut relations: Vec<Value> = by_kind
                    .into_iter()
                    .filter_map(|(kind, objects)| {
                        let mut object_ids: Vec<String> = objects
                            .iter()
                            .filter_map(|object| ids.objects.get(object).cloned())
                            .collect();
                        object_ids.sort();
                        object_ids.dedup();
                        (!object_ids.is_empty()).then(|| {
                            json!({
                                "kind": kind,
                                "objects": {
                                    "status": "known",
                                    "object_ids": object_ids,
                                },
                            })
                        })
                    })
                    .collect();
                relations.sort_by_key(|value| serde_json::to_string(value).unwrap_or_default());
                (object, relations)
            })
            .collect()
    }

    fn play_fact(
        &self,
        scene: &SceneLifecycle,
        ids: &SceneIds,
        ordinal: usize,
        play: &PlayFact,
    ) -> Value {
        let anchor = self.anchor(play.site);
        let related = vec![ids.plays[ordinal].clone()];
        let certainty_reasons =
            self.provenance
                .reason(scene, play, ProjectedField::ExecutionCertainty);
        let certainty = presence_fact_many(play.certainty, &certainty_reasons, &anchor, &related);
        let repetition_reasons = self
            .provenance
            .reason(scene, play, ProjectedField::Repetitions);
        let repetitions = number_fact(&play.repetitions, &repetition_reasons, &anchor, &related);
        let duration_reasons = self
            .provenance
            .reason(scene, play, ProjectedField::Duration);
        let duration = number_fact(&play.duration, &duration_reasons, &anchor, &related);
        let animation_arguments_complete = if play.star_args {
            let reasons = self
                .provenance
                .reason(scene, play, ProjectedField::AnimationArguments);
            unknown_status_many(
                &reasons,
                Some(&anchor),
                Some(std::slice::from_ref(&ids.plays[ordinal])),
            )
        } else {
            known_truth(true)
        };
        let helper_call_path: Vec<Value> = play
            .call_path
            .iter()
            .take(HELPER_CALL_PATH_DEPTH)
            .map(|site| self.anchor(*site))
            .collect();
        let (membership, render_order) = self.play_boundaries(scene, ids, ordinal, play);
        json!({
            "id": ids.plays[ordinal],
            "scene_id": ids.scene,
            "kind": match play.kind { PlayKind::Play => "play", PlayKind::Wait => "wait" },
            "call_anchor": anchor,
            "helper_call_path": helper_call_path,
            "cardinality": play_cardinality(play),
            "execution_certainty": certainty,
            "repetitions": repetitions,
            "duration": duration,
            "animation_arguments_complete": animation_arguments_complete,
            "animation_ids": ids.animations[ordinal],
            "membership": membership,
            "render_order": render_order,
        })
    }

    fn play_boundaries(
        &self,
        scene: &SceneLifecycle,
        ids: &SceneIds,
        ordinal: usize,
        play: &PlayFact,
    ) -> (Value, Value) {
        let anchor = self.anchor(play.site);
        let related = vec![ids.plays[ordinal].clone()];
        let before = scene.state_at(play.site.file, play.site.start).map_or_else(
            || {
                unknown_membership(
                    Vec::new(),
                    "unavailable-definition",
                    anchor.clone(),
                    related.clone(),
                )
            },
            |snapshot| self.membership_fact_from_heap(scene, ids, &snapshot.heap, &anchor),
        );
        let after_snapshot = scene.snapshots.iter().rev().find(|snapshot| {
            snapshot.site.file == play.site.file
                && snapshot.site.start <= play.site.start
                && snapshot.site.end >= play.site.end
        });
        let after = after_snapshot.map_or_else(
            || {
                unknown_membership(
                    Vec::new(),
                    "unavailable-definition",
                    anchor.clone(),
                    related.clone(),
                )
            },
            |snapshot| self.membership_fact_from_heap(scene, ids, &snapshot.heap, &anchor),
        );
        let order_inputs = crate::render_order::inputs_at_play(scene, play);
        let during_membership = order_inputs.as_ref().map_or_else(
            |reason| {
                unknown_membership(
                    Vec::new(),
                    order_reason_kind(reason),
                    anchor.clone(),
                    related.clone(),
                )
            },
            |inputs| self.membership_fact_during(ids, inputs, &anchor),
        );
        let during_order = order_inputs.map_or_else(
            |reason| self.unknown_order(ids, &anchor, &related, &reason),
            |inputs| match DisplayOrder::compute(&inputs) {
                DisplayOrder::Known(members) => {
                    let object_ids: Vec<String> = members
                        .iter()
                        .filter_map(|member| ids.objects.get(&member.id).cloned())
                        .collect();
                    json!({ "status": "known", "object_ids": object_ids })
                }
                DisplayOrder::Unknown(reason) => {
                    self.unknown_order(ids, &anchor, &related, &reason)
                }
            },
        );
        let unprojected_order = unknown_order(
            ids.objects.values().cloned().collect(),
            "unsupported-semantics",
            anchor,
            related,
        );
        (
            json!({
                "before": before,
                "during": during_membership,
                "after": after,
            }),
            json!({
                "before": unprojected_order.clone(),
                "during": during_order,
                "after": unprojected_order,
            }),
        )
    }

    fn membership_fact_from_heap(
        &self,
        _scene: &SceneLifecycle,
        ids: &SceneIds,
        heap: &AbstractHeap,
        anchor: &Value,
    ) -> Value {
        let mut entries = Vec::new();
        let mut uncertain = false;
        for (object, public_id) in &ids.objects {
            let Some(state) = heap.object(object) else {
                continue;
            };
            if !state.scene_root_membership.may_be_present()
                && !state.family_membership.may_be_present()
            {
                continue;
            }
            uncertain |= state.scene_root_membership == Presence::Maybe
                || state.family_membership == Presence::Maybe
                || state.foreground == Truth::Maybe;
            entries.push(json!({
                "object_id": public_id,
                "state": self.membership_state(state, anchor, public_id),
            }));
        }
        entries.sort_by(|a, b| string_field(a, "object_id").cmp(string_field(b, "object_id")));
        if uncertain {
            unknown_membership(
                entries,
                "unsupported-semantics",
                anchor.clone(),
                ids.objects.values().cloned().collect(),
            )
        } else {
            json!({ "status": "known", "entries": entries })
        }
    }

    fn membership_fact_during(
        &self,
        ids: &SceneIds,
        inputs: &RenderOrderInputs,
        anchor: &Value,
    ) -> Value {
        let roots: BTreeSet<ObjectId> = inputs.roots.iter().cloned().collect();
        let mut uncertain = false;
        let mut entries = Vec::new();
        for (object, member) in &inputs.members {
            let Some(public_id) = ids.objects.get(object) else {
                continue;
            };
            uncertain |= member.foreground == Truth::Maybe;
            entries.push(json!({
                "object_id": public_id,
                "state": {
                    "scene_root": known_presence(roots.contains(object)),
                    "family": known_presence(true),
                    "foreground": truth_fact(
                        member.foreground,
                        "unsupported-semantics",
                        anchor,
                        vec![public_id.clone()],
                    ),
                },
            }));
        }
        entries.sort_by(|a, b| string_field(a, "object_id").cmp(string_field(b, "object_id")));
        if uncertain {
            unknown_membership(
                entries,
                "unsupported-semantics",
                anchor.clone(),
                ids.objects.values().cloned().collect(),
            )
        } else {
            json!({ "status": "known", "entries": entries })
        }
    }

    fn unknown_order(
        &self,
        ids: &SceneIds,
        anchor: &Value,
        related: &[String],
        reason: &OrderUnknownReason,
    ) -> Value {
        let mut candidates: Vec<String> = ids.objects.values().cloned().collect();
        candidates.sort();
        unknown_order_with_detail(
            candidates,
            order_reason_kind(reason),
            anchor.clone(),
            related.to_vec(),
            reason.to_string(),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the schema's animation record is projected in one auditable field order"
    )]
    fn animation_facts(
        &self,
        _scene: &SceneLifecycle,
        ids: &SceneIds,
        play_ordinal: usize,
        play: &PlayFact,
    ) -> Vec<Value> {
        play.animations
            .iter()
            .enumerate()
            .map(|(animation_ordinal, played)| {
                let public_id = &ids.animations[play_ordinal][animation_ordinal];
                let anchor = self.anchor(played.site);
                let related = vec![public_id.clone()];
                let kind = played.state.as_ref().map_or_else(
                    || {
                        unknown_candidates(
                            Vec::new(),
                            "untracked-value",
                            anchor.clone(),
                            related.clone(),
                        )
                    },
                    |state| self.kind_fact(&state.kind, &anchor, public_id),
                );
                let targets = played.state.as_ref().map_or_else(
                    || {
                        unknown_object_candidates(
                            Vec::new(),
                            "unknown-animation-target",
                            anchor.clone(),
                            related.clone(),
                        )
                    },
                    |state| {
                        let mut target_ids: Vec<String> = state
                            .targets
                            .iter()
                            .filter_map(|target| ids.objects.get(target).cloned())
                            .collect();
                        target_ids.sort();
                        target_ids.dedup();
                        if target_ids.is_empty() || played.convertible != Truth::Yes {
                            unknown_object_candidates(
                                target_ids,
                                "unknown-animation-target",
                                anchor.clone(),
                                related.clone(),
                            )
                        } else {
                            json!({ "status": "known", "object_ids": target_ids })
                        }
                    },
                );
                let channels = played.state.as_ref().map_or_else(
                    || {
                        unknown_channels(
                            Vec::<String>::new(),
                            "unknown-write-channel",
                            anchor.clone(),
                            related.clone(),
                        )
                    },
                    |state| {
                        let channels: Vec<&str> = state
                            .write_channels
                            .iter()
                            .copied()
                            .map(channel_name)
                            .collect();
                        if played.channels_known == Truth::Yes {
                            json!({ "status": "known", "channels": channels })
                        } else {
                            unknown_channels(
                                channels,
                                "unknown-write-channel",
                                anchor.clone(),
                                related.clone(),
                            )
                        }
                    },
                );
                let effects = played.state.as_ref().map_or_else(
                    || {
                        json!({
                            "introducer": unknown_status(
                                "unsupported-semantics",
                                Some(anchor.clone()),
                                Some(related.clone()),
                            ),
                            "remover": unknown_status(
                                "unsupported-semantics",
                                Some(anchor.clone()),
                                Some(related.clone()),
                            ),
                            "replacement_targets": unknown_object_candidates(
                                Vec::new(),
                                "unknown-animation-target",
                                anchor.clone(),
                                related.clone(),
                            ),
                        })
                    },
                    |state| {
                        let replacement_targets = match state.replacement {
                            Truth::No => json!({ "status": "known", "object_ids": [] }),
                            Truth::Yes => played.replacement_target.as_ref().map_or_else(
                                || {
                                    unknown_object_candidates(
                                        Vec::new(),
                                        "unknown-animation-target",
                                        anchor.clone(),
                                        related.clone(),
                                    )
                                },
                                |target| {
                                    ids.objects.get(target).map_or_else(
                                        || {
                                            unknown_object_candidates(
                                                Vec::new(),
                                                "unknown-animation-target",
                                                anchor.clone(),
                                                related.clone(),
                                            )
                                        },
                                        |target| {
                                            json!({
                                                "status": "known",
                                                "object_ids": [target],
                                            })
                                        },
                                    )
                                },
                            ),
                            Truth::Maybe => unknown_object_candidates(
                                played
                                    .replacement_target
                                    .as_ref()
                                    .and_then(|target| ids.objects.get(target))
                                    .cloned()
                                    .into_iter()
                                    .collect(),
                                "unsupported-semantics",
                                anchor.clone(),
                                related.clone(),
                            ),
                        };
                        json!({
                            "introducer": truth_fact(
                                state.introducer,
                                "unsupported-semantics",
                                &anchor,
                                related.clone(),
                            ),
                            "remover": truth_fact(
                                state.remover,
                                "unsupported-semantics",
                                &anchor,
                                related.clone(),
                            ),
                            "replacement_targets": replacement_targets,
                        })
                    },
                );
                json!({
                    "id": public_id,
                    "play_id": ids.plays[play_ordinal],
                    "source_argument_ordinal": animation_ordinal,
                    "source_anchor": anchor,
                    "kind_candidates": kind,
                    "target_candidates": targets,
                    "write_channels": channels,
                    "effects": effects,
                })
            })
            .collect()
    }

    fn updater_facts(&self, scene: &SceneLifecycle, ids: &SceneIds) -> Vec<Value> {
        scene
            .updaters
            .iter()
            .enumerate()
            .map(|(ordinal, updater)| {
                let public_id = &ids.updaters[ordinal];
                let anchor = self.anchor(updater.site);
                let related = vec![public_id.clone()];
                let (host_ids, call_context) = match &updater.host {
                    UpdaterHost::Scene => (vec![ids.scene.clone()], Vec::new()),
                    UpdaterHost::Mobject(object) => (
                        ids.objects.get(object).cloned().into_iter().collect(),
                        object
                            .context
                            .frames()
                            .iter()
                            .map(|site| self.anchor(*site))
                            .collect(),
                    ),
                };
                let hosts = if host_ids.is_empty() {
                    unknown_entity_candidates(
                        Vec::new(),
                        "untracked-value",
                        anchor.clone(),
                        related.clone(),
                    )
                } else {
                    json!({ "status": "known", "entity_ids": host_ids })
                };
                let callbacks = match &updater.fact.callback {
                    CallbackRef::Named(name) => {
                        json!({ "status": "known", "values": [name] })
                    }
                    CallbackRef::Lambda(site) => json!({
                        "status": "known",
                        "values": [format!(
                            "lambda@{}:{}:{}",
                            self.input.sources.file(site.file).relative_path(),
                            site.start,
                            site.end,
                        )],
                    }),
                    CallbackRef::Unknown => unknown_candidates(
                        Vec::new(),
                        "unknown-callback",
                        anchor.clone(),
                        related.clone(),
                    ),
                };
                let channels: Vec<&str> = updater
                    .body
                    .write_channels
                    .iter()
                    .copied()
                    .map(channel_name)
                    .collect();
                let write_channels = if updater.body.channels_known == Truth::Yes {
                    json!({ "status": "known", "channels": channels })
                } else {
                    unknown_channels(
                        channels,
                        "unknown-write-channel",
                        anchor.clone(),
                        related.clone(),
                    )
                };
                let active_play_candidates = if ids.plays.is_empty() {
                    json!({ "status": "known", "play_ids": [] })
                } else {
                    unknown_play_candidates(
                        ids.plays.clone(),
                        "active-updater",
                        anchor.clone(),
                        related.clone(),
                    )
                };
                json!({
                    "id": public_id,
                    "scene_id": ids.scene,
                    "registration_anchor": anchor,
                    "call_context": call_context,
                    "host_candidates": hosts,
                    "callback_candidates": callbacks,
                    "time_based": truth_fact(
                        updater.fact.time_based,
                        "unknown-callback",
                        &self.anchor(updater.site),
                        related,
                    ),
                    "write_channels": write_channels,
                    "active_play_candidates": active_play_candidates,
                })
            })
            .collect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "each risk classifier mirrors one closed schema kind in a linear audit"
    )]
    fn renderer_risks(&self) -> Vec<Value> {
        let mut profiles = self.input.config.active_profile_names();
        profiles.sort();
        profiles.dedup();
        let mut risks: BTreeMap<String, Value> = BTreeMap::new();
        for (scene, ids) in self.input.lifecycle.scenes.iter().zip(&self.ids) {
            for (ordinal, play) in scene.plays.iter().enumerate() {
                let play_id = ids.plays[ordinal].clone();
                let anchor = self.anchor(play.site);
                if play.dynamic_wait != Truth::No {
                    insert_risk(
                        &mut risks,
                        &self.snapshot_id,
                        "dynamic-wait",
                        certainty_name(play.dynamic_wait == Truth::Yes),
                        anchor.clone(),
                        vec![play_id.clone()],
                        &profiles,
                    );
                }
                if play.has_stop_condition {
                    insert_risk(
                        &mut risks,
                        &self.snapshot_id,
                        "stop-condition",
                        "certain",
                        anchor.clone(),
                        vec![play_id.clone()],
                        &profiles,
                    );
                }
                if play.always_update_mobjects != Truth::No {
                    insert_risk(
                        &mut risks,
                        &self.snapshot_id,
                        "active-updater",
                        certainty_name(play.always_update_mobjects == Truth::Yes),
                        anchor.clone(),
                        vec![play_id.clone()],
                        &profiles,
                    );
                }
                if play.star_args {
                    insert_risk(
                        &mut risks,
                        &self.snapshot_id,
                        "unknown-animation-target",
                        "possible",
                        anchor.clone(),
                        vec![play_id.clone()],
                        &profiles,
                    );
                }
                for (animation_ordinal, played) in play.animations.iter().enumerate() {
                    let animation_id = ids.animations[ordinal][animation_ordinal].clone();
                    if played
                        .state
                        .as_ref()
                        .is_none_or(|state| state.targets.is_empty())
                        || played.convertible != Truth::Yes
                    {
                        insert_risk(
                            &mut risks,
                            &self.snapshot_id,
                            "unknown-animation-target",
                            "possible",
                            self.anchor(played.site),
                            vec![animation_id.clone()],
                            &profiles,
                        );
                    }
                    if played.channels_known != Truth::Yes {
                        insert_risk(
                            &mut risks,
                            &self.snapshot_id,
                            "unknown-write-channel",
                            "possible",
                            self.anchor(played.site),
                            vec![animation_id.clone()],
                            &profiles,
                        );
                    }
                    if played.state.as_ref().is_some_and(|state| {
                        state.write_channels.contains(&WriteChannel::CameraState)
                    }) {
                        insert_risk(
                            &mut risks,
                            &self.snapshot_id,
                            "camera-mutation",
                            "certain",
                            self.anchor(played.site),
                            vec![animation_id],
                            &profiles,
                        );
                    }
                }
                if match crate::render_order::inputs_at_play(scene, play)
                    .map(|inputs| DisplayOrder::compute(&inputs))
                {
                    Ok(order) => !order.is_known(),
                    Err(_) => true,
                } {
                    insert_risk(
                        &mut risks,
                        &self.snapshot_id,
                        "unknown-render-order",
                        "possible",
                        anchor,
                        vec![play_id],
                        &profiles,
                    );
                }
            }
            for (ordinal, updater) in scene.updaters.iter().enumerate() {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "active-updater",
                    "possible",
                    self.anchor(updater.site),
                    std::iter::once(ids.updaters[ordinal].clone())
                        .chain(ids.plays.iter().cloned())
                        .collect(),
                    &profiles,
                );
            }
        }
        for call in &self.input.calls.calls {
            let anchor = self.range_anchor(call.file, call.call_range);
            let targets = call_names(call);
            if is_dynamic_call(call) {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "dynamic-call",
                    "possible",
                    anchor.clone(),
                    self.call_scene_ids(call),
                    &profiles,
                );
            }
            if targets.iter().any(|target| target == ALWAYS_REDRAW) {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "always-redraw",
                    "certain",
                    anchor.clone(),
                    self.call_scene_ids(call),
                    &profiles,
                );
            }
            if targets.iter().any(|target| is_camera_call(target)) {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "camera-mutation",
                    "possible",
                    anchor.clone(),
                    self.call_scene_ids(call),
                    &profiles,
                );
            }
            if self.call_is_external(call) {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "external-state-or-io",
                    "possible",
                    anchor.clone(),
                    self.call_scene_ids(call),
                    &profiles,
                );
            }
            if targets.iter().any(|target| is_random_call(target)) {
                insert_risk(
                    &mut risks,
                    &self.snapshot_id,
                    "randomness",
                    "possible",
                    anchor,
                    self.call_scene_ids(call),
                    &profiles,
                );
            }
        }
        risks.into_values().collect()
    }

    fn call_scene_ids(&self, call: &QualifiedCall) -> Vec<String> {
        let Some(class) = call.context.class_name.as_deref() else {
            return Vec::new();
        };
        self.input
            .lifecycle
            .scenes
            .iter()
            .zip(&self.ids)
            .filter(|(scene, _)| scene.qualified_name == class)
            .map(|(_, ids)| ids.scene.clone())
            .collect()
    }

    fn call_is_external(&self, call: &QualifiedCall) -> bool {
        if call_names(call)
            .iter()
            .any(|target| is_external_call(target))
        {
            return true;
        }
        if matches!(call.callee_dotted.as_deref(), Some([name]) if name == "open") {
            return true;
        }
        let Some(chained) = &call.callee_chained else {
            return false;
        };
        if !PATH_IO_METHODS.contains(&chained.method.as_str()) {
            return false;
        }
        self.input
            .calls
            .calls
            .get(chained.inner)
            .is_some_and(|inner| {
                call_names(inner)
                    .iter()
                    .any(|target| target == "pathlib.Path")
            })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "coverage enumerates every precision frontier in the v0 contract"
    )]
    fn coverage(&self, projected_records: &[&[Value]]) -> Value {
        let mut frontiers: BTreeMap<String, Value> = BTreeMap::new();
        for source in self.input.sources.files() {
            if !source.is_parsed() {
                let anchor = self.zero_anchor(source.id());
                let reason = if source.encoding().label == "unknown" {
                    "decode-error"
                } else {
                    "parse-error"
                };
                insert_frontier(
                    &mut frontiers,
                    &self.snapshot_id,
                    "source",
                    Some(anchor.clone()),
                    Vec::new(),
                    vec![reason_value(reason, Some(anchor), None, None)],
                );
            }
            for diagnostic in
                crate::frontend::features::gate(source, &self.input.config.target_python)
            {
                let anchor = self.zero_anchor(source.id());
                insert_frontier(
                    &mut frontiers,
                    &self.snapshot_id,
                    "source",
                    Some(anchor.clone()),
                    Vec::new(),
                    vec![reason_value(
                        "unsupported-syntax",
                        Some(anchor),
                        None,
                        Some(diagnostic.message),
                    )],
                );
            }
        }
        for call in &self.input.calls.calls {
            if !is_dynamic_call(call) {
                continue;
            }
            let anchor = self.range_anchor(call.file, call.call_range);
            let related = self.call_scene_ids(call);
            insert_frontier(
                &mut frontiers,
                &self.snapshot_id,
                "call-resolution",
                Some(anchor.clone()),
                related.clone(),
                vec![reason_value(
                    "dynamic-call-target",
                    Some(anchor),
                    Some(related),
                    None,
                )],
            );
        }
        for fallback in &self.input.lifecycle.inline_fallbacks {
            let anchor = self.anchor(fallback.site);
            let (reason, detail) = match fallback.reason {
                FallbackReason::Recursion => ("recursive-summary", fallback.callee.clone()),
                FallbackReason::DepthCap => ("inline-depth-cap", fallback.callee.clone()),
                FallbackReason::Unresolvable => ("unavailable-definition", fallback.callee.clone()),
            };
            insert_frontier(
                &mut frontiers,
                &self.snapshot_id,
                "helper-expansion",
                Some(anchor.clone()),
                Vec::new(),
                vec![reason_value(reason, Some(anchor), None, Some(detail))],
            );
        }
        for (scene, ids) in self.input.lifecycle.scenes.iter().zip(&self.ids) {
            if scene.constructor_state_unknown {
                let anchor = self
                    .input
                    .index
                    .classes
                    .get(&scene.qualified_name)
                    .map_or_else(
                        || self.zero_anchor(scene.file),
                        |class| self.range_anchor(class.file, class.range),
                    );
                insert_frontier(
                    &mut frontiers,
                    &self.snapshot_id,
                    "name-resolution",
                    Some(anchor.clone()),
                    vec![ids.scene.clone()],
                    vec![reason_value(
                        "unavailable-definition",
                        Some(anchor),
                        Some(vec![ids.scene.clone()]),
                        None,
                    )],
                );
            }
            for (play_ordinal, play) in scene.plays.iter().enumerate() {
                let play_id = ids.plays[play_ordinal].clone();
                if let Err(reason) = crate::render_order::inputs_at_play(scene, play)
                    .map(|inputs| DisplayOrder::compute(&inputs))
                    .and_then(|order| match order {
                        DisplayOrder::Known(_) => Ok(()),
                        DisplayOrder::Unknown(reason) => Err(reason),
                    })
                {
                    let anchor = self.anchor(play.site);
                    insert_frontier(
                        &mut frontiers,
                        &self.snapshot_id,
                        "render-order",
                        Some(anchor.clone()),
                        vec![play_id.clone()],
                        vec![reason_value(
                            order_reason_kind(&reason),
                            Some(anchor),
                            Some(vec![play_id]),
                            Some(reason.to_string()),
                        )],
                    );
                }
                for (animation_ordinal, played) in play.animations.iter().enumerate() {
                    let animation_id = ids.animations[play_ordinal][animation_ordinal].clone();
                    if played
                        .state
                        .as_ref()
                        .is_none_or(|state| state.targets.is_empty())
                    {
                        let anchor = self.anchor(played.site);
                        insert_frontier(
                            &mut frontiers,
                            &self.snapshot_id,
                            "animation-target",
                            Some(anchor.clone()),
                            vec![animation_id.clone()],
                            vec![reason_value(
                                "unknown-animation-target",
                                Some(anchor),
                                Some(vec![animation_id.clone()]),
                                None,
                            )],
                        );
                    }
                    if played.channels_known != Truth::Yes {
                        let anchor = self.anchor(played.site);
                        insert_frontier(
                            &mut frontiers,
                            &self.snapshot_id,
                            "write-channel",
                            Some(anchor.clone()),
                            vec![animation_id.clone()],
                            vec![reason_value(
                                "unknown-write-channel",
                                Some(anchor),
                                Some(vec![animation_id]),
                                None,
                            )],
                        );
                    }
                }
            }
        }
        // Unknown projection fields and coverage must never disagree. This
        // recursive pass is deliberately over the stable public records,
        // rather than internal lattice values, so every reason-carrying
        // Unknown added to the contract automatically creates a frontier.
        // Certain renderer risks do not contain `status: unknown` and
        // therefore do not make coverage partial on their own.
        for records in projected_records {
            for record in *records {
                collect_unknown_frontiers(record, None, &mut frontiers, &self.snapshot_id);
            }
        }
        let values: Vec<Value> = frontiers.into_values().collect();
        json!({
            "completeness": if values.is_empty() { "complete" } else { "partial" },
            "frontiers": values,
        })
    }

    fn kind_fact(&self, kind: &KindSet, anchor: &Value, related: &str) -> Value {
        match kind {
            KindSet::Known(candidates) => json!({
                "status": "known",
                "values": candidates,
            }),
            KindSet::Unknown => unknown_candidates(
                Vec::new(),
                "unsupported-semantics",
                anchor.clone(),
                vec![related.to_owned()],
            ),
        }
    }

    fn membership_state(&self, state: &MobjectState, anchor: &Value, related: &str) -> Value {
        json!({
            "scene_root": presence_fact(
                state.scene_root_membership,
                "unsupported-semantics",
                anchor,
                vec![related.to_owned()],
            ),
            "family": presence_fact(
                state.family_membership,
                "unsupported-semantics",
                anchor,
                vec![related.to_owned()],
            ),
            "foreground": truth_fact(
                state.foreground,
                "unsupported-semantics",
                anchor,
                vec![related.to_owned()],
            ),
        })
    }

    fn unknown_membership_state(&self, anchor: &Value, related: &str) -> Value {
        json!({
            "scene_root": unknown_status(
                "untracked-value",
                Some(anchor.clone()),
                Some(vec![related.to_owned()]),
            ),
            "family": unknown_status(
                "untracked-value",
                Some(anchor.clone()),
                Some(vec![related.to_owned()]),
            ),
            "foreground": unknown_status(
                "untracked-value",
                Some(anchor.clone()),
                Some(vec![related.to_owned()]),
            ),
        })
    }

    fn range_anchor(&self, file: FileId, range: TextRange) -> Value {
        self.anchor(AllocationSite::new(file, range))
    }

    fn zero_anchor(&self, file: FileId) -> Value {
        self.anchor(AllocationSite {
            file,
            start: 0,
            end: 0,
        })
    }

    fn anchor(&self, site: AllocationSite) -> Value {
        source_anchor(self.input.sources, &self.files, site)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "all public ID preimages are kept together for contract review"
)]
fn build_ids(
    input: &ProjectionInput<'_>,
    files: &BTreeMap<FileId, FileMeta>,
    snapshot_id: &str,
) -> Vec<SceneIds> {
    input
        .lifecycle
        .scenes
        .iter()
        .map(|scene| {
            let definition = input.index.classes.get(&scene.qualified_name).map_or(
                AllocationSite {
                    file: scene.file,
                    start: 0,
                    end: 0,
                },
                |class| AllocationSite::new(class.file, class.range),
            );
            let definition_anchor = source_anchor(input.sources, files, definition);
            let scene_id = public_id(
                "scene",
                vec![
                    Value::String(snapshot_id.to_owned()),
                    Value::String(scene.qualified_name.clone()),
                    anchor_preimage(&definition_anchor),
                ],
            );
            let object_set = reachable_objects(scene);
            let objects: BTreeMap<ObjectId, String> = object_set
                .into_iter()
                .map(|object| {
                    let allocation = source_anchor(input.sources, files, object.site);
                    let context: Vec<Value> = object
                        .context
                        .frames()
                        .iter()
                        .map(|site| anchor_preimage(&source_anchor(input.sources, files, *site)))
                        .collect();
                    let id = public_id(
                        "object",
                        vec![
                            Value::String(snapshot_id.to_owned()),
                            Value::String(scene_id.clone()),
                            anchor_preimage(&allocation),
                            Value::Array(context),
                            Value::String(cardinality_name(object.cardinality).to_owned()),
                        ],
                    );
                    (object, id)
                })
                .collect();
            let plays: Vec<String> = scene
                .plays
                .iter()
                .map(|play| {
                    let anchor = source_anchor(input.sources, files, play.site);
                    let path: Vec<Value> = play
                        .call_path
                        .iter()
                        .take(HELPER_CALL_PATH_DEPTH)
                        .map(|site| anchor_preimage(&source_anchor(input.sources, files, *site)))
                        .collect();
                    public_id(
                        "play",
                        vec![
                            Value::String(snapshot_id.to_owned()),
                            Value::String(scene_id.clone()),
                            anchor_preimage(&anchor),
                            Value::Array(path),
                            Value::String(play_cardinality(play).to_owned()),
                        ],
                    )
                })
                .collect();
            let animations: Vec<Vec<String>> = scene
                .plays
                .iter()
                .enumerate()
                .map(|(play_ordinal, play)| {
                    play.animations
                        .iter()
                        .enumerate()
                        .map(|(animation_ordinal, animation)| {
                            let anchor = source_anchor(input.sources, files, animation.site);
                            public_id(
                                "animation",
                                vec![
                                    Value::String(snapshot_id.to_owned()),
                                    Value::String(plays[play_ordinal].clone()),
                                    json!(animation_ordinal),
                                    anchor_preimage(&anchor),
                                ],
                            )
                        })
                        .collect()
                })
                .collect();
            let mut equal_preimages: BTreeMap<String, usize> = BTreeMap::new();
            let updaters: Vec<String> = scene
                .updaters
                .iter()
                .map(|updater| {
                    let anchor = source_anchor(input.sources, files, updater.site);
                    let context: Vec<Value> = match &updater.host {
                        UpdaterHost::Scene => Vec::new(),
                        UpdaterHost::Mobject(object) => object
                            .context
                            .frames()
                            .iter()
                            .map(|site| {
                                anchor_preimage(&source_anchor(input.sources, files, *site))
                            })
                            .collect(),
                    };
                    let base = json!([snapshot_id, scene_id, anchor_preimage(&anchor), context,]);
                    let key = serde_json::to_string(&base).expect("id preimage");
                    let ordinal = equal_preimages.entry(key).or_default();
                    let id = public_id(
                        "updater",
                        vec![
                            Value::String(snapshot_id.to_owned()),
                            Value::String(scene_id.clone()),
                            anchor_preimage(&anchor),
                            base[3].clone(),
                            json!(*ordinal),
                        ],
                    );
                    *ordinal += 1;
                    id
                })
                .collect();
            SceneIds {
                scene: scene_id,
                objects,
                plays,
                animations,
                updaters,
            }
        })
        .collect()
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
                .map(|provenance| provenance.original.clone()),
        );
    }
    objects.extend(scene.final_heap.objects.keys().cloned());
    objects.extend(scene.final_heap.copy_of.keys().cloned());
    objects.extend(
        scene
            .final_heap
            .copy_of
            .values()
            .map(|provenance| provenance.original.clone()),
    );
    for play in &scene.plays {
        for played in &play.animations {
            if let Some(state) = &played.state {
                objects.extend(state.targets.iter().cloned());
            }
            objects.extend(played.replacement_target.iter().cloned());
        }
    }
    for updater in &scene.updaters {
        if let UpdaterHost::Mobject(object) = &updater.host {
            objects.insert(object.clone());
        }
    }
    objects.remove(&scene.scene_id);
    objects
}

fn source_manifest(sources: &SourceManager, files: &BTreeMap<FileId, FileMeta>) -> Value {
    let mut entries: Vec<Value> = sources
        .files()
        .iter()
        .map(|source| {
            let meta = &files[&source.id()];
            json!([
                source.relative_path(),
                meta.raw_hash,
                source.encoding().label,
                source.encoding().byte_order_mark,
                meta.decoded_hash,
            ])
        })
        .collect();
    entries.sort_by(|left, right| {
        left[0]
            .as_str()
            .unwrap_or("")
            .cmp(right[0].as_str().unwrap_or(""))
    });
    Value::Array(entries)
}

fn semantic_config_hash(config: &ResolvedConfig) -> String {
    let mut profiles: Vec<Value> = config
        .active_profiles
        .iter()
        .map(|profile| serde_json::to_value(profile).expect("render profile serializes"))
        .collect();
    profiles.sort_by(|left, right| string_field(left, "name").cmp(string_field(right, "name")));
    hash_value(&json!([
        DOMAIN,
        "semantic-config",
        config.manim_version,
        config.target_python,
        config.source_roots,
        config.stub_paths,
        profiles,
    ]))
}

fn source_anchor(
    sources: &SourceManager,
    files: &BTreeMap<FileId, FileMeta>,
    site: AllocationSite,
) -> Value {
    let source = sources.file(site.file);
    let start = usize::try_from(site.start)
        .unwrap_or(usize::MAX)
        .min(source.text().len());
    let end = usize::try_from(site.end)
        .unwrap_or(usize::MAX)
        .min(source.text().len())
        .max(start);
    let start_position = source.position_of_byte(start);
    let end_position = source.position_of_byte(end);
    json!({
        "path": source.relative_path(),
        "raw_content_hash": files[&site.file].raw_hash,
        "encoding": source.encoding().label,
        "byte_order_mark": source.encoding().byte_order_mark,
        "utf8_byte_range": { "start": start, "end": end },
        "unicode_span": {
            "start": {
                "line": start_position.line,
                "column": start_position.column,
            },
            "end": {
                "line": end_position.line,
                "column": end_position.column,
            },
        },
    })
}

/// Projects a semantic allocation site through the same source-anchor
/// contract used by `StaticFacts`. `raw_sources` must be the immutable snapshot
/// parallel to `SourceManager::files`.
pub(crate) fn project_source_anchor(
    sources: &SourceManager,
    raw_sources: &[Vec<u8>],
    site: AllocationSite,
) -> Value {
    assert_eq!(
        sources.files().len(),
        raw_sources.len(),
        "raw source snapshot must align with SourceManager"
    );
    let source = sources.file(site.file);
    let start = usize::try_from(site.start)
        .unwrap_or(usize::MAX)
        .min(source.text().len());
    let end = usize::try_from(site.end)
        .unwrap_or(usize::MAX)
        .min(source.text().len())
        .max(start);
    let start_position = source.position_of_byte(start);
    let end_position = source.position_of_byte(end);
    json!({
        "path": source.relative_path(),
        "raw_content_hash": sha256(&raw_sources[site.file.index()]),
        "encoding": source.encoding().label,
        "byte_order_mark": source.encoding().byte_order_mark,
        "utf8_byte_range": { "start": start, "end": end },
        "unicode_span": {
            "start": {
                "line": start_position.line,
                "column": start_position.column,
            },
            "end": {
                "line": end_position.line,
                "column": end_position.column,
            },
        },
    })
}

fn anchor_preimage(anchor: &Value) -> Value {
    json!([
        anchor["path"],
        anchor["raw_content_hash"],
        anchor["encoding"],
        anchor["byte_order_mark"],
        anchor["utf8_byte_range"]["start"],
        anchor["utf8_byte_range"]["end"],
        anchor["unicode_span"]["start"]["line"],
        anchor["unicode_span"]["start"]["column"],
        anchor["unicode_span"]["end"]["line"],
        anchor["unicode_span"]["end"]["column"],
    ])
}

fn public_id(kind: &str, fields: Vec<Value>) -> String {
    let mut preimage = vec![
        Value::String(DOMAIN.to_owned()),
        Value::String(kind.to_owned()),
    ];
    preimage.extend(fields);
    format!("{kind}:sf0:{}", hex_hash(&Value::Array(preimage)))
}

fn build_hash() -> String {
    let value = option_env!("MANIM_LINT_BUILD_ID").unwrap_or(crate::VERSION);
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        format!("sha256:{}", value.to_ascii_lowercase())
    } else {
        sha256(value.as_bytes())
    }
}

fn hash_serializable<T: serde::Serialize>(value: &T) -> String {
    sha256(&serde_json::to_vec(value).expect("semantic input serializes"))
}

fn hash_value(value: &Value) -> String {
    sha256(&serde_json::to_vec(value).expect("JSON value serializes"))
}

fn hex_hash(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("JSON value serializes");
    format!("{:x}", Sha256::digest(bytes))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn newline_name(newline: NewlineStyle) -> &'static str {
    match newline {
        NewlineStyle::Lf => "lf",
        NewlineStyle::CrLf => "crlf",
        NewlineStyle::Cr => "cr",
        NewlineStyle::Mixed => "mixed",
    }
}

fn cardinality_name(cardinality: Cardinality) -> &'static str {
    match cardinality {
        Cardinality::Singleton => "singleton",
        Cardinality::Many => "many",
        Cardinality::MaybeMany => "maybe-many",
    }
}

fn play_cardinality(play: &PlayFact) -> &'static str {
    match &play.repetitions {
        Num::Exact(NumLit::Int(value)) if *value <= 1 => "singleton",
        Num::Exact(NumLit::Float(value)) if *value <= 1.0 => "singleton",
        Num::Exact(_) => "many",
        Num::Interval {
            lo: Some(lo),
            hi: Some(hi),
        } if *lo > 1.0 && *hi > 1.0 => "many",
        Num::Interval { hi: Some(hi), .. } if *hi <= 1.0 => "singleton",
        Num::Interval { .. } | Num::Symbol(_) | Num::Unknown => "maybe-many",
    }
}

fn known_truth(value: bool) -> Value {
    json!({ "status": "known", "value": if value { "yes" } else { "no" } })
}

fn known_presence(value: bool) -> Value {
    json!({
        "status": "known",
        "value": if value { "present" } else { "absent" },
    })
}

fn truth_fact(truth: Truth, reason: &str, anchor: &Value, related: Vec<String>) -> Value {
    match truth {
        Truth::Yes => known_truth(true),
        Truth::No => known_truth(false),
        Truth::Maybe => unknown_status(reason, Some(anchor.clone()), Some(related)),
    }
}

fn presence_fact(presence: Presence, reason: &str, anchor: &Value, related: Vec<String>) -> Value {
    match presence {
        Presence::Present => known_presence(true),
        Presence::Absent => known_presence(false),
        Presence::Maybe => unknown_status(reason, Some(anchor.clone()), Some(related)),
    }
}

fn presence_fact_many(
    presence: Presence,
    reasons: &[&str],
    anchor: &Value,
    related: &[String],
) -> Value {
    match presence {
        Presence::Present => known_presence(true),
        Presence::Absent => known_presence(false),
        Presence::Maybe => unknown_status_many(reasons, Some(anchor), Some(related)),
    }
}

fn number_fact(number: &Num, reasons: &[&str], anchor: &Value, related: &[String]) -> Value {
    match number {
        Num::Exact(NumLit::Int(value)) => json!({ "status": "exact", "value": value }),
        Num::Exact(NumLit::Float(value)) if value.is_finite() => {
            json!({ "status": "exact", "value": value })
        }
        Num::Interval { lo, hi } => {
            let lo = lo.filter(|value| value.is_finite());
            let hi = hi.filter(|value| value.is_finite());
            if lo.is_none() && hi.is_none() {
                return unknown_status_many(reasons, Some(anchor), Some(related));
            }
            let mut fact = Map::new();
            fact.insert("status".to_owned(), Value::String("interval".to_owned()));
            if let Some(lo) = lo {
                fact.insert("lower_inclusive".to_owned(), json!(lo));
            }
            if let Some(hi) = hi {
                fact.insert("upper_inclusive".to_owned(), json!(hi));
            }
            Value::Object(fact)
        }
        Num::Symbol(name) => json!({ "status": "symbolic", "name": name }),
        Num::Exact(NumLit::Float(_)) | Num::Unknown => {
            unknown_status_many(reasons, Some(anchor), Some(related))
        }
    }
}

fn unknown_status(kind: &str, anchor: Option<Value>, related: Option<Vec<String>>) -> Value {
    json!({
        "status": "unknown",
        "reasons": [reason_value(kind, anchor, related, None)],
    })
}

fn unknown_status_many(
    kinds: &[&str],
    anchor: Option<&Value>,
    related: Option<&[String]>,
) -> Value {
    let reasons: Vec<Value> = kinds
        .iter()
        .map(|kind| reason_value(kind, anchor.cloned(), related.map(<[String]>::to_vec), None))
        .collect();
    json!({
        "status": "unknown",
        "reasons": reasons,
    })
}

fn reason_value(
    kind: &str,
    anchor: Option<Value>,
    related: Option<Vec<String>>,
    detail: Option<String>,
) -> Value {
    let mut reason = Map::new();
    reason.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(anchor) = anchor {
        reason.insert("anchor".to_owned(), anchor);
    }
    if let Some(mut related) = related {
        related.sort();
        related.dedup();
        reason.insert("related_entity_ids".to_owned(), json!(related));
    }
    if let Some(detail) = detail {
        reason.insert("detail".to_owned(), Value::String(detail));
    }
    Value::Object(reason)
}

fn unknown_candidates(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidates": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_object_candidates(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidates": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_entity_candidates(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidates": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_play_candidates(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidates": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_channels<T: serde::Serialize>(
    channels: T,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    json!({
        "status": "unknown",
        "known_channels": channels,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the helper takes ownership of a completed public membership record"
)]
fn unknown_membership(
    entries: Vec<Value>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    json!({
        "status": "unknown",
        "entries": entries,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_order(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidate_object_ids": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), None)],
    })
}

fn unknown_order_with_detail(
    mut candidates: Vec<String>,
    kind: &str,
    anchor: Value,
    related: Vec<String>,
    detail: String,
) -> Value {
    candidates.sort();
    candidates.dedup();
    json!({
        "status": "unknown",
        "candidate_object_ids": candidates,
        "reasons": [reason_value(kind, Some(anchor), Some(related), Some(detail))],
    })
}

fn channel_name(channel: WriteChannel) -> &'static str {
    match channel {
        WriteChannel::Points => "points",
        WriteChannel::Style => "style",
        WriteChannel::Opacity => "opacity",
        WriteChannel::Membership => "membership",
        WriteChannel::CameraState => "camera-state",
    }
}

fn order_reason_kind(reason: &OrderUnknownReason) -> &'static str {
    match reason {
        OrderUnknownReason::ConstructorStateUnknown
        | OrderUnknownReason::SceneStateUnavailable
        | OrderUnknownReason::MissingMemberFacts { .. } => "unavailable-definition",
        OrderUnknownReason::AggregateMember { .. } => "unknown-cardinality",
        OrderUnknownReason::FamilyCycle { .. } => "unsupported-semantics",
        OrderUnknownReason::RootOrderUnknown
        | OrderUnknownReason::ForegroundOrderUnknown
        | OrderUnknownReason::ForegroundMembershipUnknown { .. }
        | OrderUnknownReason::ChildrenOrderUnknown { .. }
        | OrderUnknownReason::ZIndexUnknown { .. }
        | OrderUnknownReason::CleanupObscuresOrder => "unknown-render-order",
    }
}

fn certainty_name(certain: bool) -> &'static str {
    if certain { "certain" } else { "possible" }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "risk insertion owns the anchor that becomes part of the public record"
)]
fn insert_risk(
    risks: &mut BTreeMap<String, Value>,
    snapshot_id: &str,
    kind: &str,
    certainty: &str,
    anchor: Value,
    mut related: Vec<String>,
    profiles: &[String],
) {
    related.sort();
    related.dedup();
    let id = public_id(
        "risk",
        vec![
            Value::String(snapshot_id.to_owned()),
            Value::String(kind.to_owned()),
            anchor_preimage(&anchor),
            json!(related),
        ],
    );
    risks.entry(id.clone()).or_insert_with(|| {
        json!({
            "id": id,
            "kind": kind,
            "certainty": certainty,
            "anchor": anchor,
            "related_entity_ids": related,
            "applicable_profiles": profiles,
        })
    });
}

fn insert_frontier(
    frontiers: &mut BTreeMap<String, Value>,
    snapshot_id: &str,
    kind: &str,
    anchor: Option<Value>,
    mut related: Vec<String>,
    mut reasons: Vec<Value>,
) {
    related.sort();
    related.dedup();
    reasons.sort_by_key(|reason| serde_json::to_string(reason).unwrap_or_default());
    reasons.dedup();
    let anchor_preimage = anchor.as_ref().map_or(Value::Null, anchor_preimage);
    let id = public_id(
        "frontier",
        vec![
            Value::String(snapshot_id.to_owned()),
            Value::String(kind.to_owned()),
            anchor_preimage,
            json!(related),
        ],
    );
    if let Some(existing) = frontiers.get_mut(&id) {
        let existing_reasons = existing["reasons"]
            .as_array_mut()
            .expect("frontier reasons are an array");
        existing_reasons.extend(reasons);
        existing_reasons.sort_by_key(|reason| serde_json::to_string(reason).unwrap_or_default());
        existing_reasons.dedup();
        return;
    }
    let mut frontier = Map::new();
    frontier.insert("id".to_owned(), Value::String(id.clone()));
    frontier.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(anchor) = anchor {
        frontier.insert("anchor".to_owned(), anchor);
    }
    frontier.insert("related_entity_ids".to_owned(), json!(related));
    frontier.insert("reasons".to_owned(), json!(reasons));
    frontiers.insert(id, Value::Object(frontier));
}

fn collect_unknown_frontiers(
    value: &Value,
    inherited_entity_id: Option<&str>,
    frontiers: &mut BTreeMap<String, Value>,
    snapshot_id: &str,
) {
    match value {
        Value::Object(object) => {
            let entity_id = object
                .get("id")
                .and_then(Value::as_str)
                .or(inherited_entity_id);
            if object.get("status").and_then(Value::as_str) == Some("unknown") {
                let reasons = object
                    .get("reasons")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if !reasons.is_empty() {
                    let anchor = reasons
                        .iter()
                        .find_map(|reason| reason.get("anchor").cloned());
                    let mut related: Vec<String> = reasons
                        .iter()
                        .filter_map(|reason| reason.get("related_entity_ids"))
                        .filter_map(Value::as_array)
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    if let Some(entity_id) = entity_id {
                        related.push(entity_id.to_owned());
                    }
                    let kind = reasons
                        .first()
                        .and_then(|reason| reason.get("kind"))
                        .and_then(Value::as_str)
                        .map_or("unsupported-semantics", frontier_kind_for_reason);
                    insert_frontier(frontiers, snapshot_id, kind, anchor, related, reasons);
                }
            }
            for child in object.values() {
                collect_unknown_frontiers(child, entity_id, frontiers, snapshot_id);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_unknown_frontiers(child, inherited_entity_id, frontiers, snapshot_id);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn frontier_kind_for_reason(reason: &str) -> &'static str {
    match reason {
        "decode-error" | "parse-error" | "unsupported-syntax" => "source",
        "unresolved-name" | "unresolved-import" | "dynamic-attribute" => "name-resolution",
        "dynamic-call-target" | "star-arguments" => "call-resolution",
        "recursive-summary" | "inline-depth-cap" | "unavailable-definition" => "helper-expansion",
        "branch-join" => "control-flow",
        "loop-widening" | "unknown-cardinality" => "cardinality",
        "unknown-animation-target" => "animation-target",
        "unknown-write-channel" => "write-channel",
        "unknown-render-order" => "render-order",
        _ => "unsupported-semantics",
    }
}

fn call_names(call: &QualifiedCall) -> Vec<String> {
    let mut names: Vec<String> = call.candidates.iter().cloned().collect();
    if names.is_empty() {
        if let Some(dotted) = &call.callee_dotted {
            names.push(dotted.join("."));
        }
    }
    names
}

fn is_dynamic_call(call: &QualifiedCall) -> bool {
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

fn is_camera_call(target: &str) -> bool {
    [
        "begin_ambient_camera_rotation",
        "move_camera",
        "set_camera_orientation",
        "stop_ambient_camera_rotation",
    ]
    .iter()
    .any(|name| target.ends_with(name))
}

fn is_external_call(target: &str) -> bool {
    target == "open"
        || target.starts_with("datetime.")
        || target.starts_with("http.")
        || target.starts_with("os.")
        || target.starts_with("pathlib.Path.")
        || target.starts_with("requests.")
        || target.starts_with("socket.")
        || target.starts_with("subprocess.")
        || target.starts_with("time.")
        || target.starts_with("urllib.request.")
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "`.seed` is a Python method suffix, not a filesystem extension"
)]
fn is_random_call(target: &str) -> bool {
    (target.starts_with("random.") && target != "random.seed")
        || (target.starts_with("numpy.random.") && !target.ends_with(".seed"))
        || target.ends_with(".random_color")
        || target.ends_with(".random_bright_color")
}

fn sort_by_id(values: &mut [Value]) {
    values.sort_by(|a, b| string_field(a, "id").cmp(string_field(b, "id")));
}

fn string_field<'a>(value: &'a Value, field: &str) -> &'a str {
    value[field].as_str().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_ids_are_domain_separated_and_stable() {
        let fields = vec![json!("snapshot"), json!(["path.py", 1, 2])];
        let first = public_id("object", fields.clone());
        assert_eq!(first, public_id("object", fields.clone()));
        assert_ne!(first, public_id("play", fields));
        assert!(first.starts_with("object:sf0:"));
        assert_eq!(first.len(), "object:sf0:".len() + 64);
    }

    #[test]
    fn semantic_config_hash_ignores_rule_selection() {
        let mut first = ResolvedConfig {
            project_root: ".".into(),
            manim_version: "0.20".to_owned(),
            declared_manim_version: None,
            target_python: "3.11".to_owned(),
            select: vec!["MLC".to_owned()],
            ignore: Vec::new(),
            min_confidence: crate::diagnostic::Confidence::High,
            fail_level: crate::diagnostic::Severity::Warning,
            knowledge_profile: None,
            respect_manim_cfg: true,
            exclude: Vec::new(),
            per_file_ignores: BTreeMap::new(),
            source_roots: vec![".".to_owned()],
            stub_paths: Vec::new(),
            default_profile: "default".to_owned(),
            all_profile_names: vec!["default".to_owned()],
            active_profiles: vec![crate::config::model::RenderProfile {
                name: "default".to_owned(),
                renderer: crate::config::model::Renderer::Cairo,
                platform: crate::config::model::Platform::Linux,
                working_directory: ".".to_owned(),
                pixel_width: 1920,
                pixel_height: 1080,
                frame_rate: 60.0,
                assets_dir: ".".to_owned(),
                allowed_fonts: Vec::new(),
                cairo_fork_workers: 0,
                cairo_static_layers: false,
                video_encoder: "libx264".to_owned(),
                transparent: false,
                antialias: "default".to_owned(),
                opengl_readback: "auto".to_owned(),
            }],
        };
        let expected = semantic_config_hash(&first);
        first.select = vec!["MLP225".to_owned()];
        first.ignore = vec!["MLC".to_owned()];
        first.min_confidence = crate::diagnostic::Confidence::Low;
        assert_eq!(expected, semantic_config_hash(&first));
    }
}
