//! `MLP204`: scene-graph growth inside an updater (DESIGN §7.3).
//!
//! Liveness-gated (DESIGN §3.2/§3.3, §15): the `O(F)` growth claim
//! requires a play that provably executes the callback per frame
//! (`CostFacts::call_execution`) — an updater suspended during every
//! play, or one that only ever sees static waits, provably never grows
//! the scene, and the catalog minimum (`high`) forbids firing on
//! only-maybe evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::knowledge::SceneMembershipEffect;
use crate::rules::base::{Rule, RuleContext};

use super::support::{
    build_diagnostic, conclusive_target, execution_plays_json, merge_evidence, short_name,
};

/// Metadata for [`HotSceneGraphGrowth`].
pub const MLP204: RuleMetadata = RuleMetadata {
    id: "MLP204",
    summary: "Scene graph grows with a fresh mobject every frame inside an updater",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// Membership effects that *add* members — the growth pattern. Removals and
/// reorders of existing objects are the DESIGN §7.3 "reorder-only" branch,
/// which is `info` only for provably large scenes; scene size is not yet a
/// cost fact, so that branch stays silent (silence over a false positive).
const GROWTH_EFFECTS: [SceneMembershipEffect; 4] = [
    SceneMembershipEffect::Add,
    SceneMembershipEffect::AddForeground,
    SceneMembershipEffect::AddFixedInFrame,
    SceneMembershipEffect::AddFixedOrientation,
];

/// `Scene.add` (and the foreground / fixed-in-frame adders) called inside a
/// per-frame callback with a freshly constructed mobject argument: the
/// scene family grows `O(F)` and cumulative rasterization approaches
/// `O(F²)` (DESIGN §7.3 `MLP204`).
pub struct HotSceneGraphGrowth;

impl Rule for HotSceneGraphGrowth {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP204
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let cost = context.cost_facts();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for mutation in cost.scene_graph_mutations_in_hot_contexts() {
            // `adds_fresh_allocation == false` means "not proven fresh",
            // never "proven existing" — only the proven growth fires.
            if !mutation.adds_fresh_allocation
                || !GROWTH_EFFECTS.contains(&mutation.effect)
                || !seen.insert(mutation.call_index)
            {
                continue;
            }
            let call = &context.qualified_calls().calls[mutation.call_index];
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != mutation.symbol {
                continue;
            }
            let Some(hot) = cost.is_call_in_hot_context(mutation.call_index) else {
                continue;
            };
            // Liveness gate: the growth claim needs a play that provably
            // executes the callback per frame.
            let execution = cost.call_execution(mutation.call_index);
            if !execution.has_proven() {
                continue;
            }
            let file = context.sources().file(call.file);
            let method = short_name(&canonical);
            let mut evidence = BTreeMap::new();
            merge_evidence(&mut evidence, cost.evidence_for(mutation.call_index));
            evidence.insert("symbol".to_owned(), Value::String(canonical.clone()));
            evidence.insert(
                "execution".to_owned(),
                execution_plays_json(context, &execution),
            );
            evidence.insert(
                "adds_fresh_allocation".to_owned(),
                Value::Bool(mutation.adds_fresh_allocation),
            );
            evidence.insert(
                "growth".to_owned(),
                json!({"family": "O(F)", "raster": "O(F^2)"}),
            );
            diagnostics.push(build_diagnostic(
                &MLP204,
                context,
                file,
                call.call_range,
                format!(
                    "`{method}` runs inside a per-frame callback ({entry}) with a \
                     freshly constructed mobject: the scene family grows by one \
                     object per rendered frame (`O(F)`), and rasterizing the \
                     accumulated members across the play approaches `O(F²)`.",
                    entry = hot.entry.label(),
                ),
                "Every frame allocates a new mobject and inserts it into the \
                 persistent scene graph, so nothing is ever freed while the \
                 scene renders: frame N draws N accumulated objects. If a \
                 growing trail is intended, `TracedPath` (with a \
                 `dissipating_time` bound where possible) expresses it with a \
                 single mobject; otherwise create the object once outside the \
                 callback and mutate it per frame."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}
