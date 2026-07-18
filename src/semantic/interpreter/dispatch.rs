//! The shared resolution context and call dispatch into curated
//! knowledge effects (DESIGN §5.3-§5.4): qualified-call facts, curated
//! method resolution along the profile base chain, project-override
//! distrust, and the scene / mobject method effect arms.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::frontend::index::{CallableSignature, ProjectIndex, QualifiedCall, QualifiedCallFacts};
use crate::knowledge::{KnowledgeProfile, SceneMembershipEffect, SymbolEntry, SymbolKind};
use crate::semantic::events::MutationKind;
use crate::semantic::state::{CallbackRef, MobjectState, SuspendBehavior, WriteChannel};
use crate::semantic::summaries::SummaryTable;
use crate::semantic::values::{CopyKind, CopyOf, KindSet, Num, ObjectId, Presence, Truth};
use crate::source::FileId;

use super::callbacks::{signature_from_args, updater_fact};
use super::exec::{AbstractValue, ExecState, Machine, OpKind};
use super::heap_ops::{
    MOBJECT_ID, VMOBJECT_ID, apply_z_index_write, clone_path_facts, literal_signed_num,
    mutator_channels, seed_path_counts, seed_z_index, set_z_index_family_arg,
};
use super::{
    DefMap, FixedAction, FixedKind, FixedRegistrationFact, TargetRequirement, TargetRequirementFact,
};

/// Lifecycle methods whose project override invalidates curated animation
/// effects (DESIGN §5.4).
const ANIMATION_LIFECYCLE_METHODS: &[&str] = &[
    "begin",
    "finish",
    "clean_up_from_scene",
    "interpolate",
    "interpolate_mobject",
    "interpolate_submobject",
    "_setup_scene",
];

// ---------------------------------------------------------------------------
// Shared analysis context.
// ---------------------------------------------------------------------------

pub(super) struct Ctx<'a> {
    pub(super) index: &'a ProjectIndex,
    pub(super) knowledge: Option<&'a KnowledgeProfile>,
    pub(super) defs: &'a DefMap<'a>,
    pub(super) summaries: &'a SummaryTable,
    call_facts: BTreeMap<(FileId, u32, u32), &'a QualifiedCall>,
}

impl<'a> Ctx<'a> {
    pub(super) fn new(
        index: &'a ProjectIndex,
        calls: &'a QualifiedCallFacts,
        knowledge: Option<&'a KnowledgeProfile>,
        defs: &'a DefMap<'a>,
        summaries: &'a SummaryTable,
    ) -> Self {
        let mut call_facts = BTreeMap::new();
        for call in &calls.calls {
            call_facts.insert(
                (
                    call.file,
                    call.call_range.start().into(),
                    call.call_range.end().into(),
                ),
                call,
            );
        }
        Self {
            index,
            knowledge,
            defs,
            summaries,
            call_facts,
        }
    }

