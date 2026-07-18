//! `MLP217`: frame-varying key with `use_svg_cache=True` in a hot
//! callback grows the process-global SVG cache every frame (DESIGN §7.3).
//!
//! The rule is enabled only when the selected knowledge profile *declares*
//! the process-global, unbounded SVG cache semantics
//! (`KnowledgeProfile::svg_cache`, `None` on `upstream_0_20` — the map
//! also exists upstream, but an undeclared profile keeps the rule inert,
//! DESIGN §7.3). The defect is distinct from `MLP226`: `MLP226` reports
//! per-frame construction cost and distinct disk assets, this rule
//! reports **unbounded process-global memory growth** (`O(F × family)` —
//! one retained deep family copy per distinct key, never evicted). The
//! DESIGN supersedes table orders neither above the other, so a
//! frame-varying `Text` / TeX key with the cache enabled carries both.
//!
//! Cache-flag resolution is conservative and uses the fork-verified
//! constructor defaults (`svg_mobject.py` / `text_mobject.py` /
//! `tex_mobject.py`): an explicit literal `use_svg_cache` wins; absent, the
//! class default applies (`Text` defaults to `False` in the fork, every
//! other cached class to `True`); a non-literal flag or a `**kwargs` splat
//! leaves the flag unknown — silence, never a guess.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::cost::estimator::{Evidence, num_bounds_json, symbolic_frames};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{LiteralFact, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;

use super::support::{
    build_diagnostic, conclusive_target, display_frames, execution_plays_json, merge_evidence,
    short_name,
};

/// Constructors that route through the process-global SVG mobject cache,
/// with the fork's `use_svg_cache` default (`SVGMobject.__init__` defaults
/// to `True`; the fork's `Text.__init__` explicitly defaults to `False`;
/// `MarkupText` and the TeX classes forward `**kwargs` to `SVGMobject`).
const SVG_CACHE_CONSTRUCTORS: [(&str, bool); 6] = [
    ("manim.mobject.svg.svg_mobject.SVGMobject", true),
    ("manim.mobject.text.tex_mobject.MathTex", true),
    ("manim.mobject.text.tex_mobject.SingleStringMathTex", true),
    ("manim.mobject.text.tex_mobject.Tex", true),
    ("manim.mobject.text.text_mobject.MarkupText", true),
    ("manim.mobject.text.text_mobject.Text", false),
];

/// Metadata for [`FrameVaryingSvgCacheGrowth`].
pub const MLP217: RuleMetadata = RuleMetadata {
    id: "MLP217",
    summary: "Frame-varying key with use_svg_cache=True grows the process-global SVG \
              cache every frame",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts", "local-fork-overlay"],
    supersedes: &[],
};

/// A hot SVG-cached construction whose key provably varies per frame while
/// the cache flag is provably on: every rendered frame inserts one deep
/// family copy into a never-evicted process-global map (DESIGN §7.3
/// `MLP217`).
pub struct FrameVaryingSvgCacheGrowth;

