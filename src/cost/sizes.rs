//! Per-object cardinality facts bridged from the lifecycle interpreter
//! (DESIGN §4.1 `N_family` / `P` / `C` dimensions).
//!
//! [`SizeFacts::resolve`] walks each scene's converged lifecycle facts and
//! derives, per abstract object:
//!
//! - the expanded **family member count** from the structural
//!   parent/child graph (definite `AddChild` events give the lower bound,
//!   the final heap's possible-children sets give the upper bound, and
//!   the child id's [`Cardinality`] gives the per-edge multiplicity);
//! - own and family-total **point / curve counts** from the curated
//!   geometry table ([`super::geometry`]), guarded by the constructor
//!   call shape.
//!
//! Certainty rules (DESIGN §15):
//!
//! - Loop-created `Many` / `MaybeMany` children contribute intervals with
//!   **open upper bounds** — never a fabricated exact count.
//! - Branch-dependent (possible-only) edges contribute a `0` lower bound.
//! - Content-dependent kinds (Text / TeX / SVG / Image) stay `Unknown`.
//! - Count-changing operations (`become`, `restore`, unknown mutations,
//!   topology-changing plays) **poison** the affected facts from their
//!   program point on: a query at or before the poisoning event still
//!   sees the pre-mutation sizes, later queries see `Unknown`.

use std::collections::{BTreeMap, BTreeSet};

use crate::frontend::index::{QualifiedCall, QualifiedCallFacts};
use crate::knowledge::KnowledgeProfile;
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{LifecycleFacts, SceneLifecycle};
use crate::semantic::state::WriteChannel;
use crate::semantic::values::{
    AllocationSite, Cardinality, KindSet, Num, ObjectId, Presence, Truth,
};
use crate::source::FileId;

use super::contexts::resolve_candidate;
use super::geometry::curated_geometry;

/// Curated method names whose `Points` write provably preserves the point
/// count: affine / placement transforms only (mobject.py `shift` /
/// `move_to` / `next_to` / `to_edge` / `rotate` / `scale` / `flip` /
/// `center` / `align_to` / `stretch` all call `apply_points_function` /
/// family transforms without inserting or removing points). This mirrors
/// the interpreter's curated mutator-channel table; anything else that
/// writes points (e.g. `become`, `restore`) voids the counts.
const COUNT_PRESERVING_POINT_WRITERS: [&str; 10] = [
    "align_to", "center", "flip", "move_to", "next_to", "rotate", "scale", "shift", "stretch",
    "to_edge",
];

/// Resolved size facts of one abstract object.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSizes {
    /// Expanded family member count (`N_family`, the object included).
    pub family: Num,
    /// Family-total point count (`P`).
    pub points: Num,
    /// Family-total cubic curve count (`C`).
    pub curves: Num,
}

impl ObjectSizes {
    /// All-unknown sizes.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            family: Num::Unknown,
            points: Num::Unknown,
            curves: Num::Unknown,
        }
    }

    /// Field-wise least upper bound (used when several abstract objects
    /// match one query site).
    #[must_use]
    pub fn join(&self, other: &Self) -> Self {
        Self {
            family: self.family.join(&other.family),
            points: self.points.join(&other.points),
            curves: self.curves.join(&other.curves),
        }
    }
}

/// One object's sizes plus the event indices after which they are void.
#[derive(Debug, Clone, PartialEq)]
struct SizeRecord {
    sizes: ObjectSizes,
    /// First event index after which the family count is unreliable.
    family_poison: Option<usize>,
    /// First event index after which point / curve counts are unreliable
    /// (includes structural poisons: a changed family changes the totals).
    geometry_poison: Option<usize>,
}

impl SizeRecord {
    const fn unknown() -> Self {
        Self {
            sizes: ObjectSizes::unknown(),
            family_poison: None,
            geometry_poison: None,
        }
    }
}

fn min_opt(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (only, None) | (None, only) => only,
    }
}

