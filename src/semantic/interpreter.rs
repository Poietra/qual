//! The Manim lifecycle abstract interpreter (DESIGN §3, §5.1, §5.5-§5.7).
//!
//! For every discovered Scene subclass the interpreter composes
//! `__init__ → setup → construct → tear_down` along the Python MRO
//! (project-local bases, `super()` chains) and abstractly executes each
//! method body over its control-flow graph using the `semantic::heap` /
//! `semantic::state` data model:
//!
//! - allocation-site object identity (`site × bounded call context ×
//!   cardinality`; loop allocations widen to `Many` / `MaybeMany`),
//! - alias propagation through assignments, returns, and fluent chains
//!   (knowledge `returns_self` drives chain identity; `copy` /
//!   `generate_target` produce **new** ids with `copy_of` provenance),
//! - Scene membership per DESIGN §3.4 (re-add order effect, root-list
//!   restructuring on remove with the surviving parent link recorded),
//! - updater registration / removal / suspension with signature facts,
//! - the exact `Scene.play` event order of DESIGN §3.2 (compile args,
//!   apply kwargs, auto-add, duration, introducer setup-add, begin with
//!   starting copies and updater suspension, finish, cleanup with remover
//!   removes and `ReplacementTransform` replaces, final updater pass),
//! - `.animate` builders (`generate_target` runs at builder creation),
//! - branch joins to `MAYBE` and bounded loop evaluation with widening
//!   (DESIGN §5.5).
//!
//! Everything the interpreter cannot prove is an explicit `Unknown` /
//! `Maybe` fact — a rule must never receive a wrong certain fact
//! (DESIGN §15 invariant 2).
//!
//! Deliberate scope limits of this phase (all degrade to `Unknown`):
//! camera state, `always_update_mobjects` value tracking, effects of
//! nested `def` bodies (their registration identity and signature are
//! modeled; their body runs per-frame and belongs to the cost phase), and
//! `finally` effects on early-return paths (see `frontend::cfg`).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::frontend::cfg::{BasicBlock, CfgStmt, ControlFlowGraph, Terminator};
use crate::frontend::index::{
    CallableSignature, ClassRecord, LiteralFact, ParamKind, ProjectIndex, QualifiedCall,
    QualifiedCallFacts,
};
use crate::knowledge::{KnowledgeProfile, SceneMembershipEffect, SymbolEntry, SymbolKind};
use crate::semantic::events::{self, CleanupEffect, Event, InvocationContext, MutationKind};
use crate::semantic::heap::AbstractHeap;
use crate::semantic::state::{
    AnimationState, CallbackRef, GeneratedTarget, MobjectState, PlayGroupId, SceneState,
    SignatureSummary, SuspendBehavior, UpdaterFact, WriteChannel,
};
use crate::semantic::summaries::{
    MethodSummary, SummaryEffect, SummaryEvent, SummaryOperand, SummaryReturn, SummaryTable,
};
use crate::semantic::values::{
    AllocationSite, CallContextId, Cardinality, CopyKind, CopyOf, KindSet, Num, ObjectId, Presence,
    Truth, Visibility,
};
use crate::source::{FileId, SourceManager};

/// Loop passes before widening kicks in at loop headers (DESIGN §5.5:
/// evaluate 0 and 1 iterations, fixed point at most 3).
const WIDEN_AFTER_PASS: usize = 3;
/// Hard cap on fixpoint passes over one body.
const MAX_PASSES: usize = 6;

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
// Public fact types (the LifecycleFacts surface consumed by rules).
// ---------------------------------------------------------------------------

/// One lifecycle event with its source anchor and path certainty.
#[derive(Debug, Clone, PartialEq)]
pub struct TracedEvent {
    /// Source expression that produced the event.
    pub site: AllocationSite,
    /// [`Presence::Present`] when the event happens on every path through
    /// the enclosing method, [`Presence::Maybe`] on branch- or
    /// loop-dependent paths. Never base a certain diagnostic on a
    /// maybe-event.
    pub certainty: Presence,
    /// The event.
    pub event: Event,
}

/// Abstract state after one executed statement.
#[derive(Debug, Clone)]
pub struct StateSnapshot {
    /// The statement's source range.
    pub site: AllocationSite,
    /// The abstract heap after the statement (converged fixpoint state).
    pub heap: AbstractHeap,
}

/// Whether a play fact came from `Scene.play` or `Scene.wait` / `pause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayKind {
    /// A `Scene.play(...)` call.
    Play,
    /// A `Scene.wait(...)` / `Scene.pause(...)` call (a `Wait` animation).
    Wait,
}

/// One compiled animation argument of a play.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayedAnimation {
    /// Source range of the argument expression.
    pub site: AllocationSite,
    /// Identity of the animation object, when one was tracked.
    pub animation: Option<ObjectId>,
    /// The animation's abstract state at play time (kind, effects,
    /// run time, write channels, live targets).
    pub state: Option<AnimationState>,
    /// The replacement target (second constructor argument of the
    /// Transform family), when tracked.
    pub replacement_target: Option<ObjectId>,
    /// The argument was a `.animate` builder.
    pub from_builder: bool,
    /// Whether the argument converts to an animation: `Yes` for
    /// animations and builders, `No` for a bare tracked mobject (a
    /// runtime `TypeError`), `Maybe` for unknown values.
    pub convertible: Truth,
    /// Whether `state.write_channels` is a complete classification. When
    /// this is not `Yes`, absence of a channel is not evidence.
    pub channels_known: Truth,
}

/// One `Scene.play` / `Scene.wait` group.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayFact {
    /// Source range of the whole call.
    pub site: AllocationSite,
    /// Identifier shared with the emitted `BeginPlay` / `FinishPlay`
    /// events.
    pub play_group: PlayGroupId,
    /// Play vs wait.
    pub kind: PlayKind,
    /// `max(run_time)` over the compiled animations; `Unknown` when any
    /// run time is unknown.
    pub duration: Num,
    /// Compiled animation arguments in source order (empty for waits).
    pub animations: Vec<PlayedAnimation>,
    /// Wait only: whether the wait renders dynamically (scene updaters,
    /// `stop_condition`, or time-based family updaters; DESIGN §3.3).
    pub dynamic_wait: Truth,
    /// A `stop_condition` argument was written.
    pub has_stop_condition: bool,
    /// Literal `frozen_frame` argument, when written.
    pub frozen_frame: Option<bool>,
    /// Path certainty of the call itself.
    pub certainty: Presence,
}

/// Where an updater was registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterHost {
    /// `Mobject.add_updater` on this object.
    Mobject(ObjectId),
    /// `Scene.add_updater` (always called with `(dt)`, DESIGN §3.3).
    Scene,
}

/// One updater registration.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdaterRegistration {
    /// Source range of the registration call.
    pub site: AllocationSite,
    /// Registration target.
    pub host: UpdaterHost,
    /// Callback identity, signature facts, and time-based classification.
    pub fact: UpdaterFact,
    /// Path certainty.
    pub certainty: Presence,
}

/// One `remove_updater` call with its identity-match verdict (MLC125).
#[derive(Debug, Clone, PartialEq)]
pub struct UpdaterRemoval {
    /// Source range of the removal call.
    pub site: AllocationSite,
    /// Removal target (`None` for `Scene.remove_updater`).
    pub host: UpdaterHost,
    /// The callback identity passed at the call site.
    pub callback: CallbackRef,
    /// Whether the identity matches a currently registered updater:
    /// `Yes` (matched and removed), `No` (definitely no registered
    /// updater has this identity — e.g. a fresh lambda), `Maybe`.
    pub matched: Truth,
}

/// One `.animate` builder (MLC113 / MLC117 / MLR102).
#[derive(Debug, Clone, PartialEq)]
pub struct AnimateBuilderFact {
    /// Source range of the `.animate` attribute expression.
    pub site: AllocationSite,
    /// The live mobject the builder was created on.
    pub target: Option<ObjectId>,
    /// Methods chained on the builder, in order. Empty means a bare
    /// `mob.animate` (MLR102 candidate).
    pub methods: Vec<String>,
    /// Write channels of the chained methods.
    pub channels: BTreeSet<WriteChannel>,
    /// Whether `channels` is a complete classification.
    pub channels_known: Truth,
    /// The target's mutation epoch when the builder was created
    /// (`generate_target` runs here, DESIGN §3.2).
    pub target_epoch_at_creation: u64,
    /// The target's mutation epoch when the builder was played, if it was.
    pub target_epoch_at_play: Option<u64>,
    /// Whether the builder reached a `play` call.
    pub played: Truth,
    /// Another builder was created on the same live target before this
    /// one was played (stale-target hazard, MLC117).
    pub overwritten_by_later_builder: Truth,
}

