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
//! - cycle-guarded inlining of project helper calls — `self.<helper>()`
//!   resolved through the project MRO *and* module-level functions
//!   (same-module or imported; a scene argument flows as the live scene
//!   state in any parameter position) — per DESIGN §2.1 "Scene helper",
//!   §5.1 step 5: a play or wait inside a helper body materializes as a
//!   real [`PlayFact`] per call site, anchored at the play call in the
//!   helper file with the call chain in [`PlayFact::call_path`]. A call
//!   that closes a recursion cycle (direct or mutual) — or exceeds the
//!   work-bounding `INLINE_DEPTH_SAFETY_CAP` backstop — falls back to
//!   the §5.7 effect summary (recorded in
//!   [`LifecycleFacts::inline_fallbacks`]): membership effects survive,
//!   and frontier plays materialize as conservative summary-derived
//!   facts ([`SceneLifecycle::summary_derived_plays`]) instead of
//!   vanishing.
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
//!
//! # Module map
//!
//! The interpreter is split into cohesive submodules; every public item
//! stays reachable at `semantic::interpreter::*`:
//!
//! - `mod.rs` (this file): the public fact types, the [`DefMap`]
//!   definition index, and the [`analyze`] entry point.
//! - `exec`: the abstract value / execution-state lattice, the recording
//!   sink, per-block loop/branch context, and the CFG fixpoint walk
//!   (branch joins, loop widening).
//! - `dispatch`: the shared resolution context (`Ctx`) and call dispatch
//!   into curated knowledge effects (scene / mobject methods, project
//!   constructors).
//! - `play`: animation construction and the DESIGN §3.2 play pipeline
//!   (compile, auto-add, suspend, begin, finish, cleanup) plus the wait
//!   freeze verdict.
//! - `inline`: helper inlining with the summary fallback, summary
//!   application, and summary extraction (the summary-vs-inline duality
//!   contract lives there).
//! - `mro`: project MRO linearization, `super()` dispatch, and the
//!   `__init__ → setup → construct → tear_down` scene composition.
//! - `heap_ops`: primitive heap / scene-membership operations, mutator
//!   classification, z-index / path-geometry tracking, and `.animate`
//!   builder facts.
//! - `snapshots`: [`StateSnapshot`] scoping and the per-statement state
//!   queries ([`SceneLifecycle::state_at`], ownership intervals).
//! - `callbacks`: updater registration / removal, signature facts, the
//!   updater-body dataflow classifier, and callback return facts.

mod callbacks;
mod dispatch;
mod exec;
mod heap_ops;
mod inline;
mod mro;
mod play;
mod snapshots;

pub use snapshots::{OwnershipInterval, PathStateFact, StateSnapshot};

pub(crate) use inline::summarize_callable;

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde::{Deserialize, Serialize};

use crate::frontend::index::{ProjectIndex, QualifiedCallFacts};
use crate::knowledge::KnowledgeProfile;
use crate::semantic::events::Event;
use crate::semantic::heap::AbstractHeap;
use crate::semantic::state::{AnimationState, CallbackRef, PlayGroupId, UpdaterFact, WriteChannel};
use crate::semantic::summaries::SummaryTable;
use crate::semantic::values::{AllocationSite, Num, ObjectId, Presence, Truth};
use crate::source::{FileId, SourceManager};

use callbacks::{classify_updater_callback, lambda_return_fact};
use dispatch::Ctx;
use mro::run_scene;

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

/// Why one helper call was not inlined during a scene run (DESIGN §5.1
/// step 5): the effect summary was applied instead, so state effects
/// survived while plays behind the frontier were rehydrated only as
/// conservative summary-derived facts (see
/// [`SceneLifecycle::summary_derived_plays`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FallbackReason {
    /// The call closes a recursion cycle (direct or mutual): the callee
    /// is already on the inline stack.
    Recursion,
    /// The inline stack reached the work-bounding depth safety cap.
    DepthCap,
    /// The callee resolved to a project name whose definition is not
    /// available for inlining.
    Unresolvable,
}

/// One scene-run call site where helper inlining fell back to the effect
/// summary. Exposed for analysis-coverage reporting: each entry marks a
/// frontier behind which facts are summary-precision (membership effects
/// applied, plays only as maybe-certainty records), not inline-precision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FallbackFact {
    /// Source range of the call that fell back.
    pub site: AllocationSite,
    /// Qualified name of the callee whose body was not inlined.
    pub callee: String,
    /// Why inlining did not apply.
    pub reason: FallbackReason,
}

