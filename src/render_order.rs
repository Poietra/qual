//! Cairo effective display order and moving-suffix estimation
//! (DESIGN §3.4, §4.3 Cairo stage, §7.3 `MLP209`).
//!
//! The Cairo renderer does not draw the scene root list as-is. The
//! effective display list of a play is (verified against
//! `scene.py Scene.get_mobject_family_members` /
//! `get_moving_and_static_mobjects`, `utils/family.py
//! extract_mobject_family_members`, and `mobject.py Mobject.get_family`
//! in the sibling Manim checkout):
//!
//! 1. `list_update(scene.mobjects, scene.foreground_mobjects)`: roots that
//!    are not foreground, in root order, then the foreground list unchanged;
//! 2. every root flattened into its family in pre-order (`[self] +
//!    concat(child.get_family())`), retaining the **last** occurrence of a
//!    shared member (`remove_list_redundancies`);
//! 3. a **stable** sort by `z_index` (`sorted(..., key=z_index)`).
//!
//! `Scene.get_moving_mobjects` walks that list and returns the suffix from
//! the first member that is an animation-family member, a foreground
//! mobject, or carries family updaters — everything after the first moving
//! member is re-rasterized every frame (DESIGN §3.4 / §4.3
//! `N_moving_suffix`). `MovingCameraScene` widens a moving camera frame to
//! the whole scene (`moving_camera_scene.py get_moving_mobjects`).
//!
//! Per the binding `MLP209` prose (DESIGN §7.3): the position is computed
//! from the family flatten, the `z_index` stable sort, and foreground —
//! never from root add order alone — and when the order is `Unknown` no
//! quantitative claim (such as "83 later members") may be produced.
//! [`moving_suffix_evidence`] enforces the refusal.
//!
//! # Inputs consumed (and widening points)
//!
//! The computation runs over [`RenderOrderInputs`], a plain fact struct so
//! that rules (or a later interpreter extension) can widen coverage by
//! filling fields the adapter cannot prove today. [`inputs_at_play`]
//! builds it from [`LifecycleFacts`](crate::semantic::interpreter::LifecycleFacts)
//! using exactly:
//!
//! - the play statement's own [`StateSnapshot`] heap
//!   (`SceneLifecycle::snapshots`): scene root list + `order_known`,
//!   per-object `children` / `updaters` / `foreground` facts;
//! - the traced event stream before the play's `BeginPlay`
//!   (`AddChild`, `Mutate`, `RegisterUpdater`, `UnknownMutation`) to
//!   recover submobject *order* and updater certainty;
//! - the play's `FinishPlay` cleanup effects, undone to reconstruct the
//!   during-play root list from the post-play snapshot.
//!
//! Each member's `z_index` comes from the interpreter's tracked
//! `MobjectState::z_index` fact: the proven Manim default `0` of a
//! curated constructor, a literal `z_index=` kwarg, or a literal
//! `set_z_index(...)` write — and [`Num::Unknown`] after any write that
//! could not be proven (the resulting order is then `Unknown` with a
//! [`OrderUnknownReason::ZIndexUnknown`] reason). A caller with even
//! better facts may still overwrite `MemberFacts::z_index` on the
//! returned inputs and call [`DisplayOrder::compute`] again. Camera
//! motion is supplied by the caller ([`Truth::No`] for a plain `Scene`
//! profile, `Yes` / `Maybe` for `MovingCameraScene` facts) because
//! lifecycle facts carry no camera state yet.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::semantic::events::{CleanupEffect, Event, MutationKind};
use crate::semantic::heap::AbstractHeap;
use crate::semantic::interpreter::{PlayFact, PlayKind, SceneLifecycle, StateSnapshot};
use crate::semantic::values::{
    AllocationSite, Cardinality, Num, NumLit, ObjectId, Presence, Truth,
};

// ---------------------------------------------------------------------------
// Input model.
// ---------------------------------------------------------------------------

/// Per-member facts the display-order computation consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberFacts {
    /// Submobjects in Manim `submobjects` order. Meaningful as an ordered
    /// list only when [`MemberFacts::children_order_known`] is
    /// [`Truth::Yes`].
    pub children: Vec<ObjectId>,
    /// Whether `children` is the exact ordered submobject list.
    pub children_order_known: Truth,
    /// The member's `z_index`. [`Num::Unknown`] whenever a set could not
    /// be proven absent — never default to `0` on a guess (DESIGN §15).
    pub z_index: Num,
    /// Whether the member itself has registered updaters
    /// (`Mobject.get_updaters`; suspension does not matter for
    /// `get_moving_mobjects`).
    pub own_updaters: Truth,
    /// Whether the member is registered as a foreground mobject.
    pub foreground: Truth,
}

