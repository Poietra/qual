//! `MLD304`: renderer-divergent semantics reached without a guard under a
//! multi-renderer run (DESIGN §7.4, §3.5).
//!
//! Only active when the run actually targets more than one renderer
//! (`--profile all` with both a Cairo and an OpenGL profile): a project
//! that explicitly targets a single renderer must not be asked for
//! renderer guards (DESIGN §7.4 prose).
//!
//! Two divergence families are implemented, both on certainty facts only:
//!
//! - **Fixed-object removal** (`ThreeDScene.remove_fixed_in_frame_mobjects`
//!   / `remove_fixed_orientation_mobjects`): Cairo only unregisters the
//!   camera fix, the OpenGL branch also `Scene.remove`s the object
//!   (DESIGN §3.5), traced by the lifecycle interpreter as a
//!   `RendererRequirement` / `DivergesBetweenRenderers` event. The rule
//!   fires only on events with all-paths certainty: any branch around the
//!   call (a renderer guard included) joins to `Maybe` and stays silent.
//!   `SceneLifecycle::fixed_registrations` enriches the diagnostic with
//!   the affected registry and the certain registration site.
//! - **Camera-frame semantics** (`MovingCameraScene`): under Cairo
//!   `self.camera` is a `MovingCamera` with an animatable `frame`
//!   mobject and `auto_zoom`; the OpenGL renderer ignores `camera_class`
//!   and installs an `OpenGLCamera` without that contract
//!   (`scene.py Scene.__init__`, `moving_camera_scene.py`; the curated
//!   `MovingCameraScene` renderer facts carry the incompatibility). The
//!   rule fires only when the scene's camera kind is *proven*
//!   `MovingCamera` (fully resolved bases) **and** `self.camera.frame` /
//!   `self.camera.auto_zoom` is reached on a straight-line statement of
//!   `construct` — any enclosing branch, loop, early return, or helper
//!   indirection means the use is not all-paths and stays silent.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::names::{collect_target_names, flatten_dotted};
use crate::frontend::statements::{each_statement, function_def_at, walk_certain_exprs};
use crate::rules::base::{CameraKind, FixedAction, FixedKind, Rule, RuleContext};
use crate::semantic::events::{Event, RendererRequirementKind};
use crate::semantic::interpreter::SceneLifecycle;
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

/// Canonical id of the curated `MovingCameraScene` entry carrying the
/// renderer-compat divergence fact.
const MOVING_CAMERA_SCENE_ID: &str = "manim.scene.moving_camera_scene.MovingCameraScene";

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
        let renderer_names: Vec<String> = renderers.into_iter().collect();
        let mut reported: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            fixed_removal_diagnostics(
                context,
                scene,
                &renderer_names,
                &mut reported,
                &mut diagnostics,
            );
            camera_frame_diagnostics(
                context,
                scene,
                &renderer_names,
                &mut reported,
                &mut diagnostics,
            );
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// Leg 1: fixed-object removal divergence (DESIGN §3.5).
// ---------------------------------------------------------------------------

fn fixed_removal_diagnostics(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    renderer_names: &[String],
    reported: &mut BTreeSet<(FileId, u32, u32)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let profiles = context.config().active_profile_names();
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
        evidence.insert("renderers".to_owned(), json!(renderer_names));

        // Deepening: the fixed-registration facts at the same site name
        // the affected registry and point back at the certain
        // registration site of each removed object.
        let mut registries: BTreeSet<&str> = BTreeSet::new();
        let mut registration_sites: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        for removal in &scene.fixed_registrations {
            if removal.site != traced.site
                || removal.action != FixedAction::Remove
                || !removal.renderer_divergent
            {
                continue;
            }
            registries.insert(match removal.kind {
                FixedKind::InFrame => "fixed_in_frame",
                FixedKind::Orientation => "fixed_orientation",
            });
            for registration in &scene.fixed_registrations {
                if registration.action == FixedAction::Register
                    && registration.object == removal.object
                    && registration.certainty == Presence::Present
                    && registration.site.start < removal.site.start
                {
                    registration_sites.insert((
                        registration.site.file,
                        registration.site.start,
                        registration.site.end,
                    ));
                }
            }
        }
        if !registries.is_empty() {
            evidence.insert(
                "registry".to_owned(),
                json!(registries.iter().copied().collect::<Vec<_>>()),
            );
        }
        let mut diagnostic = build_diagnostic(
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
        );
        for (site_file, start, end) in registration_sites {
            let related = context.sources().file(site_file);
            diagnostic.related_locations.push(RelatedLocation {
                path: related.relative_path().to_owned(),
                span: related.span_of_range(TextRange::new(start.into(), end.into())),
                message: "the object was fixed (and auto-added) here".to_owned(),
            });
        }
        diagnostics.push(diagnostic);
    }
}

// ---------------------------------------------------------------------------
// Leg 2: MovingCameraScene camera-frame semantics (DESIGN §3.5, §7.4).
// ---------------------------------------------------------------------------

