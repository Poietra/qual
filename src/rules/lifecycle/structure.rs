//! Scene / family structure rules: `MLC110`, `MLC115`, `MLC119`.
//!
//! All three read the lifecycle event trace (DESIGN §5.6) and the
//! interpreter's membership model of DESIGN §3.4: `Mobject.add` rejects
//! self-addition and family traversal loops forever on a parent cycle
//! (`MLC110`); `Scene.remove(child)` restructures only the scene root list,
//! so re-adding a surviving parent makes the child reappear (`MLC115`); and
//! `Scene.replace(old, new)` raises when `old` is not in the scene family
//! (`MLC119`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::index::{ArgShape, QualifiedCall, ReceiverKind};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{SceneLifecycle, StateSnapshot, TracedEvent};
use crate::semantic::values::{AllocationSite, Cardinality, KindSet, ObjectId, Presence};

use super::support::{
    SCENE_REPLACE, build_diagnostic, candidates_value, classify_class, conclusive_target,
    receiver_conclusive, site_range,
};

/// The four lifecycle methods whose statement order matches the snapshot
/// order of a scene run.
const LIFECYCLE_METHODS: [&str; 4] = ["__init__", "setup", "construct", "tear_down"];

// ---------------------------------------------------------------------------
// MLC110: self-addition and proven parent cycles.
// ---------------------------------------------------------------------------

/// Metadata for [`SelfOrCyclicChild`].
pub const MLC110: RuleMetadata = RuleMetadata {
    id: "MLC110",
    summary: "Mobject.add(self) or a proven parent cycle",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `mob.add(mob)` (runtime `ValueError`) or an `add` that provably closes
/// a parent cycle (infinite family recursion) — DESIGN §7.1 `MLC110`.
pub struct SelfOrCyclicChild;

/// Whether `to` is reachable from `from` over the definite parent→child
/// edges.
fn reaches(edges: &BTreeMap<ObjectId, BTreeSet<ObjectId>>, from: &ObjectId, to: &ObjectId) -> bool {
    let mut queue = vec![from.clone()];
    let mut visited = BTreeSet::new();
    while let Some(current) = queue.pop() {
        if &current == to {
            return true;
        }
        if !visited.insert(current.clone()) {
            continue;
        }
        if let Some(children) = edges.get(&current) {
            queue.extend(children.iter().cloned());
        }
    }
    false
}

/// Drops every definite edge incident to `object` (its membership was
/// mutated in a way this walk does not model).
fn purge_edges(edges: &mut BTreeMap<ObjectId, BTreeSet<ObjectId>>, object: &ObjectId) {
    edges.remove(object);
    for children in edges.values_mut() {
        children.remove(object);
    }
}

impl Rule for SelfOrCyclicChild {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC110
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            let mut edges: BTreeMap<ObjectId, BTreeSet<ObjectId>> = BTreeMap::new();
            for traced in &scene.events {
                match &traced.event {
                    Event::AddChild(add) => {
                        if add.parent == add.child {
                            if seen.insert(traced.site) {
                                diagnostics.push(self_add_diagnostic(context, scene, traced));
                            }
                            continue;
                        }
                        let definite = traced.certainty == Presence::Present
                            && add.parent.cardinality == Cardinality::Singleton
                            && add.child.cardinality == Cardinality::Singleton;
                        if !definite {
                            continue;
                        }
                        if reaches(&edges, &add.child, &add.parent) {
                            if seen.insert(traced.site) {
                                diagnostics.push(cycle_diagnostic(context, scene, traced));
                            }
                            continue;
                        }
                        edges
                            .entry(add.parent.clone())
                            .or_default()
                            .insert(add.child.clone());
                    }
                    Event::Mutate(mutate) if mutate.kind == MutationKind::Membership => {
                        purge_edges(&mut edges, &mutate.target);
                    }
                    Event::UnknownMutation(unknown) => {
                        for value in &unknown.values {
                            purge_edges(&mut edges, value);
                        }
                    }
                    _ => {}
                }
            }
        }
        diagnostics
    }
}

