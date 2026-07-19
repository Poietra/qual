//! Abstract execution state and the CFG fixpoint walk of the lifecycle
//! machine (DESIGN §5.5, §5.7): the abstract value / execution-state
//! lattice, the recording sink, per-block loop / branch context, and
//! statement / expression evaluation with branch joins to `MAYBE` and
//! bounded loop evaluation with widening.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::frontend::cfg::{BasicBlock, CfgStmt, ControlFlowGraph, Terminator};
use crate::frontend::index::{CallableSignature, LiteralFact, ReceiverKind};
use crate::semantic::events::{CleanupEffect, MutationKind};
use crate::semantic::heap::AbstractHeap;
use crate::semantic::state::{AnimationState, CallbackRef, UpdaterFact, WriteChannel};
use crate::semantic::values::{
    AllocationSite, CallContextId, Cardinality, KindSet, Num, ObjectId, Presence, Truth,
};
use crate::source::FileId;

use super::callbacks::signature_from_args;
use super::dispatch::Ctx;
use super::mro::super_call_method;
use super::{
    AnimateBuilderFact, FallbackFact, FixedRegistrationFact, PlayFact, SceneRemovalFact,
    StateSnapshot, TargetRequirementFact, UpdaterRegistration, UpdaterRemoval,
};

/// Loop passes before widening kicks in at loop headers (DESIGN §5.5:
/// evaluate 0 and 1 iterations, fixed point at most 3).
const WIDEN_AFTER_PASS: usize = 3;
/// Hard cap on fixpoint passes over one body.
const MAX_PASSES: usize = 6;

// ---------------------------------------------------------------------------
// Abstract values and execution state.
// ---------------------------------------------------------------------------

/// A `.animate` builder value.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct BuilderValue {
    pub(super) site: AllocationSite,
    /// The helper call sites on the inline stack when the builder was
    /// created, outermost first: the execution-context part of the
    /// builder's fact key. A builder created in one frame and played in
    /// another still updates its own creation execution's fact.
    pub(super) call_path: Vec<AllocationSite>,
    pub(super) target: Option<ObjectId>,
    pub(super) methods: Vec<String>,
    /// Source span of each entry in `methods`, narrowed to the method name.
    pub(super) method_sites: Vec<AllocationSite>,
    pub(super) channels: BTreeSet<WriteChannel>,
    pub(super) channels_known: Truth,
}

/// Coarse classification of a literal constant value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum LiteralValue {
    /// A `True` / `False` literal.
    Bool(bool),
    /// An int / float literal.
    Number,
    /// A string / bytes literal.
    Str,
    /// The `None` literal.
    NoneLit,
}

/// The abstract value of an expression.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum AbstractValue {
    /// A tracked mobject.
    Object(ObjectId),
    /// A tracked animation.
    Animation(ObjectId),
    /// A `.animate` builder.
    Builder(BuilderValue),
    /// A callable with identity and (when known) declared signature.
    Callable(CallbackRef, Option<CallableSignature>),
    /// The scene instance (`self` in scene methods).
    SelfScene,
    /// A literal constant (definitely not a mobject / animation).
    Literal(LiteralValue),
    /// Anything else.
    Unknown,
}

pub(super) fn join_values(a: &AbstractValue, b: &AbstractValue) -> AbstractValue {
    if a == b {
        a.clone()
    } else {
        AbstractValue::Unknown
    }
}

fn join_value_maps(
    a: &BTreeMap<String, AbstractValue>,
    b: &BTreeMap<String, AbstractValue>,
) -> BTreeMap<String, AbstractValue> {
    let mut joined = BTreeMap::new();
    for key in a.keys().chain(b.keys()) {
        if joined.contains_key(key) {
            continue;
        }
        let value = match (a.get(key), b.get(key)) {
            (Some(left), Some(right)) => join_values(left, right),
            _ => AbstractValue::Unknown,
        };
        joined.insert(key.clone(), value);
    }
    joined
}

fn snapshot_object_bindings(state: &ExecState) -> BTreeMap<String, ObjectId> {
    let mut bindings = BTreeMap::new();
    for (name, value) in &state.env {
        if let AbstractValue::Object(id) = value {
            bindings.insert(name.clone(), state.heap.resolve(id));
        }
    }
    for (name, value) in &state.attrs {
        if let AbstractValue::Object(id) = value {
            bindings.insert(format!("self.{name}"), state.heap.resolve(id));
        }
    }
    bindings
}

/// The full abstract state at one program point.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ExecState {
    pub(super) heap: AbstractHeap,
    /// Animation states keyed by the animation object's id.
    pub(super) animations: BTreeMap<ObjectId, AnimationState>,
    /// Replacement targets (second Transform-family constructor argument)
    /// keyed by animation id; drives `Scene.replace` at play cleanup.
    pub(super) replacement_targets: BTreeMap<ObjectId, ObjectId>,
    /// Local variable bindings of the current frame.
    pub(super) env: BTreeMap<String, AbstractValue>,
    /// `self.<attr>` bindings of the scene instance (shared across the
    /// lifecycle methods).
    pub(super) attrs: BTreeMap<String, AbstractValue>,
    /// Parent → children edges that hold on **every** path (joins
    /// intersect them). `MobjectState::children` keeps the possible
    /// edges.
    pub(super) definite_children: BTreeMap<ObjectId, BTreeSet<ObjectId>>,
    /// Whether `super().<current method>()` was called on this path.
    pub(super) super_called: Presence,
}