impl MemberFacts {
    /// A leaf member with no submobjects, updaters, or foreground status.
    #[must_use]
    pub const fn leaf(z_index: Num) -> Self {
        Self {
            children: Vec::new(),
            children_order_known: Truth::Yes,
            z_index,
            own_updaters: Truth::No,
            foreground: Truth::No,
        }
    }
}

/// Scene-level facts the display-order computation consumes.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderOrderInputs {
    /// Top-level scene roots in root-list order.
    pub roots: Vec<ObjectId>,
    /// Whether `roots` is the exact ordered root list.
    pub roots_order_known: Truth,
    /// Foreground mobjects in foreground-list (capture) order.
    pub foreground: Vec<ObjectId>,
    /// Whether `foreground` is the exact ordered foreground list.
    pub foreground_order_known: Truth,
    /// Facts for every member reachable from `roots` / `foreground`.
    pub members: BTreeMap<ObjectId, MemberFacts>,
}

// ---------------------------------------------------------------------------
// Display order.
// ---------------------------------------------------------------------------

/// Why a member occupies its display-list position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberProvenance {
    /// A non-foreground scene root (`position` = index in the root list).
    Root {
        /// Index in the scene root list.
        position: usize,
    },
    /// A foreground root, captured after every non-foreground root
    /// (`position` = index in the foreground list).
    ForegroundRoot {
        /// Index in the foreground list.
        position: usize,
    },
    /// A family member reached by flattening `root` (direct parent
    /// `parent`).
    Submobject {
        /// The root whose flatten produced this member.
        root: ObjectId,
        /// The direct structural parent.
        parent: ObjectId,
    },
}

/// One member of the effective display list.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayMember {
    /// The member.
    pub id: ObjectId,
    /// Its exactly known `z_index` (order is `Unknown` otherwise).
    pub z_index: NumLit,
    /// How the member entered the list.
    pub provenance: MemberProvenance,
}

/// Why the effective display order could not be determined.
#[derive(Debug, Clone, PartialEq)]
pub enum OrderUnknownReason {
    /// The scene MRO could not be linearized; constructor state is
    /// unknown (DESIGN §5.1).
    ConstructorStateUnknown,
    /// No state snapshot or event anchor exists for the play (e.g. the
    /// play happens inside a helper summary).
    SceneStateUnavailable,
    /// The scene root list order is not exactly known (branch join, loop
    /// widening, or an unresolved membership mutation).
    RootOrderUnknown,
    /// Two or more foreground mobjects whose capture order is not known,
    /// or inconsistent foreground facts.
    ForegroundOrderUnknown,
    /// A member is foreground on some paths only.
    ForegroundMembershipUnknown {
        /// The maybe-foreground member.
        member: ObjectId,
    },
    /// A parent's submobject order is not exactly known.
    ChildrenOrderUnknown {
        /// The parent whose submobject order is unknown.
        parent: ObjectId,
    },
    /// A member's `z_index` is not exactly known; the stable sort could
    /// place it anywhere, so no positional claim survives (`MLP209`
    /// prose: order is `Unknown` from that point on).
    ZIndexUnknown {
        /// The first flatten-order member with an unknown `z_index`.
        member: ObjectId,
    },
    /// A member id stands for several runtime objects (loop allocation);
    /// the list cannot be enumerated.
    AggregateMember {
        /// The non-singleton member.
        member: ObjectId,
    },
    /// A reachable member has no recorded facts.
    MissingMemberFacts {
        /// The member without facts.
        member: ObjectId,
    },
    /// The recorded family graph contains a cycle.
    FamilyCycle {
        /// The member closing the cycle.
        member: ObjectId,
    },
    /// The play's cleanup removed members whose during-play positions are
    /// unrecoverable from the post-play snapshot.
    CleanupObscuresOrder,
}

impl fmt::Display for OrderUnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConstructorStateUnknown => write!(f, "scene constructor state is unknown"),
            Self::SceneStateUnavailable => write!(f, "no scene state is recorded at this play"),
            Self::RootOrderUnknown => write!(f, "the scene root-list order is not exactly known"),
            Self::ForegroundOrderUnknown => {
                write!(f, "the foreground capture order is not exactly known")
            }
            Self::ForegroundMembershipUnknown { .. } => {
                write!(f, "a member is foreground on some paths only")
            }
            Self::ChildrenOrderUnknown { .. } => {
                write!(f, "a parent's submobject order is not exactly known")
            }
            Self::ZIndexUnknown { .. } => {
                write!(f, "a family member's z_index is not exactly known")
            }
            Self::AggregateMember { .. } => {
                write!(f, "a member stands for several loop-allocated objects")
            }
            Self::MissingMemberFacts { .. } => {
                write!(f, "a reachable member has no recorded facts")
            }
            Self::FamilyCycle { .. } => write!(f, "the recorded family graph contains a cycle"),
            Self::CleanupObscuresOrder => {
                write!(f, "play cleanup removals obscure the during-play order")
            }
        }
    }
}

