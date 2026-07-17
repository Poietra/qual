//! Abstract heap: object states, alias classes, copy provenance, and scene
//! states (DESIGN §5.5).
//!
//! Aliasing rule: alias classes may only merge
//! [`Cardinality::Singleton`] ids. `Many` / `MaybeMany` ids always stay
//! distinct, so two loop allocations are never assumed identical.

use std::collections::{BTreeMap, BTreeSet};

use crate::semantic::state::{MobjectState, SceneState};
use crate::semantic::values::{Cardinality, CopyOf, ObjectId, Presence};

/// The abstract heap of one analysis state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AbstractHeap {
    /// Mobject states keyed by their (representative) object id.
    pub objects: BTreeMap<ObjectId, MobjectState>,
    /// Union-find parent edges of the alias classes. Only singleton ids
    /// appear here; the class representative is the smallest member.
    alias_parent: BTreeMap<ObjectId, ObjectId>,
    /// Copy provenance edges: copy id → original.
    pub copy_of: BTreeMap<ObjectId, CopyOf>,
    /// Scene states keyed by the scene object's id.
    pub scenes: BTreeMap<ObjectId, SceneState>,
}

impl AbstractHeap {
    /// An empty heap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The representative id of `id`'s alias class (itself when unaliased).
    #[must_use]
    pub fn resolve(&self, id: &ObjectId) -> ObjectId {
        let mut current = id;
        while let Some(parent) = self.alias_parent.get(current) {
            current = parent;
        }
        current.clone()
    }

    /// Whether the two ids are in the same alias class.
    #[must_use]
    pub fn are_aliased(&self, a: &ObjectId, b: &ObjectId) -> bool {
        self.resolve(a) == self.resolve(b)
    }

    /// Records a fresh object state under `id`.
    ///
    /// An existing state for the same id (e.g. a second loop iteration) is
    /// joined rather than overwritten.
    pub fn insert_object(&mut self, id: ObjectId, state: MobjectState) {
        let key = if self.alias_parent.contains_key(&id) {
            self.resolve(&id)
        } else {
            id
        };
        match self.objects.get(&key) {
            Some(existing) => {
                let joined = existing.join(&state);
                self.objects.insert(key, joined);
            }
            None => {
                self.objects.insert(key, state);
            }
        }
    }

    /// The state of `id`, looked up through its alias class.
    #[must_use]
    pub fn object(&self, id: &ObjectId) -> Option<&MobjectState> {
        self.objects.get(&self.resolve(id))
    }

    /// Mutable state of `id`, looked up through its alias class.
    pub fn object_mut(&mut self, id: &ObjectId) -> Option<&mut MobjectState> {
        let key = self.resolve(id);
        self.objects.get_mut(&key)
    }

    /// Effective scene membership of `id` ([`Presence::Absent`] for ids the
    /// heap does not know).
    #[must_use]
    pub fn membership(&self, id: &ObjectId) -> Presence {
        self.object(id)
            .map_or(Presence::Absent, |state| state.family_membership)
    }

    /// Records `copy` as a copy of `provenance.original`.
    pub fn record_copy(&mut self, copy: ObjectId, provenance: CopyOf) {
        self.copy_of.insert(copy, provenance);
    }

    /// Records a scene state, joining with any existing one.
    pub fn insert_scene(&mut self, id: ObjectId, state: SceneState) {
        match self.scenes.get(&id) {
            Some(existing) => {
                let joined = existing.join(&state);
                self.scenes.insert(id, joined);
            }
            None => {
                self.scenes.insert(id, state);
            }
        }
    }

    /// The scene state for `id`.
    #[must_use]
    pub fn scene(&self, id: &ObjectId) -> Option<&SceneState> {
        self.scenes.get(id)
    }

