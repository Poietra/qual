//! Target-state requirement rules: `MLC107`, `MLC120`.
//!
//! `MoveToTarget` requires `generate_target()` and `Restore` /
//! `Mobject.restore` require `save_state()` on **every** path reaching the
//! construction (DESIGN §3.2). The abstract interpreter records a
//! [`TargetRequirementFact`] with an all-paths presence verdict; these
//! rules fire only on [`Presence::Absent`] — a `Maybe` (present on some
//! paths only) stays silent (DESIGN §15 invariant 2).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{TargetRequirement, TargetRequirementFact};
use crate::semantic::values::{AllocationSite, Presence};

use super::support::{build_diagnostic, site_range};

/// Collects, per requirement kind, the sites where the required state is
/// absent on all paths, with the scenes that prove it.
fn absent_sites(
    context: &RuleContext<'_>,
    requirement: TargetRequirement,
) -> BTreeMap<AllocationSite, Vec<String>> {
    let mut sites: BTreeMap<AllocationSite, Vec<String>> = BTreeMap::new();
    for scene in &context.lifecycle_facts().scenes {
        // An unresolved MRO means an unanalyzed base constructor could have
        // produced the required state: no certain absence claim.
        if scene.constructor_state_unknown {
            continue;
        }
        for fact in &scene.target_requirements {
            let TargetRequirementFact {
                site,
                requirement: fact_requirement,
                presence,
                ..
            } = fact;
            if *fact_requirement != requirement || *presence != Presence::Absent {
                continue;
            }
            let scenes = sites.entry(*site).or_default();
            if !scenes.contains(&scene.qualified_name) {
                scenes.push(scene.qualified_name.clone());
            }
        }
    }
    sites
}

fn requirement_diagnostics(
    metadata: &'static RuleMetadata,
    context: &RuleContext<'_>,
    requirement: TargetRequirement,
    message: impl Fn(&str) -> String,
    explanation: &str,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (site, scenes) in absent_sites(context, requirement) {
        let file = context.sources().file(site.file);
        let range = site_range(&site);
        let text = file.slice(range);
        let mut evidence = BTreeMap::new();
        evidence.insert(
            "presence".to_owned(),
            Value::String("absent-on-all-paths".to_owned()),
        );
        evidence.insert(
            "scenes".to_owned(),
            Value::Array(scenes.into_iter().map(Value::String).collect()),
        );
        diagnostics.push(build_diagnostic(
            metadata,
            context,
            file,
            range,
            message(text),
            explanation.to_owned(),
            evidence,
        ));
    }
    diagnostics
}

// ---------------------------------------------------------------------------
// MLC107: MoveToTarget without generate_target on any path.
// ---------------------------------------------------------------------------

/// Metadata for [`MissingGeneratedTarget`].
pub const MLC107: RuleMetadata = RuleMetadata {
    id: "MLC107",
    summary: "MoveToTarget without generate_target() on any path",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `MoveToTarget(mob)` reached with `mob.generate_target()` absent on every
/// path (DESIGN §7.1 `MLC107`).
pub struct MissingGeneratedTarget;

impl Rule for MissingGeneratedTarget {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC107
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        requirement_diagnostics(
            &MLC107,
            context,
            TargetRequirement::GeneratedTarget,
            |text| {
                format!(
                    "Call `generate_target()` on the mobject before `{text}`: no \
                     execution path generates a target, so this construction raises \
                     `ValueError` at render time."
                )
            },
            "`MoveToTarget` animates a mobject toward the copy stored by \
             `mobject.generate_target()`. Constructing it validates that the target \
             exists; without a prior `generate_target()` (or an `.animate` builder, \
             which generates one) the constructor raises `ValueError` (DESIGN §3.2).",
        )
    }
}

// ---------------------------------------------------------------------------
// MLC120: Restore without save_state on any path.
// ---------------------------------------------------------------------------

/// Metadata for [`MissingSavedState`].
pub const MLC120: RuleMetadata = RuleMetadata {
    id: "MLC120",
    summary: "Restore without save_state() on any path",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `Restore(mob)` / `mob.restore()` reached with `save_state()` absent on
/// every path (DESIGN §7.1 `MLC120`).
pub struct MissingSavedState;

impl Rule for MissingSavedState {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC120
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        requirement_diagnostics(
            &MLC120,
            context,
            TargetRequirement::SavedState,
            |text| {
                format!(
                    "Call `save_state()` on the mobject before `{text}`: no execution \
                     path saves a state, so restoring raises at render time."
                )
            },
            "`Restore` (and `Mobject.restore`) replays the copy stored by \
             `mobject.save_state()`. Without a prior `save_state()` on every path \
             there is nothing to restore and Manim raises at render time \
             (transform.py `Restore`, mobject.py `Mobject.restore`).",
        )
    }
}
