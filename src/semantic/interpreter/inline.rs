//! Helper inlining, effect-summary application, and summary extraction
//! (DESIGN §5.1 step 5, §5.7).
//!
//! # The summary-vs-inline duality invariant
//!
//! For any call that resolves to a project definition — `self.<method>()`
//! / `super().<method>()` scene-helper dispatch *and* project
//! module-level functions (same-module or imported), whether the scene is
//! passed positionally, by keyword, or not at all — **exactly one** of
//! the two execution modes applies — never both, never neither:
//!
//! 1. **Inline execution** ([`Machine::inline_project_callable`]): only
//!    during scene runs (`Machine::scene_run`), only while the called
//!    definition is not already on the inline stack (cycle detection) and
//!    the stack is shallower than the [`INLINE_DEPTH_SAFETY_CAP`]
//!    work-bounding backstop. The helper body executes against the live
//!    caller state, so it covers *everything* the body does: membership /
//!    updater / heap effects exactly once, plays and waits materialized
//!    as real [`PlayFact`]s per call site (anchored in the helper file,
//!    with the call chain in [`PlayFact::call_path`]), wait dynamics
//!    judged on the caller's actual updater state, and statement
//!    snapshots inside the helper body. The §5.7 summary is *not* applied
//!    for an inlined call (its only contribution is the return alias).
//!    For scene-method helpers the receiver binds to the scene; for
//!    module-level helpers every parameter binds from the written
//!    arguments, so a scene argument (`flourish(self, m)`, `tag(m,
//!    scene=self)`) flows as the live scene state in *any* position. A
//!    `*args` splat at the call site voids positional binding: the
//!    parameters stay `Unknown` and scene effects inside the body degrade
//!    to unknown mutations — conservative, never wrong. (Known scope
//!    limit, both modes: a scene-attribute write through a non-`self`
//!    parameter name, `scene.attr = x`, is not tracked.)
//! 2. **Summary application** ([`Machine::apply_summary_call`]): for
//!    summary runs (SCC fixpoints stay compositional), for the cycle /
//!    safety-cap fallback frontier of scene runs, and for calls that are
//!    not helper dispatch (project constructors, mobject methods). It
//!    replays the callable's parameter-relative effect summary —
//!    membership, children, updaters, mutations, allocations, animation
//!    creation, `self` attribute writes — against the caller state with
//!    combined certainty, and no snapshots are recorded inside the
//!    callee. Plays inside the summarized body do **not** run the §3.2
//!    pipeline; instead the summary's [`SummaryPlay`] records rehydrate
//!    into conservative summary-derived [`PlayFact`]s (always
//!    `Maybe`-certainty, open repetitions, syntactic fields only — the
//!    exact populated-vs-degraded field list is documented on
//!    [`SceneLifecycle::summary_derived_plays`]), so plays behind the
//!    frontier stay visible without fabricating pipeline effects.
//!
//! Whichever mode handles a call site owns *all* of that call's effects,
//! so no effect is ever applied twice (inline + summary) or dropped
//! (neither mode). Rehydrated play records are summary *facts*, not
//! effects: an inlined call never touches them (its plays are traced),
//! and a summary-applied call materializes each frontier play site
//! exactly once per record.
//!
//! Every scene-run fallback to mode 2 on a helper-dispatch call is
//! recorded as a [`FallbackFact`] (`Recursion` / `DepthCap` /
//! `Unresolvable`) in [`LifecycleFacts::inline_fallbacks`] — the
//! analysis-coverage frontier. Calls whose callee never resolved to a
//! project definition (unknown names, multiple candidates, third-party
//! imports) are not fallbacks: they were never inline candidates and
//! keep the DESIGN §5.3 widening semantics (third-party code is never
//! inlined).
//!
//! [`INLINE_DEPTH_SAFETY_CAP`]: self::INLINE_DEPTH_SAFETY_CAP
//! [`PlayFact`]: super::PlayFact
//! [`PlayFact::call_path`]: super::PlayFact::call_path
//! [`FallbackFact`]: super::FallbackFact
//! [`LifecycleFacts::inline_fallbacks`]: super::LifecycleFacts::inline_fallbacks
//! [`SceneLifecycle::summary_derived_plays`]: super::SceneLifecycle::summary_derived_plays

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};

use crate::frontend::index::{ProjectIndex, QualifiedCall, QualifiedCallFacts};
use crate::knowledge::KnowledgeProfile;
use crate::semantic::events::MutationKind;
use crate::semantic::heap::AbstractHeap;
use crate::semantic::state::{MobjectState, PlayGroupId, SceneState};
use crate::semantic::summaries::{
    MethodSummary, SummaryEffect, SummaryEvent, SummaryOperand, SummaryPlay, SummaryReturn,
    SummaryTable,
};
use crate::semantic::values::{
    AllocationSite, CallContextId, Cardinality, KindSet, Num, ObjectId, Presence, Truth,
};
use crate::source::SourceManager;

use super::dispatch::Ctx;
use super::exec::{
    AbstractValue, BlockCtx, ExecState, LiteralValue, Machine, OpKind, SinkOp, TraceSink,
    join_values,
};
use super::heap_ops::{seed_path_counts, widen_z_index_family};
use super::mro::linearize_project;
use super::{
    DefMap, FallbackFact, FallbackReason, FnDef, PlayFact, PlayKind, PlayedAnimation, ReturnFact,
    TargetRequirement, TargetRequirementFact,
};

