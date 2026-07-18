//! Receiver-sized hot-callback rules: `MLP202`, `MLP203`, and `MLP224`
//! (DESIGN §7.3).
//!
//! All three consume [`CostFacts::copy_align_targets_in_hot_contexts`] /
//! [`CostFacts::family_walk_queries_in_hot_contexts`]: hot `copy` /
//! `become` / `align` / family-walk calls whose receiver was resolved via
//! the updater-lambda-host contract (the callee is exactly
//! `<param>.<method>` directly inside a `Mobject.add_updater` lambda, so
//! the receiver is provably the updater host). Named-function callbacks
//! and other receivers produce no fact and stay silent — silence over a
//! guessed receiver (DESIGN §15).
//!
//! Emission gates (DESIGN §7.3): `MLP202` needs a **confirmed** large
//! family or large point count; `MLP203` needs a confirmed large family
//! (no repeated-query analysis is required once the family is confirmed
//! large); `MLP224` claims the `point_from_proportion` entries under the
//! large-points gate and supersedes `MLP203` on them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::cost::estimator::num_bounds_json;
use crate::cost::thresholds::{LARGE_FAMILY_GATE, LARGE_POINTS_GATE, Threshold};
use crate::cost::{CostFacts, HotTargetKind, HotTargetSizeFact};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;

use super::redraw_geometry::stable_redraw_call_indices;
use super::support::{build_diagnostic, merge_evidence};

/// The family-walk method routed to `MLP224` (DESIGN §7.3:
/// "`point_from_proportion` は `MLP224` を優先する").
const POINT_FROM_PROPORTION: &str = "point_from_proportion";

/// Shared emission walk: deduplicates facts per call index (several
/// scenes may register the same lambda) and hands each first fact whose
/// sizes pass `gate` to `emit`.
fn emit_gated<'f>(
    facts: impl Iterator<Item = &'f HotTargetSizeFact>,
    skip: &BTreeSet<usize>,
    gate: impl Fn(&HotTargetSizeFact) -> bool,
    mut emit: impl FnMut(&'f HotTargetSizeFact),
) {
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for fact in facts {
        if skip.contains(&fact.call_index) || seen.contains(&fact.call_index) || !gate(fact) {
            continue;
        }
        seen.insert(fact.call_index);
        emit(fact);
    }
}

/// Common evidence of a receiver-sized fact: the confirmed gate(s), the
/// receiver's real size bounds (unknown fields omitted, never fabricated),
/// the method, and the hot-context provenance.
fn receiver_evidence(
    cost: &CostFacts,
    fact: &HotTargetSizeFact,
    gates: &[(&Threshold, &Num)],
) -> BTreeMap<String, Value> {
    let mut evidence = BTreeMap::new();
    merge_evidence(&mut evidence, cost.evidence_for(fact.call_index));
    evidence.insert("method".to_owned(), Value::String(fact.method.clone()));
    evidence.insert("scene".to_owned(), Value::String(fact.scene.clone()));
    for (key, value) in [
        ("family_size", &fact.sizes.family),
        ("points", &fact.sizes.points),
        ("curves", &fact.sizes.curves),
    ] {
        let bounds = num_bounds_json(value);
        if !bounds.is_null() {
            evidence.insert(key.to_owned(), bounds);
        }
    }
    evidence.insert(
        "gates".to_owned(),
        Value::Array(
            gates
                .iter()
                .map(|(gate, value)| gate.evidence(value))
                .collect(),
        ),
    );
    evidence
}

/// Human-readable receiver size for prose: the family bound when known,
/// else the point bound — the caller only reaches this with at least one
/// confirmed gate, so a real number always exists.
fn display_size(fact: &HotTargetSizeFact) -> String {
    if let Some(lower) = fact.sizes.family.lower_bound() {
        return format!("at least {lower:.0} family members");
    }
    if let Some(lower) = fact.sizes.points.lower_bound() {
        return format!("at least {lower:.0} points");
    }
    "a confirmed large size".to_owned()
}