impl ExecState {
    pub(super) fn new(heap: AbstractHeap) -> Self {
        Self {
            heap,
            animations: BTreeMap::new(),
            replacement_targets: BTreeMap::new(),
            env: BTreeMap::new(),
            attrs: BTreeMap::new(),
            definite_children: BTreeMap::new(),
            super_called: Presence::Absent,
        }
    }

    fn join(a: &Self, b: &Self) -> Self {
        let mut animations = BTreeMap::new();
        for key in a.animations.keys().chain(b.animations.keys()) {
            if animations.contains_key(key) {
                continue;
            }
            let value = match (a.animations.get(key), b.animations.get(key)) {
                (Some(left), Some(right)) => left.join(right),
                (Some(only), None) | (None, Some(only)) => only.clone(),
                (None, None) => unreachable!("key from one of the maps"),
            };
            animations.insert(key.clone(), value);
        }
        let mut definite_children = BTreeMap::new();
        for (parent, left) in &a.definite_children {
            if let Some(right) = b.definite_children.get(parent) {
                let both: BTreeSet<ObjectId> = left.intersection(right).cloned().collect();
                if !both.is_empty() {
                    definite_children.insert(parent.clone(), both);
                }
            }
        }
        let mut replacement_targets = a.replacement_targets.clone();
        for (animation, target) in &b.replacement_targets {
            match replacement_targets.get(animation) {
                Some(existing) if existing != target => {
                    replacement_targets.remove(animation);
                }
                Some(_) => {}
                None => {
                    replacement_targets.insert(animation.clone(), target.clone());
                }
            }
        }
        Self {
            heap: AbstractHeap::join(&a.heap, &b.heap),
            animations,
            replacement_targets,
            env: join_value_maps(&a.env, &b.env),
            attrs: join_value_maps(&a.attrs, &b.attrs),
            definite_children,
            super_called: a.super_called.join(b.super_called),
        }
    }

    fn widen(previous: &Self, next: &Self) -> Self {
        let mut widened = Self::join(previous, next);
        widened.heap = AbstractHeap::widen(&previous.heap, &next.heap);
        widened
    }
}

// ---------------------------------------------------------------------------
// Sink: everything one scene run (or one summary run) records.
// ---------------------------------------------------------------------------

/// One primitive operation, recorded uniformly for the public event trace
/// (scene runs) and for summary extraction (helper runs).
#[derive(Debug, Clone)]
pub(super) struct SinkOp {
    pub(super) site: AllocationSite,
    pub(super) certainty: Presence,
    pub(super) in_loop: bool,
    pub(super) in_definite_loop: bool,
    pub(super) op: OpKind,
}

#[derive(Debug, Clone)]
pub(super) enum OpKind {
    Alloc {
        object: ObjectId,
        kind: KindSet,
    },
    SceneAdd {
        objects: Vec<ObjectId>,
        order_effect: bool,
        reorders_existing: bool,
        foreground: bool,
    },
    SceneRemove {
        objects: Vec<ObjectId>,
    },
    AddChild {
        parent: ObjectId,
        child: ObjectId,
    },
    RemoveChild {
        parent: ObjectId,
        child: ObjectId,
    },
    RegisterUpdater {
        target: Option<ObjectId>,
        scene_level: bool,
        updater: UpdaterFact,
    },
    RemoveUpdater {
        target: Option<ObjectId>,
        scene_level: bool,
        callback: CallbackRef,
    },
    ClearUpdaters {
        target: ObjectId,
    },
    Mutate {
        target: ObjectId,
        kind: MutationKind,
    },
    GenerateTarget {
        target: ObjectId,
        copy: ObjectId,
    },
    SaveState {
        target: ObjectId,
    },
    CreateAnimation {
        animation: ObjectId,
        state: AnimationState,
        targets: Vec<ObjectId>,
        replacement_target: Option<ObjectId>,
        requires_target: bool,
        requires_saved_state: bool,
    },
    BeginPlay {
        play_group: u64,
        animations: Vec<ObjectId>,
        duration: Num,
    },
    SuspendUpdater {
        target: ObjectId,
    },
    ResumeUpdater {
        target: ObjectId,
    },
    FinishPlay {
        play_group: u64,
        cleanup: Vec<CleanupEffect>,
    },
    SetSelfAttr {
        name: String,
        value: Option<ObjectId>,
        literal_bool: Option<bool>,
    },
    UnknownMutation {
        values: Vec<ObjectId>,
        includes_scene: bool,
    },
    RendererDivergentMembership,
}

/// One observed `return` statement.
#[derive(Debug, Clone)]
pub(super) struct ReturnObservation {
    /// The returned value (`Unknown` for bare returns).
    pub(super) value: AbstractValue,
    /// The statement was a bare `return` (no expression).
    pub(super) bare: bool,
}

/// Key of one `.animate` builder execution: the builder's source site
/// plus the helper call path it executed under (empty for builders in a
/// lifecycle method body). One builder site reached through two helper
/// call sites is two executions with two facts — like [`PlayFact`]s.
pub(super) type BuilderKey = (AllocationSite, Vec<AllocationSite>);

