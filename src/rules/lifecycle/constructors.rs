//! Constructor chain rule: `MLC128`.
//!
//! A project Scene subclass that defines `__init__` but never calls
//! `super().__init__()` leaves renderer / camera / file-writer state
//! unconfigured and fails at render time (DESIGN §3.1, §5.1). The abstract
//! interpreter records a per-lifecycle-method `super()` presence verdict;
//! the rule fires only on all-paths absence with a fully linearized MRO —
//! an unresolved MRO (`constructor_state_unknown`) must never fire.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ReceiverKind;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Presence;

use super::support::build_diagnostic;

/// Metadata for [`MissingSuperInit`].
pub const MLC128: RuleMetadata = RuleMetadata {
    id: "MLC128",
    summary: "Scene subclass __init__ never calls super().__init__()",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// A project Scene subclass whose own `__init__` calls `super().__init__()`
/// on no path (DESIGN §7.1 `MLC128`).
///
/// Current scope: Scene subclasses (the interpreter composes their
/// constructor chain along the MRO). Mobject subclasses have no
/// interpreter-recorded `super()` fact yet and are not checked.
pub struct MissingSuperInit;

impl Rule for MissingSuperInit {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC128
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            // Unresolved or dynamic bases: constructor state is Unknown and
            // the rule must not fire (catalog: "MRO不明時は発火しない").
            if scene.constructor_state_unknown {
                continue;
            }
            if scene.super_calls.get("__init__") != Some(&Presence::Absent) {
                continue;
            }
            // Only fire on the class that defines the __init__ itself, so a
            // subclass inheriting it does not duplicate the diagnostic.
            let Some(record) = index.classes.get(&scene.qualified_name) else {
                continue;
            };
            let Some(init_range) = record.methods.get("__init__").copied() else {
                continue;
            };
            let file = context.sources().file(record.file);
            let body = file.slice(init_range);
            // Silence guards (silence over a false positive): a legacy
            // two-argument super, a direct `Base.__init__(self, ...)` call,
            // or delegation into a project-defined helper method may
            // initialize the base outside the zero-argument `super()` form
            // the interpreter models.
            if body.contains("super") || body.contains(".__init__(") {
                continue;
            }
            if delegates_to_project_method(context, record, init_range) {
                continue;
            }
            let mut evidence = BTreeMap::new();
            evidence.insert(
                "super_init".to_owned(),
                Value::String("absent-on-all-paths".to_owned()),
            );
            evidence.insert(
                "mro".to_owned(),
                Value::Array(scene.mro.iter().cloned().map(Value::String).collect()),
            );
            diagnostics.push(build_diagnostic(
                &MLC128,
                context,
                file,
                init_range,
                format!(
                    "`{name}.__init__` never calls `super().__init__()`: the Scene \
                     base constructor (renderer, camera, file writer setup) is \
                     skipped on every path and rendering fails. Add \
                     `super().__init__(**kwargs)` first.",
                    name = record.name,
                ),
                "`Scene.render` relies on state prepared by `Scene.__init__` \
                 (renderer, camera, file writer; DESIGN §3.1). A subclass \
                 `__init__` that never reaches the base constructor leaves that \
                 state missing, and the scene fails as soon as it renders."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

/// Whether the `__init__` body calls another method on `self` that the
/// project defines — such a helper could perform the base initialization,
/// so the rule stays silent.
fn delegates_to_project_method(
    context: &RuleContext<'_>,
    record: &crate::frontend::index::ClassRecord,
    init_range: rustpython_parser::text_size::TextRange,
) -> bool {
    let index = context.project_index();
    context
        .qualified_calls()
        .calls_in_file(record.file)
        .filter(|call| {
            call.call_range.start() >= init_range.start()
                && call.call_range.end() <= init_range.end()
        })
        .any(|call| {
            matches!(call.receiver, ReceiverKind::SelfScene)
                && call
                    .candidates
                    .iter()
                    .any(|candidate| index.function_signatures.contains_key(candidate))
        })
}
