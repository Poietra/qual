//! `MLR111`: a `Scene.add_updater` callback mutates a Mobject
//! (DESIGN §7.2, §3.3).
//!
//! Verified against the sibling Manim checkout: `Scene.add_updater`
//! (`scene.py`) carries an explicit Cairo warning — mobjects modified only
//! by a *scene* updater are not detected by `get_moving_mobjects`, so the
//! Cairo static/moving partition may bake them into the cached static
//! background and the mutation might not be redrawn each frame. Mobject
//! updaters register on the object itself and mark it moving.
//!
//! The rule fires on an all-paths scene-updater registration whose
//! resolved callback body contains a call resolving to a curated visual
//! Mobject/VMobject mutator on a known mobject instance. Unresolvable
//! callbacks or receivers stay silent (DESIGN §15 invariant 2).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ReceiverKind;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{DefMap, UpdaterHost};
use crate::semantic::state::CallbackRef;
use crate::semantic::values::Presence;
use crate::source::{FileId, SourceFile};

use super::{build_diagnostic, renderer_profile_names, resolved_method_for_call, site_range};

/// Curated methods whose effect is a visible mutation of the receiver.
const VISUAL_MUTATORS: &[&str] = &[
    "become",
    "move_to",
    "next_to",
    "restore",
    "rotate",
    "scale",
    "set_color",
    "set_fill",
    "set_opacity",
    "set_stroke",
    "set_z_index",
    "shift",
    "to_edge",
];

const MLR111: RuleMetadata = RuleMetadata {
    id: "MLR111",
    summary: "Scene updater mutates a mobject; the change may escape Cairo's moving scope",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct SceneUpdaterMutatesMobject;

impl Rule for SceneUpdaterMutatesMobject {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR111
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // The moving-scope partition is a Cairo mechanism (DESIGN §15
        // invariant 8): silence unless a Cairo profile is active.
        let cairo_profiles = renderer_profile_names(context, Renderer::Cairo);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let defs = DefMap::build(context.sources(), context.project_index());
        let mut seen: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for registration in &scene.updaters {
                if registration.host != UpdaterHost::Scene
                    || registration.certainty != Presence::Present
                {
                    continue;
                }
                let Some((body_file, body_range)) =
                    callback_body_range(&defs, &registration.fact.callback)
                else {
                    continue;
                };
                let Some(mutation) =
                    first_mobject_mutation(context, profile, body_file, body_range)
                else {
                    continue;
                };
                if !seen.insert((
                    registration.site.file,
                    registration.site.start,
                    registration.site.end,
                )) {
                    continue;
                }
                let file = context.sources().file(registration.site.file);
                let body_source: &SourceFile = context.sources().file(body_file);
                let mutation_text = body_source.slice(mutation.range);
                let mut evidence = BTreeMap::new();
                evidence.insert("mutation".to_owned(), json!(mutation_text));
                evidence.insert("method".to_owned(), json!(mutation.canonical));
                evidence.insert("scene".to_owned(), json!(scene.qualified_name.clone()));
                diagnostics.push(build_diagnostic(
                    &MLR111,
                    file,
                    site_range(registration.site),
                    Confidence::High,
                    format!(
                        "This scene updater mutates a mobject (`{mutation_text}`), which \
                         Cairo's moving-object detection does not see — the change may \
                         not be redrawn; register the callback as a Mobject updater \
                         (`mob.add_updater(...)`) instead"
                    ),
                    "Scene updaters run last every frame but Scene.get_moving_mobjects \
                     only inspects animation targets, foreground mobjects, and \
                     mobject-level updaters. A mobject modified only by a scene \
                     updater can be classified static, so the Cairo renderer keeps \
                     its cached rasterization instead of redrawing the mutation \
                     (scene.py Scene.add_updater warns about exactly this). Mobject \
                     updaters mark their host as moving and are redrawn reliably.",
                    evidence,
                    cairo_profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// One resolved mobject mutation inside a callback body.
struct MobjectMutation {
    range: TextRange,
    canonical: String,
}

/// The source extent of a registered callback body: the lambda expression
/// itself, or the whole top-level `def` for a named callback. Nested defs
/// are not in the [`DefMap`]; unresolvable callbacks yield `None`.
fn callback_body_range(defs: &DefMap<'_>, callback: &CallbackRef) -> Option<(FileId, TextRange)> {
    match callback {
        CallbackRef::Lambda(site) => Some((site.file, site_range(*site))),
        CallbackRef::Named(qualified) => defs.defs.get(qualified).map(|def| (def.file, def.range)),
        CallbackRef::Unknown => None,
    }
}

/// The first call inside `range` that resolves to a curated visual
/// mutator on a known mobject instance, in source order.
fn first_mobject_mutation(
    context: &RuleContext<'_>,
    profile: &crate::knowledge::KnowledgeProfile,
    file: FileId,
    range: TextRange,
) -> Option<MobjectMutation> {
    for call in context.qualified_calls().calls_in_file(file) {
        if call.call_range.start() < range.start() || call.call_range.end() > range.end() {
            continue;
        }
        if !matches!(call.receiver, ReceiverKind::KnownInstance(_)) {
            continue;
        }
        let Some((canonical, _)) = resolved_method_for_call(profile, call) else {
            continue;
        };
        // Only mobject-owned methods count; Scene methods resolve into
        // `manim.scene.*` and are not receiver mutations.
        if !canonical.starts_with("manim.mobject.") {
            continue;
        }
        let method = super::short_name(&canonical);
        if VISUAL_MUTATORS.contains(&method) {
            return Some(MobjectMutation {
                range: call.call_range,
                canonical,
            });
        }
    }
    None
}
