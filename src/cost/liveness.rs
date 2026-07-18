//! Per-callback execution liveness: the plays / waits where a per-frame
//! callback **provably executes** once per rendered frame.
//!
//! A hot context (DESIGN §4.2) only says a call is *reachable* from a
//! per-frame entry point. Whether the callback actually runs during a
//! given play is a separate lifecycle question (DESIGN §3.2 / §3.3):
//!
//! - `Animation.begin()` **suspends** the animated mobject's updaters for
//!   the play (`animation.py` `begin` → `suspend_updating`, recursively
//!   over the target's family), unless the animation was constructed with
//!   `suspend_mobject_updating=False`;
//! - a mobject updater runs only while its **registration is active**
//!   (registered before, not removed) and its host is **in the scene
//!   family**;
//! - a `wait()` runs callbacks only when it is **dynamic** (`scene.py`
//!   `should_update_mobjects`): a one-argument updater alone leaves the
//!   default wait a static frame;
//! - `UpdateFromFunc`-style update functions and custom `interpolate*`
//!   overrides run only during plays that actually play their animation;
//! - a `stop_condition` callback runs during its own wait.
//!
//! Every verdict here is three-valued (DESIGN §15 invariants 2/9):
//! [`ExecutionCertainty::Proven`] requires every condition to hold on all
//! paths; anything unprovable is [`ExecutionCertainty::Maybe`]; a play
//! where the callback provably does *not* run (e.g. the host is the
//! suspended target of the play's own animation) is excluded entirely.
//! An entry that cannot be tied to the lifecycle facts at all yields
//! [`EntryLiveness::resolved`] `false` — "liveness unknown", never a
//! fabricated claim in either direction.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::semantic::events::{Event, MutationKind};
use crate::semantic::heap::AbstractHeap;
use crate::semantic::interpreter::{
    LifecycleFacts, PlayFact, PlayKind, SceneLifecycle, UpdaterHost,
};
use crate::semantic::state::{CallbackRef, SuspendBehavior};
use crate::semantic::values::{AllocationSite, KindSet, Num, ObjectId, Presence, Truth};

use super::contexts::{HotContext, HotEntryKind};

/// Identity of one hot entry root: the entry kind plus the callback /
/// override source span (the first provenance step).
pub type EntryKey = (HotEntryKind, AllocationSite);

/// The liveness key of a hot context, when it has an entry step.
#[must_use]
pub fn entry_key(context: &HotContext) -> Option<EntryKey> {
    let step = context.chain.first()?;
    Some((context.entry, AllocationSite::new(step.file, step.range)))
}

/// How certain one per-frame execution interval is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutionCertainty {
    /// The callback provably runs once per rendered frame of the play.
    Proven,
    /// The callback may run during the play; not provable either way.
    Maybe,
}

/// One play / wait during which a callback executes (or may execute) per
/// rendered frame.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionPlay {
    /// Qualified name of the scene the play belongs to.
    pub scene: String,
    /// Source site of the `play` / `wait` call.
    pub site: AllocationSite,
    /// Play vs wait.
    pub kind: PlayKind,
    /// The play's duration fact (seconds).
    pub duration: Num,
    /// Whether execution during this play is proven or only possible.
    pub certainty: ExecutionCertainty,
    /// Whether the play's full duration is a sound *lower* bound on the
    /// callback's per-frame executions. `false` for stop-condition waits,
    /// which may end after any frame; their duration only bounds above.
    pub full_duration_proven: bool,
}

/// Liveness verdict of one hot entry root.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryLiveness {
    /// The entry kind.
    pub entry: HotEntryKind,
    /// Source span of the callback / override (first provenance step).
    pub callback_site: AllocationSite,
    /// `true` when the entry was tied to the lifecycle facts (a matching
    /// registration, wait, or played animation was searched for across
    /// every analyzed scene). `false` means liveness is unknown: the
    /// callback identity or host could not be resolved, so no play list
    /// exists and callers must treat execution as *possible*, never as
    /// proven or as provably absent.
    pub resolved: bool,
    /// The execution plays, ascending by (scene, site).
    pub plays: Vec<ExecutionPlay>,
}

impl EntryLiveness {
    /// The proven execution plays.
    pub fn proven(&self) -> impl Iterator<Item = &ExecutionPlay> {
        self.plays
            .iter()
            .filter(|play| play.certainty == ExecutionCertainty::Proven)
    }