impl Rule for FrameVaryingSvgCacheGrowth {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP217
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // Enable-gate (DESIGN §7.3): only a profile that declares the
        // process-global, unbounded cache semantics supports the
        // memory-growth claim.
        let Some(cache) = profile.svg_cache() else {
            return Vec::new();
        };
        if cache.process_global != Some(true) || cache.unbounded != Some(true) {
            return Vec::new();
        }
        let index = context.project_index();
        let cost = context.cost_facts();
        let profiles = context.active_profiles();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for fact in cost.frame_varying_resource_keys() {
            let Some(default_enabled) = SVG_CACHE_CONSTRUCTORS
                .iter()
                .find(|(id, _)| *id == fact.symbol)
                .map(|(_, default)| *default)
            else {
                continue;
            };
            if !seen.insert(fact.call_index) {
                continue;
            }
            let call = &context.qualified_calls().calls[fact.call_index];
            if svg_cache_flag(call, default_enabled) != Some(true) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != fact.symbol {
                continue;
            }
            let Some(hot) = cost.is_call_in_hot_context(fact.call_index) else {
                continue;
            };
            // Liveness gate: the per-frame growth claim needs a play that
            // provably executes the callback per frame.
            let execution = cost.call_execution(fact.call_index);
            if !execution.has_proven() {
                continue;
            }
            let file = context.sources().file(call.file);
            let class = short_name(&canonical);

            let bounded_frames = cost.proven_frames_for_call(fact.call_index);
            let keys = bounded_frames.unwrap_or_else(symbolic_frames);
            let evidence =
                growth_evidence(context, cache, hot, &keys, &execution, &canonical, profiles);

            let quantified = match &keys {
                Num::Exact(_) | Num::Interval { .. } => format!(
                    " Across the {count} play(s) where this callback provably executes \
                     it may retain {frames} cache entries.",
                    count = execution.proven.len(),
                    frames = display_frames(&keys),
                ),
                Num::Symbol(_) | Num::Unknown => String::new(),
            };
            diagnostics.push(build_diagnostic(
                &MLP217,
                context,
                file,
                call.call_range,
                format!(
                    "`{class}` is constructed per frame with a frame-varying key while \
                     `use_svg_cache` is enabled: every rendered frame inserts a new \
                     entry — a deep copy of the whole constructed family — into the \
                     process-global SVG cache, which is never evicted \
                     (memory `O(F × family)`).{quantified}"
                ),
                "The selected knowledge profile declares the SVG mobject cache as \
                 process-global and unbounded, and hits are served as deep copies: \
                 a frame-varying key therefore buys no reuse while permanently \
                 retaining one family copy per rendered frame for the life of the \
                 process. Pass `use_svg_cache=False` for keys that vary per frame, \
                 or hoist the construction out of the per-frame callback. The \
                 diagnosed expression stays valid Manim, so no automatic fix is \
                 offered."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

/// The DESIGN §6.3 evidence map of one cache-growth diagnostic: the hot
/// context, the liveness plays, the retained-entry bound, and the curated
/// cache semantics the claim rests on.
fn growth_evidence(
    context: &RuleContext<'_>,
    cache: &crate::knowledge::SvgCacheFacts,
    hot: &crate::cost::contexts::HotContext,
    keys: &Num,
    execution: &crate::cost::CallExecution<'_>,
    canonical: &str,
    profiles: &[crate::config::model::RenderProfile],
) -> BTreeMap<String, Value> {
    let evidence_json = Evidence::for_hot_context(hot, keys.clone(), profiles).to_json();
    let mut evidence = BTreeMap::new();
    merge_evidence(&mut evidence, evidence_json);
    evidence.insert("symbol".to_owned(), Value::String(canonical.to_owned()));
    evidence.insert(
        "execution".to_owned(),
        execution_plays_json(context, execution),
    );
    evidence.insert(
        "retained_cache_entries".to_owned(),
        match keys {
            Num::Exact(_) | Num::Interval { .. } => num_bounds_json(keys),
            Num::Symbol(_) | Num::Unknown => Value::String("per-frame".to_owned()),
        },
    );
    evidence.insert(
        "memory_growth".to_owned(),
        Value::String("O(F × family): one retained deep family copy per key".to_owned()),
    );
    if !cache.keyed_by.is_empty() {
        evidence.insert(
            "cache_keyed_by".to_owned(),
            Value::Array(
                cache
                    .keyed_by
                    .iter()
                    .map(|component| Value::String(component.clone()))
                    .collect(),
            ),
        );
    }
    if cache.copies_on_hit == Some(true) {
        evidence.insert(
            "copies_on_hit".to_owned(),
            Value::String(
                "hits are served as deep copies, so caching never removes the \
                 per-construction copy cost"
                    .to_owned(),
            ),
        );
    }
    evidence
}

/// Conservative resolution of the effective `use_svg_cache` value: `None`
/// means unknown (silence). An explicit literal keyword wins; without one
/// a `**kwargs` splat hides the flag; otherwise the class default applies.
fn svg_cache_flag(call: &QualifiedCall, default_enabled: bool) -> Option<bool> {
    match call
        .arguments
        .iter()
        .find(|argument| argument.keyword.as_deref() == Some("use_svg_cache"))
    {
        Some(argument) => match argument.literal {
            Some(LiteralFact::Bool(value)) => Some(value),
            _ => None,
        },
        None if call.has_star_star_kwargs => None,
        None => Some(default_enabled),
    }
}