    /// Tries to merge the alias classes of `a` and `b`.
    ///
    /// Returns `true` when the ids are now (or already were) aliased. Only
    /// [`Cardinality::Singleton`] ids may merge (DESIGN §5.5); for any other
    /// cardinality the classes stay distinct and `false` is returned.
    pub fn try_alias(&mut self, a: &ObjectId, b: &ObjectId) -> bool {
        let rep_a = self.resolve(a);
        let rep_b = self.resolve(b);
        if rep_a == rep_b {
            return true;
        }
        if a.cardinality != Cardinality::Singleton
            || b.cardinality != Cardinality::Singleton
            || rep_a.cardinality != Cardinality::Singleton
            || rep_b.cardinality != Cardinality::Singleton
        {
            return false;
        }
        let (winner, loser) = if rep_a <= rep_b {
            (rep_a, rep_b)
        } else {
            (rep_b, rep_a)
        };
        // Both entries describe the same runtime object; keep the join
        // under the representative.
        if let Some(loser_state) = self.objects.remove(&loser) {
            match self.objects.get(&winner) {
                Some(winner_state) => {
                    let joined = winner_state.join(&loser_state);
                    self.objects.insert(winner.clone(), joined);
                }
                None => {
                    self.objects.insert(winner.clone(), loser_state);
                }
            }
        }
        self.alias_parent.insert(loser, winner);
        true
    }

    /// All ids known to either side (object keys and alias-map members).
    fn known_ids(a: &Self, b: &Self) -> BTreeSet<ObjectId> {
        let mut ids = BTreeSet::new();
        for heap in [a, b] {
            ids.extend(heap.objects.keys().cloned());
            for (child, parent) in &heap.alias_parent {
                ids.insert(child.clone());
                ids.insert(parent.clone());
            }
        }
        ids
    }

    /// Rebuilds the alias classes kept by both sides: two ids stay aliased
    /// only when *both* heaps agree they co-resolve.
    fn intersect_aliases(
        a: &Self,
        b: &Self,
        ids: &BTreeSet<ObjectId>,
    ) -> BTreeMap<ObjectId, ObjectId> {
        let mut groups: BTreeMap<(ObjectId, ObjectId), Vec<ObjectId>> = BTreeMap::new();
        for id in ids {
            let key = (a.resolve(id), b.resolve(id));
            groups.entry(key).or_default().push(id.clone());
        }
        let mut alias_parent = BTreeMap::new();
        for members in groups.into_values() {
            // `members` is sorted (BTreeSet iteration order); the smallest
            // member is the representative.
            let mut iter = members.into_iter();
            if let Some(representative) = iter.next() {
                for member in iter {
                    alias_parent.insert(member, representative.clone());
                }
            }
        }
        alias_parent
    }

    /// Least upper bound of two branch heaps.
    ///
    /// Object states present in both branches are joined field-wise; a
    /// state allocated in only one branch has its existence-dependent facts
    /// weakened to maybe-facts, so branch-dependent membership becomes
    /// [`Presence::Maybe`], never a wrong certain value.
    #[must_use]
    pub fn join(a: &Self, b: &Self) -> Self {
        let ids = Self::known_ids(a, b);
        let alias_parent = Self::intersect_aliases(a, b, &ids);

        let mut result = Self {
            objects: BTreeMap::new(),
            alias_parent,
            copy_of: BTreeMap::new(),
            scenes: BTreeMap::new(),
        };

        for id in &ids {
            let representative = result.resolve(id);
            if result.objects.contains_key(&representative) {
                continue;
            }
            let state = match (a.object(id), b.object(id)) {
                (Some(state_a), Some(state_b)) => Some(state_a.join(state_b)),
                (Some(only), None) | (None, Some(only)) => Some(only.weaken_for_missing_branch()),
                (None, None) => None,
            };
            if let Some(state) = state {
                result.objects.insert(representative, state);
            }
        }

        // Provenance is per-id constant (copies always get fresh ids), so
        // the union of both edge sets is sound.
        result.copy_of = a.copy_of.clone();
        for (id, provenance) in &b.copy_of {
            result
                .copy_of
                .entry(id.clone())
                .or_insert_with(|| provenance.clone());
        }

        for id in a.scenes.keys().chain(b.scenes.keys()) {
            if result.scenes.contains_key(id) {
                continue;
            }
            let state = match (a.scenes.get(id), b.scenes.get(id)) {
                (Some(state_a), Some(state_b)) => state_a.join(state_b),
                (Some(only), None) | (None, Some(only)) => only.clone(),
                (None, None) => unreachable!("id comes from one of the key sets"),
            };
            result.scenes.insert(id.clone(), state);
        }

        result
    }

