//! `MLP221`: a literal `t_range` / `x_range` step proves an excessive
//! sample count for `ParametricFunction` / `Axes.plot` (DESIGN §7.3).
//!
//! Resolution: a candidate is accepted on two bases only (never a
//! name-string guess):
//!
//! - the knowledge profile resolves it — `ParametricFunction` and
//!   `Axes.plot` are curated in `upstream_0_20`, so star-import aliases
//!   (`from manim import *`) resolve through the profile's exports — or
//! - the frontend's import resolution produced exactly the canonical
//!   module path (`from manim.mobject.graphing.functions import
//!   ParametricFunction`, or a method call on a tracked instance of the
//!   canonical `Axes` class) — unambiguous import evidence.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::cost::contexts::resolve_candidate;
use crate::cost::thresholds::Threshold;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{LiteralFact, QualifiedCall, ReceiverKind};
use crate::knowledge::KnowledgeProfile;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;

use super::support::build_diagnostic;

/// Canonical id of `ParametricFunction` (functions.py; verified against
/// the Manim v0.20 checkout).
const PARAMETRIC_FUNCTION: &str = "manim.mobject.graphing.functions.ParametricFunction";
/// Canonical id of `Axes.plot` (`coordinate_systems.py`).
const AXES_PLOT: &str = "manim.mobject.graphing.coordinate_systems.Axes.plot";

/// `MLP221` fires only on a provable sample count of at least this many
/// evaluations: `ParametricFunction` invokes the Python callback once
/// per `np.arange(t_min, t_max, t_step)` sample (functions.py
/// `generate_points`) and `make_smooth` post-processes every stored
/// point, so five-digit counts dominate construction.
pub const MLP221_SAMPLE_GATE: Threshold = Threshold {
    name: "parametric-sample-gate",
    quantity: "N_samples",
    minimum: 10_000.0,
    rule_ids: &["MLP221"],
};

/// Metadata for [`ExcessiveSampleCount`].
pub const MLP221: RuleMetadata = RuleMetadata {
    id: "MLP221",
    summary: "Literal t_range/x_range step proves an excessive sample count",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// A `ParametricFunction(...)` / `axes.plot(...)` whose literal
/// three-element range proves a sample count above
/// [`MLP221_SAMPLE_GATE`] (DESIGN §7.3 `MLP221`). The sample count is
/// interval-evaluated from the literal `(start, end, step)` tuple only —
/// non-literal ranges are never guessed.
pub struct ExcessiveSampleCount;

/// What the call constructs, with the basis of the resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampledTarget {
    Parametric,
    AxesPlot,
}