fn self_add_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    traced: &TracedEvent,
) -> Diagnostic {
    let file = context.sources().file(traced.site.file);
    let range = site_range(&traced.site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert("kind".to_owned(), Value::String("self-add".to_owned()));
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    build_diagnostic(
        &MLC110,
        context,
        file,
        range,
        format!(
            "`{text}` adds a mobject to itself; `Mobject.add` rejects direct \
             self-addition with `ValueError` at run time."
        ),
        "`Mobject.add` validates its children and raises `ValueError` when a mobject \
         is added as its own submobject (mobject.py `Mobject.add`, DESIGN §3.4)."
            .to_owned(),
        evidence,
    )
}

fn cycle_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    traced: &TracedEvent,
) -> Diagnostic {
    let file = context.sources().file(traced.site.file);
    let range = site_range(&traced.site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert("kind".to_owned(), Value::String("parent-cycle".to_owned()));
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    build_diagnostic(
        &MLC110,
        context,
        file,
        range,
        format!(
            "`{text}` closes a parent cycle: the child already contains this parent \
             through its submobjects, so family traversal recurses forever at \
             render time."
        ),
        "Manim expands the mobject family recursively through `submobjects` \
         (mobject.py `get_family`); a cycle in the parent/child graph makes that \
         traversal (and rendering) recurse without end (DESIGN §3.4)."
            .to_owned(),
        evidence,
    )
}

// ---------------------------------------------------------------------------
// MLC115: removed child reappears through a surviving parent.
// ---------------------------------------------------------------------------

/// Metadata for [`RemovedChildReappears`].
pub const MLC115: RuleMetadata = RuleMetadata {
    id: "MLC115",
    summary: "Scene.remove(child) is undone by re-adding the surviving parent",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `Scene.remove(child)` whose child stays in a parent's `submobjects`,
/// followed by a definite re-add of that parent (DESIGN §3.4, `MLC115`).
pub struct RemovedChildReappears;

impl Rule for RemovedChildReappears {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC115
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for removal in &scene.scene_removals {
                if removal.certainty != Presence::Present
                    || removal.removed.cardinality != Cardinality::Singleton
                {
                    continue;
                }
                let Some(removal_index) = scene.events.iter().position(|traced| {
                    traced.site == removal.site && matches!(traced.event, Event::SceneRemove(_))
                }) else {
                    continue;
                };
                for parent in &removal.surviving_parents {
                    if parent.cardinality != Cardinality::Singleton {
                        continue;
                    }
                    let Some(readd) =
                        first_definite_readd(scene, removal_index, parent, &removal.removed)
                    else {
                        continue;
                    };
                    if !seen.insert((removal.site, readd.site)) {
                        continue;
                    }
                    diagnostics.push(reappearance_diagnostic(context, scene, removal, readd));
                }
            }
        }
        diagnostics
    }
}