/// The Cairo effective display order at one play.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayOrder {
    /// The exact ordered member list with per-member provenance.
    Known(Vec<DisplayMember>),
    /// The order could not be determined; no quantitative claim may be
    /// derived from it (DESIGN §7.3 `MLP209` prose).
    Unknown(OrderUnknownReason),
}

impl DisplayOrder {
    /// Whether the order is exactly known.
    #[must_use]
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }

    /// The ordered members, when known.
    #[must_use]
    pub fn members(&self) -> Option<&[DisplayMember]> {
        match self {
            Self::Known(members) => Some(members),
            Self::Unknown(_) => None,
        }
    }

    /// Computes the effective display list: `list_update(roots,
    /// foreground)`, pre-order family flatten retaining the last
    /// occurrence of shared members, then the `z_index` stable sort.
    #[must_use]
    pub fn compute(inputs: &RenderOrderInputs) -> Self {
        if inputs.roots_order_known != Truth::Yes {
            return Self::Unknown(OrderUnknownReason::RootOrderUnknown);
        }
        if inputs.foreground_order_known != Truth::Yes {
            return Self::Unknown(OrderUnknownReason::ForegroundOrderUnknown);
        }
        // Step 1: `list_update(mobjects, foreground_mobjects)`.
        let mut sequence: Vec<(ObjectId, MemberProvenance)> = Vec::new();
        for (position, id) in inputs.roots.iter().enumerate() {
            if inputs.foreground.contains(id) {
                continue;
            }
            sequence.push((id.clone(), MemberProvenance::Root { position }));
        }
        for (position, id) in inputs.foreground.iter().enumerate() {
            sequence.push((id.clone(), MemberProvenance::ForegroundRoot { position }));
        }
        // Step 2: pre-order family flatten.
        let mut flat: Vec<DisplayMember> = Vec::new();
        for (root, provenance) in sequence {
            let mut path: BTreeSet<ObjectId> = BTreeSet::new();
            if let Err(reason) =
                flatten_family(inputs, &root, &root, provenance, &mut flat, &mut path)
            {
                return Self::Unknown(reason);
            }
        }
        // Shared members keep their **last** occurrence
        // (`remove_list_redundancies`).
        let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
        let mut deduped: Vec<DisplayMember> = Vec::new();
        for member in flat.into_iter().rev() {
            if seen.insert(member.id.clone()) {
                deduped.push(member);
            }
        }
        deduped.reverse();
        // Step 3: stable z_index sort (`sorted` is stable in Python, as is
        // `sort_by` here).
        deduped.sort_by(|a, b| {
            a.z_index
                .as_f64()
                .partial_cmp(&b.z_index.as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Self::Known(deduped)
    }
}

/// Pre-order flatten of one root's family with per-member gate checks.
fn flatten_family(
    inputs: &RenderOrderInputs,
    id: &ObjectId,
    root: &ObjectId,
    provenance: MemberProvenance,
    out: &mut Vec<DisplayMember>,
    path: &mut BTreeSet<ObjectId>,
) -> Result<(), OrderUnknownReason> {
    if id.cardinality != Cardinality::Singleton {
        return Err(OrderUnknownReason::AggregateMember { member: id.clone() });
    }
    let Some(facts) = inputs.members.get(id) else {
        return Err(OrderUnknownReason::MissingMemberFacts { member: id.clone() });
    };
    match facts.foreground {
        Truth::Maybe => {
            return Err(OrderUnknownReason::ForegroundMembershipUnknown { member: id.clone() });
        }
        Truth::Yes if !inputs.foreground.contains(id) => {
            // Inconsistent inputs: a foreground member missing from the
            // foreground list has no known capture position.
            return Err(OrderUnknownReason::ForegroundOrderUnknown);
        }
        Truth::Yes | Truth::No => {}
    }
    let z_index = match &facts.z_index {
        Num::Exact(lit) if lit.as_f64().is_finite() => *lit,
        _ => return Err(OrderUnknownReason::ZIndexUnknown { member: id.clone() }),
    };
    if facts.children_order_known != Truth::Yes {
        return Err(OrderUnknownReason::ChildrenOrderUnknown { parent: id.clone() });
    }
    if !path.insert(id.clone()) {
        return Err(OrderUnknownReason::FamilyCycle { member: id.clone() });
    }
    out.push(DisplayMember {
        id: id.clone(),
        z_index,
        provenance,
    });
    for child in &facts.children {
        flatten_family(
            inputs,
            child,
            root,
            MemberProvenance::Submobject {
                root: root.clone(),
                parent: id.clone(),
            },
            out,
            path,
        )?;
    }
    path.remove(id);
    Ok(())
}

/// Whether the display order is exactly known (free function mirror of
/// [`DisplayOrder::is_known`] for the rules wave).
#[must_use]
pub const fn is_order_known(order: &DisplayOrder) -> bool {
    order.is_known()
}

// ---------------------------------------------------------------------------
// Moving scope and static suffix.
// ---------------------------------------------------------------------------

/// What may be moving during one play, besides per-member facts.
#[derive(Debug, Clone, PartialEq)]
pub struct MovingScope {
    /// Live animation targets of the play with the certainty that they
    /// move (their whole families move, `Animation.mobject.get_family()`).
    pub animation_targets: Vec<(ObjectId, Truth)>,
    /// Whether the camera frame moves during the play
    /// (`MovingCameraScene`): [`Truth::Yes`] makes the whole scene moving,
    /// [`Truth::Maybe`] caps nothing out. Supplied by the caller; the
    /// lifecycle facts carry no camera state yet.
    pub camera_moving: Truth,
    /// A played animation's targets could not be attributed: some member
    /// may be moving without appearing in `animation_targets`.
    pub unattributed_movers: bool,
}

/// Why a member is (possibly) moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovingReason {
    /// The member belongs to a played animation's target family.
    AnimationTarget,
    /// The member is a foreground mobject.
    Foreground,
    /// The member's family carries registered updaters.
    FamilyUpdater,
}

/// One (possibly) moving member of the display list.
#[derive(Debug, Clone, PartialEq)]
pub struct MovingEvidence {
    /// Zero-based position in the effective display list.
    pub index: usize,
    /// The member.
    pub id: ObjectId,
    /// The strongest reason it moves.
    pub reason: MovingReason,
    /// Whether it definitely moves.
    pub certainty: Truth,
}

/// The moving-suffix fact of one play over a known display order
/// (DESIGN §3.4 / §4.3 `N_moving_suffix`).
#[derive(Debug, Clone, PartialEq)]
pub struct SuffixFact {
    /// Total members of the effective display list.
    pub total_members: usize,
    /// Zero-based position of the first moving member: `None` when
    /// provably nothing moves, an exact value or an interval otherwise.
    /// Never a fabricated exact number (DESIGN §15).
    pub first_moving_index: Option<Num>,
    /// How many **later** members Cairo re-rasterizes every frame:
    /// `total_members - 1 - first_moving_index` when exact, an interval
    /// when the first moving position is uncertain.
    pub suffix_len: Num,
    /// The camera-motion input the fact was computed under.
    pub camera_moving: Truth,
    /// Per-member moving evidence in display order.
    pub members_evidence: Vec<MovingEvidence>,
}

/// `Yes` beats `Maybe` beats `No` (evidence strength, not the lattice
/// join).
fn strongest(a: Truth, b: Truth) -> Truth {
    match (a, b) {
        (Truth::Yes, _) | (_, Truth::Yes) => Truth::Yes,
        (Truth::Maybe, _) | (_, Truth::Maybe) => Truth::Maybe,
        (Truth::No, Truth::No) => Truth::No,
    }
}

/// The member's family closure (itself plus transitive submobjects known
/// to the inputs).
fn family_closure(inputs: &RenderOrderInputs, id: &ObjectId) -> Vec<ObjectId> {
    let mut closure = Vec::new();
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut queue = vec![id.clone()];
    while let Some(current) = queue.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(facts) = inputs.members.get(&current) {
            queue.extend(facts.children.iter().cloned());
        }
        closure.push(current);
    }
    closure
}

