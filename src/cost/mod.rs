//! Symbolic cost model (DESIGN §4): dimensions, hot contexts, estimation.
//!
//! - [`model`]: the §4.1 dimension vocabulary and per-operation cost facts;
//! - [`contexts`]: §4.2 hot-context identification and transitive
//!   propagation over the qualified-call facts;
//! - [`estimator`]: §4.3-§4.4 frame estimation, severity scoring, and the
//!   §6.3 evidence JSON builder.
//!
//! [`CostFacts`] aggregates everything for the rule layer. The query
//! surface the `MLP2xx` rules consume:
//!
//! - [`CostFacts::is_call_in_hot_context`] / [`CostFacts::hot_contexts_for`]
//!   — whether a call fact is reachable from a per-frame entry point, with
//!   provenance;
//! - [`CostFacts::call_execution`] / [`CostFacts::proven_frames_for_call`]
//!   — the [`liveness`] layer: the plays where the enclosing callback
//!   **provably executes** per frame (registration active, host in the
//!   scene family, not suspended by the play's animations, dynamic wait),
//!   and the frame estimate summed over those plays only;
//! - [`CostFacts::frames_for_play`] — the symbolic / interval frame count of
//!   a recognized `play` / `wait` call;
//! - [`CostFacts::evidence_for`] — the DESIGN §6.3 evidence JSON of a call;
//! - [`CostFacts::constructions_in_hot_contexts`] — hot mobject
//!   constructions (`MLP201` / `MLP226` consumers);
//! - [`CostFacts::scene_graph_mutations_in_hot_contexts`] — hot Scene
//!   membership mutations (`MLP204`);
//! - [`CostFacts::frame_varying_resource_keys`] — Text / TeX constructions
//!   whose cache key varies per frame, `K_resource ≈ F` (`MLP226`);
//! - [`CostFacts::family_size_of_target`] / [`CostFacts::points_of_target`]
//!   / [`CostFacts::curves_of_target`] — per-call `N_family` / `P` / `C`
//!   resolved from the lifecycle heap (`MLP202` / `MLP203` / `MLP207` /
//!   `MLP208` / `MLP216` sizing);
//! - [`CostFacts::curve_delta_for_transform`] — `|C(source) − C(target)|`
//!   of a played Transform when both are known (`MLP207` / `MLP208`);
//! - [`CostFacts::per_frame_allocation_bytes`] — statically proven bytes
//!   of a recognized ndarray construction (`MLP211`);
//! - [`CostFacts::copy_align_targets_in_hot_contexts`] /
//!   [`CostFacts::family_walk_queries_in_hot_contexts`] — hot
//!   `copy` / `become` / `align` operations and family-walk queries with
//!   the receiver's resolved sizes (`MLP202` / `MLP203`);
//! - [`thresholds`] — the DESIGN §7.3 emission gates as citable named
//!   constants ([`thresholds::Threshold::confirmed_by`] never passes an
//!   unknown).
//!
//! Size facts require the lifecycle interpreter output: build with
//! [`CostFacts::compute_with_lifecycle`]. The plain [`CostFacts::compute`]
//! stays valid and simply leaves every size query `Unknown`.
//!
//! Invariants (DESIGN §15): unknown values never collapse to `1`, no
//! fabricated frame counts, and every hotness claim is gated on curated
//! knowledge — absent facts mean silence, not a diagnostic.

pub mod contexts;
pub mod estimator;
pub mod geometry;
pub mod liveness;
pub mod model;
pub mod sizes;
pub mod thresholds;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::config::model::RenderProfile;
use crate::frontend::index::{ArgShape, CallArgument, ProjectIndex, QualifiedCallFacts};
use crate::knowledge::{KnowledgeProfile, SceneMembershipEffect, SymbolKind};
use crate::semantic::interpreter::{LifecycleFacts, PlayFact, PlayedAnimation, UpdaterHost};
use crate::semantic::state::CallbackRef;
use crate::semantic::values::{AllocationSite, Num, ObjectId};
use crate::source::{FileId, SourceManager};

use contexts::{HotContext, HotContextFacts, HotEntryKind, hot_contexts, resolve_candidate};
use estimator::{Evidence, frames_across_profiles, play_run_time, symbolic_frames, wait_duration};
use liveness::{EntryKey, EntryLiveness, ExecutionCertainty, ExecutionPlay};
use model::{CostDimension, CostFact, InvocationContext, Multiplicity, OperationKind};
use sizes::{ObjectSizes, SizeFacts};

/// Canonical ids whose calls run the play lifecycle with a duration.
const SCENE_PLAY: &str = "manim.scene.scene.Scene.play";
const SCENE_WAIT: &str = "manim.scene.scene.Scene.wait";
const WAIT_ANIMATION: &str = "manim.animation.animation.Wait";

/// Canonical ids of constructors whose cache key is a Text / TeX / SVG /
/// Image resource key (`K_resource` tracking, DESIGN §4.1). Only ids also
/// present in the loaded knowledge profile are honored.
const RESOURCE_CONSTRUCTORS: [&str; 7] = [
    "manim.mobject.svg.svg_mobject.SVGMobject",
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
    "manim.mobject.text.text_mobject.MarkupText",
    "manim.mobject.text.text_mobject.Text",
    "manim.mobject.types.image_mobject.ImageMobject",
];

/// Canonical ids whose construction launches an external compiler on a
/// cache miss (`OperationKind::ExternalProcess`, DESIGN §4.2).
const TEX_CONSTRUCTORS: [&str; 3] = [
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
];

