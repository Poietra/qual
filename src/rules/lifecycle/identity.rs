//! Post-Transform identity rule: `MLC116`.
//!
//! A normal `Transform(source, target)` interpolates the live `source` in
//! place. It neither replaces the Scene root with `target` nor adds `target`.
//! The narrow pattern below reports only when a later `.animate` definitely
//! targets that absent `target` while the transformed `source` is still in
//! the Scene: `Scene.play` will auto-add a second object at that point.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::knowledge::KnowledgeProfile;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayKind, PlayedAnimation, SceneLifecycle};
use crate::semantic::values::{Cardinality, KindSet, ObjectId, Presence, Truth};

use super::support::{build_diagnostic, site_range};

const TRANSFORM: &str = "manim.animation.transform.Transform";

/// Metadata for [`PostTransformTargetConfusion`].
pub const MLC116: RuleMetadata = RuleMetadata {
    id: "MLC116",
    summary: "Later operations confuse post-Transform source/target identity with scene membership",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// A later `.animate` on the absent target of a completed normal Transform
/// while the live source remains displayed (DESIGN §7.1 `MLC116`).
pub struct PostTransformTargetConfusion;

impl Rule for PostTransformTargetConfusion {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC116
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let mut seen = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for (transform_index, transform_play) in scene.plays.iter().enumerate() {
                if transform_play.kind != PlayKind::Play
                    || transform_play.certainty != Presence::Present
                {
                    continue;
                }
                for transform in &transform_play.animations {
                    let Some((source, target)) = normal_transform_relation(profile, transform)
                    else {
                        continue;
                    };
                    for later_play in scene.plays.iter().skip(transform_index + 1) {
                        if later_play.kind != PlayKind::Play
                            || later_play.certainty != Presence::Present
                        {
                            continue;
                        }
                        for later in &later_play.animations {
                            if !later.from_builder
                                || !animation_targets(later, &target)
                                || later
                                    .state
                                    .as_ref()
                                    .is_none_or(|state| state.introducer != Truth::No)
                            {
                                continue;
                            }
                            let source_membership = scene
                                .membership_at(&source, later_play.site.file, later_play.site.start)
                                .map(|(_, family)| family);
                            let target_membership = scene
                                .membership_at(&target, later_play.site.file, later_play.site.start)
                                .map(|(_, family)| family);
                            if source_membership != Some(Presence::Present)
                                || target_membership != Some(Presence::Absent)
                                || !seen.insert(later.site)
                            {
                                continue;
                            }
                            diagnostics
                                .push(post_transform_diagnostic(context, scene, transform, later));
                        }
                    }
                }
            }
        }
        diagnostics
    }
}

/// The definite `(source, target)` relation of a curated normal Transform.
/// Replacement transforms are excluded by their positive replacement effect;
/// project/custom Animation classes are not guessed from their names.
fn normal_transform_relation(
    profile: &KnowledgeProfile,
    played: &PlayedAnimation,
) -> Option<(ObjectId, ObjectId)> {
    let state = played.state.as_ref()?;
    if state.replacement != Truth::No
        || state.remover != Truth::No
        || played.channels_known != Truth::Yes
    {
        return None;
    }
    let KindSet::Known(kinds) = &state.kind else {
        return None;
    };
    if kinds.is_empty()
        || !kinds
            .iter()
            .all(|kind| curated_reaches(profile, kind, TRANSFORM))
    {
        return None;
    }
    let mut sources = state.targets.iter();
    let source = sources.next()?.clone();
    if sources.next().is_some() || source.cardinality != Cardinality::Singleton {
        return None;
    }
    let target = played.replacement_target.clone()?;
    if target.cardinality != Cardinality::Singleton || source.definitely_same(&target) {
        return None;
    }
    Some((source, target))
}

fn curated_reaches(profile: &KnowledgeProfile, kind: &str, base: &str) -> bool {
    let mut queue = vec![kind.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(current) = queue.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if current == base {
            return true;
        }
        let Some(entry) = profile.symbol(&current) else {
            // A project/custom class may override lifecycle methods; absent
            // curated semantics is Unknown, never positive evidence. Keep
            // checking any other known base paths before giving up.
            continue;
        };
        queue.extend(entry.bases.iter().cloned());
    }
    false
}

fn animation_targets(animation: &PlayedAnimation, expected: &ObjectId) -> bool {
    animation.state.as_ref().is_some_and(|state| {
        state.targets.len() == 1
            && state
                .targets
                .iter()
                .all(|target| target.definitely_same(expected))
    })
}

fn post_transform_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    transform: &PlayedAnimation,
    later: &PlayedAnimation,
) -> Diagnostic {
    let file = context.sources().file(later.site.file);
    let transform_file = context.sources().file(transform.site.file);
    let later_text = file.slice(site_range(&later.site));
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    evidence.insert(
        "source_membership".to_owned(),
        Value::String("present".to_owned()),
    );
    evidence.insert(
        "target_membership".to_owned(),
        Value::String("absent".to_owned()),
    );
    evidence.insert(
        "later_effect".to_owned(),
        Value::String("non-introducer-auto-add".to_owned()),
    );
    let mut diagnostic = build_diagnostic(
        &MLC116,
        context,
        file,
        site_range(&later.site),
        format!(
            "`{later_text}` animates the target object from an earlier normal \
             `Transform`, but that target is not in the Scene; the transformed \
             source is still the displayed object, so this play auto-adds a second \
             object. Animate the source alias if the intent is to continue moving \
             the transformed object."
        ),
        "A normal `Transform(source, target)` mutates `source` in place to look \
         like `target`; cleanup does not replace it with `target` (unlike \
         `ReplacementTransform`). A later non-introducer animation of the absent \
         target triggers `Scene.play`'s auto-add step and leaves the transformed \
         source present as a separate object (DESIGN §3.2)."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: transform_file.relative_path().to_owned(),
        span: transform_file.span_of_range(site_range(&transform.site)),
        message: "normal Transform keeps its source as the live Scene object here".to_owned(),
    });
    diagnostic
}