/// Size facts of one analyzed scene.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneSizes {
    records: BTreeMap<ObjectId, SizeRecord>,
    /// First event index per source site (program order anchor).
    site_event: BTreeMap<AllocationSite, usize>,
    /// Alias-resolved objects allocated at a site (constructions and
    /// copies).
    objects_by_site: BTreeMap<AllocationSite, BTreeSet<ObjectId>>,
    /// Alias-resolved mutation targets per site (`become`, unresolved
    /// method calls, ... — the receiver objects of the call at that site).
    mutation_targets_by_site: BTreeMap<AllocationSite, BTreeSet<ObjectId>>,
    /// Alias snapshot for ids that appear in the query-facing lifecycle
    /// facts (play targets, events) under a non-representative id.
    alias: BTreeMap<ObjectId, ObjectId>,
}

impl SceneSizes {
    /// The program-order event index anchored at `site`, if any event was
    /// recorded there.
    #[must_use]
    pub fn event_index_of(&self, site: AllocationSite) -> Option<usize> {
        self.site_event.get(&site).copied()
    }

    /// The abstract objects a call at `site` is *about*: objects allocated
    /// there (constructions and copies) first, else the mutation targets
    /// (receivers) recorded there. Empty when nothing resolves — callers
    /// must treat that as "no claim", never as "no object".
    #[must_use]
    pub fn targets_at(&self, site: AllocationSite) -> BTreeSet<ObjectId> {
        if let Some(objects) = self.objects_by_site.get(&site) {
            if !objects.is_empty() {
                return objects.clone();
            }
        }
        self.mutation_targets_by_site
            .get(&site)
            .cloned()
            .unwrap_or_default()
    }

    /// Sizes of `object` as observable at `at` (a source site anchoring a
    /// program point). A field poisoned strictly before that point — or
    /// anywhere, when `at` is `None` or unanchored — reads `Unknown`.
    #[must_use]
    pub fn sizes_at(&self, object: &ObjectId, at: Option<AllocationSite>) -> ObjectSizes {
        let object = self.alias.get(object).unwrap_or(object);
        let Some(record) = self.records.get(object) else {
            return ObjectSizes::unknown();
        };
        let query_index = at.and_then(|site| self.event_index_of(site));
        let void = |poison: Option<usize>| -> bool {
            match (poison, query_index) {
                (None, _) => false,
                // The poisoning event itself still sees the old sizes
                // (strict comparison): a Transform's own begin cost is
                // measured against the pre-play geometry.
                (Some(poison), Some(query)) => poison < query,
                // Unanchored query: any poison voids the claim.
                (Some(_), None) => true,
            }
        };
        ObjectSizes {
            family: if void(record.family_poison) {
                Num::Unknown
            } else {
                record.sizes.family.clone()
            },
            points: if void(record.geometry_poison) {
                Num::Unknown
            } else {
                record.sizes.points.clone()
            },
            curves: if void(record.geometry_poison) {
                Num::Unknown
            } else {
                record.sizes.curves.clone()
            },
        }
    }
}

/// Size facts for every analyzed scene, keyed by qualified scene name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SizeFacts {
    /// Per-scene size facts, keyed by qualified scene class name.
    pub scenes: BTreeMap<String, SceneSizes>,
}

impl SizeFacts {
    /// The size facts of one scene.
    #[must_use]
    pub fn scene(&self, qualified_name: &str) -> Option<&SceneSizes> {
        self.scenes.get(qualified_name)
    }

    /// Resolves size facts from the lifecycle interpreter output.
    ///
    /// Without a knowledge profile the geometry table cannot be applied
    /// safely (constructor guards need resolved candidates), so only the
    /// structural family facts are produced.
    #[must_use]
    pub fn resolve(
        lifecycle: &LifecycleFacts,
        calls: &QualifiedCallFacts,
        knowledge: Option<&KnowledgeProfile>,
    ) -> Self {
        let call_map: BTreeMap<(FileId, u32, u32), &QualifiedCall> = calls
            .calls
            .iter()
            .map(|call| {
                (
                    (
                        call.file,
                        call.call_range.start().into(),
                        call.call_range.end().into(),
                    ),
                    call,
                )
            })
            .collect();
        let mut scenes = BTreeMap::new();
        for scene in &lifecycle.scenes {
            scenes.insert(
                scene.qualified_name.clone(),
                resolve_scene(scene, &call_map, knowledge),
            );
        }
        Self { scenes }
    }
}