/// Qualified ids of ndarray constructors whose byte size is statically
/// provable from a literal shape (`MLP211`). `NumPy`'s default dtype for
/// `zeros` / `ones` / `empty` is `float64` (8 bytes per element).
///
/// A literal integer length and literal tuple / list shapes whose
/// dimensions are all integer literals (`np.zeros((w, h))`) are analyzable;
/// anything else stays `Unknown` rather than guessed.
const NDARRAY_CONSTRUCTORS: [&str; 4] = ["numpy.empty", "numpy.full", "numpy.ones", "numpy.zeros"];

/// Mobject method names that copy the receiver (`mobject.py`
/// `Mobject.copy` :1178, `Mobject.generate_target` — both deep-copy the
/// whole family, DESIGN §4.3 Animation begin).
const COPY_METHOD_NAMES: [&str; 2] = ["copy", "generate_target"];

/// Mobject method names that rewrite the receiver's points, style, and
/// submobjects in place (`mobject.py` `Mobject.become`, `Mobject.restore`).
const BECOME_METHOD_NAMES: [&str; 2] = ["become", "restore"];

/// Mobject method names that align point / curve topology (`mobject.py`
/// `Mobject.align_data`; `vectorized_mobject.py` `VMobject.align_points`,
/// `VMobject.insert_n_curves` — DESIGN §14 source map).
const ALIGN_METHOD_NAMES: [&str; 3] = ["align_data", "align_points", "insert_n_curves"];

/// Mobject family-walk / geometry-query method names (`MLP203` /
/// `MLP224` targets): `mobject.py` `get_family`,
/// `family_members_with_points`, `get_all_points`,
/// `point_from_proportion`, `proportion_from_point`;
/// `vectorized_mobject.py` `get_arc_length`.
const FAMILY_WALK_METHOD_NAMES: [&str; 6] = [
    "family_members_with_points",
    "get_all_points",
    "get_arc_length",
    "get_family",
    "point_from_proportion",
    "proportion_from_point",
];

/// What a hot receiver-sized operation does (`MLP202` / `MLP203`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotTargetKind {
    /// `copy()` / `generate_target()` — whole-family deep copy.
    Copy,
    /// `become()` / `restore()` — in-place family rewrite.
    Become,
    /// `align_data()` / `align_points()` / `insert_n_curves()`.
    Align,
    /// `get_family()` and other family-walk / geometry queries.
    FamilyWalk,
}

/// A hot `copy` / `become` / `align` / family-walk call whose receiver
/// object (the updater host) and its sizes were resolved.
///
/// Produced only for calls **directly** inside a `Mobject.add_updater`
/// lambda whose receiver is provably the callback parameter (the updater
/// host, DESIGN §3.3) — resolution never guesses.
#[derive(Debug, Clone, PartialEq)]
pub struct HotTargetSizeFact {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Method name observed on the receiver (e.g. `copy`).
    pub method: String,
    /// Operation classification.
    pub kind: HotTargetKind,
    /// Qualified scene class the updater host belongs to.
    pub scene: String,
    /// The receiver (updater host) object.
    pub target: ObjectId,
    /// The receiver's resolved sizes ([`Num::Unknown`] fields when not
    /// provable — a threshold predicate then never passes).
    pub sizes: ObjectSizes,
}

/// A mobject construction reachable from a per-frame entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct HotConstruction {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the constructed class.
    pub symbol: String,
    /// The per-operation cost fact of the construction.
    pub cost: CostFact,
}

/// A Scene-membership mutation reachable from a per-frame entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct HotSceneMutation {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the mutating Scene API method.
    pub symbol: String,
    /// The curated membership effect.
    pub effect: SceneMembershipEffect,
    /// Whether an argument is itself a fresh mobject construction — the
    /// growth pattern `MLP204` treats as `O(F)` family growth. `false`
    /// means "not proven", never "proven absent".
    pub adds_fresh_allocation: bool,
}

/// A hot Text / TeX construction whose cache key provably varies per frame
/// (an f-string key), so `K_resource ≈ F` (DESIGN §4.2, `MLP226`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceKeyFact {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the constructed resource class.
    pub symbol: String,
    /// Estimated distinct-key count (the symbolic `frames` quantity until a
    /// literal duration binds it).
    pub keys: Num,
}

/// Aggregated execution liveness of one call fact (DESIGN §3.2/§3.3):
/// the union over every hot context the call runs under.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallExecution<'facts> {
    /// Plays where the enclosing callback provably executes per frame.
    pub proven: Vec<&'facts ExecutionPlay>,
    /// Plays where it may execute (not provable either way).
    pub maybe: Vec<&'facts ExecutionPlay>,
    /// At least one entry root could not be tied to the lifecycle facts:
    /// execution is possible beyond the listed plays, and "provably
    /// never" cannot be claimed.
    pub unresolved: bool,
}

impl CallExecution<'_> {
    /// Whether at least one play provably executes the callback.
    #[must_use]
    pub fn has_proven(&self) -> bool {
        !self.proven.is_empty()
    }

    /// Whether execution is at least possible (maybe plays or an
    /// unresolved entry). `false` together with [`Self::has_proven`]
    /// `false` means the callback provably never runs per frame.
    #[must_use]
    pub fn possibly_executes(&self) -> bool {
        self.unresolved || !self.maybe.is_empty()
    }
}