    pub(super) fn fact(&self, file: FileId, range: TextRange) -> Option<&'a QualifiedCall> {
        self.call_facts
            .get(&(file, range.start().into(), range.end().into()))
            .copied()
    }

    /// The curated symbol entry for calling `method` on `class_id`,
    /// walking the profile's base chain (e.g. `ThreeDScene.add` resolves
    /// to `Scene.add`).
    pub(super) fn resolve_method(
        &self,
        class_id: &str,
        method: &str,
    ) -> Option<(String, &'a SymbolEntry)> {
        let profile = self.knowledge?;
        let mut queue = vec![class_id.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(current) = queue.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let id = format!("{current}.{method}");
            if let Some(entry) = profile.symbol(&id) {
                return Some((id, entry));
            }
            if let Some(entry) = profile.symbol(&current) {
                for base in &entry.bases {
                    queue.push(base.clone());
                }
            }
        }
        None
    }

    /// The curated entry for a candidate that is already a full method id
    /// (`manim.scene.scene.Scene.play`) or needs base-chain resolution
    /// (`manim.scene.three_d_scene.ThreeDScene.add`).
    pub(super) fn resolve_method_candidate(
        &self,
        candidate: &str,
    ) -> Option<(String, &'a SymbolEntry)> {
        let profile = self.knowledge?;
        if let Some(entry) = profile.symbol(candidate) {
            return Some((candidate.to_owned(), entry));
        }
        let (class_id, method) = candidate.rsplit_once('.')?;
        self.resolve_method(class_id, method)
    }

    /// Walks the curated base chain of `class_id` and returns the first
    /// curated value `get` yields.
    fn chain_field<T>(&self, class_id: &str, get: impl Fn(&SymbolEntry) -> Option<T>) -> Option<T> {
        let profile = self.knowledge?;
        let mut queue = vec![class_id.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(current) = queue.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            let Some(entry) = profile.symbol(&current) else {
                continue;
            };
            if let Some(value) = get(entry) {
                return Some(value);
            }
            for base in &entry.bases {
                queue.push(base.clone());
            }
        }
        None
    }

    /// Whether the canonical class is (or reaches through the curated base
    /// chain) `VMobject`.
    pub(super) fn is_vmobject_class(&self, class_id: &str) -> bool {
        if class_id == VMOBJECT_ID {
            return true;
        }
        if let Some(entry) = self.knowledge.and_then(|profile| profile.symbol(class_id)) {
            if entry.kind == SymbolKind::Vmobject {
                return true;
            }
        }
        self.reaches_base(class_id, VMOBJECT_ID)
    }

    /// Whether the canonical class is (or reaches through the curated base
    /// chain) `Mobject`.
    pub(super) fn is_mobject_class(&self, class_id: &str) -> bool {
        if class_id == MOBJECT_ID {
            return true;
        }
        if let Some(entry) = self.knowledge.and_then(|profile| profile.symbol(class_id)) {
            if matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject) {
                return true;
            }
        }
        self.reaches_base(class_id, MOBJECT_ID)
    }

    pub(super) fn reaches_base(&self, class_id: &str, base: &str) -> bool {
        let Some(profile) = self.knowledge else {
            return false;
        };
        let mut queue = vec![class_id.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(current) = queue.pop() {
            if current == base {
                return true;
            }
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(entry) = profile.symbol(&current) {
                for parent in &entry.bases {
                    queue.push(parent.clone());
                }
            }
        }
        false
    }

    /// Curated lifecycle effects of a knowledge animation class.
    fn animation_effects(&self, class_id: &str) -> ResolvedAnimEffects {
        let truth = |get: fn(&SymbolEntry) -> Option<bool>| {
            self.chain_field(class_id, get)
                .map_or(Truth::Maybe, Truth::from)
        };
        let suspend = match self.chain_field(class_id, |entry| {
            entry
                .effects
                .as_ref()
                .and_then(|effects| effects.suspends_updaters)
        }) {
            Some(true) => SuspendBehavior::SuspendsLiveTargets,
            Some(false) => SuspendBehavior::LeavesUpdatersRunning,
            None => SuspendBehavior::Unknown,
        };
        ResolvedAnimEffects {
            introducer: truth(|entry| {
                entry
                    .effects
                    .as_ref()
                    .and_then(|effects| effects.introducer)
            }),
            remover: truth(|entry| entry.effects.as_ref().and_then(|effects| effects.remover)),
            replacement: truth(|entry| {
                entry
                    .effects
                    .as_ref()
                    .and_then(|effects| effects.replacement)
            }),
            suspend,
            requires_target: self
                .chain_field(class_id, |entry| {
                    entry
                        .effects
                        .as_ref()
                        .and_then(|effects| effects.requires_target)
                })
                .unwrap_or(false),
            requires_saved_state: self
                .chain_field(class_id, |entry| {
                    entry
                        .effects
                        .as_ref()
                        .and_then(|effects| effects.requires_saved_state)
                })
                .unwrap_or(false),
        }
    }

    /// Effects of a project Animation subclass: curated base effects are
    /// trusted only when no project ancestor overrides a lifecycle method
    /// (DESIGN §5.4).
    fn project_animation_effects(&self, class_id: &str) -> ResolvedAnimEffects {
        let mut current = Some(class_id.to_owned());
        let mut curated_bases: BTreeSet<String> = BTreeSet::new();
        let mut distrusted = false;
        let mut visited = BTreeSet::new();
        let mut queue: Vec<String> = vec![];
        if let Some(start) = current.take() {
            queue.push(start);
        }
        while let Some(id) = queue.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let Some(record) = self.index.classes.get(&id) else {
                curated_bases.insert(id);
                continue;
            };
            if !record.bases_fully_resolved {
                distrusted = true;
            }
            for method in ANIMATION_LIFECYCLE_METHODS {
                if record.methods.contains_key(*method) {
                    distrusted = true;
                }
            }
            for base in &record.bases {
                if let crate::frontend::index::BaseRef::Resolved(base_id) = base {
                    queue.push(base_id.clone());
                }
            }
        }
        if distrusted || curated_bases.is_empty() {
            return ResolvedAnimEffects::unknown();
        }
        let mut effects: Option<ResolvedAnimEffects> = None;
        for base in &curated_bases {
            let base_effects = self.animation_effects(base);
            effects = Some(match effects {
                None => base_effects,
                Some(previous) => previous.join(base_effects),
            });
        }
        effects.unwrap_or_else(ResolvedAnimEffects::unknown)
    }
}