/// What a target-state-requiring animation needs (MLC107 / MLC120).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetRequirement {
    /// `MoveToTarget` requires `generate_target()` on every path.
    GeneratedTarget,
    /// `Restore` / `Mobject.restore` requires `save_state()` on every
    /// path.
    SavedState,
}

/// Target-state fact at an animation construction site.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetRequirementFact {
    /// Source range of the animation construction.
    pub site: AllocationSite,
    /// What the animation requires.
    pub requirement: TargetRequirement,
    /// The live target, when tracked.
    pub target: Option<ObjectId>,
    /// Whether the required state exists at this point: `Absent` means
    /// absent on **all** paths (rule may fire), `Maybe` means present on
    /// some paths only (stay silent), `Present` means satisfied.
    pub presence: Presence,
}

/// One `Scene.remove` with restructuring facts (DESIGN §3.4, MLC115).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneRemovalFact {
    /// Source range of the `Scene.remove` call.
    pub site: AllocationSite,
    /// The removed object.
    pub removed: ObjectId,
    /// Structural parents that keep the object in their `submobjects`
    /// after the scene-root removal — re-adding any of these later makes
    /// the object reappear.
    pub surviving_parents: BTreeSet<ObjectId>,
    /// Path certainty of the removal.
    pub certainty: Presence,
}

/// Lifecycle analysis of one Scene subclass.
#[derive(Debug, Clone)]
pub struct SceneLifecycle {
    /// Qualified name of the Scene class.
    pub qualified_name: String,
    /// File the class is defined in.
    pub file: FileId,
    /// The abstract id of the scene instance.
    pub scene_id: ObjectId,
    /// Project-local MRO (this class first). External bases follow
    /// implicitly through the class record's reached bases.
    pub mro: Vec<String>,
    /// `true` when the MRO could not be linearized (unresolved or
    /// dynamic bases): constructor state is Unknown and rules must not
    /// emit certain diagnostics about it (DESIGN §5.1).
    pub constructor_state_unknown: bool,
    /// Event trace in program order (`__init__ → setup → construct →
    /// tear_down`).
    pub events: Vec<TracedEvent>,
    /// Statement-boundary state snapshots in the same program order.
    pub snapshots: Vec<StateSnapshot>,
    /// Play / wait groups in program order.
    pub plays: Vec<PlayFact>,
    /// Updater registrations in program order.
    pub updaters: Vec<UpdaterRegistration>,
    /// `remove_updater` calls in program order.
    pub updater_removals: Vec<UpdaterRemoval>,
    /// `.animate` builder facts keyed by builder site.
    pub builders: BTreeMap<AllocationSite, AnimateBuilderFact>,
    /// `MoveToTarget` / `Restore` requirement facts.
    pub target_requirements: Vec<TargetRequirementFact>,
    /// `Scene.remove` restructuring facts.
    pub scene_removals: Vec<SceneRemovalFact>,
    /// Per lifecycle method: whether `super().<method>()` was called
    /// (`Absent` = on no path, `Maybe` = on some paths). Only methods the
    /// project defines appear.
    pub super_calls: BTreeMap<String, Presence>,
    /// The abstract heap after `tear_down` (final membership, parents,
    /// updaters, generated targets).
    pub final_heap: AbstractHeap,
}

impl SceneLifecycle {
    /// The last statement snapshot in `file` ending at or before `byte`.
    #[must_use]
    pub fn state_at(&self, file: FileId, byte: u32) -> Option<&StateSnapshot> {
        self.snapshots
            .iter()
            .rfind(|snapshot| snapshot.site.file == file && snapshot.site.end <= byte)
    }

    /// Root and family membership of `object` at the last snapshot before
    /// `byte` in `file`. `None` when no snapshot or object state exists.
    #[must_use]
    pub fn membership_at(
        &self,
        object: &ObjectId,
        file: FileId,
        byte: u32,
    ) -> Option<(Presence, Presence)> {
        let snapshot = self.state_at(file, byte)?;
        let state = snapshot.heap.object(object)?;
        Some((state.scene_root_membership, state.family_membership))
    }

    /// Final root and family membership of `object` after `tear_down`.
    #[must_use]
    pub fn final_membership(&self, object: &ObjectId) -> Option<(Presence, Presence)> {
        let state = self.final_heap.object(object)?;
        Some((state.scene_root_membership, state.family_membership))
    }
}

/// Lifecycle facts for every discovered Scene subclass.
///
/// # Query map for the Phase-2 rules (Wave 3 consumers)
///
/// - `MLC107` / `MLC120`: [`SceneLifecycle::target_requirements`] with
///   [`TargetRequirementFact::presence`] `Absent` (all-paths-absent).
/// - `MLC108`: [`SceneLifecycle::plays`] → per-animation
///   [`PlayedAnimation::state`] write channels + targets; only fire when
///   [`PlayedAnimation::channels_known`] is `Yes` and target identity is
///   definite ([`ObjectId::definitely_same`]).
/// - `MLC110`: `AddChild` events in [`SceneLifecycle::events`] with
///   `parent == child`, or cycles in [`SceneLifecycle::final_heap`]
///   parent/children sets.
/// - `MLC113`: [`SceneLifecycle::builders`] method lists plus the
///   qualified-call facts for kwargs position.
/// - `MLC115`: [`SceneLifecycle::scene_removals`] (surviving parents) +
///   later `SceneAdd` events / [`SceneLifecycle::membership_at`].
/// - `MLC117`: [`AnimateBuilderFact::target_epoch_at_creation`] vs
///   [`AnimateBuilderFact::target_epoch_at_play`], and
///   [`AnimateBuilderFact::overwritten_by_later_builder`].
/// - `MLC125`: [`SceneLifecycle::updater_removals`] with
///   [`UpdaterRemoval::matched`] `No`.
/// - `MLR102`: [`SceneLifecycle::builders`] with empty `methods` that
///   were `played`.
/// - `MLR113`: [`SceneLifecycle::plays`] animations whose live target and
///   [`PlayedAnimation::replacement_target`] are definitely the same id.
/// - `MLR125`: [`SceneLifecycle::final_heap`] kind + children +
///   membership.
#[derive(Debug, Clone, Default)]
pub struct LifecycleFacts {
    /// Per-scene lifecycle analyses, sorted by qualified scene name.
    pub scenes: Vec<SceneLifecycle>,
}

impl LifecycleFacts {
    /// The lifecycle of the scene with this qualified class name.
    #[must_use]
    pub fn scene(&self, qualified_name: &str) -> Option<&SceneLifecycle> {
        self.scenes
            .iter()
            .find(|scene| scene.qualified_name == qualified_name)
    }
}

// ---------------------------------------------------------------------------
// Definition map: ASTs of project functions and methods.
// ---------------------------------------------------------------------------

/// One project function or method definition.
#[derive(Debug, Clone)]
pub struct FnDef<'a> {
    /// File the definition lives in.
    pub file: FileId,
    /// Byte range of the whole `def`.
    pub range: TextRange,
    /// Declared arguments.
    pub args: &'a ast::Arguments,
    /// Body statements.
    pub body: &'a [ast::Stmt],
    /// Dotted module name.
    pub module: String,
    /// Qualified class id for methods, `None` for module-level functions.
    pub class: Option<String>,
}

impl FnDef<'_> {
    /// Declared parameter names in order (`self` included for methods).
    #[must_use]
    pub fn param_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for arg in self.args.posonlyargs.iter().chain(&self.args.args) {
            names.push(arg.def.arg.to_string());
        }
        names
    }
}

/// Project function / method definitions keyed by qualified name
/// (`module.helper`, `module.Class.method`). Top-level definitions only;
/// nested `def`s are modeled as opaque callables.
#[derive(Debug, Default)]
pub struct DefMap<'a> {
    /// Definitions by qualified name.
    pub defs: BTreeMap<String, FnDef<'a>>,
}

