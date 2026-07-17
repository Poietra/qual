//! Lifecycle event IR (DESIGN §5.6) and invocation contexts (DESIGN §4.2).
//!
//! The abstract interpreter emits an ordered stream of [`Event`]s; lifecycle
//! and cost rules consume this shared fact stream instead of walking the AST
//! themselves. Frame-callback and play-start operations carry distinct
//! [`InvocationContext`]s so their frequencies are never conflated
//! (DESIGN §15 invariant 7).

use std::collections::BTreeMap;

use crate::semantic::state::{AnimationState, CallbackRef, UpdaterFact};
use crate::semantic::values::{AllocationSite, KindSet, Num, ObjectId};

/// Which lifecycle phase an operation is invoked from (DESIGN §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InvocationContext {
    /// Module import time.
    ModuleImport,
    /// `Scene.__init__` and renderer/camera/file-writer preparation.
    SceneInit,
    /// `Scene.setup()`.
    Setup,
    /// `Scene.construct()` outside any play.
    Construct,
    /// Play preparation: compile args, auto-add, `begin()`.
    PlayBegin,
    /// Per-frame callbacks: updaters, interpolate, stop conditions.
    FrameCallback,
    /// Rasterization / draw / readback / encode handoff.
    Raster,
    /// Play cleanup: `finish()`, `clean_up_from_scene`, final update.
    PlayCleanup,
    /// `Scene.tear_down()` and finalization.
    TearDown,
    /// The phase could not be determined.
    Unknown,
}

impl InvocationContext {
    /// Least upper bound: disagreement widens to
    /// [`InvocationContext::Unknown`].
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }

    /// Whether operations in this context run once per rendered frame.
    #[must_use]
    pub const fn is_per_frame(self) -> bool {
        matches!(self, Self::FrameCallback | Self::Raster)
    }
}

/// Ordered symbolic multiplicity factors (DESIGN §4.2).
///
/// Every factor defaults to an exact `1` (meaning "does not apply"), never
/// to an unknown; an unknown factor must be set to [`Num::Unknown`]
/// explicitly so it is never mistaken for `×1`.
#[derive(Debug, Clone, PartialEq)]
pub struct Multiplicity {
    /// Surrounding loop iteration count.
    pub loop_iterations: Num,
    /// Number of plays the operation participates in.
    pub plays: Num,
    /// Rendered frame count.
    pub frames: Num,
    /// Top-level scene root count.
    pub roots: Num,
    /// Expanded family member count.
    pub family: Num,
    /// Point count.
    pub points: Num,
    /// Bezier curve count.
    pub curves: Num,
    /// Function sample count.
    pub samples: Num,
    /// Device pixel count.
    pub pixels: Num,
    /// Distinct Text / TeX / SVG / Image cache keys.
    pub distinct_resource_keys: Num,
}

impl Multiplicity {
    /// The neutral multiplicity: every factor is exactly `1`.
    #[must_use]
    pub const fn one() -> Self {
        Self {
            loop_iterations: Num::int(1),
            plays: Num::int(1),
            frames: Num::int(1),
            roots: Num::int(1),
            family: Num::int(1),
            points: Num::int(1),
            curves: Num::int(1),
            samples: Num::int(1),
            pixels: Num::int(1),
            distinct_resource_keys: Num::int(1),
        }
    }

