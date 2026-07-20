//! Abstract state records (DESIGN §5.5).
//!
//! `MobjectState`, `AnimationState`, `SceneState`, `CameraState`,
//! `OutputState`, and `ResourceState` keep membership, updater, visibility,
//! and renderer facts as separate dimensions — scene membership and
//! visibility are never collapsed into one boolean (DESIGN §15
//! invariant 3).
//!
//! Every record has a `join` (branch merge: disagreement becomes a `Maybe`
//! or widened fact, never a wrong certain one) and a `widen` (loop
//! fixpoints: moving interval bounds open, candidate sets cap out).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::model::RenderProfile;
use crate::semantic::events::InvocationContext;
use crate::semantic::values::{
    AllocationSite, CopyKind, KindSet, Num, ObjectId, Presence, Truth, Visibility,
};

/// Reference to an updater / frame-callback callable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum CallbackRef {
    /// A named project function or method (qualified name).
    Named(String),
    /// A lambda or nested def, identified by its source site.
    Lambda(AllocationSite),
    /// The callback identity could not be resolved.
    Unknown,
}

/// Static summary of a callback signature.
///
/// Manim decides between `(mobject)` and `(mobject, dt)` calls by checking
/// whether the signature has a parameter *named* `dt` (DESIGN §3.3); a
/// two-parameter signature alone does not make an updater time-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SignatureSummary {
    /// Number of positional parameters, when known.
    pub positional_params: Option<u8>,
    /// Whether a parameter named `dt` exists (drives the time-based call).
    pub has_dt_named_param: Truth,
    /// Whether the signature has `*args`.
    pub has_var_positional: Truth,
    /// Whether Manim's actual positional invocation binds successfully.
    pub binds_positionally: Truth,
}

impl SignatureSummary {
    /// A summary with nothing known.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            positional_params: None,
            has_dt_named_param: Truth::Maybe,
            has_var_positional: Truth::Maybe,
            binds_positionally: Truth::Maybe,
        }
    }
}

/// One registered updater: callback, signature facts, and whether it is
/// time-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UpdaterFact {
    /// The registered callback.
    pub callback: CallbackRef,
    /// Signature facts of the callback.
    pub signature: SignatureSummary,
    /// Whether Manim will call it with `dt` (a `dt`-named parameter).
    pub time_based: Truth,
}

/// Fact about a mobject's `generate_target()` state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTarget {
    /// Whether a target exists (Maybe when only some paths generated one).
    pub presence: Presence,
    /// The target copy's identity, when a single one is known.
    pub target: Option<ObjectId>,
}

impl GeneratedTarget {
    /// No target generated.
    #[must_use]
    pub const fn absent() -> Self {
        Self {
            presence: Presence::Absent,
            target: None,
        }
    }

    /// Least upper bound.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            presence: self.presence.join(other.presence),
            target: if self.target == other.target {
                self.target.clone()
            } else {
                None
            },
        }
    }
}