impl<'a> DefMap<'a> {
    /// Collects every top-level function and directly defined method of
    /// all parsed files.
    #[must_use]
    pub fn build(sources: &'a SourceManager, index: &ProjectIndex) -> Self {
        let parsed = crate::frontend::parser::parsed_modules(sources);
        let mut map = Self {
            defs: BTreeMap::new(),
        };
        for module in &parsed {
            let module_name = index
                .module_of_file
                .get(&module.file)
                .map_or_else(String::new, |identity| identity.name.clone());
            for stmt in &module.ast.body {
                match stmt {
                    ast::Stmt::FunctionDef(def) => {
                        map.defs.insert(
                            format!("{module_name}.{}", def.name),
                            FnDef {
                                file: module.file,
                                range: def.range(),
                                args: def.args.as_ref(),
                                body: &def.body,
                                module: module_name.clone(),
                                class: None,
                            },
                        );
                    }
                    ast::Stmt::ClassDef(class_def) => {
                        let class_id = format!("{module_name}.{}", class_def.name);
                        for method in &class_def.body {
                            let (name, args, body, range) = match method {
                                ast::Stmt::FunctionDef(def) => {
                                    (def.name.as_str(), &def.args, &def.body, def.range())
                                }
                                ast::Stmt::AsyncFunctionDef(def) => {
                                    (def.name.as_str(), &def.args, &def.body, def.range())
                                }
                                _ => continue,
                            };
                            map.defs.insert(
                                format!("{class_id}.{name}"),
                                FnDef {
                                    file: module.file,
                                    range,
                                    args: args.as_ref(),
                                    body,
                                    module: module_name.clone(),
                                    class: Some(class_id.clone()),
                                },
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        map
    }
}

// ---------------------------------------------------------------------------
// Abstract values and execution state.
// ---------------------------------------------------------------------------

/// A `.animate` builder value.
#[derive(Debug, Clone, PartialEq)]
struct BuilderValue {
    site: AllocationSite,
    target: Option<ObjectId>,
    methods: Vec<String>,
    channels: BTreeSet<WriteChannel>,
    channels_known: Truth,
}

/// The abstract value of an expression.
#[derive(Debug, Clone, PartialEq)]
enum AbstractValue {
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
    /// Anything else.
    Unknown,
}

fn join_values(a: &AbstractValue, b: &AbstractValue) -> AbstractValue {
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

/// The full abstract state at one program point.
#[derive(Debug, Clone, PartialEq)]
struct ExecState {
    heap: AbstractHeap,
    /// Animation states keyed by the animation object's id.
    animations: BTreeMap<ObjectId, AnimationState>,
    /// Replacement targets (second Transform-family constructor argument)
    /// keyed by animation id; drives `Scene.replace` at play cleanup.
    replacement_targets: BTreeMap<ObjectId, ObjectId>,
    /// Local variable bindings of the current frame.
    env: BTreeMap<String, AbstractValue>,
    /// `self.<attr>` bindings of the scene instance (shared across the
    /// lifecycle methods).
    attrs: BTreeMap<String, AbstractValue>,
    /// Parent → children edges that hold on **every** path (joins
    /// intersect them). `MobjectState::children` keeps the possible
    /// edges.
    definite_children: BTreeMap<ObjectId, BTreeSet<ObjectId>>,
    /// Whether `super().<current method>()` was called on this path.
    super_called: Presence,
}

impl ExecState {
    fn new(heap: AbstractHeap) -> Self {
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
struct SinkOp {
    site: AllocationSite,
    certainty: Presence,
    in_loop: bool,
    in_definite_loop: bool,
    op: OpKind,
}

#[derive(Debug, Clone)]
enum OpKind {
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
    },
    UnknownMutation {
        values: Vec<ObjectId>,
        includes_scene: bool,
    },
    RendererDivergentMembership,
}

#[derive(Debug, Default)]
struct TraceSink {
    ops: Vec<SinkOp>,
    snapshots: Vec<StateSnapshot>,
    plays: Vec<PlayFact>,
    updaters: Vec<UpdaterRegistration>,
    updater_removals: Vec<UpdaterRemoval>,
    builders: BTreeMap<AllocationSite, AnimateBuilderFact>,
    target_requirements: Vec<TargetRequirementFact>,
    scene_removals: Vec<SceneRemovalFact>,
    returns: Vec<(AbstractValue, Presence)>,
}

/// Per-block execution context while walking a CFG.
#[derive(Debug, Clone, Copy, Default)]
struct BlockCtx {
    loop_depth: u32,
    cond_depth: u32,
    in_definite_loop: bool,
    /// Extra loop-ness from comprehension bodies.
    comprehension: bool,
}

impl BlockCtx {
    fn certainty(self) -> Presence {
        if self.cond_depth == 0 && !self.comprehension {
            Presence::Present
        } else {
            Presence::Maybe
        }
    }

    fn in_loop(self) -> bool {
        self.loop_depth > 0 || self.comprehension
    }

    fn cardinality(self) -> Cardinality {
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
// Shared analysis context.
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    index: &'a ProjectIndex,
    knowledge: Option<&'a KnowledgeProfile>,
    defs: &'a DefMap<'a>,
    summaries: &'a SummaryTable,
    call_facts: BTreeMap<(FileId, u32, u32), &'a QualifiedCall>,
}

impl<'a> Ctx<'a> {
    fn new(
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

    fn fact(&self, file: FileId, range: TextRange) -> Option<&'a QualifiedCall> {
        self.call_facts
            .get(&(file, range.start().into(), range.end().into()))
            .copied()
    }

    /// The curated symbol entry for calling `method` on `class_id`,
    /// walking the profile's base chain (e.g. `ThreeDScene.add` resolves
    /// to `Scene.add`).
    fn resolve_method(&self, class_id: &str, method: &str) -> Option<(String, &'a SymbolEntry)> {
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
    fn resolve_method_candidate(&self, candidate: &str) -> Option<(String, &'a SymbolEntry)> {
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

    fn reaches_base(&self, class_id: &str, base: &str) -> bool {
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
struct ResolvedAnimEffects {
    introducer: Truth,
    remover: Truth,
    replacement: Truth,
    suspend: SuspendBehavior,
    requires_target: bool,
    requires_saved_state: bool,
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

// ---------------------------------------------------------------------------
// Channel classification (curated-in-code; unknowns stay unknown).
// ---------------------------------------------------------------------------

/// Write channels of a curated fluent mutator, by canonical method id.
/// `None` means unclassified (callers must not treat it as "writes
/// nothing").
fn mutator_channels(canonical_method_id: &str) -> Option<BTreeSet<WriteChannel>> {
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

/// Write channels of a curated animation class, with a completeness
/// verdict.
fn animation_channels(
    ctx: &Ctx<'_>,
    class_id: &str,
    effects: ResolvedAnimEffects,
) -> (BTreeSet<WriteChannel>, Truth) {
    let mut channels = BTreeSet::new();
    let known = if class_id.starts_with("manim.animation.fading.") {
        channels.insert(WriteChannel::Opacity);
        Truth::Yes
    } else if class_id.starts_with("manim.animation.creation.") {
        channels.insert(WriteChannel::Points);
        Truth::Yes
    } else if class_id == "manim.animation.animation.Wait" {
        Truth::Yes
    } else if ctx.reaches_base(class_id, "manim.animation.transform.Transform") {
        channels.insert(WriteChannel::Points);
        channels.insert(WriteChannel::Style);
        Truth::Yes
    } else {
        Truth::Maybe
    };
    if effects.introducer == Truth::Yes
        || effects.remover == Truth::Yes
        || effects.replacement == Truth::Yes
    {
        channels.insert(WriteChannel::Membership);
    }
    (channels, known)
}

// ---------------------------------------------------------------------------
// Signature summaries (DESIGN §3.3).
// ---------------------------------------------------------------------------

fn signature_from_args(args: &ast::Arguments) -> CallableSignature {
    let mut params = Vec::new();
    for arg in &args.posonlyargs {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::PositionalOnly,
            has_default: arg.default.is_some(),
        });
    }
    for arg in &args.args {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::PositionalOrKeyword,
            has_default: arg.default.is_some(),
        });
    }
    if let Some(vararg) = &args.vararg {
        params.push(crate::frontend::index::ParamFact {
            name: vararg.arg.to_string(),
            kind: ParamKind::VarArgs,
            has_default: false,
        });
    }
    for arg in &args.kwonlyargs {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::KeywordOnly,
            has_default: arg.default.is_some(),
        });
    }
    if let Some(kwarg) = &args.kwarg {
        params.push(crate::frontend::index::ParamFact {
            name: kwarg.arg.to_string(),
            kind: ParamKind::KwArgs,
            has_default: false,
        });
    }
    CallableSignature { params }
}

/// Whether Manim's positional invocation with `provided` arguments binds.
fn binds_positionally(signature: &CallableSignature, provided: usize) -> Truth {
    let mut max_positional = 0usize;
    let mut required_positional = 0usize;
    let mut has_varargs = false;
    let mut required_keyword_only = false;
    for param in &signature.params {
        match param.kind {
            ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword => {
                max_positional += 1;
                if !param.has_default {
                    required_positional += 1;
                }
            }
            ParamKind::VarArgs => has_varargs = true,
            ParamKind::KeywordOnly => {
                if !param.has_default {
                    required_keyword_only = true;
                }
            }
            ParamKind::KwArgs => {}
        }
    }
    if required_keyword_only {
        return Truth::No;
    }
    let enough = provided >= required_positional;
    let not_too_many = has_varargs || provided <= max_positional;
    Truth::from(enough && not_too_many)
}

/// Builds the updater fact for a callback (DESIGN §3.3: the *name* `dt`
/// decides the time-based call, not the arity).
fn updater_fact(
    callback: CallbackRef,
    signature: Option<&CallableSignature>,
    scene_level: bool,
) -> UpdaterFact {
    let summary = signature.map_or_else(SignatureSummary::unknown, |signature| {
        let has_dt = signature.params.iter().any(|param| param.name == "dt");
        let positional = signature
            .params
            .iter()
            .filter(|param| {
                matches!(
                    param.kind,
                    ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword
                )
            })
            .count();
        let provided = if scene_level {
            1
        } else if has_dt {
            2
        } else {
            1
        };
        SignatureSummary {
            positional_params: u8::try_from(positional).ok(),
            has_dt_named_param: Truth::from(has_dt),
            has_var_positional: Truth::from(
                signature
                    .params
                    .iter()
                    .any(|param| param.kind == ParamKind::VarArgs),
            ),
            binds_positionally: binds_positionally(signature, provided),
        }
    });
    let time_based = if scene_level {
        // Scene updaters always receive `(dt)` and always make waits
        // dynamic (DESIGN §3.3).
        Truth::Yes
    } else {
        summary.has_dt_named_param
    };
    UpdaterFact {
        callback,
        signature: summary,
        time_based,
    }
}

// ---------------------------------------------------------------------------
// The machine.
// ---------------------------------------------------------------------------

struct Machine<'a, 'b> {
    ctx: &'b Ctx<'a>,
    sink: &'b mut TraceSink,
    file: FileId,
    module: String,
    scene_id: ObjectId,
    /// Project-local MRO of the scene being run (empty for summary runs
    /// of non-scene callables).
    mro: Vec<String>,
    /// External canonical bases reached by the scene class.
    reached_bases: BTreeSet<String>,
    current_class: Option<String>,
    current_method: String,
    call_context: CallContextId,
    play_counter: u64,
    /// Emit ops / facts (first pass only).
    emit: bool,
    /// Record statement snapshots (final pass of scene methods only).
    snapshot: bool,
    block: BlockCtx,
    /// While applying a summary event: its combined certainty overrides
    /// the block certainty for recorded ops and state effects.
    forced_certainty: Option<Presence>,
}

impl<'a> Machine<'a, '_> {
    fn site(&self, range: TextRange) -> AllocationSite {
        AllocationSite::new(self.file, range)
    }

    fn certainty(&self) -> Presence {
        self.forced_certainty
            .unwrap_or_else(|| self.block.certainty())
    }

    fn record(&mut self, site: AllocationSite, op: OpKind) {
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

    // -- body execution over the CFG ---------------------------------------

    fn run_body(&mut self, body: &'a [ast::Stmt], initial: &ExecState) -> ExecState {
        let cfg = ControlFlowGraph::build(body);
        let order = cfg.reverse_postorder();
        let preds = cfg.predecessors();
        let mut in_states: Vec<Option<ExecState>> = vec![None; cfg.blocks.len()];
        let mut out_states: Vec<Option<ExecState>> = vec![None; cfg.blocks.len()];
        let outer_emit = self.emit;
        let outer_snapshot = self.snapshot;

        for pass in 0..MAX_PASSES {
            self.emit = outer_emit && pass == 0;
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

        // Final pass over the converged states: snapshots + exit state.
        self.emit = false;
        self.snapshot = outer_snapshot;
        let mut exit: Option<ExecState> = None;
        for &block_id in &order {
            let block = &cfg.blocks[block_id.0];
            let Some(input) = in_states[block_id.0].clone() else {
                continue;
            };
            let out = self.exec_block(block, input);
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
        self.block = BlockCtx {
            loop_depth: block.loop_depth,
            cond_depth: block.cond_depth,
            in_definite_loop: block.in_definite_loop,
            comprehension: false,
        };
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
                let returned = value.map_or(AbstractValue::Unknown, |expr| {
                    self.eval_expr(expr, &mut state)
                });
                if self.emit {
                    self.sink.returns.push((returned, self.certainty()));
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
                        heap: state.heap.clone(),
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
                        state.attrs.insert(attribute.attr.to_string(), value);
                        self.record(
                            self.site(attribute.range()),
                            OpKind::SetSelfAttr {
                                name: attribute.attr.to_string(),
                                value: recorded,
                            },
                        );
                        return;
                    }
                }
                self.eval_expr(&attribute.value, state);
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
    fn eval_expr(&mut self, expr: &'a ast::Expr, state: &mut ExecState) -> AbstractValue {
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
            ast::Expr::Constant(_) => AbstractValue::Unknown,
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

    // -- .animate builders --------------------------------------------------

    fn make_builder(
        &mut self,
        target: &ObjectId,
        range: TextRange,
        state: &mut ExecState,
    ) -> AbstractValue {
        let site = self.site(range);
        // Builder creation runs `generate_target()` immediately
        // (DESIGN §3.2).
        self.generate_target(target, site, self.certainty(), state);
        let epoch = state
            .heap
            .object(target)
            .map_or(0, |object| object.mutation_epoch);
        if self.emit {
            // A previous un-played builder on the same target is now
            // overwritten (its `mobject.target` was replaced).
            let overwrite = if self.certainty() == Presence::Present {
                Truth::Yes
            } else {
                Truth::Maybe
            };
            for fact in self.sink.builders.values_mut() {
                if fact.site != site
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
                .builders
                .entry(site)
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
        }
        AbstractValue::Builder(BuilderValue {
            site,
            target: Some(target.clone()),
            methods: Vec::new(),
            channels: BTreeSet::new(),
            channels_known: Truth::Yes,
        })
    }

    fn builder_chain(
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
        match channels {
            Some(channels) => builder.channels.extend(channels),
            None => builder.channels_known = Truth::Maybe,
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
            if let Some(fact) = self.sink.builders.get_mut(&builder.site) {
                fact.methods.clone_from(&builder.methods);
                fact.channels.clone_from(&builder.channels);
                fact.channels_known = builder.channels_known;
            }
        }
        AbstractValue::Builder(builder)
    }

    // -- primitive heap / scene operations ---------------------------------

    fn alloc_object(
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

    fn scene_state_mut<'s>(&self, state: &'s mut ExecState) -> Option<&'s mut SceneState> {
        state.heap.scenes.get_mut(&self.scene_id)
    }

    fn recompute_family(&self, state: &mut ExecState) {
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

    fn scene_add(
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
    fn scene_remove(
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

    fn scene_replace(
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

    fn add_child(
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

    fn remove_child(
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

    fn generate_target(
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

    fn save_state(
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

    fn mutate(
        &mut self,
        target: &ObjectId,
        kind: MutationKind,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
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

    fn register_updater(
        &mut self,
        host: Option<&ObjectId>,
        fact: UpdaterFact,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        match host {
            Some(target) => {
                if let Some(object) = state.heap.object_mut(target) {
                    object.updaters.insert(fact.clone());
                }
            }
            None => {
                if let Some(scene) = self.scene_state_mut(state) {
                    scene.scene_updaters.insert(fact.clone());
                }
            }
        }
        if self.emit {
            self.sink.updaters.push(UpdaterRegistration {
                site,
                host: host.map_or(UpdaterHost::Scene, |id| UpdaterHost::Mobject(id.clone())),
                fact: fact.clone(),
                certainty: self.certainty(),
            });
        }
        self.record(
            site,
            OpKind::RegisterUpdater {
                target: host.cloned(),
                scene_level: host.is_none(),
                updater: fact,
            },
        );
    }

    fn remove_updater(
        &mut self,
        host: Option<&ObjectId>,
        callback: &CallbackRef,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        let matched = if let Some(target) = host {
            let registered = state
                .heap
                .object(target)
                .map(|object| object.updaters.clone())
                .unwrap_or_default();
            Self::match_and_remove(&registered, callback, |updaters| {
                if let Some(object) = state.heap.object_mut(target) {
                    object.updaters = updaters;
                }
            })
        } else {
            {
                let registered = state
                    .heap
                    .scenes
                    .get(&self.scene_id)
                    .map(|scene| scene.scene_updaters.clone())
                    .unwrap_or_default();
                Self::match_and_remove(&registered, callback, |updaters| {
                    if let Some(scene) = state.heap.scenes.get_mut(&self.scene_id) {
                        scene.scene_updaters = updaters;
                    }
                })
            }
        };
        if self.emit {
            self.sink.updater_removals.push(UpdaterRemoval {
                site,
                host: host.map_or(UpdaterHost::Scene, |id| UpdaterHost::Mobject(id.clone())),
                callback: callback.clone(),
                matched,
            });
        }
        self.record(
            site,
            OpKind::RemoveUpdater {
                target: host.cloned(),
                scene_level: host.is_none(),
                callback: callback.clone(),
            },
        );
    }

    fn match_and_remove(
        registered: &BTreeSet<UpdaterFact>,
        callback: &CallbackRef,
        write_back: impl FnOnce(BTreeSet<UpdaterFact>),
    ) -> Truth {
        if matches!(callback, CallbackRef::Unknown) {
            // Unknown identity: it may match anything; leave the set.
            return Truth::Maybe;
        }
        let matches: Vec<&UpdaterFact> = registered
            .iter()
            .filter(|fact| &fact.callback == callback)
            .collect();
        if matches.is_empty() {
            // A distinct identity (e.g. a fresh lambda) definitely does
            // not match any registered updater.
            return Truth::No;
        }
        let remaining: BTreeSet<UpdaterFact> = registered
            .iter()
            .filter(|fact| &fact.callback != callback)
            .cloned()
            .collect();
        write_back(remaining);
        Truth::Yes
    }

    fn unknown_mutation(
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
                object.family_size = Num::Unknown;
                object.point_count = Num::Unknown;
                object.curve_count = Num::Unknown;
                object.subpath_count = Num::Unknown;
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
}

/// Builtins that never mutate Manim state; calling them does not widen
/// their arguments.
const PURE_BUILTINS: &[&str] = &[
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

/// `super().<method>(...)` detection.
fn super_call_method(call: &ast::ExprCall) -> Option<&str> {
    let ast::Expr::Attribute(attribute) = call.func.as_ref() else {
        return None;
    };
    let ast::Expr::Call(inner) = attribute.value.as_ref() else {
        return None;
    };
    let ast::Expr::Name(name) = inner.func.as_ref() else {
        return None;
    };
    (name.id.as_str() == "super" && inner.args.is_empty()).then(|| attribute.attr.as_str())
}

fn literal_num(argument: &crate::frontend::index::CallArgument) -> Option<Num> {
    match argument.literal.as_ref()? {
        LiteralFact::Int(value) => Some(Num::int(*value)),
        LiteralFact::Float(value) => Some(Num::float(*value)),
        _ => None,
    }
}

fn literal_bool(argument: &crate::frontend::index::CallArgument) -> Option<bool> {
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
    fn eval_call_args(
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
    fn eval_args_and_widen(&mut self, call: &'a ast::ExprCall, state: &mut ExecState) {
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

    fn dispatch_direct(
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
            let id = self.alloc_object(self.site(call.range()), kind, state);
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
                let id =
                    self.alloc_object(self.site(call.range()), KindSet::single(candidate), state);
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

    // -- animation construction --------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the play compile stage is inherently long"
    )]
    fn create_animation(
        &mut self,
        kind: &KindSet,
        class_label: Option<&str>,
        effects: ResolvedAnimEffects,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let positional = self.eval_call_args(call, state);
        let site = self.site(call.range());

        // Composition groups: targets and effects come from the children.
        let child_states: Vec<AnimationState> = positional
            .iter()
            .filter_map(|value| match value {
                AbstractValue::Animation(id) => state.animations.get(id).cloned(),
                _ => None,
            })
            .collect();
        let is_composition = !child_states.is_empty()
            && positional
                .iter()
                .all(|value| matches!(value, AbstractValue::Animation(_) | AbstractValue::Unknown));

        let mut animation = AnimationState::unknown(kind.clone());
        animation.introducer = effects.introducer;
        animation.remover = effects.remover;
        animation.replacement = effects.replacement;
        animation.suspend = effects.suspend;

        let mut replacement_target = None;
        if is_composition {
            let mut targets = BTreeSet::new();
            let mut introducer: Option<Truth> = None;
            let mut remover: Option<Truth> = None;
            let mut replacement: Option<Truth> = None;
            for child in &child_states {
                targets.extend(child.targets.iter().cloned());
                introducer =
                    Some(introducer.map_or(child.introducer, |t| t.join(child.introducer)));
                remover = Some(remover.map_or(child.remover, |t| t.join(child.remover)));
                replacement =
                    Some(replacement.map_or(child.replacement, |t| t.join(child.replacement)));
            }
            animation.targets = targets;
            animation.introducer = introducer.unwrap_or(Truth::Maybe);
            animation.remover = remover.unwrap_or(Truth::Maybe);
            animation.replacement = replacement.unwrap_or(Truth::Maybe);
            animation.write_channels = child_states
                .iter()
                .flat_map(|child| child.write_channels.iter().copied())
                .collect();
        } else {
            if let Some(AbstractValue::Object(target)) = positional.first() {
                animation.targets.insert(target.clone());
            }
            if let Some(AbstractValue::Object(second)) = positional.get(1) {
                replacement_target = Some(second.clone());
            }
            if let Some(label) = class_label {
                let (channels, _known) = animation_channels(self.ctx, label, effects);
                animation.write_channels = channels;
            }
        }

        if let Some(argument) = fact.and_then(|fact| fact.keyword("run_time")) {
            if let Some(run_time) = literal_num(argument) {
                animation.run_time = run_time;
            }
        }
        if let Some(argument) = fact.and_then(|fact| fact.keyword("suspend_mobject_updating")) {
            if let Some(flag) = literal_bool(argument) {
                animation.suspend = if flag {
                    SuspendBehavior::SuspendsLiveTargets
                } else {
                    SuspendBehavior::LeavesUpdatersRunning
                };
            }
        }

        // MoveToTarget / Restore requirements against the current state
        // (all-paths-absent vs maybe-present, MLC107 / MLC120).
        if self.emit && (effects.requires_target || effects.requires_saved_state) {
            let target = animation.targets.iter().next().cloned();
            let requirement = if effects.requires_target {
                TargetRequirement::GeneratedTarget
            } else {
                TargetRequirement::SavedState
            };
            let presence = target.as_ref().map_or(Presence::Maybe, |target| {
                state.heap.object(target).map_or(Presence::Maybe, |object| {
                    if effects.requires_target {
                        object.generated_target.presence
                    } else {
                        object.saved_state
                    }
                })
            });
            self.sink.target_requirements.push(TargetRequirementFact {
                site,
                requirement,
                target,
                presence,
            });
        }

        let id = ObjectId::new(site, self.call_context.clone(), self.block.cardinality());
        state.animations.insert(id.clone(), animation.clone());
        if let Some(target) = &replacement_target {
            state.replacement_targets.insert(id.clone(), target.clone());
        }
        let targets: Vec<ObjectId> = animation.targets.iter().cloned().collect();
        self.record(
            site,
            OpKind::CreateAnimation {
                animation: id.clone(),
                state: animation,
                targets,
                replacement_target: replacement_target.clone(),
                requires_target: effects.requires_target,
                requires_saved_state: effects.requires_saved_state,
            },
        );
        AbstractValue::Animation(id)
    }

    // -- scene method dispatch ---------------------------------------------

    fn dispatch_scene_method(
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
                return self.apply_summary_call(
                    &qualified,
                    Some(AbstractValue::SelfScene),
                    call,
                    fact,
                    state,
                );
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
    fn apply_scene_effect(
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
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                // 3D fixed helpers auto-add to the scene (DESIGN §3.5).
                self.scene_add(&objects, site, false, false, certainty, state);
                for id in &objects {
                    if let Some(object) = state.heap.object_mut(id) {
                        if fixed_in_frame {
                            object.fixed_in_frame = Truth::Yes;
                        } else {
                            object.fixed_orientation = Truth::Yes;
                        }
                    }
                }
                self_result()
            }
            Some(
                SceneMembershipEffect::RemoveFixedInFrame
                | SceneMembershipEffect::RemoveFixedOrientation,
            ) => {
                let fixed_in_frame = membership == Some(SceneMembershipEffect::RemoveFixedInFrame);
                let positional = self.eval_call_args(call, state);
                let objects = self.object_args(&positional, state);
                for id in &objects {
                    if let Some(object) = state.heap.object_mut(id) {
                        if fixed_in_frame {
                            object.fixed_in_frame = Truth::No;
                        } else {
                            object.fixed_orientation = Truth::No;
                        }
                        // Membership after unfixing diverges between the
                        // renderers (DESIGN §3.5): never a certain fact.
                        object.scene_root_membership =
                            object.scene_root_membership.join(Presence::Maybe);
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

    fn dispatch_object_method(
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
                    // A copy carries the original's updaters but no scene
                    // membership.
                    fresh.updaters.clone_from(&object.updaters);
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

    // -- super() dispatch ---------------------------------------------------

    fn dispatch_super(
        &mut self,
        method: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if method == self.current_method {
            state.super_called = Presence::Present;
        }
        let Some(current) = self.current_class.clone() else {
            self.eval_call_args(call, state);
            return AbstractValue::Unknown;
        };
        let start = self
            .mro
            .iter()
            .position(|class| class == &current)
            .map_or(0, |position| position + 1);
        let mro = self.mro.clone();
        for class in &mro[start.min(mro.len())..] {
            let qualified = format!("{class}.{method}");
            if self.ctx.defs.defs.contains_key(&qualified) {
                return self.apply_summary_call(
                    &qualified,
                    Some(AbstractValue::SelfScene),
                    call,
                    fact,
                    state,
                );
            }
        }
        // External base: curated effect if any, else the empty base
        // implementation (`Scene.__init__` / `setup` / `tear_down`).
        let resolved = self
            .reached_bases
            .iter()
            .find_map(|base| self.ctx.resolve_method(base, method));
        if let Some((canonical, entry)) = resolved {
            return self.apply_scene_effect(&canonical, entry, call, fact, state);
        }
        self.eval_call_args(call, state);
        AbstractValue::Unknown
    }

    // -- play / wait ---------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the DESIGN §3.2 event order is one sequence"
    )]
    fn do_play(
        &mut self,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let group = self.play_counter;
        self.play_counter += 1;
        let call_site = self.site(call.range());
        let certainty = self.certainty();

        // 1. compile arguments.
        let mut compiled: Vec<PlayedAnimation> = Vec::new();
        for arg in &call.args {
            if let ast::Expr::Starred(starred) = arg {
                self.eval_expr(&starred.value, state);
                continue;
            }
            let value = self.eval_expr(arg, state);
            let arg_site = self.site(arg.range());
            match value {
                AbstractValue::Animation(id) => {
                    let animation_state = state.animations.get(&id).cloned();
                    let channels_known =
                        animation_state
                            .as_ref()
                            .map_or(Truth::Maybe, |st| match &st.kind {
                                KindSet::Known(kinds) if kinds.len() == 1 => {
                                    let kind = kinds.iter().next().expect("len checked");
                                    let effects = ResolvedAnimEffects {
                                        introducer: st.introducer,
                                        remover: st.remover,
                                        replacement: st.replacement,
                                        suspend: st.suspend,
                                        requires_target: false,
                                        requires_saved_state: false,
                                    };
                                    animation_channels(self.ctx, kind, effects).1
                                }
                                _ => Truth::Maybe,
                            });
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: Some(id.clone()),
                        replacement_target: state.replacement_targets.get(&id).cloned(),
                        state: animation_state,
                        from_builder: false,
                        convertible: Truth::Yes,
                        channels_known,
                    });
                }
                AbstractValue::Builder(builder) => {
                    let mut animation = AnimationState::unknown(KindSet::single(
                        "manim.animation.transform._MethodAnimation",
                    ));
                    animation.introducer = Truth::No;
                    animation.remover = Truth::No;
                    animation.replacement = Truth::No;
                    animation.suspend = SuspendBehavior::SuspendsLiveTargets;
                    animation.write_channels.clone_from(&builder.channels);
                    if let Some(target) = &builder.target {
                        animation.targets.insert(target.clone());
                    }
                    let id = ObjectId::new(
                        arg_site,
                        self.call_context.clone(),
                        self.block.cardinality(),
                    );
                    state.animations.insert(id.clone(), animation.clone());
                    if self.emit {
                        if let Some(fact) = self.sink.builders.get_mut(&builder.site) {
                            fact.played = if certainty == Presence::Present {
                                Truth::Yes
                            } else {
                                fact.played.join(Truth::Yes)
                            };
                            fact.target_epoch_at_play = builder
                                .target
                                .as_ref()
                                .and_then(|target| state.heap.object(target))
                                .map(|object| object.mutation_epoch);
                        }
                    }
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: Some(id),
                        state: Some(animation),
                        replacement_target: None,
                        from_builder: true,
                        convertible: Truth::Yes,
                        channels_known: builder.channels_known,
                    });
                }
                AbstractValue::Object(id) => {
                    // A bare mobject cannot be converted to an animation
                    // (runtime TypeError; MLC102 evidence).
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::No,
                        channels_known: Truth::No,
                    });
                    let _ = id;
                }
                _ => {
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::Maybe,
                        channels_known: Truth::Maybe,
                    });
                }
            }
        }
        for keyword in &call.keywords {
            self.eval_expr(&keyword.value, state);
        }

        // 2. apply play kwargs to every animation (setattr semantics).
        if let Some(run_time) = fact
            .and_then(|fact| fact.keyword("run_time"))
            .and_then(literal_num)
        {
            for played in &mut compiled {
                if let Some(animation_state) = &mut played.state {
                    animation_state.run_time = run_time.clone();
                }
                if let Some(id) = &played.animation {
                    if let Some(animation_state) = state.animations.get_mut(id) {
                        animation_state.run_time = run_time.clone();
                    }
                }
            }
        }

        // 3. duration = max(run_time).
        let mut duration: Option<Num> = None;
        for played in &compiled {
            let run_time = played
                .state
                .as_ref()
                .map_or(Num::Unknown, |st| st.run_time.clone());
            duration = Some(match duration {
                None => run_time,
                Some(current) => current.max_with(&run_time),
            });
        }
        let duration = duration.unwrap_or(Num::Unknown);

        // 4. auto-add non-introducer targets that are not in the family.
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            match animation_state.introducer {
                Truth::No => {
                    for target in &animation_state.targets {
                        let membership = state
                            .heap
                            .object(target)
                            .map_or(Presence::Absent, |object| object.family_membership);
                        if membership == Presence::Present {
                            continue;
                        }
                        let add_certainty =
                            if membership == Presence::Absent && certainty == Presence::Present {
                                Presence::Present
                            } else {
                                Presence::Maybe
                            };
                        self.scene_add(
                            std::slice::from_ref(target),
                            played.site,
                            false,
                            false,
                            add_certainty,
                            state,
                        );
                    }
                }
                Truth::Yes => {}
                Truth::Maybe => {
                    // Unknown introducer status: the target is added
                    // either way during the play, but never certainly
                    // *here*.
                    for target in &animation_state.targets {
                        self.scene_add(
                            std::slice::from_ref(target),
                            played.site,
                            false,
                            false,
                            Presence::Maybe,
                            state,
                        );
                    }
                }
            }
        }

        let animation_ids: Vec<ObjectId> = compiled
            .iter()
            .filter_map(|played| played.animation.clone())
            .collect();
        self.record(
            call_site,
            OpKind::BeginPlay {
                play_group: group,
                animations: animation_ids,
                duration: duration.clone(),
            },
        );

        // 5. introducer setup-add (`_setup_scene`).
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            if animation_state.introducer == Truth::Yes {
                for target in &animation_state.targets {
                    let membership = state
                        .heap
                        .object(target)
                        .map_or(Presence::Absent, |object| object.family_membership);
                    if membership == Presence::Present {
                        continue;
                    }
                    let add_certainty =
                        if membership == Presence::Absent && certainty == Presence::Present {
                            Presence::Present
                        } else {
                            Presence::Maybe
                        };
                    self.scene_add(
                        std::slice::from_ref(target),
                        played.site,
                        false,
                        false,
                        add_certainty,
                        state,
                    );
                }
            }
        }

        // 6. begin(): starting copies + updater suspension.
        let mut suspended: Vec<ObjectId> = Vec::new();
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            for target in &animation_state.targets {
                // The starting copy is a fresh identity (DESIGN §15
                // invariant 6).
                let copy = ObjectId::new(
                    played.site,
                    self.call_context.push(call_site),
                    self.block.cardinality(),
                );
                let copy_state = state.heap.object(target).map_or_else(
                    || MobjectState::fresh(KindSet::Unknown),
                    |object| {
                        let mut fresh = MobjectState::fresh(object.kind.clone());
                        fresh.copy_provenance = Some(CopyKind::AnimationStartingCopy);
                        fresh
                    },
                );
                state.heap.insert_object(copy.clone(), copy_state);
                state.heap.record_copy(
                    copy,
                    CopyOf {
                        original: target.clone(),
                        kind: CopyKind::AnimationStartingCopy,
                    },
                );
                match animation_state.suspend {
                    SuspendBehavior::SuspendsLiveTargets => {
                        if let Some(object) = state.heap.object_mut(target) {
                            object.updating_suspended = if certainty == Presence::Present {
                                Truth::Yes
                            } else {
                                object.updating_suspended.join(Truth::Yes)
                            };
                        }
                        suspended.push(target.clone());
                        self.record(
                            played.site,
                            OpKind::SuspendUpdater {
                                target: target.clone(),
                            },
                        );
                    }
                    SuspendBehavior::Unknown => {
                        if let Some(object) = state.heap.object_mut(target) {
                            object.updating_suspended = object.updating_suspended.join(Truth::Yes);
                        }
                    }
                    SuspendBehavior::LeavesUpdatersRunning => {}
                }
            }
        }

        // 7. finish(): suspended updaters resume.
        for target in &suspended {
            if let Some(object) = state.heap.object_mut(target) {
                object.updating_suspended = if certainty == Presence::Present {
                    Truth::No
                } else {
                    object.updating_suspended.join(Truth::No)
                };
            }
            self.record(
                call_site,
                OpKind::ResumeUpdater {
                    target: target.clone(),
                },
            );
        }

        // 8. clean_up_from_scene(): removers remove, replacements replace.
        let mut cleanup: Vec<CleanupEffect> = Vec::new();
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            let targets: Vec<ObjectId> = animation_state.targets.iter().cloned().collect();
            match animation_state.remover {
                Truth::Yes => {
                    self.scene_remove(&targets, played.site, certainty, state);
                    cleanup.push(CleanupEffect::SceneRemove(targets.clone()));
                }
                Truth::Maybe => {
                    self.scene_remove(&targets, played.site, Presence::Maybe, state);
                }
                Truth::No => {}
            }
            match animation_state.replacement {
                Truth::Yes => {
                    if let (Some(old), Some(new)) =
                        (targets.first(), played.replacement_target.as_ref())
                    {
                        self.scene_replace(old, new, certainty, state);
                        cleanup.push(CleanupEffect::SceneReplace {
                            old: old.clone(),
                            new: new.clone(),
                        });
                    }
                }
                Truth::Maybe => {
                    if let (Some(old), Some(new)) =
                        (targets.first(), played.replacement_target.as_ref())
                    {
                        self.scene_replace(old, new, Presence::Maybe, state);
                    }
                }
                Truth::No => {}
            }
            // Interpolation wrote the animation's channels on its live
            // targets.
            if !animation_state.write_channels.is_empty() || played.from_builder {
                for target in &targets {
                    if let Some(object) = state.heap.object_mut(target) {
                        object.mutation_epoch += 1;
                    }
                }
            }
        }
        if !suspended.is_empty() {
            cleanup.push(CleanupEffect::ResumeUpdaters(suspended));
        }
        cleanup.push(CleanupEffect::FinalUpdaterPass);
        self.record(
            call_site,
            OpKind::FinishPlay {
                play_group: group,
                cleanup,
            },
        );

        if let Some(scene) = self.scene_state_mut(state) {
            scene.elapsed_time = scene.elapsed_time.add(&duration);
        }
        if self.emit {
            self.sink.plays.push(PlayFact {
                site: call_site,
                play_group: PlayGroupId(group),
                kind: PlayKind::Play,
                duration,
                animations: compiled,
                dynamic_wait: Truth::No,
                has_stop_condition: false,
                frozen_frame: None,
                certainty,
            });
        }
        AbstractValue::Unknown
    }

    fn do_wait(
        &mut self,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let group = self.play_counter;
        self.play_counter += 1;
        self.eval_call_args(call, state);
        let call_site = self.site(call.range());
        let duration = fact
            .and_then(|fact| {
                fact.positional(0)
                    .or_else(|| fact.keyword("duration"))
                    .and_then(literal_num)
            })
            .unwrap_or(Num::Unknown);
        let has_stop_condition = fact.is_some_and(|fact| fact.keyword("stop_condition").is_some());
        let frozen_frame = fact
            .and_then(|fact| fact.keyword("frozen_frame"))
            .and_then(literal_bool);

        // Wait freeze verdict (DESIGN §3.3 / `should_update_mobjects`).
        let scene_has_updaters = state
            .heap
            .scenes
            .get(&self.scene_id)
            .is_some_and(|scene| !scene.scene_updaters.is_empty());
        let mut dynamic = if has_stop_condition || scene_has_updaters {
            Truth::Yes
        } else {
            Truth::No
        };
        if dynamic == Truth::No {
            // Any time-based updater in the scene family makes the wait
            // dynamic; a one-argument updater alone does not.
            for object in state.heap.objects.values() {
                if !object.family_membership.may_be_present() {
                    continue;
                }
                for updater in &object.updaters {
                    match (updater.time_based, object.family_membership) {
                        (Truth::Yes, Presence::Present) => {
                            dynamic = Truth::Yes;
                        }
                        (Truth::Yes | Truth::Maybe, _) => {
                            if dynamic == Truth::No {
                                dynamic = Truth::Maybe;
                            }
                        }
                        (Truth::No, _) => {}
                    }
                }
                if dynamic == Truth::Yes {
                    break;
                }
            }
            // `always_update_mobjects` assigned somewhere: unknown value.
            if state.attrs.contains_key("always_update_mobjects") && dynamic == Truth::No {
                dynamic = Truth::Maybe;
            }
        }

        if let Some(scene) = self.scene_state_mut(state) {
            scene.elapsed_time = scene.elapsed_time.add(&duration);
        }
        if self.emit {
            self.sink.plays.push(PlayFact {
                site: call_site,
                play_group: PlayGroupId(group),
                kind: PlayKind::Wait,
                duration,
                animations: Vec::new(),
                dynamic_wait: dynamic,
                has_stop_condition,
                frozen_frame,
                certainty: self.certainty(),
            });
        }
        AbstractValue::Unknown
    }

    // -- summary application -------------------------------------------------

    fn apply_summary_call(
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
                state
                    .heap
                    .insert_object(id.clone(), MobjectState::fresh(kind.clone()));
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
// Op → public event conversion.
// ---------------------------------------------------------------------------

fn op_to_event(op: &SinkOp, scene_id: &ObjectId) -> Option<Event> {
    match &op.op {
        OpKind::Alloc { object, kind } => Some(Event::Alloc(events::Alloc {
            object: object.clone(),
            kind: kind.clone(),
        })),
        OpKind::SceneAdd {
            objects,
            order_effect,
            ..
        } => Some(Event::SceneAdd(events::SceneAdd {
            scene: scene_id.clone(),
            objects: objects.clone(),
            order_effect: *order_effect,
        })),
        OpKind::SceneRemove { objects } => Some(Event::SceneRemove(events::SceneRemove {
            scene: scene_id.clone(),
            objects: objects.clone(),
        })),
        OpKind::AddChild { parent, child } => Some(Event::AddChild(events::AddChild {
            parent: parent.clone(),
            child: child.clone(),
        })),
        OpKind::RemoveChild { parent, .. } => Some(Event::Mutate(events::Mutate {
            target: parent.clone(),
            kind: MutationKind::Membership,
        })),
        OpKind::RegisterUpdater {
            target, updater, ..
        } => Some(Event::RegisterUpdater(events::RegisterUpdater {
            target: target.clone().unwrap_or_else(|| scene_id.clone()),
            updater: updater.clone(),
        })),
        OpKind::RemoveUpdater { target, .. } => Some(Event::Mutate(events::Mutate {
            target: target.clone().unwrap_or_else(|| scene_id.clone()),
            kind: MutationKind::Updaters,
        })),
        OpKind::ClearUpdaters { target } => Some(Event::Mutate(events::Mutate {
            target: target.clone(),
            kind: MutationKind::Updaters,
        })),
        OpKind::Mutate { target, kind } => Some(Event::Mutate(events::Mutate {
            target: target.clone(),
            kind: *kind,
        })),
        OpKind::GenerateTarget { copy, .. } => Some(Event::Alloc(events::Alloc {
            object: copy.clone(),
            kind: KindSet::Unknown,
        })),
        OpKind::SaveState { .. } | OpKind::SetSelfAttr { .. } => None,
        OpKind::CreateAnimation {
            animation, state, ..
        } => Some(Event::CreateAnimation(events::CreateAnimation {
            animation: animation.clone(),
            state: state.clone(),
        })),
        OpKind::BeginPlay {
            play_group,
            animations,
            duration,
        } => Some(Event::BeginPlay(events::BeginPlay {
            play_group: *play_group,
            animations: animations.clone(),
            duration: duration.clone(),
        })),
        OpKind::SuspendUpdater { target } => Some(Event::SuspendUpdater(events::SuspendUpdater {
            target: target.clone(),
        })),
        OpKind::ResumeUpdater { target } => Some(Event::ResumeUpdater(events::ResumeUpdater {
            target: target.clone(),
        })),
        OpKind::FinishPlay {
            play_group,
            cleanup,
        } => Some(Event::FinishPlay(events::FinishPlay {
            play_group: *play_group,
            cleanup: cleanup.clone(),
        })),
        OpKind::UnknownMutation { values, .. } => {
            Some(Event::UnknownMutation(events::UnknownMutation {
                values: values.clone(),
            }))
        }
        OpKind::RendererDivergentMembership => {
            Some(Event::RendererRequirement(events::RendererRequirement {
                kind: events::RendererRequirementKind::DivergesBetweenRenderers,
                note: "fixed-object removal membership diverges between renderers".to_owned(),
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// MRO linearization (C3 over project classes).
// ---------------------------------------------------------------------------

fn linearize_project(index: &ProjectIndex, class_id: &str) -> (Vec<String>, bool) {
    fn linearize(
        index: &ProjectIndex,
        id: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        let Some(record) = index.classes.get(id) else {
            // External terminal.
            return Some(vec![id.to_owned()]);
        };
        if !visiting.insert(id.to_owned()) {
            return None;
        }
        let mut parents = Vec::new();
        for base in &record.bases {
            match base {
                crate::frontend::index::BaseRef::Resolved(base_id) => {
                    parents.push(base_id.clone());
                }
                crate::frontend::index::BaseRef::Unresolved(_) => {
                    visiting.remove(id);
                    return None;
                }
            }
        }
        let mut sequences: Vec<Vec<String>> = Vec::new();
        for parent in &parents {
            sequences.push(linearize(index, parent, visiting)?);
        }
        if !parents.is_empty() {
            sequences.push(parents);
        }
        visiting.remove(id);
        let merged = c3_merge(sequences)?;
        let mut result = vec![id.to_owned()];
        result.extend(merged);
        Some(result)
    }

    let mut visiting = BTreeSet::new();
    match linearize(index, class_id, &mut visiting) {
        Some(full) => {
            let project: Vec<String> = full
                .into_iter()
                .filter(|id| index.classes.contains_key(id))
                .collect();
            (project, false)
        }
        None => (vec![class_id.to_owned()], true),
    }
}

/// Standard C3 merge; `None` when no consistent linearization exists.
fn c3_merge(mut sequences: Vec<Vec<String>>) -> Option<Vec<String>> {
    let mut result = Vec::new();
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return Some(result);
        }
        let mut chosen: Option<String> = None;
        for sequence in &sequences {
            let head = &sequence[0];
            let in_tail = sequences
                .iter()
                .any(|other| other.iter().skip(1).any(|item| item == head));
            if !in_tail {
                chosen = Some(head.clone());
                break;
            }
        }
        let head = chosen?;
        result.push(head.clone());
        for sequence in &mut sequences {
            sequence.retain(|item| item != &head);
        }
    }
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Runs the lifecycle abstract interpreter over every discovered Scene
/// subclass (DESIGN §5.1 step 5).
#[must_use]
pub fn analyze(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
) -> LifecycleFacts {
    let defs = DefMap::build(sources, index);
    let summaries = crate::semantic::summaries::build(sources, index, calls, knowledge, &defs);
    let ctx = Ctx::new(index, calls, knowledge, &defs, &summaries);
    let mut scenes = Vec::new();
    for class_id in &index.scene_classes {
        let Some(record) = index.classes.get(class_id) else {
            continue;
        };
        scenes.push(run_scene(&ctx, record));
    }
    LifecycleFacts { scenes }
}

const LIFECYCLE_PHASES: [(&str, InvocationContext); 4] = [
    ("__init__", InvocationContext::SceneInit),
    ("setup", InvocationContext::Setup),
    ("construct", InvocationContext::Construct),
    ("tear_down", InvocationContext::TearDown),
];

fn run_scene(ctx: &Ctx<'_>, record: &ClassRecord) -> SceneLifecycle {
    let (mro, constructor_state_unknown) = linearize_project(ctx.index, &record.qualified_name);
    let scene_id = ObjectId::new(
        AllocationSite::new(record.file, record.range),
        CallContextId::empty(),
        Cardinality::Singleton,
    );
    let mut heap = AbstractHeap::new();
    heap.insert_scene(
        scene_id.clone(),
        SceneState::initial(KindSet::single(&record.qualified_name)),
    );
    let mut state = ExecState::new(heap);
    let mut sink = TraceSink::default();
    let mut play_counter = 0u64;
    let mut super_calls = BTreeMap::new();

    for (method, phase) in LIFECYCLE_PHASES {
        let Some((def_class, def)) = mro.iter().find_map(|class| {
            ctx.defs
                .defs
                .get(&format!("{class}.{method}"))
                .map(|def| (class.clone(), def))
        }) else {
            continue;
        };
        if let Some(scene) = state.heap.scenes.get_mut(&scene_id) {
            scene.phase = phase;
        }
        let mut method_state = state.clone();
        method_state.env.clear();
        method_state
            .env
            .insert("self".to_owned(), AbstractValue::SelfScene);
        method_state.super_called = Presence::Absent;

        let mut machine = Machine {
            ctx,
            sink: &mut sink,
            file: def.file,
            module: def.module.clone(),
            scene_id: scene_id.clone(),
            mro: mro.clone(),
            reached_bases: record.reached_bases.clone(),
            current_class: Some(def_class),
            current_method: method.to_owned(),
            call_context: CallContextId::empty(),
            play_counter,
            emit: true,
            snapshot: true,
            block: BlockCtx::default(),
            forced_certainty: None,
        };
        let exit = machine.run_body(def.body, &method_state);
        play_counter = machine.play_counter;
        super_calls.insert(method.to_owned(), exit.super_called);
        state = exit;
        state.env.clear();
    }

    let events = sink
        .ops
        .iter()
        .filter_map(|op| {
            op_to_event(op, &scene_id).map(|event| TracedEvent {
                site: op.site,
                certainty: op.certainty,
                event,
            })
        })
        .collect();
    SceneLifecycle {
        qualified_name: record.qualified_name.clone(),
        file: record.file,
        scene_id,
        mro,
        constructor_state_unknown,
        events,
        snapshots: sink.snapshots,
        plays: sink.plays,
        updaters: sink.updaters,
        updater_removals: sink.updater_removals,
        builders: sink.builders,
        target_requirements: sink.target_requirements,
        scene_removals: sink.scene_removals,
        super_calls,
        final_heap: state.heap,
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

    // Return alias: all return paths must agree.
    let mut returns: Option<SummaryReturn> = None;
    for (value, _certainty) in &sink.returns {
        let this = match value_operand(value) {
            SummaryOperand::SelfRef => SummaryReturn::SelfValue,
            SummaryOperand::Param(position) => SummaryReturn::Param(position),
            SummaryOperand::Fresh(site) => SummaryReturn::Fresh(site),
            SummaryOperand::Opaque => SummaryReturn::Unknown,
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

    MethodSummary {
        qualified_name: qualified_name.to_owned(),
        params,
        events,
        returns,
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
        OpKind::SetSelfAttr { name, value } => Some(SummaryEffect::SetSelfAttr {
            name: name.clone(),
            value: value.as_ref().map_or(SummaryOperand::Opaque, translate),
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
