//! Renderer-profile-gated compatibility rules (DESIGN §7.2, §15.8):
//!
//! - `MLR107`: an API / mobject the knowledge profile marks unsupported
//!   under a renderer an active profile targets. Implemented generically
//!   over the curated [`RendererCompat`] entries — anything with
//!   `cairo: false` / `opengl: false` — at both call sites and direct
//!   base-class positions. Entries without an explicit `false` flag (a
//!   `note` alone) never fire.
//! - `MLR119`: a `MovingCameraScene` subclass while an active profile
//!   targets OpenGL. Verified against the sibling Manim checkout: the
//!   OpenGL renderer ignores `camera_class` (`scene.py Scene.__init__`
//!   builds `OpenGLRenderer()` without it) and `OpenGLCamera` has no
//!   `.frame` mobject, so every `self.camera.frame` path (including
//!   `MovingCameraScene.get_moving_mobjects` itself) raises
//!   `AttributeError`. Supersedes the generic `MLR107` on the shared
//!   base-class span.
//! - `MLR120`: `focal_distance` passed to `ThreeDScene`'s camera setters
//!   while an active profile targets OpenGL. Verified: `OpenGLCamera`
//!   defines no `set_focal_distance`, so
//!   `set_camera_orientation(focal_distance=...)` raises `AttributeError`
//!   under OpenGL, and `move_camera(focal_distance=...)` explicitly warns
//!   "focal distance of `OpenGLCamera` can not be adjusted" and ignores the
//!   value (`three_d_scene.py`, `opengl_renderer.py`).

use std::collections::BTreeMap;

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{BaseRef, ClassRecord};
use crate::knowledge::{KnowledgeProfile, SymbolEntry};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::CameraKind;
use crate::source::FileId;

use super::{
    build_diagnostic, each_statement, renderer_profile_names, resolved_method_for_call, short_name,
    single_knowledge_symbol,
};

/// The canonical id whose renderer contract `MLR119` reports.
const MOVING_CAMERA_SCENE: &str = "manim.scene.moving_camera_scene.MovingCameraScene";
/// The curated `ThreeDScene` camera setters `MLR120` inspects, with the
/// zero-based positional slot of `focal_distance` (after `self`).
const FOCAL_DISTANCE_METHODS: &[(&str, usize)] = &[
    ("manim.scene.three_d_scene.ThreeDScene.move_camera", 4),
    (
        "manim.scene.three_d_scene.ThreeDScene.set_camera_orientation",
        4,
    ),
];

/// The renderers a curated entry marks explicitly unsupported.
fn unsupported_renderers(entry: &SymbolEntry) -> Vec<Renderer> {
    let Some(compat) = &entry.renderer else {
        return Vec::new();
    };
    let mut unsupported = Vec::new();
    if compat.cairo == Some(false) {
        unsupported.push(Renderer::Cairo);
    }
    if compat.opengl == Some(false) {
        unsupported.push(Renderer::Opengl);
    }
    unsupported
}

/// The source range of the `index`-th base expression of the class
/// statement recorded at `record.range`, from the file's AST.
fn base_expr_range(
    context: &RuleContext<'_>,
    record: &ClassRecord,
    index: usize,
) -> Option<TextRange> {
    let file = context.sources().file(record.file);
    let module = file.ast()?;
    let mut found = None;
    each_statement(&module.body, &mut |stmt| {
        if let ast::Stmt::ClassDef(def) = stmt {
            if def.range() == record.range {
                found = def.bases.get(index).map(Ranged::range);
            }
        }
    });
    found
}

