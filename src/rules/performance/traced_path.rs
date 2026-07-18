//! `MLP220`: unbounded `TracedPath` alive over a long span (DESIGN §7.3).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::cost::estimator::{num_bounds_json, sum_frames_across_profiles, sum_seconds};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::values::{AllocationSite, Cardinality, Num, ObjectId, Presence, Truth};

use super::support::{
    build_diagnostic, conclusive_target, display_frames, display_seconds, enclosing_scene,
};

/// Canonical id of `TracedPath`.
const TRACED_PATH: &str = "manim.animation.changing.TracedPath";

/// Metadata for [`UnboundedTracedPath`].
pub const MLP220: RuleMetadata = RuleMetadata {
    id: "MLP220",
    summary: "TracedPath without dissipating_time accumulates over a long span",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "cost-facts"],
    // DESIGN §7.3 declares this specificity edge for the catalog, but the
    // shared dedup only collapses diagnostics on the SAME primary span.
    // This rule anchors on the `TracedPath` constructor while `MLP204`
    // anchors on a per-frame `Scene.add` call, and the two describe
    // genuinely different defects: the path's *internal* point history
    // growing frame by frame versus the *user's* scene graph gaining a
    // fresh mobject per frame. `MLP204` can never fire on a `TracedPath`
    // construction (it requires an add-effect call in a hot context), so
    // one defect is never double-reported; a scenario containing both
    // patterns intentionally reports both, each at its own span (pinned
    // by `mlp220_and_mlp204_are_distinct_defects_that_co_report`).
    supersedes: &["MLP204", "MLP211"],
};

/// Minimum provable alive span (seconds of subsequent plays with the path
/// in the scene family) before `MLP220` fires. Shorter traces stay cheap;
/// `TracedPath` itself is never banned (DESIGN §7.3 prose).
pub const MLP220_LONG_SPAN_SECONDS: f64 = 5.0;

/// A `TracedPath` constructed without `dissipating_time` (or with a literal
/// `None`) that provably stays in the scene family across a long span of
/// plays: its point count grows `O(F)` and cumulative rasterization
/// approaches `O(F²)` (DESIGN §7.3 `MLP220`).
pub struct UnboundedTracedPath;

impl Rule for UnboundedTracedPath {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP220
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let profiles = context.active_profiles();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != TRACED_PATH {
                continue;
            }
            // A `**kwargs` splat could still pass `dissipating_time`.
            if call.has_star_star_kwargs {
                continue;
            }
            let dissipating = match call.keyword("dissipating_time") {
                None => "absent",
                Some(argument) if matches!(argument.literal, Some(LiteralFact::NoneLit)) => "None",
                Some(_) => continue,
            };
            let Some(scene) = enclosing_scene(context.lifecycle_facts(), call) else {
                continue;
            };
            let site = AllocationSite::new(call.file, call.call_range);
            // The abstract object allocated at this construction; only a
            // singleton identity supports a definite lifetime claim.
            let Some(object) = scene
                .final_heap
                .objects
                .keys()
                .find(|id| id.site == site)
                .cloned()
            else {
                continue;
            };
            if object.cardinality != Cardinality::Singleton {
                continue;
            }
            let alive = alive_plays(scene, &object);
            if alive.is_empty() {
                continue;
            }
            let span = sum_seconds(alive.iter().map(|play| &play.duration));
            let Some(span_lower) = span.lower_bound() else {
                continue;
            };
            if span_lower < MLP220_LONG_SPAN_SECONDS {
                continue;
            }
            // One traced point per updater call, one call per rendered
            // frame — and each play renders its own frame grid, so the
            // per-play ceils are summed (never the summed seconds ceiled).
            let frames =
                sum_frames_across_profiles(alive.iter().map(|play| &play.duration), profiles);
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(TRACED_PATH.to_owned()));
            evidence.insert(
                "dissipating_time".to_owned(),
                Value::String(dissipating.to_owned()),
            );
            evidence.insert("alive_span_seconds".to_owned(), num_bounds_json(&span));
            evidence.insert("estimated_points".to_owned(), num_bounds_json(&frames));
            evidence.insert(
                "growth".to_owned(),
                json!({"points": "O(F)", "raster": "O(F^2)"}),
            );
            let points = match frames {
                Num::Exact(_) | Num::Interval { .. } => format!(
                    " (about one traced point per frame: {frames} points at the \
                     analyzed frame rates)",
                    frames = display_frames(&frames),
                ),
                Num::Symbol(_) | Num::Unknown => String::new(),
            };
            diagnostics.push(build_diagnostic(
                &MLP220,
                context,
                file,
                call.call_range,
                format!(
                    "This `TracedPath` has no `dissipating_time`, and it stays in \
                     the scene for {span} of subsequent plays: the path keeps \
                     every point it ever traced{points}, so its geometry grows \
                     `O(F)` and rasterizing the accumulated path across the run \
                     approaches `O(F²)`.",
                    span = display_seconds(&span),
                ),
                "TracedPath appends the traced point every frame and, without \
                 `dissipating_time`, never discards history. An unbounded trace \
                 is a legitimate effect — if the full history is intended, keep \
                 it as written. If only a recent window matters, pass \
                 `dissipating_time=<seconds>` so the path length stays bounded."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

/// Plays during which `object` is provably alive and updating: in the
/// scene family at the play site, not a target of the play's animations
/// (targets get their updaters suspended), and — for waits — only provably
/// dynamic waits (a frozen wait runs no updaters).
fn alive_plays<'p>(scene: &'p SceneLifecycle, object: &ObjectId) -> Vec<&'p PlayFact> {
    let mut plays = Vec::new();
    for play in &scene.plays {
        if play.certainty != Presence::Present {
            continue;
        }
        let Some((_, family)) = scene.membership_at(object, play.site.file, play.site.start) else {
            continue;
        };
        if family != Presence::Present {
            continue;
        }
        if play.kind == PlayKind::Wait && play.dynamic_wait != Truth::Yes {
            continue;
        }
        let targets_object = play.animations.iter().any(|animation| {
            animation.state.as_ref().is_some_and(|state| {
                state
                    .targets
                    .iter()
                    .any(|target| target.may_be_same(object) != Truth::No)
            })
        });
        if targets_object {
            continue;
        }
        plays.push(play);
    }
    plays
}