/// Safety backstop on the inline stack depth (DESIGN §5.1 step 5).
///
/// Termination is guaranteed by CYCLE DETECTION — a helper call whose
/// callee is already on the inline stack falls back to the DESIGN §5.7
/// effect summary — so this cap never decides ordinary programs (any
/// acyclic chain of up to 32 distinct helpers inlines fully). It exists
/// ONLY to bound work on pathological call graphs (e.g. dozens of
/// distinct helpers fanning out at every level). When hit, the behavior
/// is the established conservative fallback: the summary applies
/// membership / updater effects, and plays inside the unexpanded
/// frontier materialize only as summary-derived maybe-certainty records
/// (never as fabricated pipeline facts).
const INLINE_DEPTH_SAFETY_CAP: usize = 32;

impl<'a> Machine<'a, '_> {
    // -- helper inlining (scene runs only) -----------------------------------

    /// A `self.<method>()` / `super().<method>()` call resolved to a
    /// project definition: inline the body during scene runs, apply the
    /// effect summary otherwise. Inlining is governed by cycle detection
    /// — a method already on the inline stack (direct *or* mutual
    /// recursion) falls back to the summary — with the
    /// [`INLINE_DEPTH_SAFETY_CAP`] backstop bounding pathological
    /// non-recursive fan-out; acyclic helper chains inline at any
    /// ordinary depth.
    ///
    /// Inlining executes the helper body against the live caller state,
    /// so plays and waits inside it materialize as real [`PlayFact`]s
    /// with their sites in the helper file, exact per-animation argument
    /// facts, membership effects applied exactly once (the summary is
    /// *not* applied for an inlined call), and wait dynamics judged on
    /// the caller's actual updater state. Summary application remains
    /// the semantics for summary runs (SCC fixpoints stay compositional)
    /// and for the cycle / safety-cap fallback frontier.
    pub(super) fn call_scene_helper(
        &mut self,
        qualified: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if !self.scene_run {
            return self.apply_summary_call(
                qualified,
                Some(AbstractValue::SelfScene),
                call,
                fact,
                state,
            );
        }
        if let Some(reason) = self.inline_fallback_reason(qualified) {
            self.record_fallback(qualified, call, reason);
            return self.apply_summary_call(
                qualified,
                Some(AbstractValue::SelfScene),
                call,
                fact,
                state,
            );
        }
        let def = self
            .ctx
            .defs
            .defs
            .get(qualified)
            .cloned()
            .expect("inline_fallback_reason checked the definition exists");
        self.inline_project_callable(qualified, &def, call, true, state)
    }

    /// A direct / module-alias call resolved to a project *module-level*
    /// function definition: inline the body during scene runs (a scene
    /// argument flows as the live scene state in any parameter position),
    /// apply the effect summary otherwise. Cycle detection and the depth
    /// cap are shared with scene-method helpers — mixed method/function
    /// recursion closes the same guard.
    pub(super) fn call_module_function(
        &mut self,
        qualified: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if !self.scene_run {
            return self.apply_summary_call(qualified, None, call, fact, state);
        }
        if let Some(reason) = self.inline_fallback_reason(qualified) {
            self.record_fallback(qualified, call, reason);
            return self.apply_summary_call(qualified, None, call, fact, state);
        }
        let def = self
            .ctx
            .defs
            .defs
            .get(qualified)
            .cloned()
            .expect("inline_fallback_reason checked the definition exists");
        self.inline_project_callable(qualified, &def, call, false, state)
    }

    /// Why `qualified` cannot be inlined at the current point of a scene
    /// run, or `None` when inlining applies. Cycle detection: the callee
    /// identity already on the inline stack means this call closes a
    /// recursion cycle (direct or mutual) — the summary fallback keeps
    /// the analysis terminating and the semantics conservative.
    fn inline_fallback_reason(&self, qualified: &str) -> Option<FallbackReason> {
        if self.inline_stack.iter().any(|(name, _)| name == qualified) {
            return Some(FallbackReason::Recursion);
        }
        if self.inline_stack.len() >= INLINE_DEPTH_SAFETY_CAP {
            return Some(FallbackReason::DepthCap);
        }
        if !self.ctx.defs.defs.contains_key(qualified) {
            return Some(FallbackReason::Unresolvable);
        }
        None
    }

    /// Records one inline fallback for the coverage frontier
    /// (`LifecycleFacts::inline_fallbacks`); final emitting pass only, so
    /// fixpoint passes do not duplicate the fact.
    fn record_fallback(
        &mut self,
        qualified: &str,
        call: &'a ast::ExprCall,
        reason: FallbackReason,
    ) {
        if !self.emit {
            return;
        }
        self.ctx.record_inline_fallback(FallbackFact {
            site: self.site(call.range()),
            callee: qualified.to_owned(),
            reason,
        });
    }

    /// Executes one resolved helper body inline (cycle-guarded by the
    /// caller). Arguments bind to the declared parameter names, and every
    /// recorded op composes the call site's certainty / loop context
    /// through [`Machine::base_block`]. With `binds_scene_receiver` the
    /// first parameter is the implicit `self` receiver and binds to the
    /// scene (scene-method helpers); without it every parameter binds
    /// from the written arguments (module-level helpers), so a scene
    /// argument flows as the live scene state wherever it was passed.
    #[allow(
        clippy::too_many_lines,
        reason = "argument binding, frame switch, and write-back form one sequence"
    )]
    fn inline_project_callable(
        &mut self,
        qualified: &str,
        def: &FnDef<'a>,
        call: &'a ast::ExprCall,
        binds_scene_receiver: bool,
        state: &mut ExecState,
    ) -> AbstractValue {
        let call_site = self.site(call.range());
        let params = def.param_names();

        // Evaluate the arguments in the caller frame and bind them to
        // parameter slots (the apply_summary_call discipline: a `*args`
        // splat voids positional mapping, keywords bind by name).
        let mut slots: Vec<AbstractValue> = vec![AbstractValue::Unknown; params.len()];
        let star = call
            .args
            .iter()
            .any(|arg| matches!(arg, ast::Expr::Starred(_)));
        let mut next = usize::from(binds_scene_receiver);
        for arg in &call.args {
            if let ast::Expr::Starred(starred) = arg {
                self.eval_expr(&starred.value, state);
                continue;
            }
            let value = self.eval_expr(arg, state);
            if !star {
                if let Some(slot) = slots.get_mut(next) {
                    *slot = value;
                }
            }
            next += 1;
        }
        let mut keyword_bindings: Vec<(String, AbstractValue)> = Vec::new();
        let declared_keyword_only: BTreeSet<&str> = def
            .args
            .kwonlyargs
            .iter()
            .map(|arg| arg.def.arg.as_str())
            .collect();
        for keyword in &call.keywords {
            let value = self.eval_expr(&keyword.value, state);
            if let Some(name) = &keyword.arg {
                if let Some(position) = params.iter().position(|param| param == name.as_str()) {
                    slots[position] = value;
                } else if declared_keyword_only.contains(name.as_str()) {
                    keyword_bindings.push((name.to_string(), value));
                }
            }
        }

        // The callee environment: every declared name binds explicitly
        // (an unbound parameter must not fall back to a same-named
        // module function), the receiver binds to the scene.
        let mut bindings: BTreeMap<String, AbstractValue> = BTreeMap::new();
        for name in &declared_keyword_only {
            bindings.insert((*name).to_owned(), AbstractValue::Unknown);
        }
        for (position, name) in params.iter().enumerate() {
            bindings.insert(name.clone(), slots[position].clone());
        }
        for (name, value) in keyword_bindings {
            bindings.insert(name, value);
        }
        for arg in [&def.args.vararg, &def.args.kwarg].into_iter().flatten() {
            bindings.insert(arg.arg.to_string(), AbstractValue::Unknown);
        }
        if binds_scene_receiver {
            if let Some(receiver) = params.first() {
                bindings.insert(receiver.clone(), AbstractValue::SelfScene);
            }
        }

        // Switch to the callee frame.
        let child_context = self.call_context.push(call_site);
        let saved_file = self.file;
        let saved_module = std::mem::replace(&mut self.module, def.module.clone());
        let saved_class = std::mem::replace(&mut self.current_class, def.class.clone());
        let method_name = qualified.rsplit('.').next().unwrap_or(qualified).to_owned();
        let saved_method = std::mem::replace(&mut self.current_method, method_name);
        let saved_context = std::mem::replace(&mut self.call_context, child_context.clone());
        let saved_block = self.block;
        let saved_base = std::mem::replace(&mut self.base_block, self.block);
        let saved_forced = self.forced_certainty.take();
        let saved_body_site = self.body_site;
        self.file = def.file;
        self.body_site = AllocationSite::new(def.file, def.range);
        self.inline_stack.push((qualified.to_owned(), call_site));

        let mut callee_state = state.clone();
        callee_state.env = bindings;
        callee_state.super_called = Presence::Absent;
        self.record_entry_snapshot(&callee_state);
        let exit = self.run_body(def.body, &callee_state);

        self.inline_stack.pop();
        self.file = saved_file;
        self.module = saved_module;
        self.current_class = saved_class;
        self.current_method = saved_method;
        self.call_context = saved_context;
        self.block = saved_block;
        self.base_block = saved_base;
        self.forced_certainty = saved_forced;
        self.body_site = saved_body_site;

        // The callee's heap effects flow back; the caller keeps its own
        // bindings and `super_called` flag.
        state.heap = exit.heap;
        state.animations = exit.animations;
        state.replacement_targets = exit.replacement_targets;
        state.attrs = exit.attrs;
        state.definite_children = exit.definite_children;

        // Return alias from the converged summary, with the actual
        // argument values substituted (the same lookup discipline as
        // apply_summary_call; a fresh id only resolves when the inline
        // run allocated it under the same context and cardinality).
        let returns = self
            .ctx
            .summaries
            .get(qualified)
            .map_or(SummaryReturn::Unknown, |summary| summary.returns.clone());
        match returns {
            SummaryReturn::SelfValue => AbstractValue::SelfScene,
            SummaryReturn::Param(position) => slots
                .get(position as usize)
                .cloned()
                .unwrap_or(AbstractValue::Unknown),
            SummaryReturn::Fresh(site) => {
                let id = ObjectId::new(site, child_context, self.block.cardinality());
                if state.animations.contains_key(&id) {
                    AbstractValue::Animation(id)
                } else if state.heap.object(&id).is_some() {
                    AbstractValue::Object(id)
                } else {
                    AbstractValue::Unknown
                }
            }
            SummaryReturn::Unknown => AbstractValue::Unknown,
        }
    }

    // -- summary application -------------------------------------------------

    pub(super) fn apply_summary_call(
        &mut self,
        qualified: &str,
        self_value: Option<AbstractValue>,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let Some(summary) = self.ctx.summaries.get(qualified).cloned() else {
            self.eval_args_and_widen(call, state);
            return AbstractValue::Unknown;
        };
        let is_method = self
            .ctx
            .defs
            .defs
            .get(qualified)
            .is_some_and(|def| def.class.is_some());

        // Bind arguments to parameter slots.
        let mut slots: Vec<AbstractValue> = vec![AbstractValue::Unknown; summary.params.len()];
        let mut next = usize::from(is_method);
        if is_method {
            if let (Some(slot), Some(value)) = (slots.first_mut(), self_value.as_ref()) {
                *slot = value.clone();
            }
        }
        let star = fact.is_some_and(|fact| fact.has_star_args);
        for arg in &call.args {
            if let ast::Expr::Starred(starred) = arg {
                self.eval_expr(&starred.value, state);
                continue;
            }
            let value = self.eval_expr(arg, state);
            if !star {
                if let Some(slot) = slots.get_mut(next) {
                    *slot = value;
                }
            }
            next += 1;
        }
        for keyword in &call.keywords {
            let value = self.eval_expr(&keyword.value, state);
            if let Some(name) = &keyword.arg {
                if let Some(position) = summary
                    .params
                    .iter()
                    .position(|param| param == name.as_str())
                {
                    slots[position] = value;
                }
            }
        }

        let call_site = self.site(call.range());
        let child_context = self.call_context.push(call_site);
        for event in &summary.events {
            self.apply_summary_event(event, &slots, self_value.as_ref(), &child_context, state);
        }
        self.rehydrate_summary_plays(&summary, call_site);
        match &summary.returns {
            SummaryReturn::SelfValue => self_value.unwrap_or(AbstractValue::Unknown),
            SummaryReturn::Param(position) => slots
                .get(*position as usize)
                .cloned()
                .unwrap_or(AbstractValue::Unknown),
            SummaryReturn::Fresh(site) => {
                let id = ObjectId::new(*site, child_context, self.block.cardinality());
                if state.animations.contains_key(&id) {
                    AbstractValue::Animation(id)
                } else if state.heap.object(&id).is_some() {
                    AbstractValue::Object(id)
                } else {
                    AbstractValue::Unknown
                }
            }
            SummaryReturn::Unknown => AbstractValue::Unknown,
        }
    }

    /// Materializes the summary's play records as summary-derived
    /// [`PlayFact`]s (final emitting pass only).
    ///
    /// The DESIGN §3.2 pipeline is *not* re-run: only the record's
    /// syntactic facts survive (site, kind, literal duration, argument
    /// sites, stop condition / frozen frame / `*args` flags), every
    /// caller-state-dependent judgment degrades to `Maybe` / `Unknown`,
    /// certainty is capped at `Maybe`, and repetitions are the open
    /// `[0, ∞)` interval — a summarized site (recursion in particular)
    /// may execute any number of times per caller run (the exact
    /// populated-vs-degraded contract is documented on
    /// `SceneLifecycle::summary_derived_plays`). During summary runs the
    /// same push feeds the enclosing extraction, which is how records
    /// propagate transitively through helper chains.
    fn rehydrate_summary_plays(&mut self, summary: &MethodSummary, call_site: AllocationSite) {
        if !self.emit {
            return;
        }
        for record in &summary.plays {
            let group = self.play_counter;
            self.play_counter += 1;
            let mut call_path: Vec<AllocationSite> =
                self.inline_stack.iter().map(|(_, site)| *site).collect();
            call_path.push(call_site);
            let animations = record
                .animation_sites
                .iter()
                .map(|site| PlayedAnimation {
                    site: *site,
                    animation: None,
                    state: None,
                    replacement_target: None,
                    from_builder: false,
                    convertible: Truth::Maybe,
                    channels_known: Truth::Maybe,
                })
                .collect();
            self.sink.plays.push(PlayFact {
                site: record.site,
                play_group: PlayGroupId(group),
                kind: record.kind,
                duration: record.duration.clone(),
                animations,
                dynamic_wait: match record.kind {
                    PlayKind::Wait => Truth::Maybe,
                    PlayKind::Play => Truth::No,
                },
                has_stop_condition: record.has_stop_condition,
                frozen_frame: record.frozen_frame,
                always_update_mobjects: Truth::Maybe,
                star_args: record.star_args,
                certainty: Presence::Maybe,
                repetitions: Num::Interval {
                    lo: Some(0.0),
                    hi: None,
                },
                call_path,
            });
            if self.scene_run {
                self.ctx.mark_summary_play(group);
            }
        }
    }

    fn summary_operand(
        &self,
        operand: &SummaryOperand,
        slots: &[AbstractValue],
        self_value: Option<&AbstractValue>,
        child_context: &CallContextId,
        event: &SummaryEvent,
    ) -> AbstractValue {
        match operand {
            SummaryOperand::SelfRef => self_value.cloned().unwrap_or(AbstractValue::Unknown),
            SummaryOperand::Param(position) => slots
                .get(*position as usize)
                .cloned()
                .unwrap_or(AbstractValue::Unknown),
            SummaryOperand::Fresh(site) => AbstractValue::Object(ObjectId::new(
                *site,
                child_context.clone(),
                self.summary_cardinality(event),
            )),
            SummaryOperand::LiteralBool(flag) => AbstractValue::Literal(LiteralValue::Bool(*flag)),
            SummaryOperand::Opaque => AbstractValue::Unknown,
        }
    }

    fn summary_cardinality(&self, event: &SummaryEvent) -> Cardinality {
        if event.in_definite_loop || self.block.in_definite_loop {
            Cardinality::Many
        } else if event.in_loop || self.block.in_loop() {
            Cardinality::MaybeMany
        } else {
            Cardinality::Singleton
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per summary effect")]
    fn apply_summary_event(
        &mut self,
        event: &SummaryEvent,
        slots: &[AbstractValue],
        self_value: Option<&AbstractValue>,
        child_context: &CallContextId,
        state: &mut ExecState,
    ) {
        let combined = if event.certainty == Presence::Present
            && self.block.certainty() == Presence::Present
        {
            Presence::Present
        } else {
            Presence::Maybe
        };
        let previous_forced = self.forced_certainty;
        self.forced_certainty = Some(combined);
        let operand = |machine: &Self, op: &SummaryOperand| -> AbstractValue {
            machine.summary_operand(op, slots, self_value, child_context, event)
        };
        let object_of = |value: AbstractValue| -> Option<ObjectId> {
            match value {
                AbstractValue::Object(id) => Some(id),
                _ => None,
            }
        };
        let scene_targeted = |value: &AbstractValue| matches!(value, AbstractValue::SelfScene);
        match &event.effect {
            SummaryEffect::Alloc { site, kind } => {
                let id = ObjectId::new(
                    *site,
                    child_context.clone(),
                    self.summary_cardinality(event),
                );
                let mut fresh = MobjectState::fresh(kind.clone());
                // Constructor path facts survive summary application (the
                // kwargs of the original call are unavailable here, so the
                // points-per-curve fact stays Unknown).
                seed_path_counts(&mut fresh, kind, None);
                state.heap.insert_object(id.clone(), fresh);
                self.record(
                    event.site,
                    OpKind::Alloc {
                        object: id,
                        kind: kind.clone(),
                    },
                );
            }
            SummaryEffect::SceneAdd {
                objects,
                reorders_existing,
                foreground,
            } => {
                let mut ids = Vec::new();
                let mut unknown = false;
                for op in objects {
                    match operand(self, op) {
                        AbstractValue::Object(id) => ids.push(id),
                        _ => unknown = true,
                    }
                }
                if unknown {
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.roots.order_known = scene.roots.order_known.join(Truth::Maybe);
                    }
                }
                self.scene_add(
                    &ids,
                    event.site,
                    *reorders_existing,
                    *foreground,
                    combined,
                    state,
                );
            }
            SummaryEffect::SceneRemove { objects } => {
                let ids: Vec<ObjectId> = objects
                    .iter()
                    .filter_map(|op| object_of(operand(self, op)))
                    .collect();
                self.scene_remove(&ids, event.site, combined, state);
            }
            SummaryEffect::AddChild { parent, child } => {
                if let (Some(parent), Some(child)) = (
                    object_of(operand(self, parent)),
                    object_of(operand(self, child)),
                ) {
                    self.add_child(&parent, &child, event.site, combined, state);
                }
            }
            SummaryEffect::RemoveChild { parent, child } => {
                if let (Some(parent), Some(child)) = (
                    object_of(operand(self, parent)),
                    object_of(operand(self, child)),
                ) {
                    self.remove_child(&parent, &child, event.site, combined, state);
                }
            }
            SummaryEffect::RegisterUpdater {
                target,
                scene_level,
                updater,
            } => {
                let value = operand(self, target);
                if *scene_level || scene_targeted(&value) {
                    self.register_updater(None, updater.clone(), event.site, state);
                } else if let Some(id) = object_of(value) {
                    self.register_updater(Some(&id), updater.clone(), event.site, state);
                }
            }
            SummaryEffect::RemoveUpdater {
                target,
                scene_level,
                callback,
            } => {
                let value = operand(self, target);
                if *scene_level || scene_targeted(&value) {
                    self.remove_updater(None, callback, event.site, state);
                } else if let Some(id) = object_of(value) {
                    self.remove_updater(Some(&id), callback, event.site, state);
                }
            }
            SummaryEffect::ClearUpdaters { target } => {
                if let Some(id) = object_of(operand(self, target)) {
                    if combined == Presence::Present {
                        if let Some(object) = state.heap.object_mut(&id) {
                            object.updaters.clear();
                        }
                    }
                    self.record(event.site, OpKind::ClearUpdaters { target: id });
                }
            }
            SummaryEffect::Mutate { target, kind } => {
                if let Some(id) = object_of(operand(self, target)) {
                    if *kind == MutationKind::PathTopology {
                        // A helper changed the path topology; the exact
                        // arithmetic does not survive summarization, so the
                        // counts widen instead of staying stale.
                        if let Some(object) = state.heap.object_mut(&id) {
                            object.point_count = Num::Unknown;
                            object.curve_count = Num::Unknown;
                            object.subpath_count = Num::Unknown;
                        }
                    }
                    if *kind == MutationKind::Style {
                        // A helper's Style mutate may be a `set_z_index`
                        // (recorded under the style channel); the exact z
                        // write does not survive summarization, so the
                        // fact widens instead of staying stale.
                        widen_z_index_family(state, &id);
                    }
                    self.mutate(&id, *kind, event.site, state);
                }
            }
            SummaryEffect::GenerateTarget { target } => {
                if let Some(id) = object_of(operand(self, target)) {
                    self.generate_target(&id, event.site, combined, state);
                }
            }
            SummaryEffect::SaveState { target } => {
                if let Some(id) = object_of(operand(self, target)) {
                    self.save_state(&id, event.site, combined, state);
                }
            }
            SummaryEffect::CreateAnimation {
                site,
                state: template,
                targets,
                replacement_target,
                requires_target,
                requires_saved_state,
            } => {
                let id = ObjectId::new(
                    *site,
                    child_context.clone(),
                    self.summary_cardinality(event),
                );
                let mut animation = template.clone();
                animation.targets = targets
                    .iter()
                    .filter_map(|op| object_of(operand(self, op)))
                    .collect();
                let replacement = replacement_target
                    .as_ref()
                    .and_then(|op| object_of(operand(self, op)));
                if let Some(target) = &replacement {
                    state.replacement_targets.insert(id.clone(), target.clone());
                }
                if self.emit && (*requires_target || *requires_saved_state) {
                    let target = animation.targets.iter().next().cloned();
                    let requirement = if *requires_target {
                        TargetRequirement::GeneratedTarget
                    } else {
                        TargetRequirement::SavedState
                    };
                    let presence = target.as_ref().map_or(Presence::Maybe, |target| {
                        state.heap.object(target).map_or(Presence::Maybe, |object| {
                            if *requires_target {
                                object.generated_target.presence
                            } else {
                                object.saved_state
                            }
                        })
                    });
                    self.sink.target_requirements.push(TargetRequirementFact {
                        site: event.site,
                        requirement,
                        target,
                        presence,
                    });
                }
                let target_list: Vec<ObjectId> = animation.targets.iter().cloned().collect();
                state.animations.insert(id.clone(), animation.clone());
                self.record(
                    event.site,
                    OpKind::CreateAnimation {
                        animation: id,
                        state: animation,
                        targets: target_list,
                        replacement_target: replacement,
                        requires_target: *requires_target,
                        requires_saved_state: *requires_saved_state,
                    },
                );
            }
            SummaryEffect::SetSelfAttr { name, value } => {
                if matches!(self_value, Some(AbstractValue::SelfScene)) {
                    let mapped = operand(self, value);
                    if name == "always_update_mobjects" {
                        let tracked = match &mapped {
                            AbstractValue::Literal(LiteralValue::Bool(flag)) => Truth::from(*flag),
                            _ => Truth::Maybe,
                        };
                        if let Some(scene) = self.scene_state_mut(state) {
                            scene.always_update_mobjects = if combined == Presence::Present {
                                tracked
                            } else {
                                scene.always_update_mobjects.join(tracked)
                            };
                        }
                    }
                    let stored = if combined == Presence::Present {
                        mapped
                    } else {
                        match state.attrs.get(name) {
                            Some(existing) => join_values(existing, &mapped),
                            None => AbstractValue::Unknown,
                        }
                    };
                    state.attrs.insert(name.clone(), stored);
                }
            }
            SummaryEffect::UnknownMutation {
                values,
                includes_scene,
            } => {
                let ids: Vec<ObjectId> = values
                    .iter()
                    .filter_map(|op| object_of(operand(self, op)))
                    .collect();
                let scene =
                    *includes_scene || values.iter().any(|op| scene_targeted(&operand(self, op)));
                self.unknown_mutation(&ids, scene, event.site, state);
            }
        }
        self.forced_certainty = previous_forced;
    }
}