    /// The factors in canonical order, labelled.
    #[must_use]
    pub fn factors(&self) -> [(&'static str, &Num); 10] {
        [
            ("loop_iterations", &self.loop_iterations),
            ("plays", &self.plays),
            ("frames", &self.frames),
            ("roots", &self.roots),
            ("family", &self.family),
            ("points", &self.points),
            ("curves", &self.curves),
            ("samples", &self.samples),
            ("pixels", &self.pixels),
            ("distinct_resource_keys", &self.distinct_resource_keys),
        ]
    }

    /// The product of all factors. Any unknown factor makes the product
    /// unknown instead of being dropped.
    #[must_use]
    pub fn product(&self) -> Num {
        self.factors()
            .into_iter()
            .fold(Num::int(1), |product, (_, factor)| product.mul(factor))
    }

    /// Factor-wise least upper bound.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            loop_iterations: self.loop_iterations.join(&other.loop_iterations),
            plays: self.plays.join(&other.plays),
            frames: self.frames.join(&other.frames),
            roots: self.roots.join(&other.roots),
            family: self.family.join(&other.family),
            points: self.points.join(&other.points),
            curves: self.curves.join(&other.curves),
            samples: self.samples.join(&other.samples),
            pixels: self.pixels.join(&other.pixels),
            distinct_resource_keys: self
                .distinct_resource_keys
                .join(&other.distinct_resource_keys),
        }
    }
}

impl Default for Multiplicity {
    fn default() -> Self {
        Self::one()
    }
}

/// Which fact dimension a mutation writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MutationKind {
    /// Geometry points changed.
    Points,
    /// Color / stroke / fill style changed.
    Style,
    /// Fill / stroke / points opacity changed.
    Opacity,
    /// Family or scene membership changed.
    Membership,
    /// Camera frame / orientation state changed.
    CameraState,
    /// Updater registration state changed.
    Updaters,
    /// The mutated dimension could not be determined.
    Unknown,
}

/// What kind of operation a [`Cost`] fact describes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OperationKind {
    /// Object construction (`Text` / `MathTex` / `SVGMobject` / ...).
    Construction,
    /// `copy()` / `deepcopy` / starting or target copies.
    Copy,
    /// `align_data` / `insert_n_curves` topology alignment.
    AlignData,
    /// Family traversal / updater walk.
    FamilyWalk,
    /// Per-frame point and style interpolation.
    Interpolation,
    /// Cairo raster or OpenGL draw.
    Raster,
    /// FBO / frame readback and writer handoff.
    Readback,
    /// Video encode.
    Encode,
    /// External compiler / process launch (e.g. TeX, `dvisvgm`).
    ExternalProcess,
    /// SVG / XML parse and path construction.
    SvgParse,
    /// Large in-memory allocation.
    Allocation,
    /// Some other named operation.
    Other(String),
}

/// Payload of [`Event::Alloc`]: a new abstract object.
#[derive(Debug, Clone, PartialEq)]
pub struct Alloc {
    /// The freshly allocated object.
    pub object: ObjectId,
    /// Candidate classes of the allocation.
    pub kind: KindSet,
}

/// Payload of [`Event::Alias`]: `dst` now refers to the same object as
/// `src`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    /// The alias being introduced.
    pub dst: ObjectId,
    /// The already-known object.
    pub src: ObjectId,
}

/// What a callable's return value aliases (DESIGN §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnAlias {
    /// Returns `self` (fluent mutator).
    SelfReceiver,
    /// Returns the parameter at this zero-based position.
    Parameter(u32),
    /// Returns a fresh or known allocation.
    Allocation(ObjectId),
    /// The returned identity is unknown.
    Unknown,
}

/// Payload of [`Event::SceneAdd`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneAdd {
    /// The scene receiving the objects.
    pub scene: ObjectId,
    /// Objects added, in argument order.
    pub objects: Vec<ObjectId>,
    /// Whether this add re-orders an existing member: `Scene.add` removes a
    /// present object first and re-inserts it at the end (DESIGN §3.4).
    pub order_effect: bool,
}

/// Payload of [`Event::SceneRemove`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneRemove {
    /// The scene the objects are removed from.
    pub scene: ObjectId,
    /// Objects removed, in argument order.
    pub objects: Vec<ObjectId>,
}

/// Payload of [`Event::AddChild`]: `parent.add(child)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddChild {
    /// The structural parent.
    pub parent: ObjectId,
    /// The new submobject.
    pub child: ObjectId,
}