/// Abstract state of one mobject (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct MobjectState {
    /// Candidate classes.
    pub kind: KindSet,
    /// How this object was produced, when it is a copy.
    pub copy_provenance: Option<CopyKind>,
    /// Membership in the scene's top-level root list.
    pub scene_root_membership: Presence,
    /// Effective scene membership through any family path.
    pub family_membership: Presence,
    /// Structural parents (possible `submobjects` containers).
    pub parents: BTreeSet<ObjectId>,
    /// Structural children (`submobjects`).
    pub children: BTreeSet<ObjectId>,
    /// Whether the object contributes visible pixels.
    pub visibility: Visibility,
    /// Fill opacity fact.
    pub fill_opacity: Num,
    /// Stroke opacity fact.
    pub stroke_opacity: Num,
    /// Foreground stroke width fact.
    pub stroke_width: Num,
    /// Whether the object is in the scene's foreground list.
    pub foreground: Truth,
    /// The object's `z_index` (Cairo display-order sort key, DESIGN §3.4).
    ///
    /// `Exact` only when proven: a curated constructor with the Manim
    /// default (`Mobject.__init__` declares `z_index: float = 0`), a
    /// literal `z_index=` constructor kwarg, or a literal
    /// `set_z_index(...)` call. Any unknown mutation, non-literal write,
    /// or summarized helper mutation widens it — never a guessed `0`
    /// (DESIGN §15).
    pub z_index: Num,
    /// Whether the object is registered fixed-orientation (3D).
    pub fixed_orientation: Truth,
    /// Whether the object is registered fixed-in-frame (3D).
    pub fixed_in_frame: Truth,
    /// Registered updaters.
    pub updaters: BTreeSet<UpdaterFact>,
    /// Whether updating is currently suspended.
    pub updating_suspended: Truth,
    /// Expanded family member count.
    pub family_size: Num,
    /// Own-path point count (the object's own `points` array, not the
    /// family total). `Exact(0)` only when the constructor provably starts
    /// with an empty path (curated; `VMobject()` yes, `Square()` no).
    pub point_count: Num,
    /// Bezier curve count.
    pub curve_count: Num,
    /// Subpath count.
    pub subpath_count: Num,
    /// Points per cubic curve (`n_points_per_cubic_curve`, default 4).
    /// Exact only when the construction provably used the default; path
    /// arithmetic that depends on it stays `Unknown` otherwise.
    pub points_per_curve: Num,
    /// Monotonic mutation counter (bumped on every observed mutation).
    pub mutation_epoch: u64,
    /// `generate_target()` state.
    pub generated_target: GeneratedTarget,
    /// Whether `save_state()` ran on every path.
    pub saved_state: Presence,
    /// Renderer compatibility notes (e.g. raw point-layout assumptions).
    pub renderer_notes: BTreeSet<String>,
}

impl MobjectState {
    /// A freshly allocated mobject: not in any scene, invisible, nothing
    /// known about its sizes.
    #[must_use]
    pub fn fresh(kind: KindSet) -> Self {
        Self {
            kind,
            copy_provenance: None,
            scene_root_membership: Presence::Absent,
            family_membership: Presence::Absent,
            parents: BTreeSet::new(),
            children: BTreeSet::new(),
            visibility: Visibility::Invisible,
            fill_opacity: Num::Unknown,
            stroke_opacity: Num::Unknown,
            stroke_width: Num::Unknown,
            foreground: Truth::No,
            z_index: Num::Unknown,
            fixed_orientation: Truth::No,
            fixed_in_frame: Truth::No,
            updaters: BTreeSet::new(),
            updating_suspended: Truth::No,
            family_size: Num::Unknown,
            point_count: Num::Unknown,
            curve_count: Num::Unknown,
            subpath_count: Num::Unknown,
            points_per_curve: Num::Unknown,
            mutation_epoch: 0,
            generated_target: GeneratedTarget::absent(),
            saved_state: Presence::Absent,
            renderer_notes: BTreeSet::new(),
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            kind: self.kind.join(&other.kind),
            copy_provenance: if self.copy_provenance == other.copy_provenance {
                self.copy_provenance
            } else {
                None
            },
            scene_root_membership: self.scene_root_membership.join(other.scene_root_membership),
            family_membership: self.family_membership.join(other.family_membership),
            parents: self.parents.union(&other.parents).cloned().collect(),
            children: self.children.union(&other.children).cloned().collect(),
            visibility: self.visibility.join(other.visibility),
            fill_opacity: self.fill_opacity.join(&other.fill_opacity),
            stroke_opacity: self.stroke_opacity.join(&other.stroke_opacity),
            stroke_width: self.stroke_width.join(&other.stroke_width),
            foreground: self.foreground.join(other.foreground),
            z_index: self.z_index.join(&other.z_index),
            fixed_orientation: self.fixed_orientation.join(other.fixed_orientation),
            fixed_in_frame: self.fixed_in_frame.join(other.fixed_in_frame),
            updaters: self.updaters.union(&other.updaters).cloned().collect(),
            updating_suspended: self.updating_suspended.join(other.updating_suspended),
            family_size: self.family_size.join(&other.family_size),
            point_count: self.point_count.join(&other.point_count),
            curve_count: self.curve_count.join(&other.curve_count),
            subpath_count: self.subpath_count.join(&other.subpath_count),
            points_per_curve: self.points_per_curve.join(&other.points_per_curve),
            mutation_epoch: self.mutation_epoch.max(other.mutation_epoch),
            generated_target: self.generated_target.join(&other.generated_target),
            saved_state: self.saved_state.join(other.saved_state),
            renderer_notes: self
                .renderer_notes
                .union(&other.renderer_notes)
                .cloned()
                .collect(),
        }
    }