fn camera_frame_diagnostics(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    renderer_names: &[String],
    reported: &mut BTreeSet<(FileId, u32, u32)>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    // Certainty gate 1: the camera contract is proven MovingCamera
    // (fully resolved bases; `Unknown` is never guessed).
    if scene.camera_kind != CameraKind::MovingCamera {
        return;
    }
    // Certainty gate 2: the divergence is a curated positive fact on the
    // knowledge profile, never assumed.
    let divergent = context
        .knowledge()
        .and_then(|profile| profile.symbol(MOVING_CAMERA_SCENE_ID))
        .and_then(|entry| entry.renderer.as_ref())
        .is_some_and(|compat| compat.cairo == Some(true) && compat.opengl == Some(false));
    if !divergent {
        return;
    }
    // Certainty gate 3: a MovingCamera-only attribute is reached on a
    // straight-line statement of this scene's `construct`.
    let Some((file_id, attribute, range)) = first_certain_camera_use(context, scene) else {
        return;
    };
    if !reported.insert((file_id, range.start().into(), range.end().into())) {
        return;
    }
    let file = context.sources().file(file_id);
    let mut evidence = BTreeMap::new();
    evidence.insert("scene".to_owned(), json!(scene.qualified_name.clone()));
    evidence.insert("camera".to_owned(), json!("moving_camera"));
    evidence.insert("attribute".to_owned(), json!(attribute));
    evidence.insert("renderers".to_owned(), json!(renderer_names));
    diagnostics.push(build_diagnostic(
        &MLD304,
        file,
        range,
        Confidence::Medium,
        format!(
            "This MovingCameraScene's camera-frame semantics diverge between \
             the renderers this run targets ({renderers}); `{attribute}` is \
             reached without a renderer guard",
            renderers = renderer_names.join(", "),
        ),
        "Under Cairo, `self.camera` on a MovingCameraScene is a MovingCamera \
         whose `frame` is an animatable mobject (and `auto_zoom` exists); the \
         OpenGL renderer ignores `camera_class` and installs an OpenGLCamera \
         without that contract (scene.py Scene.__init__, \
         moving_camera_scene.py). Rendering this scene with both renderers \
         therefore diverges at this access. Restrict the scene to a \
         Cairo-only profile, or guard the camera-frame path by renderer.",
        evidence,
        context.config().active_profile_names().clone(),
        None,
    ));
}

/// The first `self.camera.frame` / `self.camera.auto_zoom` reference that
/// is certainly evaluated when the scene renders: on a straight-line
/// prefix of the `construct` body (the first branch, loop, `try`, `with`,
/// nested definition, or return/raise ends the certain prefix), outside
/// lambda bodies, comprehensions, conditional expression branches, and
/// short-circuit tails. `self` must not be rebound inside `construct`.
fn first_certain_camera_use(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
) -> Option<(FileId, &'static str, TextRange)> {
    // The first MRO class defining `construct` owns the executed body.
    let (record, construct_range) = scene.mro.iter().find_map(|class| {
        let record = context.project_index().classes.get(class)?;
        let range = record.methods.get("construct")?;
        Some((record, *range))
    })?;
    let file = context.sources().file(record.file);
    let construct = function_def_at(file, construct_range)?;
    if construct.name.as_str() != "construct" || binds_self(&construct.body) {
        return None;
    }
    let mut found: Option<(&'static str, TextRange)> = None;
    'stmts: for stmt in &construct.body {
        let exprs: Vec<&ast::Expr> = match stmt {
            ast::Stmt::Assign(inner) => {
                let mut exprs = vec![inner.value.as_ref()];
                exprs.extend(inner.targets.iter());
                exprs
            }
            ast::Stmt::AnnAssign(inner) => match &inner.value {
                Some(value) => vec![value.as_ref(), inner.target.as_ref()],
                None => Vec::new(),
            },
            ast::Stmt::AugAssign(inner) => vec![inner.value.as_ref(), inner.target.as_ref()],
            ast::Stmt::Expr(inner) => vec![inner.value.as_ref()],
            ast::Stmt::Pass(_) | ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => Vec::new(),
            // Anything that can branch, loop, raise, return, or defer
            // execution ends the certain straight-line prefix.
            _ => break 'stmts,
        };
        for expr in exprs {
            walk_certain_exprs(expr, &mut |certain| {
                if found.is_some() {
                    return;
                }
                if let Some((root, segments)) = flatten_dotted(certain) {
                    let tail: Vec<&str> = segments.iter().map(String::as_str).collect();
                    if root == "self" && tail == ["camera", "frame"] {
                        found = Some(("self.camera.frame", certain.range()));
                    } else if root == "self" && tail == ["camera", "auto_zoom"] {
                        found = Some(("self.camera.auto_zoom", certain.range()));
                    }
                }
            });
            if found.is_some() {
                break 'stmts;
            }
        }
    }
    found.map(|(attribute, range)| (record.file, attribute, range))
}

/// Whether any statement in `construct` (at any nesting depth) rebinds the
/// name `self`: assignments, loop targets, or `with ... as` targets. A
/// rebound `self` means `self.camera` is no longer certainly the scene.
fn binds_self(body: &[ast::Stmt]) -> bool {
    let mut bound = false;
    each_statement(body, &mut |stmt| {
        let mut names = BTreeSet::new();
        match stmt {
            ast::Stmt::Assign(inner) => {
                for target in &inner.targets {
                    collect_target_names(target, &mut names);
                }
            }
            ast::Stmt::AnnAssign(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::AugAssign(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::For(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::AsyncFor(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, &mut names);
                    }
                }
            }
            _ => {}
        }
        if names.contains("self") {
            bound = true;
        }
    });
    bound
}