/// Whether any member of `id`'s family carries registered updaters.
fn family_updaters(inputs: &RenderOrderInputs, id: &ObjectId) -> Truth {
    let mut truth = Truth::No;
    for member in family_closure(inputs, id) {
        if let Some(facts) = inputs.members.get(&member) {
            truth = strongest(truth, facts.own_updaters);
        }
    }
    truth
}

#[allow(
    clippy::cast_precision_loss,
    reason = "display-list member counts are far below 2^52"
)]
fn approx(count: usize) -> f64 {
    count as f64
}

fn exact_count(count: usize) -> Num {
    i64::try_from(count).map_or(Num::Unknown, Num::int)
}

impl SuffixFact {
    /// Computes the moving suffix over a **known** display order; refuses
    /// (`None`) when the order is `Unknown`, so no quantitative claim can
    /// be built on an unknown order.
    #[must_use]
    pub fn compute(
        order: &DisplayOrder,
        inputs: &RenderOrderInputs,
        scope: &MovingScope,
    ) -> Option<Self> {
        let members = order.members()?;
        let total = members.len();
        if total == 0 {
            return Some(Self {
                total_members: 0,
                first_moving_index: None,
                suffix_len: Num::int(0),
                camera_moving: scope.camera_moving,
                members_evidence: Vec::new(),
            });
        }
        // Animation-target families.
        let mut animated: BTreeMap<ObjectId, Truth> = BTreeMap::new();
        for (target, truth) in &scope.animation_targets {
            for member in family_closure(inputs, target) {
                let entry = animated.entry(member).or_insert(Truth::No);
                *entry = strongest(*entry, *truth);
            }
        }
        let mut members_evidence = Vec::new();
        let mut first_certain: Option<usize> = None;
        let mut first_possible: Option<usize> = None;
        for (index, member) in members.iter().enumerate() {
            let facts = inputs.members.get(&member.id)?;
            let animation = animated.get(&member.id).copied().unwrap_or(Truth::No);
            let foreground = facts.foreground;
            let updaters = family_updaters(inputs, &member.id);
            let truth = strongest(strongest(animation, foreground), updaters);
            if truth == Truth::No {
                continue;
            }
            let reason = if animation == truth {
                MovingReason::AnimationTarget
            } else if foreground == truth {
                MovingReason::Foreground
            } else {
                MovingReason::FamilyUpdater
            };
            members_evidence.push(MovingEvidence {
                index,
                id: member.id.clone(),
                reason,
                certainty: truth,
            });
            if truth == Truth::Yes && first_certain.is_none() {
                first_certain = Some(index);
            }
            if first_possible.is_none() {
                first_possible = Some(index);
            }
        }
        if scope.camera_moving == Truth::Yes {
            first_certain = Some(0);
        }
        if scope.camera_moving == Truth::Yes
            || scope.camera_moving == Truth::Maybe
            || scope.unattributed_movers
        {
            first_possible = Some(0);
        }
        let first_possible = match (first_certain, first_possible) {
            (Some(certain), Some(possible)) => Some(possible.min(certain)),
            (Some(certain), None) => Some(certain),
            (None, possible) => possible,
        };
        let (first_moving_index, suffix_len) = match (first_certain, first_possible) {
            (Some(certain), Some(possible)) if certain == possible => {
                (Some(exact_count(certain)), exact_count(total - 1 - certain))
            }
            (Some(certain), Some(possible)) => (
                Some(Num::Interval {
                    lo: Some(approx(possible)),
                    hi: Some(approx(certain)),
                }),
                Num::Interval {
                    lo: Some(approx(total - 1 - certain)),
                    hi: Some(approx(total - 1 - possible)),
                },
            ),
            (None, Some(possible)) => (
                // Some member may move, or none does.
                Some(Num::Interval {
                    lo: Some(approx(possible)),
                    hi: None,
                }),
                Num::Interval {
                    lo: Some(0.0),
                    hi: Some(approx(total - 1 - possible)),
                },
            ),
            (None, None) => (None, Num::int(0)),
            (Some(_), None) => unreachable!("first_possible covers first_certain"),
        };
        Some(Self {
            total_members: total,
            first_moving_index,
            suffix_len,
            camera_moving: scope.camera_moving,
            members_evidence,
        })
    }
}