    /// Widening for loop fixpoints: like [`AbstractHeap::join`], but state
    /// facts that keep changing between iterates are widened (intervals
    /// open, candidate sets cap out). No new aliases appear in a widening.
    #[must_use]
    pub fn widen(previous: &Self, next: &Self) -> Self {
        let mut result = Self::join(previous, next);
        for (id, state) in &mut result.objects {
            if let (Some(prev_state), Some(next_state)) = (previous.object(id), next.object(id)) {
                *state = prev_state.widen(next_state);
            }
        }
        for (id, state) in &mut result.scenes {
            if let (Some(prev_state), Some(next_state)) =
                (previous.scenes.get(id), next.scenes.get(id))
            {
                *state = prev_state.widen(next_state);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::state::OrderedIdList;
    use crate::semantic::values::{
        AllocationSite, CallContextId, CopyKind, KindSet, Truth, Visibility,
    };
    use crate::source::SourceManager;

    fn object(start: u32, cardinality: Cardinality) -> ObjectId {
        let mut sources = SourceManager::new(".");
        let file = sources.load_bytes(std::path::Path::new("test.py"), b"pass\n");
        ObjectId::new(
            AllocationSite {
                file,
                start,
                end: start + 1,
            },
            CallContextId::empty(),
            cardinality,
        )
    }

    fn square() -> MobjectState {
        MobjectState::fresh(KindSet::single("manim.Square"))
    }

    #[test]
    fn alias_merges_singletons_only() {
        let mut heap = AbstractHeap::new();
        let a = object(1, Cardinality::Singleton);
        let b = object(2, Cardinality::Singleton);
        heap.insert_object(a.clone(), square());
        heap.insert_object(b.clone(), square());

        assert!(heap.try_alias(&a, &b));
        assert!(heap.are_aliased(&a, &b));
        // The merged class has one state entry under the representative.
        assert_eq!(heap.objects.len(), 1);
        assert!(heap.object(&b).is_some());

        // Many / MaybeMany ids never merge.
        let many_a = object(3, Cardinality::Many);
        let many_b = object(4, Cardinality::Many);
        assert!(!heap.try_alias(&many_a, &many_b));
        assert!(!heap.are_aliased(&many_a, &many_b));
        let maybe = object(5, Cardinality::MaybeMany);
        assert!(!heap.try_alias(&a, &maybe));

        // Aliasing an id with itself is a no-op success.
        assert!(heap.try_alias(&a, &a.clone()));
    }

    #[test]
    fn alias_lookup_joins_states_under_representative() {
        let mut heap = AbstractHeap::new();
        let a = object(1, Cardinality::Singleton);
        let b = object(2, Cardinality::Singleton);
        let mut visible = square();
        visible.visibility = Visibility::Visible;
        heap.insert_object(a.clone(), square());
        heap.insert_object(b.clone(), visible);

        heap.try_alias(&a, &b);
        // The two records disagreed on visibility, so the merged object is
        // maybe-visible.
        assert_eq!(heap.object(&a).unwrap().visibility, Visibility::Maybe);
        assert_eq!(heap.object(&b).unwrap().visibility, Visibility::Maybe);
    }

    #[test]
    fn join_produces_maybe_membership_when_branches_disagree() {
        let id = object(1, Cardinality::Singleton);

        // Branch A adds the object to the scene; branch B does not.
        let mut branch_a = AbstractHeap::new();
        let mut added = square();
        added.scene_root_membership = Presence::Present;
        added.family_membership = Presence::Present;
        added.visibility = Visibility::Visible;
        branch_a.insert_object(id.clone(), added);

        let mut branch_b = AbstractHeap::new();
        branch_b.insert_object(id.clone(), square());

        let joined = AbstractHeap::join(&branch_a, &branch_b);
        let state = joined.object(&id).unwrap();
        assert_eq!(state.scene_root_membership, Presence::Maybe);
        assert_eq!(state.family_membership, Presence::Maybe);
        assert_eq!(state.visibility, Visibility::Maybe);
        assert_eq!(joined.membership(&id), Presence::Maybe);
    }

    #[test]
    fn join_weakens_objects_allocated_in_one_branch_only() {
        let id = object(7, Cardinality::Singleton);
        let mut branch_a = AbstractHeap::new();
        let mut added = square();
        added.family_membership = Presence::Present;
        added.visibility = Visibility::Visible;
        added.foreground = Truth::Yes;
        branch_a.insert_object(id.clone(), added);

        let branch_b = AbstractHeap::new();
        let joined = AbstractHeap::join(&branch_a, &branch_b);
        let state = joined.object(&id).unwrap();
        assert_eq!(state.family_membership, Presence::Maybe);
        assert_eq!(state.visibility, Visibility::Maybe);
        assert_eq!(state.foreground, Truth::Maybe);
        // Definite negatives stay definite: absent in one branch and
        // nonexistent in the other is still absent.
        assert_eq!(state.scene_root_membership, Presence::Absent);
    }

    #[test]
    fn join_keeps_aliases_only_when_both_branches_agree() {
        let a = object(1, Cardinality::Singleton);
        let b = object(2, Cardinality::Singleton);
        let c = object(3, Cardinality::Singleton);

        let mut branch_a = AbstractHeap::new();
        for id in [&a, &b, &c] {
            branch_a.insert_object(id.clone(), square());
        }
        let mut branch_b = branch_a.clone();

        // Both branches alias a=b; only branch A also aliases b=c.
        assert!(branch_a.try_alias(&a, &b));
        assert!(branch_a.try_alias(&b, &c));
        assert!(branch_b.try_alias(&a, &b));

        let joined = AbstractHeap::join(&branch_a, &branch_b);
        assert!(joined.are_aliased(&a, &b));
        assert!(!joined.are_aliased(&b, &c));
        assert!(!joined.are_aliased(&a, &c));
    }

    #[test]
    fn copy_edges_survive_join() {
        let original = object(1, Cardinality::Singleton);
        let copy = object(2, Cardinality::Singleton);

        let mut branch_a = AbstractHeap::new();
        branch_a.insert_object(original.clone(), square());
        branch_a.insert_object(copy.clone(), square());
        branch_a.record_copy(
            copy.clone(),
            CopyOf {
                original: original.clone(),
                kind: CopyKind::GenerateTarget,
            },
        );
        let branch_b = AbstractHeap::new();

        let joined = AbstractHeap::join(&branch_a, &branch_b);
        assert_eq!(
            joined.copy_of.get(&copy).map(|edge| edge.kind),
            Some(CopyKind::GenerateTarget)
        );
        // A copy is never aliased with its original.
        assert!(!joined.are_aliased(&copy, &original));
    }

    #[test]
    fn scene_join_and_widen_delegate_to_state() {
        let scene = object(100, Cardinality::Singleton);
        let root = object(1, Cardinality::Singleton);

        let mut branch_a = AbstractHeap::new();
        let mut state_a = SceneState::initial(KindSet::single("manim.Scene"));
        state_a.roots = OrderedIdList {
            items: vec![root.clone()],
            order_known: Truth::Yes,
        };
        branch_a.insert_scene(scene.clone(), state_a);

        let mut branch_b = AbstractHeap::new();
        branch_b.insert_scene(
            scene.clone(),
            SceneState::initial(KindSet::single("manim.Scene")),
        );

        let joined = AbstractHeap::join(&branch_a, &branch_b);
        let scene_state = joined.scene(&scene).unwrap();
        assert_eq!(scene_state.roots.order_known, Truth::Maybe);
        assert!(scene_state.roots.contains(&root));
    }

    #[test]
    fn widen_opens_growing_object_intervals() {
        let id = object(1, Cardinality::Singleton);
        let mut previous = AbstractHeap::new();
        let mut state = square();
        state.family_size = crate::semantic::values::Num::int(2);
        previous.insert_object(id.clone(), state);

        let mut next = AbstractHeap::new();
        let mut grown = square();
        grown.family_size = crate::semantic::values::Num::int(4);
        next.insert_object(id.clone(), grown);

        let widened = AbstractHeap::widen(&previous, &next);
        assert_eq!(
            widened.object(&id).unwrap().family_size,
            crate::semantic::values::Num::Interval {
                lo: Some(2.0),
                hi: None,
            }
        );
    }

    #[test]
    fn join_is_commutative() {
        let id = object(1, Cardinality::Singleton);
        let other = object(2, Cardinality::Singleton);
        let mut branch_a = AbstractHeap::new();
        let mut member = square();
        member.family_membership = Presence::Present;
        branch_a.insert_object(id.clone(), member);
        let mut branch_b = AbstractHeap::new();
        branch_b.insert_object(id, square());
        branch_b.insert_object(other, square());

        assert_eq!(
            AbstractHeap::join(&branch_a, &branch_b),
            AbstractHeap::join(&branch_b, &branch_a)
        );
    }
}
