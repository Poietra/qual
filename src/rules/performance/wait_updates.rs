//! `MLP227`: `always_update_mobjects=True` forces a provably static wait
//! through the full per-frame pipeline (DESIGN §3.3, §7.3).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::cost::estimator::num_bounds_json;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{CameraKind, PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::values::{KindSet, Presence, Truth};

use super::frame_scope::{frames_at_play, intrinsic_per_frame_kinds, site_range, snapshot_before};
use super::support::{build_diagnostic, display_frames};

/// Metadata for [`ForcedAlwaysUpdateWait`].
pub const MLP227: RuleMetadata = RuleMetadata {
    id: "MLP227",
    summary: "always_update_mobjects=True dynamicizes a provably static wait",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// A wait made dynamic only by a literal `always_update_mobjects=True`
/// while its interval provably has no time-based updater, no scene
/// updater, no stop condition, and no camera motion (`camera_kind`
/// `Standard`): every frame runs the full frame pipeline to reproduce
/// an identical image (DESIGN §7.3 `MLP227`). A `Maybe` flag value never
/// fires.
///
/// The static proof additionally requires **no registered mobject
/// updater of any arity**: without the flag a wait with only
/// one-argument updaters freezes and those updaters stop running
/// (DESIGN §3.3), so suggesting the flag's removal is only
/// visual-preserving when nothing updates at all.
pub struct ForcedAlwaysUpdateWait;

/// Whether every object possibly in the family at the wait is provably
/// inert: no registered updaters, an exactly known kind, and no
/// intrinsic per-frame class (mirrors the `MLP205` static-content
/// proof).
fn interval_provably_static(
    scene: &SceneLifecycle,
    play: &PlayFact,
    intrinsic_kinds: &BTreeSet<String>,
) -> bool {
    let Some(snapshot) = snapshot_before(scene, play) else {
        return false;
    };
    let Some(scene_state) = snapshot.heap.scene(&scene.scene_id) else {
        return false;
    };
    if !scene_state.scene_updaters.is_empty() {
        return false;
    }
    snapshot.heap.objects.values().all(|object| {
        if !object.family_membership.may_be_present() {
            return true;
        }
        if !object.updaters.is_empty() {
            return false;
        }
        match &object.kind {
            KindSet::Known(candidates) => candidates
                .iter()
                .all(|candidate| !intrinsic_kinds.contains(candidate)),
            KindSet::Unknown => false,
        }
    })
}

impl Rule for ForcedAlwaysUpdateWait {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP227
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let intrinsic = intrinsic_per_frame_kinds(profile);
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown || scene.camera_kind != CameraKind::Standard {
                continue;
            }
            for play in &scene.plays {
                if play.kind != PlayKind::Wait
                    || play.certainty != Presence::Present
                    || play.always_update_mobjects != Truth::Yes
                    || play.has_stop_condition
                {
                    continue;
                }
                if !interval_provably_static(scene, play, &intrinsic) {
                    continue;
                }
                let frames = frames_at_play(context, play);
                let file = context.sources().file(play.site.file);
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                evidence.insert("always_update_mobjects".to_owned(), json!("yes (literal)"));
                evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                evidence.insert(
                    "interval".to_owned(),
                    json!({
                        "time_based_updaters": "none",
                        "mobject_updaters": "none",
                        "scene_updaters": "none",
                        "stop_condition": false,
                        "camera": "Standard (static)",
                    }),
                );
                diagnostics.push(build_diagnostic(
                    &MLP227,
                    context,
                    file,
                    site_range(play.site.start, play.site.end),
                    format!(
                        "`always_update_mobjects=True` is the only reason this wait \
                         renders dynamically: its interval provably has no updater of \
                         any kind, no stop condition, and no camera motion, so \
                         {frames} identical frames run the full frame pipeline. Drop \
                         the flag (or scope it to the plays that need it) and let the \
                         wait freeze.",
                        frames = display_frames(&frames),
                    ),
                    "A wait freezes unless always_update_mobjects, a Scene updater, a \
                     stop_condition, or a time-based family updater makes it dynamic \
                     (DESIGN 3.3). With the flag forced on, every wait frame walks \
                     the family, runs the update pass, and re-renders — here provably \
                     reproducing the same image each time. The encoder writes the \
                     full wait duration either way, so freezing does not shorten the \
                     video."
                        .to_owned(),
                    evidence,
                ));
            }
        }
        diagnostics
    }
}