/// Payload of [`Event::RegisterUpdater`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterUpdater {
    /// The mobject or scene the updater is attached to.
    pub target: ObjectId,
    /// The callback and its signature facts.
    pub updater: UpdaterFact,
}

/// Payload of [`Event::SuspendUpdater`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendUpdater {
    /// The object whose updaters are suspended.
    pub target: ObjectId,
}

/// Payload of [`Event::ResumeUpdater`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeUpdater {
    /// The object whose updaters resume.
    pub target: ObjectId,
}

/// Payload of [`Event::Mutate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mutate {
    /// The mutated object.
    pub target: ObjectId,
    /// Which dimension was written.
    pub kind: MutationKind,
}

/// Payload of [`Event::CreateAnimation`].
///
/// Constructing an animation has no scene effect by itself; introducer /
/// remover / replacement effects happen at play setup and cleanup
/// (DESIGN §15 invariant 4).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateAnimation {
    /// Identity of the animation object itself.
    pub animation: ObjectId,
    /// Kind, targets, effects, run time, and channel sets.
    pub state: AnimationState,
}

/// Payload of [`Event::BeginPlay`].
#[derive(Debug, Clone, PartialEq)]
pub struct BeginPlay {
    /// Identifier shared by every animation of the same `play` call.
    pub play_group: u64,
    /// Animations participating in the play.
    pub animations: Vec<ObjectId>,
    /// Play duration: `max(animation.run_time)`.
    pub duration: Num,
}

/// Payload of [`Event::FrameCallback`]: a callback that runs on the
/// per-frame time grid.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameCallback {
    /// The invoked callback.
    pub callback: CallbackRef,
    /// Phase the callback is invoked from.
    pub context: InvocationContext,
    /// Symbolic invocation-count factors.
    pub multiplicity: Multiplicity,
}

/// One cleanup effect of finishing a play (DESIGN §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupEffect {
    /// Remover animations remove their targets from the scene.
    SceneRemove(Vec<ObjectId>),
    /// `ReplacementTransform` replaces the source with the target.
    SceneReplace {
        /// Object leaving the scene.
        old: ObjectId,
        /// Object taking its place.
        new: ObjectId,
    },
    /// Suspended updaters resume.
    ResumeUpdaters(Vec<ObjectId>),
    /// The trailing `Scene.update_mobjects(0)` pass.
    FinalUpdaterPass,
}

/// Payload of [`Event::FinishPlay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishPlay {
    /// The play being finished.
    pub play_group: u64,
    /// Cleanup effects in execution order.
    pub cleanup: Vec<CleanupEffect>,
}

/// What a renderer requirement demands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RendererRequirementKind {
    /// Only meaningful / supported under Cairo.
    RequiresCairo,
    /// Only meaningful / supported under OpenGL.
    RequiresOpengl,
    /// Behaves differently between the renderers.
    DivergesBetweenRenderers,
}

/// Payload of [`Event::RendererRequirement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererRequirement {
    /// The requirement kind.
    pub kind: RendererRequirementKind,
    /// Short human-readable reason (API name or semantics note).
    pub note: String,
}

/// Payload of [`Event::Cost`]: one symbolic cost fact (DESIGN §4.1-§4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    /// The operation performed.
    pub operation: OperationKind,
    /// Named size dimensions (`points`, `curves`, `family`, ...), each a
    /// symbolic value.
    pub dimensions: BTreeMap<String, Num>,
    /// Phase the operation is invoked from.
    pub context: InvocationContext,
    /// Symbolic invocation-count factors.
    pub multiplicity: Multiplicity,
}

/// Payload of [`Event::UnknownMutation`]: values whose facts had to be
/// widened because an unresolved call may mutate them (DESIGN §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownMutation {
    /// The objects whose facts were widened.
    pub values: Vec<ObjectId>,
}

