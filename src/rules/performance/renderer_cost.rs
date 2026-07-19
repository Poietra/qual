//! Renderer/pixel-cost rules `MLP212`, `MLP213`, and `MLP223`
//! (DESIGN §7.3).
//!
//! The three rules deliberately share a narrow proof boundary: a runtime
//! object must have singleton allocation identity, a certain play must
//! target it directly, and every numeric claim comes from a literal,
//! lifecycle snapshot, render profile, or versioned calibration gate.
//! Unknown style, target identity, duration, renderer, or future opacity
//! mutation keeps the corresponding rule silent.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::config::model::Renderer;
use crate::cost::estimator::{frames_across_profiles, num_bounds_json};
use crate::cost::model::pixel_frames;
use crate::cost::thresholds::{CAIRO_SURFACE_FACE_GATE, FULL_SCREEN_TRANSLUCENT_SECONDS_GATE};
use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::index::{LiteralFact, QualifiedCall};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::state::{MobjectState, WriteChannel};
use crate::semantic::values::{Cardinality, KindSet, Num, ObjectId, Presence, Truth};

use super::frame_scope::{
    cairo_profile_names, frames_at_play, related_site, renders_frames, site_range, snapshot_before,
};
use super::support::{conclusive_target, display_frames, display_seconds};
use crate::rules::base::{Rule, RuleContext};

const FULL_SCREEN_RECTANGLE: &str = "manim.mobject.frame.FullScreenRectangle";
const SURFACE: &str = "manim.mobject.three_d.three_dimensions.Surface";

fn call_at_object<'a>(
    context: &'a RuleContext<'_>,
    object: &ObjectId,
) -> Option<&'a QualifiedCall> {
    context
        .qualified_calls()
        .calls_in_file(object.site.file)
        .find(|call| {
            u32::from(call.call_range.start()) == object.site.start
                && u32::from(call.call_range.end()) == object.site.end
        })
}

