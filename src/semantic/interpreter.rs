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
//!   (DESIGN §5.5),
//! - bounded inlining of `self.<helper>()` calls resolved through the
//!   project MRO (DESIGN §2.1 "Scene helper", §5.1 step 5): a play or
//!   wait inside a helper body materializes as a real [`PlayFact`] per
//!   call site, anchored at the play call in the helper file with the
//!   call chain in [`PlayFact::call_path`]. Recursion and chains deeper
//!   than [`MAX_INLINE_DEPTH`] fall back to the §5.7 effect summary
//!   (membership effects survive; frontier plays stay unmaterialized).
//!
//! Everything the interpreter cannot prove is an explicit `Unknown` /
//! `Maybe` fact — a rule must never receive a wrong certain fact
//! (DESIGN §15 invariant 2).
//!
//! On top of the lifecycle trace this layer exposes the capability facts
//! of the reserved-rule waves:
//!
//! - own-path point/curve counts with `SceneLifecycle::path_state_at`
//!   (MLR116; curated empty-start constructors, exact `set_points` /
//!   `start_new_path` / `add_line_to` arithmetic, unknown mutations widen),
//! - `SceneState::always_update_mobjects` literal tracking and the
//!   [`PlayFact::always_update_mobjects`] snapshot (MLP227),
//! - per-callable [`ReturnFact`]s in [`CallbackReturnFacts`] (MLC123),
//! - conservative [`UpdaterBodyFact`] dataflow classification attached to
//!   every [`UpdaterRegistration`] (MLC112 / MLP218 / MLD301),
//! - per-statement ownership intervals via
//!   `SceneLifecycle::ownership_intervals` (MLC111),
//! - queryable fixed-in-frame / fixed-orientation registrations and the
//!   scene's [`CameraKind`] (renderer-rules groundwork, DESIGN §3.5).
//!
//! Deliberate scope limits of this phase (all degrade to `Unknown`):
//! full camera state, point counts of project-defined mobject subclasses
//! (their `__init__` may build arbitrary paths), effects of nested `def`
//! bodies (their registration identity and signature are modeled; their
//! body runs per-frame and belongs to the cost phase), and `finally`
//! effects on early-return paths (see `frontend::cfg`).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::frontend::cfg::{BasicBlock, CfgStmt, ControlFlowGraph, Terminator};
use crate::frontend::index::{
    CallableSignature, ClassRecord, LiteralFact, ParamKind, ProjectIndex, QualifiedCall,
    QualifiedCallFacts, ReceiverKind,
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
/// Maximum depth of `self.<helper>()` calls inlined during a scene run
/// (DESIGN §5.1 step 5). A recursive or deeper call falls back to the
/// DESIGN §5.7 effect summary: membership and updater effects still
/// apply, while plays inside the unexpanded frontier stay unmaterialized
/// — missing information, never a wrong fact.
const MAX_INLINE_DEPTH: usize = 3;

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
    /// The byte range of the method body (`def`) the statement executed
    /// in. Site-order lookups ([`SceneLifecycle::state_at`]) restrict to
    /// snapshots whose scope contains the query byte, so a query inside
    /// one method never resolves to a state from a textually earlier but
    /// chronologically later statement of a *different* method (helper
    /// bodies execute between the caller's statements).
    pub scope: AllocationSite,
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
    /// `stop_condition`, `always_update_mobjects`, or time-based family
    /// updaters; DESIGN §3.3).
    pub dynamic_wait: Truth,
    /// A `stop_condition` argument was written.
    pub has_stop_condition: bool,
    /// Literal `frozen_frame` argument, when written.
    pub frozen_frame: Option<bool>,
    /// Tracked `self.always_update_mobjects` value at this call
    /// (MLP227): `No` is the Manim default, literal assignments set it
    /// exactly, non-literal writes degrade it to `Maybe`.
    pub always_update_mobjects: Truth,
    /// A `*args` splat appeared among the play arguments: the compiled
    /// animation list is incomplete, so untracked animations may target
    /// (and suspend) any mobject.
    pub star_args: bool,
    /// Path certainty of the call itself. For a play inside an inlined
    /// helper this already composes the certainty of every call site on
    /// [`PlayFact::call_path`] with the play's own path certainty.
    pub certainty: Presence,
    /// How many times one run of the enclosing lifecycle method executes
    /// this play site: exactly `1` outside loops; `[0, n]` inside loops
    /// whose trip counts are all literal `range(...)` bounds (each
    /// execution renders its own frame grid, so frame totals multiply);
    /// open above when any enclosing trip count is unknown — never left
    /// at `1` (DESIGN §4.1: unknowns must not underestimate).
    pub repetitions: Num,
    /// The `self.<helper>()` call sites through which a lifecycle method
    /// reached this play, outermost first; empty for plays written
    /// directly in a lifecycle method. One helper called from two sites
    /// produces one fact per call site (the facts share
    /// [`PlayFact::site`] but differ here and in their animation
    /// identities), so per-site consumers see every execution.
    pub call_path: Vec<AllocationSite>,
}

/// Where an updater was registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdaterHost {
    /// `Mobject.add_updater` on this object.
    Mobject(ObjectId),
    /// `Scene.add_updater` (always called with `(dt)`, DESIGN §3.3).
    Scene,
}

/// Conservative dataflow classification of an updater callback body
/// (MLC112 / MLP218 / MLD301).
///
/// Every field is a [`Truth`]: `Yes` / `No` only when the body proves it,
/// `Maybe` otherwise. A call to an unresolvable function makes
/// [`UpdaterBodyFact::calls_unknown`] `Yes` and degrades every dependent
/// field to at most `Maybe` — a rule must never treat `Maybe` as a
/// definite verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdaterBodyFact {
    /// The body reads the frame-delta parameter (the parameter named `dt`
    /// for mobject updaters, the first parameter for scene updaters).
    pub uses_dt: Truth,
    /// The body provably reads frame-varying state: `ValueTracker`
    /// `get_value`, `random` / `numpy.random`, wall-clock time, or
    /// iterator advancement (`next`). Conditional reads are `Maybe`.
    pub reads_frame_varying: Truth,
    /// The body mutates the updater's mobject parameter (curated mutator
    /// call, raw attribute write, or the parameter escaping into a call).
    pub mutates_target: Truth,
    /// The body performs *only* `shift` / `rotate` / `scale` / `move_to` /
    /// `set_*` calls on the updater parameter plus pure reads. `No` when a
    /// definitely different effect exists, `Maybe` when unprovable.
    pub pure_affine_on_target: Truth,
    /// The body calls something whose identity cannot be resolved (or an
    /// unrecognized method on the updater parameter).
    pub calls_unknown: Truth,
}

impl UpdaterBodyFact {
    /// The all-`Maybe` fact for callbacks whose body is unavailable.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            uses_dt: Truth::Maybe,
            reads_frame_varying: Truth::Maybe,
            mutates_target: Truth::Maybe,
            pure_affine_on_target: Truth::Maybe,
            calls_unknown: Truth::Maybe,
        }
    }
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
    /// Conservative body dataflow classification; all-`Maybe` when the
    /// callback body could not be resolved.
    pub body: UpdaterBodyFact,
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
    /// Path certainty of the removal call itself: `Present` when the
    /// removal runs on every path, `Maybe` on branch- / loop-dependent
    /// paths (the registered-updater set then still may-contains the
    /// callback after a matched removal).
    pub certainty: Presence,
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

/// Return-path classification of a project callable (MLC123).
///
/// `returns_mobject` asks: does *every* normal return path yield a tracked
/// mobject, assuming mobject-valued parameters (the assumption under which
/// `ApplyFunction` invokes its callback)? A bare `return` or a
/// fall-off-the-end path is a definite `No`; a path returning an
/// untracked value is `Maybe` — the two are never conflated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnFact {
    /// Every normal return path yields a (parameter-derived or freshly
    /// allocated) mobject. `No` as soon as one path definitely does not.
    pub returns_mobject: Truth,
    /// Some path executes a bare `return` (returning `None`).
    pub has_bare_return_path: Truth,
    /// Some CFG path reaches the end of the body without a `return`
    /// statement (paths ending in `raise` do not count).
    pub has_no_return_path: Truth,
}

impl ReturnFact {
    /// The all-`Maybe` fact for callables whose body was not analyzed.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            returns_mobject: Truth::Maybe,
            has_bare_return_path: Truth::Maybe,
            has_no_return_path: Truth::Maybe,
        }
    }
}