/// Whether every resolvable candidate of the call at `site` is a curated
/// count-preserving point writer. Unresolvable sites answer `false`
/// (unknown writers void the counts).
fn site_preserves_counts(
    call_map: &BTreeMap<(FileId, u32, u32), &QualifiedCall>,
    knowledge: Option<&KnowledgeProfile>,
    site: AllocationSite,
) -> bool {
    let Some(knowledge) = knowledge else {
        return false;
    };
    let Some(call) = call_map.get(&(site.file, site.start, site.end)) else {
        return false;
    };
    let mut resolved_any = false;
    for candidate in &call.candidates {
        let Some((canonical, _)) = resolve_candidate(knowledge, candidate) else {
            continue;
        };
        resolved_any = true;
        let method = canonical.rsplit('.').next().unwrap_or_default();
        if !COUNT_PRESERVING_POINT_WRITERS.contains(&method) {
            return false;
        }
    }
    resolved_any
}

/// Alias snapshot for ids that reach rule queries under a possibly
/// non-representative id (play targets, event payloads).
fn collect_aliases(scene: &SceneLifecycle) -> BTreeMap<ObjectId, ObjectId> {
    let heap = &scene.final_heap;
    let mut alias: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    let mut note_alias = |raw: &ObjectId| {
        let resolved = heap.resolve(raw);
        if &resolved != raw {
            alias.insert(raw.clone(), resolved);
        }
    };
    for play in &scene.plays {
        for animation in &play.animations {
            if let Some(target) = &animation.replacement_target {
                note_alias(target);
            }
            if let Some(state) = &animation.state {
                for target in &state.targets {
                    note_alias(target);
                }
            }
        }
    }
    for event in &scene.events {
        match &event.event {
            Event::AddChild(edge) => {
                note_alias(&edge.parent);
                note_alias(&edge.child);
            }
            Event::Mutate(mutate) => note_alias(&mutate.target),
            Event::UnknownMutation(mutation) => {
                for value in &mutation.values {
                    note_alias(value);
                }
            }
            _ => {}
        }
    }
    alias
}

/// Played animations that write points with a possible topology change
/// (Transform alignment inserts curves and pads families) void the
/// targets' counts from the play on. The play itself still queries the
/// pre-play sizes (strict poison comparison in `sizes_at`).
fn poison_played_topology_changes(
    scene: &SceneLifecycle,
    site_event: &BTreeMap<AllocationSite, usize>,
    family_poison: &mut BTreeMap<ObjectId, usize>,
    geometry_poison: &mut BTreeMap<ObjectId, usize>,
) {
    let heap = &scene.final_heap;
    for play in &scene.plays {
        let play_index = site_event.get(&play.site).copied().unwrap_or(0);
        for animation in &play.animations {
            let Some(state) = &animation.state else {
                continue;
            };
            if !state.write_channels.contains(&WriteChannel::Points)
                || state.topology_change == Truth::No
            {
                continue;
            }
            for target in &state.targets {
                let target = heap.resolve(target);
                poison(geometry_poison, target.clone(), play_index);
                poison(family_poison, target, play_index);
            }
        }
    }
}

/// Min-merges a poison index for a target.
fn poison(map: &mut BTreeMap<ObjectId, usize>, target: ObjectId, index: usize) {
    match map.get_mut(&target) {
        Some(existing) => *existing = (*existing).min(index),
        None => {
            map.insert(target, index);
        }
    }
}