    /// Widening for loop fixpoints: like [`MobjectState::join`], but size
    /// intervals that keep moving open their bounds and the kind set caps
    /// out.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.kind = self.kind.widen(&next.kind);
        widened.fill_opacity = self.fill_opacity.widen(&next.fill_opacity);
        widened.stroke_opacity = self.stroke_opacity.widen(&next.stroke_opacity);
        widened.stroke_width = self.stroke_width.widen(&next.stroke_width);
        widened.z_index = self.z_index.widen(&next.z_index);
        widened.family_size = self.family_size.widen(&next.family_size);
        widened.point_count = self.point_count.widen(&next.point_count);
        widened.curve_count = self.curve_count.widen(&next.curve_count);
        widened.subpath_count = self.subpath_count.widen(&next.subpath_count);
        widened.points_per_curve = self.points_per_curve.widen(&next.points_per_curve);
        widened
    }

    /// Weakens definite facts for a join against a branch in which this
    /// object was never allocated: everything "definitely so" that depends
    /// on the object existing becomes a maybe-fact.
    #[must_use]
    pub fn weaken_for_missing_branch(&self) -> Self {
        let mut weakened = self.clone();
        weakened.scene_root_membership = weakened.scene_root_membership.join(Presence::Absent);
        weakened.family_membership = weakened.family_membership.join(Presence::Absent);
        weakened.visibility = weakened.visibility.join(Visibility::Invisible);
        weakened.foreground = weakened.foreground.join(Truth::No);
        weakened.fixed_orientation = weakened.fixed_orientation.join(Truth::No);
        weakened.fixed_in_frame = weakened.fixed_in_frame.join(Truth::No);
        weakened.saved_state = weakened.saved_state.join(Presence::Absent);
        weakened.generated_target = weakened.generated_target.join(&GeneratedTarget::absent());
        weakened
    }
}

/// Which channel of a live mobject an animation writes or reads
/// (DESIGN §7.1, `MLC108`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WriteChannel {
    /// Geometry points.
    Points,
    /// Color / stroke / fill style.
    Style,
    /// Fill / stroke / points opacity.
    Opacity,
    /// Scene or family membership.
    Membership,
    /// Camera frame / orientation state.
    CameraState,
}

/// Whether an animation suspends its live targets' updaters while playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SuspendBehavior {
    /// Suspends live-target updaters during the play (the usual behavior).
    SuspendsLiveTargets,
    /// Leaves updaters running (e.g. `suspend_mobject_updating=False`).
    LeavesUpdatersRunning,
    /// Not determined.
    Unknown,
}

impl SuspendBehavior {
    /// Least upper bound.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }
}

/// Identifier shared by all animations of the same `play` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PlayGroupId(pub u64);

/// Abstract state of one animation (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationState {
    /// Candidate animation classes.
    pub kind: KindSet,
    /// Live target mobjects (never the starting/target copies).
    pub targets: BTreeSet<ObjectId>,
    /// Whether the animation auto-adds its target at play setup.
    pub introducer: Truth,
    /// Whether cleanup removes the target from the scene.
    pub remover: Truth,
    /// Whether cleanup replaces the source with the target
    /// (`ReplacementTransform`).
    pub replacement: Truth,
    /// Updater suspension behavior on live targets.
    pub suspend: SuspendBehavior,
    /// Run time in seconds.
    pub run_time: Num,
    /// Which `play` call this animation belongs to, once known.
    pub play_group: Option<PlayGroupId>,
    /// Whether the animation may change point/curve topology.
    pub topology_change: Truth,
    /// Channels the animation writes on its live targets.
    pub write_channels: BTreeSet<WriteChannel>,
    /// Channels the animation reads.
    pub read_channels: BTreeSet<WriteChannel>,
}