    /// The maybe execution plays.
    pub fn maybe(&self) -> impl Iterator<Item = &ExecutionPlay> {
        self.plays
            .iter()
            .filter(|play| play.certainty == ExecutionCertainty::Maybe)
    }

    /// Whether at least one play provably executes the callback.
    #[must_use]
    pub fn has_proven(&self) -> bool {
        self.proven().next().is_some()
    }
}

/// Resolves the liveness of every distinct hot entry root against the
/// lifecycle facts.
#[must_use]
pub fn resolve_liveness(
    hot: &super::contexts::HotContextFacts,
    lifecycle: &LifecycleFacts,
) -> BTreeMap<EntryKey, EntryLiveness> {
    // Distinct entry roots with their registering call and symbol.
    let mut entries: BTreeMap<EntryKey, (AllocationSite, Option<String>)> = BTreeMap::new();
    let contexts = hot
        .call_contexts
        .values()
        .flatten()
        .chain(hot.function_contexts.values().flatten());
    for context in contexts {
        let Some(key) = entry_key(context) else {
            continue;
        };
        let symbol = context.chain.first().and_then(|step| step.symbol.clone());
        entries.entry(key).or_insert((context.entry_call, symbol));
    }
    entries
        .into_iter()
        .map(|((entry, callback_site), (entry_call, symbol))| {
            let liveness = resolve_entry(
                entry,
                callback_site,
                entry_call,
                symbol.as_deref(),
                lifecycle,
            );
            ((entry, callback_site), liveness)
        })
        .collect()
}

/// Resolves one entry root.
fn resolve_entry(
    entry: HotEntryKind,
    callback_site: AllocationSite,
    entry_call: AllocationSite,
    symbol: Option<&str>,
    lifecycle: &LifecycleFacts,
) -> EntryLiveness {
    let callback = symbol.map_or(CallbackRef::Lambda(callback_site), |name| {
        CallbackRef::Named(name.to_owned())
    });
    let (resolved, plays) = match entry {
        HotEntryKind::MobjectUpdater
        | HotEntryKind::AlwaysRedraw
        | HotEntryKind::TracedPointFunc => mobject_updater_plays(&callback, lifecycle),
        HotEntryKind::SceneUpdater => scene_updater_plays(&callback, lifecycle),
        HotEntryKind::StopCondition => stop_condition_plays(entry_call, lifecycle),
        HotEntryKind::UpdateFunctionAnimation => played_animation_plays(entry_call, lifecycle),
        HotEntryKind::AnimationInterpolateOverride => interpolate_override_plays(symbol, lifecycle),
    };
    EntryLiveness {
        entry,
        callback_site,
        resolved,
        plays,
    }
}

/// Suspension verdict of one host during one play (DESIGN §3.2 step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Suspension {
    /// Provably not suspended.
    None,
    /// Possibly suspended (unknown animation, maybe-target, family
    /// descendant of a target, or unknown suspend behavior).
    Possible,
    /// Provably suspended: the host is definitely a live target of a
    /// definitely-suspending animation of this play.
    Certain,
}

/// Whether `host` may sit inside the family (submobject closure) of
/// `root` under `heap`'s may-children edges. Containment through a
/// may-set is never certain, so the answer is `No` or `Maybe`.
fn may_contain(heap: &AbstractHeap, root: &ObjectId, host: &ObjectId) -> Truth {
    let needle = heap.resolve(host);
    let mut queue: VecDeque<ObjectId> = VecDeque::from([heap.resolve(root)]);
    let mut visited: BTreeSet<ObjectId> = BTreeSet::new();
    let mut result = Truth::No;
    while let Some(current) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        let Some(state) = heap.object(&current) else {
            continue;
        };
        for child in &state.children {
            let child = heap.resolve(child);
            if child.may_be_same(&needle) != Truth::No {
                result = Truth::Maybe;
            }
            queue.push_back(child);
        }
    }
    result
}

