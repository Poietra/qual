//! Shared fact-resolution, evidence, and display helpers for the
//! performance rules and the `manim-lint cost` report.
//!
//! Every helper here is conservative: a call target counts as resolved only
//! when every frontend candidate maps to the same curated knowledge symbol,
//! and every quantity is either derived from literal facts or reported as
//! "unknown" / "per-frame" — never a fabricated number (DESIGN §15
//! invariants 2 and 9).

use std::collections::BTreeMap;

use rustpython_parser::text_size::TextRange;
use serde_json::{Value, json};

use crate::cost::CallExecution;
use crate::cost::contexts::resolve_candidate;
use crate::cost::liveness::{ExecutionCertainty, ExecutionPlay};
use crate::diagnostic::{Diagnostic, RuleMetadata};
use crate::frontend::index::{ProjectIndex, QualifiedCall, ReceiverKind};
use crate::knowledge::{KnowledgeProfile, SymbolEntry};
use crate::rules::base::RuleContext;
use crate::semantic::interpreter::PlayKind;
use crate::semantic::values::Num;
use crate::source::SourceFile;

/// Canonical id of `Scene.play`.
pub(crate) const SCENE_PLAY: &str = "manim.scene.scene.Scene.play";
/// Canonical id of `Scene.wait`.
pub(crate) const SCENE_WAIT: &str = "manim.scene.scene.Scene.wait";

/// Whether the receiver classification of `call` is complete enough to
/// support a diagnostic: a project receiver class with unresolved bases may
/// inherit an override this index cannot see, so it never confirms a
/// curated target.
fn receiver_conclusive(call: &QualifiedCall, index: &ProjectIndex) -> bool {
    let class = match &call.receiver {
        ReceiverKind::SelfScene => call.context.class_name.as_deref(),
        ReceiverKind::KnownInstance(kind) => Some(kind.as_str()),
        ReceiverKind::ModuleAlias | ReceiverKind::Direct => None,
        ReceiverKind::Unknown => return false,
    };
    match class.and_then(|id| index.classes.get(id)) {
        Some(record) => record.bases_fully_resolved,
        // External canonical class, or no receiver class at all.
        None => true,
    }
}

/// Resolves `call` to a single curated knowledge symbol.
///
/// Succeeds only when the candidate set is non-empty, the receiver is
/// conclusively classified, and *every* candidate resolves to the same
/// canonical id. Anything less is `None`: silence over a false positive.
pub(crate) fn conclusive_target<'p>(
    profile: &'p KnowledgeProfile,
    index: &ProjectIndex,
    call: &QualifiedCall,
) -> Option<(String, &'p SymbolEntry)> {
    if call.candidates.is_empty() || !receiver_conclusive(call, index) {
        return None;
    }
    let mut resolved: Option<(String, &'p SymbolEntry)> = None;
    for candidate in &call.candidates {
        let (id, entry) = resolve_candidate(profile, candidate)?;
        match &resolved {
            None => resolved = Some((id, entry)),
            Some((existing, _)) if *existing == id => {}
            Some(_) => return None,
        }
    }
    resolved
}

/// Whether `call` is a bound method call (`self.play(...)`) rather than an
/// unbound class-attribute call.
pub(crate) fn bound_receiver(call: &QualifiedCall) -> bool {
    matches!(
        call.receiver,
        ReceiverKind::SelfScene | ReceiverKind::KnownInstance(_)
    )
}

/// The unqualified class name of a canonical id (`MathTex` for
/// `manim.mobject.text.tex_mobject.MathTex`).
pub(crate) fn short_name(canonical: &str) -> &str {
    canonical.rsplit('.').next().unwrap_or(canonical)
}

/// Builds a diagnostic with the rule's default severity and minimum
/// confidence.
pub(crate) fn build_diagnostic(
    metadata: &'static RuleMetadata,
    context: &RuleContext<'_>,
    file: &SourceFile,
    range: TextRange,
    message: String,
    explanation: String,
    evidence: BTreeMap<String, Value>,
) -> Diagnostic {
    Diagnostic {
        rule_id: metadata.id.to_owned(),
        severity: metadata.default_severity,
        confidence: metadata.minimum_confidence,
        path: file.relative_path().to_owned(),
        primary_span: file.span_of_range(range),
        message,
        explanation: Some(explanation),
        related_locations: Vec::new(),
        evidence,
        estimated_cost: None,
        applicable_profiles: context.config().active_profile_names(),
        fix: None,
    }
}

/// Merges the fields of a JSON-object evidence value (the DESIGN §6.3
/// shape produced by `CostFacts::evidence_for` / `Evidence::to_json`) into
/// a diagnostic evidence map.
pub(crate) fn merge_evidence(evidence: &mut BTreeMap<String, Value>, value: Value) {
    if let Value::Object(fields) = value {
        for (key, field) in fields {
            evidence.insert(key, field);
        }
    }
}

/// The scene lifecycle whose class encloses `call`, if any.
pub(crate) fn enclosing_scene<'l>(
    lifecycle: &'l crate::semantic::interpreter::LifecycleFacts,
    call: &QualifiedCall,
) -> Option<&'l crate::semantic::interpreter::SceneLifecycle> {
    let class_name = call.context.class_name.as_ref()?;
    lifecycle
        .scenes
        .iter()
        .find(|scene| &scene.qualified_name == class_name)
}