fn resolve_scene(
    scene: &SceneLifecycle,
    call_map: &BTreeMap<(FileId, u32, u32), &QualifiedCall>,
    knowledge: Option<&KnowledgeProfile>,
) -> SceneSizes {
    let heap = &scene.final_heap;
    let mut site_event: BTreeMap<AllocationSite, usize> = BTreeMap::new();
    let mut mutation_targets_by_site: BTreeMap<AllocationSite, BTreeSet<ObjectId>> =
        BTreeMap::new();
    let alias = collect_aliases(scene);
    let mut definite_children: BTreeMap<ObjectId, BTreeSet<ObjectId>> = BTreeMap::new();
    let mut family_poison: BTreeMap<ObjectId, usize> = BTreeMap::new();
    let mut geometry_poison: BTreeMap<ObjectId, usize> = BTreeMap::new();

    for (index, event) in scene.events.iter().enumerate() {
        site_event.entry(event.site).or_insert(index);
        match &event.event {
            Event::AddChild(edge) => {
                if event.certainty == Presence::Present {
                    definite_children
                        .entry(heap.resolve(&edge.parent))
                        .or_default()
                        .insert(heap.resolve(&edge.child));
                }
            }
            Event::Mutate(mutate) => {
                let target = heap.resolve(&mutate.target);
                mutation_targets_by_site
                    .entry(event.site)
                    .or_default()
                    .insert(target.clone());
                match mutate.kind {
                    // `remove` / restructuring: the definite lower bound
                    // is gone; the possible upper bound stays in the heap.
                    MutationKind::Membership => {
                        definite_children.remove(&target);
                    }
                    // A points write that is not a curated affine
                    // transform may change counts (`become` also rewrites
                    // submobjects): void both fact groups from here on.
                    MutationKind::Points => {
                        if !site_preserves_counts(call_map, knowledge, event.site) {
                            poison(&mut geometry_poison, target.clone(), index);
                            poison(&mut family_poison, target, index);
                        }
                    }
                    // Path-construction methods (`set_points`,
                    // `add_line_to`, `clear_points`, ...) change how much
                    // geometry there is by definition — no curated guard
                    // can preserve counts — but they never touch the
                    // submobject family, so family facts survive.
                    MutationKind::PathTopology => {
                        poison(&mut geometry_poison, target, index);
                    }
                    MutationKind::Style
                    | MutationKind::Opacity
                    | MutationKind::CameraState
                    | MutationKind::Updaters
                    | MutationKind::Unknown => {}
                }
            }
            Event::UnknownMutation(mutation) => {
                for value in &mutation.values {
                    let target = heap.resolve(value);
                    mutation_targets_by_site
                        .entry(event.site)
                        .or_default()
                        .insert(target.clone());
                    poison(&mut geometry_poison, target.clone(), index);
                    poison(&mut family_poison, target, index);
                }
            }
            _ => {}
        }
    }

    poison_played_topology_changes(scene, &site_event, &mut family_poison, &mut geometry_poison);

    // Allocation sites (constructions, copies, generated targets).
    let mut objects_by_site: BTreeMap<AllocationSite, BTreeSet<ObjectId>> = BTreeMap::new();
    for id in heap.objects.keys() {
        objects_by_site
            .entry(id.site)
            .or_default()
            .insert(id.clone());
    }

    let resolver = Resolver {
        scene,
        call_map,
        definite_children: &definite_children,
        family_poison: &family_poison,
        geometry_poison: &geometry_poison,
        memo: BTreeMap::new(),
    };
    let records = resolver.resolve_all();

    SceneSizes {
        records,
        site_event,
        objects_by_site,
        mutation_targets_by_site,
        alias,
    }
}

struct Resolver<'a> {
    scene: &'a SceneLifecycle,
    call_map: &'a BTreeMap<(FileId, u32, u32), &'a QualifiedCall>,
    definite_children: &'a BTreeMap<ObjectId, BTreeSet<ObjectId>>,
    family_poison: &'a BTreeMap<ObjectId, usize>,
    geometry_poison: &'a BTreeMap<ObjectId, usize>,
    /// `None` marks "resolution in progress" (cycle sentinel).
    memo: BTreeMap<ObjectId, Option<SizeRecord>>,
}