/// Suspension of `host`'s updaters during `play`, judged on the pre-play
/// heap. `Animation.begin` suspends the animated target's whole family
/// (`suspend_updating(recursive=True)`); `suspend_mobject_updating=False`
/// leaves updaters running (curated / literal kwarg,
/// [`SuspendBehavior::LeavesUpdatersRunning`]).
fn suspension_during(play: &PlayFact, heap: &AbstractHeap, host: &ObjectId) -> Suspension {
    let mut verdict = if play.star_args {
        // Untracked splat animations may target (and suspend) anything.
        Suspension::Possible
    } else {
        Suspension::None
    };
    for played in &play.animations {
        let Some(state) = &played.state else {
            // Unknown animation value: it may target the host.
            verdict = verdict.max(Suspension::Possible);
            continue;
        };
        if state.suspend == SuspendBehavior::LeavesUpdatersRunning {
            continue;
        }
        let mut direct = Truth::No;
        let mut descendant = Truth::No;
        for target in &state.targets {
            let target = heap.resolve(target);
            direct = strongest(direct, target.may_be_same(&heap.resolve(host)));
            descendant = strongest(descendant, may_contain(heap, &target, host));
        }
        match (state.suspend, direct) {
            (SuspendBehavior::SuspendsLiveTargets, Truth::Yes) => return Suspension::Certain,
            (_, Truth::Yes | Truth::Maybe) => verdict = verdict.max(Suspension::Possible),
            (_, Truth::No) => {
                if descendant != Truth::No {
                    verdict = verdict.max(Suspension::Possible);
                }
            }
        }
    }
    verdict
}

/// The stronger of two may-truths (`Yes` > `Maybe` > `No`).
const fn strongest(a: Truth, b: Truth) -> Truth {
    match (a, b) {
        (Truth::Yes, _) | (_, Truth::Yes) => Truth::Yes,
        (Truth::Maybe, _) | (_, Truth::Maybe) => Truth::Maybe,
        (Truth::No, Truth::No) => Truth::No,
    }
}

/// Whether a play invokes per-frame callbacks at all (DESIGN §3.3): a
/// `play` always runs the frame grid; a wait only when it is dynamic
/// (or forced un-frozen), and `frozen_frame=True` freezes regardless.
fn runs_callbacks(play: &PlayFact) -> Truth {
    match play.kind {
        PlayKind::Play => Truth::Yes,
        PlayKind::Wait => match play.frozen_frame {
            Some(true) => Truth::No,
            Some(false) => Truth::Yes,
            None => play.dynamic_wait,
        },
    }
}