/// Metadata for [`LongTranslucentFullScreenAnimation`].
pub const MLP212: RuleMetadata = RuleMetadata {
    id: "MLP212",
    summary: "Long animation of a full-screen translucent object or layer",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// Metadata for [`CairoSurfaceMismatch`].
pub const MLP213: RuleMetadata = RuleMetadata {
    id: "MLP213",
    summary: "Calibrated large Surface workload rendered under Cairo",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// Metadata for [`TransparentStrokeCapture`].
pub const MLP223: RuleMetadata = RuleMetadata {
    id: "MLP223",
    summary: "Fully transparent positive-width stroke processed on every Cairo frame",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

fn definitely_kind(state: &MobjectState, canonical: &str) -> bool {
    matches!(
        &state.kind,
        KindSet::Known(candidates)
            if !candidates.is_empty() && candidates.iter().all(|candidate| candidate == canonical)
    )
}

fn exact_value(value: &Num) -> Option<f64> {
    match value {
        Num::Exact(literal) => Some(literal.as_f64()),
        Num::Interval { .. } | Num::Symbol(_) | Num::Unknown => None,
    }
}

fn no_active_updaters(scene: &SceneLifecycle, play: &PlayFact) -> bool {
    let Some(snapshot) = snapshot_before(scene, play) else {
        return false;
    };
    snapshot
        .heap
        .objects
        .values()
        .all(|object| object.updaters.is_empty())
        && snapshot
            .heap
            .scene(&scene.scene_id)
            .is_some_and(|state| state.scene_updaters.is_empty())
}

/// Definite animation arguments of `play` that target `object`. Any
/// unresolved sibling argument makes a style-stability proof impossible:
/// it could target the same object and write opacity.
fn stable_target_animations<'a>(
    play: &'a PlayFact,
    object: &ObjectId,
) -> Option<Vec<&'a crate::semantic::interpreter::PlayedAnimation>> {
    if play.kind != PlayKind::Play
        || play.certainty != Presence::Present
        || play.star_args
        || !renders_frames(play)
    {
        return None;
    }
    let mut matched = Vec::new();
    for animation in &play.animations {
        if animation.convertible != Truth::Yes {
            return None;
        }
        let state = animation.state.as_ref()?;
        let mut targets_object = false;
        for target in &state.targets {
            match target.may_be_same(object) {
                Truth::Yes => targets_object = true,
                Truth::Maybe => return None,
                Truth::No => {}
            }
        }
        if !targets_object {
            continue;
        }
        if animation.channels_known != Truth::Yes
            || state.write_channels.contains(&WriteChannel::Style)
            || state.write_channels.contains(&WriteChannel::Opacity)
        {
            return None;
        }
        matched.push(animation);
    }
    (!matched.is_empty()).then_some(matched)
}

fn direct_target_play(play: &PlayFact, object: &ObjectId) -> bool {
    play.kind == PlayKind::Play
        && play.certainty == Presence::Present
        && renders_frames(play)
        && play.animations.iter().any(|animation| {
            animation.convertible == Truth::Yes
                && animation.state.as_ref().is_some_and(|state| {
                    state
                        .targets
                        .iter()
                        .any(|target| target.definitely_same(object))
                })
        })
}

fn object_related(context: &RuleContext<'_>, object: &ObjectId, message: &str) -> RelatedLocation {
    related_site(
        context,
        object.site.file,
        object.site.start,
        object.site.end,
        message.to_owned(),
    )
}

fn active_profile_names(context: &RuleContext<'_>) -> Vec<String> {
    context
        .active_profiles()
        .iter()
        .map(|profile| profile.name.clone())
        .collect()
}

fn cairo_frames_at_play(context: &RuleContext<'_>, play: &PlayFact) -> Num {
    let profiles: Vec<_> = context
        .active_profiles()
        .iter()
        .filter(|profile| profile.renderer == Renderer::Cairo)
        .cloned()
        .collect();
    frames_across_profiles(&play.duration, &profiles).mul(&play.repetitions)
}

/// A `FullScreenRectangle` whose exact fill opacity stays strictly between
/// zero and one through a certain, at-least-five-second target animation.
pub struct LongTranslucentFullScreenAnimation;

impl Rule for LongTranslucentFullScreenAnimation {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP212
    }

    #[allow(
        clippy::too_many_lines,
        reason = "coverage/style/play gates and per-profile pixel evidence stay together"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        // FullScreenRectangle inherits ScreenRectangle's default 16:9
        // aspect ratio. Restrict the pixel-coverage claim to profiles with
        // that exact frame aspect; other profiles are Unknown, not "full".
        if context.active_profiles().is_empty()
            || context.active_profiles().iter().any(|profile| {
                u64::from(profile.pixel_width) * 9 != u64::from(profile.pixel_height) * 16
            })
        {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        let mut seen: BTreeSet<(crate::source::FileId, u32, ObjectId)> = BTreeSet::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for play in &scene.plays {
                if !FULL_SCREEN_TRANSLUCENT_SECONDS_GATE.confirmed_by(&play.duration)
                    || !no_active_updaters(scene, play)
                {
                    continue;
                }
                let Some(snapshot) = snapshot_before(scene, play) else {
                    continue;
                };
                for (object, state) in &snapshot.heap.objects {
                    if object.cardinality != Cardinality::Singleton
                        || !definitely_kind(state, FULL_SCREEN_RECTANGLE)
                    {
                        continue;
                    }
                    let Some(constructor) = call_at_object(context, object) else {
                        continue;
                    };
                    if constructor.has_star_args
                        || constructor.has_star_star_kwargs
                        || constructor.positional_count != 0
                        || constructor.keyword("aspect_ratio").is_some()
                    {
                        continue;
                    }
                    let Some(opacity) = exact_value(&state.fill_opacity) else {
                        continue;
                    };
                    if !(0.0 < opacity && opacity < 1.0) {
                        continue;
                    }
                    let Some(animations) = stable_target_animations(play, object) else {
                        continue;
                    };
                    if !seen.insert((play.site.file, play.site.start, object.clone())) {
                        continue;
                    }
                    let frames = frames_at_play(context, play);
                    let pixels: Vec<Value> = context
                        .active_profiles()
                        .iter()
                        .map(|profile| {
                            json!({
                                "profile": profile.name,
                                "frame_pixels": u64::from(profile.pixel_width)
                                    * u64::from(profile.pixel_height),
                                "pixel_frames": num_bounds_json(&pixel_frames(&frames, profile)),
                            })
                        })
                        .collect();
                    let mut evidence = BTreeMap::new();
                    evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                    evidence.insert("object_kind".to_owned(), json!(FULL_SCREEN_RECTANGLE));
                    evidence.insert("fill_opacity".to_owned(), json!(opacity));
                    evidence.insert(
                        "duration_gate".to_owned(),
                        FULL_SCREEN_TRANSLUCENT_SECONDS_GATE.evidence(&play.duration),
                    );
                    evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                    evidence.insert("profiles".to_owned(), Value::Array(pixels));
                    let animation = animations[0];
                    let file = context.sources().file(animation.site.file);
                    diagnostics.push(Diagnostic {
                        rule_id: MLP212.id.to_owned(),
                        severity: MLP212.default_severity,
                        confidence: MLP212.minimum_confidence,
                        path: file.relative_path().to_owned(),
                        primary_span: file.span_of_range(site_range(
                            animation.site.start,
                            animation.site.end,
                        )),
                        message: format!(
                            "This full-screen layer remains {:.0}% opaque for {} ({}): every frame blends the full output area. Consider animating a smaller region or shortening the translucent interval.",
                            opacity * 100.0,
                            display_seconds(&play.duration),
                            display_frames(&frames),
                        ),
                        explanation: Some(
                            "FullScreenRectangle is sized to Manim's frame height and aspect ratio. With a fill opacity strictly between zero and one, compositing must read and blend the pixels beneath it instead of replacing them. The diagnostic requires a literal-derived stable opacity, no active updater, a certain direct target animation, and a proven duration of at least five seconds; unknown or opacity-changing plays stay silent."
                                .to_owned(),
                        ),
                        related_locations: vec![object_related(
                            context,
                            object,
                            "the full-screen translucent layer is created here",
                        )],
                        evidence,
                        estimated_cost: None,
                        applicable_profiles: active_profile_names(context),
                        fix: None,
                    });
                }
            }
        }
        diagnostics
    }
}

