//! `MLP211`: large per-frame allocation inside a hot context
//! (DESIGN §7.3).
//!
//! Two provable arms (both DESIGN §7.3 emission-gate arms):
//!
//! - a recognized ndarray construction whose literal shape statically
//!   proves at least 64 KiB per invocation
//!   ([`crate::cost::CostFacts::per_frame_allocation_bytes`], the
//!   [`PER_FRAME_ALLOCATION_GATE`]);
//! - a hot call whose resolved target object has a **confirmed** large
//!   point count ([`LARGE_POINTS_GATE`]).
//!
//! The catalog's third arm — any allocation inside a loop over the family
//! within a hot callback — is deliberately out of scope: hot-context
//! provenance chains record entry points and helper calls, not enclosing
//! loops inside callback bodies, so "family loop" containment is not
//! provable from the current fact layers and is never guessed.
//!
//! Small coordinate vectors are excluded by the byte gate itself, and
//! unknown shapes / dtypes stay `Unknown` (never guessed) in the cost
//! layer.
//!
//! Liveness (DESIGN §3.2/§3.3, §15): a call whose callback provably never
//! executes per frame (suspended during every play, only static waits)
//! stays silent. When execution is proven, the message states the
//! per-frame claim; when it is only possible (maybe plays or an entry the
//! lifecycle cannot resolve), the rule still fires at its `medium`
//! catalog confidence but with qualitative wording — the allocation size
//! itself is statically proven either way.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::cost::estimator::num_bounds_json;
use crate::cost::thresholds::{LARGE_POINTS_GATE, PER_FRAME_ALLOCATION_GATE};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;

use super::support::{build_diagnostic, execution_plays_json, merge_evidence};

/// Metadata for [`HotLargeAllocation`].
pub const MLP211: RuleMetadata = RuleMetadata {
    id: "MLP211",
    summary: "Large per-frame allocation inside a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    // DESIGN §7.3 declares `MLP220 > MLP211`; the shared same-span dedup
    // in the application layer enforces it (a `TracedPath` constructor and
    // an allocation call never share a span today, but the catalog edge is
    // owned by MLP220's metadata).
    supersedes: &[],
};

/// Renders a byte bound for prose (`800 KiB`, `1.2 MiB`).
fn display_bytes(bytes: f64) -> String {
    if bytes >= 1024.0 * 1024.0 {
        format!("{:.1} MiB", bytes / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", bytes / 1024.0)
    }
}

/// A statically proven large allocation running once per rendered frame
/// (DESIGN §7.3 `MLP211`).
pub struct HotLargeAllocation;

impl Rule for HotLargeAllocation {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP211
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let cost = context.cost_facts();
        // Call indices already owned by the receiver-sized rules
        // (MLP202 / MLP203 / MLP224): their copy / query facts also carry
        // large point counts, and one call must yield one diagnostic.
        let receiver_owned: BTreeSet<usize> = cost
            .hot_target_sizes
            .iter()
            .map(|fact| fact.call_index)
            .collect();
        let mut diagnostics = Vec::new();
        for &call_index in cost.hot.call_contexts.keys() {
            let bytes = cost.per_frame_allocation_bytes(call_index);
            let bytes_confirmed = PER_FRAME_ALLOCATION_GATE.confirmed_by(&bytes);
            let points = if bytes_confirmed || receiver_owned.contains(&call_index) {
                Num::Unknown
            } else {
                cost.points_of_target(call_index)
            };
            let points_confirmed = LARGE_POINTS_GATE.confirmed_by(&points);
            if !bytes_confirmed && !points_confirmed {
                continue;
            }
            // Liveness: provably-never-executing callbacks stay silent;
            // proven execution supports the per-frame claim, only-maybe
            // execution keeps qualitative wording (medium confidence).
            let execution = cost.call_execution(call_index);
            if !execution.has_proven() && !execution.possibly_executes() {
                continue;
            }
            let frequency = if execution.has_proven() {
                "runs once per rendered frame"
            } else {
                "sits in a per-frame callback (though no play provably \
                 executes it)"
            };
            let call = &context.qualified_calls().calls[call_index];
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            merge_evidence(&mut evidence, cost.evidence_for(call_index));
            evidence.insert(
                "execution".to_owned(),
                execution_plays_json(context, &execution),
            );
            let (gate_evidence, message) = if bytes_confirmed {
                evidence.insert("bytes_per_invocation".to_owned(), num_bounds_json(&bytes));
                let lower = bytes
                    .lower_bound()
                    .expect("a confirmed gate has a proven lower bound");
                (
                    PER_FRAME_ALLOCATION_GATE.evidence(&bytes),
                    format!(
                        "This call allocates at least {size} per invocation and \
                         {frequency}.",
                        size = display_bytes(lower),
                    ),
                )
            } else {
                evidence.insert("points".to_owned(), num_bounds_json(&points));
                let lower = points
                    .lower_bound()
                    .expect("a confirmed gate has a proven lower bound");
                (
                    LARGE_POINTS_GATE.evidence(&points),
                    format!(
                        "This call materializes an object with at least \
                         {lower:.0} points and {frequency}."
                    ),
                )
            };
            evidence.insert("gates".to_owned(), Value::Array(vec![gate_evidence]));
            diagnostics.push(build_diagnostic(
                &MLP211,
                context,
                file,
                call.call_range,
                message,
                format!(
                    "A fresh large buffer per frame multiplies allocator and \
                     memory-bandwidth cost by the frame count ({citation}). \
                     Allocate the buffer once outside the callback and fill it \
                     in place (e.g. `numpy` `out=` parameters or slice \
                     assignment), or shrink it to what one frame actually \
                     needs.",
                    citation = PER_FRAME_ALLOCATION_GATE.citation(),
                ),
                evidence,
            ));
        }
        diagnostics
    }
}
