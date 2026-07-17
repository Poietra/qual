//! Play-timing performance rules: `MLP205` and `MLP206` (DESIGN §7.3).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::{TextRange, TextSize};
use serde_json::{Value, json};

use crate::config::model::RenderProfile;
use crate::cost::estimator::{
    frames_across_profiles, num_bounds_json, play_run_time, wait_duration,
};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::knowledge::{KnowledgeProfile, SymbolKind};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::values::{KindSet, Presence, Truth};

use super::support::{
    SCENE_PLAY, SCENE_WAIT, bound_receiver, build_diagnostic, conclusive_target, display_frames,
    display_seconds,
};

// ---------------------------------------------------------------------------
// MLP206: literal duration shorter than one frame.
// ---------------------------------------------------------------------------

/// Metadata for [`SubFrameDuration`].
pub const MLP206: RuleMetadata = RuleMetadata {
    id: "MLP206",
    summary: "Literal play duration shorter than one frame is clamped",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// A `play` / `wait` whose literal duration is provably shorter than one
/// frame at every analyzed profile's frame rate: the play is clamped to a
/// single rendered frame (DESIGN §7.3 `MLP206`).
pub struct SubFrameDuration;

impl Rule for SubFrameDuration {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP206
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let profiles = context.active_profiles();
        if profiles.is_empty()
            || profiles
                .iter()
                .any(|render| !render.frame_rate.is_finite() || render.frame_rate <= 0.0)
        {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        for call in &calls.calls {
            if !bound_receiver(call) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            let duration = match canonical.as_str() {
                SCENE_PLAY => play_run_time(call, calls),
                SCENE_WAIT => wait_duration(call, &["duration"]),
                _ => continue,
            };
            // Both bounds must be literal facts (a positive literal; zero
            // and negative durations are MLC104's runtime error).
            let (Some(lower), Some(upper)) = (duration.lower_bound(), duration.upper_bound())
            else {
                continue;
            };
            if lower <= 0.0 {
                continue;
            }
            // Provably shorter than one frame under every analyzed profile.
            if !profiles
                .iter()
                .all(|render| upper < 1.0 / render.frame_rate)
            {
                continue;
            }
            let file = context.sources().file(call.file);
            let frame_seconds: Vec<Value> = profiles
                .iter()
                .map(|render| json!({"profile": render.name, "frame_seconds": 1.0 / render.frame_rate}))
                .collect();
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(canonical.clone()));
            evidence.insert("duration_seconds".to_owned(), num_bounds_json(&duration));
            evidence.insert("profiles".to_owned(), Value::Array(frame_seconds));
            evidence.insert("frames".to_owned(), json!({"lower": 1, "upper": 1}));
            let fps_list = profiles
                .iter()
                .map(|render| format!("{} fps", render.frame_rate))
                .collect::<Vec<_>>()
                .join(", ");
            diagnostics.push(build_diagnostic(
                &MLP206,
                context,
                file,
                call.call_range,
                format!(
                    "The literal duration {duration} is shorter than one frame at \
                     {fps_list}: Manim clamps this to a single rendered frame, so \
                     the whole play startup (compile, hash, begin copies, cleanup) \
                     is paid for one frame of output.",
                    duration = display_seconds(&duration),
                ),
                "The frame grid is `np.arange(0, run_time, 1/frame_rate)`: a run \
                 time below one frame period still renders exactly one frame, so \
                 the animation shows only its final state while the per-play \
                 fixed costs remain. Either extend the duration to cover the \
                 intended motion or apply the final state directly without a \
                 play."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP205: frozen-eligible wait forced dynamic with frozen_frame=False.
// ---------------------------------------------------------------------------

/// Metadata for [`StaticUnfrozenWait`].
pub const MLP205: RuleMetadata = RuleMetadata {
    id: "MLP205",
    summary: "wait(frozen_frame=False) re-renders provably identical frames",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// Minimum literal wait duration (seconds) before `MLP205` fires: shorter
/// static waits re-render few frames and are not worth a warning
/// (DESIGN §7.3 duration threshold).
pub const MLP205_MIN_WAIT_SECONDS: f64 = 2.0;

/// Minimum profile pixel area (`pixel_width × pixel_height`) before
/// `MLP205` fires: below 720p the redundant full-frame raster is cheap
/// (DESIGN §7.3 pixel threshold).
pub const MLP205_MIN_PIXEL_AREA: u64 = 1280 * 720;

/// `wait(frozen_frame=False)` over a span with provably zero dynamic
/// content: no scene updaters, no stop condition, no registered mobject
/// updaters, no unknown callbacks — every rendered frame is identical
/// (DESIGN §7.3 `MLP205`).
pub struct StaticUnfrozenWait;

impl Rule for StaticUnfrozenWait {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP205
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.active_profiles();
        if profiles.is_empty()
            || !profiles.iter().all(|render| {
                u64::from(render.pixel_width) * u64::from(render.pixel_height)
                    >= MLP205_MIN_PIXEL_AREA
            })
        {
            return Vec::new();
        }
        let intrinsic_hot_kinds = intrinsically_hot_mobject_kinds(profile);
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for play in &scene.plays {
                if play.kind != PlayKind::Wait
                    || play.frozen_frame != Some(false)
                    || play.has_stop_condition
                    || play.dynamic_wait != Truth::No
                    || play.certainty != Presence::Present
                {
                    continue;
                }
                let Some(lower) = play.duration.lower_bound() else {
                    continue;
                };
                if lower < MLP205_MIN_WAIT_SECONDS {
                    continue;
                }
                if !wait_content_provably_static(scene, play, &intrinsic_hot_kinds) {
                    continue;
                }
                let frames = frames_across_profiles(&play.duration, profiles);
                let file = context.sources().file(play.site.file);
                let range = TextRange::new(
                    TextSize::from(play.site.start),
                    TextSize::from(play.site.end),
                );
                let mut evidence = BTreeMap::new();
                evidence.insert(
                    "duration_seconds".to_owned(),
                    num_bounds_json(&play.duration),
                );
                evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                evidence.insert("dynamic_wait".to_owned(), Value::String("no".to_owned()));
                evidence.insert(
                    "pixel_area".to_owned(),
                    Value::Array(profiles.iter().map(pixel_area_json).collect()),
                );
                diagnostics.push(build_diagnostic(
                    &MLP205,
                    context,
                    file,
                    range,
                    format!(
                        "`frozen_frame=False` forces this wait to re-render \
                         {frames} provably identical frames: nothing in the \
                         scene can change during the wait. Drop \
                         `frozen_frame=False` so Manim freezes the wait and \
                         renders the frame once.",
                        frames = display_frames(&frames),
                    ),
                    "No scene updater, stop condition, or registered mobject \
                     updater exists over this wait, so every frame repeats the \
                     previous one; the full per-frame pipeline (family walk, \
                     raster, readback) runs anyway. Freezing skips that work — \
                     the encoder still writes the full wait duration either \
                     way, so the video length does not change."
                        .to_owned(),
                    evidence,
                ));
            }
        }
        diagnostics
    }
}

/// Curated mobject classes whose constructor intrinsically installs a
/// per-frame callback the interpreter does not model (e.g. `TracedPath`):
/// an instance possibly in the family means the wait content is not
/// provably static.
fn intrinsically_hot_mobject_kinds(profile: &KnowledgeProfile) -> BTreeSet<String> {
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

/// Whether every object possibly in the scene family at the wait site is
/// provably inert: no registered updaters and a fully known kind with no
/// intrinsic per-frame behavior. Anything unknown fails the proof.
fn wait_content_provably_static(
    scene: &SceneLifecycle,
    play: &PlayFact,
    intrinsic_hot_kinds: &BTreeSet<String>,
) -> bool {
    let Some(snapshot) = scene.state_at(play.site.file, play.site.start) else {
        return false;
    };
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
                .all(|candidate| !intrinsic_hot_kinds.contains(candidate)),
            KindSet::Unknown => false,
        }
    })
}

/// `{"profile": ..., "pixel_area": ...}` for the evidence map.
fn pixel_area_json(profile: &RenderProfile) -> Value {
    json!({
        "profile": profile.name,
        "pixel_area": u64::from(profile.pixel_width) * u64::from(profile.pixel_height),
    })
}