fn positive_resolution(value: &LiteralFact) -> Option<i64> {
    match value {
        LiteralFact::Int(value) if *value > 0 => Some(*value),
        _ => None,
    }
}

fn surface_faces(call: &QualifiedCall) -> Option<i64> {
    if call.has_star_args || call.has_star_star_kwargs {
        return None;
    }
    let resolution = call
        .keyword("resolution")
        .or_else(|| call.positional(3))?
        .literal
        .as_ref()?;
    match resolution {
        LiteralFact::Int(_) => {
            let side = positive_resolution(resolution)?;
            side.checked_mul(side)
        }
        LiteralFact::Tuple(values) | LiteralFact::List(values) if values.len() == 2 => {
            positive_resolution(&values[0])?.checked_mul(positive_resolution(&values[1])?)
        }
        _ => None,
    }
}

/// A literal-large `Surface` that is directly animated under at least one
/// Cairo profile. The 1,024-face boundary is tied to the checked-in
/// calibration evidence; no measured milliseconds enter the diagnostic.
pub struct CairoSurfaceMismatch;

impl Rule for CairoSurfaceMismatch {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP213
    }

    #[allow(
        clippy::too_many_lines,
        reason = "constructor/play aggregation and calibration evidence stay together"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cairo_profiles = cairo_profile_names(context);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((canonical, _)) = conclusive_target(profile, context.project_index(), call)
            else {
                continue;
            };
            if canonical != SURFACE {
                continue;
            }
            let Some(faces) = surface_faces(call) else {
                continue;
            };
            let face_num = Num::int(faces);
            if !CAIRO_SURFACE_FACE_GATE.confirmed_by(&face_num) {
                continue;
            }
            for scene in &context.lifecycle_facts().scenes {
                let objects: Vec<&ObjectId> = scene
                    .final_heap
                    .objects
                    .keys()
                    .filter(|object| {
                        object.cardinality == Cardinality::Singleton
                            && object.site.file == call.file
                            && object.site.start == u32::from(call.call_range.start())
                            && object.site.end == u32::from(call.call_range.end())
                            && scene
                                .final_heap
                                .object(object)
                                .is_some_and(|state| definitely_kind(state, SURFACE))
                    })
                    .collect();
                for object in objects {
                    let plays: Vec<&PlayFact> = scene
                        .plays
                        .iter()
                        .filter(|play| direct_target_play(play, object))
                        .collect();
                    if plays.is_empty() {
                        continue;
                    }
                    let frames = plays.iter().fold(Num::int(0), |total, play| {
                        total.add(&cairo_frames_at_play(context, play))
                    });
                    let mut evidence = BTreeMap::new();
                    evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                    evidence.insert("object_kind".to_owned(), json!(SURFACE));
                    evidence.insert(
                        "face_gate".to_owned(),
                        CAIRO_SURFACE_FACE_GATE.evidence(&face_num),
                    );
                    evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                    evidence.insert(
                        "calibration".to_owned(),
                        json!({
                            "source": "docs/research/perf-evidence.md",
                            "workload": "Cairo Surface (32, 32), 1,024 faces",
                            "portable_wall_time_claim": false,
                        }),
                    );
                    let related = plays
                        .iter()
                        .map(|play| {
                            related_site(
                                context,
                                play.site.file,
                                play.site.start,
                                play.site.end,
                                "this Cairo play captures the Surface every frame".to_owned(),
                            )
                        })
                        .collect();
                    let file = context.sources().file(call.file);
                    diagnostics.push(Diagnostic {
                        rule_id: MLP213.id.to_owned(),
                        severity: MLP213.default_severity,
                        confidence: MLP213.minimum_confidence,
                        path: file.relative_path().to_owned(),
                        primary_span: file.span_of_range(call.call_range),
                        message: format!(
                            "This {faces}-face Surface is captured for {} under Cairo. The calibrated workload is renderer-mismatched; consider OpenGL or a lower literal resolution.",
                            display_frames(&frames),
                        ),
                        explanation: Some(
                            "Cairo represents Surface as one VMobject face per u/v cell and repeatedly sorts, projects, shades, strokes, and rasterizes those faces when the Surface moves. The 1,024-face advisory boundary comes from the versioned calibration evidence, while the diagnostic keeps cost symbolic and never turns the recorded machine's milliseconds into a portable claim. OpenGL uses its mesh path for this workload."
                                .to_owned(),
                        ),
                        related_locations: related,
                        evidence,
                        estimated_cost: None,
                        applicable_profiles: cairo_profiles.clone(),
                        fix: None,
                    });
                    break;
                }
            }
        }
        diagnostics
    }
}

