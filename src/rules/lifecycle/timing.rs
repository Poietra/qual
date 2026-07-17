//! Duration and wait rules: `MLC104`, `MLC106`.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, TextEdit,
};
use crate::frontend::index::{CallArgument, LiteralFact, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};
use crate::source::SourceFile;

use super::support::{
    SCENE_PLAY, SCENE_WAIT, WAIT_ANIMATION, bound_receiver, build_diagnostic, candidates_value,
    conclusive_target,
};

/// The literal numeric value of an argument, when statically known.
fn literal_number(argument: &CallArgument) -> Option<f64> {
    match argument.literal {
        #[allow(clippy::cast_precision_loss, reason = "durations are small numbers")]
        Some(LiteralFact::Int(value)) => Some(value as f64),
        Some(LiteralFact::Float(value)) => Some(value),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// MLC104: literal non-positive durations.
// ---------------------------------------------------------------------------

/// Metadata for [`NonPositiveDuration`].
pub const MLC104: RuleMetadata = RuleMetadata {
    id: "MLC104",
    summary: "Literal non-positive run_time or wait duration",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// Literal `run_time <= 0` on `play` / `Wait`, and literal non-positive
/// `wait` durations (DESIGN §7.1 `MLC104`).
pub struct NonPositiveDuration;

impl Rule for NonPositiveDuration {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC104
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            let mut suspects: Vec<(&CallArgument, &str)> = Vec::new();
            match canonical.as_str() {
                SCENE_PLAY if bound_receiver(call) => {
                    if let Some(argument) = call.keyword("run_time") {
                        suspects.push((argument, "run_time"));
                    }
                }
                SCENE_WAIT if bound_receiver(call) => {
                    if let Some(argument) = call.positional(0) {
                        suspects.push((argument, "duration"));
                    }
                    for keyword in ["duration", "run_time"] {
                        if let Some(argument) = call.keyword(keyword) {
                            suspects.push((argument, keyword));
                        }
                    }
                }
                WAIT_ANIMATION => {
                    if let Some(argument) = call.keyword("run_time") {
                        suspects.push((argument, "run_time"));
                    }
                }
                _ => {}
            }
            let file = context.sources().file(call.file);
            for (argument, parameter) in suspects {
                let Some(value) = literal_number(argument) else {
                    continue;
                };
                if value > 0.0 {
                    continue;
                }
                diagnostics.push(duration_diagnostic(
                    context, file, call, argument, &canonical, parameter, value,
                ));
            }
        }
        diagnostics
    }
}

#[allow(clippy::too_many_arguments, reason = "plain diagnostic assembly")]
fn duration_diagnostic(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    argument: &CallArgument,
    canonical: &str,
    parameter: &str,
    value: f64,
) -> Diagnostic {
    let text = file.slice(argument.range);
    let mut evidence = BTreeMap::new();
    evidence.insert("resolved".to_owned(), Value::String(canonical.to_owned()));
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence.insert("parameter".to_owned(), Value::String(parameter.to_owned()));
    evidence.insert(
        "literal_value".to_owned(),
        serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
    );
    build_diagnostic(
        &MLC104,
        context,
        file,
        argument.range,
        format!(
            "Use a positive `{parameter}`: the literal `{text}` is non-positive \
                 and `Scene` rejects it with `ValueError` before rendering."
        ),
        "Manim validates every play/wait duration (`Scene.validate_run_time`): a \
             run time of zero or less cannot produce any frame, so the render aborts \
             with `ValueError` the moment this call executes."
            .to_owned(),
        evidence,
    )
}

// ---------------------------------------------------------------------------
// MLC106: stop_condition combined with frozen_frame=True.
// ---------------------------------------------------------------------------

/// Metadata for [`FrozenFrameStopCondition`].
pub const MLC106: RuleMetadata = RuleMetadata {
    id: "MLC106",
    summary: "wait() combines stop_condition with frozen_frame=True",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `self.wait(stop_condition=..., frozen_frame=True)` (DESIGN §7.1
/// `MLC106`).
pub struct FrozenFrameStopCondition;

impl Rule for FrozenFrameStopCondition {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC106
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if !bound_receiver(call) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != SCENE_WAIT {
                continue;
            }
            let Some(stop_condition) = call.keyword("stop_condition") else {
                continue;
            };
            // An explicit `stop_condition=None` restates the default.
            if matches!(stop_condition.literal, Some(LiteralFact::NoneLit)) {
                continue;
            }
            let Some(frozen_frame) = call.keyword("frozen_frame") else {
                continue;
            };
            if !matches!(frozen_frame.literal, Some(LiteralFact::Bool(true))) {
                continue;
            }
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(SCENE_WAIT.to_owned()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            let mut diagnostic = build_diagnostic(
                &MLC106,
                context,
                file,
                frozen_frame.range,
                "Remove `frozen_frame=True` (or the `stop_condition`): a frozen wait \
                 renders one static frame, so the stop condition is never evaluated \
                 against changing state and Manim rejects the combination."
                    .to_owned(),
                "`Scene.wait(stop_condition=...)` needs a dynamic wait that re-renders \
                 and re-evaluates the condition every frame; `frozen_frame=True` \
                 explicitly freezes the wait to a single repeated frame. The two \
                 options contradict each other and `Wait` raises `ValueError`."
                    .to_owned(),
                evidence,
            );
            diagnostic.fix = Some(Fix {
                applicability: FixApplicability::Unsafe,
                message: "Change `frozen_frame=True` to `frozen_frame=False`".to_owned(),
                edits: vec![TextEdit {
                    path: file.relative_path().to_owned(),
                    span: file.span_of_range(frozen_frame.range),
                    replacement: "False".to_owned(),
                }],
            });
            diagnostics.push(diagnostic);
        }
        diagnostics
    }
}