#[derive(Debug, Default)]
pub(super) struct TraceSink {
    pub(super) ops: Vec<SinkOp>,
    pub(super) snapshots: Vec<StateSnapshot>,
    pub(super) plays: Vec<PlayFact>,
    pub(super) updaters: Vec<UpdaterRegistration>,
    pub(super) updater_removals: Vec<UpdaterRemoval>,
    /// Per-execution builder facts (the working state): every join stays
    /// within one execution context. [`TraceSink::sync_builders`] derives
    /// the public per-site view in [`TraceSink::builders`].
    pub(super) builder_executions: BTreeMap<BuilderKey, AnimateBuilderFact>,
    /// The public per-site view ([`SceneLifecycle::builders`]), rebuilt
    /// from [`TraceSink::builder_executions`] after every builder update.
    pub(super) builders: BTreeMap<AllocationSite, AnimateBuilderFact>,
    pub(super) target_requirements: Vec<TargetRequirementFact>,
    pub(super) scene_removals: Vec<SceneRemovalFact>,
    pub(super) fixed_registrations: Vec<FixedRegistrationFact>,
    pub(super) returns: Vec<ReturnObservation>,
    /// Helper calls that fell back to summaries during this scene run.
    pub(super) inline_fallbacks: Vec<FallbackFact>,
    /// Play groups rehydrated from summaries during this scene run.
    pub(super) summary_play_groups: BTreeSet<u64>,
    /// A reachable CFG path falls off the end of the body without a
    /// `return` (paths ending in `raise` do not count).
    pub(super) fall_off_end: bool,
}

impl TraceSink {
    /// Rebuilds the per-site builder view from the per-execution facts.
    ///
    /// A site with one execution keeps that execution's fact verbatim.
    /// Across several execution contexts the merged fact keeps only what
    /// is sound without mixing executions (DESIGN §15 invariant 2):
    ///
    /// - a disagreeing `target` drops to `None` — no cross-execution
    ///   identity claim;
    /// - disagreeing epoch pairs drop `target_epoch_at_play` — a creation
    ///   epoch from one execution is never compared against a play epoch
    ///   from another;
    /// - `played` takes the strongest per-execution verdict (some
    ///   execution definitely played the builder);
    /// - `overwritten_by_later_builder` keeps `Yes` only from an
    ///   execution that also definitely played (the MLC117 combination
    ///   must hold within a single execution), degrading to `Maybe`
    ///   otherwise;
    /// - chained methods/sites keep the longest syntactic prefix, and
    ///   channels union with their completeness verdict.
    pub(super) fn sync_builders(&mut self) {
        /// `Yes` > `Maybe` > `No` (a may-fact strengthens, never resets).
        const fn strongest(a: Truth, b: Truth) -> Truth {
            match (a, b) {
                (Truth::Yes, _) | (_, Truth::Yes) => Truth::Yes,
                (Truth::Maybe, _) | (_, Truth::Maybe) => Truth::Maybe,
                (Truth::No, Truth::No) => Truth::No,
            }
        }
        self.builders.clear();
        for ((site, _path), fact) in &self.builder_executions {
            match self.builders.get_mut(site) {
                None => {
                    self.builders.insert(*site, fact.clone());
                }
                Some(merged) => {
                    if merged.target != fact.target {
                        merged.target = None;
                    }
                    // The chain is syntactic and identical per execution;
                    // keep the longest observed prefix deterministically.
                    if fact.methods.len() > merged.methods.len() {
                        merged.methods.clone_from(&fact.methods);
                        merged.method_sites.clone_from(&fact.method_sites);
                    }
                    merged.channels.extend(fact.channels.iter().copied());
                    merged.channels_known = merged.channels_known.join(fact.channels_known);
                    if (merged.target_epoch_at_creation, merged.target_epoch_at_play)
                        != (fact.target_epoch_at_creation, fact.target_epoch_at_play)
                    {
                        merged.target_epoch_at_play = None;
                    }
                    // Gate the overwrite verdicts of both sides on their
                    // own played verdict before the fields merge: the
                    // "played builder was overwritten" combination must
                    // come from one execution, never from a played
                    // execution paired with an overwritten one.
                    let gate = |played: Truth, overwritten: Truth| {
                        if played == Truth::Yes || overwritten != Truth::Yes {
                            overwritten
                        } else {
                            Truth::Maybe
                        }
                    };
                    let merged_overwritten =
                        gate(merged.played, merged.overwritten_by_later_builder);
                    let fact_overwritten = gate(fact.played, fact.overwritten_by_later_builder);
                    merged.overwritten_by_later_builder =
                        strongest(merged_overwritten, fact_overwritten);
                    merged.played = strongest(merged.played, fact.played);
                }
            }
        }
    }
}

/// Per-block execution context while walking a CFG.
#[derive(Debug, Clone, Copy)]
pub(super) struct BlockCtx {
    pub(super) loop_depth: u32,
    pub(super) cond_depth: u32,
    pub(super) in_definite_loop: bool,
    /// Extra loop-ness from comprehension bodies.
    pub(super) comprehension: bool,
    /// Upper bound on per-body-run executions of the current site from
    /// the enclosing loops' literal trip counts
    /// ([`crate::frontend::cfg::BasicBlock::repetitions`]); `None` when
    /// any enclosing trip count is unknown.
    pub(super) repetitions: Option<i64>,
}

impl Default for BlockCtx {
    fn default() -> Self {
        Self {
            loop_depth: 0,
            cond_depth: 0,
            in_definite_loop: false,
            comprehension: false,
            repetitions: Some(1),
        }
    }
}