// ---------------------------------------------------------------------------
// Evidence formatting (DESIGN §7.3 MLP209 diagnostic example).
// ---------------------------------------------------------------------------

fn format_count(value: f64) -> String {
    format!("{value:.0}")
}

/// Formats the `MLP209`-style evidence sentence ("… at position 1/84.
/// Approximately 83 later family members may be rasterized for every frame
/// of this play."). Returns `None` — refusing to produce any number —
/// when the display order is `Unknown` or the suffix carries no numeric
/// bounds.
#[must_use]
pub fn moving_suffix_evidence(order: &DisplayOrder, suffix: &SuffixFact) -> Option<String> {
    if !order.is_known() {
        return None;
    }
    if suffix.total_members == 0 {
        return Some("the effective display list is empty for this play".to_owned());
    }
    let total = suffix.total_members;
    let Some(first) = &suffix.first_moving_index else {
        return Some(
            "no moving family member: the static base is rasterized once for this play".to_owned(),
        );
    };
    let position = match first {
        Num::Exact(NumLit::Int(index)) => format!("position {}/{total}", index + 1),
        Num::Interval {
            lo: Some(lo),
            hi: Some(hi),
        } => format!(
            "position between {} and {} of {total}",
            format_count(lo + 1.0),
            format_count(hi + 1.0)
        ),
        Num::Interval {
            lo: Some(lo),
            hi: None,
        } => format!("position {} or later of {total}", format_count(lo + 1.0)),
        _ => return None,
    };
    let later = match &suffix.suffix_len {
        Num::Exact(NumLit::Int(count)) => format!(
            "Approximately {count} later family members may be rasterized for every frame of this play."
        ),
        Num::Interval {
            lo: Some(lo),
            hi: Some(hi),
        } => format!(
            "Between {} and {} later family members may be rasterized for every frame of this play.",
            format_count(*lo),
            format_count(*hi)
        ),
        _ => return None,
    };
    Some(format!(
        "enters Cairo's effective z-ordered display list at {position}. {later}"
    ))
}

