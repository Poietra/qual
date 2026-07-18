//! `MLR123`: an OpenGL-only mesh added to the scene under a Cairo-target
//! profile (DESIGN §7.2, §15.8).
//!
//! The knowledge profile curates the mesh contract as a positive
//! renderer-requirement fact ([`RendererCompat::opengl_only_mesh`]):
//! `Object3D` / `Mesh` (`renderer/shader.py`) and the `OpenGLSurface`
//! family (`mobject/opengl/`) are OpenGL-only scene objects. Verified
//! against the sibling Manim checkout (clean base `4d25c031`):
//! `Scene.add` diverts `Object3D` instances into `Scene.meshes` only
//! under `RendererType.OPENGL` (`scene.py Scene.add`); under Cairo the
//! object lands in `Scene.mobjects`, and because none of these classes
//! is a Cairo `Mobject`, the first captured frame raises `TypeError` in
//! `Camera.type_or_raise` (`camera/camera.py` — the `display_funcs`
//! table matches `VMobject` / `PMobject` / `AbstractImageMobject` /
//! `Mobject`, none of which an `OpenGLMobject`- or `Object3D`-rooted
//! class is). Cairo-capable 3D mobjects (`ThreeDVMobject`, `Surface`)
//! are ordinary `VMobject`s and never carry the fact.
//!
//! The rule fires on every definite `SceneAdd` event — a direct
//! `self.add(...)`, a play auto-add, or an introducer setup-add — whose
//! object kinds all resolve to curated meshes, while at least one active
//! profile targets Cairo. A profile that does not set a renderer targets
//! Manim's default (Cairo), so "renderer unknown" collapses into the
//! same gate. Branch-dependent (`Maybe`) adds, Unknown kinds, mixed or
//! partially-resolved base chains (a class inheriting both a mesh and a
//! Cairo `Mobject` is dispatched by `isinstance` to the `Mobject` arm
//! and does not crash), and OpenGL-only profile sets stay silent
//! (DESIGN §15 invariants 2 and 8).
//!
//! [`RendererCompat::opengl_only_mesh`]:
//! crate::knowledge::RendererCompat::opengl_only_mesh

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ProjectIndex;
use crate::knowledge::KnowledgeProfile;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::Event;
use crate::semantic::values::{AllocationSite, KindSet, Presence};

use super::{build_diagnostic, renderer_profile_names, short_name, site_range};

const MLR123: RuleMetadata = RuleMetadata {
    id: "MLR123",
    summary: "OpenGL-only mesh mobject is added to a scene under a Cairo-target profile",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// Mesh classification of one class id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeshClass {
    /// A curated OpenGL-only mesh (or a class provably rooted at one).
    Mesh,
    /// A curated non-mesh class (or provably rooted only at those).
    NotMesh,
    /// Not enough facts to classify.
    Unknown,
}

/// Classifies a class id via the curated mesh fact, falling back to the
/// project class hierarchy (whose `reached_bases` are canonical ids).
///
/// A `Mesh` verdict must be definite: every reached base resolves and
/// agrees. A chain mixing a mesh base with a Cairo `Mobject` base stays
/// `Unknown` — the Cairo camera's `isinstance` dispatch finds the
/// `Mobject` arm for such a class, so it does not crash (silence over
/// false positives, DESIGN §15 invariant 2).
fn classify_mesh(profile: &KnowledgeProfile, index: &ProjectIndex, id: &str) -> MeshClass {
    if let Some(entry) = profile.symbol(id) {
        let curated_mesh = entry
            .renderer
            .as_ref()
            .and_then(|compat| compat.opengl_only_mesh)
            == Some(true);
        return if curated_mesh {
            MeshClass::Mesh
        } else {
            MeshClass::NotMesh
        };
    }
    let Some(class) = index.classes.get(id) else {
        return MeshClass::Unknown;
    };
    let mut mesh = false;
    let mut not_mesh = false;
    let mut unknown = false;
    for base in &class.reached_bases {
        match classify_mesh(profile, index, base) {
            MeshClass::Mesh => mesh = true,
            MeshClass::NotMesh => not_mesh = true,
            MeshClass::Unknown => unknown = true,
        }
    }
    if unknown || !class.bases_fully_resolved || mesh == not_mesh {
        // Unresolved chains, no facts at all (`mesh == not_mesh == false`),
        // and mixed mesh/Mobject inheritance all stay Unknown.
        MeshClass::Unknown
    } else if mesh {
        MeshClass::Mesh
    } else {
        MeshClass::NotMesh
    }
}