impl BlockCtx {
    /// The context of a statement at `inner` depths inside a body entered
    /// from a call site at `self` depths (helper inlining): loop and
    /// condition depths add, definite-loop and comprehension contexts
    /// carry over — a play inside a helper called from a branch is a
    /// maybe-fact, and an allocation inside a helper called from a loop
    /// is not a singleton. Repetition bounds multiply (call-site loops ×
    /// helper-internal loops); an unknown factor on either side keeps the
    /// composed bound unknown, and overflow degrades to unknown rather
    /// than wrapping (DESIGN §4.1: never underestimate).
    fn compose(self, inner: Self) -> Self {
        Self {
            loop_depth: self.loop_depth + inner.loop_depth,
            cond_depth: self.cond_depth + inner.cond_depth,
            in_definite_loop: self.in_definite_loop || inner.in_definite_loop,
            comprehension: self.comprehension || inner.comprehension,
            repetitions: match (self.repetitions, inner.repetitions) {
                (Some(outer), Some(body)) => outer.checked_mul(body),
                _ => None,
            },
        }
    }

    pub(super) fn certainty(self) -> Presence {
        if self.cond_depth == 0 && !self.comprehension {
            Presence::Present
        } else {
            Presence::Maybe
        }
    }

    pub(super) fn in_loop(self) -> bool {
        self.loop_depth > 0 || self.comprehension
    }

    /// Executions of the current site per run of the enclosing body, as an
    /// interval (DESIGN §4.1: an unknown factor never collapses to 1):
    /// exactly once outside loops; `[0, product]` under loops whose trip
    /// counts are all literal `range(...)` bounds (`break` / `return` /
    /// `raise` can only reduce the count); open above when any enclosing
    /// trip count is unknown.
    #[allow(
        clippy::cast_precision_loss,
        reason = "counts above 2^53 take the open-bound arm instead"
    )]
    pub(super) fn repetition_bound(self) -> Num {
        /// Largest count the `f64` upper bound represents exactly (2^53).
        const EXACT_LIMIT: i64 = 1 << 53;
        if !self.in_loop() {
            return Num::int(1);
        }
        match self.repetitions {
            Some(count) if count <= EXACT_LIMIT => Num::Interval {
                lo: Some(0.0),
                hi: Some(count as f64),
            },
            _ => Num::Interval {
                lo: Some(0.0),
                hi: None,
            },
        }
    }

    pub(super) fn cardinality(self) -> Cardinality {
        if self.in_definite_loop {
            Cardinality::Many
        } else if self.in_loop() {
            Cardinality::MaybeMany
        } else {
            Cardinality::Singleton
        }
    }
}

// ---------------------------------------------------------------------------
// The machine.
// ---------------------------------------------------------------------------

pub(super) struct Machine<'a, 'b> {
    pub(super) ctx: &'b Ctx<'a>,
    pub(super) sink: &'b mut TraceSink,
    pub(super) file: FileId,
    pub(super) module: String,
    pub(super) scene_id: ObjectId,
    /// Project-local MRO of the scene being run (empty for summary runs
    /// of non-scene callables).
    pub(super) mro: Vec<String>,
    /// External canonical bases reached by the scene class.
    pub(super) reached_bases: BTreeSet<String>,
    pub(super) current_class: Option<String>,
    pub(super) current_method: String,
    pub(super) call_context: CallContextId,
    pub(super) play_counter: u64,
    /// Emit ops / facts (post-convergence final pass only).
    pub(super) emit: bool,
    /// Record statement snapshots (final pass of scene methods only).
    pub(super) snapshot: bool,
    pub(super) block: BlockCtx,
    /// While applying a summary event: its combined certainty overrides
    /// the block certainty for recorded ops and state effects.
    pub(super) forced_certainty: Option<Presence>,
    /// This machine runs a scene lifecycle (not a summary extraction):
    /// only scene runs inline `self.<helper>()` bodies.
    pub(super) scene_run: bool,
    /// Qualified names and call sites of the inlined helper calls
    /// currently on the stack: the recursion guard and the
    /// [`PlayFact::call_path`] source.
    pub(super) inline_stack: Vec<(String, AllocationSite)>,
    /// Depth context of the enclosing call site while executing an
    /// inlined helper body; composed with every block's own depths so
    /// certainty and cardinality reflect the whole call path.
    pub(super) base_block: BlockCtx,
    /// Byte range of the method body currently executing (the `def`);
    /// recorded as every snapshot's scope.
    pub(super) body_site: AllocationSite,
}