/// The first event after `removal_index` that definitely re-adds `parent`
/// to the scene while the parent link is still intact. Any membership
/// mutation of the parent (e.g. `parent.remove(child)`) or an explicit
/// re-add of the child ends the search.
fn first_definite_readd<'a>(
    scene: &'a SceneLifecycle,
    removal_index: usize,
    parent: &ObjectId,
    removed: &ObjectId,
) -> Option<&'a TracedEvent> {
    for traced in &scene.events[removal_index + 1..] {
        match &traced.event {
            Event::Mutate(mutate)
                if mutate.kind == MutationKind::Membership
                    && (mutate.target.definitely_same(parent)
                        || mutate.target.definitely_same(removed)) =>
            {
                return None;
            }
            Event::UnknownMutation(unknown)
                if unknown
                    .values
                    .iter()
                    .any(|value| value.definitely_same(parent)) =>
            {
                return None;
            }
            Event::SceneAdd(add) => {
                if add
                    .objects
                    .iter()
                    .any(|object| object.definitely_same(removed))
                {
                    // The child was brought back explicitly.
                    return None;
                }
                if add
                    .objects
                    .iter()
                    .any(|object| object.definitely_same(parent))
                {
                    if traced.certainty == Presence::Present {
                        return Some(traced);
                    }
                    // A branch-dependent re-add is not a definite
                    // reappearance: stay silent.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

fn reappearance_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    removal: &crate::semantic::interpreter::SceneRemovalFact,
    readd: &TracedEvent,
) -> Diagnostic {
    let file = context.sources().file(removal.site.file);
    let readd_file = context.sources().file(readd.site.file);
    let range = site_range(&removal.site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    evidence.insert(
        "surviving_parents".to_owned(),
        Value::Number(removal.surviving_parents.len().into()),
    );
    let mut diagnostic = build_diagnostic(
        &MLC115,
        context,
        file,
        range,
        format!(
            "`{text}` removes the child from the scene root list only; its parent \
             still holds it in `submobjects` and is re-added (or animated) later, so \
             the child reappears. Remove it from the parent too \
             (`parent.remove(child)`) if the removal should be permanent."
        ),
        "`Scene.remove(child)` restructures the scene's root list: a parent group \
         containing the child dissolves into its other children, but the parent's \
         own `submobjects` list is not edited (scene.py `restructure_mobjects`, \
         DESIGN §3.4). Re-adding or animating the parent later shows the whole \
         family again — including the removed child."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: readd_file.relative_path().to_owned(),
        span: readd_file.span_of_range(site_range(&readd.site)),
        message: "the surviving parent returns to the scene here, and the removed \
                  child with it"
            .to_owned(),
    });
    diagnostic
}

// ---------------------------------------------------------------------------
// MLC119: Scene.replace with old definitely outside the scene family.
// ---------------------------------------------------------------------------

/// Metadata for [`ReplaceMissingOld`].
pub const MLC119: RuleMetadata = RuleMetadata {
    id: "MLC119",
    summary: "Scene.replace(old, new) with old definitely not in the scene",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

/// `self.replace(old, new)` where `old` is provably absent from the scene
/// family at the call point (DESIGN §7.1 `MLC119`).
pub struct ReplaceMissingOld;

impl Rule for ReplaceMissingOld {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC119
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let mut diagnostics = Vec::new();
        for call in &calls.calls {
            if call.receiver != ReceiverKind::SelfScene || call.has_star_args {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != SCENE_REPLACE {
                continue;
            }
            let Some(argument) = call.positional(0) else {
                continue;
            };
            let verdict = match &argument.shape {
                ArgShape::Call(inner_index) => {
                    fresh_construction_verdict(context, &calls.calls[*inner_index])
                }
                ArgShape::Name => absent_by_kind_verdict(context, call, argument),
                _ => None,
            };
            let Some(reason) = verdict else {
                continue;
            };
            let file = context.sources().file(call.file);
            let text = file.slice(argument.range);
            let mut evidence = BTreeMap::new();
            evidence.insert(
                "resolved".to_owned(),
                Value::String(SCENE_REPLACE.to_owned()),
            );
            evidence.insert("candidates".to_owned(), candidates_value(call));
            evidence.insert("proof".to_owned(), Value::String(reason.to_owned()));
            diagnostics.push(build_diagnostic(
                &MLC119,
                context,
                file,
                argument.range,
                format!(
                    "`{text}` is definitely not in the scene family at this point; \
                     `Scene.replace(old, new)` requires `old` to be present (directly \
                     or inside a parent) and raises `ValueError` otherwise."
                ),
                "`Scene.replace` swaps `old` for `new` in place, preserving draw \
                 order; it validates that `old` is currently part of the scene \
                 family (scene.py `Scene.replace`). Add the mobject first, or use \
                 `Scene.add` if the old object was never shown."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

/// A freshly constructed mobject can never already be in the scene.
fn fresh_construction_verdict(
    context: &RuleContext<'_>,
    inner: &QualifiedCall,
) -> Option<&'static str> {
    let profile = context.knowledge()?;
    let index = context.project_index();
    if inner.candidates.is_empty() || !receiver_conclusive(inner, index) {
        return None;
    }
    let all_constructors = inner.candidates.iter().all(|candidate| {
        classify_class(profile, index, candidate)
            .is_some_and(super::support::ClassFact::conclusive_mobject)
    });
    all_constructors.then_some("fresh-construction")
}

/// `old` is a name whose every possible tracked instance is definitely
/// outside the scene family in the pre-call snapshot.
fn absent_by_kind_verdict(
    context: &RuleContext<'_>,
    call: &QualifiedCall,
    argument: &crate::frontend::index::CallArgument,
) -> Option<&'static str> {
    if argument.kind_candidates.is_empty() {
        return None;
    }
    let class_name = call.context.class_name.as_deref()?;
    let function = call.context.function.as_deref()?;
    if !LIFECYCLE_METHODS.contains(&function) {
        return None;
    }
    let scene = context.lifecycle_facts().scene(class_name)?;
    if scene.constructor_state_unknown {
        return None;
    }
    let method_range = *context
        .project_index()
        .classes
        .get(class_name)?
        .methods
        .get(function)?;
    let snapshot = pre_call_snapshot(scene, call, method_range)?;

    let mut known_matches = 0usize;
    for state in snapshot.heap.objects.values() {
        let matches = argument
            .kind_candidates
            .iter()
            .any(|candidate| state.kind.may_be(candidate));
        if !matches {
            continue;
        }
        if state.family_membership != Presence::Absent {
            // Some possible referent may be in the scene: no certain claim.
            return None;
        }
        if matches!(state.kind, KindSet::Known(_)) {
            known_matches += 1;
        }
    }
    (known_matches > 0).then_some("all-candidates-absent")
}

/// The last statement snapshot before the call, restricted to the same
/// lifecycle method body so program order and byte order agree.
fn pre_call_snapshot<'a>(
    scene: &'a SceneLifecycle,
    call: &QualifiedCall,
    method_range: rustpython_parser::text_size::TextRange,
) -> Option<&'a StateSnapshot> {
    let call_start: u32 = call.call_range.start().into();
    scene.snapshots.iter().rfind(|snapshot| {
        snapshot.site.file == call.file
            && snapshot.site.end <= call_start
            && snapshot.site.start >= u32::from(method_range.start())
            && snapshot.site.end <= u32::from(method_range.end())
    })
}