fn future_opacity_unchanged(
    scene: &SceneLifecycle,
    current_index: usize,
    object: &ObjectId,
) -> bool {
    for play in &scene.plays[current_index..] {
        if play.star_args {
            return false;
        }
        for animation in &play.animations {
            if animation.convertible != Truth::Yes {
                return false;
            }
            let Some(state) = &animation.state else {
                return false;
            };
            let mut possible_target = false;
            for target in &state.targets {
                match target.may_be_same(object) {
                    Truth::Yes => possible_target = true,
                    Truth::Maybe => return false,
                    Truth::No => {}
                }
            }
            if possible_target
                && (animation.channels_known != Truth::Yes
                    || state.write_channels.contains(&WriteChannel::Style)
                    || state.write_channels.contains(&WriteChannel::Opacity))
            {
                return false;
            }
        }
    }
    let Some(begin_index) = scene.events.iter().position(|traced| {
        matches!(
            &traced.event,
            Event::BeginPlay(begin)
                if begin.play_group == scene.plays[current_index].play_group.0
        )
    }) else {
        return false;
    };
    for traced in &scene.events[begin_index + 1..] {
        match &traced.event {
            Event::Mutate(mutation)
                if mutation.target.may_be_same(object) != Truth::No
                    && matches!(
                        mutation.kind,
                        MutationKind::Style | MutationKind::Opacity | MutationKind::Unknown
                    ) =>
            {
                return false;
            }
            Event::UnknownMutation(mutation)
                if mutation
                    .values
                    .iter()
                    .any(|value| value.may_be_same(object) != Truth::No) =>
            {
                return false;
            }
            Event::RegisterUpdater(_) => return false,
            _ => {}
        }
    }
    true
}