/// Whether a matched registration may have been removed at *some* point
/// the pre-play snapshot cannot rule out: a `remove_updater` whose
/// identity may match (`matched != No`) on a branch-dependent path, or a
/// branch-dependent `clear_updaters` (an `Updaters` mutation event with
/// no removal record), or the host escaping into an unresolved call.
/// All-paths matched removals need no flag — the registered-updater set
/// at the pre-play snapshot already reflects them.
fn may_removed(scene: &SceneLifecycle, host: Option<&ObjectId>, callback: &CallbackRef) -> bool {
    let heap = &scene.final_heap;
    let removal_sites: BTreeSet<AllocationSite> = scene
        .updater_removals
        .iter()
        .map(|removal| removal.site)
        .collect();
    for removal in &scene.updater_removals {
        let host_compatible = match (&removal.host, host) {
            (UpdaterHost::Scene, None) => true,
            (UpdaterHost::Mobject(removed), Some(host)) => {
                heap.resolve(removed).may_be_same(&heap.resolve(host)) != Truth::No
            }
            (UpdaterHost::Scene, Some(_)) | (UpdaterHost::Mobject(_), None) => false,
        };
        if !host_compatible {
            continue;
        }
        let callback_compatible =
            matches!(removal.callback, CallbackRef::Unknown) || &removal.callback == callback;
        if !callback_compatible {
            continue;
        }
        match removal.matched {
            Truth::No => {}
            Truth::Maybe => return true,
            Truth::Yes => {
                if removal.certainty != Presence::Present {
                    return true;
                }
            }
        }
    }
    if let Some(host) = host {
        let resolved_host = heap.resolve(host);
        for event in &scene.events {
            match &event.event {
                Event::Mutate(mutate) if mutate.kind == MutationKind::Updaters => {
                    // `clear_updaters` leaves no removal record; a
                    // branch-dependent clear keeps the may-set intact.
                    if removal_sites.contains(&event.site) || event.certainty == Presence::Present {
                        continue;
                    }
                    if heap.resolve(&mutate.target).may_be_same(&resolved_host) != Truth::No {
                        return true;
                    }
                }
                Event::UnknownMutation(unknown) => {
                    // The host escaped into an unresolved call, which may
                    // remove or suspend its updaters at runtime.
                    if unknown
                        .values
                        .iter()
                        .any(|value| heap.resolve(value).may_be_same(&resolved_host) != Truth::No)
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

/// Play accumulator keyed by (scene, play site); a proven verdict wins
/// over a maybe verdict for the same play.
type PlayMap = BTreeMap<(String, AllocationSite), ExecutionPlay>;

fn record_play(
    plays: &mut PlayMap,
    scene: &SceneLifecycle,
    site: AllocationSite,
    kind: PlayKind,
    duration: Num,
    proven: bool,
    full_duration_proven: bool,
) {
    let key = (scene.qualified_name.clone(), site);
    let certainty = if proven {
        ExecutionCertainty::Proven
    } else {
        ExecutionCertainty::Maybe
    };
    match plays.get(&key) {
        Some(existing) if existing.certainty <= certainty => {}
        _ => {
            plays.insert(
                key,
                ExecutionPlay {
                    scene: scene.qualified_name.clone(),
                    site,
                    kind,
                    duration,
                    certainty,
                    full_duration_proven,
                },
            );
        }
    }
}

fn finish(plays: PlayMap, resolved: bool) -> (bool, Vec<ExecutionPlay>) {
    (resolved, plays.into_values().collect())
}

/// Liveness of a mobject-host updater callback (`Mobject.add_updater`,
/// `always_redraw`, traced-point funcs): registration active at the play,
/// host in the scene family, host not suspended, and the play actually
/// runs callbacks.
fn mobject_updater_plays(
    callback: &CallbackRef,
    lifecycle: &LifecycleFacts,
) -> (bool, Vec<ExecutionPlay>) {
    let mut plays = PlayMap::new();
    let mut resolved = false;
    for scene in &lifecycle.scenes {
        for registration in &scene.updaters {
            if &registration.fact.callback != callback {
                continue;
            }
            let UpdaterHost::Mobject(host) = &registration.host else {
                continue;
            };
            resolved = true;
            let removed = may_removed(scene, Some(host), callback);
            for play in &scene.plays {
                let runs = runs_callbacks(play);
                if runs == Truth::No {
                    continue;
                }
                let Some(snapshot) = scene.state_at(play.site.file, play.site.start) else {
                    continue;
                };
                let heap = &snapshot.heap;
                let resolved_host = heap.resolve(host);
                let Some(object) = heap.object(&resolved_host) else {
                    continue;
                };
                // Registration active: the registered-updater may-set at
                // the pre-play snapshot still carries this fact (all-path
                // removals before the play have already dropped it).
                if !object.updaters.contains(&registration.fact) {
                    continue;
                }
                let membership = object.family_membership;
                if membership == Presence::Absent {
                    continue;
                }
                let suspension = suspension_during(play, heap, &resolved_host);
                if suspension == Suspension::Certain {
                    continue;
                }
                let proven = play.certainty == Presence::Present
                    && registration.certainty == Presence::Present
                    && runs == Truth::Yes
                    && membership == Presence::Present
                    && suspension == Suspension::None
                    && !removed;
                record_play(
                    &mut plays,
                    scene,
                    play.site,
                    play.kind,
                    play.duration.clone(),
                    proven,
                    !play.has_stop_condition,
                );
            }
        }
    }
    finish(plays, resolved)
}

/// Liveness of a `Scene.add_updater` callback: scene updaters run in
/// every play and make waits dynamic; they are never suspended by
/// animations (DESIGN §3.3).
fn scene_updater_plays(
    callback: &CallbackRef,
    lifecycle: &LifecycleFacts,
) -> (bool, Vec<ExecutionPlay>) {
    let mut plays = PlayMap::new();
    let mut resolved = false;
    for scene in &lifecycle.scenes {
        for registration in &scene.updaters {
            if &registration.fact.callback != callback || registration.host != UpdaterHost::Scene {
                continue;
            }
            resolved = true;
            let removed = may_removed(scene, None, callback);
            for play in &scene.plays {
                let runs = runs_callbacks(play);
                if runs == Truth::No {
                    continue;
                }
                let Some(snapshot) = scene.state_at(play.site.file, play.site.start) else {
                    continue;
                };
                let registered = snapshot
                    .heap
                    .scenes
                    .get(&scene.scene_id)
                    .is_some_and(|state| state.scene_updaters.contains(&registration.fact));
                if !registered {
                    continue;
                }
                let proven = play.certainty == Presence::Present
                    && registration.certainty == Presence::Present
                    && runs == Truth::Yes
                    && !removed;
                record_play(
                    &mut plays,
                    scene,
                    play.site,
                    play.kind,
                    play.duration.clone(),
                    proven,
                    !play.has_stop_condition,
                );
            }
        }
    }
    finish(plays, resolved)
}

/// Liveness of a `stop_condition` callback: it runs once per frame of
/// its own wait, which may end after any frame (upper bound only).
fn stop_condition_plays(
    entry_call: AllocationSite,
    lifecycle: &LifecycleFacts,
) -> (bool, Vec<ExecutionPlay>) {
    let mut plays = PlayMap::new();
    let mut resolved = false;
    for scene in &lifecycle.scenes {
        for play in &scene.plays {
            if play.site != entry_call || !play.has_stop_condition {
                continue;
            }
            resolved = true;
            record_play(
                &mut plays,
                scene,
                play.site,
                play.kind,
                play.duration.clone(),
                play.certainty == Presence::Present,
                false,
            );
        }
    }
    finish(plays, resolved)
}

/// Liveness of an `UpdateFromFunc` / `UpdateFromAlphaFunc` update
/// function: it runs once per frame of every play that plays the
/// animation constructed at `entry_call`, for that animation's own run
/// time.
fn played_animation_plays(
    entry_call: AllocationSite,
    lifecycle: &LifecycleFacts,
) -> (bool, Vec<ExecutionPlay>) {
    let mut plays = PlayMap::new();
    let mut resolved = false;
    for scene in &lifecycle.scenes {
        for play in &scene.plays {
            if play.kind != PlayKind::Play {
                continue;
            }
            for played in &play.animations {
                let matches = played
                    .animation
                    .as_ref()
                    .is_some_and(|id| id.site == entry_call);
                if !matches {
                    continue;
                }
                resolved = true;
                let duration = played
                    .state
                    .as_ref()
                    .map_or(Num::Unknown, |state| state.run_time.clone());
                record_play(
                    &mut plays,
                    scene,
                    play.site,
                    play.kind,
                    duration,
                    play.certainty == Presence::Present,
                    true,
                );
            }
        }
    }
    finish(plays, resolved)
}

/// Liveness of a project `interpolate` / `interpolate_mobject` /
/// `interpolate_submobject` override: it runs once per frame of every
/// play that plays an animation of that class. Plays with untracked
/// animation values may be playing it (maybe evidence).
fn interpolate_override_plays(
    symbol: Option<&str>,
    lifecycle: &LifecycleFacts,
) -> (bool, Vec<ExecutionPlay>) {
    let Some(class_id) = symbol.and_then(|name| name.rsplit_once('.').map(|(class, _)| class))
    else {
        return (false, Vec::new());
    };
    let mut plays = PlayMap::new();
    for scene in &lifecycle.scenes {
        for play in &scene.plays {
            if play.kind != PlayKind::Play {
                continue;
            }
            for played in &play.animations {
                let Some(state) = &played.state else {
                    if played.convertible != Truth::No {
                        record_play(
                            &mut plays,
                            scene,
                            play.site,
                            play.kind,
                            play.duration.clone(),
                            false,
                            true,
                        );
                    }
                    continue;
                };
                let (may_be, only) = match &state.kind {
                    KindSet::Known(kinds) => {
                        (kinds.iter().any(|kind| kind == class_id), kinds.len() == 1)
                    }
                    KindSet::Unknown => (true, false),
                };
                if !may_be {
                    continue;
                }
                record_play(
                    &mut plays,
                    scene,
                    play.site,
                    play.kind,
                    state.run_time.clone(),
                    only && play.certainty == Presence::Present,
                    true,
                );
            }
        }
    }
    // The override itself is a definite fact and every analyzed play was
    // searched; an empty list means no analyzed play can run it.
    finish(plays, true)
}
