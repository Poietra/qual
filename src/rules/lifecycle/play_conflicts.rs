//! Same-play conflict rules: `MLC108`, `MLC129`.
//!
//! `MLC108` fires when two animations of one `play` write the same channel
//! of the definitely-same live mobject: the later interpolation overwrites
//! the earlier one every frame (DESIGN §3.2). It requires complete channel
//! classification (`channels_known == Yes`) and definite object identity
//! ([`ObjectId::definitely_same`]); disjoint channels, `Maybe` identity,
//! and loop-widened (non-singleton) targets stay silent.
//!
//! `MLC129` flags `play(..., lag_ratio=...)` with multiple animations: the
//! play kwarg is `setattr` onto **each** animation and does not stagger
//! them (scene.py `compile_animations`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayKind, PlayedAnimation};
use crate::semantic::state::WriteChannel;
use crate::semantic::values::{AllocationSite, ObjectId, Truth};

use super::support::{
    SCENE_PLAY, bound_receiver, build_diagnostic, candidates_value, channel_label,
    conclusive_target, site_range,
};

// ---------------------------------------------------------------------------
// MLC108: conflicting writes to the same live target in one play.
// ---------------------------------------------------------------------------

/// Metadata for [`ConflictingPlayWrites`].
pub const MLC108: RuleMetadata = RuleMetadata {
    id: "MLC108",
    summary: "Two animations in one play write the same channel of the same mobject",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// The definitely-same live target shared by two played animations, if any.
fn shared_target(a: &PlayedAnimation, b: &PlayedAnimation) -> Option<ObjectId> {
    let (state_a, state_b) = (a.state.as_ref()?, b.state.as_ref()?);
    for target_a in &state_a.targets {
        for target_b in &state_b.targets {
            if target_a.definitely_same(target_b) {
                return Some(target_a.clone());
            }
        }
    }
    None
}

/// Channels both animations definitely write.
fn shared_channels(a: &PlayedAnimation, b: &PlayedAnimation) -> BTreeSet<WriteChannel> {
    match (&a.state, &b.state) {
        (Some(state_a), Some(state_b)) => state_a
            .write_channels
            .intersection(&state_b.write_channels)
            .copied()
            .collect(),
        _ => BTreeSet::new(),
    }
}

/// Two `.animate` builders (or plain animations) of the same play writing
/// the same channel of the same live mobject (DESIGN §7.1 `MLC108`).
pub struct ConflictingPlayWrites;

impl Rule for ConflictingPlayWrites {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC108
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for play in &scene.plays {
                if play.kind != PlayKind::Play {
                    continue;
                }
                for (first_index, first) in play.animations.iter().enumerate() {
                    for second in play.animations.iter().skip(first_index + 1) {
                        // Only a complete channel classification supports a
                        // conflict claim (absence of a channel is evidence
                        // only when the set is complete).
                        if first.channels_known != Truth::Yes || second.channels_known != Truth::Yes
                        {
                            continue;
                        }
                        let Some(target) = shared_target(first, second) else {
                            continue;
                        };
                        let channels = shared_channels(first, second);
                        if channels.is_empty() {
                            continue;
                        }
                        if !seen.insert((first.site, second.site)) {
                            continue;
                        }
                        diagnostics.push(conflict_diagnostic(
                            context, scene, first, second, &target, &channels,
                        ));
                    }
                }
            }
        }
        diagnostics
    }
}

fn conflict_diagnostic(
    context: &RuleContext<'_>,
    scene: &crate::semantic::interpreter::SceneLifecycle,
    first: &PlayedAnimation,
    second: &PlayedAnimation,
    target: &ObjectId,
    channels: &BTreeSet<WriteChannel>,
) -> Diagnostic {
    let file = context.sources().file(second.site.file);
    let first_file = context.sources().file(first.site.file);
    let channel_names: Vec<&str> = channels
        .iter()
        .map(|&channel| channel_label(channel))
        .collect();
    let channel_list = channel_names.join(", ");
    let target_file = context.sources().file(target.site.file);
    let target_text = target_file.slice(site_range(&target.site));
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "channels".to_owned(),
        Value::Array(
            channel_names
                .iter()
                .map(|name| Value::String((*name).to_owned()))
                .collect(),
        ),
    );
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    evidence.insert(
        "target_allocation".to_owned(),
        Value::String(target_text.to_owned()),
    );
    let mut diagnostic = build_diagnostic(
        &MLC108,
        context,
        file,
        site_range(&second.site),
        format!(
            "This animation writes the same channel(s) ({channel_list}) of the same \
             live mobject as an earlier animation in the same `play`; the later \
             interpolation overwrites the earlier one every frame. If both effects \
             are wanted, merge them into one `.animate` chain (note the shared rate \
             function and path may change the motion)."
        ),
        "Animations of one `Scene.play` interpolate in order on every frame; when \
         two of them write the same channel of the definitely-same live mobject, \
         the last write wins and the first animation's effect is discarded \
         (DESIGN §3.2). A single `.animate` chain such as \
         `mob.animate.shift(...).rotate(...)` applies both effects in one animation."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: first_file.relative_path().to_owned(),
        span: first_file.span_of_range(site_range(&first.site)),
        message: "first animation writing the shared channel(s)".to_owned(),
    });
    diagnostic
}

// ---------------------------------------------------------------------------
// MLC129: play-level lag_ratio with multiple animations.
// ---------------------------------------------------------------------------

/// Metadata for [`PlayLagRatioStagger`].
pub const MLC129: RuleMetadata = RuleMetadata {
    id: "MLC129",
    summary: "play(..., lag_ratio=...) does not stagger multiple animations",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `play(a, b, lag_ratio=x)` used as an inter-animation stagger
/// (DESIGN §7.1 `MLC129`).
pub struct PlayLagRatioStagger;

impl Rule for PlayLagRatioStagger {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC129
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if !bound_receiver(call) || call.has_star_args || call.positional_count < 2 {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != SCENE_PLAY {
                continue;
            }
            let Some(argument) = call.keyword("lag_ratio") else {
                continue;
            };
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(SCENE_PLAY.to_owned()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            evidence.insert(
                "positional_animations".to_owned(),
                Value::Number(call.positional_count.into()),
            );
            diagnostics.push(build_diagnostic(
                &MLC129,
                context,
                file,
                argument.range,
                "`lag_ratio` passed to `Scene.play` is applied to each animation \
                 individually and does not stagger them against each other; wrap the \
                 animations in `AnimationGroup(..., lag_ratio=...)` or \
                 `LaggedStart(...)` to get a stagger."
                    .to_owned(),
                "`Scene.play` copies its keyword arguments onto every compiled \
                 animation via `setattr` (scene.py `compile_animations`); each \
                 animation's own `lag_ratio` only staggers its internal submobjects. \
                 Inter-animation staggering is a property of the animation *group*, \
                 so it belongs in the `AnimationGroup` / `LaggedStart` constructor."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}