// ---------------------------------------------------------------------------
// Lifecycle-fact adapter.
// ---------------------------------------------------------------------------

/// The play statement's own snapshot (the last snapshot whose statement
/// range contains the play call).
fn play_snapshot<'a>(scene: &'a SceneLifecycle, play: &PlayFact) -> Option<&'a StateSnapshot> {
    scene.snapshots.iter().rev().find(|snapshot| {
        snapshot.site.file == play.site.file
            && snapshot.site.start <= play.site.start
            && snapshot.site.end >= play.site.end
    })
}

/// Index of the play's `BeginPlay` event in the scene event trace.
fn begin_event_index(scene: &SceneLifecycle, play: &PlayFact) -> Option<usize> {
    scene.events.iter().position(|traced| {
        matches!(&traced.event, Event::BeginPlay(begin) if begin.play_group == play.play_group.0)
    })
}

/// Replay cut for a wait: waits record no `BeginPlay` event, so the cut is
/// the first event produced at or after the wait statement in the same
/// file. This can only over-include future events (from `tear_down` or
/// out-of-order method layouts); over-inclusion is caught conservatively
/// by the snapshot cross-checks and the site guard on updater certainty.
fn wait_event_cut(scene: &SceneLifecycle, play: &PlayFact) -> usize {
    scene
        .events
        .iter()
        .position(|traced| {
            traced.site.file == play.site.file && traced.site.start >= play.site.start
        })
        .unwrap_or(scene.events.len())
}

/// The play's `FinishPlay` cleanup effects.
fn finish_cleanup<'a>(scene: &'a SceneLifecycle, play: &PlayFact) -> Option<&'a [CleanupEffect]> {
    scene.events.iter().find_map(|traced| match &traced.event {
        Event::FinishPlay(finish) if finish.play_group == play.play_group.0 => {
            Some(finish.cleanup.as_slice())
        }
        _ => None,
    })
}

/// Sequential per-member updater-certainty automaton state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdaterReplay {
    Certain,
    Uncertain,
}

/// The during-play root list: the play snapshot's post-play roots with
/// this play's cleanup effects undone and alias duplicates dropped to
/// their last (`Scene.add` re-add) position.
fn during_play_roots(
    scene: &SceneLifecycle,
    play: &PlayFact,
    heap: &AbstractHeap,
) -> Result<Vec<ObjectId>, OrderUnknownReason> {
    let scene_state = heap
        .scene(&scene.scene_id)
        .ok_or(OrderUnknownReason::SceneStateUnavailable)?;
    if scene_state.roots.order_known != Truth::Yes {
        return Err(OrderUnknownReason::RootOrderUnknown);
    }
    let mut roots: Vec<ObjectId> = scene_state
        .roots
        .items
        .iter()
        .map(|id| heap.resolve(id))
        .collect();
    // Waits have no animations and no cleanup membership effects.
    if play.kind == PlayKind::Play {
        let cleanup =
            finish_cleanup(scene, play).ok_or(OrderUnknownReason::SceneStateUnavailable)?;
        for effect in cleanup.iter().rev() {
            match effect {
                CleanupEffect::SceneRemove(ids) => {
                    if !ids.is_empty() {
                        // The removed members' during-play positions are gone.
                        return Err(OrderUnknownReason::CleanupObscuresOrder);
                    }
                }
                CleanupEffect::SceneReplace { old, new } => {
                    let new_id = heap.resolve(new);
                    let old_id = heap.resolve(old);
                    let Some(position) = roots.iter().position(|id| *id == new_id) else {
                        return Err(OrderUnknownReason::RootOrderUnknown);
                    };
                    roots[position] = old_id;
                }
                CleanupEffect::ResumeUpdaters(_) | CleanupEffect::FinalUpdaterPass => {}
            }
        }
    }
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut deduped: Vec<ObjectId> = Vec::new();
    for id in roots.into_iter().rev() {
        if seen.insert(id.clone()) {
            deduped.push(id);
        }
    }
    deduped.reverse();
    Ok(deduped)
}

/// Replay products of the pre-play event trace.
#[derive(Debug, Default)]
struct EventReplay {
    /// Reconstructed submobject add order per parent.
    child_order: BTreeMap<ObjectId, Vec<ObjectId>>,
    /// Parents whose submobject order or set is uncertain.
    poisoned_children: BTreeSet<ObjectId>,
    /// Per-member updater registration certainty.
    updaters: BTreeMap<ObjectId, UpdaterReplay>,
}

