//! `MLR121`: `shift(OUT)` / `set_z(...)` for stacking order in a 2D Cairo
//! scene (DESIGN §7.2, §3.4).
//!
//! Verified against the sibling Manim checkout: the plain 2D `Camera`
//! projects points by taking only the x/y columns
//! (`camera.py points_to_subpixel_coords`), and the Cairo display order is
//! the family flatten stable-sorted by `z_index`
//! (`utils/family.py extract_mobject_family_members`). The z coordinate
//! therefore has *no* effect in a 2D Cairo scene — neither position nor
//! stacking — so a literal `shift(OUT)` or `set_z(literal)` can only be a
//! stacking-order attempt, and `set_z_index` is the working API.
//!
//! Conservative subset: the call sits inside a method of a scene whose
//! camera contract is definitely `Standard`, the receiver is a confirmed
//! mobject, and the direction / z value is the literal curated `OUT`
//! constant or a numeric literal. `ThreeD` / `MovingCamera` / unknown-camera
//! scenes never fire.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{ArgShape, LiteralFact, QualifiedCall, ReceiverKind};
use crate::frontend::statements::FileBindingFacts;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::CameraKind;
use crate::source::SourceFile;

use super::{
    VmClass, build_diagnostic, renderer_profile_names, resolved_method_for_call, short_name,
};

/// The canonical id of the curated `OUT` direction constant.
const OUT_CONSTANT: &str = "manim.constants.OUT";

const MLR121: RuleMetadata = RuleMetadata {
    id: "MLR121",
    summary: "shift(OUT)/set_z has no effect in a 2D Cairo scene; use set_z_index",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct ZShiftForStacking;

impl Rule for ZShiftForStacking {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR121
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // The claim is about the Cairo 2D pipeline (DESIGN §15 invariant
        // 8): silence unless a Cairo profile is active.
        let cairo_profiles = renderer_profile_names(context, Renderer::Cairo);
        if cairo_profiles.is_empty() {
            return Vec::new();
        }
        let index = context.project_index();
        // Scenes with a definite plain-2D camera contract.
        let standard_scenes: BTreeSet<&str> = context
            .lifecycle_facts()
            .scenes
            .iter()
            .filter(|scene| scene.camera_kind == CameraKind::Standard)
            .map(|scene| scene.qualified_name.as_str())
            .collect();
        if standard_scenes.is_empty() {
            return Vec::new();
        }
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some(class_name) = call.context.class_name.as_deref() else {
                continue;
            };
            if !standard_scenes.contains(class_name) {
                continue;
            }
            let file = context.sources().file(call.file);
            let Some(bindings) = context.binding_facts().file(call.file) else {
                continue;
            };
            let Some(written) = z_stacking_use(profile, index, file, call, bindings) else {
                continue;
            };
            let mut evidence = BTreeMap::new();
            evidence.insert("call".to_owned(), json!(written.method));
            evidence.insert("scene".to_owned(), json!(class_name));
            evidence.insert("camera_kind".to_owned(), json!("standard"));
            diagnostics.push(build_diagnostic(
                &MLR121,
                file,
                call.call_range,
                Confidence::High,
                format!(
                    "`{written}` has no visual effect in this 2D Cairo scene: the \
                     camera drops the z coordinate and the display order sorts by \
                     `z_index` — use `set_z_index(...)` for stacking order",
                    written = written.display,
                ),
                "The plain 2D Camera projects points with only their x/y \
                 components, and Cairo draws the family list stable-sorted by \
                 z_index. Moving a mobject along OUT (or setting its z \
                 coordinate) changes neither its position on screen nor its \
                 stacking, so the intended overlap fix never happens. \
                 set_z_index participates in the display-order sort on every \
                 renderer.",
                evidence,
                cairo_profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// A confirmed z-stacking attempt at one call site.
struct ZStackingUse {
    method: &'static str,
    display: String,
}

impl std::fmt::Display for ZStackingUse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display)
    }
}

/// Classifies one call as `mob.shift(OUT)` or `mob.set_z(literal)` on a
/// confirmed mobject receiver.
fn z_stacking_use(
    profile: &crate::knowledge::KnowledgeProfile,
    index: &crate::frontend::index::ProjectIndex,
    file: &SourceFile,
    call: &QualifiedCall,
    bindings: &FileBindingFacts,
) -> Option<ZStackingUse> {
    let ReceiverKind::KnownInstance(kind) = &call.receiver else {
        return None;
    };
    if super::classify_class(profile, index, kind) == VmClass::Unknown {
        return None;
    }
    if call.has_star_args
        || call.has_star_star_kwargs
        || call.positional_count != 1
        || !call.keyword_names.is_empty()
    {
        return None;
    }
    let argument = call.positional(0)?;
    // `mob.shift(OUT)`: the curated fluent `shift` with the literal OUT
    // direction.
    if let Some((canonical, _)) = resolved_method_for_call(profile, call) {
        if canonical.starts_with("manim.mobject.") && short_name(&canonical) == "shift" {
            let is_out = match &argument.shape {
                ArgShape::Name => is_manim_out_name(bindings, file.slice(argument.range)),
                ArgShape::Attribute(reference) => {
                    reference.candidates.len() == 1
                        && reference.candidates.iter().next().map(String::as_str)
                            == Some(OUT_CONSTANT)
                }
                _ => false,
            };
            if is_out {
                return Some(ZStackingUse {
                    method: "shift",
                    display: format!(".shift({})", file.slice(argument.range)),
                });
            }
        }
        return None;
    }
    // `mob.set_z(literal)`: not curated, so it resolves through the
    // candidate ids instead; a project override of `set_z` yields a
    // project-qualified candidate and stays silent.
    let is_canonical_set_z = !call.candidates.is_empty()
        && call
            .candidates
            .iter()
            .all(|candidate| candidate.starts_with("manim.") && candidate.ends_with(".set_z"));
    if !is_canonical_set_z {
        return None;
    }
    if !matches!(
        argument.literal,
        Some(LiteralFact::Int(_) | LiteralFact::Float(_))
    ) {
        return None;
    }
    Some(ZStackingUse {
        method: "set_z",
        display: format!(".set_z({})", file.slice(argument.range)),
    })
}

/// Whether `name` definitely means `manim.constants.OUT` in this file:
/// bound by `from manim import OUT [as alias]` (or reaching `OUT` through
/// `from manim import *`) and never re-bound by anything else (frontend
/// binding facts). Any non-manim star import distrusts everything.
fn is_manim_out_name(bindings: &FileBindingFacts, name: &str) -> bool {
    if bindings.has_star_other_than("manim") || bindings.is_poisoned(name) {
        return false;
    }
    match bindings.import_target(name) {
        Some("manim.OUT") => true,
        Some(_) => false,
        None => name == "OUT" && bindings.has_star_from("manim"),
    }
}
