//! State-dependent rendering rules over lifecycle facts (Phase 2):
//! `MLR102` (played bare `.animate`), `MLR113` (self-Transform), and
//! `MLR125` (bare `Mobject()` leaf added as a display object).
//!
//! Every rule reads only definite interpreter facts: `Truth::Yes` /
//! `Presence::Present` / [`ObjectId::definitely_same`]. Any `Maybe` fact
//! means silence (DESIGN §15 invariant 2).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, TextEdit,
};
use crate::frontend::index::QualifiedCallFacts;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::Event;
use crate::semantic::interpreter::{PlayFact, SceneLifecycle};
use crate::semantic::values::{AllocationSite, KindSet, ObjectId, Presence, Truth};
use crate::source::SourceFile;

use super::{call_at, site_range};

/// The canonical id of the base (non-drawable) `Mobject` class.
const MOBJECT: &str = "manim.mobject.mobject.Mobject";
/// Transform-family animations whose `(source, target)` identity `MLR113`
/// judges. Only classes whose default interpolation is a provable no-op
/// when source and target are the same live object belong here (arc-path
/// subclasses like `ClockwiseTransform` are visible even then).
const SELF_NOOP_TRANSFORMS: &[&str] = &[
    "manim.animation.transform.ReplacementTransform",
    "manim.animation.transform.Transform",
];

// ---------------------------------------------------------------------------
// MLR102: a bare `mob.animate` (no chained method) is played, and the
// target is provably unchanged and already displayed — the play renders
// identical frames.
// ---------------------------------------------------------------------------

const MLR102: RuleMetadata = RuleMetadata {
    id: "MLR102",
    summary: "Bare `.animate` without a method call is played; the animation changes nothing",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct BarePlayedAnimate;

impl Rule for BarePlayedAnimate {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR102
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let calls = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for (site, builder) in &scene.builders {
                if !builder.methods.is_empty() || builder.played != Truth::Yes {
                    continue;
                }
                let Some(target) = &builder.target else {
                    continue;
                };
                // The target must be provably unchanged between builder
                // creation (`generate_target()`) and the play; a stale
                // target is MLC117 territory, not a no-op.
                if builder.target_epoch_at_play != Some(builder.target_epoch_at_creation) {
                    continue;
                }
                // The play this builder was compiled into (helper-body
                // plays have no PlayFact: stay silent there).
                let Some(play) = scene.plays.iter().find(|play| {
                    play.animations
                        .iter()
                        .any(|animation| animation.from_builder && animation.site == *site)
                }) else {
                    continue;
                };
                // Already displayed on every path: otherwise the play's
                // auto-add makes the target appear, which is an effect.
                let membership = scene.membership_at(target, site.file, play.site.start);
                if membership.map(|(_, family)| family) != Some(Presence::Present) {
                    continue;
                }
                if !seen.insert(*site) {
                    continue;
                }
                let file = context.sources().file(site.file);
                let written = file.slice(site_range(*site));
                let receiver = written.strip_suffix(".animate").unwrap_or(written);
                let mut evidence = BTreeMap::new();
                evidence.insert("builder".to_owned(), json!(written));
                evidence.insert("chained_methods".to_owned(), json!([] as [&str; 0]));
                evidence.insert("target_membership".to_owned(), json!("present"));
                evidence.insert("target_unchanged".to_owned(), json!(true));
                diagnostics.push(super::build_diagnostic(
                    &MLR102,
                    file,
                    site_range(*site),
                    Confidence::High,
                    format!(
                        "`{written}` is played without chaining any method: the \
                         animation interpolates `{receiver}` to its own unchanged \
                         state, so every frame is identical"
                    ),
                    "Accessing `.animate` runs `generate_target()` immediately; \
                     chained methods mutate that target copy and the play \
                     interpolates the live mobject toward it. With no chained \
                     method the target equals the live state, so the play only \
                     holds the picture for its run_time. Deleting the argument is \
                     unsafe because the play's duration (an implicit pause) \
                     would disappear with it.",
                    evidence,
                    profiles.clone(),
                    deletion_fix(file, calls, play, *site),
                ));
            }
        }
        diagnostics
    }
}