impl EventReplay {
    /// Replays the pre-play event trace for submobject order and updater
    /// certainty (the heap stores children as unordered sets).
    fn collect(
        scene: &SceneLifecycle,
        play: &PlayFact,
        heap: &AbstractHeap,
    ) -> Result<Self, OrderUnknownReason> {
        let cut = if play.kind == PlayKind::Play {
            begin_event_index(scene, play).ok_or(OrderUnknownReason::SceneStateUnavailable)?
        } else {
            wait_event_cut(scene, play)
        };
        // The wait cut may over-include future events; a `Certain` verdict
        // then additionally requires the registration site to sit before
        // the wait statement in the same file.
        let site_guard = |site: &AllocationSite| {
            play.kind == PlayKind::Play
                || (site.file == play.site.file && site.end <= play.site.start)
        };
        let scene_object = heap.resolve(&scene.scene_id);
        let mut replay = Self::default();
        for traced in &scene.events[..cut] {
            match &traced.event {
                Event::AddChild(add) => {
                    let parent = heap.resolve(&add.parent);
                    let child = heap.resolve(&add.child);
                    if parent == child || traced.certainty != Presence::Present {
                        // Self-adds are rejected by Manim; branch-dependent
                        // adds leave the order unknown.
                        replay.poisoned_children.insert(parent);
                        continue;
                    }
                    let order = replay.child_order.entry(parent).or_default();
                    // `Mobject.add` ignores an already-present child.
                    if !order.contains(&child) {
                        order.push(child);
                    }
                }
                Event::Mutate(mutate) => {
                    let target = heap.resolve(&mutate.target);
                    match mutate.kind {
                        MutationKind::Membership => {
                            // `Mobject.remove` (and other membership
                            // writes) surface as this; the resulting
                            // order is unknown.
                            replay.poisoned_children.insert(target);
                        }
                        MutationKind::Unknown => {
                            replay.poisoned_children.insert(target.clone());
                            replay.updaters.insert(target, UpdaterReplay::Uncertain);
                        }
                        MutationKind::Updaters => {
                            replay.updaters.insert(target, UpdaterReplay::Uncertain);
                        }
                        _ => {}
                    }
                }
                Event::RegisterUpdater(register) => {
                    let target = heap.resolve(&register.target);
                    if target == scene_object {
                        // Scene-level updaters do not enter
                        // `get_moving_mobjects` (scene.py).
                        continue;
                    }
                    if traced.certainty == Presence::Present && site_guard(&traced.site) {
                        replay.updaters.insert(target, UpdaterReplay::Certain);
                    } else {
                        replay
                            .updaters
                            .entry(target)
                            .or_insert(UpdaterReplay::Uncertain);
                    }
                }
                Event::UnknownMutation(unknown) => {
                    for value in &unknown.values {
                        let target = heap.resolve(value);
                        replay.poisoned_children.insert(target.clone());
                        replay.updaters.insert(target, UpdaterReplay::Uncertain);
                    }
                }
                _ => {}
            }
        }
        Ok(replay)
    }
}

/// Builds [`RenderOrderInputs`] for one play from the lifecycle facts.
///
/// See the module docs for exactly which facts are consumed. Member
/// `z_index` values carry the interpreter's tracked facts; a member whose
/// `z_index` could not be proven poisons [`DisplayOrder::compute`] into
/// `Unknown` from that member on.
pub fn inputs_at_play(
    scene: &SceneLifecycle,
    play: &PlayFact,
) -> Result<RenderOrderInputs, OrderUnknownReason> {
    if scene.constructor_state_unknown {
        return Err(OrderUnknownReason::ConstructorStateUnknown);
    }
    let snapshot = play_snapshot(scene, play).ok_or(OrderUnknownReason::SceneStateUnavailable)?;
    let heap = &snapshot.heap;
    let roots = during_play_roots(scene, play, heap)?;
    let replay = EventReplay::collect(scene, play, heap)?;

    // Member facts for everything reachable from the during-play roots.
    let mut members: BTreeMap<ObjectId, MemberFacts> = BTreeMap::new();
    let mut queue: Vec<ObjectId> = roots.clone();
    while let Some(id) = queue.pop() {
        if members.contains_key(&id) {
            continue;
        }
        let facts = member_facts(heap, &id, &replay)?;
        queue.extend(facts.children.iter().cloned());
        members.insert(id, facts);
    }

    // Foreground list: capture order is only known for at most one
    // foreground mobject (the interpreter keeps no foreground order).
    let foreground: Vec<ObjectId> = roots
        .iter()
        .filter(|id| {
            members
                .get(*id)
                .is_some_and(|facts| facts.foreground == Truth::Yes)
        })
        .cloned()
        .collect();
    let foreground_order_known = if foreground.len() <= 1 {
        Truth::Yes
    } else {
        Truth::Maybe
    };

    Ok(RenderOrderInputs {
        roots,
        roots_order_known: Truth::Yes,
        foreground,
        foreground_order_known,
        members,
    })
}