/// Resolved lifecycle effects of one animation class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResolvedAnimEffects {
    pub(super) introducer: Truth,
    pub(super) remover: Truth,
    pub(super) replacement: Truth,
    pub(super) suspend: SuspendBehavior,
    pub(super) requires_target: bool,
    pub(super) requires_saved_state: bool,
}

impl ResolvedAnimEffects {
    const fn unknown() -> Self {
        Self {
            introducer: Truth::Maybe,
            remover: Truth::Maybe,
            replacement: Truth::Maybe,
            suspend: SuspendBehavior::Unknown,
            requires_target: false,
            requires_saved_state: false,
        }
    }

    fn join(self, other: Self) -> Self {
        Self {
            introducer: self.introducer.join(other.introducer),
            remover: self.remover.join(other.remover),
            replacement: self.replacement.join(other.replacement),
            suspend: self.suspend.join(other.suspend),
            requires_target: self.requires_target || other.requires_target,
            requires_saved_state: self.requires_saved_state || other.requires_saved_state,
        }
    }
}

/// Builtins that never mutate Manim state; calling them does not widen
/// their arguments.
pub(super) const PURE_BUILTINS: &[&str] = &[
    "abs",
    "bool",
    "dict",
    "enumerate",
    "float",
    "int",
    "isinstance",
    "len",
    "list",
    "max",
    "min",
    "print",
    "range",
    "reversed",
    "round",
    "set",
    "sorted",
    "str",
    "sum",
    "tuple",
    "zip",
];