impl AnimationState {
    /// A state with nothing known beyond the kind candidates.
    #[must_use]
    pub fn unknown(kind: KindSet) -> Self {
        Self {
            kind,
            targets: BTreeSet::new(),
            introducer: Truth::Maybe,
            remover: Truth::Maybe,
            replacement: Truth::Maybe,
            suspend: SuspendBehavior::Unknown,
            run_time: Num::Unknown,
            play_group: None,
            topology_change: Truth::Maybe,
            write_channels: BTreeSet::new(),
            read_channels: BTreeSet::new(),
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            kind: self.kind.join(&other.kind),
            targets: self.targets.union(&other.targets).cloned().collect(),
            introducer: self.introducer.join(other.introducer),
            remover: self.remover.join(other.remover),
            replacement: self.replacement.join(other.replacement),
            suspend: self.suspend.join(other.suspend),
            run_time: self.run_time.join(&other.run_time),
            play_group: if self.play_group == other.play_group {
                self.play_group
            } else {
                None
            },
            topology_change: self.topology_change.join(other.topology_change),
            write_channels: self
                .write_channels
                .union(&other.write_channels)
                .copied()
                .collect(),
            read_channels: self
                .read_channels
                .union(&other.read_channels)
                .copied()
                .collect(),
        }
    }

    /// Widening for loop fixpoints.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.kind = self.kind.widen(&next.kind);
        widened.run_time = self.run_time.widen(&next.run_time);
        widened
    }
}

/// An ordered object-id list whose order may become uncertain after a
/// branch join (e.g. the scene root list, DESIGN §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedIdList {
    /// Possible members; when `order_known` is [`Truth::Yes`] this is the
    /// exact display order.
    pub items: Vec<ObjectId>,
    /// Whether `items` is the exact ordered list.
    pub order_known: Truth,
}

impl OrderedIdList {
    /// An empty list with exactly known (empty) order.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            items: Vec::new(),
            order_known: Truth::Yes,
        }
    }

    /// Least upper bound: identical lists stay ordered; disagreeing lists
    /// merge to a possible-member list with unknown order.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        if self.items == other.items {
            return Self {
                items: self.items.clone(),
                order_known: self.order_known.join(other.order_known),
            };
        }
        let mut items = self.items.clone();
        for id in &other.items {
            if !items.contains(id) {
                items.push(id.clone());
            }
        }
        Self {
            items,
            order_known: Truth::Maybe,
        }
    }

    /// Whether the id is a possible member.
    #[must_use]
    pub fn contains(&self, id: &ObjectId) -> bool {
        self.items.contains(id)
    }
}

/// Abstract state of one scene (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct SceneState {
    /// Candidate scene classes.
    pub kind: KindSet,
    /// Ordered top-level mobject roots.
    pub roots: OrderedIdList,
    /// Foreground mobjects.
    pub foreground: OrderedIdList,
    /// Elapsed scene time in seconds.
    pub elapsed_time: Num,
    /// Current lifecycle phase.
    pub phase: InvocationContext,
    /// Scene-level updaters (called with `(dt)` only, DESIGN §3.3).
    pub scene_updaters: BTreeSet<UpdaterFact>,
    /// Mobjects with currently active (non-suspended) updaters.
    pub active_updater_owners: BTreeSet<ObjectId>,
    /// Animations currently playing.
    pub active_animations: BTreeSet<ObjectId>,
    /// Tracked value of `self.always_update_mobjects` (DESIGN §3.3):
    /// `No` is the Manim default, a literal assignment (or a literal
    /// `super().__init__(always_update_mobjects=...)` kwarg) sets it
    /// exactly, and any non-literal write degrades it to `Maybe` — the
    /// fact is never silently widened away (MLP227).
    pub always_update_mobjects: Truth,
}

