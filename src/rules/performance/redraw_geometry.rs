//! `MLP216`: `always_redraw` rebuilding provably stable geometry
//! (DESIGN §7.3).
//!
//! Scope (documented limits — silence beyond them, DESIGN §15):
//!
//! - only lambda factories are analyzed: the returned-mobject proof comes
//!   from [`crate::semantic::interpreter::CallbackReturnFacts`] for the
//!   lambda span, and named-function factories cannot pin which
//!   constructor the returned object comes from without interprocedural
//!   dataflow;
//! - the factory body must be exactly one curated **leaf** constructor
//!   call (`Square` / `Circle` / `Line` / ... from the curated geometry
//!   table with its guards holding). Argument sub-expressions inside the
//!   constructor call (e.g. `Line(dot.get_center(), RIGHT)`) are fine —
//!   they parameterize placement, not topology. Chained method calls on
//!   the result (`Square().next_to(...)`) resolve to no candidates today
//!   and make the lambda's return fact `Maybe`, so they stay silent;
//! - containers (`VGroup` and friends) own no curated leaf geometry and
//!   never qualify.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::cost::contexts::HotEntryKind;
use crate::cost::geometry::{GeometryEntry, curated_geometry};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ArgShape;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Truth;

use super::support::{build_diagnostic, conclusive_target, merge_evidence, short_name};

/// Canonical id of the `always_redraw` factory helper.
const ALWAYS_REDRAW: &str = "manim.animation.updaters.mobject_update_utils.always_redraw";

/// One proven stable-geometry `always_redraw`.
struct StableRedraw {
    /// Call index of the `always_redraw` call (the diagnostic anchor).
    redraw_index: usize,
    /// Call index of the constructor inside the factory lambda.
    constructor_index: usize,
    /// Canonical id of the constructed class.
    canonical: String,
    /// The curated geometry proof.
    geometry: GeometryEntry,
}

/// Resolves every provable stable-geometry `always_redraw` of the project.
fn stable_redraws(context: &RuleContext<'_>) -> Vec<StableRedraw> {
    let Some(profile) = context.knowledge() else {
        return Vec::new();
    };
    let index = context.project_index();
    let calls = &context.qualified_calls().calls;
    let cost = context.cost_facts();
    let lifecycle = context.lifecycle_facts();
    let mut redraws = Vec::new();
    for (redraw_index, call) in calls.iter().enumerate() {
        let Some((canonical, _)) = conclusive_target(profile, index, call) else {
            continue;
        };
        if canonical != ALWAYS_REDRAW {
            continue;
        }
        // The factory must be a lambda whose every return path yields a
        // mobject (the object `always_redraw` re-becomes each frame).
        let Some(factory) = call.positional(0) else {
            continue;
        };
        if factory.shape != ArgShape::Lambda {
            continue;
        }
        let returns = lifecycle.callback_returns.lambda_at(
            call.file,
            factory.range.start().into(),
            factory.range.end().into(),
        );
        if returns.map(|fact| fact.returns_mobject) != Some(Truth::Yes) {
            continue;
        }
        // Calls lexically inside the factory lambda.
        let inner: Vec<(usize, &crate::frontend::index::QualifiedCall)> = calls
            .iter()
            .enumerate()
            .filter(|(inner_index, inner)| {
                *inner_index != redraw_index
                    && inner.file == call.file
                    && inner.call_range.start() >= factory.range.start()
                    && inner.call_range.end() <= factory.range.end()
            })
            .collect();
        // Exactly one curated leaf constructor (own curves > 0: containers
        // prove nothing about what the factory returns), with its guards
        // holding so the default point-generation path is certain.
        let constructors: Vec<(usize, String, GeometryEntry)> = inner
            .iter()
            .filter_map(|(inner_index, inner)| {
                let (inner_canonical, _) = conclusive_target(profile, index, inner)?;
                let entry = curated_geometry(&inner_canonical)?;
                (entry.curves > 0 && entry.guards_hold(Some(inner))).then_some((
                    *inner_index,
                    inner_canonical,
                    entry,
                ))
            })
            .collect();
        let [(constructor_index, constructed, geometry)] = constructors.as_slice() else {
            continue;
        };
        let constructor_range = calls[*constructor_index].call_range;
        // Every other call in the factory must sit inside the constructor
        // (an argument read). A call wrapping or beside the constructor
        // means the returned object may not be this construction.
        let all_argument_reads = inner.iter().all(|(inner_index, inner)| {
            inner_index == constructor_index
                || (inner.call_range.start() >= constructor_range.start()
                    && inner.call_range.end() <= constructor_range.end()
                    && inner.call_range != constructor_range)
        });
        if !all_argument_reads {
            continue;
        }
        // The construction must be provably per-frame via this factory.
        let hot_via_always_redraw = cost
            .hot_contexts_for(*constructor_index)
            .iter()
            .any(|hot| hot.entry == HotEntryKind::AlwaysRedraw);
        if !hot_via_always_redraw {
            continue;
        }
        redraws.push(StableRedraw {
            redraw_index,
            constructor_index: *constructor_index,
            canonical: constructed.clone(),
            geometry: *geometry,
        });
    }
    redraws
}

