//! `MLR108`: an object is still treated as visible after a
//! renderer-divergent `remove_fixed_*` call (DESIGN §7.2, §3.5).
//!
//! `ThreeDScene.remove_fixed_in_frame_mobjects` /
//! `remove_fixed_orientation_mobjects` only unregister the camera fix
//! under Cairo — the object stays displayed — while the OpenGL branch
//! additionally calls `Scene.remove` (verified in `three_d_scene.py`).
//! Code that keeps styling / moving the object afterwards without an
//! explicit `Scene.add` / `Scene.remove` therefore assumes the Cairo
//! behavior: under an OpenGL-target profile the mutation happens on an
//! object that is no longer displayed.
//!
//! The rule anchors at the first definite mutation of the object after
//! the divergent removal. Any earlier explicit `Scene.add` /
//! `Scene.remove` (a play's auto-add included) pins the intended
//! membership and silences it; an unknown call that may touch the object
//! widens to silence (DESIGN §15 invariant 2).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::Event;
use crate::semantic::interpreter::{FixedAction, FixedKind, SceneLifecycle};
use crate::semantic::values::{AllocationSite, ObjectId, Presence, Truth};
use crate::source::FileId;

use super::{build_diagnostic, renderer_profile_names, site_range};

const MLR108: RuleMetadata = RuleMetadata {
    id: "MLR108",
    summary: "Object treated as still visible after a renderer-divergent remove_fixed_* call",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct StaleFixedVisibility;

impl Rule for StaleFixedVisibility {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR108
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        // The visible-continuation assumption only breaks under OpenGL
        // (DESIGN §15 invariant 8): a Cairo-only run renders exactly what
        // the code assumes.
        let opengl_profiles = renderer_profile_names(context, Renderer::Opengl);
        if opengl_profiles.is_empty() {
            return Vec::new();
        }
        let mut seen: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for fact in &scene.fixed_registrations {
                if fact.action != FixedAction::Remove
                    || !fact.renderer_divergent
                    || fact.certainty != Presence::Present
                {
                    continue;
                }
                let Some(use_site) = first_stale_use(scene, fact.site, &fact.object) else {
                    continue;
                };
                if !seen.insert((use_site.site.file, use_site.site.start, use_site.site.end)) {
                    continue;
                }
                let file = context.sources().file(use_site.site.file);
                let removal = context.sources().file(fact.site.file);
                let removal_span = removal.span_of_range(site_range(fact.site));
                let registry = match fact.kind {
                    FixedKind::InFrame => "remove_fixed_in_frame_mobjects",
                    FixedKind::Orientation => "remove_fixed_orientation_mobjects",
                };
                let mut evidence = BTreeMap::new();
                evidence.insert("removal".to_owned(), json!(registry));
                evidence.insert("removal_line".to_owned(), json!(removal_span.start.line));
                evidence.insert("scene".to_owned(), json!(scene.qualified_name.clone()));
                diagnostics.push(build_diagnostic(
                    &MLR108,
                    file,
                    site_range(use_site.site),
                    Confidence::High,
                    format!(
                        "This mutation assumes the object is still displayed after \
                         `{registry}` (line {line}), but under the OpenGL renderer this \
                         run targets ({profiles}) that call also removed it from the \
                         scene — follow the removal with an explicit `self.add(...)` or \
                         `self.remove(...)`",
                        line = removal_span.start.line,
                        profiles = opengl_profiles.join(", "),
                    ),
                    "Unfixing a fixed object diverges between the renderers \
                     (DESIGN 3.5): Cairo only unregisters the camera fixation and \
                     keeps the object displayed, while the OpenGL branch also calls \
                     Scene.remove. Mutating the object afterwards without pinning its \
                     membership renders a visible change under Cairo and nothing under \
                     OpenGL. An explicit Scene.add / Scene.remove right after the \
                     unfix makes the intended state identical everywhere.",
                    evidence,
                    opengl_profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// The first traced event after `removal_site` that settles the stale-use
/// question for `object`.
struct StaleUse {
    site: AllocationSite,
}

/// Scans the scene's event trace after the divergent removal: the first
/// event referencing the object decides. A definite mutation is the
/// finding; an explicit membership event or any uncertainty is silence.
fn first_stale_use(
    scene: &SceneLifecycle,
    removal_site: AllocationSite,
    object: &ObjectId,
) -> Option<StaleUse> {
    // Locate the removal in the event stream: the interpreter records the
    // divergence marker at the removal call's own site.
    let start = scene.events.iter().position(|traced| {
        traced.site == removal_site && matches!(traced.event, Event::RendererRequirement(_))
    })?;
    for traced in &scene.events[start + 1..] {
        match &traced.event {
            Event::SceneAdd(add) => {
                if add
                    .objects
                    .iter()
                    .any(|other| other.may_be_same(object) != Truth::No)
                {
                    // Explicit add (or a play's auto-add) pins membership.
                    return None;
                }
            }
            Event::SceneRemove(remove) => {
                if remove
                    .objects
                    .iter()
                    .any(|other| other.may_be_same(object) != Truth::No)
                {
                    // Explicit removal pins membership.
                    return None;
                }
            }
            Event::UnknownMutation(unknown) => {
                if unknown
                    .values
                    .iter()
                    .any(|other| other.may_be_same(object) != Truth::No)
                {
                    // An unresolved call may have re-added / removed it.
                    return None;
                }
            }
            Event::Mutate(mutate) => {
                if mutate.target.definitely_same(object) {
                    if traced.certainty == Presence::Present {
                        return Some(StaleUse { site: traced.site });
                    }
                    // A branch-dependent mutation is at most Maybe.
                    return None;
                }
                if mutate.target.may_be_same(object) != Truth::No {
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}
