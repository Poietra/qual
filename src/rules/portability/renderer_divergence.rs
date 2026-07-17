//! `MLD304`: renderer-divergent semantics reached without a guard under a
//! multi-renderer run (DESIGN §7.4, §3.5).
//!
//! Only active when the run actually targets more than one renderer
//! (`--profile all` with both a Cairo and an OpenGL profile): a project
//! that explicitly targets a single renderer must not be asked for
//! renderer guards (DESIGN §7.4 prose).
//!
//! The implemented case is the fixed-object unfix divergence the
//! lifecycle interpreter proves: `ThreeDScene.remove_fixed_in_frame_mobjects`
//! / `remove_fixed_orientation_mobjects` only unregister the camera fix
//! under Cairo but also `Scene.remove` the object under OpenGL
//! (DESIGN §3.5), traced as a `RendererRequirement` /
//! `DivergesBetweenRenderers` event. The rule fires only on events with
//! all-paths certainty: any branch around the call (a renderer guard
//! included) joins to `Maybe` and stays silent — guarded code is never
//! flagged.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::{Event, RendererRequirementKind};
use crate::semantic::values::Presence;
use crate::source::FileId;

use super::build_diagnostic;

pub(super) const MLD304: RuleMetadata = RuleMetadata {
    id: "MLD304",
    summary: "Renderer-divergent membership effect reached without a guard in a multi-renderer run",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct UnguardedRendererDivergence;

impl Rule for UnguardedRendererDivergence {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD304
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let renderers: BTreeSet<String> = context
            .active_profiles()
            .iter()
            .map(|profile| profile.renderer.to_string())
            .collect();
        // Single-renderer runs never require renderer guards.
        if renderers.len() < 2 {
            return Vec::new();
        }
        let profiles = context.config().active_profile_names();
        let renderer_names: Vec<String> = renderers.into_iter().collect();
        let mut reported: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for traced in &scene.events {
                let Event::RendererRequirement(requirement) = &traced.event else {
                    continue;
                };
                if requirement.kind != RendererRequirementKind::DivergesBetweenRenderers {
                    continue;
                }
                // A branch around the call (renderer guard or otherwise)
                // joins the certainty to Maybe: stay silent.
                if traced.certainty != Presence::Present {
                    continue;
                }
                if !reported.insert((traced.site.file, traced.site.start, traced.site.end)) {
                    continue;
                }
                let file = context.sources().file(traced.site.file);
                let range = TextRange::new(traced.site.start.into(), traced.site.end.into());
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name.clone()));
                evidence.insert("note".to_owned(), json!(requirement.note.clone()));
                evidence.insert("renderers".to_owned(), json!(renderer_names.clone()));
                diagnostics.push(build_diagnostic(
                    &MLD304,
                    file,
                    range,
                    Confidence::Medium,
                    format!(
                        "This call's scene-membership effect diverges between the \
                         renderers this run targets ({renderers}) and is reached without \
                         a renderer guard",
                        renderers = renderer_names.join(", "),
                    ),
                    "Unfixing a fixed object only unregisters the camera fix under \
                     Cairo, but the OpenGL branch additionally removes the object from \
                     the Scene (DESIGN 3.5). Rendering the same scene with both \
                     renderers therefore produces different membership after this call. \
                     Follow it with an explicit Scene.add / Scene.remove to pin the \
                     intended state, or guard the call by renderer.",
                    evidence,
                    profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}