/// Return facts for every project callable: functions/methods by
/// qualified name, lambdas by their source span — so a rule can resolve
/// both `ApplyFunction(helper, ...)` and `ApplyFunction(lambda m: ..., ...)`
/// arguments (MLC123).
#[derive(Debug, Clone, Default)]
pub struct CallbackReturnFacts {
    /// Facts keyed by qualified callable name (`module.helper`,
    /// `module.Class.method`).
    pub functions: BTreeMap<String, ReturnFact>,
    /// Facts keyed by the lambda expression's source span.
    pub lambdas: BTreeMap<AllocationSite, ReturnFact>,
}

impl CallbackReturnFacts {
    /// The fact for a resolved callback reference.
    #[must_use]
    pub fn for_callback(&self, callback: &CallbackRef) -> Option<&ReturnFact> {
        match callback {
            CallbackRef::Named(name) => self.functions.get(name),
            CallbackRef::Lambda(site) => self.lambdas.get(site),
            CallbackRef::Unknown => None,
        }
    }

    /// The fact for the lambda spanning exactly `start..end` in `file`.
    #[must_use]
    pub fn lambda_at(&self, file: FileId, start: u32, end: u32) -> Option<&ReturnFact> {
        self.lambdas.get(&AllocationSite { file, start, end })
    }
}

/// Own-path state of one mobject at a program point (MLR116).
#[derive(Debug, Clone, PartialEq)]
pub struct PathStateFact {
    /// Own `points` array length (not the family total).
    pub point_count: Num,
    /// Bezier curve count.
    pub curve_count: Num,
    /// Subpath count (currently always `Unknown`; subpath splits are not
    /// tracked).
    pub subpath_count: Num,
    /// Whether the own path is provably empty (`Yes`), provably non-empty
    /// (`No`), or unknown (`Maybe`).
    pub empty: Truth,
}

/// Ownership classification of one object at one statement (MLC111): is
/// it in the scene family, and is it the live target of an in-flight
/// animation of this statement's play?
#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipInterval {
    /// The statement span the classification holds after.
    pub site: AllocationSite,
    /// Effective scene-family membership at this statement.
    pub in_family: Presence,
    /// The object is a live target of a play issued *directly* by this
    /// statement. Plays inside inlined helpers are attributed to the
    /// helper body's own statement intervals, not to the calling
    /// statement (whose membership effects still show in `in_family`),
    /// so `Absent` here describes the steady state after the statement,
    /// never "no animation ever ran here".
    pub animation_target: Presence,
    /// The object carries registered updaters here. `No` is definite;
    /// `Maybe` means the may-set is non-empty — combine with
    /// [`UpdaterRegistration::certainty`] for a definite verdict.
    pub has_updaters: Truth,
}

/// Which 3D fixed-object registry a call touched (DESIGN §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedKind {
    /// `add_fixed_in_frame_mobjects` / `remove_fixed_in_frame_mobjects`.
    InFrame,
    /// `add_fixed_orientation_mobjects` / `remove_...`.
    Orientation,
}

/// Registration vs removal of a fixed-object registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedAction {
    /// The object was registered (and auto-added to the scene).
    Register,
    /// The registration was removed.
    Remove,
}

/// One fixed-in-frame / fixed-orientation registration or removal,
/// queryable per object (renderer-rules groundwork; DESIGN §3.5).
#[derive(Debug, Clone, PartialEq)]
pub struct FixedRegistrationFact {
    /// Source range of the call.
    pub site: AllocationSite,
    /// The affected object.
    pub object: ObjectId,
    /// Which registry.
    pub kind: FixedKind,
    /// Registration vs removal.
    pub action: FixedAction,
    /// The membership effect diverges between Cairo and OpenGL (curated;
    /// true for the removal APIs in v0.20).
    pub renderer_divergent: bool,
    /// Path certainty of the call.
    pub certainty: Presence,
}

/// Which camera contract the scene class commits to (DESIGN §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraKind {
    /// Plain 2D `Scene` camera.
    Standard,
    /// `MovingCameraScene`: an animatable camera frame.
    MovingCamera,
    /// `ThreeDScene`: `ThreeDCamera` with fixed-object registries.
    ThreeD,
    /// Unresolved or mixed base chain — never guessed.
    Unknown,
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
    /// Which camera contract the scene class commits to.
    pub camera_kind: CameraKind,
    /// Fixed-in-frame / fixed-orientation registrations and removals in
    /// program order.
    pub fixed_registrations: Vec<FixedRegistrationFact>,
    /// Per lifecycle method: whether `super().<method>()` was called
    /// (`Absent` = on no path, `Maybe` = on some paths). Only methods the
    /// project defines appear.
    pub super_calls: BTreeMap<String, Presence>,
    /// The abstract heap after `tear_down` (final membership, parents,
    /// updaters, generated targets).
    pub final_heap: AbstractHeap,
}