impl SceneState {
    /// A scene at `__init__` time with nothing added yet.
    #[must_use]
    pub fn initial(kind: KindSet) -> Self {
        Self {
            kind,
            roots: OrderedIdList::empty(),
            foreground: OrderedIdList::empty(),
            elapsed_time: Num::int(0),
            phase: InvocationContext::SceneInit,
            scene_updaters: BTreeSet::new(),
            active_updater_owners: BTreeSet::new(),
            active_animations: BTreeSet::new(),
            always_update_mobjects: Truth::No,
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            kind: self.kind.join(&other.kind),
            roots: self.roots.join(&other.roots),
            foreground: self.foreground.join(&other.foreground),
            elapsed_time: self.elapsed_time.join(&other.elapsed_time),
            phase: self.phase.join(other.phase),
            scene_updaters: self
                .scene_updaters
                .union(&other.scene_updaters)
                .cloned()
                .collect(),
            active_updater_owners: self
                .active_updater_owners
                .union(&other.active_updater_owners)
                .cloned()
                .collect(),
            active_animations: self
                .active_animations
                .union(&other.active_animations)
                .cloned()
                .collect(),
            always_update_mobjects: self
                .always_update_mobjects
                .join(other.always_update_mobjects),
        }
    }

    /// Widening for loop fixpoints.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.kind = self.kind.widen(&next.kind);
        widened.elapsed_time = self.elapsed_time.widen(&next.elapsed_time);
        widened
    }
}

/// Abstract camera state (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct CameraState {
    /// Candidate camera classes.
    pub kind: KindSet,
    /// The `MovingCamera` frame / `ThreeDCamera` tracker object, when known.
    pub frame_object: Option<ObjectId>,
    /// Whether an ambient camera updater (e.g. ambient rotation) is active.
    pub has_ambient_updater: Truth,
    /// Time interval during which the camera is in motion.
    pub motion_interval: Num,
    /// Objects registered fixed-orientation.
    pub fixed_orientation: BTreeSet<ObjectId>,
    /// Objects registered fixed-in-frame.
    pub fixed_in_frame: BTreeSet<ObjectId>,
    /// Whether camera motion makes the whole scene moving scope.
    pub scene_fully_moving: Truth,
}

impl CameraState {
    /// A static default camera.
    #[must_use]
    pub fn initial(kind: KindSet) -> Self {
        Self {
            kind,
            frame_object: None,
            has_ambient_updater: Truth::No,
            motion_interval: Num::int(0),
            fixed_orientation: BTreeSet::new(),
            fixed_in_frame: BTreeSet::new(),
            scene_fully_moving: Truth::No,
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            kind: self.kind.join(&other.kind),
            frame_object: if self.frame_object == other.frame_object {
                self.frame_object.clone()
            } else {
                None
            },
            has_ambient_updater: self.has_ambient_updater.join(other.has_ambient_updater),
            motion_interval: self.motion_interval.join(&other.motion_interval),
            fixed_orientation: self
                .fixed_orientation
                .union(&other.fixed_orientation)
                .cloned()
                .collect(),
            fixed_in_frame: self
                .fixed_in_frame
                .union(&other.fixed_in_frame)
                .cloned()
                .collect(),
            scene_fully_moving: self.scene_fully_moving.join(other.scene_fully_moving),
        }
    }

    /// Widening for loop fixpoints.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.kind = self.kind.widen(&next.kind);
        widened.motion_interval = self.motion_interval.widen(&next.motion_interval);
        widened
    }
}

/// Abstract output / writer state, mirroring the resolved
/// [`RenderProfile`] fields (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputState {
    /// Output width in pixels.
    pub pixel_width: Num,
    /// Output height in pixels.
    pub pixel_height: Num,
    /// Frames per second.
    pub frame_rate: Num,
    /// Video encoder name, when a single one is known.
    pub video_encoder: Option<String>,
    /// Whether output has an alpha channel.
    pub transparent: Truth,
    /// Local-fork Cairo static layer reuse.
    pub cairo_static_layers: Truth,
    /// Local-fork Cairo fork worker count.
    pub cairo_fork_workers: Num,
    /// Renderer-wide monotonic fork invalidation: once an unsupported play
    /// opens the parent encoder, later eligible plays cannot fork either
    /// (DESIGN §7.3).
    pub fork_disabled_permanently: Truth,
    /// OpenGL readback mode label, when known.
    pub opengl_readback: Option<String>,
    /// Antialias mode label, when known.
    pub antialias: Option<String>,
}