/// Resolves the call to a sampled-construction target. Every candidate
/// must agree; the knowledge profile wins when it can resolve, the exact
/// canonical module path is accepted otherwise.
fn sampled_target(
    knowledge: &KnowledgeProfile,
    index: &crate::frontend::index::ProjectIndex,
    call: &QualifiedCall,
) -> Option<(SampledTarget, &'static str)> {
    if call.candidates.is_empty() {
        return None;
    }
    // A project receiver class with unresolved bases may override `plot`.
    if let ReceiverKind::KnownInstance(kind) = &call.receiver {
        if let Some(record) = index.classes.get(kind) {
            if !record.bases_fully_resolved {
                return None;
            }
        }
    }
    let mut resolved: Option<(SampledTarget, &'static str)> = None;
    for candidate in &call.candidates {
        let (canonical, basis) = match resolve_candidate(knowledge, candidate) {
            Some((canonical, _)) => (canonical, "knowledge-profile"),
            None => (candidate.clone(), "canonical-module-import"),
        };
        let target = match canonical.as_str() {
            PARAMETRIC_FUNCTION => SampledTarget::Parametric,
            AXES_PLOT => SampledTarget::AxesPlot,
            _ => return None,
        };
        match resolved {
            None => resolved = Some((target, basis)),
            Some((existing, _)) if existing == target => {}
            Some(_) => return None,
        }
    }
    resolved
}

/// The literal three-element `(start, end, step)` range, when every
/// element is an int / float literal.
fn literal_range3(argument: &crate::frontend::index::CallArgument) -> Option<(f64, f64, f64)> {
    let (LiteralFact::Tuple(elements) | LiteralFact::List(elements)) = argument.literal.as_ref()?
    else {
        return None;
    };
    let [start, end, step] = elements.as_slice() else {
        return None;
    };
    let as_f64 = |fact: &LiteralFact| -> Option<f64> {
        match fact {
            LiteralFact::Int(value) =>
            {
                #[allow(clippy::cast_precision_loss, reason = "range literals far below 2^52")]
                Some(*value as f64)
            }
            LiteralFact::Float(value) => Some(*value),
            _ => None,
        }
    };
    Some((as_f64(start)?, as_f64(end)?, as_f64(step)?))
}

impl Rule for ExcessiveSampleCount {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP221
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for (call_index, call) in context.qualified_calls().calls.iter().enumerate() {
            let Some((target, basis)) = sampled_target(knowledge, index, call) else {
                continue;
            };
            let (range_keyword, symbol) = match target {
                SampledTarget::Parametric => ("t_range", PARAMETRIC_FUNCTION),
                SampledTarget::AxesPlot => ("x_range", AXES_PLOT),
            };
            let argument = call
                .keyword(range_keyword)
                .or_else(|| call.positional(1))
                .filter(|argument| {
                    argument.keyword.is_none() || argument.keyword.as_deref() == Some(range_keyword)
                });
            let Some((start, end, step)) = argument.and_then(literal_range3) else {
                continue;
            };
            if !(step > 0.0 && end > start && step.is_finite() && start.is_finite()) {
                continue;
            }
            let samples = ((end - start) / step).ceil();
            let samples_num = Num::float(samples);
            if !MLP221_SAMPLE_GATE.confirmed_by(&samples_num) {
                continue;
            }
            let vectorized = call
                .keyword("use_vectorized")
                .and_then(|argument| match argument.literal.as_ref() {
                    Some(LiteralFact::Bool(value)) => Some(*value),
                    _ => None,
                })
                .unwrap_or(false);
            let hot = context.cost_facts().is_call_in_hot_context(call_index);
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(symbol.to_owned()));
            evidence.insert("resolution".to_owned(), Value::String(basis.to_owned()));
            evidence.insert(
                "range".to_owned(),
                json!({"start": start, "end": end, "step": step}),
            );
            evidence.insert("samples".to_owned(), json!(samples));
            evidence.insert(
                "threshold".to_owned(),
                MLP221_SAMPLE_GATE.evidence(&samples_num),
            );
            evidence.insert("citation".to_owned(), json!(MLP221_SAMPLE_GATE.citation()));
            evidence.insert("use_vectorized".to_owned(), json!(vectorized));
            if hot.is_some() {
                evidence.insert(
                    "hot_context".to_owned(),
                    json!("construction reachable per frame: samples × frames"),
                );
            }
            let calls_clause = if vectorized {
                format!(
                    "about {samples:.0} points are generated and post-processed \
                     (the vectorized callback is invoked once over the whole array)"
                )
            } else {
                format!(
                    "about {samples:.0} Python callback invocations run at \
                     construction, one per sample, before smoothing post-processes \
                     every stored point"
                )
            };
            let vector_note = if vectorized {
                ""
            } else {
                " If the function is built from NumPy ufuncs only, \
                 `use_vectorized=True` evaluates the whole sample array in one \
                 call — do not enable it for arbitrary Python callbacks."
            };
            diagnostics.push(build_diagnostic(
                &MLP221,
                context,
                file,
                call.call_range,
                format!(
                    "The literal range ({start}, {end}, {step}) proves about \
                     {samples:.0} samples: {calls_clause}. Increase the step (or \
                     narrow the range) unless this resolution is intended.{vector_note}",
                ),
                "ParametricFunction samples `np.arange(t_min, t_max, t_step)` and \
                 calls the plotted function once per sample (functions.py \
                 generate_points; Axes.plot builds a ParametricFunction from \
                 x_range the same way). The sample count is fixed at construction \
                 by the literal range, independent of how large the curve appears \
                 on screen."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}