impl Resolver<'_> {
    fn resolve_all(mut self) -> BTreeMap<ObjectId, SizeRecord> {
        let ids: Vec<ObjectId> = self.scene.final_heap.objects.keys().cloned().collect();
        for id in &ids {
            self.record_of(id);
        }
        self.memo
            .into_iter()
            .filter_map(|(id, record)| record.map(|record| (id, record)))
            .collect()
    }

    /// The size record of an alias-resolved object id. Cycles in the
    /// structural graph (invalid in Manim, `MLC110` territory) resolve to
    /// all-unknown records.
    fn record_of(&mut self, id: &ObjectId) -> SizeRecord {
        if let Some(memoized) = self.memo.get(id) {
            // `None` means this id is currently being resolved higher up
            // the stack: a structural cycle. Unknown, never a guess.
            return memoized.clone().unwrap_or_else(SizeRecord::unknown);
        }
        self.memo.insert(id.clone(), None);
        let record = self.compute(id);
        self.memo.insert(id.clone(), Some(record.clone()));
        record
    }

    fn compute(&mut self, id: &ObjectId) -> SizeRecord {
        let heap = &self.scene.final_heap;
        let Some(state) = heap.object(id) else {
            return SizeRecord::unknown();
        };

        // Copies mirror their original's structure and geometry: the
        // interpreter gives the copy a fresh, childless state, so the
        // heap graph must not be read as "family of 1". Without a
        // provenance edge the copy stays unknown.
        if state.copy_provenance.is_some() {
            let Some(provenance) = heap.copy_of.get(id) else {
                return SizeRecord::unknown();
            };
            let original = heap.resolve(&provenance.original);
            let mut record = self.record_of(&original);
            record.family_poison =
                min_opt(record.family_poison, self.family_poison.get(id).copied());
            record.geometry_poison = min_opt(
                record.geometry_poison,
                self.geometry_poison.get(id).copied(),
            );
            return record;
        }

        let (own_curves, own_points) = self.own_geometry(id, &state.kind);
        let mut family = Num::int(1);
        let mut curves = own_curves;
        let mut points = own_points;
        let mut family_poison = self.family_poison.get(id).copied();
        let mut geometry_poison = min_opt(
            self.geometry_poison.get(id).copied(),
            // A rewritten family rewrites the totals too.
            family_poison,
        );

        // Alias-resolved, deduplicated children (deterministic order).
        let children: BTreeSet<ObjectId> = state
            .children
            .iter()
            .map(|child| heap.resolve(child))
            .filter(|child| child != id)
            .collect();
        let definite = self.definite_children.get(id);
        for child in &children {
            let child_record = self.record_of(child);
            let definite_edge = definite.is_some_and(|set| set.contains(child));
            let multiplicity = edge_multiplicity(child.cardinality, definite_edge);
            family = family.add(&multiplicity.mul(&child_record.sizes.family));
            curves = curves.add(&multiplicity.mul(&child_record.sizes.curves));
            points = points.add(&multiplicity.mul(&child_record.sizes.points));
            family_poison = min_opt(family_poison, child_record.family_poison);
            geometry_poison = min_opt(
                geometry_poison,
                min_opt(child_record.geometry_poison, child_record.family_poison),
            );
        }

        SizeRecord {
            sizes: ObjectSizes {
                family,
                points,
                curves,
            },
            family_poison,
            geometry_poison,
        }
    }

    /// Own (non-family) curve / point counts from the curated geometry
    /// table, guarded by the constructor call shape at the allocation
    /// site. Content-dependent or uncurated kinds stay unknown.
    fn own_geometry(&self, id: &ObjectId, kind: &KindSet) -> (Num, Num) {
        let KindSet::Known(candidates) = kind else {
            return (Num::Unknown, Num::Unknown);
        };
        let call = self
            .call_map
            .get(&(id.site.file, id.site.start, id.site.end))
            .copied();
        let mut joined: Option<(Num, Num)> = None;
        for candidate in candidates {
            let Some(entry) = curated_geometry(candidate) else {
                return (Num::Unknown, Num::Unknown);
            };
            if !entry.guards_hold(call) {
                return (Num::Unknown, Num::Unknown);
            }
            let counts = (Num::int(entry.curves), Num::int(entry.points));
            joined = Some(match joined {
                None => counts,
                Some((curves, points)) => (curves.join(&counts.0), points.join(&counts.1)),
            });
        }
        joined.unwrap_or((Num::Unknown, Num::Unknown))
    }
}

/// Per-edge member multiplicity: how many runtime children one abstract
/// child id contributes through one structural edge.
///
/// A definite edge of a [`Cardinality::Singleton`] child is exactly one
/// member; loop-created ids widen to open-above intervals (`Many` proves
/// at least two); possible-only edges drop the lower bound to `0`.
fn edge_multiplicity(cardinality: Cardinality, definite_edge: bool) -> Num {
    let lower = if definite_edge {
        match cardinality {
            Cardinality::Singleton | Cardinality::MaybeMany => 1.0,
            Cardinality::Many => 2.0,
        }
    } else {
        0.0
    };
    match cardinality {
        Cardinality::Singleton => {
            if definite_edge {
                Num::int(1)
            } else {
                Num::Interval {
                    lo: Some(0.0),
                    hi: Some(1.0),
                }
            }
        }
        Cardinality::Many | Cardinality::MaybeMany => Num::Interval {
            lo: Some(lower),
            hi: None,
        },
    }
}