// ---------------------------------------------------------------------------
// Summary extraction (used by `semantic::summaries`).
// ---------------------------------------------------------------------------

/// Summarizes one project callable by abstractly executing its body with
/// placeholder objects bound to the parameters, then translating the
/// recorded operations into parameter-relative effects.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "placeholder setup, execution, and translation form one pipeline"
)]
pub(crate) fn summarize_callable(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    defs: &DefMap<'_>,
    table: &SummaryTable,
    qualified_name: &str,
) -> MethodSummary {
    let _ = sources;
    let Some(def) = defs.defs.get(qualified_name) else {
        return MethodSummary::seed(qualified_name, Vec::new());
    };
    let ctx = Ctx::new(index, calls, knowledge, defs, table);
    let params = def.param_names();
    let is_method = def.class.is_some();
    let is_scene_method = def
        .class
        .as_deref()
        .is_some_and(|class| index.scene_classes.contains(class));
    let (mro, _) = def
        .class
        .as_deref()
        .map_or((Vec::new(), false), |class| linearize_project(index, class));
    let reached_bases = def
        .class
        .as_deref()
        .and_then(|class| index.classes.get(class))
        .map(|record| record.reached_bases.clone())
        .unwrap_or_default();

    let scene_id = ObjectId::new(
        AllocationSite::new(def.file, def.range),
        CallContextId::empty(),
        Cardinality::Singleton,
    );
    let mut heap = AbstractHeap::new();
    heap.insert_scene(scene_id.clone(), SceneState::initial(KindSet::Unknown));
    let mut state = ExecState::new(heap);
    let mut placeholders = BTreeMap::new();
    let positional: Vec<&ast::ArgWithDefault> =
        def.args.posonlyargs.iter().chain(&def.args.args).collect();
    for (position, arg) in positional.iter().enumerate() {
        let name = arg.def.arg.as_str();
        if position == 0 && is_method && name == "self" && is_scene_method {
            state
                .env
                .insert("self".to_owned(), AbstractValue::SelfScene);
            continue;
        }
        let placeholder = ObjectId::new(
            AllocationSite::new(def.file, arg.def.range()),
            CallContextId::empty(),
            Cardinality::Singleton,
        );
        state
            .heap
            .insert_object(placeholder.clone(), MobjectState::fresh(KindSet::Unknown));
        placeholders.insert(
            placeholder.clone(),
            u32::try_from(position).unwrap_or(u32::MAX),
        );
        state
            .env
            .insert(name.to_owned(), AbstractValue::Object(placeholder));
    }

    let method_name = qualified_name
        .rsplit('.')
        .next()
        .unwrap_or(qualified_name)
        .to_owned();
    let mut sink = TraceSink::default();
    let mut machine = Machine {
        ctx: &ctx,
        sink: &mut sink,
        file: def.file,
        module: def.module.clone(),
        scene_id: scene_id.clone(),
        mro,
        reached_bases,
        current_class: def.class.clone(),
        current_method: method_name,
        call_context: CallContextId::empty(),
        play_counter: 0,
        emit: true,
        snapshot: false,
        block: BlockCtx::default(),
        forced_certainty: None,
        scene_run: false,
        inline_stack: Vec::new(),
        base_block: BlockCtx::default(),
        body_site: AllocationSite::new(def.file, def.range),
    };
    let _exit = machine.run_body(def.body, &state);

    let span = (def.file, def.range);
    let translate = |id: &ObjectId| -> SummaryOperand {
        if *id == scene_id {
            return SummaryOperand::SelfRef;
        }
        if let Some(position) = placeholders.get(id) {
            return SummaryOperand::Param(*position);
        }
        if id.site.file == span.0
            && id.site.start >= u32::from(span.1.start())
            && id.site.end <= u32::from(span.1.end())
        {
            return SummaryOperand::Fresh(id.site);
        }
        SummaryOperand::Opaque
    };
    let value_operand = |value: &AbstractValue| -> SummaryOperand {
        match value {
            AbstractValue::SelfScene => SummaryOperand::SelfRef,
            AbstractValue::Object(id) | AbstractValue::Animation(id) => translate(id),
            _ => SummaryOperand::Opaque,
        }
    };

    let mut events = Vec::new();
    for op in &sink.ops {
        let effect = translate_op(op, &translate);
        if let Some(effect) = effect {
            events.push(SummaryEvent {
                certainty: op.certainty,
                in_loop: op.in_loop,
                in_definite_loop: op.in_definite_loop,
                site: op.site,
                effect,
            });
        }
    }

    // Play / wait records: one per site and kind, in body order. The
    // summary-run play facts carry caller-state-dependent judgments made
    // against placeholder state (auto-add certainty, wait dynamics,
    // suspension) — only the syntactic fields survive into the record;
    // rehydration degrades everything else (never a wrong fact).
    // Rehydrated callee records already sit in `sink.plays`, so records
    // propagate transitively; deduplication by site keeps recursive SCC
    // iteration from growing the list unboundedly.
    let mut plays: Vec<SummaryPlay> = Vec::new();
    let mut play_sites: BTreeSet<(AllocationSite, bool)> = BTreeSet::new();
    for fact in &sink.plays {
        if !play_sites.insert((fact.site, matches!(fact.kind, PlayKind::Wait))) {
            continue;
        }
        plays.push(SummaryPlay {
            site: fact.site,
            kind: fact.kind,
            duration: fact.duration.clone(),
            animation_sites: fact.animations.iter().map(|played| played.site).collect(),
            has_stop_condition: fact.has_stop_condition,
            frozen_frame: fact.frozen_frame,
            star_args: fact.star_args,
            certainty: fact.certainty,
        });
    }

    // Return alias: all return paths must agree.
    let mut returns: Option<SummaryReturn> = None;
    for observation in &sink.returns {
        let this = match value_operand(&observation.value) {
            SummaryOperand::SelfRef => SummaryReturn::SelfValue,
            SummaryOperand::Param(position) => SummaryReturn::Param(position),
            SummaryOperand::Fresh(site) => SummaryReturn::Fresh(site),
            SummaryOperand::LiteralBool(_) | SummaryOperand::Opaque => SummaryReturn::Unknown,
        };
        returns = Some(match returns {
            None => this,
            Some(previous) if previous == this => previous,
            Some(_) => SummaryReturn::Unknown,
        });
    }
    // Methods returning `self` conventionally: only trust explicit
    // returns; a fall-off-the-end path yields Unknown only when no
    // explicit return exists at all.
    let returns = returns.unwrap_or(SummaryReturn::Unknown);

    // Return-path classification (MLC123): "no return on some path" is a
    // definite No for `returns_mobject`; "returns an untracked value" is
    // only Maybe. Parameter placeholders are bound as mobjects, so a
    // returned parameter (or a fluent chain on one) counts as Yes — the
    // assumption under which `ApplyFunction` invokes its callback.
    let mut has_bare = false;
    let mut any_definite_non_mobject = false;
    let mut any_untracked = false;
    let mut any_mobject = false;
    for observation in &sink.returns {
        if observation.bare {
            has_bare = true;
            continue;
        }
        match &observation.value {
            AbstractValue::Object(_) => any_mobject = true,
            AbstractValue::Unknown => any_untracked = true,
            // Animations, builders, callables, the scene itself, and
            // literal constants are definitely not mobjects.
            AbstractValue::Animation(_)
            | AbstractValue::Builder(_)
            | AbstractValue::Callable(..)
            | AbstractValue::SelfScene
            | AbstractValue::Literal(_) => any_definite_non_mobject = true,
        }
    }
    let returns_mobject = if has_bare || sink.fall_off_end || any_definite_non_mobject {
        Truth::No
    } else if any_mobject && !any_untracked {
        Truth::Yes
    } else {
        // Untracked return values — or no normal return path at all (the
        // body always raises): never a definite verdict.
        Truth::Maybe
    };
    let return_fact = ReturnFact {
        returns_mobject,
        has_bare_return_path: Truth::from(has_bare),
        has_no_return_path: Truth::from(sink.fall_off_end),
    };

    MethodSummary {
        qualified_name: qualified_name.to_owned(),
        params,
        events,
        plays,
        returns,
        return_fact,
        converged: true,
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per op kind")]
fn translate_op(
    op: &SinkOp,
    translate: &dyn Fn(&ObjectId) -> SummaryOperand,
) -> Option<SummaryEffect> {
    match &op.op {
        OpKind::Alloc { object, kind } => match translate(object) {
            SummaryOperand::Fresh(site) => Some(SummaryEffect::Alloc {
                site,
                kind: kind.clone(),
            }),
            _ => None,
        },
        OpKind::SceneAdd {
            objects,
            reorders_existing,
            foreground,
            ..
        } => Some(SummaryEffect::SceneAdd {
            objects: objects.iter().map(translate).collect(),
            reorders_existing: *reorders_existing,
            foreground: *foreground,
        }),
        OpKind::SceneRemove { objects } => Some(SummaryEffect::SceneRemove {
            objects: objects.iter().map(translate).collect(),
        }),
        OpKind::AddChild { parent, child } => Some(SummaryEffect::AddChild {
            parent: translate(parent),
            child: translate(child),
        }),
        OpKind::RemoveChild { parent, child } => Some(SummaryEffect::RemoveChild {
            parent: translate(parent),
            child: translate(child),
        }),
        OpKind::RegisterUpdater {
            target,
            scene_level,
            updater,
        } => Some(SummaryEffect::RegisterUpdater {
            target: target.as_ref().map_or(SummaryOperand::SelfRef, translate),
            scene_level: *scene_level,
            updater: updater.clone(),
        }),
        OpKind::RemoveUpdater {
            target,
            scene_level,
            callback,
        } => Some(SummaryEffect::RemoveUpdater {
            target: target.as_ref().map_or(SummaryOperand::SelfRef, translate),
            scene_level: *scene_level,
            callback: callback.clone(),
        }),
        OpKind::ClearUpdaters { target } => Some(SummaryEffect::ClearUpdaters {
            target: translate(target),
        }),
        OpKind::Mutate { target, kind } => Some(SummaryEffect::Mutate {
            target: translate(target),
            kind: *kind,
        }),
        OpKind::GenerateTarget { target, .. } => Some(SummaryEffect::GenerateTarget {
            target: translate(target),
        }),
        OpKind::SaveState { target } => Some(SummaryEffect::SaveState {
            target: translate(target),
        }),
        OpKind::CreateAnimation {
            state,
            targets,
            replacement_target,
            requires_target,
            requires_saved_state,
            animation,
        } => {
            let SummaryOperand::Fresh(site) = translate(animation) else {
                return None;
            };
            let mut template = state.clone();
            template.targets = BTreeSet::new();
            Some(SummaryEffect::CreateAnimation {
                site,
                state: template,
                targets: targets.iter().map(translate).collect(),
                replacement_target: replacement_target.as_ref().map(translate),
                requires_target: *requires_target,
                requires_saved_state: *requires_saved_state,
            })
        }
        OpKind::SetSelfAttr {
            name,
            value,
            literal_bool,
        } => Some(SummaryEffect::SetSelfAttr {
            name: name.clone(),
            value: match (value, literal_bool) {
                (Some(id), _) => translate(id),
                (None, Some(flag)) => SummaryOperand::LiteralBool(*flag),
                (None, None) => SummaryOperand::Opaque,
            },
        }),
        OpKind::BeginPlay { .. }
        | OpKind::SuspendUpdater { .. }
        | OpKind::ResumeUpdater { .. }
        | OpKind::FinishPlay { .. }
        | OpKind::RendererDivergentMembership => None,
        OpKind::UnknownMutation {
            values,
            includes_scene,
        } => Some(SummaryEffect::UnknownMutation {
            values: values.iter().map(translate).collect(),
            includes_scene: *includes_scene,
        }),
    }
}