/// Facts of one member from the play snapshot plus the event replay.
fn member_facts(
    heap: &AbstractHeap,
    id: &ObjectId,
    replay: &EventReplay,
) -> Result<MemberFacts, OrderUnknownReason> {
    let Some(state) = heap.object(id) else {
        return Err(OrderUnknownReason::MissingMemberFacts { member: id.clone() });
    };
    let heap_children: BTreeSet<ObjectId> = state
        .children
        .iter()
        .map(|child| heap.resolve(child))
        .collect();
    let empty: Vec<ObjectId> = Vec::new();
    let replayed = replay.child_order.get(id).unwrap_or(&empty);
    let replay_set: BTreeSet<ObjectId> = replayed.iter().cloned().collect();
    let (children, children_order_known) = if replay.poisoned_children.contains(id) {
        (heap_children.iter().cloned().collect(), Truth::Maybe)
    } else if replay_set == heap_children {
        // The replayed add order accounts for every recorded child.
        (replayed.clone(), Truth::Yes)
    } else if heap_children.is_empty() {
        (Vec::new(), Truth::Yes)
    } else {
        // Children exist that no traced `AddChild` explains (e.g. copies).
        (heap_children.iter().cloned().collect(), Truth::Maybe)
    };
    let own_updaters = if state.updaters.is_empty() {
        Truth::No
    } else {
        match replay.updaters.get(id) {
            Some(UpdaterReplay::Certain) => Truth::Yes,
            _ => Truth::Maybe,
        }
    };
    Ok(MemberFacts {
        children,
        children_order_known,
        // The interpreter's tracked `z_index`: the proven Mobject default
        // `0`, a literal constructor kwarg / `set_z_index` write, or
        // `Unknown` after any unproven write (`MobjectState::z_index`).
        z_index: state.z_index.clone(),
        own_updaters,
        foreground: state.foreground,
    })
}

/// The play's moving scope from its compiled animations. `camera_moving`
/// is supplied by the caller (no camera facts exist in the lifecycle layer
/// yet): [`Truth::No`] for a plain-`Scene` profile, `Yes` / `Maybe` for
/// `MovingCameraScene` frame motion.
#[must_use]
pub fn moving_scope_at_play(
    scene: &SceneLifecycle,
    play: &PlayFact,
    camera_moving: Truth,
) -> MovingScope {
    let resolve = |id: &ObjectId| -> ObjectId {
        play_snapshot(scene, play).map_or_else(|| id.clone(), |snapshot| snapshot.heap.resolve(id))
    };
    let base = if play.certainty == Presence::Present {
        Truth::Yes
    } else {
        Truth::Maybe
    };
    let mut animation_targets: Vec<(ObjectId, Truth)> = Vec::new();
    let mut unattributed_movers = false;
    for played in &play.animations {
        match &played.state {
            Some(state) if !state.targets.is_empty() => {
                for target in &state.targets {
                    animation_targets.push((resolve(target), base));
                }
            }
            _ => {
                // A play argument whose target could not be tracked may
                // move any member (a bare mobject would be a TypeError).
                if play.kind == PlayKind::Play && played.convertible != Truth::No {
                    unattributed_movers = true;
                }
            }
        }
    }
    MovingScope {
        animation_targets,
        camera_moving,
        unattributed_movers,
    }
}

/// The Cairo effective display order at one play (DESIGN §3.4, §7.3).
///
/// `z_index` facts come from the interpreter (`MobjectState::z_index`):
/// exact for proven defaults and literal writes, `Unknown` otherwise —
/// an unproven member yields [`DisplayOrder::Unknown`] with a
/// [`OrderUnknownReason::ZIndexUnknown`] reason. Use [`inputs_at_play`] +
/// [`DisplayOrder::compute`] to inject better `z_index` facts if a
/// caller has them.
#[must_use]
pub fn display_order_at_play(scene: &SceneLifecycle, play: &PlayFact) -> DisplayOrder {
    match inputs_at_play(scene, play) {
        Ok(inputs) => DisplayOrder::compute(&inputs),
        Err(reason) => DisplayOrder::Unknown(reason),
    }
}

/// The moving-suffix fact at one play, `None` whenever the display order
/// is `Unknown` (no quantitative claim without a known order).
#[must_use]
pub fn moving_suffix_at_play(
    scene: &SceneLifecycle,
    play: &PlayFact,
    camera_moving: Truth,
) -> Option<SuffixFact> {
    let inputs = inputs_at_play(scene, play).ok()?;
    let order = DisplayOrder::compute(&inputs);
    let scope = moving_scope_at_play(scene, play, camera_moving);
    SuffixFact::compute(&order, &inputs, &scope)
}
