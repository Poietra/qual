//! Primitive heap and scene-membership operations of the machine
//! (DESIGN §3.4, §5.5): allocation, scene add / remove / replace with
//! root-list restructuring, family recomputation, parent / child edges,
//! generated targets and saved state, mutator classification, z-index and
//! path-geometry tracking (MLR116 / MLP209), and `.animate` builder facts.

use std::collections::BTreeSet;

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::frontend::index::QualifiedCall;
use crate::semantic::events::MutationKind;
use crate::semantic::state::{GeneratedTarget, MobjectState, SceneState, WriteChannel};
use crate::semantic::values::{
    AllocationSite, CopyKind, CopyOf, KindSet, Num, ObjectId, Presence, Truth, Visibility,
};

use super::exec::{AbstractValue, BuilderValue, ExecState, Machine, OpKind, literal_num};
use super::{AnimateBuilderFact, SceneRemovalFact};

// ---------------------------------------------------------------------------
// Path-state curation (MLR116; verified against
// manim/mobject/types/vectorized_mobject.py and manim/mobject/mobject.py,
// read-only).
// ---------------------------------------------------------------------------

/// Canonical id of `VMobject` / `Mobject`.
pub(super) const VMOBJECT_ID: &str = "manim.mobject.types.vectorized_mobject.VMobject";
pub(super) const MOBJECT_ID: &str = "manim.mobject.mobject.Mobject";

/// Classes whose constructor provably leaves the *own* path empty:
/// `Mobject.__init__` runs `reset_points()` and the base `generate_points()`
/// is a no-op, and none of these override it (verified in `mobject.py` /
/// `vectorized_mobject.py`).
const EMPTY_PATH_CONSTRUCTORS: &[&str] = &[
    MOBJECT_ID,
    "manim.mobject.mobject.Group",
    VMOBJECT_ID,
    "manim.mobject.types.vectorized_mobject.VGroup",
];

/// Classes whose `generate_points` override provably creates a non-empty
/// own path (Arc / Line / Polygram families; verified in geometry/*.py).
const NONEMPTY_PATH_CONSTRUCTORS: &[&str] = &[
    "manim.mobject.geometry.polygram.Square",
    "manim.mobject.geometry.polygram.Rectangle",
    "manim.mobject.geometry.arc.Circle",
    "manim.mobject.geometry.arc.Dot",
    "manim.mobject.geometry.line.Line",
];

/// Initial (point, curve, subpath) counts a curated constructor proves.
/// `None` when the candidate set proves neither emptiness nor
/// non-emptiness — counts then stay `Unknown` (project subclasses always
/// land here: their `__init__` may build arbitrary paths).
fn initial_path_counts(kind: &KindSet) -> Option<(Num, Num, Num)> {
    let KindSet::Known(candidates) = kind else {
        return None;
    };
    if candidates.is_empty() {
        return None;
    }
    if candidates
        .iter()
        .all(|candidate| EMPTY_PATH_CONSTRUCTORS.contains(&candidate.as_str()))
    {
        return Some((Num::int(0), Num::int(0), Num::int(0)));
    }
    if candidates
        .iter()
        .all(|candidate| NONEMPTY_PATH_CONSTRUCTORS.contains(&candidate.as_str()))
    {
        let at_least_one = || Num::Interval {
            lo: Some(1.0),
            hi: None,
        };
        return Some((at_least_one(), at_least_one(), at_least_one()));
    }
    None
}

/// Seeds the path-count facts of a freshly constructed object. The
/// `n_points_per_cubic_curve` fact is only exact when the call fact proves
/// the default was used; the counts themselves are constructor facts and
/// hold regardless.
pub(super) fn seed_path_counts(
    object: &mut MobjectState,
    kind: &KindSet,
    fact: Option<&QualifiedCall>,
) {
    let Some((points, curves, subpaths)) = initial_path_counts(kind) else {
        return;
    };
    object.point_count = points;
    object.curve_count = curves;
    object.subpath_count = subpaths;
    let default_nppc = fact.is_some_and(|fact| {
        !fact.has_star_star_kwargs && fact.keyword("n_points_per_cubic_curve").is_none()
    });
    object.points_per_curve = if default_nppc {
        Num::int(4)
    } else {
        Num::Unknown
    };
}