/// Whether a play fact came from `Scene.play` or `Scene.wait` / `pause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdaterTargetRead {
    /// Lexical object binding read by the callback (`driver` or
    /// `self.driver`). Rules map it to identity at the frame site.
    pub binding: String,
    /// Curated getter spelling (`get_center`, ...).
    pub method: String,
    /// Source range of the getter call.
    pub site: AllocationSite,
    /// Live-state channel consumed by the getter.
    pub channel: WriteChannel,
    /// Whether the read and the enclosing target write execute on every
    /// callback invocation.
    pub certainty: Truth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// Curated live-target write channels used by the callback. Absence is
    /// meaningful only when [`UpdaterBodyFact::channels_known`] is `Yes`.
    pub write_channels: BTreeSet<WriteChannel>,
    /// Direct external-object getters whose result is passed as an
    /// argument to a curated write on the updater target. This narrow
    /// dependency fact supports `MLR109` without treating unrelated reads
    /// in the same callback as data dependencies.
    pub target_reads: Vec<UpdaterTargetRead>,
    /// Whether `write_channels` completely classifies every target write.
    pub channels_known: Truth,
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
            write_channels: BTreeSet::new(),
            target_reads: Vec::new(),
            channels_known: Truth::Maybe,
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
///
/// Internally the interpreter keeps one fact per *execution context*
/// (builder site × helper call path, mirroring [`PlayFact::call_path`]),
/// so joins happen only within one execution. The per-site fact exposed
/// in [`SceneLifecycle::builders`] is the sound merge across executions:
/// a builder site reached through helper call sites with *different*
/// live targets reports `target: None` (no cross-execution identity
/// claim), disagreeing epoch pairs drop
/// [`AnimateBuilderFact::target_epoch_at_play`] (a creation epoch from
/// one execution is never compared against a play epoch from another),
/// and `Yes` verdicts survive only when a single execution carries them.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimateBuilderFact {
    /// Source range of the `.animate` attribute expression.
    pub site: AllocationSite,
    /// The live mobject the builder was created on; `None` when the
    /// builder site executed under several helper call sites with
    /// different targets (per-execution targets still reach rules
    /// through each play's [`PlayedAnimation::state`]).
    pub target: Option<ObjectId>,
    /// Methods chained on the builder, in order. Empty means a bare
    /// `mob.animate` (MLR102 candidate).
    pub methods: Vec<String>,
    /// Source span of each entry in [`AnimateBuilderFact::methods`],
    /// narrowed to the method attribute name. Parallel to `methods`.
    pub method_sites: Vec<AllocationSite>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The [`PlayFact::play_group`] ids of plays in [`SceneLifecycle::plays`]
    /// that were *rehydrated from effect summaries* instead of being traced
    /// through the DESIGN §3.2 pipeline (recursion / depth-cap inline
    /// fallbacks and summary-applied non-helper calls, DESIGN §5.7).
    ///
    /// For such a fact only the syntactic fields are populated:
    /// [`PlayFact::site`], [`PlayFact::kind`], [`PlayFact::duration`]
    /// (literal-derived or `Unknown`), per-argument
    /// [`PlayedAnimation::site`]s, [`PlayFact::has_stop_condition`],
    /// [`PlayFact::frozen_frame`], [`PlayFact::star_args`], and
    /// [`PlayFact::call_path`]. Everything caller-state-dependent is
    /// degraded and must never be treated as evidence:
    /// [`PlayFact::certainty`] is always [`Presence::Maybe`],
    /// [`PlayFact::repetitions`] is the open `[0, ∞)` interval (a
    /// recursive site may execute any number of times),
    /// [`PlayFact::dynamic_wait`] and
    /// [`PlayFact::always_update_mobjects`] are `Maybe`, and every
    /// [`PlayedAnimation`] carries no identity, no state, `convertible:
    /// Maybe`, and `channels_known: Maybe`. No `BeginPlay` / `FinishPlay`
    /// event exists for these groups.
    pub summary_derived_plays: BTreeSet<PlayGroupId>,
    /// Updater registrations in program order.
    pub updaters: Vec<UpdaterRegistration>,
    /// `remove_updater` calls in program order.
    pub updater_removals: Vec<UpdaterRemoval>,
    /// `.animate` builder facts keyed by builder site: the sound merge
    /// over every execution context of the site (see
    /// [`AnimateBuilderFact`] — internal joins stay within one
    /// execution).
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
/// - `MLC114`: builder method names/sites plus project/profile
///   `@override_animate` facts; only a positive override in a multi-method
///   chain fires.
/// - `MLC115`: [`SceneLifecycle::scene_removals`] (surviving parents) +
///   later `SceneAdd` events / [`SceneLifecycle::membership_at`].
/// - `MLC116`: normal-Transform source/target identities from
///   [`PlayedAnimation`] plus their pre-later-play membership snapshot.
/// - `MLC117`: [`AnimateBuilderFact::target_epoch_at_creation`] vs
///   [`AnimateBuilderFact::target_epoch_at_play`], and
///   [`AnimateBuilderFact::overwritten_by_later_builder`].
/// - `MLC118`: active updater registrations with complete callback write
///   channels intersected with a suspending Animation's channels.
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
/// - `MLR109`: direct [`UpdaterBodyFact::target_reads`] resolved through
///   [`StateSnapshot::object_bindings`] at a dynamic wait, intersected with
///   a later leaf root updater's complete write channels.
/// - `MLR122`: exact post-re-add scene-root order plus per-leaf `z_index`
///   facts under Cairo.
/// - `MLP218`: [`UpdaterRegistration::body`] with `uses_dt == No`,
///   `reads_frame_varying == No`, `calls_unknown == No`, and
///   `pure_affine_on_target == Yes`.
/// - `MLP221`: qualified-call literal facts
///   (`frontend::index::LiteralFact::Tuple` / `List` on `t_range` /
///   plot-step arguments).
/// - `MLP212`: pre-play singleton `FullScreenRectangle` style state
///   (`MobjectState::fill_opacity`) plus certain animation targets and
///   literal duration/profile pixels.
/// - `MLP213`: exact `Surface` allocation identity, literal constructor
///   resolution, certain target plays, and active Cairo profiles.
/// - `MLP223`: pre-play `stroke_opacity` / `stroke_width` / non-empty path
///   state plus the future event/play trace proving opacity immutability.
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
    /// Every scene-run call site where helper inlining fell back to the
    /// DESIGN §5.7 effect summary, deduplicated and sorted by
    /// (site, callee, reason) — the analysis-coverage frontier.
    pub inline_fallbacks: Vec<FallbackFact>,
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
    analyze_inner(
        sources,
        index,
        calls,
        knowledge,
        None,
        None,
        SummaryTable::default(),
    )
    .0
}