/// Call indices `MLP216` claims (the `always_redraw` call and the
/// constructor inside its factory): `MLP202` excludes these so the
/// implicit per-frame `become` of `always_redraw` is never double-reported
/// (DESIGN §7.3).
pub(crate) fn stable_redraw_call_indices(context: &RuleContext<'_>) -> BTreeSet<usize> {
    stable_redraws(context)
        .into_iter()
        .flat_map(|redraw| [redraw.redraw_index, redraw.constructor_index])
        .collect()
}

/// Metadata for [`StableGeometryRedraw`].
pub const MLP216: RuleMetadata = RuleMetadata {
    id: "MLP216",
    summary: "always_redraw rebuilds identical curated topology every frame",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "cost-facts"],
    supersedes: &[],
};

/// An `always_redraw` whose factory provably reconstructs the same curated
/// topology every frame — only placement / style parameters can change —
/// so each frame pays construction plus the factory's implicit `become`
/// (DESIGN §7.3 `MLP216`).
///
/// Stays info/medium: replacing the factory with relative affine updates
/// on a persistent object can accumulate drift, so the rewrite is not
/// equivalence-preserving in general (DESIGN §7.3 prose).
pub struct StableGeometryRedraw;

impl Rule for StableGeometryRedraw {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP216
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cost = context.cost_facts();
        let mut diagnostics = Vec::new();
        for redraw in stable_redraws(context) {
            let call = &context.qualified_calls().calls[redraw.redraw_index];
            let file = context.sources().file(call.file);
            let class = short_name(&redraw.canonical);
            let mut evidence = BTreeMap::new();
            merge_evidence(&mut evidence, cost.evidence_for(redraw.constructor_index));
            evidence.insert(
                "constructed".to_owned(),
                Value::String(redraw.canonical.clone()),
            );
            // Curated per-construction topology (proven constants from the
            // geometry table, not estimates).
            evidence.insert(
                "topology".to_owned(),
                json!({
                    "curves": redraw.geometry.curves,
                    "points": redraw.geometry.points,
                    "stable": true,
                }),
            );
            diagnostics.push(build_diagnostic(
                &MLP216,
                context,
                file,
                call.call_range,
                format!(
                    "This `always_redraw` factory rebuilds a `{class}` with \
                     identical topology ({curves} curves, {points} points) \
                     every frame: only placement or style parameters can \
                     differ between reconstructions.",
                    curves = redraw.geometry.curves,
                    points = redraw.geometry.points,
                ),
                format!(
                    "`always_redraw` calls the factory and `become`s the \
                     result on every frame, paying construction plus a full \
                     point/style/submobject rewrite for geometry that never \
                     changes shape. A persistent `{class}` with an \
                     `add_updater` applying the placement change avoids the \
                     per-frame reconstruction. Note that switching to \
                     *relative* affine updates can accumulate drift over many \
                     frames — prefer updaters that recompute the absolute \
                     placement — so this stays an advisory diagnostic."
                ),
                evidence,
            ));
        }
        diagnostics
    }
}