/// Copies duplicate the original's geometry: the path-count facts carry
/// over to `copy()` / `generate_target()` / animation starting copies.
/// `Mobject.copy` is a deepcopy, so the `z_index` fact carries over too
/// (mobject.py `Mobject.copy`).
pub(super) fn clone_path_facts(copy: &mut MobjectState, original: &MobjectState) {
    copy.point_count = original.point_count.clone();
    copy.curve_count = original.curve_count.clone();
    copy.subpath_count = original.subpath_count.clone();
    copy.points_per_curve = original.points_per_curve.clone();
    copy.z_index = original.z_index.clone();
}

/// `target` plus its transitive (may-)children, alias-resolved and
/// cycle-safe — the family `set_z_index(family=True)` writes.
fn z_family_closure(state: &ExecState, target: &ObjectId) -> Vec<ObjectId> {
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut queue = vec![state.heap.resolve(target)];
    let mut closure = Vec::new();
    while let Some(current) = queue.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(object) = state.heap.object(&current) {
            for child in &object.children {
                queue.push(state.heap.resolve(child));
            }
        }
        closure.push(current);
    }
    closure
}

/// Widens the tracked `z_index` of `target` and every transitive
/// (may-)child to `Unknown`: an effect that may write `z_index` reached
/// them (`set_z_index` writes the whole family by default), so no exact
/// display-order claim survives (DESIGN §15).
pub(super) fn widen_z_index_family(state: &mut ExecState, target: &ObjectId) {
    for member in z_family_closure(state, target) {
        if let Some(object) = state.heap.object_mut(&member) {
            object.z_index = Num::Unknown;
        }
    }
}

/// Applies a tracked `set_z_index(value, family=...)` write
/// (mobject.py `Mobject.set_z_index`): the receiver takes the value
/// exactly on all-paths calls (hull otherwise); unless `family` is a
/// literal `False`, every transitive child joins the value in — child
/// edges are may-relations, so the exact value is never asserted for
/// them.
pub(super) fn apply_z_index_write(
    state: &mut ExecState,
    target: &ObjectId,
    value: &Num,
    family: Truth,
    certainty: Presence,
) {
    let receiver = state.heap.resolve(target);
    if let Some(object) = state.heap.object_mut(&receiver) {
        object.z_index = if certainty == Presence::Present {
            value.clone()
        } else {
            object.z_index.join(value)
        };
    }
    if family == Truth::No {
        return;
    }
    for member in z_family_closure(state, &receiver) {
        if member == receiver {
            continue;
        }
        if let Some(object) = state.heap.object_mut(&member) {
            object.z_index = object.z_index.join(value);
        }
    }
}

