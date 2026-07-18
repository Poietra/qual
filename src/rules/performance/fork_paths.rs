//! `MLP225`: causal fork fast-path explanations (DESIGN §7.3).
//!
//! Not a normal warning. The rule explains, in the opt-in cost report,
//! which feature closes which of the local fork's fast paths — "fork-per-
//! play is serial fallback because of this Scene updater", "the packed
//! interpolation gate is closed by this updater-bearing family" — and
//! **never** recommends deleting a feature that may be intentional
//! (Scene updaters, foreground registration, custom rate functions,
//! stop conditions and the rest can be correct expression).
//!
//! Contract (DESIGN §7.3 prose, binding):
//!
//! - `default_enabled` is `false` and `required_capabilities` is
//!   `("cost-report", "local-fork-overlay")`: a normal `check` run never
//!   registers the rule; its home is the `cost` command's fork fast-path
//!   section, and an explicit `--select MLP225` opt-in evaluates the same
//!   engine as diagnostics.
//! - The rule is inert unless the loaded knowledge profile declares
//!   `fork_capabilities` (never under `upstream_0_20`).
//! - Fork loss is reported only for profiles whose `cairo_fork_workers`
//!   reaches the curated minimum — workers 0 is unrequested, not a loss.
//! - The monotonic renderer-wide disable is modeled: per-play
//!   independence is never assumed (see [`cost::fork`]).

use std::collections::BTreeMap;

use rustpython_parser::text_size::{TextRange, TextSize};
use serde_json::{Value, json};

use crate::cost::fork::{
    self, GateEvaluation, GateKind, GateVerdict, PlayGateOutcome, describe_outcome,
};
use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::AllocationSite;

/// Metadata for [`ForkFastPathLoss`].
pub const MLP225: RuleMetadata = RuleMetadata {
    id: "MLP225",
    summary: "Cost-report-only explanation of features that block local-fork fast paths",
    default_enabled: false,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["cost-report", "local-fork-overlay"],
    supersedes: &[],
};

/// Explains which statically proven feature closes which fork fast path
/// for which play (DESIGN §7.3 `MLP225`). Emits only proven losses —
/// blocked plays and monotonically disabled plays of a *requested* fast
/// path — as `info` diagnostics with the causal chain; unaudited plays
/// and unrequested pipelines produce nothing.
pub struct ForkFastPathLoss;

impl Rule for ForkFastPathLoss {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP225
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        // Inertness gate: fork fast-path interpretation is local-fork-
        // overlay only (DESIGN §7.3); upstream profiles declare nothing.
        if knowledge.fork_capabilities().is_none() {
            return Vec::new();
        }
        let facts = fork::evaluate(
            context.lifecycle_facts(),
            context.qualified_calls(),
            Some(knowledge),
            context.active_profiles(),
        );
        // Merge identical losses across render profiles: the profile list
        // becomes `applicable_profiles` instead of duplicate diagnostics.
        let mut merged: BTreeMap<(String, GateKind, usize), (GateOutcomeRef<'_>, Vec<String>)> =
            BTreeMap::new();
        for scene_paths in &facts.scenes {
            for profile_paths in &scene_paths.profiles {
                let gates = [
                    (GateKind::ForkPerPlay, &profile_paths.fork),
                    (GateKind::StaticLayers, &profile_paths.static_layers),
                    (GateKind::BulkInterpolation, &profile_paths.bulk),
                ];
                for (gate, evaluation) in gates {
                    let GateEvaluation::Plays(outcomes) = evaluation else {
                        continue;
                    };
                    for outcome in outcomes {
                        if !matches!(
                            outcome.verdict,
                            GateVerdict::Blocked(_) | GateVerdict::MonotonicallyDisabled { .. }
                        ) {
                            continue;
                        }
                        merged
                            .entry((scene_paths.scene.clone(), gate, outcome.ordinal))
                            .or_insert_with(|| {
                                (
                                    GateOutcomeRef {
                                        scene: &scene_paths.scene,
                                        gate,
                                        outcome,
                                    },
                                    Vec::new(),
                                )
                            })
                            .1
                            .push(profile_paths.profile.clone());
                    }
                }
            }
        }
        merged
            .into_values()
            .map(|(loss, profiles)| build_loss_diagnostic(context, &loss, profiles))
            .collect()
    }
}