/// Aggregated symbolic cost facts of the analyzed project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostFacts {
    /// Hot-context facts (per call site and per project function).
    pub hot: HotContextFacts,
    /// Per-operation cost facts of hot calls, keyed by call-fact index.
    pub call_costs: BTreeMap<usize, Vec<CostFact>>,
    /// Frame-count estimates of recognized `play` / `wait` calls, keyed by
    /// call-fact index: an interval when a literal duration is known, the
    /// symbolic `frames` quantity otherwise.
    pub frames_by_call: BTreeMap<usize, Num>,
    /// Hot mobject constructions, ascending by call index.
    pub hot_constructions: Vec<HotConstruction>,
    /// Hot Scene-membership mutations, ascending by call index.
    pub hot_scene_mutations: Vec<HotSceneMutation>,
    /// Frame-varying resource keys, ascending by call index.
    pub frame_varying_resources: Vec<ResourceKeyFact>,
    /// Render profiles analyzed in this run (frame rates, renderers,
    /// resolutions for the pixel helpers).
    pub profiles: Vec<RenderProfile>,
    /// Per-object size facts bridged from the lifecycle interpreter
    /// (empty unless built with [`CostFacts::compute_with_lifecycle`]).
    pub sizes: SizeFacts,
    /// Hot `copy` / `become` / `align` / family-walk receivers with
    /// resolved sizes, ascending by call index.
    pub hot_target_sizes: Vec<HotTargetSizeFact>,
    /// Statically proven per-invocation allocation bytes of recognized
    /// ndarray constructions, keyed by call-fact index.
    pub allocation_bytes: BTreeMap<usize, Num>,
    /// Per-entry execution liveness resolved against the lifecycle facts
    /// (DESIGN §3.2 updater suspension, §3.3 wait dynamics): which plays
    /// provably (or possibly) run each hot callback per frame.
    pub liveness: BTreeMap<EntryKey, EntryLiveness>,
    /// Source site of every call fact, aligned by call index (query
    /// anchor for the size facts).
    call_sites: Vec<AllocationSite>,
}

impl CostFacts {
    /// Builds the cost facts for the whole project.
    ///
    /// Without a knowledge profile every collection stays empty — no
    /// hotness or cost claim is ever made from name strings alone.
    /// Size queries all answer `Unknown` on facts built this way; use
    /// [`CostFacts::compute_with_lifecycle`] to bridge the lifecycle
    /// cardinalities in.
    #[must_use]
    pub fn compute(
        sources: &SourceManager,
        index: &ProjectIndex,
        calls: &QualifiedCallFacts,
        knowledge: Option<&KnowledgeProfile>,
        profiles: &[RenderProfile],
    ) -> Self {
        Self::compute_with_lifecycle(
            sources,
            index,
            calls,
            knowledge,
            profiles,
            &LifecycleFacts::default(),
        )
    }

    /// Builds the cost facts including the per-object size facts bridged
    /// from the lifecycle interpreter output (DESIGN §4.1 `N_family` /
    /// `P` / `C` dimensions).
    #[must_use]
    pub fn compute_with_lifecycle(
        sources: &SourceManager,
        index: &ProjectIndex,
        calls: &QualifiedCallFacts,
        knowledge: Option<&KnowledgeProfile>,
        profiles: &[RenderProfile],
        lifecycle: &LifecycleFacts,
    ) -> Self {
        let mut facts = Self {
            hot: hot_contexts(sources, index, calls, knowledge),
            profiles: profiles.to_vec(),
            call_sites: calls
                .calls
                .iter()
                .map(|call| AllocationSite::new(call.file, call.call_range))
                .collect(),
            ..Self::default()
        };
        facts.collect_allocation_bytes(calls);
        facts.liveness = liveness::resolve_liveness(&facts.hot, lifecycle);
        let Some(knowledge) = knowledge else {
            return facts;
        };
        facts.collect_play_frames(calls, knowledge);
        facts.collect_hot_call_costs(sources, calls, knowledge);
        facts.sizes = SizeFacts::resolve(lifecycle, calls, Some(knowledge));
        facts.collect_hot_target_sizes(sources, calls, knowledge, lifecycle);
        facts
    }

    /// The first (deterministically ordered) hot context of a call fact, or
    /// `None` when the call is not provably hot.
    #[must_use]
    pub fn is_call_in_hot_context(&self, call_index: usize) -> Option<&HotContext> {
        self.hot.contexts_for_call(call_index).first()
    }

    /// The resolved liveness of one hot context's entry root, if any.
    #[must_use]
    pub fn context_liveness(&self, context: &HotContext) -> Option<&EntryLiveness> {
        self.liveness.get(&liveness::entry_key(context)?)
    }

