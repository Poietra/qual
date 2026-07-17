//! Updater signature rule: `MLC105`.
//!
//! Manim's calling conventions (DESIGN §3.3):
//!
//! - `Mobject.add_updater(callback)` inspects the callback signature; when
//!   any parameter is *named* `dt` the callback is called positionally as
//!   `callback(mobject, dt)`, otherwise as `callback(mobject)`.
//! - `Scene.add_updater(callback)` always calls `callback(dt)` with one
//!   positional argument.
//!
//! The rule simulates Python positional binding over the declared
//! signature; it fires only when the simulated call provably cannot bind.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{CallArgument, CallableSignature, ParamKind, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};

use super::support::{
    MOBJECT_ADD_UPDATER, SCENE_ADD_UPDATER, bound_receiver, build_diagnostic, candidates_value,
    conclusive_target,
};

/// Metadata for [`UpdaterCannotBind`].
pub const MLC105: RuleMetadata = RuleMetadata {
    id: "MLC105",
    summary: "Updater callback cannot bind to Manim's positional invocation",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// Which updater registration contract applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Contract {
    /// `Mobject.add_updater`: `(mobject, dt)` or `(mobject)` depending on
    /// whether a parameter is named `dt`.
    Mobject,
    /// `Scene.add_updater`: always `(dt)`.
    Scene,
}

/// Whether `signature` accepts `count` positional arguments and no
/// keywords, mirroring `inspect.Signature.bind`.
fn binds_positionally(signature: &CallableSignature, count: usize) -> bool {
    let mut remaining = count;
    for parameter in &signature.params {
        match parameter.kind {
            ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword => {
                if remaining > 0 {
                    remaining -= 1;
                } else if !parameter.has_default {
                    // A required parameter receives no value.
                    return false;
                }
            }
            ParamKind::VarArgs => {
                remaining = 0;
            }
            ParamKind::KeywordOnly => {
                if !parameter.has_default {
                    // No keywords are passed; a required keyword-only
                    // parameter can never be satisfied.
                    return false;
                }
            }
            ParamKind::KwArgs => {}
        }
    }
    // Leftover positional arguments have no slot.
    remaining == 0
}

/// The callback argument of a confirmed `add_updater` call.
fn callback_argument(call: &QualifiedCall, contract: Contract) -> Option<&CallArgument> {
    let keyword = match contract {
        Contract::Mobject => "update_function",
        Contract::Scene => "func",
    };
    call.positional(0).or_else(|| call.keyword(keyword))
}

/// Updater callbacks that cannot bind to Manim's actual invocation
/// (DESIGN §7.1 `MLC105`).
pub struct UpdaterCannotBind;

impl Rule for UpdaterCannotBind {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC105
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
            let contract = match canonical.as_str() {
                MOBJECT_ADD_UPDATER => Contract::Mobject,
                SCENE_ADD_UPDATER => Contract::Scene,
                _ => continue,
            };
            let Some(callback) = callback_argument(call, contract) else {
                continue;
            };
            let Some(signature) = &callback.callable_signature else {
                // Unknown callable: silence, never a guess.
                continue;
            };
            // A leading `self` parameter means the reference may be an
            // unbound method; binding cannot be proven either way.
            if signature
                .params
                .first()
                .is_some_and(|parameter| parameter.name == "self")
            {
                continue;
            }
            let has_dt = signature
                .params
                .iter()
                .any(|parameter| parameter.name == "dt");
            let (count, invocation) = match contract {
                Contract::Mobject if has_dt => (2, "callback(mobject, dt)"),
                Contract::Mobject => (1, "callback(mobject)"),
                Contract::Scene => (1, "callback(dt)"),
            };
            if binds_positionally(signature, count) {
                continue;
            }
            let file = context.sources().file(call.file);
            diagnostics.push(cannot_bind_diagnostic(
                context, file, call, callback, signature, contract, has_dt, invocation, &canonical,
            ));
        }
        diagnostics
    }
}

/// Builds the `MLC105` diagnostic for one confirmed non-binding callback.
#[allow(clippy::too_many_arguments, reason = "plain diagnostic assembly")]
fn cannot_bind_diagnostic(
    context: &RuleContext<'_>,
    file: &crate::source::SourceFile,
    call: &QualifiedCall,
    callback: &CallArgument,
    signature: &CallableSignature,
    contract: Contract,
    has_dt: bool,
    invocation: &str,
    canonical: &str,
) -> Diagnostic {
    let parameters = signature
        .params
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let (message, explanation) = match contract {
        Contract::Mobject => (
            format!(
                "Adjust this updater's parameters (`{parameters}`): because \
                         {reason}, `Mobject.add_updater` will call it as \
                         `{invocation}` and that call cannot bind.",
                reason = if has_dt {
                    "a parameter is named `dt`"
                } else {
                    "no parameter is named `dt`"
                },
            ),
            "Manim inspects the callback signature for a parameter *named* \
                     `dt`: if present the callback is invoked positionally as \
                     `callback(mobject, dt)`, otherwise as `callback(mobject)`. A \
                     signature that cannot bind that exact positional call raises \
                     `TypeError` on the first rendered frame (DESIGN §3.3)."
                .to_owned(),
        ),
        Contract::Scene => (
            format!(
                "Adjust this Scene updater's parameters (`{parameters}`): \
                         `Scene.add_updater` always calls it as `{invocation}` with \
                         one positional argument, and that call cannot bind."
            ),
            "Scene-level updaters have a fixed contract: every frame the \
                     callback receives exactly one positional argument, the frame \
                     time delta `dt`. A signature that cannot bind one positional \
                     argument raises `TypeError` on the first rendered frame \
                     (DESIGN §3.3)."
                .to_owned(),
        ),
    };
    let mut evidence = BTreeMap::new();
    evidence.insert("resolved".to_owned(), Value::String(canonical.to_owned()));
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence.insert(
        "simulated_call".to_owned(),
        Value::String(invocation.to_owned()),
    );
    evidence.insert(
        "callback_parameters".to_owned(),
        Value::Array(
            signature
                .params
                .iter()
                .map(|parameter| Value::String(parameter.name.clone()))
                .collect(),
        ),
    );
    build_diagnostic(
        &MLC105,
        context,
        file,
        callback.range,
        message,
        explanation,
        evidence,
    )
}
