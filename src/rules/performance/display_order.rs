//! Cairo display-order rules: `MLP209` (front-loaded static suffix) and
//! `MLP222` (image caught in the moving suffix) — DESIGN §3.4, §4.3
//! Cairo stage, §7.3.
//!
//! Both rules are Cairo-specific: the effective z-ordered display list
//! and the moving-suffix re-raster are Cairo semantics, so
//! `applicable_profiles` is restricted to the active Cairo profiles and
//! the rules stay silent when the run targets none (DESIGN §15.8).
//!
//! Quantification discipline (binding `MLP209` prose): positions and
//! member counts are only reported when the display order is `Known` and
//! the moving-suffix bounds are `Num::Exact` — an unknown order never
//! produces a number.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::cost::estimator::num_bounds_json;
use crate::cost::thresholds::Threshold;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::render_order::{
    DisplayOrder, MovingReason, SuffixFact, inputs_at_play, moving_scope_at_play,
    moving_suffix_evidence,
};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayFact, SceneLifecycle};
use crate::semantic::values::{KindSet, Num, NumLit, Presence};

use super::frame_scope::{
    cairo_profile_names, camera_truth, frames_at_play, related_site, renders_frames, site_range,
};
use super::support::display_frames;

/// Canonical id of the curated `ImageMobject` class.
const IMAGE_MOBJECT: &str = "manim.mobject.types.image_mobject.ImageMobject";

/// `MLP209` fires only on a proven static suffix of at least this many
/// later display-list members (DESIGN §7.3 "large static suffix"; smaller
/// suffixes re-rasterize little).
pub const MLP209_STATIC_SUFFIX_GATE: Threshold = Threshold {
    name: "static-suffix-gate",
    quantity: "N_moving_suffix",
    minimum: 8.0,
    rule_ids: &["MLP209"],
};

/// `MLP222` fires only when every active Cairo profile rasterizes at
/// least this many pixels per frame (DESIGN §7.3 "screen area threshold";
/// below 720p the full-frame composite is cheap).
pub const MLP222_MIN_PIXEL_AREA: u64 = 1280 * 720;

/// Metadata for [`FrontLoadedStaticSuffix`].
pub const MLP209: RuleMetadata = RuleMetadata {
    id: "MLP209",
    summary: "Animated or updater-bearing object early in Cairo's display order re-rasterizes a large static suffix",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// A moving (animated / updater-bearing / foreground) member near the
/// front of Cairo's effective display order whose static suffix is
/// re-rasterized every frame of the play (DESIGN §7.3 `MLP209`).
///
/// Advisory: reordering the display list changes what draws over what,
/// so the suggestion is never an autofix and severity stays `info`.
pub struct FrontLoadedStaticSuffix;

/// The exact moving-suffix quantification of one play, or `None` when
/// any part of it is not proven (unknown order, interval bounds).
struct ExactSuffix {
    first_index: i64,
    suffix_len: i64,
    reason: Option<MovingReason>,
    sentence: String,
}

/// Computes the display order and moving suffix of one play, returning
/// the exact quantification only (DESIGN §7.3 `MLP209` prose: no numbers
/// from an unknown order).
fn exact_suffix_at(scene: &SceneLifecycle, play: &PlayFact) -> Option<ExactSuffix> {
    let inputs = inputs_at_play(scene, play).ok()?;
    let order = DisplayOrder::compute(&inputs);
    if !order.is_known() {
        return None;
    }
    let camera = camera_truth(scene.camera_kind);
    let scope = moving_scope_at_play(scene, play, camera);
    let suffix = SuffixFact::compute(&order, &inputs, &scope)?;
    let Some(Num::Exact(NumLit::Int(first_index))) = suffix.first_moving_index else {
        return None;
    };
    let Num::Exact(NumLit::Int(suffix_len)) = suffix.suffix_len else {
        return None;
    };
    let sentence = moving_suffix_evidence(&order, &suffix)?;
    let reason = suffix
        .members_evidence
        .iter()
        .find(|evidence| i64::try_from(evidence.index) == Ok(first_index))
        .map(|evidence| evidence.reason);
    Some(ExactSuffix {
        first_index,
        suffix_len,
        reason,
        sentence,
    })
}

impl Rule for FrontLoadedStaticSuffix {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP209
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cairo_profiles = cairo_profile_names(context);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for play in &scene.plays {
                if !renders_frames(play) || play.certainty != Presence::Present {
                    continue;
                }
                let Some(exact) = exact_suffix_at(scene, play) else {
                    continue;
                };
                let suffix_len = Num::int(exact.suffix_len);
                if !MLP209_STATIC_SUFFIX_GATE.confirmed_by(&suffix_len) {
                    continue;
                }
                let mover = match exact.reason {
                    Some(MovingReason::FamilyUpdater) => "An updater-bearing object",
                    Some(MovingReason::AnimationTarget) => "An animated object",
                    Some(MovingReason::Foreground) => "A foreground object",
                    None => "A moving object",
                };
                let frames = frames_at_play(context, play);
                let file = context.sources().file(play.site.file);
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                evidence.insert(
                    "threshold".to_owned(),
                    MLP209_STATIC_SUFFIX_GATE.evidence(&suffix_len),
                );
                evidence.insert(
                    "citation".to_owned(),
                    json!(MLP209_STATIC_SUFFIX_GATE.citation()),
                );
                evidence.insert("first_moving_index".to_owned(), json!(exact.first_index));
                evidence.insert("static_suffix_members".to_owned(), json!(exact.suffix_len));
                evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                evidence.insert(
                    "camera_moving".to_owned(),
                    json!(format!("{:?}", camera_truth(scene.camera_kind))),
                );
                diagnostics.push(Diagnostic {
                    rule_id: MLP209.id.to_owned(),
                    severity: MLP209.default_severity,
                    confidence: Confidence::High,
                    path: file.relative_path().to_owned(),
                    primary_span: file.span_of_range(site_range(play.site.start, play.site.end)),
                    message: format!(
                        "{mover} {sentence} If z-order permits, move the dynamic \
                         object later or separate the static layer.",
                        sentence = exact.sentence,
                    ),
                    explanation: Some(
                        "Cairo partitions the effective z-ordered display list at the \
                         first moving (animated, foreground, or updater-bearing) member: \
                         everything after it is re-rasterized and composited every frame \
                         (scene.py get_moving_mobjects), so a dynamic object near the \
                         front turns the whole static remainder into per-frame work. \
                         Reordering changes what draws over what, so this is advisory — \
                         verify the visual stacking before moving anything."
                            .to_owned(),
                    ),
                    related_locations: Vec::new(),
                    evidence,
                    estimated_cost: None,
                    applicable_profiles: cairo_profiles.clone(),
                    fix: None,
                });
            }
        }
        diagnostics
    }
}