/// A literal (possibly `-`-negated) int / float expression.
pub(super) fn literal_signed_num(expr: &ast::Expr) -> Option<Num> {
    use crate::semantic::values::NumLit;
    match expr {
        ast::Expr::Constant(constant) => match &constant.value {
            ast::Constant::Int(value) => i64::try_from(value).ok().map(Num::int),
            ast::Constant::Float(value) => Some(Num::float(*value)),
            _ => None,
        },
        ast::Expr::UnaryOp(unary) if matches!(unary.op, ast::UnaryOp::USub) => {
            match literal_signed_num(&unary.operand)? {
                Num::Exact(NumLit::Int(value)) => Some(Num::int(-value)),
                Num::Exact(NumLit::Float(value)) => Some(Num::float(-value)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The `family` argument of a `set_z_index` call (mobject.py declares
/// `family: bool = True`): the default is a definite `Yes`, a literal is
/// exact, and anything non-literal (including a possible `**kwargs`
/// supply) is `Maybe`.
pub(super) fn set_z_index_family_arg(call: &ast::ExprCall) -> Truth {
    let explicit = call
        .keywords
        .iter()
        .find(|keyword| {
            keyword
                .arg
                .as_ref()
                .is_some_and(|name| name.as_str() == "family")
        })
        .map(|keyword| &keyword.value)
        .or_else(|| call.args.get(1));
    match explicit {
        Some(ast::Expr::Constant(constant)) => match &constant.value {
            ast::Constant::Bool(value) => Truth::from(*value),
            _ => Truth::Maybe,
        },
        Some(_) => Truth::Maybe,
        // A `**kwargs` splat may still supply `family=False`; the
        // conservative direction here is the *family* write, so `Maybe`
        // (join, never exact) is sound either way.
        None if call.keywords.iter().any(|keyword| keyword.arg.is_none()) => Truth::Maybe,
        None => Truth::Yes,
    }
}

/// Seeds the `z_index` fact of a freshly constructed curated mobject
/// (DESIGN §3.4 / MLP209 z tracking).
///
/// `Mobject.__init__` declares `z_index: float = 0` (verified in
/// mobject.py), so a curated constructor call with no `z_index` kwarg and
/// no `**kwargs` splat *proves* `z == 0`. A literal kwarg is exact; a
/// non-literal kwarg or a `**kwargs` splat leaves it `Unknown` — never a
/// guessed default (DESIGN §15).
pub(super) fn seed_z_index(object: &mut MobjectState, fact: Option<&QualifiedCall>) {
    let Some(fact) = fact else {
        return;
    };
    object.z_index = if let Some(argument) = fact.keyword("z_index") {
        literal_num(argument).unwrap_or(Num::Unknown)
    } else if fact.has_star_star_kwargs {
        Num::Unknown
    } else {
        Num::int(0)
    };
}

/// Element count of a literal list / tuple display (each element is one
/// point-like for the `set_points` family). `None` for anything else,
/// including displays with starred elements.
fn display_element_count(expr: &ast::Expr) -> Option<i64> {
    let elements = match expr {
        ast::Expr::List(list) => &list.elts,
        ast::Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    if elements
        .iter()
        .any(|element| matches!(element, ast::Expr::Starred(_)))
    {
        return None;
    }
    i64::try_from(elements.len()).ok()
}

/// A literal non-negative integer argument (`resize_points(6)`).
fn literal_int_arg(expr: &ast::Expr) -> Option<i64> {
    let ast::Expr::Constant(constant) = expr else {
        return None;
    };
    let ast::Constant::Int(value) = &constant.value else {
        return None;
    };
    i64::try_from(value).ok()
}

// ---------------------------------------------------------------------------
// Channel classification (curated-in-code; unknowns stay unknown).
// ---------------------------------------------------------------------------

/// Write channels of a curated fluent mutator, by canonical method id.
/// `None` means unclassified (callers must not treat it as "writes
/// nothing").
pub(super) fn mutator_channels(canonical_method_id: &str) -> Option<BTreeSet<WriteChannel>> {
    let method = canonical_method_id.rsplit('.').next()?;
    let channels: &[WriteChannel] = match method {
        "shift" | "move_to" | "next_to" | "to_edge" | "rotate" | "scale" | "flip" | "center"
        | "align_to" | "stretch" => &[WriteChannel::Points],
        "set_color" | "set_fill" | "set_stroke" => &[WriteChannel::Style],
        "set_opacity" => &[WriteChannel::Opacity],
        "become" => &[WriteChannel::Points, WriteChannel::Style],
        _ => return None,
    };
    Some(channels.iter().copied().collect())
}

impl<'a> Machine<'a, '_> {
    // -- .animate builders --------------------------------------------------

    pub(super) fn make_builder(
        &mut self,
        target: &ObjectId,
        range: TextRange,
        state: &mut ExecState,
    ) -> AbstractValue {
        let site = self.site(range);
        let call_path = self.inline_call_path();
        // Builder creation runs `generate_target()` immediately
        // (DESIGN §3.2).
        self.generate_target(target, site, self.certainty(), state);
        let epoch = state
            .heap
            .object(target)
            .map_or(0, |object| object.mutation_epoch);
        if self.emit {
            // A previous un-played builder on the same target is now
            // overwritten (its `mobject.target` was replaced). The check
            // runs over the per-execution facts, so each execution's own
            // target decides — one helper's builder on `a` never marks
            // (or misses) another execution's builder on `b`.
            let overwrite = if self.certainty() == Presence::Present {
                Truth::Yes
            } else {
                Truth::Maybe
            };
            for ((fact_site, _path), fact) in &mut self.sink.builder_executions {
                if *fact_site != site
                    && fact.target.as_ref() == Some(target)
                    && fact.played != Truth::Yes
                {
                    fact.overwritten_by_later_builder =
                        if fact.overwritten_by_later_builder == Truth::No {
                            overwrite
                        } else {
                            fact.overwritten_by_later_builder.join(overwrite)
                        };
                }
            }
            self.sink
                .builder_executions
                .entry((site, call_path.clone()))
                .or_insert_with(|| AnimateBuilderFact {
                    site,
                    target: Some(target.clone()),
                    methods: Vec::new(),
                    channels: BTreeSet::new(),
                    channels_known: Truth::Yes,
                    target_epoch_at_creation: epoch,
                    target_epoch_at_play: None,
                    played: Truth::No,
                    overwritten_by_later_builder: Truth::No,
                });
            self.sink.sync_builders();
        }
        AbstractValue::Builder(BuilderValue {
            site,
            call_path,
            target: Some(target.clone()),
            methods: Vec::new(),
            channels: BTreeSet::new(),
            channels_known: Truth::Yes,
        })
    }

    pub(super) fn builder_chain(
        &mut self,
        mut builder: BuilderValue,
        method: &str,
        call: &'a ast::ExprCall,
        state: &mut ExecState,
    ) -> AbstractValue {
        for arg in &call.args {
            self.eval_expr(arg, state);
        }
        for keyword in &call.keywords {
            self.eval_expr(&keyword.value, state);
        }
        if method == "animate" {
            // `mob.animate(run_time=...)`: kwargs application, same
            // builder.
            return AbstractValue::Builder(builder);
        }
        builder.methods.push(method.to_owned());
        // Classify the chained mutator against the target's kind.
        let channels = builder
            .target
            .as_ref()
            .and_then(|target| state.heap.object(target))
            .and_then(|object| match &object.kind {
                KindSet::Known(kinds) if kinds.len() == 1 => {
                    let kind = kinds.iter().next().expect("len checked");
                    self.ctx
                        .resolve_method(kind, method)
                        .and_then(|(id, _)| mutator_channels(&id))
                }
                _ => None,
            })
            .or_else(|| mutator_channels(&format!("builder.{method}")));
        if let Some(channels) = channels {
            builder.channels.extend(channels);
        } else {
            builder.channels_known = Truth::Maybe;
            // The unclassified chained method may be `set_z_index`
            // (family write by default): when the builder is played the
            // live target's z_index is rewritten, so the tracked fact
            // does not survive.
            if let Some(target) = builder.target.clone() {
                widen_z_index_family(state, &target);
            }
        }
        // The chained method mutates the *target copy*, not the live
        // object (DESIGN §3.2); the live target's epoch is untouched.
        if let Some(target) = &builder.target {
            if let Some(copy) = state
                .heap
                .object(target)
                .and_then(|object| object.generated_target.target.clone())
            {
                if let Some(copy_state) = state.heap.object_mut(&copy) {
                    copy_state.mutation_epoch += 1;
                }
            }
        }
        if self.emit {
            let key = (builder.site, builder.call_path.clone());
            if let Some(fact) = self.sink.builder_executions.get_mut(&key) {
                fact.methods.clone_from(&builder.methods);
                fact.channels.clone_from(&builder.channels);
                fact.channels_known = builder.channels_known;
            }
            self.sink.sync_builders();
        }
        AbstractValue::Builder(builder)
    }

    // -- primitive heap / scene operations ---------------------------------

    pub(super) fn alloc_object(
        &mut self,
        site: AllocationSite,
        kind: KindSet,
        state: &mut ExecState,
    ) -> ObjectId {
        let id = ObjectId::new(site, self.call_context.clone(), self.block.cardinality());
        state
            .heap
            .insert_object(id.clone(), MobjectState::fresh(kind.clone()));
        self.record(
            site,
            OpKind::Alloc {
                object: id.clone(),
                kind,
            },
        );
        id
    }

    pub(super) fn scene_state_mut<'s>(
        &self,
        state: &'s mut ExecState,
    ) -> Option<&'s mut SceneState> {
        state.heap.scenes.get_mut(&self.scene_id)
    }

    pub(super) fn recompute_family(&self, state: &mut ExecState) {
        let Some(scene) = state.heap.scenes.get(&self.scene_id) else {
            return;
        };
        // Definite reach: Present roots via definite edges.
        let mut definite: BTreeSet<ObjectId> = BTreeSet::new();
        let mut possible: BTreeSet<ObjectId> = BTreeSet::new();
        let mut definite_queue: Vec<ObjectId> = Vec::new();
        let mut possible_queue: Vec<ObjectId> = Vec::new();
        for root in &scene.roots.items {
            match state
                .heap
                .object(root)
                .map_or(Presence::Absent, |object| object.scene_root_membership)
            {
                Presence::Present => {
                    definite_queue.push(root.clone());
                    possible_queue.push(root.clone());
                }
                Presence::Maybe => possible_queue.push(root.clone()),
                Presence::Absent => {}
            }
        }
        while let Some(id) = definite_queue.pop() {
            if !definite.insert(id.clone()) {
                continue;
            }
            if let Some(children) = state.definite_children.get(&id) {
                definite_queue.extend(children.iter().cloned());
            }
        }
        while let Some(id) = possible_queue.pop() {
            if !possible.insert(id.clone()) {
                continue;
            }
            if let Some(object) = state.heap.object(&id) {
                possible_queue.extend(object.children.iter().cloned());
            }
        }
        let ids: Vec<ObjectId> = state.heap.objects.keys().cloned().collect();
        for id in ids {
            let membership = if definite.contains(&id) {
                Presence::Present
            } else if possible.contains(&id) {
                Presence::Maybe
            } else {
                Presence::Absent
            };
            if let Some(object) = state.heap.objects.get_mut(&id) {
                object.family_membership = membership;
                object.visibility = match membership {
                    Presence::Absent => Visibility::Invisible,
                    Presence::Present | Presence::Maybe => Visibility::Maybe,
                };
            }
        }
    }

    pub(super) fn scene_add(
        &mut self,
        objects: &[ObjectId],
        site: AllocationSite,
        reorders_existing: bool,
        foreground: bool,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        let mut order_effect = false;
        for id in objects {
            let present = state
                .heap
                .object(id)
                .map_or(Presence::Absent, |object| object.scene_root_membership);
            if let Some(scene) = self.scene_state_mut(state) {
                if certainty == Presence::Present {
                    if present == Presence::Absent {
                        scene.roots.items.push(id.clone());
                    } else if reorders_existing {
                        scene.roots.items.retain(|item| item != id);
                        scene.roots.items.push(id.clone());
                        order_effect = true;
                    }
                    // Auto-add semantics leave an already-present object
                    // in place.
                } else {
                    if !scene.roots.items.contains(id) {
                        scene.roots.items.push(id.clone());
                    }
                    scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
                }
            }
            if let Some(object) = state.heap.object_mut(id) {
                object.scene_root_membership = match certainty {
                    Presence::Present => Presence::Present,
                    _ => object.scene_root_membership.join(Presence::Present),
                };
                if foreground {
                    object.foreground = match certainty {
                        Presence::Present => Truth::Yes,
                        _ => object.foreground.join(Truth::Yes),
                    };
                }
            }
        }
        self.recompute_family(state);
        self.record(
            site,
            OpKind::SceneAdd {
                objects: objects.to_vec(),
                order_effect,
                reorders_existing,
                foreground,
            },
        );
    }

    /// `Scene.remove` with root-list restructuring (DESIGN §3.4).
    pub(super) fn scene_remove(
        &mut self,
        objects: &[ObjectId],
        site: AllocationSite,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        // Family closure of the removed objects (extract_families).
        let mut removed: BTreeSet<ObjectId> = BTreeSet::new();
        let mut queue: Vec<ObjectId> = objects.to_vec();
        while let Some(id) = queue.pop() {
            if !removed.insert(id.clone()) {
                continue;
            }
            if let Some(object) = state.heap.object(&id) {
                queue.extend(object.children.iter().cloned());
            }
        }

        if certainty == Presence::Present {
            let roots = self
                .scene_state_mut(state)
                .map(|scene| scene.roots.items.clone())
                .unwrap_or_default();
            let mut new_roots: Vec<ObjectId> = Vec::new();
            for root in roots {
                Self::restructure_root(&root, &removed, &mut new_roots, state);
            }
            let dropped: Vec<ObjectId> = self
                .scene_state_mut(state)
                .map(|scene| {
                    scene
                        .roots
                        .items
                        .iter()
                        .filter(|id| !new_roots.contains(id))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if let Some(scene) = self.scene_state_mut(state) {
                scene.roots.items.clone_from(&new_roots);
            }
            for id in &dropped {
                if let Some(object) = state.heap.object_mut(id) {
                    object.scene_root_membership = Presence::Absent;
                }
            }
            for id in &new_roots {
                if let Some(object) = state.heap.object_mut(id) {
                    if object.scene_root_membership == Presence::Absent {
                        object.scene_root_membership = Presence::Present;
                    }
                }
            }
        } else {
            for id in &removed {
                if let Some(object) = state.heap.object_mut(id) {
                    object.scene_root_membership =
                        object.scene_root_membership.join(Presence::Absent);
                }
            }
            if let Some(scene) = self.scene_state_mut(state) {
                scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
            }
        }
        self.recompute_family(state);

        if self.emit {
            for id in objects {
                let surviving_parents = state
                    .heap
                    .object(id)
                    .map(|object| object.parents.clone())
                    .unwrap_or_default();
                self.sink.scene_removals.push(SceneRemovalFact {
                    site,
                    removed: id.clone(),
                    surviving_parents,
                    certainty,
                });
            }
        }
        self.record(
            site,
            OpKind::SceneRemove {
                objects: objects.to_vec(),
            },
        );
    }

    /// One root of `get_restructured_mobject_list`: keep it, or replace it
    /// by its safe children when its family intersects the removed set.
    fn restructure_root(
        root: &ObjectId,
        removed: &BTreeSet<ObjectId>,
        out: &mut Vec<ObjectId>,
        state: &ExecState,
    ) {
        if removed.contains(root) {
            return;
        }
        // Does the root's (possible) family intersect the removed set?
        let mut intersects = false;
        let mut queue: Vec<ObjectId> = state
            .heap
            .object(root)
            .map(|object| object.children.iter().cloned().collect())
            .unwrap_or_default();
        let mut seen = BTreeSet::new();
        while let Some(id) = queue.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if removed.contains(&id) {
                intersects = true;
                break;
            }
            if let Some(object) = state.heap.object(&id) {
                queue.extend(object.children.iter().cloned());
            }
        }
        if !intersects {
            out.push(root.clone());
            return;
        }
        // The group dissolves: recurse into its definite children (the
        // parent link itself survives — `submobjects` is not edited).
        let children: Vec<ObjectId> = state
            .definite_children
            .get(root)
            .map(|children| children.iter().cloned().collect())
            .unwrap_or_default();
        for child in children {
            Self::restructure_root(&child, removed, out, state);
        }
    }

    pub(super) fn scene_replace(
        &mut self,
        old: &ObjectId,
        new: &ObjectId,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        if let Some(scene) = self.scene_state_mut(state) {
            if certainty == Presence::Present {
                if let Some(position) = scene.roots.items.iter().position(|id| id == old) {
                    scene.roots.items[position] = new.clone();
                } else {
                    scene.roots.items.push(new.clone());
                    scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
                }
            } else {
                if !scene.roots.items.contains(new) {
                    scene.roots.items.push(new.clone());
                }
                scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
            }
        }
        if let Some(object) = state.heap.object_mut(old) {
            object.scene_root_membership = match certainty {
                Presence::Present => Presence::Absent,
                _ => object.scene_root_membership.join(Presence::Absent),
            };
        }
        if let Some(object) = state.heap.object_mut(new) {
            object.scene_root_membership = match certainty {
                Presence::Present => Presence::Present,
                _ => object.scene_root_membership.join(Presence::Present),
            };
        }
        self.recompute_family(state);
    }

    pub(super) fn add_child(
        &mut self,
        parent: &ObjectId,
        child: &ObjectId,
        site: AllocationSite,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        if parent == child {
            // Direct self-add is rejected by Manim; record the event for
            // MLC110 without corrupting the graph.
            self.record(
                site,
                OpKind::AddChild {
                    parent: parent.clone(),
                    child: child.clone(),
                },
            );
            return;
        }
        if let Some(object) = state.heap.object_mut(parent) {
            object.children.insert(child.clone());
        }
        if let Some(object) = state.heap.object_mut(child) {
            object.parents.insert(parent.clone());
        }
        if certainty == Presence::Present {
            state
                .definite_children
                .entry(parent.clone())
                .or_default()
                .insert(child.clone());
        }
        self.recompute_family(state);
        self.record(
            site,
            OpKind::AddChild {
                parent: parent.clone(),
                child: child.clone(),
            },
        );
    }

    pub(super) fn remove_child(
        &mut self,
        parent: &ObjectId,
        child: &ObjectId,
        site: AllocationSite,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        if certainty == Presence::Present {
            if let Some(object) = state.heap.object_mut(parent) {
                object.children.remove(child);
            }
            if let Some(object) = state.heap.object_mut(child) {
                object.parents.remove(parent);
            }
        }
        if let Some(children) = state.definite_children.get_mut(parent) {
            children.remove(child);
        }
        self.recompute_family(state);
        self.record(
            site,
            OpKind::RemoveChild {
                parent: parent.clone(),
                child: child.clone(),
            },
        );
    }

    pub(super) fn generate_target(
        &mut self,
        target: &ObjectId,
        site: AllocationSite,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        let copy = ObjectId::new(site, self.call_context.clone(), self.block.cardinality());
        let copy_state = state.heap.object(target).map_or_else(
            || MobjectState::fresh(KindSet::Unknown),
            |object| {
                let mut fresh = MobjectState::fresh(object.kind.clone());
                fresh.copy_provenance = Some(CopyKind::GenerateTarget);
                clone_path_facts(&mut fresh, object);
                fresh
            },
        );
        state.heap.insert_object(copy.clone(), copy_state);
        state.heap.record_copy(
            copy.clone(),
            CopyOf {
                original: target.clone(),
                kind: CopyKind::GenerateTarget,
            },
        );
        if let Some(object) = state.heap.object_mut(target) {
            object.generated_target = GeneratedTarget {
                presence: if certainty == Presence::Present {
                    Presence::Present
                } else {
                    object.generated_target.presence.join(Presence::Present)
                },
                target: Some(copy.clone()),
            };
        }
        self.record(
            site,
            OpKind::GenerateTarget {
                target: target.clone(),
                copy,
            },
        );
    }

    pub(super) fn save_state(
        &mut self,
        target: &ObjectId,
        site: AllocationSite,
        certainty: Presence,
        state: &mut ExecState,
    ) {
        if let Some(object) = state.heap.object_mut(target) {
            object.saved_state = if certainty == Presence::Present {
                Presence::Present
            } else {
                object.saved_state.join(Presence::Present)
            };
        }
        self.record(
            site,
            OpKind::SaveState {
                target: target.clone(),
            },
        );
    }

    pub(super) fn mutate(
        &mut self,
        target: &ObjectId,
        kind: MutationKind,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        if kind == MutationKind::Unknown {
            // An unclassified curated mutator may write anything the
            // classified channels do not cover — including the family
            // `z_index` — so the exact fact does not survive.
            widen_z_index_family(state, target);
        }
        if let Some(object) = state.heap.object_mut(target) {
            object.mutation_epoch += 1;
        }
        self.record(
            site,
            OpKind::Mutate {
                target: target.clone(),
                kind,
            },
        );
    }

    pub(super) fn unknown_mutation(
        &mut self,
        values: &[ObjectId],
        includes_scene: bool,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        for id in values {
            if let Some(object) = state.heap.object_mut(id) {
                object.fill_opacity = Num::Unknown;
                object.stroke_opacity = Num::Unknown;
                object.z_index = Num::Unknown;
                object.family_size = Num::Unknown;
                object.point_count = Num::Unknown;
                object.curve_count = Num::Unknown;
                object.subpath_count = Num::Unknown;
                object.points_per_curve = Num::Unknown;
                object.mutation_epoch += 1;
                object.generated_target = object.generated_target.join(&GeneratedTarget::absent());
                object.generated_target.presence =
                    object.generated_target.presence.join(Presence::Maybe);
                object.saved_state = object.saved_state.join(Presence::Maybe);
            }
        }
        if includes_scene {
            if let Some(scene) = self.scene_state_mut(state) {
                scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
            }
            for id in values {
                if let Some(object) = state.heap.object_mut(id) {
                    object.scene_root_membership =
                        object.scene_root_membership.join(Presence::Maybe);
                }
            }
            self.recompute_family(state);
        }
        if !values.is_empty() || includes_scene {
            self.record(
                site,
                OpKind::UnknownMutation {
                    values: values.to_vec(),
                    includes_scene,
                },
            );
        }
    }

    // -- VMobject path-construction methods (MLR116) ------------------------

    /// Whether the receiver class is a (project or canonical) subclass of
    /// `VMobject` (`vectorized`) / `Mobject` with a fully resolved chain.
    /// For project classes the caller has already ruled out a project
    /// override of the method (it would have dispatched to a summary).
    fn path_receiver_is(&self, kind: &str, vectorized: bool) -> bool {
        let check = |class_id: &str| {
            if vectorized {
                self.ctx.is_vmobject_class(class_id)
            } else {
                self.ctx.is_mobject_class(class_id)
            }
        };
        if let Some(record) = self.ctx.index.classes.get(kind) {
            record.bases_fully_resolved
                && !record.reached_bases.is_empty()
                && record.reached_bases.iter().all(|base| check(base))
        } else {
            check(kind)
        }
    }

    /// Curated `VMobject` path-construction methods (MLR116; semantics
    /// verified against `vectorized_mobject.py`): exact point-count
    /// arithmetic where the current count and the points-per-curve fact
    /// are exact integers, `Unknown` otherwise, plus a `PathTopology`
    /// mutation. Returns `None` when the method is not a path method for
    /// this receiver (the caller falls back to unknown-call widening).
    pub(super) fn apply_path_method(
        &mut self,
        id: &ObjectId,
        kind: &str,
        method: &str,
        call: &'a ast::ExprCall,
        state: &mut ExecState,
    ) -> Option<AbstractValue> {
        const PATH_METHODS: &[&str] = &[
            "set_points",
            "clear_points",
            "append_points",
            "start_new_path",
            "add_line_to",
            "add_cubic_bezier_curve_to",
            "add_quadratic_bezier_curve_to",
            "add_smooth_curve_to",
            "add_points_as_corners",
            "set_points_as_corners",
            "set_points_smoothly",
            "close_path",
            "resize_points",
        ];
        let vmobject_method = PATH_METHODS.contains(&method) && self.path_receiver_is(kind, true);
        let mobject_method = method == "reset_points" && self.path_receiver_is(kind, false);
        if !vmobject_method && !mobject_method {
            return None;
        }
        self.eval_call_args(call, state);
        let site = self.site(call.range());

        let (points, nppc) = state
            .heap
            .object(id)
            .map_or((Num::Unknown, Num::Unknown), |object| {
                (object.point_count.clone(), object.points_per_curve.clone())
            });
        let exact_int = |num: &Num| match num {
            Num::Exact(crate::semantic::values::NumLit::Int(value)) => Some(*value),
            _ => None,
        };
        let current = exact_int(&points);
        let per_curve = exact_int(&nppc).filter(|count| *count > 0);
        let first_arg = call.args.first();
        let new_points = match method {
            "clear_points" | "reset_points" => Num::int(0),
            "set_points" => first_arg
                .and_then(display_element_count)
                .map_or(Num::Unknown, Num::int),
            "append_points" => match (current, first_arg.and_then(display_element_count)) {
                (Some(current), Some(count)) => Num::int(current + count),
                _ => Num::Unknown,
            },
            "resize_points" => first_arg
                .and_then(literal_int_arg)
                .map_or(Num::Unknown, Num::int),
            "start_new_path" => match (current, per_curve) {
                // An unfinished curve is closed by repeating the last
                // anchor `n - (k % n)` times, then the new point appends.
                (Some(current), Some(per_curve)) => {
                    let partial = current % per_curve;
                    Num::int(if partial == 0 {
                        current + 1
                    } else {
                        current + (per_curve - partial) + 1
                    })
                }
                _ => Num::Unknown,
            },
            "add_line_to" | "add_cubic_bezier_curve_to" | "add_quadratic_bezier_curve_to" => {
                match (current, per_curve) {
                    // `has_new_path_started()` (`k % n == 1`): the started
                    // curve completes with `n - 1` points; otherwise the
                    // last anchor is duplicated first (`n` points). The
                    // empty-path case raises at runtime
                    // (`throw_error_if_no_points`, the MLR116 defect); the
                    // post-state is modeled anyway and never observed.
                    (Some(current), Some(per_curve)) => Num::int(if current % per_curve == 1 {
                        current + (per_curve - 1)
                    } else {
                        current + per_curve
                    }),
                    _ => Num::Unknown,
                }
            }
            "close_path" => match (current, per_curve) {
                // An already-closed path adds nothing; an open path adds
                // one closing curve — the count is the interval hull.
                (Some(current), Some(per_curve)) => {
                    Num::int(current).join(&Num::int(current + per_curve))
                }
                _ => Num::Unknown,
            },
            // add_smooth_curve_to / add_points_as_corners /
            // set_points_as_corners / set_points_smoothly append a
            // data-dependent number of points.
            _ => Num::Unknown,
        };
        let curves = match (exact_int(&new_points), per_curve) {
            (Some(points), Some(per_curve)) if points % per_curve == 0 => {
                Num::int(points / per_curve)
            }
            _ => Num::Unknown,
        };
        if let Some(object) = state.heap.object_mut(id) {
            object.point_count = new_points;
            object.curve_count = curves;
            // Subpath splits are not tracked (doc on `PathStateFact`).
            object.subpath_count = Num::Unknown;
        }
        self.mutate(id, MutationKind::PathTopology, site, state);
        let returns_self = !matches!(method, "clear_points" | "close_path");
        Some(if returns_self {
            AbstractValue::Object(id.clone())
        } else {
            AbstractValue::Unknown
        })
    }
}
