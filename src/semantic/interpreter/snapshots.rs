//! Statement-boundary state snapshots and the per-statement queries of
//! [`SceneLifecycle`]: scope-restricted site-order lookup
//! ([`SceneLifecycle::state_at`]), membership and own-path state at a
//! program point, and per-statement ownership intervals (MLC111).

use std::collections::BTreeMap;

use crate::semantic::heap::AbstractHeap;
use crate::semantic::values::{AllocationSite, Num, ObjectId, Presence, Truth};
use crate::source::FileId;

use super::SceneLifecycle;

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
    /// Exact local / `self.<attr>` bindings to tracked mobjects at this
    /// statement. Disagreeing branch bindings are omitted by the abstract
    /// environment join. Callback-dependency rules use this to resolve a
    /// lexical closure read at the frame site, never at registration time.
    pub object_bindings: BTreeMap<String, ObjectId>,
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