    /// Aggregated execution liveness of a call fact over every hot
    /// context it runs under (DESIGN §3.2/§3.3): the plays where the
    /// enclosing callback provably or possibly executes per frame.
    #[must_use]
    pub fn call_execution(&self, call_index: usize) -> CallExecution<'_> {
        let mut keys: BTreeSet<EntryKey> = BTreeSet::new();
        let mut unresolved = false;
        for context in self.hot.contexts_for_call(call_index) {
            match liveness::entry_key(context) {
                Some(key) => {
                    keys.insert(key);
                }
                None => unresolved = true,
            }
        }
        let mut proven: Vec<&ExecutionPlay> = Vec::new();
        let mut maybe: Vec<&ExecutionPlay> = Vec::new();
        let mut seen: BTreeSet<(&str, AllocationSite)> = BTreeSet::new();
        for key in &keys {
            let Some(entry) = self.liveness.get(key) else {
                unresolved = true;
                continue;
            };
            if !entry.resolved {
                unresolved = true;
                continue;
            }
            for play in &entry.plays {
                if !seen.insert((play.scene.as_str(), play.site)) {
                    continue;
                }
                match play.certainty {
                    ExecutionCertainty::Proven => proven.push(play),
                    ExecutionCertainty::Maybe => maybe.push(play),
                }
            }
        }
        CallExecution {
            proven,
            maybe,
            unresolved,
        }
    }

    /// Frame-count estimate of a hot call's per-frame executions, summed
    /// **only** over the plays where the callback provably executes;
    /// maybe plays (and unresolved entries) widen the upper bound only.
    /// `None` when no play provably executes the callback or no real
    /// bound survives — never a fabricated number (DESIGN §15
    /// invariant 9).
    #[must_use]
    pub fn proven_frames_for_call(&self, call_index: usize) -> Option<Num> {
        let execution = self.call_execution(call_index);
        if execution.proven.is_empty() || self.profiles.is_empty() {
            return None;
        }
        // Per-scene duration sums (a rendered project runs each scene's
        // plays in sequence), joined into one hull across scenes.
        let mut per_scene: BTreeMap<&str, (f64, Option<f64>)> = BTreeMap::new();
        for play in &execution.proven {
            let entry = per_scene
                .entry(play.scene.as_str())
                .or_insert((0.0, Some(0.0)));
            if play.full_duration_proven {
                entry.0 += play.duration.lower_bound().unwrap_or(0.0);
            }
            entry.1 = match (entry.1, play.duration.upper_bound()) {
                (Some(current), Some(upper)) => Some(current + upper),
                _ => None,
            };
        }
        for play in &execution.maybe {
            let entry = per_scene
                .entry(play.scene.as_str())
                .or_insert((0.0, Some(0.0)));
            entry.1 = match (entry.1, play.duration.upper_bound()) {
                (Some(current), Some(upper)) => Some(current + upper),
                _ => None,
            };
        }
        let mut joined: Option<Num> = None;
        for (lower, upper) in per_scene.values() {
            let duration = Num::Interval {
                lo: Some(*lower),
                hi: if execution.unresolved { None } else { *upper },
            };
            let frames = frames_across_profiles(&duration, &self.profiles);
            joined = Some(match joined {
                None => frames,
                Some(current) => current.join(&frames),
            });
        }
        match joined {
            Some(frames @ (Num::Exact(_) | Num::Interval { .. })) => {
                // A quantity with neither a positive lower bound nor an
                // upper bound proves nothing; report no number at all.
                let informative = frames.lower_bound().is_some_and(|lower| lower > 0.0)
                    || frames.upper_bound().is_some();
                informative.then_some(frames)
            }
            _ => None,
        }
    }

    /// Every hot context of a call fact (empty when cold / unknown).
    #[must_use]
    pub fn hot_contexts_for(&self, call_index: usize) -> &[HotContext] {
        self.hot.contexts_for_call(call_index)
    }

    /// Frame-count estimate of a recognized `play` / `wait` call fact.
    ///
    /// An interval when a literal duration bound it, the symbolic `frames`
    /// quantity when the duration is unknown, and [`Num::Unknown`] when the
    /// call is not a recognized play at all.
    #[must_use]
    pub fn frames_for_play(&self, call_index: usize) -> Num {
        self.frames_by_call
            .get(&call_index)
            .cloned()
            .unwrap_or(Num::Unknown)
    }

    /// The DESIGN §6.3 evidence JSON for a call fact. Unknown frame
    /// counts are `null` and unknown sizes are omitted — never fabricated
    /// numbers.
    #[must_use]
    pub fn evidence_for(&self, call_index: usize) -> Value {
        let hot = self.is_call_in_hot_context(call_index);
        let frames = self
            .frames_by_call
            .get(&call_index)
            .cloned()
            .or_else(|| hot.map(|context| context.multiplicity.frames.clone()));
        let sizes = self.sizes_of_target(call_index);
        let evidence = Evidence {
            invocation_context: hot.map(|context| context.context),
            multiplicity: hot
                .map(|context| estimator::multiplicity_factor_names(&context.multiplicity))
                .unwrap_or_default(),
            frames,
            family_size: Some(sizes.family),
            points: Some(sizes.points),
            renderers: estimator::renderers_of(&self.profiles),
            state_path: hot.map(HotContext::state_path).unwrap_or_default(),
        };
        evidence.to_json()
    }

    /// Resolved sizes of the object a call fact is about: the object
    /// allocated at the call site (constructions, copies) or the
    /// mutation receiver recorded there, joined over every scene that
    /// analyzed the site. All-`Unknown` when nothing resolves.
    #[must_use]
    pub fn sizes_of_target(&self, call_index: usize) -> ObjectSizes {
        let Some(site) = self.call_sites.get(call_index).copied() else {
            return ObjectSizes::unknown();
        };
        let mut joined: Option<ObjectSizes> = None;
        for scene in self.sizes.scenes.values() {
            for target in scene.targets_at(site) {
                let sizes = scene.sizes_at(&target, Some(site));
                joined = Some(match joined {
                    None => sizes,
                    Some(current) => current.join(&sizes),
                });
            }
        }
        joined.unwrap_or_else(ObjectSizes::unknown)
    }

    /// `N_family` of the call's target object ([`Num::Unknown`] when not
    /// provable). `MLP202` / `MLP203` / `MLP208` sizing query.
    #[must_use]
    pub fn family_size_of_target(&self, call_index: usize) -> Num {
        self.sizes_of_target(call_index).family
    }

    /// Family-total point count `P` of the call's target object.
    #[must_use]
    pub fn points_of_target(&self, call_index: usize) -> Num {
        self.sizes_of_target(call_index).points
    }

    /// Family-total cubic curve count `C` of the call's target object.
    #[must_use]
    pub fn curves_of_target(&self, call_index: usize) -> Num {
        self.sizes_of_target(call_index).curves
    }

    /// `|C(source) − C(target)|` of one played Transform-family animation
    /// (`MLP207` / `MLP208`): the estimated curve insertion
    /// `Animation.begin` pays for a topology mismatch (DESIGN §4.3).
    ///
    /// Both curve counts are measured **at the play site** (pre-play
    /// geometry). `Unknown` unless the animation has exactly one live
    /// source, a tracked replacement target, and both counts carry real
    /// bounds.
    #[must_use]
    pub fn curve_delta_for_transform(
        &self,
        scene: &str,
        play: &PlayFact,
        animation: &PlayedAnimation,
    ) -> Num {
        let Some(scene_sizes) = self.sizes.scene(scene) else {
            return Num::Unknown;
        };
        let Some(state) = &animation.state else {
            return Num::Unknown;
        };
        let Some(target) = &animation.replacement_target else {
            return Num::Unknown;
        };
        if state.targets.len() != 1 {
            return Num::Unknown;
        }
        let source = state.targets.first().expect("length checked");
        let at = Some(play.site);
        let source_curves = scene_sizes.sizes_at(source, at).curves;
        let target_curves = scene_sizes.sizes_at(target, at).curves;
        abs_difference(&source_curves, &target_curves)
    }

    /// Statically proven bytes one invocation of a recognized ndarray
    /// construction allocates (`MLP211`): literal length × element size.
    /// [`Num::Unknown`] for everything else — tuple shapes, unknown
    /// dtypes, and non-literal lengths are never guessed.
    #[must_use]
    pub fn per_frame_allocation_bytes(&self, call_index: usize) -> Num {
        self.allocation_bytes
            .get(&call_index)
            .cloned()
            .unwrap_or(Num::Unknown)
    }

    /// Hot `copy` / `become` / `align` operations with resolved receiver
    /// sizes (`MLP202` consumer).
    pub fn copy_align_targets_in_hot_contexts(&self) -> impl Iterator<Item = &HotTargetSizeFact> {
        self.hot_target_sizes.iter().filter(|fact| {
            matches!(
                fact.kind,
                HotTargetKind::Copy | HotTargetKind::Become | HotTargetKind::Align
            )
        })
    }

    /// Hot family-walk / geometry queries with resolved receiver sizes
    /// (`MLP203` consumer).
    pub fn family_walk_queries_in_hot_contexts(&self) -> impl Iterator<Item = &HotTargetSizeFact> {
        self.hot_target_sizes
            .iter()
            .filter(|fact| fact.kind == HotTargetKind::FamilyWalk)
    }

    /// Hot mobject constructions (`MLP201` / `MLP226` consumers).
    pub fn constructions_in_hot_contexts(&self) -> impl Iterator<Item = &HotConstruction> {
        self.hot_constructions.iter()
    }

    /// Hot Scene-membership mutations (`MLP204` consumer).
    pub fn scene_graph_mutations_in_hot_contexts(&self) -> impl Iterator<Item = &HotSceneMutation> {
        self.hot_scene_mutations.iter()
    }

    /// Frame-varying Text / TeX resource keys (`MLP226` consumer).
    pub fn frame_varying_resource_keys(&self) -> impl Iterator<Item = &ResourceKeyFact> {
        self.frame_varying_resources.iter()
    }

    /// Records frame estimates for every recognized play / wait call.
    fn collect_play_frames(&mut self, calls: &QualifiedCallFacts, knowledge: &KnowledgeProfile) {
        for (call_index, call) in calls.calls.iter().enumerate() {
            let mut duration: Option<Num> = None;
            for candidate in &call.candidates {
                let Some((canonical, _)) = resolve_candidate(knowledge, candidate) else {
                    continue;
                };
                let candidate_duration = match canonical.as_str() {
                    SCENE_PLAY => play_run_time(call, calls),
                    SCENE_WAIT => wait_duration(call, &["duration"]),
                    WAIT_ANIMATION => wait_duration(call, &["run_time"]),
                    _ => continue,
                };
                duration = Some(match duration {
                    None => candidate_duration,
                    // Ambiguous candidates: hull over both interpretations.
                    Some(current) => current.join(&candidate_duration),
                });
            }
            if let Some(duration) = duration {
                self.frames_by_call.insert(
                    call_index,
                    frames_across_profiles(&duration, &self.profiles),
                );
            }
        }
    }

    /// Derives per-operation cost facts for every hot call.
    fn collect_hot_call_costs(
        &mut self,
        sources: &SourceManager,
        calls: &QualifiedCallFacts,
        knowledge: &KnowledgeProfile,
    ) {
        let hot_indices: Vec<usize> = self.hot.call_contexts.keys().copied().collect();
        for call_index in hot_indices {
            let call = &calls.calls[call_index];
            let multiplicity = self.joined_multiplicity(call_index);
            let context = self
                .is_call_in_hot_context(call_index)
                .map_or(InvocationContext::Unknown, |hot| hot.context);
            for candidate in &call.candidates {
                let Some((canonical, entry)) = resolve_candidate(knowledge, candidate) else {
                    continue;
                };
                match entry.kind {
                    SymbolKind::Mobject | SymbolKind::Vmobject => {
                        self.record_hot_construction(
                            sources,
                            calls,
                            call_index,
                            &canonical,
                            context,
                            &multiplicity,
                        );
                    }
                    SymbolKind::Method => {
                        let mutation = entry
                            .effects
                            .as_ref()
                            .and_then(|effects| effects.scene_membership)
                            .filter(|effect| is_graph_mutation(*effect));
                        if let Some(effect) = mutation {
                            self.record_hot_mutation(
                                calls,
                                knowledge,
                                call_index,
                                &canonical,
                                effect,
                                context,
                                &multiplicity,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Records one hot mobject construction (plus TeX external-process and
    /// frame-varying key facts where they apply).
    fn record_hot_construction(
        &mut self,
        sources: &SourceManager,
        calls: &QualifiedCallFacts,
        call_index: usize,
        canonical: &str,
        context: InvocationContext,
        multiplicity: &Multiplicity,
    ) {
        if self
            .hot_constructions
            .iter()
            .any(|existing| existing.call_index == call_index && existing.symbol == canonical)
        {
            return;
        }
        let call = &calls.calls[call_index];
        let frame_varying_key = RESOURCE_CONSTRUCTORS.contains(&canonical)
            && key_argument_is_fstring(sources, call.file, call.positional(0));
        let cost = CostFact::new(OperationKind::Construction, context)
            .with_multiplicity(multiplicity.clone());
        self.push_cost(call_index, cost.clone());
        self.hot_constructions.push(HotConstruction {
            call_index,
            symbol: canonical.to_owned(),
            cost,
        });
        if TEX_CONSTRUCTORS.contains(&canonical) {
            // A cache miss launches the TeX compiler; how many distinct
            // keys occur is unknown unless the key provably varies per
            // frame. Unknown stays unknown — never assumed to be 1.
            let keys = if frame_varying_key {
                symbolic_frames()
            } else {
                Num::Unknown
            };
            let mut external_multiplicity = multiplicity.clone();
            external_multiplicity.distinct_resource_keys = keys.clone();
            let external = CostFact::new(OperationKind::ExternalProcess, context)
                .with_dimension(CostDimension::ResourceKeys, keys)
                .with_multiplicity(external_multiplicity);
            self.push_cost(call_index, external);
        }
        if frame_varying_key {
            self.frame_varying_resources.push(ResourceKeyFact {
                call_index,
                symbol: canonical.to_owned(),
                keys: symbolic_frames(),
            });
        }
    }

    /// Records one hot Scene-membership mutation.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal builder step; grouping into a struct adds no clarity"
    )]
    fn record_hot_mutation(
        &mut self,
        calls: &QualifiedCallFacts,
        knowledge: &KnowledgeProfile,
        call_index: usize,
        canonical: &str,
        effect: SceneMembershipEffect,
        context: InvocationContext,
        multiplicity: &Multiplicity,
    ) {
        if self
            .hot_scene_mutations
            .iter()
            .any(|existing| existing.call_index == call_index && existing.symbol == canonical)
        {
            return;
        }
        let call = &calls.calls[call_index];
        let adds_fresh_allocation = call.arguments.iter().any(|argument| {
            let ArgShape::Call(child_index) = argument.shape else {
                return false;
            };
            calls.calls.get(child_index).is_some_and(|child| {
                child.candidates.iter().any(|candidate| {
                    resolve_candidate(knowledge, candidate).is_some_and(|(_, entry)| {
                        matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                    })
                })
            })
        });
        let cost = CostFact::new(
            OperationKind::Other("scene-graph-mutation".to_owned()),
            context,
        )
        .with_multiplicity(multiplicity.clone());
        self.push_cost(call_index, cost);
        self.hot_scene_mutations.push(HotSceneMutation {
            call_index,
            symbol: canonical.to_owned(),
            effect,
            adds_fresh_allocation,
        });
    }

    /// Records statically provable ndarray allocation sizes (`MLP211`).
    ///
    /// Recognition is by qualified id ([`NDARRAY_CONSTRUCTORS`]) with a
    /// literal integer length; the element size comes from the dtype
    /// argument or `NumPy`'s `float64` default. Anything else stays absent
    /// (queries answer `Unknown`).
    fn collect_allocation_bytes(&mut self, calls: &QualifiedCallFacts) {
        for (call_index, call) in calls.calls.iter().enumerate() {
            if !call
                .candidates
                .iter()
                .any(|candidate| NDARRAY_CONSTRUCTORS.contains(&candidate.as_str()))
            {
                continue;
            }
            let bytes = ndarray_allocation_bytes(call);
            if !bytes.is_unknown() {
                self.allocation_bytes.insert(call_index, bytes);
            }
        }
    }

    /// Resolves hot `copy` / `become` / `align` / family-walk calls whose
    /// receiver is provably the updater-host parameter of a
    /// `Mobject.add_updater` lambda, and attaches the host's sizes.
    ///
    /// The frame callback body is never executed by the lifecycle
    /// interpreter (DESIGN §15 invariant 7), so the receiver cannot come
    /// from heap events; instead it is derived from the updater contract:
    /// Manim invokes the callback with the host mobject as its first
    /// argument (DESIGN §3.3). A lambda body cannot rebind the parameter
    /// (no statements); a walrus assignment anywhere in the lambda
    /// disqualifies the whole region.
    fn collect_hot_target_sizes(
        &mut self,
        sources: &SourceManager,
        calls: &QualifiedCallFacts,
        knowledge: &KnowledgeProfile,
        lifecycle: &LifecycleFacts,
    ) {
        let hot_indices: Vec<usize> = self.hot.call_contexts.keys().copied().collect();
        for call_index in hot_indices {
            let call = &calls.calls[call_index];
            for context in self.hot.contexts_for_call(call_index) {
                // Only calls directly inside a mobject-updater lambda: a
                // longer chain means the call sits in a helper whose
                // receiver is not the lambda parameter.
                if context.entry != HotEntryKind::MobjectUpdater || context.chain.len() != 1 {
                    continue;
                }
                let step = &context.chain[0];
                if step.symbol.is_some() {
                    // Named callback function: parameter rebinding inside
                    // the body cannot be ruled out without dataflow.
                    continue;
                }
                let lambda_site = AllocationSite::new(step.file, step.range);
                let lambda_text = sources.file(step.file).slice(step.range);
                if lambda_text.contains(":=") {
                    continue;
                }
                let facts: Vec<HotTargetSizeFact> = lifecycle
                    .scenes
                    .iter()
                    .filter_map(|scene| {
                        hot_target_fact(
                            sources,
                            calls,
                            knowledge,
                            &self.sizes,
                            scene,
                            lambda_site,
                            call_index,
                            call,
                        )
                    })
                    .collect();
                for fact in facts {
                    if !self.hot_target_sizes.contains(&fact) {
                        self.hot_target_sizes.push(fact);
                    }
                }
            }
        }
        self.hot_target_sizes.sort_by(|a, b| {
            (a.call_index, &a.scene, &a.method, &a.target).cmp(&(
                b.call_index,
                &b.scene,
                &b.method,
                &b.target,
            ))
        });
    }

    /// Factor-wise join of every hot context multiplicity of a call.
    fn joined_multiplicity(&self, call_index: usize) -> Multiplicity {
        let contexts = self.hot.contexts_for_call(call_index);
        let mut joined: Option<Multiplicity> = None;
        for context in contexts {
            joined = Some(match joined {
                None => context.multiplicity.clone(),
                Some(current) => current.join(&context.multiplicity),
            });
        }
        joined.unwrap_or_default()
    }

    fn push_cost(&mut self, call_index: usize, cost: CostFact) {
        self.call_costs.entry(call_index).or_default().push(cost);
    }
}

/// Whether a curated membership effect mutates the scene graph in the
/// `MLP204` sense (adds / removes / replaces / reorders members).
const fn is_graph_mutation(effect: SceneMembershipEffect) -> bool {
    matches!(
        effect,
        SceneMembershipEffect::Add
            | SceneMembershipEffect::Remove
            | SceneMembershipEffect::Replace
            | SceneMembershipEffect::ReorderToFront
            | SceneMembershipEffect::ReorderToBack
            | SceneMembershipEffect::Clear
            | SceneMembershipEffect::AddForeground
            | SceneMembershipEffect::RemoveForeground
            | SceneMembershipEffect::AddFixedInFrame
            | SceneMembershipEffect::AddFixedOrientation
            | SceneMembershipEffect::RemoveFixedInFrame
            | SceneMembershipEffect::RemoveFixedOrientation
    )
}

/// Whether the resource-key argument is an f-string literal (a
/// frame-varying cache key candidate). Detection uses the lexer's string
/// kind, never a source-text heuristic; anything unresolvable is `false`
/// (not varying is never asserted — this is only the *proven varying*
/// gate).
/// Resolves one hot lambda-updater call against one scene: the updater
/// registration whose lambda callback sits at `lambda_site` names the
/// host object; the call qualifies when its callee is exactly
/// `<param>.<method>` with `<param>` the lambda's first parameter and
/// `<method>` a curated copy / become / align / family-walk name, and the
/// host's class candidates all resolve to curated Manim mobjects.
#[allow(
    clippy::too_many_arguments,
    reason = "internal resolution step over borrowed fact layers"
)]
fn hot_target_fact(
    sources: &SourceManager,
    calls: &QualifiedCallFacts,
    knowledge: &KnowledgeProfile,
    sizes: &SizeFacts,
    scene: &crate::semantic::interpreter::SceneLifecycle,
    lambda_site: AllocationSite,
    call_index: usize,
    call: &crate::frontend::index::QualifiedCall,
) -> Option<HotTargetSizeFact> {
    // 1. The registration whose callback is this lambda.
    let registration = scene
        .updaters
        .iter()
        .find(|registration| registration.fact.callback == CallbackRef::Lambda(lambda_site))?;
    let UpdaterHost::Mobject(host) = &registration.host else {
        return None;
    };
    // 2. The lambda's first parameter name, from the frontend signature of
    //    the `add_updater` call argument.
    let registration_call = calls.calls.iter().find(|candidate| {
        candidate.file == registration.site.file
            && u32::from(candidate.call_range.start()) == registration.site.start
            && u32::from(candidate.call_range.end()) == registration.site.end
    })?;
    let lambda_argument = registration_call.arguments.iter().find(|argument| {
        argument.range.start() == lambda_site.start.into()
            && argument.range.end() == lambda_site.end.into()
    })?;
    let param = lambda_argument
        .callable_signature
        .as_ref()
        .and_then(|signature| signature.params.first())
        .map(|param| param.name.as_str())?;
    // 3. The callee must be exactly `<param>.<method>`.
    let callee = sources.file(call.file).slice(call.callee_range);
    let method = callee.strip_prefix(param).and_then(|rest| {
        rest.strip_prefix('.').filter(|method| {
            !method.is_empty()
                && method
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
    })?;
    let kind = if COPY_METHOD_NAMES.contains(&method) {
        HotTargetKind::Copy
    } else if BECOME_METHOD_NAMES.contains(&method) {
        HotTargetKind::Become
    } else if ALIGN_METHOD_NAMES.contains(&method) {
        HotTargetKind::Align
    } else if FAMILY_WALK_METHOD_NAMES.contains(&method) {
        HotTargetKind::FamilyWalk
    } else {
        return None;
    };
    // 4. The host must be a curated Manim mobject (a project subclass
    //    could override the method with different semantics).
    let heap = &scene.final_heap;
    let host = heap.resolve(host);
    let state = heap.object(&host)?;
    let crate::semantic::values::KindSet::Known(candidates) = &state.kind else {
        return None;
    };
    let all_curated_mobjects = !candidates.is_empty()
        && candidates.iter().all(|candidate| {
            knowledge.symbol(candidate).is_some_and(|entry| {
                matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
            })
        });
    if !all_curated_mobjects {
        return None;
    }
    // 5. Sizes of the host, unanchored (the callback runs per frame; any
    //    count-changing operation anywhere voids the claim).
    let host_sizes = sizes
        .scene(&scene.qualified_name)
        .map_or_else(ObjectSizes::unknown, |scene_sizes| {
            scene_sizes.sizes_at(&host, None)
        });
    Some(HotTargetSizeFact {
        call_index,
        method: method.to_owned(),
        kind,
        scene: scene.qualified_name.clone(),
        target: host,
        sizes: host_sizes,
    })
}

/// Element byte width of a recognized `NumPy` dtype label.
fn dtype_bytes(label: &str) -> Option<i64> {
    // Fixed-width NumPy scalar types (numpy.org dtype reference); the
    // platform-dependent aliases (`int`, `float`, `int_`, `intp`) are
    // deliberately absent.
    let bytes = match label {
        "bool" | "bool_" | "int8" | "uint8" => 1,
        "float16" | "int16" | "uint16" => 2,
        "float32" | "int32" | "uint32" => 4,
        "complex64" | "float64" | "int64" | "uint64" => 8,
        "complex128" => 16,
        _ => return None,
    };
    Some(bytes)
}

/// Element count of a literal ndarray shape argument: a non-negative
/// integer length, or the product of a tuple / list of non-negative
/// integer dimensions (`np.zeros((w, h))`). `None` for anything `NumPy`
/// would reject or that is not fully literal — never guessed.
fn ndarray_element_count(shape: &crate::frontend::index::LiteralFact) -> Option<i64> {
    use crate::frontend::index::LiteralFact;
    match shape {
        LiteralFact::Int(length) if *length >= 0 => Some(*length),
        // NumPy multiplies the dimensions; an empty shape is a 0-d array
        // holding exactly one element. Negative or non-integer dimensions
        // raise at runtime and prove nothing.
        LiteralFact::Tuple(dimensions) | LiteralFact::List(dimensions) => {
            let mut elements: i64 = 1;
            for dimension in dimensions {
                let LiteralFact::Int(extent) = dimension else {
                    return None;
                };
                if *extent < 0 {
                    return None;
                }
                elements = elements.checked_mul(*extent)?;
            }
            Some(elements)
        }
        _ => None,
    }
}

/// Statically proven allocation size of one ndarray constructor call:
/// literal shape element count × element size ([`Num::Unknown`]
/// otherwise).
fn ndarray_allocation_bytes(call: &crate::frontend::index::QualifiedCall) -> Num {
    use crate::frontend::index::LiteralFact;
    if call.has_star_args || call.has_star_star_kwargs {
        return Num::Unknown;
    }
    let Some(shape) = call.positional(0) else {
        return Num::Unknown;
    };
    let Some(length) = shape.literal.as_ref().and_then(ndarray_element_count) else {
        // Non-literal (or partially literal) shapes are never guessed.
        return Num::Unknown;
    };
    let element_bytes = if let Some(argument) = call.keyword("dtype") {
        let label = match (&argument.literal, &argument.shape) {
            (Some(LiteralFact::Str { value, .. }), _) => Some(value.clone()),
            (None, ArgShape::Attribute(attribute)) => Some(attribute.name.clone()),
            _ => None,
        };
        match label.as_deref().and_then(dtype_bytes) {
            Some(bytes) => bytes,
            None => return Num::Unknown,
        }
    } else if call
        .candidates
        .iter()
        .any(|candidate| candidate == "numpy.full")
    {
        // `numpy.full` infers the dtype from the fill value; only a
        // float literal proves float64. Integer fills are
        // platform-dependent (`numpy.int_`) — never guessed.
        match call.positional(1).and_then(|fill| fill.literal.as_ref()) {
            Some(LiteralFact::Float(_)) => 8,
            _ => return Num::Unknown,
        }
    } else {
        // `zeros` / `ones` / `empty` default to float64.
        8
    };
    Num::int(length).mul(&Num::int(element_bytes))
}

/// Interval absolute difference `|a − b|`; `Unknown` unless both values
/// carry real bounds.
fn abs_difference(a: &Num, b: &Num) -> Num {
    use crate::semantic::values::NumLit;
    if let (Num::Exact(NumLit::Int(a)), Num::Exact(NumLit::Int(b))) = (a, b) {
        return Num::int((a - b).abs());
    }
    let (Some((a_lo, a_hi)), Some((b_lo, b_hi))) = (a.bounds(), b.bounds()) else {
        return Num::Unknown;
    };
    let lower = {
        let a_exceeds = match (a_lo, b_hi) {
            (Some(a_lo), Some(b_hi)) => a_lo - b_hi,
            _ => f64::NEG_INFINITY,
        };
        let b_exceeds = match (b_lo, a_hi) {
            (Some(b_lo), Some(a_hi)) => b_lo - a_hi,
            _ => f64::NEG_INFINITY,
        };
        a_exceeds.max(b_exceeds).max(0.0)
    };
    let upper = match (a_lo, a_hi, b_lo, b_hi) {
        (Some(a_lo), Some(a_hi), Some(b_lo), Some(b_hi)) => {
            Some((a_hi - b_lo).abs().max((b_hi - a_lo).abs()))
        }
        _ => None,
    };
    Num::Interval {
        lo: Some(lower),
        hi: upper,
    }
}

fn key_argument_is_fstring(
    sources: &SourceManager,
    file: FileId,
    argument: Option<&CallArgument>,
) -> bool {
    let Some(argument) = argument else {
        return false;
    };
    // A fully static string produces a literal fact; only a Literal-shaped
    // argument without one can be an f-string.
    if !matches!(argument.shape, ArgShape::Literal) || argument.literal.is_some() {
        return false;
    }
    sources.file(file).tokens().iter().any(|(token, range)| {
        range.start() >= argument.range.start()
            && range.end() <= argument.range.end()
            && matches!(
                token,
                rustpython_parser::Tok::String { kind, .. } if kind.is_any_fstring()
            )
    })
}