// ---------------------------------------------------------------------------
// MLP202: hot copy / become / align on a confirmed-large receiver.
// ---------------------------------------------------------------------------

/// Metadata for [`HotFamilyCopy`].
pub const MLP202: RuleMetadata = RuleMetadata {
    id: "MLP202",
    summary: "copy/become/align of a confirmed-large mobject inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "cost-facts"],
    supersedes: &[],
};

/// `copy` / `deepcopy` / `become` / `align_data` in a hot context, emitted
/// only when the receiver's family or point count is **confirmed** large
/// (DESIGN §7.3 `MLP202`: "large family / points が確定した場合だけ").
pub struct HotFamilyCopy;

impl Rule for HotFamilyCopy {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP202
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let cost = context.cost_facts();
        // DESIGN §7.3: the implicit `become` of an `always_redraw` factory
        // belongs to MLP201 / MLP216 and is never double-reported here.
        // Structurally the receiver facts only cover explicit
        // `<param>.<method>` calls in `add_updater` lambdas, so the claim
        // sets cannot collide today; the exclusion pins the contract
        // against future fact-layer growth.
        let mut skip = stable_redraw_call_indices(context);
        skip.extend(
            cost.constructions_in_hot_contexts()
                .map(|construction| construction.call_index),
        );
        let mut diagnostics = Vec::new();
        emit_gated(
            cost.copy_align_targets_in_hot_contexts(),
            &skip,
            |fact| {
                LARGE_FAMILY_GATE.confirmed_by(&fact.sizes.family)
                    || LARGE_POINTS_GATE.confirmed_by(&fact.sizes.points)
            },
            |fact| {
                let call = &context.qualified_calls().calls[fact.call_index];
                let file = context.sources().file(call.file);
                let evidence = receiver_evidence(
                    cost,
                    fact,
                    &[
                        (&LARGE_FAMILY_GATE, &fact.sizes.family),
                        (&LARGE_POINTS_GATE, &fact.sizes.points),
                    ],
                );
                let effect = match fact.kind {
                    HotTargetKind::Copy => "deep-copies the whole family",
                    HotTargetKind::Become => {
                        "rewrites the whole family's points, style, and submobjects"
                    }
                    HotTargetKind::Align | HotTargetKind::FamilyWalk => {
                        "re-aligns the whole family's point and curve data"
                    }
                };
                diagnostics.push(build_diagnostic(
                    &MLP202,
                    context,
                    file,
                    call.call_range,
                    format!(
                        "`{method}()` runs once per rendered frame on an updater \
                         host with {size}: each invocation {effect}.",
                        method = fact.method,
                        size = display_size(fact),
                    ),
                    format!(
                        "The updater contract passes the host mobject itself as \
                         the callback parameter, so this `{method}` walks the \
                         host's entire family every frame — the per-frame cost \
                         scales with the family and point counts ({citation}). \
                         Hoist the copy out of the callback (build it once and \
                         mutate it), or operate on the smallest submobject that \
                         actually changes.",
                        method = fact.method,
                        citation = LARGE_FAMILY_GATE.citation(),
                    ),
                    evidence,
                ));
            },
        );
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP203: hot family-walk / geometry query on a confirmed-large receiver.
// ---------------------------------------------------------------------------

/// Metadata for [`HotFamilyWalk`].
pub const MLP203: RuleMetadata = RuleMetadata {
    id: "MLP203",
    summary: "Family-walk query of a confirmed-large mobject inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "cost-facts"],
    supersedes: &[],
};

/// `get_family` / `family_members_with_points` / `get_all_points` /
/// arc-length queries in a hot context on a **confirmed** large family
/// (DESIGN §7.3 `MLP203`). `point_from_proportion` entries are routed to
/// the more specific `MLP224` and excluded here.
pub struct HotFamilyWalk;

impl Rule for HotFamilyWalk {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP203
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let cost = context.cost_facts();
        let mut diagnostics = Vec::new();
        emit_gated(
            cost.family_walk_queries_in_hot_contexts()
                .filter(|fact| fact.method != POINT_FROM_PROPORTION),
            &BTreeSet::new(),
            |fact| LARGE_FAMILY_GATE.confirmed_by(&fact.sizes.family),
            |fact| {
                let call = &context.qualified_calls().calls[fact.call_index];
                let file = context.sources().file(call.file);
                let evidence =
                    receiver_evidence(cost, fact, &[(&LARGE_FAMILY_GATE, &fact.sizes.family)]);
                diagnostics.push(build_diagnostic(
                    &MLP203,
                    context,
                    file,
                    call.call_range,
                    format!(
                        "`{method}()` runs once per rendered frame on an updater \
                         host with {size}: each invocation walks the whole \
                         family (and its point arrays) again.",
                        method = fact.method,
                        size = display_size(fact),
                    ),
                    format!(
                        "Family-walk and geometry queries recompute their result \
                         from every family member on each call; inside a \
                         per-frame callback that cost is multiplied by the frame \
                         count ({citation}). Cache the result outside the \
                         callback when the family does not change between \
                         frames, or query the specific submobject that matters.",
                        citation = LARGE_FAMILY_GATE.citation(),
                    ),
                    evidence,
                ));
            },
        );
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP224: hot point_from_proportion on a confirmed-long path.
// ---------------------------------------------------------------------------

/// Metadata for [`HotPointFromProportion`].
pub const MLP224: RuleMetadata = RuleMetadata {
    id: "MLP224",
    summary: "point_from_proportion on a confirmed-long path inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "cost-facts"],
    // DESIGN §7.3 specificity ordering: `MLP224 > MLP203`. The generic
    // rule also pre-filters `point_from_proportion` entries out, so the
    // same call never carries both diagnostics.
    supersedes: &["MLP203"],
};

/// `point_from_proportion` in a hot context on a receiver whose point
/// count is **confirmed** large (DESIGN §7.3 `MLP224`): each call
/// measures curve lengths across the whole path before interpolating.
///
/// The catalog's general `apply_function` leg is deliberately out of
/// scope: no fact layer proves that an `apply_function` callback iterates
/// the full point set of a specific receiver, so only the
/// `point_from_proportion` entries of the receiver-size facts are claimed.
pub struct HotPointFromProportion;

impl Rule for HotPointFromProportion {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP224
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let cost = context.cost_facts();
        let mut diagnostics = Vec::new();
        emit_gated(
            cost.family_walk_queries_in_hot_contexts()
                .filter(|fact| fact.method == POINT_FROM_PROPORTION),
            &BTreeSet::new(),
            |fact| LARGE_POINTS_GATE.confirmed_by(&fact.sizes.points),
            |fact| {
                let call = &context.qualified_calls().calls[fact.call_index];
                let file = context.sources().file(call.file);
                let evidence =
                    receiver_evidence(cost, fact, &[(&LARGE_POINTS_GATE, &fact.sizes.points)]);
                diagnostics.push(build_diagnostic(
                    &MLP224,
                    context,
                    file,
                    call.call_range,
                    format!(
                        "`point_from_proportion()` runs once per rendered frame \
                         on a path with {size}: every call measures the length \
                         of each curve in the path before locating the sample.",
                        size = display_size(fact),
                    ),
                    format!(
                        "`point_from_proportion` walks all curves of the \
                         receiver to build an arc-length table on every call; \
                         per frame on a long path that repeated measurement \
                         dominates ({citation}). Precompute the proportions \
                         outside the callback, or sample a shorter dedicated \
                         path object.",
                        citation = LARGE_POINTS_GATE.citation(),
                    ),
                    evidence,
                ));
            },
        );
        diagnostics
    }
}