/// Incremental lifecycle entry point used after a whole-project cache miss.
/// Cached summaries seed unchanged graph components; only definitions and
/// Scene classes owned by `recompute_files` are interpreted again. The
/// frontend remains project-wide so resolution and Unknown propagation are
/// identical to a cold analysis.
#[must_use]
pub(crate) fn analyze_incremental(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    recompute_files: &BTreeSet<FileId>,
    summary_seed: SummaryTable,
) -> (LifecycleFacts, SummaryTable) {
    analyze_inner(
        sources,
        index,
        calls,
        knowledge,
        Some(recompute_files),
        Some(recompute_files),
        summary_seed,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the incremental entry point adds independent Scene and summary selections"
)]
fn analyze_inner(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    scene_files: Option<&BTreeSet<FileId>>,
    summary_files: Option<&BTreeSet<FileId>>,
    summary_seed: SummaryTable,
) -> (LifecycleFacts, SummaryTable) {
    let defs = DefMap::build(sources, index);
    let summaries = summary_files.map_or_else(
        || crate::semantic::summaries::build(sources, index, calls, knowledge, &defs),
        |files| {
            crate::semantic::summaries::build_incremental(
                sources,
                index,
                calls,
                knowledge,
                &defs,
                files,
                summary_seed,
            )
        },
    );
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

    let scene_ids: Vec<&String> = index
        .scene_classes
        .iter()
        .filter(|class_id| {
            index
                .classes
                .get(*class_id)
                .is_some_and(|record| scene_files.is_none_or(|files| files.contains(&record.file)))
        })
        .collect();
    let scene_runs: Vec<Option<_>> = scene_ids
        .par_iter()
        .map(|class_id| {
            index
                .classes
                .get(*class_id)
                .map(|record| run_scene(&ctx, record))
        })
        .collect();
    let scene_runs: Vec<_> = scene_runs.into_iter().flatten().collect();
    let mut inline_fallbacks: Vec<FallbackFact> = scene_runs
        .iter()
        .flat_map(|run| run.inline_fallbacks.iter().cloned())
        .collect();
    let mut scenes: Vec<SceneLifecycle> = scene_runs.into_iter().map(|run| run.lifecycle).collect();

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

    // Inline-fallback frontier (coverage reporting): deduplicated across
    // scenes sharing a helper chain, deterministically ordered.
    inline_fallbacks.sort();
    inline_fallbacks.dedup();

    (
        LifecycleFacts {
            scenes,
            callback_returns,
            inline_fallbacks,
        },
        summaries,
    )
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