impl SceneLifecycle {
    /// The last statement snapshot in `file` ending at or before `byte`,
    /// restricted to snapshots of the method body containing `byte`.
    ///
    /// The scope restriction keeps site order and program order aligned:
    /// helper bodies (and the `__init__ → setup → construct → tear_down`
    /// methods) may appear in any textual order and execute between other
    /// statements, so an unrestricted "last site before byte" could
    /// resolve to a chronologically *later* state of a different method.
    /// Every executed body records an entry snapshot at its `def` start,
    /// so the query for its first statement still finds the state the
    /// body started from. A byte inside a never-executed body yields
    /// `None` — no state is known there, never a borrowed one.
    #[must_use]
    pub fn state_at(&self, file: FileId, byte: u32) -> Option<&StateSnapshot> {
        self.snapshots.iter().rfind(|snapshot| {
            snapshot.site.file == file
                && snapshot.site.end <= byte
                && snapshot.scope.file == file
                && snapshot.scope.start <= byte
                && byte <= snapshot.scope.end
        })
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

    /// Own-path state of `object` at the last statement snapshot ending
    /// at or before `byte` in `file` (MLR116).
    ///
    /// Snapshots are taken *after* each statement; pass the byte offset of
    /// the call expression of interest (its start) to observe the state
    /// established before its enclosing statement — e.g. "is the path
    /// provably empty at this `add_line_to` call".
    #[must_use]
    pub fn path_state_at(
        &self,
        object: &ObjectId,
        file: FileId,
        byte: u32,
    ) -> Option<PathStateFact> {
        let snapshot = self.state_at(file, byte)?;
        let state = snapshot.heap.object(object)?;
        let empty = match state.point_count.bounds() {
            Some((_, Some(hi))) if hi <= 0.0 => Truth::Yes,
            Some((Some(lo), _)) if lo >= 1.0 => Truth::No,
            _ => Truth::Maybe,
        };
        Some(PathStateFact {
            point_count: state.point_count.clone(),
            curve_count: state.curve_count.clone(),
            subpath_count: state.subpath_count.clone(),
            empty,
        })
    }

    /// Per-statement ownership classification of `object` (MLC111), in
    /// program order: for every statement snapshot where the object
    /// exists, whether it is in the scene family and whether it is a live
    /// target of a play issued by that statement. A rule looks for
    /// intervals where an updater-bearing object is provably in neither
    /// (`Maybe` membership is never a violation interval).
    #[must_use]
    pub fn ownership_intervals(&self, object: &ObjectId) -> Vec<OwnershipInterval> {
        let mut intervals = Vec::new();
        for snapshot in &self.snapshots {
            let Some(state) = snapshot.heap.object(object) else {
                continue;
            };
            let mut animation_target = Presence::Absent;
            for play in &self.plays {
                let within = play.site.file == snapshot.site.file
                    && play.site.start >= snapshot.site.start
                    && play.site.end <= snapshot.site.end;
                if !within {
                    continue;
                }
                for played in &play.animations {
                    let Some(animation) = &played.state else {
                        continue;
                    };
                    for target in &animation.targets {
                        match target.may_be_same(object) {
                            Truth::Yes if play.certainty == Presence::Present => {
                                animation_target = Presence::Present;
                            }
                            Truth::Yes | Truth::Maybe => {
                                if animation_target != Presence::Present {
                                    animation_target = Presence::Maybe;
                                }
                            }
                            Truth::No => {}
                        }
                    }
                }
            }
            let has_updaters = if state.updaters.is_empty() {
                Truth::No
            } else {
                Truth::Maybe
            };
            intervals.push(OwnershipInterval {
                site: snapshot.site,
                in_family: state.family_membership,
                animation_target,
                has_updaters,
            });
        }
        intervals
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
///
/// # Query map for the capability-pack rules (the follow-up wave)
///
/// - `MLC111`: [`SceneLifecycle::ownership_intervals`] per updater-bearing
///   object (from [`SceneLifecycle::updaters`]); a violation interval
///   needs `in_family == Absent` **and** `animation_target == Absent`
///   plus a `Present`-certainty registration.
/// - `MLC112`: [`UpdaterRegistration::body`] with
///   [`UpdaterBodyFact::reads_frame_varying`] `Yes`,
///   [`UpdaterBodyFact::uses_dt`] `No`, and the wait's
///   [`PlayFact::dynamic_wait`] `No`.
/// - `MLC123`: [`LifecycleFacts::callback_returns`] resolved via the
///   `ApplyFunction` argument (function name or lambda span); fire only on
///   [`ReturnFact::returns_mobject`] `No`.
/// - `MLR116`: [`SceneLifecycle::path_state_at`] at an `add_line_to` /
///   `close_path` call span; fire only on [`PathStateFact::empty`] `Yes`.
/// - `MLP218`: [`UpdaterRegistration::body`] with `uses_dt == No`,
///   `reads_frame_varying == No`, `calls_unknown == No`, and
///   `pure_affine_on_target == Yes`.
/// - `MLP221`: qualified-call literal facts
///   (`frontend::index::LiteralFact::Tuple` / `List` on `t_range` /
///   plot-step arguments).
/// - `MLP227`: [`PlayFact::always_update_mobjects`] `Yes` with no
///   time-based updater / scene updater / stop condition in the interval
///   ([`PlayFact::dynamic_wait`] evidence) and a static
///   [`SceneLifecycle::camera_kind`].
/// - Renderer wave: [`SceneLifecycle::fixed_registrations`] +
///   [`SceneLifecycle::camera_kind`].
#[derive(Debug, Clone, Default)]
pub struct LifecycleFacts {
    /// Per-scene lifecycle analyses, sorted by qualified scene name.
    pub scenes: Vec<SceneLifecycle>,
    /// Return facts for every project callable and lambda (MLC123).
    pub callback_returns: CallbackReturnFacts,
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

/// Coarse classification of a literal constant value.
#[derive(Debug, Clone, Copy, PartialEq)]
enum LiteralValue {
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
    /// A literal constant (definitely not a mobject / animation).
    Literal(LiteralValue),
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
struct ReturnObservation {
    /// The returned value (`Unknown` for bare returns).
    value: AbstractValue,
    /// The statement was a bare `return` (no expression).
    bare: bool,
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
    fixed_registrations: Vec<FixedRegistrationFact>,
    returns: Vec<ReturnObservation>,
    /// A reachable CFG path falls off the end of the body without a
    /// `return` (paths ending in `raise` do not count).
    fall_off_end: bool,
}

/// Per-block execution context while walking a CFG.
#[derive(Debug, Clone, Copy)]
struct BlockCtx {
    loop_depth: u32,
    cond_depth: u32,
    in_definite_loop: bool,
    /// Extra loop-ness from comprehension bodies.
    comprehension: bool,
    /// Upper bound on per-body-run executions of the current site from
    /// the enclosing loops' literal trip counts
    /// ([`crate::frontend::cfg::BasicBlock::repetitions`]); `None` when
    /// any enclosing trip count is unknown.
    repetitions: Option<i64>,
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
    fn repetition_bound(self) -> Num {
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

    /// Whether the canonical class is (or reaches through the curated base
    /// chain) `VMobject`.
    fn is_vmobject_class(&self, class_id: &str) -> bool {
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
    fn is_mobject_class(&self, class_id: &str) -> bool {
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
// Path-state curation (MLR116; verified against
// manim/mobject/types/vectorized_mobject.py and manim/mobject/mobject.py,
// read-only).
// ---------------------------------------------------------------------------

/// Canonical id of `VMobject` / `Mobject`.
const VMOBJECT_ID: &str = "manim.mobject.types.vectorized_mobject.VMobject";
const MOBJECT_ID: &str = "manim.mobject.mobject.Mobject";

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
fn seed_path_counts(object: &mut MobjectState, kind: &KindSet, fact: Option<&QualifiedCall>) {
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
fn clone_path_facts(copy: &mut MobjectState, original: &MobjectState) {
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
fn widen_z_index_family(state: &mut ExecState, target: &ObjectId) {
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
fn apply_z_index_write(
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
fn literal_signed_num(expr: &ast::Expr) -> Option<Num> {
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
fn set_z_index_family_arg(call: &ast::ExprCall) -> Truth {
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
fn seed_z_index(object: &mut MobjectState, fact: Option<&QualifiedCall>) {
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
    /// Emit ops / facts (post-convergence final pass only).
    emit: bool,
    /// Record statement snapshots (final pass of scene methods only).
    snapshot: bool,
    block: BlockCtx,
    /// While applying a summary event: its combined certainty overrides
    /// the block certainty for recorded ops and state effects.
    forced_certainty: Option<Presence>,
    /// This machine runs a scene lifecycle (not a summary extraction):
    /// only scene runs inline `self.<helper>()` bodies.
    scene_run: bool,
    /// Qualified names and call sites of the inlined helper calls
    /// currently on the stack: the recursion guard and the
    /// [`PlayFact::call_path`] source.
    inline_stack: Vec<(String, AllocationSite)>,
    /// Depth context of the enclosing call site while executing an
    /// inlined helper body; composed with every block's own depths so
    /// certainty and cardinality reflect the whole call path.
    base_block: BlockCtx,
    /// Byte range of the method body currently executing (the `def`);
    /// recorded as every snapshot's scope.
    body_site: AllocationSite,
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

    /// Records the state a method body starts from as a zero-width
    /// snapshot at the `def` start. [`SceneLifecycle::state_at`] then
    /// resolves a query at the body's *first* statement (e.g. the
    /// pre-play state of a play that opens the body) to this entry state
    /// instead of finding nothing.
    fn record_entry_snapshot(&mut self, state: &ExecState) {
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
                // Body dataflow is classified after the scene run, once the
                // callback bodies of the whole project are indexed.
                body: UpdaterBodyFact::unknown(),
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
            let certainty = self.certainty();
            self.sink.updater_removals.push(UpdaterRemoval {
                site,
                host: host.map_or(UpdaterHost::Scene, |id| UpdaterHost::Mobject(id.clone())),
                callback: callback.clone(),
                matched,
                certainty,
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
            // The DESIGN §3.2 escape hatch: only a literal proves the
            // behavior; a non-literal value makes suspension Unknown —
            // never "definitely suspends" (DESIGN §15 invariant 2).
            animation.suspend = match literal_bool(argument) {
                Some(true) => SuspendBehavior::SuspendsLiveTargets,
                Some(false) => SuspendBehavior::LeavesUpdatersRunning,
                None => SuspendBehavior::Unknown,
            };
        } else if fact.is_some_and(|fact| fact.has_star_star_kwargs) {
            // A `**kwargs` splat may carry `suspend_mobject_updating`.
            animation.suspend = SuspendBehavior::Unknown;
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
    fn apply_path_method(
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
                return self.call_scene_helper(&qualified, call, fact, state);
            }
        }
        // External base: curated effect if any, else the empty base
        // implementation (`Scene.__init__` / `setup` / `tear_down`).
        if method == "__init__" && matches!(state.env.get("self"), Some(AbstractValue::SelfScene)) {
            // `super().__init__(always_update_mobjects=...)` reaching the
            // external Scene constructor: a literal kwarg is an exact
            // SceneState fact, anything non-literal is Maybe (MLP227).
            if let Some(fact) = fact {
                if let Some(argument) = fact.keyword("always_update_mobjects") {
                    let tracked = literal_bool(argument).map_or(Truth::Maybe, Truth::from);
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.always_update_mobjects = tracked;
                    }
                } else if fact.has_star_star_kwargs {
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.always_update_mobjects =
                            scene.always_update_mobjects.join(Truth::Maybe);
                    }
                }
            }
        }
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
                AbstractValue::Literal(_) => {
                    // A literal constant cannot be converted to an
                    // animation either (MLC102 evidence).
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::No,
                        channels_known: Truth::No,
                    });
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

        // 2. apply play kwargs to every animation (setattr semantics,
        //    scene.py compile_animations).
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
        // A play-level `suspend_mobject_updating` overrides every
        // animation's constructor value; a literal proves the behavior, a
        // non-literal (or a `**kwargs` splat that may carry the key)
        // degrades it to Unknown — never "definitely suspends".
        let play_suspend = match fact.and_then(|fact| fact.keyword("suspend_mobject_updating")) {
            Some(argument) => Some(match literal_bool(argument) {
                Some(true) => SuspendBehavior::SuspendsLiveTargets,
                Some(false) => SuspendBehavior::LeavesUpdatersRunning,
                None => SuspendBehavior::Unknown,
            }),
            None if fact.is_some_and(|fact| fact.has_star_star_kwargs) => {
                Some(SuspendBehavior::Unknown)
            }
            None => None,
        };
        if let Some(suspend) = play_suspend {
            for played in &mut compiled {
                if let Some(animation_state) = &mut played.state {
                    animation_state.suspend = suspend;
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
                        clone_path_facts(&mut fresh, object);
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
            // Only a complete channel classification proves the animation
            // left `z_index` alone (curated families never write it); a
            // custom `interpolate` may set it on any live target.
            if played.channels_known != Truth::Yes {
                for target in &targets {
                    widen_z_index_family(state, target);
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
            let always_update = state
                .heap
                .scenes
                .get(&self.scene_id)
                .map_or(Truth::Maybe, |scene| scene.always_update_mobjects);
            self.sink.plays.push(PlayFact {
                site: call_site,
                play_group: PlayGroupId(group),
                kind: PlayKind::Play,
                duration,
                animations: compiled,
                dynamic_wait: Truth::No,
                has_stop_condition: false,
                frozen_frame: None,
                always_update_mobjects: always_update,
                star_args: call
                    .args
                    .iter()
                    .any(|arg| matches!(arg, ast::Expr::Starred(_))),
                certainty,
                repetitions: self.block.repetition_bound(),
                call_path: self.inline_call_path(),
            });
        }
        AbstractValue::Unknown
    }

    /// The helper call sites currently on the inline stack, outermost
    /// first ([`PlayFact::call_path`]).
    fn inline_call_path(&self) -> Vec<AllocationSite> {
        self.inline_stack.iter().map(|(_, site)| *site).collect()
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
        let always_update = state
            .heap
            .scenes
            .get(&self.scene_id)
            .map_or(Truth::Maybe, |scene| scene.always_update_mobjects);
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
        }
        // Tracked `always_update_mobjects` (DESIGN §3.3): a literal True
        // makes every wait dynamic; a non-literal write is a maybe-fact.
        match always_update {
            Truth::Yes => dynamic = Truth::Yes,
            Truth::Maybe => {
                if dynamic == Truth::No {
                    dynamic = Truth::Maybe;
                }
            }
            Truth::No => {}
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
                always_update_mobjects: always_update,
                star_args: false,
                certainty: self.certainty(),
                repetitions: self.block.repetition_bound(),
                call_path: self.inline_call_path(),
            });
        }
        AbstractValue::Unknown
    }

    // -- helper inlining (scene runs only) -----------------------------------

    /// A `self.<method>()` / `super().<method>()` call resolved to a
    /// project definition: inline the body during scene runs (bounded by
    /// [`MAX_INLINE_DEPTH`], recursion falls back), apply the effect
    /// summary otherwise.
    ///
    /// Inlining executes the helper body against the live caller state,
    /// so plays and waits inside it materialize as real [`PlayFact`]s
    /// with their sites in the helper file, exact per-animation argument
    /// facts, membership effects applied exactly once (the summary is
    /// *not* applied for an inlined call), and wait dynamics judged on
    /// the caller's actual updater state. Summary application remains
    /// the semantics for summary runs (SCC fixpoints stay compositional)
    /// and for the recursion / depth fallback frontier.
    fn call_scene_helper(
        &mut self,
        qualified: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let recursive = self.inline_stack.iter().any(|(name, _)| name == qualified);
        if !self.scene_run || recursive || self.inline_stack.len() >= MAX_INLINE_DEPTH {
            return self.apply_summary_call(
                qualified,
                Some(AbstractValue::SelfScene),
                call,
                fact,
                state,
            );
        }
        let Some(def) = self.ctx.defs.defs.get(qualified).cloned() else {
            return self.apply_summary_call(
                qualified,
                Some(AbstractValue::SelfScene),
                call,
                fact,
                state,
            );
        };
        self.inline_scene_method(qualified, &def, call, state)
    }

    /// Executes one resolved helper body inline (depth-limited by the
    /// caller). Arguments bind to the declared parameter names, the
    /// receiver binds to the scene, and every recorded op composes the
    /// call site's certainty / loop context through
    /// [`Machine::base_block`].
    #[allow(
        clippy::too_many_lines,
        reason = "argument binding, frame switch, and write-back form one sequence"
    )]
    fn inline_scene_method(
        &mut self,
        qualified: &str,
        def: &FnDef<'a>,
        call: &'a ast::ExprCall,
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
        let mut next = 1usize;
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
        if let Some(receiver) = params.first() {
            bindings.insert(receiver.clone(), AbstractValue::SelfScene);
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

    // Every lambda of the project, keyed by its source span: resolves
    // updater callbacks and `ApplyFunction` arguments (MLC112 / MLC123).
    let parsed = crate::frontend::parser::parsed_modules(sources);
    let mut lambda_index: BTreeMap<AllocationSite, &ast::ExprLambda> = BTreeMap::new();
    for module in &parsed {
        let mut found = Vec::new();
        collect_lambdas(&module.ast.body, &mut found);
        for lambda in found {
            lambda_index.insert(AllocationSite::new(module.file, lambda.range()), lambda);
        }
    }

    let mut scenes = Vec::new();
    for class_id in &index.scene_classes {
        let Some(record) = index.classes.get(class_id) else {
            continue;
        };
        scenes.push(run_scene(&ctx, record));
    }

    // Updater-body dataflow classification (MLC112 / MLP218): resolve
    // every registered callback body and attach its conservative fact.
    for scene in &mut scenes {
        for registration in &mut scene.updaters {
            let scene_level = matches!(registration.host, UpdaterHost::Scene);
            registration.body = classify_updater_callback(
                &ctx,
                &lambda_index,
                scene_level,
                &registration.fact.callback,
            );
        }
    }

    // Callback return facts (MLC123): every summarized project callable
    // plus every lambda.
    let mut callback_returns = CallbackReturnFacts::default();
    for (name, summary) in &summaries.summaries {
        callback_returns
            .functions
            .insert(name.clone(), summary.return_fact);
    }
    for (site, lambda) in &lambda_index {
        callback_returns
            .lambdas
            .insert(*site, lambda_return_fact(&ctx, site.file, lambda));
    }

    LifecycleFacts {
        scenes,
        callback_returns,
    }
}

const LIFECYCLE_PHASES: [(&str, InvocationContext); 4] = [
    ("__init__", InvocationContext::SceneInit),
    ("setup", InvocationContext::Setup),
    ("construct", InvocationContext::Construct),
    ("tear_down", InvocationContext::TearDown),
];

/// Canonical scene base ids that decide the camera contract.
const THREE_D_SCENE_ID: &str = "manim.scene.three_d_scene.ThreeDScene";
const MOVING_CAMERA_SCENE_ID: &str = "manim.scene.moving_camera_scene.MovingCameraScene";
const SCENE_ID: &str = "manim.scene.scene.Scene";

/// The camera contract a scene class commits to (DESIGN §3.5), derived
/// from its reached external bases through the curated base chain. An
/// unresolved or mixed chain is `Unknown` — never guessed.
fn scene_camera_kind(ctx: &Ctx<'_>, record: &ClassRecord) -> CameraKind {
    if !record.bases_fully_resolved {
        return CameraKind::Unknown;
    }
    let mut three_d = false;
    let mut moving = false;
    let mut plain = false;
    for base in &record.reached_bases {
        if ctx.reaches_base(base, THREE_D_SCENE_ID) {
            three_d = true;
        } else if ctx.reaches_base(base, MOVING_CAMERA_SCENE_ID) {
            moving = true;
        } else if ctx.reaches_base(base, SCENE_ID) {
            plain = true;
        }
        // Non-scene mixins do not change the camera kind.
    }
    match (three_d, moving) {
        (true, false) => CameraKind::ThreeD,
        (false, true) => CameraKind::MovingCamera,
        (false, false) if plain => CameraKind::Standard,
        // Mixed 3D + moving chains and non-scene chains stay unresolved.
        _ => CameraKind::Unknown,
    }
}

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
            scene_run: true,
            inline_stack: Vec::new(),
            base_block: BlockCtx::default(),
            body_site: AllocationSite::new(def.file, def.range),
        };
        machine.record_entry_snapshot(&method_state);
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
        camera_kind: scene_camera_kind(ctx, record),
        fixed_registrations: sink.fixed_registrations,
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

// ---------------------------------------------------------------------------
// Lambda collection (shared by updater-body classification and MLC123).
// ---------------------------------------------------------------------------

/// Collects every lambda expression under `stmts`, recursing through all
/// nested statements and expressions (nested defs and lambdas included).
fn collect_lambdas<'a>(stmts: &'a [ast::Stmt], out: &mut Vec<&'a ast::ExprLambda>) {
    for stmt in stmts {
        collect_lambdas_stmt(stmt, out);
    }
}

fn collect_lambdas_args<'a>(args: &'a ast::Arguments, out: &mut Vec<&'a ast::ExprLambda>) {
    for arg in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        if let Some(default) = &arg.default {
            collect_lambdas_expr(default, out);
        }
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
fn collect_lambdas_stmt<'a>(stmt: &'a ast::Stmt, out: &mut Vec<&'a ast::ExprLambda>) {
    match stmt {
        ast::Stmt::FunctionDef(def) => {
            for decorator in &def.decorator_list {
                collect_lambdas_expr(decorator, out);
            }
            collect_lambdas_args(&def.args, out);
            collect_lambdas(&def.body, out);
        }
        ast::Stmt::AsyncFunctionDef(def) => {
            for decorator in &def.decorator_list {
                collect_lambdas_expr(decorator, out);
            }
            collect_lambdas_args(&def.args, out);
            collect_lambdas(&def.body, out);
        }
        ast::Stmt::ClassDef(def) => {
            for decorator in &def.decorator_list {
                collect_lambdas_expr(decorator, out);
            }
            for base in &def.bases {
                collect_lambdas_expr(base, out);
            }
            for keyword in &def.keywords {
                collect_lambdas_expr(&keyword.value, out);
            }
            collect_lambdas(&def.body, out);
        }
        ast::Stmt::Assign(inner) => {
            for target in &inner.targets {
                collect_lambdas_expr(target, out);
            }
            collect_lambdas_expr(&inner.value, out);
        }
        ast::Stmt::AnnAssign(inner) => {
            collect_lambdas_expr(&inner.target, out);
            if let Some(value) = &inner.value {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Stmt::AugAssign(inner) => {
            collect_lambdas_expr(&inner.target, out);
            collect_lambdas_expr(&inner.value, out);
        }
        ast::Stmt::Expr(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Stmt::Return(inner) => {
            if let Some(value) = &inner.value {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Stmt::Raise(inner) => {
            for part in [&inner.exc, &inner.cause].into_iter().flatten() {
                collect_lambdas_expr(part, out);
            }
        }
        ast::Stmt::Assert(inner) => {
            collect_lambdas_expr(&inner.test, out);
            if let Some(message) = &inner.msg {
                collect_lambdas_expr(message, out);
            }
        }
        ast::Stmt::Delete(inner) => {
            for target in &inner.targets {
                collect_lambdas_expr(target, out);
            }
        }
        ast::Stmt::If(inner) => {
            collect_lambdas_expr(&inner.test, out);
            collect_lambdas(&inner.body, out);
            collect_lambdas(&inner.orelse, out);
        }
        ast::Stmt::While(inner) => {
            collect_lambdas_expr(&inner.test, out);
            collect_lambdas(&inner.body, out);
            collect_lambdas(&inner.orelse, out);
        }
        ast::Stmt::For(inner) => {
            collect_lambdas_expr(&inner.target, out);
            collect_lambdas_expr(&inner.iter, out);
            collect_lambdas(&inner.body, out);
            collect_lambdas(&inner.orelse, out);
        }
        ast::Stmt::AsyncFor(inner) => {
            collect_lambdas_expr(&inner.target, out);
            collect_lambdas_expr(&inner.iter, out);
            collect_lambdas(&inner.body, out);
            collect_lambdas(&inner.orelse, out);
        }
        ast::Stmt::With(inner) => {
            for item in &inner.items {
                collect_lambdas_expr(&item.context_expr, out);
            }
            collect_lambdas(&inner.body, out);
        }
        ast::Stmt::AsyncWith(inner) => {
            for item in &inner.items {
                collect_lambdas_expr(&item.context_expr, out);
            }
            collect_lambdas(&inner.body, out);
        }
        ast::Stmt::Try(inner) => {
            collect_lambdas(&inner.body, out);
            for handler in &inner.handlers {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(kind) = &handler.type_ {
                    collect_lambdas_expr(kind, out);
                }
                collect_lambdas(&handler.body, out);
            }
            collect_lambdas(&inner.orelse, out);
            collect_lambdas(&inner.finalbody, out);
        }
        ast::Stmt::TryStar(inner) => {
            collect_lambdas(&inner.body, out);
            for handler in &inner.handlers {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(kind) = &handler.type_ {
                    collect_lambdas_expr(kind, out);
                }
                collect_lambdas(&handler.body, out);
            }
            collect_lambdas(&inner.orelse, out);
            collect_lambdas(&inner.finalbody, out);
        }
        ast::Stmt::Match(inner) => {
            collect_lambdas_expr(&inner.subject, out);
            for case in &inner.cases {
                if let Some(guard) = &case.guard {
                    collect_lambdas_expr(guard, out);
                }
                collect_lambdas(&case.body, out);
            }
        }
        ast::Stmt::Import(_)
        | ast::Stmt::ImportFrom(_)
        | ast::Stmt::Global(_)
        | ast::Stmt::Nonlocal(_)
        | ast::Stmt::TypeAlias(_)
        | ast::Stmt::Pass(_)
        | ast::Stmt::Break(_)
        | ast::Stmt::Continue(_) => {}
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
fn collect_lambdas_expr<'a>(expr: &'a ast::Expr, out: &mut Vec<&'a ast::ExprLambda>) {
    match expr {
        ast::Expr::Lambda(lambda) => {
            out.push(lambda);
            collect_lambdas_args(&lambda.args, out);
            collect_lambdas_expr(&lambda.body, out);
        }
        ast::Expr::BoolOp(inner) => {
            for value in &inner.values {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Expr::NamedExpr(inner) => {
            collect_lambdas_expr(&inner.target, out);
            collect_lambdas_expr(&inner.value, out);
        }
        ast::Expr::BinOp(inner) => {
            collect_lambdas_expr(&inner.left, out);
            collect_lambdas_expr(&inner.right, out);
        }
        ast::Expr::UnaryOp(inner) => collect_lambdas_expr(&inner.operand, out),
        ast::Expr::IfExp(inner) => {
            collect_lambdas_expr(&inner.test, out);
            collect_lambdas_expr(&inner.body, out);
            collect_lambdas_expr(&inner.orelse, out);
        }
        ast::Expr::Dict(inner) => {
            for key in inner.keys.iter().flatten() {
                collect_lambdas_expr(key, out);
            }
            for value in &inner.values {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Expr::Set(inner) => {
            for element in &inner.elts {
                collect_lambdas_expr(element, out);
            }
        }
        ast::Expr::ListComp(inner) => {
            collect_lambdas_expr(&inner.elt, out);
            collect_lambdas_generators(&inner.generators, out);
        }
        ast::Expr::SetComp(inner) => {
            collect_lambdas_expr(&inner.elt, out);
            collect_lambdas_generators(&inner.generators, out);
        }
        ast::Expr::DictComp(inner) => {
            collect_lambdas_expr(&inner.key, out);
            collect_lambdas_expr(&inner.value, out);
            collect_lambdas_generators(&inner.generators, out);
        }
        ast::Expr::GeneratorExp(inner) => {
            collect_lambdas_expr(&inner.elt, out);
            collect_lambdas_generators(&inner.generators, out);
        }
        ast::Expr::Await(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Expr::Yield(inner) => {
            if let Some(value) = &inner.value {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Expr::YieldFrom(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Expr::Compare(inner) => {
            collect_lambdas_expr(&inner.left, out);
            for comparator in &inner.comparators {
                collect_lambdas_expr(comparator, out);
            }
        }
        ast::Expr::Call(inner) => {
            collect_lambdas_expr(&inner.func, out);
            for arg in &inner.args {
                collect_lambdas_expr(arg, out);
            }
            for keyword in &inner.keywords {
                collect_lambdas_expr(&keyword.value, out);
            }
        }
        ast::Expr::FormattedValue(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Expr::JoinedStr(inner) => {
            for value in &inner.values {
                collect_lambdas_expr(value, out);
            }
        }
        ast::Expr::Attribute(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Expr::Subscript(inner) => {
            collect_lambdas_expr(&inner.value, out);
            collect_lambdas_expr(&inner.slice, out);
        }
        ast::Expr::Starred(inner) => collect_lambdas_expr(&inner.value, out),
        ast::Expr::List(inner) => {
            for element in &inner.elts {
                collect_lambdas_expr(element, out);
            }
        }
        ast::Expr::Tuple(inner) => {
            for element in &inner.elts {
                collect_lambdas_expr(element, out);
            }
        }
        ast::Expr::Slice(inner) => {
            for part in [&inner.lower, &inner.upper, &inner.step]
                .into_iter()
                .flatten()
            {
                collect_lambdas_expr(part, out);
            }
        }
        ast::Expr::Constant(_) | ast::Expr::Name(_) => {}
    }
}

fn collect_lambdas_generators<'a>(
    generators: &'a [ast::Comprehension],
    out: &mut Vec<&'a ast::ExprLambda>,
) {
    for generator in generators {
        collect_lambdas_expr(&generator.target, out);
        collect_lambdas_expr(&generator.iter, out);
        for condition in &generator.ifs {
            collect_lambdas_expr(condition, out);
        }
    }
}

// ---------------------------------------------------------------------------
// Updater-body dataflow classification (MLC112 / MLP218 / MLD301).
// ---------------------------------------------------------------------------

/// Methods on the updater parameter allowed by `pure_affine_on_target`.
fn is_affine_or_setter(method: &str) -> bool {
    matches!(method, "shift" | "rotate" | "scale" | "move_to") || method.starts_with("set_")
}

/// Curated frame-varying read sources, by canonical candidate id.
/// `Yes`-evidence is reserved for provable reads (MLC112 fires on `Yes`);
/// everything else stays `Maybe` at most.
fn frame_varying_candidate(candidate: &str) -> bool {
    (candidate.starts_with("manim.mobject.value_tracker.") && candidate.ends_with(".get_value"))
        || (candidate.starts_with("random.") && candidate != "random.seed")
        || candidate.starts_with("numpy.random.")
        || matches!(
            candidate,
            "time.time"
                | "time.monotonic"
                | "time.perf_counter"
                | "time.time_ns"
                | "time.monotonic_ns"
                | "time.perf_counter_ns"
        )
}

/// Candidates that are provably pure, deterministic computations.
fn pure_deterministic_candidate(candidate: &str) -> bool {
    (candidate.starts_with("numpy.") && !candidate.starts_with("numpy.random."))
        || candidate.starts_with("math.")
}

/// Accumulates occurrence evidence: `Yes` sticks, any `Maybe` degrades a
/// `No`, and `No` + `No` stays `No`.
fn bump(slot: &mut Truth, evidence: Truth) {
    *slot = match (*slot, evidence) {
        (Truth::Yes, _) | (_, Truth::Yes) => Truth::Yes,
        (Truth::No, Truth::No) => Truth::No,
        _ => Truth::Maybe,
    };
}

/// The root name of a (possibly dotted / subscripted) expression.
fn root_name(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.as_str()),
        ast::Expr::Attribute(attribute) => root_name(&attribute.value),
        ast::Expr::Subscript(subscript) => root_name(&subscript.value),
        ast::Expr::Starred(starred) => root_name(&starred.value),
        _ => None,
    }
}

/// The body being classified.
enum BodyRef<'a> {
    /// A `def` body.
    Stmts(&'a [ast::Stmt]),
    /// A lambda body (one expression, evaluated unconditionally).
    Expr(&'a ast::Expr),
}

/// Syntactic, conservative classifier over one updater callback body.
struct BodyClassifier<'a, 'b> {
    ctx: &'b Ctx<'a>,
    file: FileId,
    /// The updater's mobject parameter name (`None` for scene updaters or
    /// while shadowed by a nested scope).
    target: Option<String>,
    /// The frame-delta parameter name (`None` while shadowed).
    dt: Option<String>,
    /// Depth inside nested callables defined in the body (their code may
    /// or may not run per frame: evidence degrades to `Maybe`).
    nested: u32,
    /// Depth inside conditionally executed regions (branch arms, loop
    /// bodies, handlers, short-circuit operands): a conditional read is
    /// not a per-frame proof.
    conditional: u32,
    uses_dt: bool,
    reads_frame_varying: Truth,
    mutates_target: Truth,
    /// Evidence of an operation outside the pure-affine allowlist.
    disallowed: Truth,
    calls_unknown: Truth,
}

impl BodyClassifier<'_, '_> {
    fn evidence(&self) -> Truth {
        if self.nested > 0 || self.conditional > 0 {
            Truth::Maybe
        } else {
            Truth::Yes
        }
    }

    fn walk_stmts(&mut self, stmts: &[ast::Stmt]) {
        for stmt in stmts {
            self.walk_stmt(stmt);
        }
    }

    fn conditional_stmts(&mut self, stmts: &[ast::Stmt]) {
        self.conditional += 1;
        self.walk_stmts(stmts);
        self.conditional -= 1;
    }

    fn conditional_expr(&mut self, expr: &ast::Expr) {
        self.conditional += 1;
        self.walk_expr(expr);
        self.conditional -= 1;
    }

    /// Enters a nested callable scope, shadowing re-bound tracked names.
    fn nested_scope(&mut self, params: &ast::Arguments, walk: impl FnOnce(&mut Self)) {
        let mut bound: BTreeSet<String> = BTreeSet::new();
        for arg in params
            .posonlyargs
            .iter()
            .chain(&params.args)
            .chain(&params.kwonlyargs)
        {
            bound.insert(arg.def.arg.to_string());
        }
        for arg in [&params.vararg, &params.kwarg].into_iter().flatten() {
            bound.insert(arg.arg.to_string());
        }
        let saved_dt = self.dt.clone();
        let saved_target = self.target.clone();
        if self.dt.as_ref().is_some_and(|name| bound.contains(name)) {
            self.dt = None;
        }
        if self
            .target
            .as_ref()
            .is_some_and(|name| bound.contains(name))
        {
            self.target = None;
        }
        self.nested += 1;
        walk(self);
        self.nested -= 1;
        self.dt = saved_dt;
        self.target = saved_target;
    }

    /// A write target of an assignment / deletion.
    fn classify_store_target(&mut self, target: &ast::Expr) {
        match target {
            // Local rebinds are pure.
            ast::Expr::Name(_) => {}
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.classify_store_target(element);
                }
            }
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.classify_store_target(element);
                }
            }
            ast::Expr::Starred(inner) => self.classify_store_target(&inner.value),
            _ => {
                // Attribute / subscript writes: a raw mutation of whatever
                // the root binding refers to (e.g. `mob.points = ...`).
                let evidence = self.evidence();
                if root_name(target).is_some_and(|root| self.target.as_deref() == Some(root)) {
                    bump(&mut self.mutates_target, evidence);
                }
                bump(&mut self.disallowed, evidence);
                self.walk_expr(target);
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn walk_stmt(&mut self, stmt: &ast::Stmt) {
        match stmt {
            ast::Stmt::Expr(inner) => self.walk_expr(&inner.value),
            ast::Stmt::Return(inner) => {
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                }
            }
            ast::Stmt::Assign(inner) => {
                self.walk_expr(&inner.value);
                for target in &inner.targets {
                    self.classify_store_target(target);
                }
            }
            ast::Stmt::AugAssign(inner) => {
                self.walk_expr(&inner.value);
                self.classify_store_target(&inner.target);
            }
            ast::Stmt::AnnAssign(inner) => {
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                    self.classify_store_target(&inner.target);
                }
            }
            ast::Stmt::If(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::While(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::For(inner) => {
                self.walk_expr(&inner.iter);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::AsyncFor(inner) => {
                self.walk_expr(&inner.iter);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    self.walk_expr(&item.context_expr);
                }
                self.walk_stmts(&inner.body);
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    self.walk_expr(&item.context_expr);
                }
                self.walk_stmts(&inner.body);
            }
            ast::Stmt::Try(inner) => {
                self.conditional_stmts(&inner.body);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(kind) = &handler.type_ {
                        self.walk_expr(kind);
                    }
                    self.conditional_stmts(&handler.body);
                }
                self.conditional_stmts(&inner.orelse);
                self.walk_stmts(&inner.finalbody);
            }
            ast::Stmt::TryStar(inner) => {
                self.conditional_stmts(&inner.body);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(kind) = &handler.type_ {
                        self.walk_expr(kind);
                    }
                    self.conditional_stmts(&handler.body);
                }
                self.conditional_stmts(&inner.orelse);
                self.walk_stmts(&inner.finalbody);
            }
            ast::Stmt::Match(inner) => {
                self.walk_expr(&inner.subject);
                for case in &inner.cases {
                    if let Some(guard) = &case.guard {
                        self.conditional_expr(guard);
                    }
                    self.conditional_stmts(&case.body);
                }
            }
            ast::Stmt::Raise(inner) => {
                for part in [&inner.exc, &inner.cause].into_iter().flatten() {
                    self.walk_expr(part);
                }
            }
            ast::Stmt::Assert(inner) => {
                self.walk_expr(&inner.test);
                if let Some(message) = &inner.msg {
                    self.walk_expr(message);
                }
            }
            ast::Stmt::Delete(inner) => {
                for target in &inner.targets {
                    self.classify_store_target(target);
                }
            }
            ast::Stmt::FunctionDef(def) => {
                collect_defaults_walk(self, &def.args);
                self.nested_scope(&def.args, |classifier| {
                    classifier.walk_stmts(&def.body);
                });
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                collect_defaults_walk(self, &def.args);
                self.nested_scope(&def.args, |classifier| {
                    classifier.walk_stmts(&def.body);
                });
            }
            ast::Stmt::ClassDef(def) => {
                // A class body executes at definition time; treat it as a
                // nested (maybe-running) region.
                self.nested += 1;
                self.walk_stmts(&def.body);
                self.nested -= 1;
            }
            ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => {
                // Declared intent to write enclosing-scope state.
                bump(&mut self.disallowed, Truth::Maybe);
            }
            ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => {
                bump(&mut self.disallowed, Truth::Maybe);
            }
            ast::Stmt::TypeAlias(_)
            | ast::Stmt::Pass(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
    fn walk_expr(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::Name(name) => {
                if self.dt.as_deref() == Some(name.id.as_str()) {
                    self.uses_dt = true;
                }
            }
            ast::Expr::Call(call) => self.classify_call(call),
            ast::Expr::Lambda(lambda) => {
                collect_defaults_walk(self, &lambda.args);
                self.nested_scope(&lambda.args, |classifier| {
                    classifier.walk_expr(&lambda.body);
                });
            }
            ast::Expr::BoolOp(inner) => {
                let mut values = inner.values.iter();
                if let Some(first) = values.next() {
                    self.walk_expr(first);
                }
                for value in values {
                    self.conditional_expr(value);
                }
            }
            ast::Expr::NamedExpr(inner) => {
                self.walk_expr(&inner.value);
            }
            ast::Expr::BinOp(inner) => {
                self.walk_expr(&inner.left);
                self.walk_expr(&inner.right);
            }
            ast::Expr::UnaryOp(inner) => self.walk_expr(&inner.operand),
            ast::Expr::IfExp(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_expr(&inner.body);
                self.conditional_expr(&inner.orelse);
            }
            ast::Expr::Dict(inner) => {
                for key in inner.keys.iter().flatten() {
                    self.walk_expr(key);
                }
                for value in &inner.values {
                    self.walk_expr(value);
                }
            }
            ast::Expr::Set(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::ListComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::SetComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::DictComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.key, &inner.value]);
            }
            ast::Expr::GeneratorExp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::Await(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                self.walk_expr(&inner.value);
            }
            ast::Expr::Yield(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                }
            }
            ast::Expr::YieldFrom(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                self.walk_expr(&inner.value);
            }
            ast::Expr::Compare(inner) => {
                self.walk_expr(&inner.left);
                for comparator in &inner.comparators {
                    self.walk_expr(comparator);
                }
            }
            ast::Expr::FormattedValue(inner) => self.walk_expr(&inner.value),
            ast::Expr::JoinedStr(inner) => {
                for value in &inner.values {
                    self.walk_expr(value);
                }
            }
            ast::Expr::Attribute(inner) => self.walk_expr(&inner.value),
            ast::Expr::Subscript(inner) => {
                self.walk_expr(&inner.value);
                self.walk_expr(&inner.slice);
            }
            ast::Expr::Starred(inner) => self.walk_expr(&inner.value),
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::Slice(inner) => {
                for part in [&inner.lower, &inner.upper, &inner.step]
                    .into_iter()
                    .flatten()
                {
                    self.walk_expr(part);
                }
            }
            ast::Expr::Constant(_) => {}
        }
    }

    fn walk_generators(&mut self, generators: &[ast::Comprehension], elements: &[&ast::Expr]) {
        // The first iterable evaluates unconditionally; everything else
        // runs per element (0..n times).
        let mut iterables = generators.iter().map(|generator| &generator.iter);
        if let Some(first) = iterables.next() {
            self.walk_expr(first);
        }
        for iterable in iterables {
            self.conditional_expr(iterable);
        }
        // Comprehension targets shadow tracked names inside the element.
        let mut bound: BTreeSet<String> = BTreeSet::new();
        for generator in generators {
            crate::frontend::names::collect_target_names(&generator.target, &mut bound);
        }
        let saved_dt = self.dt.clone();
        let saved_target = self.target.clone();
        if self.dt.as_ref().is_some_and(|name| bound.contains(name)) {
            self.dt = None;
        }
        if self
            .target
            .as_ref()
            .is_some_and(|name| bound.contains(name))
        {
            self.target = None;
        }
        for generator in generators {
            for condition in &generator.ifs {
                self.conditional_expr(condition);
            }
        }
        for element in elements {
            self.conditional_expr(element);
        }
        self.dt = saved_dt;
        self.target = saved_target;
    }

    /// The updater parameter appearing as an argument of a call whose
    /// effects are not modeled: the callee may mutate it.
    fn check_target_escape(&mut self, call: &ast::ExprCall) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let mentions = call
            .args
            .iter()
            .chain(call.keywords.iter().map(|keyword| &keyword.value))
            .any(|argument| root_name(argument) == Some(target.as_str()));
        if mentions {
            bump(&mut self.mutates_target, Truth::Maybe);
            bump(&mut self.disallowed, Truth::Maybe);
        }
    }

    fn walk_call_args(&mut self, call: &ast::ExprCall) {
        for argument in &call.args {
            self.walk_expr(argument);
        }
        for keyword in &call.keywords {
            self.walk_expr(&keyword.value);
        }
    }

    fn classify_call(&mut self, call: &ast::ExprCall) {
        let evidence = self.evidence();
        // Method call on the updater's mobject parameter.
        if let ast::Expr::Attribute(attribute) = call.func.as_ref() {
            if let ast::Expr::Name(base) = attribute.value.as_ref() {
                if self.target.as_deref() == Some(base.id.as_str()) {
                    let method = attribute.attr.as_str();
                    if is_affine_or_setter(method) {
                        bump(&mut self.mutates_target, evidence);
                    } else if method.starts_with("get_") || method == "copy" {
                        // Curated read convention: `get_*` / `copy` do not
                        // mutate the receiver.
                    } else if mutator_channels(&format!("target.{method}")).is_some()
                        || matches!(
                            method,
                            "become"
                                | "add"
                                | "remove"
                                | "add_updater"
                                | "remove_updater"
                                | "clear_updaters"
                                | "generate_target"
                                | "save_state"
                                | "restore"
                        )
                    {
                        // A curated mutator outside the affine allowlist.
                        bump(&mut self.mutates_target, evidence);
                        bump(&mut self.disallowed, evidence);
                    } else {
                        // Unrecognized method on the target parameter.
                        bump(&mut self.mutates_target, Truth::Maybe);
                        bump(&mut self.calls_unknown, evidence);
                        bump(&mut self.disallowed, Truth::Maybe);
                    }
                    self.walk_call_args(call);
                    return;
                }
            }
        }
        // A callee the frontend resolved to candidates.
        let candidates = self
            .ctx
            .fact(self.file, call.range())
            .map(|fact| fact.candidates.clone())
            .filter(|candidates| !candidates.is_empty());
        if let Some(candidates) = candidates {
            if candidates
                .iter()
                .all(|candidate| frame_varying_candidate(candidate))
            {
                bump(&mut self.reads_frame_varying, evidence);
            } else if candidates
                .iter()
                .any(|candidate| frame_varying_candidate(candidate))
            {
                bump(&mut self.reads_frame_varying, Truth::Maybe);
                bump(&mut self.disallowed, Truth::Maybe);
            } else if candidates
                .iter()
                .all(|candidate| pure_deterministic_candidate(candidate))
            {
                // Pure computation: no reads, no effects.
            } else {
                // Resolvable, but its effects are not modeled here: not an
                // unknown call, yet also not provably frame-invariant or
                // pure-affine.
                bump(&mut self.reads_frame_varying, Truth::Maybe);
                bump(&mut self.disallowed, Truth::Maybe);
            }
            self.check_target_escape(call);
            self.walk_expr(&call.func);
            self.walk_call_args(call);
            return;
        }
        // No candidates: a builtin or an unresolvable callee.
        if let ast::Expr::Name(name) = call.func.as_ref() {
            let name = name.id.as_str();
            if name == "next" {
                // Iterator advancement reads (and moves) external state.
                bump(&mut self.reads_frame_varying, evidence);
                bump(&mut self.disallowed, Truth::Maybe);
                self.check_target_escape(call);
                self.walk_call_args(call);
                return;
            }
            if PURE_BUILTINS.contains(&name) {
                self.walk_call_args(call);
                return;
            }
        }
        bump(&mut self.calls_unknown, evidence);
        bump(&mut self.disallowed, Truth::Maybe);
        self.check_target_escape(call);
        self.walk_expr(&call.func);
        self.walk_call_args(call);
    }

    fn finish(self, has_dt_param: bool) -> UpdaterBodyFact {
        let uses_dt = if has_dt_param {
            Truth::from(self.uses_dt)
        } else {
            Truth::No
        };
        let calls_unknown = self.calls_unknown;
        // "Any call to an unresolvable function → everything downstream
        // Maybe": an unknown callee may read frame-varying state or reach
        // the target through another alias.
        let degrade = |value: Truth| {
            if calls_unknown == Truth::No || value == Truth::Yes {
                value
            } else {
                Truth::Maybe
            }
        };
        let pure_affine_on_target = match self.disallowed {
            Truth::No => Truth::Yes,
            Truth::Maybe => Truth::Maybe,
            Truth::Yes => Truth::No,
        };
        UpdaterBodyFact {
            uses_dt,
            reads_frame_varying: degrade(self.reads_frame_varying),
            mutates_target: degrade(self.mutates_target),
            pure_affine_on_target,
            calls_unknown,
        }
    }
}

/// Walks the default expressions of a nested callable at the *current*
/// scope (defaults evaluate at definition time).
fn collect_defaults_walk(classifier: &mut BodyClassifier<'_, '_>, args: &ast::Arguments) {
    for arg in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        if let Some(default) = &arg.default {
            classifier.walk_expr(default);
        }
    }
}

/// Classifies one callback body.
fn classify_body(
    ctx: &Ctx<'_>,
    file: FileId,
    args: &ast::Arguments,
    scene_level: bool,
    body: &BodyRef<'_>,
) -> UpdaterBodyFact {
    let positional: Vec<String> = args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .map(|arg| arg.def.arg.to_string())
        .collect();
    let (target, dt) = if scene_level {
        // Scene updaters always receive exactly `(dt)` (DESIGN §3.3).
        (None, positional.first().cloned())
    } else {
        let dt = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .map(|arg| arg.def.arg.as_str())
            .find(|name| *name == "dt")
            .map(str::to_owned);
        (positional.first().cloned(), dt)
    };
    let has_dt_param = dt.is_some();
    let mut classifier = BodyClassifier {
        ctx,
        file,
        target,
        dt,
        nested: 0,
        conditional: 0,
        uses_dt: false,
        reads_frame_varying: Truth::No,
        mutates_target: Truth::No,
        disallowed: Truth::No,
        calls_unknown: Truth::No,
    };
    match body {
        BodyRef::Stmts(stmts) => classifier.walk_stmts(stmts),
        BodyRef::Expr(expr) => classifier.walk_expr(expr),
    }
    classifier.finish(has_dt_param)
}

/// Resolves a registered updater callback to its body and classifies it;
/// unresolvable callbacks stay all-`Maybe`.
fn classify_updater_callback(
    ctx: &Ctx<'_>,
    lambdas: &BTreeMap<AllocationSite, &ast::ExprLambda>,
    scene_level: bool,
    callback: &CallbackRef,
) -> UpdaterBodyFact {
    match callback {
        CallbackRef::Named(qualified) => match ctx.defs.defs.get(qualified) {
            Some(def) => classify_body(
                ctx,
                def.file,
                def.args,
                scene_level,
                &BodyRef::Stmts(def.body),
            ),
            None => UpdaterBodyFact::unknown(),
        },
        CallbackRef::Lambda(site) => match lambdas.get(site) {
            Some(lambda) => classify_body(
                ctx,
                site.file,
                &lambda.args,
                scene_level,
                &BodyRef::Expr(&lambda.body),
            ),
            None => UpdaterBodyFact::unknown(),
        },
        CallbackRef::Unknown => UpdaterBodyFact::unknown(),
    }
}

// ---------------------------------------------------------------------------
// Lambda return facts (MLC123).
// ---------------------------------------------------------------------------

/// The return fact of one lambda: a lambda always returns its body
/// expression, so bare-return and no-return paths are definite `No`s and
/// only the mobject-ness of the body is classified.
fn lambda_return_fact(ctx: &Ctx<'_>, file: FileId, lambda: &ast::ExprLambda) -> ReturnFact {
    let param = lambda
        .args
        .posonlyargs
        .iter()
        .chain(&lambda.args.args)
        .next()
        .map(|arg| arg.def.arg.to_string());
    ReturnFact {
        returns_mobject: classify_mobject_expr(ctx, file, param.as_deref(), &lambda.body),
        has_bare_return_path: Truth::No,
        has_no_return_path: Truth::No,
    }
}

/// Whether an expression's value is a mobject, assuming the first
/// parameter (`param`) is one (the `ApplyFunction` callback contract).
fn classify_mobject_expr(
    ctx: &Ctx<'_>,
    file: FileId,
    param: Option<&str>,
    expr: &ast::Expr,
) -> Truth {
    match expr {
        // Literals, displays, strings, and nested lambdas are definitely
        // not mobjects.
        ast::Expr::Constant(_)
        | ast::Expr::JoinedStr(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::List(_)
        | ast::Expr::Tuple(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::SetComp(_)
        | ast::Expr::DictComp(_)
        | ast::Expr::GeneratorExp(_)
        | ast::Expr::Lambda(_)
        | ast::Expr::Compare(_)
        | ast::Expr::BoolOp(_) => Truth::No,
        ast::Expr::Name(name) if param == Some(name.id.as_str()) => Truth::Yes,
        ast::Expr::IfExp(inner) => {
            let then_arm = classify_mobject_expr(ctx, file, param, &inner.body);
            let else_arm = classify_mobject_expr(ctx, file, param, &inner.orelse);
            match (then_arm, else_arm) {
                (Truth::Yes, Truth::Yes) => Truth::Yes,
                (Truth::No, _) | (_, Truth::No) => Truth::No,
                _ => Truth::Maybe,
            }
        }
        ast::Expr::Call(call) => {
            let Some(fact) = ctx.fact(file, call.range()) else {
                return Truth::Maybe;
            };
            if fact.candidates.is_empty() {
                return Truth::Maybe;
            }
            let mobject_candidate = |candidate: &str| {
                ctx.index.mobject_classes.contains(candidate)
                    || ctx
                        .knowledge
                        .and_then(|profile| profile.symbol(candidate))
                        .is_some_and(|entry| {
                            matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                        })
            };
            if fact
                .candidates
                .iter()
                .all(|candidate| mobject_candidate(candidate))
            {
                return Truth::Yes;
            }
            // A single project callable: delegate to its summary fact.
            if fact.candidates.len() == 1 {
                let candidate = fact.candidates.iter().next().expect("len checked");
                if let Some(summary) = ctx.summaries.get(candidate) {
                    return summary.return_fact.returns_mobject;
                }
            }
            let non_mobject_candidate = |candidate: &str| {
                ctx.index.animation_classes.contains(candidate)
                    || ctx
                        .knowledge
                        .and_then(|profile| profile.symbol(candidate))
                        .is_some_and(|entry| {
                            matches!(
                                entry.kind,
                                SymbolKind::Animation | SymbolKind::Scene | SymbolKind::Camera
                            )
                        })
            };
            if fact
                .candidates
                .iter()
                .all(|candidate| non_mobject_candidate(candidate))
            {
                Truth::No
            } else {
                Truth::Maybe
            }
        }
        _ => Truth::Maybe,
    }
}
