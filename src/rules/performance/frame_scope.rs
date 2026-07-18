//! Shared fact resolution for the timing / display-order performance
//! tranche (`MLP209` / `MLP215` / `MLP218` / `MLP219` / `MLP222` /
//! `MLP227`).
//!
//! Every helper is conservative (DESIGN §15): a proof either holds on
//! all paths from definite facts, or the helper answers "not proven" and
//! the calling rule stays silent.

use std::collections::BTreeSet;

use rustpython_parser::text_size::{TextRange, TextSize};

use crate::config::model::Renderer;
use crate::cost::estimator::frames_across_profiles;
use crate::diagnostic::RelatedLocation;
use crate::knowledge::{KnowledgeProfile, SymbolKind};
use crate::rules::base::RuleContext;
use crate::semantic::interpreter::{
    CameraKind, PlayFact, PlayKind, SceneLifecycle, StateSnapshot, UpdaterHost, UpdaterRegistration,
};
use crate::semantic::values::{KindSet, Num, Presence, Truth};

/// Whether the camera can move the whole scene into the moving scope
/// (`MovingScope::camera_moving`, DESIGN §3.4): a plain `Scene` camera
/// provably cannot; every other (or unresolved) camera contract is a
/// maybe — never a guessed `No`.
pub(crate) fn camera_truth(kind: CameraKind) -> Truth {
    match kind {
        CameraKind::Standard => Truth::No,
        CameraKind::MovingCamera | CameraKind::ThreeD | CameraKind::Unknown => Truth::Maybe,
    }
}

/// The byte range of a lifecycle site.
pub(crate) fn site_range(start: u32, end: u32) -> TextRange {
    TextRange::new(TextSize::from(start), TextSize::from(end))
}

/// Whether the play renders per-frame work at all: a `play` always does;
/// a wait only when provably dynamic (a frozen wait re-renders nothing,
/// DESIGN §3.3).
pub(crate) fn renders_frames(play: &PlayFact) -> bool {
    play.kind == PlayKind::Play || play.dynamic_wait == Truth::Yes
}

/// Names of the active Cairo profiles. Renderer-specific rules restrict
/// `applicable_profiles` to matching renderers (DESIGN §15.8) and stay
/// silent when the run targets none.
pub(crate) fn cairo_profile_names(context: &RuleContext<'_>) -> Vec<String> {
    context
        .active_profiles()
        .iter()
        .filter(|profile| profile.renderer == Renderer::Cairo)
        .map(|profile| profile.name.clone())
        .collect()
}

/// Frame estimate of one play: the cost-fact estimate of the recognized
/// call when one exists ([`crate::cost::CostFacts::frames_for_play`]),
/// the duration across profiles otherwise. Never a fabricated number —
/// unknown durations stay symbolic / unknown.
pub(crate) fn frames_at_play(context: &RuleContext<'_>, play: &PlayFact) -> Num {
    let matched = context.qualified_calls().calls.iter().position(|call| {
        call.file == play.site.file
            && u32::from(call.call_range.start()) == play.site.start
            && u32::from(call.call_range.end()) == play.site.end
    });
    if let Some(index) = matched {
        let frames = context.cost_facts().frames_for_play(index);
        if !frames.is_unknown() {
            return frames;
        }
    }
    frames_across_profiles(&play.duration, context.active_profiles())
}

/// A related location pointing at a lifecycle site.
pub(crate) fn related_site(
    context: &RuleContext<'_>,
    file: crate::source::FileId,
    start: u32,
    end: u32,
    message: String,
) -> RelatedLocation {
    let source = context.sources().file(file);
    RelatedLocation {
        path: source.relative_path().to_owned(),
        span: source.span_of_range(site_range(start, end)),
        message,
    }
}

/// Curated mobject classes whose constructor intrinsically installs a
/// per-frame callback the interpreter does not model (e.g. `TracedPath`):
/// an instance possibly in the family voids any "nothing else updates"
/// proof.
pub(crate) fn intrinsic_per_frame_kinds(profile: &KnowledgeProfile) -> BTreeSet<String> {
    profile
        .symbols
        .iter()
        .filter(|(_, entry)| {
            matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                && entry.effects.as_ref().is_some_and(|effects| {
                    effects.registers_updater == Some(true)
                        || effects.per_frame_callback == Some(true)
                })
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The last statement snapshot before the play (the state the wait /
/// play interval starts from).
pub(crate) fn snapshot_before<'a>(
    scene: &'a SceneLifecycle,
    play: &PlayFact,
) -> Option<&'a StateSnapshot> {
    scene.state_at(play.site.file, play.site.start)
}

/// Proof that the wait `play` is dynamic *solely* because of
/// `registration` (DESIGN §3.3 wait-freeze inputs): no stop condition,
/// `always_update_mobjects` provably off, no scene updater other than the
/// registration's own, no other possibly-time-based family updater, and
/// every possibly-present family member of exactly known kind with no
/// intrinsic per-frame behavior. The registration itself must stand with
/// all-paths certainty and — for a mobject host — definite family
/// membership and a definitely time-based callback.
pub(crate) fn wait_solely_dynamicized_by(
    scene: &SceneLifecycle,
    play: &PlayFact,
    registration: &UpdaterRegistration,
    intrinsic_kinds: &BTreeSet<String>,
) -> bool {
    if play.kind != PlayKind::Wait
        || play.dynamic_wait != Truth::Yes
        || play.has_stop_condition
        || play.always_update_mobjects != Truth::No
        || play.certainty != Presence::Present
        || registration.certainty != Presence::Present
    {
        return false;
    }
    // The registration must precede the wait in the same file.
    if registration.site.file != play.site.file || registration.site.end > play.site.start {
        return false;
    }
    let Some(snapshot) = snapshot_before(scene, play) else {
        return false;
    };
    let heap = &snapshot.heap;
    let Some(scene_state) = heap.scene(&scene.scene_id) else {
        return false;
    };
    match &registration.host {
        UpdaterHost::Scene => {
            if scene_state.scene_updaters.len() != 1
                || !scene_state.scene_updaters.contains(&registration.fact)
            {
                return false;
            }
        }
        UpdaterHost::Mobject(_) => {
            if !scene_state.scene_updaters.is_empty() {
                return false;
            }
        }
    }
    let host = match &registration.host {
        UpdaterHost::Mobject(id) => Some(heap.resolve(id)),
        UpdaterHost::Scene => None,
    };
    // For a scene-level host the scene-updater check above already proves
    // the dynamicizing source; a mobject host must prove it below.
    let mut host_dynamicizes = host.is_none();
    for (id, object) in &heap.objects {
        if !object.family_membership.may_be_present() {
            continue;
        }
        // Unknown kinds (or intrinsic per-frame classes) may carry
        // unmodeled time-based callbacks: the sole-cause proof fails.
        match &object.kind {
            KindSet::Known(candidates) => {
                if candidates
                    .iter()
                    .any(|candidate| intrinsic_kinds.contains(candidate))
                {
                    return false;
                }
            }
            KindSet::Unknown => return false,
        }
        for updater in &object.updaters {
            if updater.time_based == Truth::No {
                continue;
            }
            let resolved = heap.resolve(id);
            let is_host_own = host.as_ref() == Some(&resolved) && *updater == registration.fact;
            if !is_host_own {
                // Another (possibly) time-based family updater exists.
                return false;
            }
            if updater.time_based != Truth::Yes || object.family_membership != Presence::Present {
                return false;
            }
            host_dynamicizes = true;
        }
    }
    host_dynamicizes
}
