//! Hot-construction rules: `MLP201` and `MLP226` (DESIGN §7.3).
//!
//! Both rules consume `CostFacts::constructions_in_hot_contexts` /
//! `CostFacts::frame_varying_resource_keys`. `MLP226` supersedes `MLP201`
//! on the same primary span (DESIGN §7.3 specificity ordering), so the
//! frame-varying call indices are excluded from `MLP201` here — the same
//! span never carries both diagnostics.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::cost::CostFacts;
use crate::cost::estimator::{Evidence, symbolic_frames};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;

use super::support::{
    build_diagnostic, conclusive_target, display_frames, enclosing_scene, merge_evidence,
    scene_frames_after, short_name,
};

/// Canonical ids of the provably expensive constructors `MLP201` covers
/// (DESIGN §7.3: `Text` / `MathTex` / `Tex` / `SVGMobject` / `Surface` and
/// friends; `Axes.plot` is not curated in `upstream_0_20`, so it cannot be
/// resolved yet). Cheap constructions (`Dot()`, `Square()`) stay out: per
/// DESIGN §4.4 hot context alone does not make them diagnostics.
const EXPENSIVE_CONSTRUCTORS: [&str; 8] = [
    "manim.mobject.svg.svg_mobject.SVGMobject",
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
    "manim.mobject.text.text_mobject.MarkupText",
    "manim.mobject.text.text_mobject.Text",
    "manim.mobject.three_d.three_dimensions.Surface",
    "manim.mobject.types.image_mobject.ImageMobject",
];

/// Canonical ids whose cache key is a Text / TeX resource key (`MLP226`).
const TEXT_TEX_CONSTRUCTORS: [&str; 5] = [
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
    "manim.mobject.text.text_mobject.MarkupText",
    "manim.mobject.text.text_mobject.Text",
];

/// Call indices `MLP226` claims (frame-varying Text / TeX keys): `MLP226`
/// supersedes `MLP201` on these spans.
fn frame_varying_text_tex_calls(cost: &CostFacts) -> BTreeSet<usize> {
    cost.frame_varying_resource_keys()
        .filter(|fact| TEXT_TEX_CONSTRUCTORS.contains(&fact.symbol.as_str()))
        .map(|fact| fact.call_index)
        .collect()
}

// ---------------------------------------------------------------------------
// MLP201: expensive construction inside a hot context.
// ---------------------------------------------------------------------------