/// UNSAFE fix: delete the bare builder argument from the play call.
///
/// Only offered when another positional animation remains (an empty
/// `play()` is a runtime error) and the deletion boundary is a plain
/// positional neighbor, so the edit cannot eat a keyword. Unsafe because
/// the play's `run_time` (an implicit pause) disappears with the
/// argument.
fn deletion_fix(
    file: &SourceFile,
    calls: &QualifiedCallFacts,
    play: &PlayFact,
    builder_site: AllocationSite,
) -> Option<Fix> {
    let call = call_at(calls, play.site)?;
    if call.positional_count < 2 || call.has_star_args {
        return None;
    }
    let builder_range = site_range(builder_site);
    let index = call
        .arguments
        .iter()
        .position(|argument| argument.keyword.is_none() && argument.range == builder_range)?;
    let deleted = if let Some(next) = call.arguments.get(index + 1) {
        // Not the last argument: delete up to the next argument's start,
        // which must be positional (a keyword's range excludes `name=`).
        if next.keyword.is_some() {
            return None;
        }
        TextRange::new(builder_range.start(), next.range.start())
    } else {
        let previous = call.arguments.get(index.checked_sub(1)?)?;
        if previous.keyword.is_some() {
            return None;
        }
        TextRange::new(previous.range.end(), builder_range.end())
    };
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: "Delete the bare `.animate` argument. Unsafe: the play's \
                  run_time currently acts as a pause, and deleting the \
                  argument changes that timing."
            .to_owned(),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(deleted),
            replacement: String::new(),
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR113: Transform(mob, mob) — source and target are provably the same
// live object.
// ---------------------------------------------------------------------------

const MLR113: RuleMetadata = RuleMetadata {
    id: "MLR113",
    summary: "Transform source and target are the same object",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct SelfTransform;

impl Rule for SelfTransform {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR113
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let calls = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for play in &scene.plays {
                for played in &play.animations {
                    let (Some(state), Some(replacement)) =
                        (&played.state, &played.replacement_target)
                    else {
                        continue;
                    };
                    let KindSet::Known(kinds) = &state.kind else {
                        continue;
                    };
                    if kinds.is_empty()
                        || !kinds
                            .iter()
                            .all(|kind| SELF_NOOP_TRANSFORMS.contains(&kind.as_str()))
                    {
                        continue;
                    }
                    if state.targets.len() != 1 {
                        continue;
                    }
                    let target = state.targets.iter().next().expect("len checked");
                    // Never fire on Maybe aliasing: only definite identity.
                    if !target.definitely_same(replacement) {
                        continue;
                    }
                    // A custom path (path_arc / path_func) makes even a
                    // self-transform visibly move.
                    let Some(constructor) = call_at(calls, played.site) else {
                        continue;
                    };
                    if constructor.has_star_args
                        || constructor.has_star_star_kwargs
                        || constructor.keyword_names.contains("path_arc")
                        || constructor.keyword_names.contains("path_func")
                    {
                        continue;
                    }
                    if !seen.insert(played.site) {
                        continue;
                    }
                    let file = context.sources().file(played.site.file);
                    let kind_names: Vec<&str> =
                        kinds.iter().map(|kind| super::short_name(kind)).collect();
                    let name = kind_names.join("` / `");
                    let mut evidence = BTreeMap::new();
                    evidence.insert("animation".to_owned(), json!(kinds));
                    evidence.insert("identity".to_owned(), json!("definitely-same"));
                    diagnostics.push(super::build_diagnostic(
                        &MLR113,
                        file,
                        site_range(played.site),
                        Confidence::High,
                        format!(
                            "`{name}` source and target are the same object: the \
                             animation morphs the mobject into its current state \
                             and shows no change"
                        ),
                        "Transform aligns the source with a copy of the target and \
                         interpolates between them; when both are the same live \
                         object every interpolation step reproduces the current \
                         state. ReplacementTransform additionally swaps the source \
                         for the target in the scene, which is also a no-op here. \
                         Pass a distinct target (e.g. a moved `.copy()`), or use \
                         `.animate` to express the intended change.",
                        evidence,
                        profiles.clone(),
                        None,
                    ));
                }
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLR125: a bare `Mobject()` leaf (no children, no updaters, no drawable
// points) is added to the scene as a display object.
// ---------------------------------------------------------------------------

const MLR125: RuleMetadata = RuleMetadata {
    id: "MLR125",
    summary: "Bare Mobject() leaf added to the scene displays nothing",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

pub(super) struct BareMobjectLeaf;

impl Rule for BareMobjectLeaf {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR125
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let profiles = context.config().active_profile_names();
        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            let (parents, updater_hosts) = container_and_updater_sites(scene);
            for event in &scene.events {
                let Event::SceneAdd(add) = &event.event else {
                    continue;
                };
                // A branch-dependent add supports at most medium
                // confidence: below this rule's minimum, so stay silent.
                if event.certainty != Presence::Present {
                    continue;
                }
                // Play-driven adds (auto-add / introducer setup) are the
                // animation's business: a bad animation target is MLR101
                // territory, and a play never "adds a display object" in
                // the sense of this rule.
                if scene.plays.iter().any(|play| {
                    play.site.file == event.site.file
                        && play.site.start <= event.site.start
                        && event.site.end <= play.site.end
                }) {
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
                    if kinds.len() != 1 || kinds.iter().next().map(String::as_str) != Some(MOBJECT)
                    {
                        continue;
                    }
                    // Container or updater-host uses are legitimate.
                    if !state.children.is_empty()
                        || !state.updaters.is_empty()
                        || parents.contains(&resolved)
                        || updater_hosts.contains(&resolved)
                    {
                        continue;
                    }
                    if !seen.insert((event.site, resolved.site)) {
                        continue;
                    }
                    let file = context.sources().file(event.site.file);
                    let mut evidence = BTreeMap::new();
                    evidence.insert("kind".to_owned(), json!(MOBJECT));
                    evidence.insert("children".to_owned(), json!(0));
                    evidence.insert("updaters".to_owned(), json!(0));
                    diagnostics.push(super::build_diagnostic(
                        &MLR125,
                        file,
                        site_range(event.site),
                        Confidence::High,
                        "A bare `Mobject()` has no drawable points: adding it to \
                         the scene as a display object shows nothing. If it is a \
                         deliberate container or positional anchor, suppress with \
                         `# manim-lint: ignore[MLR125]`"
                            .to_owned(),
                        "The base Mobject class carries no geometry the renderers \
                         can draw; only subclasses with points (VMobjects, images) \
                         produce pixels. Adding an empty leaf keeps the scene \
                         visually unchanged. Legitimate uses — grouping children \
                         or hosting updaters — never fire this rule.",
                        evidence,
                        profiles.clone(),
                        None,
                    ));
                }
            }
        }
        diagnostics
    }
}

/// Objects that were ever used as a structural parent (`AddChild`) or as
/// an updater host, by resolved id. Both uses make a points-free
/// `Mobject()` legitimate, even if the children or updaters are gone by
/// tear-down.
fn container_and_updater_sites(scene: &SceneLifecycle) -> (BTreeSet<ObjectId>, BTreeSet<ObjectId>) {
    let mut parents = BTreeSet::new();
    let mut hosts = BTreeSet::new();
    for event in &scene.events {
        match &event.event {
            Event::AddChild(add) => {
                parents.insert(scene.final_heap.resolve(&add.parent));
            }
            Event::RegisterUpdater(register) => {
                hosts.insert(scene.final_heap.resolve(&register.target));
            }
            _ => {}
        }
    }
    (parents, hosts)
}