/// A visible path whose foreground stroke has exact opacity zero and
/// positive width during a certain Cairo target animation, with every
/// future opacity/style write proven absent.
pub struct TransparentStrokeCapture;

impl Rule for TransparentStrokeCapture {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP223
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cairo_profiles = cairo_profile_names(context);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        let mut seen = BTreeSet::new();
        for scene in &context.lifecycle_facts().scenes {
            if scene.constructor_state_unknown {
                continue;
            }
            for (play_index, play) in scene.plays.iter().enumerate() {
                if !no_active_updaters(scene, play) {
                    continue;
                }
                let Some(snapshot) = snapshot_before(scene, play) else {
                    continue;
                };
                for (object, state) in &snapshot.heap.objects {
                    if object.cardinality != Cardinality::Singleton
                        || stable_target_animations(play, object).is_none()
                        || !future_opacity_unchanged(scene, play_index, object)
                    {
                        continue;
                    }
                    if exact_value(&state.stroke_opacity) != Some(0.0)
                        || exact_value(&state.stroke_width).is_none_or(|width| width <= 0.0)
                        || state
                            .curve_count
                            .lower_bound()
                            .is_none_or(|curves| curves < 1.0)
                        || !seen.insert(object.clone())
                    {
                        continue;
                    }
                    let width = exact_value(&state.stroke_width).expect("checked");
                    let frames = cairo_frames_at_play(context, play);
                    let mut evidence = BTreeMap::new();
                    evidence.insert("scene".to_owned(), json!(scene.qualified_name));
                    evidence.insert("stroke_opacity".to_owned(), json!(0));
                    evidence.insert("stroke_width".to_owned(), json!(width));
                    evidence.insert("curves".to_owned(), num_bounds_json(&state.curve_count));
                    evidence.insert("frames".to_owned(), num_bounds_json(&frames));
                    evidence.insert("future_opacity_unchanged".to_owned(), json!(true));
                    let file = context.sources().file(play.site.file);
                    diagnostics.push(Diagnostic {
                        rule_id: MLP223.id.to_owned(),
                        severity: MLP223.default_severity,
                        confidence: MLP223.minimum_confidence,
                        path: file.relative_path().to_owned(),
                        primary_span: file.span_of_range(site_range(play.site.start, play.site.end)),
                        message: format!(
                            "This Cairo play processes a fully transparent stroke of width {width} for {}. Set stroke_width=0 when the stroke is intentionally invisible.",
                            display_frames(&frames),
                        ),
                        explanation: Some(
                            "Cairo's VMobject path pipeline still receives positive-width stroke geometry even when its stroke opacity is zero. The rule requires exact zero opacity, exact positive width, a non-empty path, a certain direct target play, no active updater, and proof that no later style or opacity write can make the stroke visible; otherwise it stays silent."
                                .to_owned(),
                        ),
                        related_locations: vec![object_related(
                            context,
                            object,
                            "the transparent positive-width path is created here",
                        )],
                        evidence,
                        estimated_cost: None,
                        applicable_profiles: cairo_profiles.clone(),
                        fix: None,
                    });
                }
            }
        }
        diagnostics
    }
}
