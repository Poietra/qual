//! `MLC123`: an `ApplyFunction` callback provably returns no mobject
//! (DESIGN §7.1).
//!
//! `ApplyFunction.create_target()` calls `function(mobject.copy())` and
//! uses the return value as the transform target; a callback that returns
//! `None` (bare `return` or falling off the end) or a definite
//! non-mobject breaks the transform at play time.
//!
//! Callback resolution goes through the interpreter's
//! [`CallbackReturnFacts`]: lambdas by their exact source span, named
//! arguments by qualified name. Only [`ReturnFact::returns_mobject`]
//! `No` fires — an untracked return value is `Maybe` and stays silent, and
//! raise-only paths never count as a missing return (DESIGN §15).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ArgShape;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::ReturnFact;
use crate::semantic::values::Truth;

use super::support::{build_diagnostic, candidates_value, conclusive_target};

/// Canonical id of the `ApplyFunction` animation class.
const APPLY_FUNCTION: &str = "manim.animation.transform.ApplyFunction";

/// Metadata for [`ApplyFunctionCallbackNoMobject`].
pub const MLC123: RuleMetadata = RuleMetadata {
    id: "MLC123",
    summary: "ApplyFunction callback returns no mobject on some path",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

/// `ApplyFunction(callback, ...)` whose callback provably fails to return
/// a mobject on some normal return path (DESIGN §7.1 `MLC123`).
pub struct ApplyFunctionCallbackNoMobject;

impl Rule for ApplyFunctionCallbackNoMobject {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC123
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let callback_returns = &context.lifecycle_facts().callback_returns;
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != APPLY_FUNCTION {
                continue;
            }
            let Some(argument) = call.keyword("function").or_else(|| {
                if call.has_star_args {
                    None
                } else {
                    call.positional(0)
                }
            }) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let (fact, kind) = match &argument.shape {
                ArgShape::Lambda => (
                    callback_returns.lambda_at(
                        call.file,
                        argument.range.start().into(),
                        argument.range.end().into(),
                    ),
                    "lambda",
                ),
                ArgShape::Name => {
                    // The frontend proved the name resolves to a project
                    // function def; re-derive the qualified name and
                    // require the indexed signature to match, so a
                    // shadowing nested def never borrows the wrong fact.
                    let Some(signature) = &argument.callable_signature else {
                        continue;
                    };
                    let name = file.slice(argument.range);
                    let qualified = format!("{}.{name}", call.context.module);
                    if index.function_signature(&qualified) != Some(signature) {
                        continue;
                    }
                    (callback_returns.functions.get(&qualified), "function")
                }
                _ => continue,
            };
            let Some(fact) = fact else {
                continue;
            };
            if fact.returns_mobject != Truth::No {
                continue;
            }
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(canonical.clone()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            evidence.insert("callback".to_owned(), Value::String(kind.to_owned()));
            evidence.insert("returns_mobject".to_owned(), Value::String("no".to_owned()));
            evidence.insert(
                "bare_return_path".to_owned(),
                Value::String(truth_label(fact.has_bare_return_path).to_owned()),
            );
            evidence.insert(
                "fall_off_end_path".to_owned(),
                Value::String(truth_label(fact.has_no_return_path).to_owned()),
            );
            diagnostics.push(build_diagnostic(
                &MLC123,
                context,
                file,
                argument.range,
                format!(
                    "This `ApplyFunction` callback {reason}; `ApplyFunction` uses the \
                     callback's return value as the transform target, so the play \
                     fails. Return the (mutated) mobject from every path.",
                    reason = failure_reason(*fact),
                ),
                "`ApplyFunction.create_target()` calls `function(mobject.copy())` and \
                 raises `TypeError` unless the function returns a Mobject \
                 (transform.py `ApplyFunction.create_target`). A callback that \
                 mutates its argument but returns `None` — or returns a definite \
                 non-mobject such as an Animation — breaks the transform the moment \
                 the play begins."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

/// The proven defect, phrased for the message.
fn failure_reason(fact: ReturnFact) -> &'static str {
    if fact.has_bare_return_path == Truth::Yes {
        "returns nothing on some path (a bare `return` yields `None`)"
    } else if fact.has_no_return_path == Truth::Yes {
        "falls off the end without a `return` statement (yielding `None`)"
    } else {
        "provably returns a non-mobject value"
    }
}

/// Lowercase label of a [`Truth`] for the evidence map.
const fn truth_label(value: Truth) -> &'static str {
    match value {
        Truth::Yes => "yes",
        Truth::No => "no",
        Truth::Maybe => "maybe",
    }
}