/// Metadata for [`ImageInMovingSuffix`].
pub const MLP222: RuleMetadata = RuleMetadata {
    id: "MLP222",
    summary: "ImageMobject re-rasterized every frame inside Cairo's moving suffix",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// An `ImageMobject` that sits at or after the first moving member of
/// Cairo's effective display order during a rendering play: the image is
/// re-rasterized and composited at full frame resolution every frame
/// (DESIGN §7.3 `MLP222`).
///
/// No pixel-area fact exists for the image itself, so the image size is
/// described qualitatively; the frame area comes from the profiles.
pub struct ImageInMovingSuffix;

impl Rule for ImageInMovingSuffix {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP222
    }

    #[allow(
        clippy::too_many_lines,
        reason = "per-image aggregation with refusal-gated quantification"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cairo_profiles: Vec<&crate::config::model::RenderProfile> = context
            .active_profiles()
            .iter()
            .filter(|profile| profile.renderer == crate::config::model::Renderer::Cairo)
            .collect();
        if cairo_profiles.is_empty()
            || !cairo_profiles.iter().all(|profile| {
                u64::from(profile.pixel_width) * u64::from(profile.pixel_height)
                    >= MLP222_MIN_PIXEL_AREA
            })
        {
            return Vec::new();
        }
        let profile_names: Vec<String> = cairo_profiles
            .iter()
            .map(|profile| profile.name.clone())
            .collect();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            // Qualifying plays per image allocation, in play order.
            let mut per_image: BTreeMap<
                crate::semantic::values::ObjectId,
                Vec<(&PlayFact, usize, usize)>,
            > = BTreeMap::new();
            for play in &scene.plays {
                if !renders_frames(play) || play.certainty != Presence::Present {
                    continue;
                }
                let Ok(inputs) = inputs_at_play(scene, play) else {
                    continue;
                };
                let order = DisplayOrder::compute(&inputs);
                let Some(members) = order.members() else {
                    continue;
                };
                let camera = camera_truth(scene.camera_kind);
                let scope = moving_scope_at_play(scene, play, camera);
                let Some(suffix) = SuffixFact::compute(&order, &inputs, &scope) else {
                    continue;
                };
                let Some(Num::Exact(NumLit::Int(first))) = suffix.first_moving_index else {
                    continue;
                };
                let Ok(first) = usize::try_from(first) else {
                    continue;
                };
                let Some(snapshot) = scene.state_at(play.site.file, play.site.end) else {
                    continue;
                };
                for (index, member) in members.iter().enumerate() {
                    if index < first {
                        continue;
                    }
                    // An image that is itself a target of this play's
                    // animations moves because the play animates it —
                    // that is the play's purpose, not a re-raster defect.
                    // Updater-driven / foreground motion and being caught
                    // behind another mover still count.
                    let is_play_target = suffix.members_evidence.iter().any(|evidence| {
                        evidence.id == member.id && evidence.reason == MovingReason::AnimationTarget
                    });
                    if is_play_target {
                        continue;
                    }
                    let Some(state) = snapshot.heap.object(&member.id) else {
                        continue;
                    };
                    let KindSet::Known(candidates) = &state.kind else {
                        continue;
                    };
                    if candidates.is_empty()
                        || !candidates
                            .iter()
                            .all(|candidate| candidate == IMAGE_MOBJECT)
                    {
                        continue;
                    }
                    per_image.entry(member.id.clone()).or_default().push((
                        play,
                        index,
                        members.len(),
                    ));
                }
            }
            for (image, plays) in per_image {
                let Some((_, index, total)) = plays.first().copied() else {
                    continue;
                };
                // Total frame estimate across the qualifying plays: known
                // lower bounds add up; an unknown duration opens the
                // upper bound instead of fabricating one.
                let mut lower = 0.0_f64;
                let mut upper = Some(0.0_f64);
                for (play, _, _) in &plays {
                    let frames = frames_at_play(context, play);
                    if let Some(bound) = frames.lower_bound() {
                        lower += bound;
                    }
                    upper = match (upper, frames.upper_bound()) {
                        (Some(current), Some(bound)) => Some(current + bound),
                        _ => None,
                    };
                }
                let frames = Num::Interval {
                    lo: Some(lower),
                    hi: upper,
                };
                // No real bound: say "every frame" instead of a
                // zero-looking lower bound (DESIGN §15: no fabricated
                // numbers, and no misleading ones either).
                let span_clause = if lower > 0.0 || upper.is_some() {
                    format!(
                        "for {frames} frames across {plays} play(s)",
                        frames = display_frames(&frames),
                        plays = plays.len(),
                    )
                } else {
                    format!("for every frame of {plays} play(s)", plays = plays.len())
                };
                let file = context.sources().file(image.site.file);
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                evidence.insert("resolved".to_owned(), json!(IMAGE_MOBJECT));
                evidence.insert(
                    "display_position".to_owned(),
                    json!({"index": index, "total": total}),
                );
                evidence.insert("plays_in_moving_suffix".to_owned(), json!(plays.len()));
                evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                evidence.insert(
                    "frame_pixels".to_owned(),
                    Value::Array(
                        cairo_profiles
                            .iter()
                            .map(|profile| {
                                json!({
                                    "profile": profile.name,
                                    "pixel_area": u64::from(profile.pixel_width)
                                        * u64::from(profile.pixel_height),
                                })
                            })
                            .collect(),
                    ),
                );
                evidence.insert(
                    "image_area".to_owned(),
                    json!("unknown (no pixel-area fact; size described qualitatively)"),
                );
                let related = plays
                    .iter()
                    .map(|(play, index, total)| {
                        related_site(
                            context,
                            play.site.file,
                            play.site.start,
                            play.site.end,
                            format!(
                                "this play re-rasterizes the image every frame \
                                 (display position {position}/{total})",
                                position = index + 1,
                            ),
                        )
                    })
                    .collect();
                diagnostics.push(Diagnostic {
                    rule_id: MLP222.id.to_owned(),
                    severity: MLP222.default_severity,
                    confidence: Confidence::High,
                    path: file.relative_path().to_owned(),
                    primary_span: file.span_of_range(site_range(image.site.start, image.site.end)),
                    message: format!(
                        "This ImageMobject sits at position {position}/{total} of \
                         Cairo's effective display list, at or after the first moving \
                         member: it is re-rasterized and composited {span_clause}. If \
                         the image is static background, give it a lower z_index (or \
                         add it before the moving objects) so it stays in the static \
                         base.",
                        position = index + 1,
                    ),
                    explanation: Some(
                        "Cairo re-rasterizes every display-list member from the first \
                         moving one onward, each frame, and composites the result over \
                         the full frame (scene.py get_moving_mobjects; DESIGN 4.3 \
                         Cairo stage). Image rasters pay per-pixel work — expensive \
                         for large images — even when the image itself never changes. \
                         The image's own pixel size is not statically known; the \
                         re-raster claim holds regardless."
                            .to_owned(),
                    ),
                    related_locations: related,
                    evidence,
                    estimated_cost: None,
                    applicable_profiles: profile_names.clone(),
                    fix: None,
                });
            }
        }
        diagnostics
    }
}