fn join_options(a: Option<&String>, b: Option<&String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) if a == b => Some(a.clone()),
        _ => None,
    }
}

impl OutputState {
    /// The output state a resolved render profile prescribes.
    #[must_use]
    pub fn from_profile(profile: &RenderProfile) -> Self {
        Self {
            pixel_width: Num::int(i64::from(profile.pixel_width)),
            pixel_height: Num::int(i64::from(profile.pixel_height)),
            frame_rate: Num::float(profile.frame_rate),
            video_encoder: Some(profile.video_encoder.clone()),
            transparent: Truth::from(profile.transparent),
            cairo_static_layers: Truth::from(profile.cairo_static_layers),
            cairo_fork_workers: Num::int(i64::from(profile.cairo_fork_workers)),
            fork_disabled_permanently: Truth::No,
            opengl_readback: Some(profile.opengl_readback.clone()),
            antialias: Some(profile.antialias.clone()),
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            pixel_width: self.pixel_width.join(&other.pixel_width),
            pixel_height: self.pixel_height.join(&other.pixel_height),
            frame_rate: self.frame_rate.join(&other.frame_rate),
            video_encoder: join_options(self.video_encoder.as_ref(), other.video_encoder.as_ref()),
            transparent: self.transparent.join(other.transparent),
            cairo_static_layers: self.cairo_static_layers.join(other.cairo_static_layers),
            cairo_fork_workers: self.cairo_fork_workers.join(&other.cairo_fork_workers),
            fork_disabled_permanently: self
                .fork_disabled_permanently
                .join(other.fork_disabled_permanently),
            opengl_readback: join_options(
                self.opengl_readback.as_ref(),
                other.opengl_readback.as_ref(),
            ),
            antialias: join_options(self.antialias.as_ref(), other.antialias.as_ref()),
        }
    }

    /// Widening for loop fixpoints.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.pixel_width = self.pixel_width.widen(&next.pixel_width);
        widened.pixel_height = self.pixel_height.widen(&next.pixel_height);
        widened.frame_rate = self.frame_rate.widen(&next.frame_rate);
        widened.cairo_fork_workers = self.cairo_fork_workers.widen(&next.cairo_fork_workers);
        widened
    }
}

/// Cache temperature assumption for Text / TeX / SVG / Image resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CacheAssumption {
    /// Nothing cached yet (first render).
    Cold,
    /// All previously seen keys are cached.
    Warm,
    /// Not determined.
    Unknown,
}

impl CacheAssumption {
    /// Least upper bound.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }
}

/// Abstract resource / cache state (DESIGN §5.5).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceState {
    /// Exactly known resource cache keys (Text / TeX / SVG / Image).
    pub known_keys: BTreeSet<String>,
    /// Distinct resource key count `K_resource` (includes symbolic keys).
    pub distinct_key_count: Num,
    /// Cache temperature assumption.
    pub cache: CacheAssumption,
}

impl ResourceState {
    /// An empty resource state with the given cache assumption.
    #[must_use]
    pub fn empty(cache: CacheAssumption) -> Self {
        Self {
            known_keys: BTreeSet::new(),
            distinct_key_count: Num::int(0),
            cache,
        }
    }