/// `path:line:column` of one execution play's call site, for evidence and
/// report rows.
pub(crate) fn play_location(context: &RuleContext<'_>, play: &ExecutionPlay) -> String {
    let file = context.sources().file(play.site.file);
    let position = file.position_of_byte(play.site.start as usize);
    format!(
        "{path}:{line}:{column}",
        path = file.relative_path(),
        line = position.line,
        column = position.column,
    )
}

/// Machine-readable list of the execution plays a per-frame claim counts
/// (DESIGN §15: the claim must be auditable — each entry names the play's
/// span, kind, and whether execution there is proven or only possible).
pub(crate) fn execution_plays_json(
    context: &RuleContext<'_>,
    execution: &CallExecution<'_>,
) -> Value {
    let entry = |play: &ExecutionPlay| {
        json!({
            "location": play_location(context, play),
            "kind": match play.kind {
                PlayKind::Play => "play",
                PlayKind::Wait => "wait",
            },
            "certainty": match play.certainty {
                ExecutionCertainty::Proven => "proven",
                ExecutionCertainty::Maybe => "maybe",
            },
        })
    };
    let mut plays: Vec<Value> = execution.proven.iter().map(|play| entry(play)).collect();
    plays.extend(execution.maybe.iter().map(|play| entry(play)));
    json!({
        "plays": plays,
        "unresolved_entries": execution.unresolved,
    })
}

/// Renders a frame-count estimate for prose: `~120` / `~120 to ~480` when
/// bounded, `"per-frame"` otherwise — never a fabricated number.
pub(crate) fn display_frames(frames: &Num) -> String {
    match frames.bounds() {
        Some((Some(lower), Some(upper))) => {
            if format_count(lower) == format_count(upper) {
                format!("~{}", format_count(upper))
            } else {
                format!("~{} to ~{}", format_count(lower), format_count(upper))
            }
        }
        Some((None, Some(upper))) => format!("up to ~{}", format_count(upper)),
        Some((Some(lower), None)) => format!("at least ~{}", format_count(lower)),
        _ => "per-frame".to_owned(),
    }
}

/// Renders a duration in seconds: `2 s` / `2 s to 4 s` when bounded,
/// `"unknown"` otherwise.
pub(crate) fn display_seconds(duration: &Num) -> String {
    match duration.bounds() {
        Some((Some(lower), Some(upper))) => {
            if format_seconds(lower) == format_seconds(upper) {
                format!("{} s", format_seconds(upper))
            } else {
                format!("{} s to {} s", format_seconds(lower), format_seconds(upper))
            }
        }
        Some((Some(lower), None)) => format!("at least {} s", format_seconds(lower)),
        Some((None, Some(upper))) => format!("up to {} s", format_seconds(upper)),
        _ => "unknown".to_owned(),
    }
}

/// Formats a count bound without a trailing `.0` for integral values.
#[allow(
    clippy::cast_possible_truncation,
    reason = "integrality and 2^53 range are checked before the cast"
)]
fn format_count(bound: f64) -> String {
    const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53
    if bound.fract() == 0.0 && bound.abs() < EXACT_INT_LIMIT {
        format!("{}", bound as i64)
    } else {
        format!("{bound}")
    }
}

/// Formats a seconds bound, trimming a trailing `.0`.
fn format_seconds(bound: f64) -> String {
    format_count(bound)
}