impl<'a> Machine<'a, '_> {
    pub(super) fn site(&self, range: TextRange) -> AllocationSite {
        AllocationSite::new(self.file, range)
    }

    pub(super) fn certainty(&self) -> Presence {
        self.forced_certainty
            .unwrap_or_else(|| self.block.certainty())
    }

    pub(super) fn record(&mut self, site: AllocationSite, op: OpKind) {
        if !self.emit {
            return;
        }
        self.sink.ops.push(SinkOp {
            site,
            certainty: self.certainty(),
            in_loop: self.block.in_loop(),
            in_definite_loop: self.block.in_definite_loop,
            op,
        });
    }

    /// Records the state a method body starts from as a zero-width
    /// snapshot at the `def` start. [`SceneLifecycle::state_at`] then
    /// resolves a query at the body's *first* statement (e.g. the
    /// pre-play state of a play that opens the body) to this entry state
    /// instead of finding nothing.
    pub(super) fn record_entry_snapshot(&mut self, state: &ExecState) {
        if !self.snapshot {
            return;
        }
        self.sink.snapshots.push(StateSnapshot {
            site: AllocationSite {
                file: self.body_site.file,
                start: self.body_site.start,
                end: self.body_site.start,
            },
            scope: self.body_site,
            heap: state.heap.clone(),
            object_bindings: snapshot_object_bindings(state),
        });
    }

    // -- body execution over the CFG ---------------------------------------

    pub(super) fn run_body(&mut self, body: &'a [ast::Stmt], initial: &ExecState) -> ExecState {
        let cfg = ControlFlowGraph::build(body);
        let order = cfg.reverse_postorder();
        let preds = cfg.predecessors();
        let mut in_states: Vec<Option<ExecState>> = vec![None; cfg.blocks.len()];
        let mut out_states: Vec<Option<ExecState>> = vec![None; cfg.blocks.len()];
        let outer_emit = self.emit;
        let outer_snapshot = self.snapshot;

        // Facts, ops, and snapshots are recorded only on the final pass
        // below, after the fixpoint converges. On pass 0 a block after a
        // loop has not yet absorbed the loop's back-edge state, so a fact
        // recorded that early can present a pre-fixpoint `Absent` as a
        // converged all-paths truth — exactly the certain/high-on-Maybe
        // violation DESIGN §15.2 forbids (e.g. `generate_target()` inside
        // a loop consumed by `MoveToTarget` after it). The fixpoint passes
        // are therefore pure state computation.
        for pass in 0..MAX_PASSES {
            self.emit = false;
            self.snapshot = false;
            let mut changed = false;
            for &block_id in &order {
                let block = &cfg.blocks[block_id.0];
                let mut input: Option<ExecState> = if block_id == cfg.entry {
                    Some(initial.clone())
                } else {
                    None
                };
                for pred in &preds[block_id.0] {
                    if let Some(out) = &out_states[pred.0] {
                        input = Some(match input {
                            None => out.clone(),
                            Some(current) => ExecState::join(&current, out),
                        });
                    }
                }
                let Some(mut input) = input else {
                    continue;
                };
                if block.is_loop_header && pass >= WIDEN_AFTER_PASS {
                    if let Some(previous) = &in_states[block_id.0] {
                        input = ExecState::widen(previous, &input);
                    }
                }
                if pass > 0 && in_states[block_id.0].as_ref() == Some(&input) {
                    continue;
                }
                changed = true;
                in_states[block_id.0] = Some(input.clone());
                let out = self.exec_block(block, input);
                out_states[block_id.0] = Some(out);
            }
            if !changed {
                break;
            }
        }

        // Final pass over the converged states: facts, ops, snapshots, and
        // the exit state. Every reachable block executes exactly once here,
        // in the same reverse postorder as the fixpoint passes, so the
        // recorded event stream keeps program order while every state read
        // (target/saved-state presence, epochs, membership) reflects the
        // converged join of all paths, including loop back edges.
        self.emit = outer_emit;
        self.snapshot = outer_snapshot;
        let mut exit: Option<ExecState> = None;
        for &block_id in &order {
            let block = &cfg.blocks[block_id.0];
            let Some(input) = in_states[block_id.0].clone() else {
                continue;
            };
            let out = self.exec_block(block, input);
            if self.emit && matches!(block.terminator, Terminator::End) {
                // A reachable block ends the body without `return`: the
                // callable has a no-return path (paths ending in `raise`
                // carry `Terminator::Raise` and do not count).
                self.sink.fall_off_end = true;
            }
            let is_exit = matches!(block.terminator, Terminator::Return(_) | Terminator::End);
            if is_exit {
                exit = Some(match exit {
                    None => out.clone(),
                    Some(current) => ExecState::join(&current, &out),
                });
            }
            out_states[block_id.0] = Some(out);
        }
        self.emit = outer_emit;
        self.snapshot = outer_snapshot;
        exit.unwrap_or_else(|| initial.clone())
    }

    fn exec_block(&mut self, block: &BasicBlock<'a>, mut state: ExecState) -> ExecState {
        self.block = self.base_block.compose(BlockCtx {
            loop_depth: block.loop_depth,
            cond_depth: block.cond_depth,
            in_definite_loop: block.in_definite_loop,
            comprehension: false,
            repetitions: block.repetitions,
        });
        for item in &block.stmts {
            self.exec_cfg_stmt(item, &mut state);
        }
        match &block.terminator {
            Terminator::Branch {
                test: Some(test), ..
            } => {
                self.eval_expr(test, &mut state);
            }
            Terminator::Return(value) => {
                let bare = value.is_none();
                let returned = value.map_or(AbstractValue::Unknown, |expr| {
                    self.eval_expr(expr, &mut state)
                });
                if self.emit {
                    self.sink.returns.push(ReturnObservation {
                        value: returned,
                        bare,
                    });
                }
            }
            _ => {}
        }
        state
    }

    fn exec_cfg_stmt(&mut self, item: &CfgStmt<'a>, state: &mut ExecState) {
        match item {
            CfgStmt::Stmt(stmt) => {
                self.exec_stmt(stmt, state);
                if self.snapshot {
                    self.sink.snapshots.push(StateSnapshot {
                        site: self.site(stmt.range()),
                        scope: self.body_site,
                        heap: state.heap.clone(),
                        object_bindings: snapshot_object_bindings(state),
                    });
                }
            }
            CfgStmt::Eval(expr) => {
                self.eval_expr(expr, state);
            }
            CfgStmt::WithEnter(with_item) => {
                self.eval_expr(&with_item.context_expr, state);
                if let Some(vars) = &with_item.optional_vars {
                    self.bind_target(vars, AbstractValue::Unknown, state);
                }
            }
            CfgStmt::LoopTarget(target) => {
                self.bind_target(target, AbstractValue::Unknown, state);
            }
            CfgStmt::PatternBind(pattern) => {
                let mut names = BTreeSet::new();
                crate::frontend::names::collect_pattern_names(pattern, &mut names);
                for name in names {
                    state.env.insert(name, AbstractValue::Unknown);
                }
            }
        }
    }

    fn exec_stmt(&mut self, stmt: &'a ast::Stmt, state: &mut ExecState) {
        match stmt {
            ast::Stmt::Assign(assign) => {
                let value = self.eval_expr(&assign.value, state);
                for target in &assign.targets {
                    self.bind_target(target, value.clone(), state);
                }
            }
            ast::Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    let value = self.eval_expr(value, state);
                    self.bind_target(&assign.target, value, state);
                }
            }
            ast::Stmt::AugAssign(assign) => {
                self.eval_expr(&assign.value, state);
                self.bind_target(&assign.target, AbstractValue::Unknown, state);
            }
            ast::Stmt::Expr(expr) => {
                self.eval_expr(&expr.value, state);
            }
            ast::Stmt::Raise(raise) => {
                if let Some(exc) = &raise.exc {
                    self.eval_expr(exc, state);
                }
                if let Some(cause) = &raise.cause {
                    self.eval_expr(cause, state);
                }
            }
            ast::Stmt::Assert(assert) => {
                self.eval_expr(&assert.test, state);
                if let Some(message) = &assert.msg {
                    self.eval_expr(message, state);
                }
            }
            ast::Stmt::Delete(delete) => {
                for target in &delete.targets {
                    if let ast::Expr::Name(name) = target {
                        state.env.remove(name.id.as_str());
                    } else {
                        self.eval_expr(target, state);
                    }
                }
            }
            ast::Stmt::FunctionDef(def) => {
                let qualified = format!("{}.{}", self.current_path(), def.name);
                state.env.insert(
                    def.name.to_string(),
                    AbstractValue::Callable(
                        CallbackRef::Named(qualified),
                        Some(signature_from_args(&def.args)),
                    ),
                );
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                let qualified = format!("{}.{}", self.current_path(), def.name);
                state.env.insert(
                    def.name.to_string(),
                    AbstractValue::Callable(
                        CallbackRef::Named(qualified),
                        Some(signature_from_args(&def.args)),
                    ),
                );
            }
            ast::Stmt::ClassDef(def) => {
                state
                    .env
                    .insert(def.name.to_string(), AbstractValue::Unknown);
            }
            // Function-level imports keep bindings untouched (lookups
            // fall back to Unknown); other statement kinds have no
            // straight-line effect.
            ast::Stmt::Global(names) => {
                for name in &names.names {
                    state.env.insert(name.to_string(), AbstractValue::Unknown);
                }
            }
            ast::Stmt::Nonlocal(names) => {
                for name in &names.names {
                    state.env.insert(name.to_string(), AbstractValue::Unknown);
                }
            }
            _ => {}
        }
    }

    fn current_path(&self) -> String {
        match &self.current_class {
            Some(class) => format!("{class}.{}", self.current_method),
            None => format!("{}.{}", self.module, self.current_method),
        }
    }

    fn bind_target(&mut self, target: &'a ast::Expr, value: AbstractValue, state: &mut ExecState) {
        match target {
            ast::Expr::Name(name) => {
                state.env.insert(name.id.to_string(), value);
            }
            ast::Expr::Attribute(attribute) => {
                if let ast::Expr::Name(base) = attribute.value.as_ref() {
                    if base.id.as_str() == "self"
                        && matches!(state.env.get("self"), Some(AbstractValue::SelfScene))
                    {
                        let recorded = match &value {
                            AbstractValue::Object(id) | AbstractValue::Animation(id) => {
                                Some(id.clone())
                            }
                            _ => None,
                        };
                        let literal_bool = match &value {
                            AbstractValue::Literal(LiteralValue::Bool(flag)) => Some(*flag),
                            _ => None,
                        };
                        if attribute.attr.as_str() == "always_update_mobjects" {
                            // A literal write is an exact SceneState fact;
                            // any other write degrades to Maybe instead of
                            // being widened away (MLP227, DESIGN §3.3).
                            let tracked = literal_bool.map_or(Truth::Maybe, Truth::from);
                            if let Some(scene) = self.scene_state_mut(state) {
                                scene.always_update_mobjects = tracked;
                            }
                        }
                        state.attrs.insert(attribute.attr.to_string(), value);
                        self.record(
                            self.site(attribute.range()),
                            OpKind::SetSelfAttr {
                                name: attribute.attr.to_string(),
                                value: recorded,
                                literal_bool,
                            },
                        );
                        return;
                    }
                }
                let base = self.eval_expr(&attribute.value, state);
                if attribute.attr.as_str() == "z_index" {
                    if let AbstractValue::Object(id) = &base {
                        // A raw `mob.z_index = ...` write bypasses
                        // `set_z_index` (receiver only — no family
                        // propagation); the tracked fact widens.
                        let resolved = state.heap.resolve(id);
                        if let Some(object) = state.heap.object_mut(&resolved) {
                            object.z_index = Num::Unknown;
                        }
                    }
                }
            }
            ast::Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.bind_target(element, AbstractValue::Unknown, state);
                }
            }
            ast::Expr::List(list) => {
                for element in &list.elts {
                    self.bind_target(element, AbstractValue::Unknown, state);
                }
            }
            ast::Expr::Starred(starred) => {
                self.bind_target(&starred.value, AbstractValue::Unknown, state);
            }
            ast::Expr::Subscript(subscript) => {
                self.eval_expr(&subscript.value, state);
                self.eval_expr(&subscript.slice, state);
            }
            _ => {}
        }
    }

    // -- expression evaluation ---------------------------------------------

    #[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
    pub(super) fn eval_expr(
        &mut self,
        expr: &'a ast::Expr,
        state: &mut ExecState,
    ) -> AbstractValue {
        match expr {
            ast::Expr::Call(call) => self.eval_call(call, state),
            ast::Expr::Name(name) => self.lookup_name(name.id.as_str(), state),
            ast::Expr::Attribute(attribute) => self.eval_attribute(attribute, expr, state),
            ast::Expr::Lambda(lambda) => AbstractValue::Callable(
                CallbackRef::Lambda(self.site(lambda.range())),
                Some(signature_from_args(&lambda.args)),
            ),
            ast::Expr::NamedExpr(inner) => {
                let value = self.eval_expr(&inner.value, state);
                self.bind_target(&inner.target, value.clone(), state);
                value
            }
            ast::Expr::IfExp(inner) => {
                self.eval_expr(&inner.test, state);
                let a = self.eval_expr(&inner.body, state);
                let b = self.eval_expr(&inner.orelse, state);
                join_values(&a, &b)
            }
            ast::Expr::BoolOp(inner) => {
                for value in &inner.values {
                    self.eval_expr(value, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::BinOp(inner) => {
                self.eval_expr(&inner.left, state);
                self.eval_expr(&inner.right, state);
                AbstractValue::Unknown
            }
            ast::Expr::UnaryOp(inner) => {
                self.eval_expr(&inner.operand, state);
                AbstractValue::Unknown
            }
            ast::Expr::Compare(inner) => {
                self.eval_expr(&inner.left, state);
                for comparator in &inner.comparators {
                    self.eval_expr(comparator, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.eval_expr(element, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.eval_expr(element, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::Set(inner) => {
                for element in &inner.elts {
                    self.eval_expr(element, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::Dict(inner) => {
                for key in inner.keys.iter().flatten() {
                    self.eval_expr(key, state);
                }
                for value in &inner.values {
                    self.eval_expr(value, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::ListComp(inner) => {
                self.eval_comprehension(&inner.generators, &[&inner.elt], state)
            }
            ast::Expr::SetComp(inner) => {
                self.eval_comprehension(&inner.generators, &[&inner.elt], state)
            }
            ast::Expr::GeneratorExp(inner) => {
                self.eval_comprehension(&inner.generators, &[&inner.elt], state)
            }
            ast::Expr::DictComp(inner) => {
                self.eval_comprehension(&inner.generators, &[&inner.key, &inner.value], state)
            }
            ast::Expr::Subscript(inner) => {
                self.eval_expr(&inner.value, state);
                self.eval_expr(&inner.slice, state);
                AbstractValue::Unknown
            }
            ast::Expr::Starred(inner) => {
                self.eval_expr(&inner.value, state);
                AbstractValue::Unknown
            }
            ast::Expr::Slice(inner) => {
                for part in [&inner.lower, &inner.upper, &inner.step]
                    .into_iter()
                    .flatten()
                {
                    self.eval_expr(part, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::Await(inner) => self.eval_expr(&inner.value, state),
            ast::Expr::Yield(inner) => {
                if let Some(value) = &inner.value {
                    self.eval_expr(value, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::YieldFrom(inner) => {
                self.eval_expr(&inner.value, state);
                AbstractValue::Unknown
            }
            ast::Expr::JoinedStr(inner) => {
                for value in &inner.values {
                    self.eval_expr(value, state);
                }
                AbstractValue::Unknown
            }
            ast::Expr::FormattedValue(inner) => {
                self.eval_expr(&inner.value, state);
                AbstractValue::Unknown
            }
            ast::Expr::Constant(constant) => match &constant.value {
                ast::Constant::Bool(flag) => AbstractValue::Literal(LiteralValue::Bool(*flag)),
                ast::Constant::Int(_) | ast::Constant::Float(_) | ast::Constant::Complex { .. } => {
                    AbstractValue::Literal(LiteralValue::Number)
                }
                ast::Constant::Str(_) | ast::Constant::Bytes(_) => {
                    AbstractValue::Literal(LiteralValue::Str)
                }
                ast::Constant::None => AbstractValue::Literal(LiteralValue::NoneLit),
                ast::Constant::Ellipsis | ast::Constant::Tuple(_) => AbstractValue::Unknown,
            },
        }
    }

    fn eval_comprehension(
        &mut self,
        generators: &'a [ast::Comprehension],
        elements: &[&'a ast::Expr],
        state: &mut ExecState,
    ) -> AbstractValue {
        // Bounded single-evaluation summary (DESIGN §5.7): every inner
        // expression is evaluated once with loop cardinality.
        let saved = self.block;
        self.block.comprehension = true;
        // Comprehension iteration counts are not modeled: any site inside
        // gains an open repetition bound, never a fabricated count.
        self.block.repetitions = None;
        for generator in generators {
            self.eval_expr(&generator.iter, state);
            self.bind_target(&generator.target, AbstractValue::Unknown, state);
            for condition in &generator.ifs {
                self.eval_expr(condition, state);
            }
        }
        for element in elements {
            self.eval_expr(element, state);
        }
        self.block = saved;
        AbstractValue::Unknown
    }

    fn lookup_name(&self, name: &str, state: &ExecState) -> AbstractValue {
        if let Some(value) = state.env.get(name) {
            return value.clone();
        }
        // Module-level function definitions are visible from method
        // bodies (final module namespace).
        let qualified = format!("{}.{name}", self.module);
        if self.ctx.defs.defs.contains_key(&qualified) {
            let signature = self.ctx.index.function_signature(&qualified).cloned();
            return AbstractValue::Callable(CallbackRef::Named(qualified), signature);
        }
        AbstractValue::Unknown
    }

    fn eval_attribute(
        &mut self,
        attribute: &'a ast::ExprAttribute,
        _expr: &'a ast::Expr,
        state: &mut ExecState,
    ) -> AbstractValue {
        // `self.<attr>` loads from the tracked instance attributes.
        if let ast::Expr::Name(base) = attribute.value.as_ref() {
            if base.id.as_str() == "self"
                && matches!(state.env.get("self"), Some(AbstractValue::SelfScene))
            {
                return state
                    .attrs
                    .get(attribute.attr.as_str())
                    .cloned()
                    .unwrap_or(AbstractValue::Unknown);
            }
        }
        let base = self.eval_expr(&attribute.value, state);
        match base {
            AbstractValue::Object(id) => match attribute.attr.as_str() {
                "animate" => self.make_builder(&id, attribute.range(), state),
                "target" => state
                    .heap
                    .object(&id)
                    .and_then(|object| object.generated_target.target.clone())
                    .map_or(AbstractValue::Unknown, AbstractValue::Object),
                _ => AbstractValue::Unknown,
            },
            _ => AbstractValue::Unknown,
        }
    }
}

pub(super) fn literal_num(argument: &crate::frontend::index::CallArgument) -> Option<Num> {
    match argument.literal.as_ref()? {
        LiteralFact::Int(value) => Some(Num::int(*value)),
        LiteralFact::Float(value) => Some(Num::float(*value)),
        _ => None,
    }
}

pub(super) fn literal_bool(argument: &crate::frontend::index::CallArgument) -> Option<bool> {
    match argument.literal.as_ref()? {
        LiteralFact::Bool(value) => Some(*value),
        _ => None,
    }
}

impl<'a> Machine<'a, '_> {
    // -- call dispatch ------------------------------------------------------

    fn eval_call(&mut self, call: &'a ast::ExprCall, state: &mut ExecState) -> AbstractValue {
        let fact = self.ctx.fact(self.file, call.range());
        if let Some(method) = super_call_method(call) {
            let method = method.to_owned();
            return self.dispatch_super(&method, call, fact, state);
        }
        if let ast::Expr::Attribute(attribute) = call.func.as_ref() {
            // Module-alias callees (`import manim as mn; mn.Square()`):
            // the frontend classified the receiver as an imported module
            // binding (branch-aware — a name rebound on any path is not
            // `ModuleAlias`) and resolved the canonical candidates, so
            // such a call goes through the same candidate machinery as a
            // direct name. A module attribute can never be a tracked
            // object / builder / scene value, so no method dispatch is
            // lost; without this bridge the call would widen to Unknown
            // and silence every state rule under module-alias imports.
            if fact.is_some_and(|fact| fact.receiver == ReceiverKind::ModuleAlias) {
                return self.dispatch_direct(call, fact, state);
            }
            let base = self.eval_expr(&attribute.value, state);
            let method = attribute.attr.to_string();
            return match base {
                AbstractValue::Builder(builder) => {
                    self.builder_chain(builder, &method, call, state)
                }
                AbstractValue::SelfScene => self.dispatch_scene_method(&method, call, fact, state),
                AbstractValue::Object(id) => {
                    self.dispatch_object_method(&id, &method, call, fact, state)
                }
                _ => {
                    self.eval_args_and_widen(call, state);
                    AbstractValue::Unknown
                }
            };
        }
        self.dispatch_direct(call, fact, state)
    }

    /// Evaluates every argument for effects; returns the positional
    /// values (starred args evaluate but yield `Unknown`).
    pub(super) fn eval_call_args(
        &mut self,
        call: &'a ast::ExprCall,
        state: &mut ExecState,
    ) -> Vec<AbstractValue> {
        let mut positional = Vec::new();
        for arg in &call.args {
            if let ast::Expr::Starred(starred) = arg {
                self.eval_expr(&starred.value, state);
                positional.push(AbstractValue::Unknown);
            } else {
                positional.push(self.eval_expr(arg, state));
            }
        }
        for keyword in &call.keywords {
            self.eval_expr(&keyword.value, state);
        }
        positional
    }

    /// Evaluates the arguments and widens every tracked object (and the
    /// scene, when `self` is passed) for an unresolved call (DESIGN §5.3).
    pub(super) fn eval_args_and_widen(&mut self, call: &'a ast::ExprCall, state: &mut ExecState) {
        let mut objects = Vec::new();
        let mut includes_scene = false;
        for arg in &call.args {
            let expr: &ast::Expr = match arg {
                ast::Expr::Starred(starred) => &starred.value,
                other => other,
            };
            match self.eval_expr(expr, state) {
                AbstractValue::Object(id) => objects.push(id),
                AbstractValue::SelfScene => includes_scene = true,
                _ => {}
            }
        }
        for keyword in &call.keywords {
            match self.eval_expr(&keyword.value, state) {
                AbstractValue::Object(id) => objects.push(id),
                AbstractValue::SelfScene => includes_scene = true,
                _ => {}
            }
        }
        self.unknown_mutation(&objects, includes_scene, self.site(call.range()), state);
    }
}