/// One proven loss picked out of the gate evaluation.
struct GateOutcomeRef<'a> {
    scene: &'a str,
    gate: GateKind,
    outcome: &'a PlayGateOutcome,
}

/// A related location pointing at `site` with `message`.
fn related_at(context: &RuleContext<'_>, site: AllocationSite, message: String) -> RelatedLocation {
    let file = context.sources().file(site.file);
    RelatedLocation {
        path: file.relative_path().to_owned(),
        span: file.span_of_range(TextRange::new(
            TextSize::from(site.start),
            TextSize::from(site.end),
        )),
        message,
    }
}

/// The evidence map and cause-span related locations of one loss.
fn loss_evidence(
    context: &RuleContext<'_>,
    loss: &GateOutcomeRef<'_>,
) -> (BTreeMap<String, Value>, Vec<RelatedLocation>) {
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "fast_path".to_owned(),
        Value::String(loss.gate.label().to_owned()),
    );
    evidence.insert("scene".to_owned(), Value::String(loss.scene.to_owned()));
    evidence.insert("play_ordinal".to_owned(), json!(loss.outcome.ordinal));
    let mut related = Vec::new();
    match &loss.outcome.verdict {
        GateVerdict::Blocked(causes) => {
            evidence.insert(
                "blockers".to_owned(),
                Value::Array(
                    causes
                        .iter()
                        .map(|cause| Value::String(cause.blocker.as_str().to_owned()))
                        .collect(),
                ),
            );
            related.extend(causes.iter().filter_map(|cause| {
                cause
                    .site
                    .map(|site| related_at(context, site, cause.detail.clone()))
            }));
        }
        GateVerdict::MonotonicallyDisabled {
            first_ordinal,
            first_site,
            first_blocker,
        } => {
            evidence.insert(
                "blockers".to_owned(),
                Value::Array(vec![Value::String("parent_encoder_opened".to_owned())]),
            );
            evidence.insert("first_serial_play".to_owned(), json!(first_ordinal));
            related.push(related_at(
                context,
                *first_site,
                format!(
                    "first serially rendered play ({blocker}); a rendered serial play \
                     opens the parent encoder",
                    blocker = first_blocker.as_str(),
                ),
            ));
        }
        GateVerdict::Clear | GateVerdict::NotApplicable { .. } | GateVerdict::Unaudited { .. } => {}
    }
    (evidence, related)
}

fn build_loss_diagnostic(
    context: &RuleContext<'_>,
    loss: &GateOutcomeRef<'_>,
    profiles: Vec<String>,
) -> Diagnostic {
    let sources = context.sources();
    let locate = |site: AllocationSite| {
        let file = sources.file(site.file);
        let position = file.position_of_byte(site.start as usize);
        format!(
            "{path}:{line}:{column}",
            path = file.relative_path(),
            line = position.line,
            column = position.column,
        )
    };
    let description = describe_outcome(loss.gate, loss.outcome, &locate);
    let file = sources.file(loss.outcome.site.file);
    let range = TextRange::new(
        TextSize::from(loss.outcome.site.start),
        TextSize::from(loss.outcome.site.end),
    );
    let (evidence, related) = loss_evidence(context, loss);

    let mut explanation = String::from(
        "This is a causal cost explanation, not a defect report: the named feature \
         can be correct expression, and removing it would change the scene's \
         meaning. The report only states which fast path the current fork keeps \
         closed for this play and why.",
    );
    if let Some(measured) = loss.gate.measured_evidence() {
        explanation.push(' ');
        explanation.push_str("Calibration evidence: ");
        explanation.push_str(measured);
        explanation.push('.');
    }

    Diagnostic {
        rule_id: MLP225.id.to_owned(),
        severity: MLP225.default_severity,
        confidence: MLP225.minimum_confidence,
        path: file.relative_path().to_owned(),
        primary_span: file.span_of_range(range),
        message: format!(
            "{gate}: play #{ordinal} of {scene}: {description}",
            gate = loss.gate.label(),
            ordinal = loss.outcome.ordinal,
            scene = loss.scene,
        ),
        explanation: Some(explanation),
        related_locations: related,
        evidence,
        estimated_cost: None,
        applicable_profiles: profiles,
        fix: None,
    }
}