/// One lifecycle event produced by the abstract interpreter (DESIGN §5.6).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A new abstract object was allocated.
    Alloc(Alloc),
    /// An alias between two ids was established.
    Alias(Alias),
    /// A callable's return-value alias fact.
    ReturnAlias(ReturnAlias),
    /// Objects were added to a scene.
    SceneAdd(SceneAdd),
    /// Objects were removed from a scene.
    SceneRemove(SceneRemove),
    /// A submobject was attached to a parent.
    AddChild(AddChild),
    /// An updater was registered.
    RegisterUpdater(RegisterUpdater),
    /// Updaters were suspended.
    SuspendUpdater(SuspendUpdater),
    /// Updaters resumed.
    ResumeUpdater(ResumeUpdater),
    /// An object was mutated.
    Mutate(Mutate),
    /// An animation object was constructed (no scene effect yet).
    CreateAnimation(CreateAnimation),
    /// A play started.
    BeginPlay(BeginPlay),
    /// A callback runs on the per-frame grid.
    FrameCallback(FrameCallback),
    /// A play finished, with its cleanup effects.
    FinishPlay(FinishPlay),
    /// A renderer-specific requirement was observed.
    RendererRequirement(RendererRequirement),
    /// A symbolic cost fact.
    Cost(Cost),
    /// An unresolved call may have mutated these values.
    UnknownMutation(UnknownMutation),
}

/// An [`Event`] anchored to the source expression that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRecord {
    /// The originating source expression.
    pub site: AllocationSite,
    /// The event.
    pub event: Event,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::values::NumLit;

    #[test]
    fn invocation_context_join() {
        let contexts = [
            InvocationContext::ModuleImport,
            InvocationContext::SceneInit,
            InvocationContext::Setup,
            InvocationContext::Construct,
            InvocationContext::PlayBegin,
            InvocationContext::FrameCallback,
            InvocationContext::Raster,
            InvocationContext::PlayCleanup,
            InvocationContext::TearDown,
            InvocationContext::Unknown,
        ];
        for a in contexts {
            assert_eq!(a.join(a), a);
            assert_eq!(
                a.join(InvocationContext::Unknown),
                InvocationContext::Unknown
            );
            for b in contexts {
                assert_eq!(a.join(b), b.join(a));
            }
        }
        assert_eq!(
            InvocationContext::Construct.join(InvocationContext::FrameCallback),
            InvocationContext::Unknown
        );
        assert!(InvocationContext::FrameCallback.is_per_frame());
        assert!(!InvocationContext::PlayBegin.is_per_frame());
    }

    #[test]
    fn multiplicity_product_multiplies_all_factors() {
        let mut multiplicity = Multiplicity::one();
        assert_eq!(multiplicity.product(), Num::int(1));

        multiplicity.frames = Num::int(480);
        multiplicity.family = Num::int(46);
        assert_eq!(multiplicity.product(), Num::int(480 * 46));

        // An unknown factor makes the product unknown, never ×1.
        multiplicity.points = Num::Unknown;
        assert_eq!(multiplicity.product(), Num::Unknown);
    }

    #[test]
    fn multiplicity_factor_order_is_canonical() {
        let names: Vec<&str> = Multiplicity::one()
            .factors()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            names,
            vec![
                "loop_iterations",
                "plays",
                "frames",
                "roots",
                "family",
                "points",
                "curves",
                "samples",
                "pixels",
                "distinct_resource_keys",
            ]
        );
    }

    #[test]
    fn multiplicity_join_is_factor_wise() {
        let mut a = Multiplicity::one();
        a.frames = Num::int(100);
        let mut b = Multiplicity::one();
        b.frames = Num::int(200);
        let joined = a.join(&b);
        assert_eq!(
            joined.frames,
            Num::Interval {
                lo: Some(100.0),
                hi: Some(200.0),
            }
        );
        assert_eq!(joined.plays, Num::Exact(NumLit::Int(1)));
    }
}
