//! `MLC111`: an updater-bearing object renders frames while it is in
//! neither the scene family nor an animation (DESIGN §7.1).
//!
//! The abstract interpreter classifies every statement snapshot via
//! [`SceneLifecycle::ownership_intervals`]: effective scene-family
//! membership, whether the object is a live target of a play issued
//! *directly* by that statement, and whether it carries registered
//! updaters. A violation interval needs `in_family == Absent` **and**
//! `animation_target == Absent` on every path, gated on an updater
//! registration with `Present` certainty — `Maybe` membership is never a
//! violation (DESIGN §15).
//!
//! An orphaned updater only matters while frames are produced, so the
//! rule anchors on the first `play` / `wait` inside the violating
//! interval; a register-then-`self.add` sequence with no rendering in
//! between stays silent.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RelatedLocation, RuleMetadata, Severity,
    TextEdit,
};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::UpdaterRegistration;
use crate::semantic::values::{AllocationSite, Presence, Truth};

use super::support::{build_diagnostic, site_range};

/// Metadata for [`OrphanedUpdaterObject`].
pub const MLC111: RuleMetadata = RuleMetadata {
    id: "MLC111",
    summary: "Updater-bearing object is in neither the scene family nor an animation",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// Frames render while an updater-bearing object is provably outside the
/// scene family and not an animation target (DESIGN §7.1 `MLC111`).
pub struct OrphanedUpdaterObject;

impl Rule for OrphanedUpdaterObject {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC111
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        use crate::semantic::interpreter::UpdaterHost;

        // Dedupe on (rendering statement, object allocation): two Present
        // registrations on the same object report once per interval.
        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for registration in &scene.updaters {
                let UpdaterHost::Mobject(object) = &registration.host else {
                    continue;
                };
                if registration.certainty != Presence::Present {
                    continue;
                }
                let intervals = scene.ownership_intervals(object);
                let mut reported_in_run = false;
                for interval in &intervals {
                    let violating = interval.in_family == Presence::Absent
                        && interval.animation_target == Presence::Absent
                        && interval.has_updaters != Truth::No;
                    if !violating {
                        reported_in_run = false;
                        continue;
                    }
                    if reported_in_run {
                        continue;
                    }
                    // The interval only matters once frames are produced:
                    // find a play / wait issued by this very statement.
                    let Some(play) = scene.plays.iter().find(|play| {
                        play.site.file == interval.site.file
                            && play.site.start >= interval.site.start
                            && play.site.end <= interval.site.end
                    }) else {
                        continue;
                    };
                    reported_in_run = true;
                    if !seen.insert((play.site, object.site)) {
                        continue;
                    }
                    let file = context.sources().file(play.site.file);
                    let registration_file = context.sources().file(registration.site.file);
                    let registration_span =
                        registration_file.span_of_range(site_range(&registration.site));
                    let mut evidence = BTreeMap::new();
                    evidence.insert(
                        "scene".to_owned(),
                        Value::String(scene.qualified_name.clone()),
                    );
                    evidence.insert(
                        "registration".to_owned(),
                        Value::String(format!(
                            "{}:{}",
                            registration_file.relative_path(),
                            registration_span.start.line
                        )),
                    );
                    evidence.insert("in_family".to_owned(), Value::String("absent".to_owned()));
                    evidence.insert(
                        "animation_target".to_owned(),
                        Value::String("absent".to_owned()),
                    );
                    evidence.insert(
                        "registration_certainty".to_owned(),
                        Value::String("present".to_owned()),
                    );
                    let mut diagnostic = build_diagnostic(
                        &MLC111,
                        context,
                        file,
                        site_range(&play.site),
                        format!(
                            "Frames render here while the object whose updater is \
                             registered at line {line} is in neither the scene family \
                             nor an animation: the object is invisible and its updater \
                             never runs. Add it to the scene (`self.add(...)`) before \
                             rendering, or remove the updater.",
                            line = registration_span.start.line,
                        ),
                        "Manim only invokes a mobject updater while the mobject is \
                         reachable from the scene (`Scene.mobjects` family walk) or \
                         owned by an in-flight animation. An updater registered on an \
                         object that is in neither is dead weight: nothing is drawn \
                         and the callback is never called (DESIGN §3.3)."
                            .to_owned(),
                        evidence,
                    );
                    diagnostic.related_locations.push(RelatedLocation {
                        path: registration_file.relative_path().to_owned(),
                        span: registration_span,
                        message: "updater registered here".to_owned(),
                    });
                    diagnostic.fix = add_to_scene_fix(context, registration, &interval.site);
                    diagnostics.push(diagnostic);
                }
            }
        }
        diagnostics
    }
}

/// Builds the unsafe `self.add(<name>)` insertion before the first
/// rendering statement of the violating interval, when the registration
/// receiver is a plain identifier in the same file. `None` otherwise —
/// the diagnostic still stands without a mechanical fix.
fn add_to_scene_fix(
    context: &RuleContext<'_>,
    registration: &UpdaterRegistration,
    statement: &AllocationSite,
) -> Option<Fix> {
    if registration.site.file != statement.file {
        return None;
    }
    let file = context.sources().file(statement.file);
    let registration_text = file.slice(site_range(&registration.site));
    let name = registration_text.split(".add_updater").next()?;
    let identifier = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_');
    if !identifier {
        return None;
    }
    // The statement must start its line so the inserted line inherits a
    // meaningful indentation.
    let text = file.text();
    let start = statement.start as usize;
    let line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let indent = &text[line_start..start];
    if !indent
        .chars()
        .all(|character| character == ' ' || character == '\t')
    {
        return None;
    }
    let insertion = TextRange::new(statement.start.into(), statement.start.into());
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: format!("Insert `self.add({name})` before this statement"),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(insertion),
            replacement: format!("self.add({name})\n{indent}"),
        }],
    })
}