/// Metadata for [`HotExpensiveConstruction`].
pub const MLP201: RuleMetadata = RuleMetadata {
    id: "MLP201",
    summary: "Expensive Text/TeX/SVG/Surface construction inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// `Text` / `MathTex` / `Tex` / `SVGMobject` / `Surface` constructed inside
/// an updater, `always_redraw` factory, or other per-frame callback
/// (DESIGN §7.3 `MLP201`).
pub struct HotExpensiveConstruction;

impl Rule for HotExpensiveConstruction {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP201
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let cost = context.cost_facts();
        let superseded = frame_varying_text_tex_calls(cost);
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for construction in cost.constructions_in_hot_contexts() {
            if !EXPENSIVE_CONSTRUCTORS.contains(&construction.symbol.as_str())
                || superseded.contains(&construction.call_index)
                || !seen.insert(construction.call_index)
            {
                continue;
            }
            let call = &context.qualified_calls().calls[construction.call_index];
            // Every candidate must agree on the constructed class.
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != construction.symbol {
                continue;
            }
            let Some(hot) = cost.is_call_in_hot_context(construction.call_index) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let class = short_name(&canonical);
            let mut evidence = BTreeMap::new();
            merge_evidence(&mut evidence, cost.evidence_for(construction.call_index));
            evidence.insert("symbol".to_owned(), Value::String(canonical.clone()));
            evidence.insert(
                "entry".to_owned(),
                Value::String(hot.entry.label().to_owned()),
            );
            diagnostics.push(build_diagnostic(
                &MLP201,
                context,
                file,
                call.call_range,
                format!(
                    "`{class}` is constructed inside a per-frame callback \
                     ({entry}): the full construction cost runs once per \
                     rendered frame of every play this callback is live in.",
                    entry = hot.entry.label(),
                ),
                format!(
                    "Constructing `{class}` includes shaping / parsing / family \
                     construction (and for TeX classes a cache lookup whose miss \
                     launches an external compiler), so a per-frame construction \
                     multiplies that cost by the frame count. If only a numeric \
                     value changes per frame, construct a `DecimalNumber` once and \
                     drive it with `set_value` from the updater instead of \
                     rebuilding `MathTex`/`Text` — the alternative changes \
                     typography, so no automatic fix is offered."
                ),
                evidence,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP226: frame-varying Text / TeX cache key in a hot callback.
// ---------------------------------------------------------------------------

/// Metadata for [`FrameVaryingResourceKey`].
pub const MLP226: RuleMetadata = RuleMetadata {
    id: "MLP226",
    summary: "Frame-varying Text/TeX cache key inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &["MLP201"],
};

/// A hot `Text` / `MathTex` / `Tex` construction whose cache key is a
/// provably frame-varying f-string, so `K_resource ≈ F` and each frame can
/// mint a distinct disk asset (DESIGN §7.3 `MLP226`).
pub struct FrameVaryingResourceKey;

impl Rule for FrameVaryingResourceKey {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP226
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let cost = context.cost_facts();
        let profiles = context.active_profiles();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for fact in cost.frame_varying_resource_keys() {
            if !TEXT_TEX_CONSTRUCTORS.contains(&fact.symbol.as_str())
                || !seen.insert(fact.call_index)
            {
                continue;
            }
            let call = &context.qualified_calls().calls[fact.call_index];
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != fact.symbol {
                continue;
            }
            let Some(hot) = cost.is_call_in_hot_context(fact.call_index) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let class = short_name(&canonical);

            // Bind the distinct-key count to real frame bounds only when
            // every subsequent play of the enclosing scene has a literal
            // duration; otherwise the estimate stays "per-frame".
            let bounded_frames = enclosing_scene(context.lifecycle_facts(), call)
                .and_then(|scene| scene_frames_after(scene, call, profiles));
            let keys = bounded_frames.clone().unwrap_or_else(symbolic_frames);

            let evidence_json = Evidence::for_hot_context(hot, keys.clone(), profiles).to_json();
            let mut evidence = BTreeMap::new();
            merge_evidence(&mut evidence, evidence_json);
            evidence.insert("symbol".to_owned(), Value::String(canonical.clone()));
            evidence.insert(
                "distinct_resource_keys".to_owned(),
                match &keys {
                    Num::Exact(_) | Num::Interval { .. } => {
                        crate::cost::estimator::num_bounds_json(&keys)
                    }
                    Num::Symbol(_) | Num::Unknown => Value::String("per-frame".to_owned()),
                },
            );

            let quantified = match &keys {
                Num::Exact(_) | Num::Interval { .. } => format!(
                    " Across the literal play durations of the enclosing scene \
                     this may create {frames} distinct keys.",
                    frames = display_frames(&keys),
                ),
                Num::Symbol(_) | Num::Unknown => String::new(),
            };
            diagnostics.push(build_diagnostic(
                &MLP226,
                context,
                file,
                call.call_range,
                format!(
                    "Each invocation constructs a `{class}` and performs a \
                     cache-key lookup, and this f-string key varies per frame: \
                     every rendered frame can mint a distinct Text/TeX cache key \
                     and disk asset (`K_resource ≈ F`).{quantified}"
                ),
                format!(
                    "A frame-varying key defeats the `{class}` cache: instead of \
                     one shaping/compile job reused every frame, each frame pays \
                     construction plus a cache miss, and for TeX classes each \
                     distinct key also launches the external TeX compiler and \
                     `dvisvgm`, leaving one disk asset per key. If only a numeric \
                     value changes, construct a `DecimalNumber` once and drive it \
                     with `set_value` from the updater; the diagnosed expression \
                     stays valid Manim, so no automatic fix is offered."
                ),
                evidence,
            ));
        }
        diagnostics
    }
}