impl<'a> Machine<'a, '_> {
    pub(super) fn dispatch_direct(
        &mut self,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if !matches!(call.func.as_ref(), ast::Expr::Name(_)) {
            self.eval_expr(&call.func, state);
        }
        let candidates: Vec<String> = fact
            .map(|fact| fact.candidates.iter().cloned().collect())
            .unwrap_or_default();
        if candidates.is_empty() {
            if let ast::Expr::Name(name) = call.func.as_ref() {
                if PURE_BUILTINS.contains(&name.id.as_str()) {
                    self.eval_call_args(call, state);
                    return AbstractValue::Unknown;
                }
            }
            self.eval_args_and_widen(call, state);
            return AbstractValue::Unknown;
        }
        if candidates.len() == 1 {
            let candidate = candidates[0].clone();
            if self.ctx.defs.defs.contains_key(&candidate) {
                return self.apply_summary_call(&candidate, None, call, fact, state);
            }
            if self.ctx.index.classes.contains_key(&candidate) {
                return self.instantiate_project_class(&candidate, call, fact, state);
            }
            if let Some(entry) = self
                .ctx
                .knowledge
                .and_then(|profile| profile.symbol(&candidate))
            {
                return self.apply_knowledge_symbol(&candidate, entry, call, fact, state);
            }
            self.eval_args_and_widen(call, state);
            return AbstractValue::Unknown;
        }
        // Multiple candidates: model only the all-mobject-classes case.
        let all_mobject = candidates.iter().all(|candidate| {
            self.ctx
                .knowledge
                .and_then(|profile| profile.symbol(candidate))
                .is_some_and(|entry| {
                    matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                })
        });
        if all_mobject {
            self.eval_call_args(call, state);
            let kind = KindSet::Known(candidates.into_iter().collect());
            let id = self.alloc_object(self.site(call.range()), kind.clone(), state);
            if let Some(object) = state.heap.object_mut(&id) {
                seed_path_counts(object, &kind, fact);
                seed_z_index(object, fact);
            }
            return AbstractValue::Object(id);
        }
        self.eval_args_and_widen(call, state);
        AbstractValue::Unknown
    }

    fn apply_knowledge_symbol(
        &mut self,
        candidate: &str,
        entry: &'a SymbolEntry,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        match entry.kind {
            SymbolKind::Animation => {
                let effects = self.ctx.animation_effects(candidate);
                self.create_animation(
                    &KindSet::single(candidate),
                    Some(candidate),
                    effects,
                    call,
                    fact,
                    state,
                )
            }
            SymbolKind::Mobject | SymbolKind::Vmobject => {
                let positional = self.eval_call_args(call, state);
                let kind = KindSet::single(candidate);
                let id = self.alloc_object(self.site(call.range()), kind.clone(), state);
                if let Some(object) = state.heap.object_mut(&id) {
                    seed_path_counts(object, &kind, fact);
                    seed_z_index(object, fact);
                }
                // Container constructors adopt their positional mobject
                // arguments as submobjects (exact curated identities).
                if candidate == "manim.mobject.types.vectorized_mobject.VGroup"
                    || candidate == "manim.mobject.mobject.Group"
                {
                    let site = self.site(call.range());
                    let certainty = self.certainty();
                    for value in &positional {
                        if let AbstractValue::Object(child) = value {
                            self.add_child(&id.clone(), child, site, certainty, state);
                        }
                    }
                }
                AbstractValue::Object(id)
            }
            SymbolKind::Function => {
                let per_frame_factory = entry
                    .effects
                    .as_ref()
                    .is_some_and(|effects| effects.registers_updater == Some(true));
                if per_frame_factory {
                    // `always_redraw(factory)`: the returned mobject
                    // carries a per-frame reconstruction updater.
                    let positional = self.eval_call_args(call, state);
                    let id = self.alloc_object(self.site(call.range()), KindSet::Unknown, state);
                    let (callback, signature) = match (call.args.first(), positional.first()) {
                        (Some(_), Some(AbstractValue::Callable(callback, signature))) => {
                            (callback.clone(), signature.clone())
                        }
                        _ => (CallbackRef::Unknown, None),
                    };
                    let fact = updater_fact(callback, signature.as_ref(), false);
                    let site = self.site(call.range());
                    self.register_updater(Some(&id.clone()), fact, site, state);
                    return AbstractValue::Object(id);
                }
                self.eval_call_args(call, state);
                AbstractValue::Unknown
            }
            SymbolKind::Scene | SymbolKind::Camera | SymbolKind::Constant => {
                self.eval_call_args(call, state);
                AbstractValue::Unknown
            }
            SymbolKind::Method => {
                self.eval_args_and_widen(call, state);
                AbstractValue::Unknown
            }
        }
    }

    fn instantiate_project_class(
        &mut self,
        class_id: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if self.ctx.index.animation_classes.contains(class_id) {
            let effects = self.ctx.project_animation_effects(class_id);
            return self.create_animation(
                &KindSet::single(class_id),
                Some(class_id),
                effects,
                call,
                fact,
                state,
            );
        }
        if self.ctx.index.mobject_classes.contains(class_id) {
            let init = format!("{class_id}.__init__");
            let id = self.alloc_object(self.site(call.range()), KindSet::single(class_id), state);
            if self.ctx.defs.defs.contains_key(&init) {
                // Run the constructor summary with `self` bound to the new
                // object (child adds, updater registrations, ...).
                self.apply_summary_call(
                    &init,
                    Some(AbstractValue::Object(id.clone())),
                    call,
                    fact,
                    state,
                );
            } else {
                self.eval_call_args(call, state);
            }
            return AbstractValue::Object(id);
        }
        if self.ctx.index.scene_classes.contains(class_id) {
            self.eval_call_args(call, state);
            return AbstractValue::Unknown;
        }
        // Plain project class: run its constructor summary if one exists.
        let init = format!("{class_id}.__init__");
        if self.ctx.defs.defs.contains_key(&init) {
            return self.apply_summary_call(&init, Some(AbstractValue::Unknown), call, fact, state);
        }
        self.eval_call_args(call, state);
        AbstractValue::Unknown
    }

    // -- scene method dispatch ---------------------------------------------

    pub(super) fn dispatch_scene_method(
        &mut self,
        method: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        // Nearest project override wins (a project override of a curated
        // Scene method also *distrusts* the curated effect, DESIGN §5.4).
        let mro = self.mro.clone();
        for class in &mro {
            let qualified = format!("{class}.{method}");
            if self.ctx.defs.defs.contains_key(&qualified) {
                return self.call_scene_helper(&qualified, call, fact, state);
            }
        }
        let resolved = fact
            .and_then(|fact| {
                fact.candidates
                    .iter()
                    .find_map(|candidate| self.ctx.resolve_method_candidate(candidate))
            })
            .or_else(|| {
                self.reached_bases
                    .iter()
                    .find_map(|base| self.ctx.resolve_method(base, method))
            });
        let Some((canonical, entry)) = resolved else {
            self.eval_args_and_widen(call, state);
            return AbstractValue::Unknown;
        };
        self.apply_scene_effect(&canonical, entry, call, fact, state)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per curated scene effect")]
    pub(super) fn apply_scene_effect(
        &mut self,
        canonical: &str,
        entry: &'a SymbolEntry,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let _ = canonical;
        let effects = entry.effects.clone().unwrap_or_default();
        let membership = effects.scene_membership;
        let site = self.site(call.range());
        let certainty = self.certainty();
        let self_result = || {
            if entry.returns_self == Some(true) {
                AbstractValue::SelfScene
            } else {
                AbstractValue::Unknown
            }
        };
        match membership {
            Some(SceneMembershipEffect::Play) => self.do_play(call, fact, state),
            Some(SceneMembershipEffect::Wait) => self.do_wait(call, fact, state),
            Some(SceneMembershipEffect::Add) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                let reorder = effects.reorders_existing_to_front.unwrap_or(false);
                self.scene_add(&objects, site, reorder, false, certainty, state);
                self_result()
            }
            Some(SceneMembershipEffect::Remove) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                self.scene_remove(&objects, site, certainty, state);
                self_result()
            }
            Some(SceneMembershipEffect::Replace) => {
                let positional = self.eval_call_args(call, state);
                if let (Some(AbstractValue::Object(old)), Some(AbstractValue::Object(new))) =
                    (positional.first(), positional.get(1))
                {
                    let (old, new) = (old.clone(), new.clone());
                    self.scene_replace(&old, &new, certainty, state);
                } else {
                    let objects = self.object_args(&positional, state);
                    self.unknown_mutation(&objects, true, site, state);
                }
                self_result()
            }
            Some(SceneMembershipEffect::ReorderToFront) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                self.scene_add(&objects, site, true, false, certainty, state);
                self_result()
            }
            Some(SceneMembershipEffect::ReorderToBack) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                if certainty == Presence::Present {
                    if let Some(scene) = self.scene_state_mut(state) {
                        for id in objects.iter().rev() {
                            scene.roots.items.retain(|item| item != id);
                            scene.roots.items.insert(0, id.clone());
                        }
                    }
                } else if let Some(scene) = self.scene_state_mut(state) {
                    scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
                }
                self.recompute_family(state);
                self_result()
            }
            Some(SceneMembershipEffect::Clear) => {
                self.eval_call_args(call, state);
                let roots: Vec<ObjectId> = self
                    .scene_state_mut(state)
                    .map(|scene| scene.roots.items.clone())
                    .unwrap_or_default();
                if certainty == Presence::Present {
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.roots.items.clear();
                        scene.foreground.items.clear();
                    }
                    for id in &roots {
                        if let Some(object) = state.heap.object_mut(id) {
                            object.scene_root_membership = Presence::Absent;
                            object.foreground = Truth::No;
                        }
                    }
                } else {
                    for id in &roots {
                        if let Some(object) = state.heap.object_mut(id) {
                            object.scene_root_membership =
                                object.scene_root_membership.join(Presence::Absent);
                        }
                    }
                }
                self.recompute_family(state);
                self.record(site, OpKind::SceneRemove { objects: roots });
                self_result()
            }
            Some(SceneMembershipEffect::AddForeground) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                self.scene_add(&objects, site, true, true, certainty, state);
                self_result()
            }
            Some(SceneMembershipEffect::RemoveForeground) => {
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                for id in &objects {
                    if let Some(object) = state.heap.object_mut(id) {
                        object.foreground = if certainty == Presence::Present {
                            Truth::No
                        } else {
                            object.foreground.join(Truth::No)
                        };
                    }
                }
                self_result()
            }
            Some(
                SceneMembershipEffect::AddFixedInFrame | SceneMembershipEffect::AddFixedOrientation,
            ) => {
                let fixed_in_frame = membership == Some(SceneMembershipEffect::AddFixedInFrame);
                let fixed_kind = if fixed_in_frame {
                    FixedKind::InFrame
                } else {
                    FixedKind::Orientation
                };
                let divergent = effects.renderer_divergent_membership == Some(true);
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                // 3D fixed helpers auto-add to the scene (DESIGN §3.5).
                self.scene_add(&objects, site, false, false, certainty, state);
                for id in &objects {
                    if let Some(object) = state.heap.object_mut(id) {
                        let flag = if fixed_in_frame {
                            &mut object.fixed_in_frame
                        } else {
                            &mut object.fixed_orientation
                        };
                        *flag = if certainty == Presence::Present {
                            Truth::Yes
                        } else {
                            flag.join(Truth::Yes)
                        };
                    }
                    if self.emit {
                        self.sink.fixed_registrations.push(FixedRegistrationFact {
                            site,
                            object: id.clone(),
                            kind: fixed_kind,
                            action: FixedAction::Register,
                            renderer_divergent: divergent,
                            certainty,
                        });
                    }
                }
                self_result()
            }
            Some(
                SceneMembershipEffect::RemoveFixedInFrame
                | SceneMembershipEffect::RemoveFixedOrientation,
            ) => {
                let fixed_in_frame = membership == Some(SceneMembershipEffect::RemoveFixedInFrame);
                let fixed_kind = if fixed_in_frame {
                    FixedKind::InFrame
                } else {
                    FixedKind::Orientation
                };
                let divergent = effects.renderer_divergent_membership == Some(true);
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                for id in &objects {
                    if let Some(object) = state.heap.object_mut(id) {
                        let flag = if fixed_in_frame {
                            &mut object.fixed_in_frame
                        } else {
                            &mut object.fixed_orientation
                        };
                        *flag = if certainty == Presence::Present {
                            Truth::No
                        } else {
                            flag.join(Truth::No)
                        };
                        // Membership after unfixing diverges between the
                        // renderers (DESIGN §3.5): never a certain fact.
                        object.scene_root_membership =
                            object.scene_root_membership.join(Presence::Maybe);
                    }
                    if self.emit {
                        self.sink.fixed_registrations.push(FixedRegistrationFact {
                            site,
                            object: id.clone(),
                            kind: fixed_kind,
                            action: FixedAction::Remove,
                            renderer_divergent: divergent,
                            certainty,
                        });
                    }
                }
                self.recompute_family(state);
                self.record(site, OpKind::RendererDivergentMembership);
                self_result()
            }
            Some(SceneMembershipEffect::RegisterSceneUpdater) => {
                let positional = self.eval_call_args(call, state);
                let (callback, signature) = self.callback_of(call.args.first(), positional.first());
                let fact = updater_fact(callback, signature.as_ref(), true);
                self.register_updater(None, fact, site, state);
                self_result()
            }
            Some(SceneMembershipEffect::RemoveSceneUpdater) => {
                let positional = self.eval_call_args(call, state);
                let (callback, _) = self.callback_of(call.args.first(), positional.first());
                self.remove_updater(None, &callback, site, state);
                self_result()
            }
            None => {
                // Curated non-membership Scene API (camera moves, ...):
                // no membership effect; evaluate arguments only.
                self.eval_call_args(call, state);
                self_result()
            }
        }
    }

    fn object_args(&mut self, values: &[AbstractValue], state: &mut ExecState) -> Vec<ObjectId> {
        let mut objects = Vec::new();
        let mut has_unknown = false;
        for value in values {
            match value {
                AbstractValue::Object(id) => objects.push(id.clone()),
                AbstractValue::Unknown
                | AbstractValue::Animation(_)
                | AbstractValue::Builder(_)
                | AbstractValue::Callable(..)
                | AbstractValue::Literal(_)
                | AbstractValue::SelfScene => has_unknown = true,
            }
        }
        if has_unknown {
            // An untracked member entered the scene: the exact order is no
            // longer fully known.
            if let Some(scene) = self.scene_state_mut(state) {
                scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
            }
        }
        objects
    }

    fn callback_of(
        &self,
        arg: Option<&'a ast::Expr>,
        value: Option<&AbstractValue>,
    ) -> (CallbackRef, Option<CallableSignature>) {
        match value {
            Some(AbstractValue::Callable(callback, signature)) => {
                (callback.clone(), signature.clone())
            }
            _ => match arg {
                Some(ast::Expr::Lambda(lambda)) => (
                    CallbackRef::Lambda(self.site(lambda.range())),
                    Some(signature_from_args(&lambda.args)),
                ),
                _ => (CallbackRef::Unknown, None),
            },
        }
    }

    // -- mobject method dispatch -------------------------------------------

    pub(super) fn dispatch_object_method(
        &mut self,
        id: &ObjectId,
        method: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let kind = state.heap.object(id).and_then(|object| match &object.kind {
            KindSet::Known(kinds) if kinds.len() == 1 => kinds.iter().next().cloned(),
            _ => None,
        });
        let Some(kind) = kind else {
            self.eval_call_args(call, state);
            self.unknown_mutation(
                std::slice::from_ref(id),
                false,
                self.site(call.range()),
                state,
            );
            return AbstractValue::Unknown;
        };
        if self.ctx.index.classes.contains_key(&kind) {
            let candidates = self.ctx.index.method_candidates(&kind, method);
            if candidates.len() == 1 {
                let candidate = candidates.iter().next().expect("len checked").clone();
                if self.ctx.defs.defs.contains_key(&candidate) {
                    return self.apply_summary_call(
                        &candidate,
                        Some(AbstractValue::Object(id.clone())),
                        call,
                        fact,
                        state,
                    );
                }
                if let Some((canonical, entry)) = self.ctx.resolve_method_candidate(&candidate) {
                    return self.apply_mobject_method(id, &canonical, entry, call, state);
                }
            }
        } else if let Some((canonical, entry)) = self.ctx.resolve_method(&kind, method) {
            return self.apply_mobject_method(id, &canonical, entry, call, state);
        }
        // Curated VMobject path-construction methods are modeled in-code
        // (MLR116) — reached only when neither a project override nor a
        // curated profile entry claimed the call above.
        if let Some(result) = self.apply_path_method(id, &kind, method, call, state) {
            return result;
        }
        self.eval_call_args(call, state);
        self.unknown_mutation(
            std::slice::from_ref(id),
            false,
            self.site(call.range()),
            state,
        );
        AbstractValue::Unknown
    }

    #[allow(clippy::too_many_lines, reason = "one arm per curated method effect")]
    fn apply_mobject_method(
        &mut self,
        id: &ObjectId,
        canonical: &str,
        entry: &'a SymbolEntry,
        call: &'a ast::ExprCall,
        state: &mut ExecState,
    ) -> AbstractValue {
        let positional = self.eval_call_args(call, state);
        let site = self.site(call.range());
        let certainty = self.certainty();
        let effects = entry.effects.clone().unwrap_or_default();

        if effects.generates_target == Some(true) {
            self.generate_target(id, site, certainty, state);
            // `generate_target` returns the fresh target copy.
            return state
                .heap
                .object(id)
                .and_then(|object| object.generated_target.target.clone())
                .map_or(AbstractValue::Unknown, AbstractValue::Object);
        }
        if effects.saves_state == Some(true) {
            self.save_state(id, site, certainty, state);
            return AbstractValue::Object(id.clone());
        }
        if effects.registers_updater == Some(true) {
            let (callback, signature) = self.callback_of(call.args.first(), positional.first());
            let fact = updater_fact(callback, signature.as_ref(), false);
            self.register_updater(Some(id), fact, site, state);
            return AbstractValue::Object(id.clone());
        }
        if effects.removes_updater == Some(true) {
            if call.args.is_empty() {
                // `clear_updaters()`.
                if let Some(object) = state.heap.object_mut(id) {
                    if certainty == Presence::Present {
                        object.updaters.clear();
                    }
                }
                self.record(site, OpKind::ClearUpdaters { target: id.clone() });
            } else {
                let (callback, _) = self.callback_of(call.args.first(), positional.first());
                self.remove_updater(Some(id), &callback, site, state);
            }
            return AbstractValue::Object(id.clone());
        }
        if effects.requires_saved_state == Some(true) {
            // `Mobject.restore()`.
            if self.emit {
                let presence = state
                    .heap
                    .object(id)
                    .map_or(Presence::Maybe, |object| object.saved_state);
                self.sink.target_requirements.push(TargetRequirementFact {
                    site,
                    requirement: TargetRequirement::SavedState,
                    target: Some(id.clone()),
                    presence,
                });
            }
            self.mutate(id, MutationKind::Points, site, state);
            return AbstractValue::Object(id.clone());
        }
        // Structural container methods (exact curated identities).
        if canonical == "manim.mobject.mobject.Mobject.add" {
            for value in &positional {
                if let AbstractValue::Object(child) = value {
                    self.add_child(&id.clone(), child, site, certainty, state);
                }
            }
            return AbstractValue::Object(id.clone());
        }
        if canonical == "manim.mobject.mobject.Mobject.remove" {
            for value in &positional {
                if let AbstractValue::Object(child) = value {
                    self.remove_child(&id.clone(), child, site, certainty, state);
                }
            }
            return AbstractValue::Object(id.clone());
        }
        if canonical == "manim.mobject.mobject.Mobject.copy" {
            let copy = ObjectId::new(site, self.call_context.clone(), self.block.cardinality());
            let copy_state = state.heap.object(id).map_or_else(
                || MobjectState::fresh(KindSet::Unknown),
                |object| {
                    let mut fresh = MobjectState::fresh(object.kind.clone());
                    fresh.copy_provenance = Some(CopyKind::Copy);
                    // A copy carries the original's updaters and geometry
                    // but no scene membership.
                    fresh.updaters.clone_from(&object.updaters);
                    clone_path_facts(&mut fresh, object);
                    fresh
                },
            );
            state.heap.insert_object(copy.clone(), copy_state);
            state.heap.record_copy(
                copy.clone(),
                CopyOf {
                    original: id.clone(),
                    kind: CopyKind::Copy,
                },
            );
            self.record(
                site,
                OpKind::Alloc {
                    object: copy.clone(),
                    kind: state
                        .heap
                        .object(id)
                        .map_or(KindSet::Unknown, |object| object.kind.clone()),
                },
            );
            return AbstractValue::Object(copy);
        }
        // `set_z_index(value, family=True)` writes the display-order sort
        // key of the receiver — and, by default, its whole family
        // (mobject.py `Mobject.set_z_index`; DESIGN §3.4 / MLP209).
        if canonical == "manim.mobject.mobject.Mobject.set_z_index" {
            let value = call
                .args
                .first()
                .filter(|arg| !matches!(arg, ast::Expr::Starred(_)))
                .and_then(literal_signed_num)
                .unwrap_or(Num::Unknown);
            let family = set_z_index_family_arg(call);
            apply_z_index_write(state, id, &value, family, certainty);
            // A z-order write is a style-channel mutation: it never moves
            // points or restructures the family (render_order replay and
            // count facts survive).
            self.mutate(id, MutationKind::Style, site, state);
            return AbstractValue::Object(id.clone());
        }
        // Fluent mutators and getters.
        match entry.returns_self {
            Some(true) => {
                let kind = mutator_channels(canonical).map_or(MutationKind::Unknown, |channels| {
                    channels
                        .iter()
                        .next()
                        .map_or(MutationKind::Unknown, |channel| match channel {
                            WriteChannel::Points => MutationKind::Points,
                            WriteChannel::Style => MutationKind::Style,
                            WriteChannel::Opacity => MutationKind::Opacity,
                            WriteChannel::Membership => MutationKind::Membership,
                            WriteChannel::CameraState => MutationKind::CameraState,
                        })
                });
                self.mutate(id, kind, site, state);
                AbstractValue::Object(id.clone())
            }
            Some(false) => AbstractValue::Unknown,
            None => {
                self.unknown_mutation(std::slice::from_ref(id), false, site, state);
                AbstractValue::Unknown
            }
        }
    }
}