    /// Least upper bound of two branch states.
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            known_keys: self.known_keys.union(&other.known_keys).cloned().collect(),
            distinct_key_count: self.distinct_key_count.join(&other.distinct_key_count),
            cache: self.cache.join(other.cache),
        }
    }

    /// Widening for loop fixpoints: a key set still growing widens the
    /// distinct-key count instead of enumerating forever.
    #[must_use]
    pub fn widen(&self, next: &Self) -> Self {
        let mut widened = self.join(next);
        widened.distinct_key_count = self.distinct_key_count.widen(&next.distinct_key_count);
        widened
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{Platform, Renderer};
    use crate::semantic::values::{CallContextId, Cardinality};
    use crate::source::SourceManager;

    fn object(start: u32) -> ObjectId {
        let mut sources = SourceManager::new(".");
        let file = sources.load_bytes(std::path::Path::new("test.py"), b"pass\n");
        ObjectId::new(
            AllocationSite {
                file,
                start,
                end: start + 1,
            },
            CallContextId::empty(),
            Cardinality::Singleton,
        )
    }

    #[test]
    fn mobject_join_weakens_disagreeing_membership() {
        let base = MobjectState::fresh(KindSet::single("manim.Square"));
        let mut added = base.clone();
        added.scene_root_membership = Presence::Present;
        added.family_membership = Presence::Present;
        added.visibility = Visibility::Visible;

        let joined = base.join(&added);
        assert_eq!(joined.scene_root_membership, Presence::Maybe);
        assert_eq!(joined.family_membership, Presence::Maybe);
        assert_eq!(joined.visibility, Visibility::Maybe);
        // Agreement stays certain.
        assert_eq!(joined.foreground, Truth::No);
        assert_eq!(base.join(&base), base);
    }

    #[test]
    fn mobject_join_is_commutative_and_takes_max_epoch() {
        let mut a = MobjectState::fresh(KindSet::single("manim.Square"));
        a.mutation_epoch = 3;
        a.parents.insert(object(50));
        let mut b = MobjectState::fresh(KindSet::single("manim.Circle"));
        b.mutation_epoch = 7;
        b.updaters.insert(UpdaterFact {
            callback: CallbackRef::Unknown,
            signature: SignatureSummary::unknown(),
            time_based: Truth::Maybe,
        });

        let ab = a.join(&b);
        let ba = b.join(&a);
        assert_eq!(ab, ba);
        assert_eq!(ab.mutation_epoch, 7);
        assert!(ab.kind.may_be("manim.Square") && ab.kind.may_be("manim.Circle"));
        assert_eq!(ab.updaters.len(), 1);
        assert!(ab.parents.contains(&object(50)));
    }

    #[test]
    fn mobject_widen_opens_moving_size_bounds() {
        let mut prev = MobjectState::fresh(KindSet::Unknown);
        prev.family_size = Num::Interval {
            lo: Some(1.0),
            hi: Some(4.0),
        };
        let mut next = prev.clone();
        next.family_size = Num::Interval {
            lo: Some(1.0),
            hi: Some(8.0),
        };
        let widened = prev.widen(&next);
        assert_eq!(
            widened.family_size,
            Num::Interval {
                lo: Some(1.0),
                hi: None,
            }
        );
    }

    #[test]
    fn generated_target_join_disagreement_is_maybe() {
        let with_target = GeneratedTarget {
            presence: Presence::Present,
            target: Some(object(9)),
        };
        let joined = with_target.join(&GeneratedTarget::absent());
        assert_eq!(joined.presence, Presence::Maybe);
        assert_eq!(joined.target, None);
    }

    #[test]
    fn animation_join_merges_channels_and_effects() {
        let mut a = AnimationState::unknown(KindSet::single("manim.Transform"));
        a.introducer = Truth::No;
        a.write_channels.insert(WriteChannel::Points);
        a.run_time = Num::float(1.0);
        let mut b = AnimationState::unknown(KindSet::single("manim.Transform"));
        b.introducer = Truth::Yes;
        b.write_channels.insert(WriteChannel::Style);
        b.run_time = Num::float(2.0);

        let joined = a.join(&b);
        assert_eq!(joined.introducer, Truth::Maybe);
        assert!(joined.write_channels.contains(&WriteChannel::Points));
        assert!(joined.write_channels.contains(&WriteChannel::Style));
        assert_eq!(
            joined.run_time,
            Num::Interval {
                lo: Some(1.0),
                hi: Some(2.0),
            }
        );
        assert_eq!(a.join(&b), b.join(&a));
    }

    #[test]
    fn ordered_list_join_keeps_equal_order_and_degrades_otherwise() {
        let first = object(1);
        let second = object(2);
        let mut a = OrderedIdList::empty();
        a.items.push(first.clone());
        a.items.push(second.clone());

        let same = a.join(&a.clone());
        assert_eq!(same.order_known, Truth::Yes);
        assert_eq!(same.items, a.items);

        let mut b = OrderedIdList::empty();
        b.items.push(second.clone());
        let joined = a.join(&b);
        assert_eq!(joined.order_known, Truth::Maybe);
        assert!(joined.contains(&first));
        assert!(joined.contains(&second));
    }

    #[test]
    fn scene_join_degrades_phase_on_disagreement() {
        let mut a = SceneState::initial(KindSet::single("manim.Scene"));
        a.phase = InvocationContext::Construct;
        let mut b = a.clone();
        b.phase = InvocationContext::PlayBegin;
        assert_eq!(a.join(&b).phase, InvocationContext::Unknown);
        assert_eq!(a.join(&a.clone()).phase, InvocationContext::Construct);
    }

    fn profile(width: u32) -> RenderProfile {
        RenderProfile {
            name: "test".to_owned(),
            renderer: Renderer::Cairo,
            platform: Platform::Linux,
            working_directory: ".".to_owned(),
            pixel_width: width,
            pixel_height: 1080,
            frame_rate: 60.0,
            assets_dir: ".".to_owned(),
            allowed_fonts: Vec::new(),
            cairo_fork_workers: 0,
            cairo_static_layers: false,
            video_encoder: "libx264".to_owned(),
            transparent: false,
            antialias: "default".to_owned(),
            opengl_readback: "auto".to_owned(),
        }
    }

    #[test]
    fn output_state_mirrors_profile_and_joins() {
        let output = OutputState::from_profile(&profile(1920));
        assert_eq!(output.pixel_width, Num::int(1920));
        assert_eq!(output.transparent, Truth::No);
        assert_eq!(output.video_encoder.as_deref(), Some("libx264"));

        let other = OutputState::from_profile(&profile(3840));
        let joined = output.join(&other);
        assert_eq!(
            joined.pixel_width,
            Num::Interval {
                lo: Some(1920.0),
                hi: Some(3840.0),
            }
        );
        // Agreeing labels survive the join.
        assert_eq!(joined.video_encoder.as_deref(), Some("libx264"));
        assert_eq!(output.join(&output.clone()), output);
    }

    #[test]
    fn resource_state_join_and_widen() {
        let mut a = ResourceState::empty(CacheAssumption::Cold);
        a.known_keys.insert("tex:x^2".to_owned());
        a.distinct_key_count = Num::int(1);
        let mut b = ResourceState::empty(CacheAssumption::Warm);
        b.known_keys.insert("text:hello".to_owned());
        b.distinct_key_count = Num::int(2);

        let joined = a.join(&b);
        assert_eq!(joined.cache, CacheAssumption::Unknown);
        assert_eq!(joined.known_keys.len(), 2);
        assert_eq!(
            joined.distinct_key_count,
            Num::Interval {
                lo: Some(1.0),
                hi: Some(2.0),
            }
        );

        // A count still growing at the widening point opens its bound.
        let mut next = a.clone();
        next.distinct_key_count = Num::int(5);
        assert_eq!(
            a.widen(&next).distinct_key_count,
            Num::Interval {
                lo: Some(1.0),
                hi: None,
            }
        );
    }

    #[test]
    fn suspend_and_cache_lattice_laws() {
        let suspends = [
            SuspendBehavior::SuspendsLiveTargets,
            SuspendBehavior::LeavesUpdatersRunning,
            SuspendBehavior::Unknown,
        ];
        for a in suspends {
            assert_eq!(a.join(a), a);
            assert_eq!(a.join(SuspendBehavior::Unknown), SuspendBehavior::Unknown);
            for b in suspends {
                assert_eq!(a.join(b), b.join(a));
            }
        }
        let caches = [
            CacheAssumption::Cold,
            CacheAssumption::Warm,
            CacheAssumption::Unknown,
        ];
        for a in caches {
            assert_eq!(a.join(a), a);
            assert_eq!(a.join(CacheAssumption::Unknown), CacheAssumption::Unknown);
            for b in caches {
                assert_eq!(a.join(b), b.join(a));
            }
        }
    }
}