pub(super) struct MeshUnderCairoTarget;

impl Rule for MeshUnderCairoTarget {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR123
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // No active profile targets Cairo: silence (DESIGN §15
        // invariant 8). Profiles without an explicit renderer resolve to
        // Manim's default Cairo renderer, so they are part of this set.
        let cairo_profiles = renderer_profile_names(context, Renderer::Cairo);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let index = context.project_index();
        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for event in &scene.events {
                let Event::SceneAdd(add) = &event.event else {
                    continue;
                };
                // A branch-dependent add is a Maybe fact: below this
                // rule's minimum confidence, so stay silent.
                if event.certainty != Presence::Present {
                    continue;
                }
                for object in &add.objects {
                    let resolved = scene.final_heap.resolve(object);
                    let Some(state) = scene.final_heap.object(object) else {
                        continue;
                    };
                    let KindSet::Known(kinds) = &state.kind else {
                        continue;
                    };
                    if kinds.is_empty()
                        || !kinds
                            .iter()
                            .all(|kind| classify_mesh(profile, index, kind) == MeshClass::Mesh)
                    {
                        continue;
                    }
                    if !seen.insert((event.site, resolved.site)) {
                        continue;
                    }
                    let file = context.sources().file(event.site.file);
                    let kind_names: Vec<&str> = kinds.iter().map(|kind| short_name(kind)).collect();
                    let name = kind_names.join("` / `");
                    let mut evidence = BTreeMap::new();
                    evidence.insert("kinds".to_owned(), json!(kinds));
                    evidence.insert("unsupported_renderer".to_owned(), json!("cairo"));
                    if kinds.len() == 1 {
                        let note = kinds
                            .iter()
                            .next()
                            .and_then(|kind| profile.symbol(kind))
                            .and_then(|entry| entry.renderer.as_ref())
                            .and_then(|compat| compat.note.clone());
                        if let Some(note) = note {
                            evidence.insert("note".to_owned(), json!(note));
                        }
                    }
                    diagnostics.push(build_diagnostic(
                        &MLR123,
                        file,
                        site_range(event.site),
                        Confidence::High,
                        format!(
                            "`{name}` is an OpenGL-only mesh, but this run targets the \
                             Cairo renderer ({profiles}): the Cairo camera cannot \
                             display it and raises TypeError at the first rendered \
                             frame — use an OpenGL profile (renderer = \"opengl\") \
                             for this scene",
                            profiles = cairo_profiles.join(", "),
                        ),
                        "Scene.add only diverts Object3D meshes into the meshes list \
                         under the OpenGL renderer, and OpenGLMobject-rooted classes \
                         are not Cairo Mobjects: the Cairo camera's display table \
                         (VMobject / PMobject / AbstractImageMobject / Mobject) \
                         matches none of them, so Camera.type_or_raise raises \
                         TypeError when the frame is captured. Render profiles that \
                         do not set a renderer target Manim's default Cairo \
                         renderer; declare an OpenGL profile (renderer = \"opengl\") \
                         for mesh scenes, or use the Cairo-capable Surface / \
                         ThreeDVMobject instead.",
                        evidence,
                        cairo_profiles.clone(),
                        None,
                    ));
                }
            }
        }
        diagnostics
    }
}