/// Direct-base positions of `record` that resolve to the canonical
/// `symbol` id (project-expanded ancestors are the root class's finding,
/// not this one's).
fn direct_base_positions(record: &ClassRecord, symbol: &str) -> Vec<usize> {
    record
        .bases
        .iter()
        .enumerate()
        .filter_map(|(position, base)| match base {
            BaseRef::Resolved(id) if id == symbol => Some(position),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// MLR107: curated renderer-compat entry used under the unsupported
// renderer.
// ---------------------------------------------------------------------------

const MLR107: RuleMetadata = RuleMetadata {
    id: "MLR107",
    summary: "API/mobject is unsupported under a renderer this run targets",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct UnsupportedRendererApi;

impl UnsupportedRendererApi {
    /// Builds the shared diagnostic for one flagged use of `symbol`.
    fn diagnostic(
        context: &RuleContext<'_>,
        file: FileId,
        range: TextRange,
        symbol: &str,
        entry: &SymbolEntry,
        use_kind: &str,
    ) -> Option<Diagnostic> {
        let mut renderers: Vec<String> = Vec::new();
        let mut applicable: Vec<String> = Vec::new();
        for renderer in unsupported_renderers(entry) {
            let profiles = renderer_profile_names(context, renderer);
            if profiles.is_empty() {
                continue;
            }
            renderers.push(renderer.to_string());
            applicable.extend(profiles);
        }
        // No active profile targets an unsupported renderer: silence
        // (DESIGN §15 invariant 8).
        if applicable.is_empty() {
            return None;
        }
        let source = context.sources().file(file);
        let name = short_name(symbol);
        let mut evidence = BTreeMap::new();
        evidence.insert("symbol".to_owned(), json!(symbol));
        evidence.insert("unsupported_renderers".to_owned(), json!(renderers));
        evidence.insert("use".to_owned(), json!(use_kind));
        if let Some(note) = entry
            .renderer
            .as_ref()
            .and_then(|compat| compat.note.clone())
        {
            evidence.insert("note".to_owned(), json!(note));
        }
        Some(build_diagnostic(
            &MLR107,
            source,
            range,
            Confidence::High,
            format!(
                "`{name}` is not supported under the {renderers} renderer this run \
                 targets (profiles: {profiles})",
                renderers = renderers.join("/"),
                profiles = applicable.join(", "),
            ),
            "The versioned Manim knowledge profile marks this symbol's contract as \
             renderer-specific: under the flagged renderer the API either raises or \
             renders differently. Restrict the render profiles or guard the usage \
             by renderer.",
            evidence,
            applicable,
            None,
        ))
    }
}

impl Rule for UnsupportedRendererApi {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR107
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        // Call sites resolving to a flagged curated symbol.
        for call in &context.qualified_calls().calls {
            let Some((symbol, entry)) = single_knowledge_symbol(profile, &call.candidates) else {
                continue;
            };
            if unsupported_renderers(entry).is_empty() {
                continue;
            }
            diagnostics.extend(Self::diagnostic(
                context,
                call.file,
                call.call_range,
                symbol,
                entry,
                "call",
            ));
        }
        // Direct base-class positions naming a flagged curated symbol.
        for record in index.classes.values() {
            for (symbol, entry) in flagged_symbols(profile) {
                for position in direct_base_positions(record, symbol) {
                    let Some(range) = base_expr_range(context, record, position) else {
                        continue;
                    };
                    diagnostics.extend(Self::diagnostic(
                        context,
                        record.file,
                        range,
                        symbol,
                        entry,
                        "base-class",
                    ));
                }
            }
        }
        diagnostics
    }
}

/// Every curated symbol carrying an explicit unsupported-renderer flag,
/// in id order.
fn flagged_symbols(profile: &KnowledgeProfile) -> Vec<(&str, &SymbolEntry)> {
    profile
        .symbols
        .iter()
        .filter(|(_, entry)| !unsupported_renderers(entry).is_empty())
        .map(|(id, entry)| (id.as_str(), entry))
        .collect()
}

// ---------------------------------------------------------------------------
// MLR119: MovingCameraScene subclass under an OpenGL-target profile.
// ---------------------------------------------------------------------------

const MLR119: RuleMetadata = RuleMetadata {
    id: "MLR119",
    summary: "MovingCameraScene is incompatible with the OpenGL renderer this run targets",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &["MLR107"],
};

pub(super) struct MovingCameraUnderOpengl;

impl Rule for MovingCameraUnderOpengl {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR119
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // Fire only while the loaded profile still curates the OpenGL
        // incompatibility (stay conservative under profile evolution).
        let flagged = profile
            .symbol(MOVING_CAMERA_SCENE)
            .is_some_and(|entry| unsupported_renderers(entry).contains(&Renderer::Opengl));
        if !flagged {
            return Vec::new();
        }
        let opengl_profiles = renderer_profile_names(context, Renderer::Opengl);
        if opengl_profiles.is_empty() {
            return Vec::new();
        }
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            // The camera contract must be definite; Unknown never fires.
            if scene.camera_kind != CameraKind::MovingCamera {
                continue;
            }
            let Some(record) = index.classes.get(&scene.qualified_name) else {
                continue;
            };
            // Only the class naming MovingCameraScene directly carries the
            // finding; deeper subclasses inherit it from their root.
            for position in direct_base_positions(record, MOVING_CAMERA_SCENE) {
                let Some(range) = base_expr_range(context, record, position) else {
                    continue;
                };
                let file = context.sources().file(record.file);
                let mut evidence = BTreeMap::new();
                evidence.insert("scene".to_owned(), json!(scene.qualified_name.clone()));
                evidence.insert("base".to_owned(), json!(MOVING_CAMERA_SCENE));
                evidence.insert("camera_kind".to_owned(), json!("moving-camera"));
                diagnostics.push(build_diagnostic(
                    &MLR119,
                    file,
                    range,
                    Confidence::High,
                    format!(
                        "`{name}` subclasses MovingCameraScene, but this run targets the \
                         OpenGL renderer ({profiles}): the animatable `self.camera.frame` \
                         contract only exists under Cairo — use a Cairo-only profile for \
                         this scene",
                        name = record.name,
                        profiles = opengl_profiles.join(", "),
                    ),
                    "Under OpenGL, Scene builds OpenGLRenderer() and ignores the \
                     camera_class MovingCameraScene configures, so self.camera is an \
                     OpenGLCamera without a `.frame` mobject. Every frame access — \
                     including MovingCameraScene.get_moving_mobjects itself — raises \
                     AttributeError. Render this scene with a Cairo profile, or drop \
                     the MovingCameraScene base for OpenGL runs.",
                    evidence,
                    opengl_profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLR120: focal_distance camera setter under an OpenGL-target profile.
// ---------------------------------------------------------------------------

const MLR120: RuleMetadata = RuleMetadata {
    id: "MLR120",
    summary: "focal_distance camera setter has no effect under the OpenGL renderer",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct FocalDistanceUnderOpengl;

impl Rule for FocalDistanceUnderOpengl {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR120
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let opengl_profiles = renderer_profile_names(context, Renderer::Opengl);
        if opengl_profiles.is_empty() {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((canonical, _)) = resolved_method_for_call(profile, call) else {
                continue;
            };
            let Some((_, slot)) = FOCAL_DISTANCE_METHODS
                .iter()
                .find(|(id, _)| *id == canonical)
            else {
                continue;
            };
            let argument = call.keyword("focal_distance").or_else(|| {
                if call.has_star_args {
                    None
                } else {
                    call.positional(*slot)
                        .filter(|argument| argument.keyword.is_none())
                }
            });
            let Some(argument) = argument else {
                continue;
            };
            // An explicit None is the documented "leave unchanged" value.
            if matches!(
                argument.literal,
                Some(crate::frontend::index::LiteralFact::NoneLit)
            ) {
                continue;
            }
            let file = context.sources().file(call.file);
            let method = short_name(&canonical);
            let consequence = if method == "set_camera_orientation" {
                "raises AttributeError (OpenGLCamera has no set_focal_distance)"
            } else {
                "is ignored with a runtime warning (\"focal distance of OpenGLCamera \
                 can not be adjusted\")"
            };
            let mut evidence = BTreeMap::new();
            evidence.insert("method".to_owned(), json!(canonical));
            evidence.insert("parameter".to_owned(), json!("focal_distance"));
            diagnostics.push(build_diagnostic(
                &MLR120,
                file,
                argument.range,
                Confidence::High,
                format!(
                    "`focal_distance` passed to `{method}()` {consequence} under the \
                     OpenGL renderer this run targets ({profiles})",
                    profiles = opengl_profiles.join(", "),
                ),
                "Only the Cairo ThreeDCamera exposes focal-distance control \
                 (ThreeDCamera.set_focal_distance and its value tracker). The \
                 OpenGLCamera has no focal-distance setter: set_camera_orientation \
                 crashes and move_camera warns and drops the value. Guard the call \
                 by renderer or restrict this scene to Cairo profiles.",
                evidence,
                opengl_profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}
