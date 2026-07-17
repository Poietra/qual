//! `.animate` builder rules: `MLC113`, `MLC117`, `MLC124`.
//!
//! An `_AnimationBuilder` runs `generate_target()` the moment `.animate`
//! is read; animation kwargs must be passed by calling `animate` itself
//! **before** any method access (`__call__` raises `ValueError` afterwards,
//! mobject.py `_AnimationBuilder`); and chained methods run against the
//! target copy, so a getter that only returns a value never changes the
//! animation (DESIGN §3.2).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RelatedLocation, RuleMetadata, Severity,
    TextEdit,
};
use crate::frontend::index::QualifiedCall;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{AnimateBuilderFact, PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::state::WriteChannel;
use crate::semantic::values::{AllocationSite, Cardinality, KindSet, ObjectId, Presence, Truth};

use super::support::{build_diagnostic, resolve_symbol, site_range};

// ---------------------------------------------------------------------------
// MLC113: animation kwargs after a builder method access.
// ---------------------------------------------------------------------------

/// Metadata for [`AnimateKwargsAfterMethod`].
pub const MLC113: RuleMetadata = RuleMetadata {
    id: "MLC113",
    summary: "Animation kwargs passed after a .animate method access",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

/// `mob.animate.method(...)(run_time=...)`: calling the builder after a
/// method access raises `ValueError` (DESIGN §7.1 `MLC113`).
pub struct AnimateKwargsAfterMethod;

impl Rule for AnimateKwargsAfterMethod {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC113
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let calls = context.qualified_calls();
        // Call facts by their exact call range, for callee-is-a-call lookup.
        let by_range: BTreeMap<(crate::source::FileId, u32, u32), &QualifiedCall> = calls
            .calls
            .iter()
            .map(|call| {
                (
                    (
                        call.file,
                        call.call_range.start().into(),
                        call.call_range.end().into(),
                    ),
                    call,
                )
            })
            .collect();
        // Chained builder sites, deduplicated across scenes.
        let mut builder_sites: BTreeSet<AllocationSite> = BTreeSet::new();
        for scene in &context.lifecycle_facts().scenes {
            for (site, fact) in &scene.builders {
                if !fact.methods.is_empty() && fact.target.is_some() {
                    builder_sites.insert(*site);
                }
            }
        }
        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for outer in &calls.calls {
            // The callee must itself be a call expression we have a fact for.
            let key = (
                outer.file,
                outer.callee_range.start().into(),
                outer.callee_range.end().into(),
            );
            let Some(inner) = by_range.get(&key) else {
                continue;
            };
            // ... whose own callee is a chained `.animate` builder chain.
            let Some(builder_site) = builder_sites.iter().find(|site| {
                site.file == outer.file
                    && site.start == u32::from(inner.callee_range.start())
                    && site.end <= u32::from(inner.callee_range.end())
            }) else {
                continue;
            };
            let builder_site = *builder_site;
            let trailing = TextRange::new(outer.callee_range.end(), outer.call_range.end());
            if !seen.insert(AllocationSite::new(outer.file, trailing)) {
                continue;
            }
            let file = context.sources().file(outer.file);
            let trailing_text = file.slice(trailing);
            let builder_text = file.slice(site_range(&builder_site));
            let mut evidence = BTreeMap::new();
            evidence.insert("builder".to_owned(), Value::String(builder_text.to_owned()));
            evidence.insert(
                "trailing_call".to_owned(),
                Value::String(trailing_text.to_owned()),
            );
            let mut diagnostic = build_diagnostic(
                &MLC113,
                context,
                file,
                trailing,
                format!(
                    "Pass animation kwargs on `animate` itself, before any method \
                     access: `{builder_text}{trailing_text}.<method>(...)`. Calling \
                     the builder after a method access raises `ValueError` at run \
                     time."
                ),
                "`_AnimationBuilder.__call__` accepts animation kwargs only before \
                 the first method access; every method access (and a first call) \
                 sets `cannot_pass_args`, after which calling the builder raises \
                 `ValueError` (mobject.py `_AnimationBuilder`, DESIGN §3.2)."
                    .to_owned(),
                evidence,
            );
            diagnostic.fix = move_kwargs_fix(file, outer, builder_site, trailing);
            diagnostics.push(diagnostic);
        }
        diagnostics
    }
}

/// SAFE fix: move the trailing `(kwargs)` onto `animate` itself. Only
/// keyword arguments move (the builder's `__call__` accepts nothing else).
fn move_kwargs_fix(
    file: &crate::source::SourceFile,
    outer: &QualifiedCall,
    builder_site: AllocationSite,
    trailing: TextRange,
) -> Option<Fix> {
    if outer.positional_count != 0 || outer.has_star_args {
        return None;
    }
    let trailing_text = file.slice(trailing).to_owned();
    let insert_at = TextRange::empty(site_range(&builder_site).end());
    Some(Fix {
        applicability: FixApplicability::Safe,
        message: format!("Move `{trailing_text}` onto `.animate` itself"),
        edits: vec![
            TextEdit {
                path: file.relative_path().to_owned(),
                span: file.span_of_range(insert_at),
                replacement: trailing_text,
            },
            TextEdit {
                path: file.relative_path().to_owned(),
                span: file.span_of_range(trailing),
                replacement: String::new(),
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// MLC117: stale or overwritten builder target.
// ---------------------------------------------------------------------------

/// Metadata for [`StaleAnimateBuilder`].
pub const MLC117: RuleMetadata = RuleMetadata {
    id: "MLC117",
    summary: "Mobject changed between .animate builder creation and play",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// A played `.animate` builder whose live target was definitely mutated —
/// or whose generated target was overwritten by a later builder — between
/// builder creation and the play (DESIGN §7.1 `MLC117`).
pub struct StaleAnimateBuilder;

impl Rule for StaleAnimateBuilder {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC117
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            let begin_index: BTreeMap<u64, usize> = scene
                .events
                .iter()
                .enumerate()
                .filter_map(|(index, traced)| match &traced.event {
                    Event::BeginPlay(begin) => Some((begin.play_group, index)),
                    _ => None,
                })
                .collect();
            for (site, fact) in &scene.builders {
                if fact.played != Truth::Yes {
                    continue;
                }
                let Some(target) = fact.target.as_ref() else {
                    continue;
                };
                if target.cardinality != Cardinality::Singleton {
                    continue;
                }
                if seen.contains(site) {
                    continue;
                }
                if fact.overwritten_by_later_builder == Truth::Yes {
                    seen.insert(*site);
                    diagnostics.push(overwritten_diagnostic(context, scene, site));
                    continue;
                }
                if let Some(mutation) =
                    definite_stale_mutation(scene, &begin_index, site, fact, target)
                {
                    seen.insert(*site);
                    diagnostics.push(stale_diagnostic(context, scene, site, mutation));
                }
            }
        }
        diagnostics
    }
}

/// A mutation of these kinds between builder creation and play leaves the
/// generated target stale.
const STALE_MUTATIONS: [MutationKind; 5] = [
    MutationKind::Points,
    MutationKind::Style,
    MutationKind::Opacity,
    MutationKind::CameraState,
    MutationKind::Unknown,
];

/// The site of a definite live-target mutation between the builder's
/// creation event and the first play that could have consumed it.
fn definite_stale_mutation(
    scene: &SceneLifecycle,
    begin_index: &BTreeMap<u64, usize>,
    builder_site: &AllocationSite,
    fact: &AnimateBuilderFact,
    target: &ObjectId,
) -> Option<AllocationSite> {
    // Corroborating epochs must both be known and differ.
    let play_epoch = fact.target_epoch_at_play?;
    if play_epoch == fact.target_epoch_at_creation {
        return None;
    }
    // The builder's creation shows up as the generate_target allocation at
    // the builder site.
    let created = scene
        .events
        .iter()
        .position(|traced| traced.site == *builder_site)?;
    // The first play (after creation) with a builder animation on the same
    // target bounds the window from above: the actual consuming play is
    // this one or a later one, so a mutation inside the window precedes it.
    let consuming = scene.plays.iter().find_map(|play| {
        let index = *begin_index.get(&play.play_group.0)?;
        (index > created && play_has_builder_on(play, target)).then_some(index)
    })?;
    for traced in &scene.events[created + 1..consuming] {
        if traced.certainty != Presence::Present {
            continue;
        }
        match &traced.event {
            Event::Mutate(mutate)
                if mutate.target.definitely_same(target)
                    && STALE_MUTATIONS.contains(&mutate.kind) =>
            {
                return Some(traced.site);
            }
            Event::BeginPlay(begin) => {
                // An intervening play whose animations definitely write the
                // target also mutates it (channel interpolation).
                let play = scene
                    .plays
                    .iter()
                    .find(|play| play.play_group.0 == begin.play_group)?;
                if play.certainty == Presence::Present && play_definitely_writes(play, target) {
                    return Some(traced.site);
                }
            }
            _ => {}
        }
    }
    None
}

fn play_has_builder_on(play: &PlayFact, target: &ObjectId) -> bool {
    play.kind == PlayKind::Play
        && play.animations.iter().any(|animation| {
            animation.from_builder
                && animation
                    .state
                    .as_ref()
                    .is_some_and(|state| state.targets.iter().any(|t| t.definitely_same(target)))
        })
}

fn play_definitely_writes(play: &PlayFact, target: &ObjectId) -> bool {
    play.animations.iter().any(|animation| {
        animation.channels_known == Truth::Yes
            && animation.state.as_ref().is_some_and(|state| {
                !state.write_channels.is_empty()
                    && state
                        .write_channels
                        .iter()
                        .any(|channel| *channel != WriteChannel::Membership)
                    && state.targets.iter().any(|t| t.definitely_same(target))
            })
    })
}

fn overwritten_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    site: &AllocationSite,
) -> Diagnostic {
    let file = context.sources().file(site.file);
    let range = site_range(site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "kind".to_owned(),
        Value::String("overwritten-target".to_owned()),
    );
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    build_diagnostic(
        &MLC117,
        context,
        file,
        range,
        format!(
            "A later `.animate` builder on the same mobject replaced the target \
             this `{text}` builder generated; playing this builder animates toward \
             the later builder's target instead. Build and play each `.animate` \
             expression directly inside `self.play(...)`, or merge the chains."
        ),
        "Reading `.animate` runs `generate_target()` immediately and the built \
         animation animates toward `mobject.target` (DESIGN §3.2). Creating a \
         second builder on the same mobject overwrites `mobject.target`, so an \
         earlier, not-yet-played builder silently uses the later target."
            .to_owned(),
        evidence,
    )
}

fn stale_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    site: &AllocationSite,
    mutation: AllocationSite,
) -> Diagnostic {
    let file = context.sources().file(site.file);
    let mutation_file = context.sources().file(mutation.file);
    let range = site_range(site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert("kind".to_owned(), Value::String("stale-target".to_owned()));
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    let mut diagnostic = build_diagnostic(
        &MLC117,
        context,
        file,
        range,
        format!(
            "The mobject is changed between this `{text}` builder's creation and \
             the play that consumes it; `generate_target()` ran at creation, so the \
             animation targets the stale pre-mutation snapshot. Move the `.animate` \
             expression directly into the `self.play(...)` call."
        ),
        "Reading `.animate` runs `generate_target()` immediately (DESIGN §3.2): the \
         builder's target snapshot is taken at creation, not at play. Mutating the \
         live mobject before the play makes the animation start from the mutated \
         state but end at the stale target."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: mutation_file.relative_path().to_owned(),
        span: mutation_file.span_of_range(site_range(&mutation)),
        message: "the live mobject is changed here, after the target snapshot was \
                  taken"
            .to_owned(),
    });
    diagnostic
}

// ---------------------------------------------------------------------------
// MLC124: non-mutating method chained on .animate.
// ---------------------------------------------------------------------------

/// Metadata for [`NonMutatingAnimateMethod`].
pub const MLC124: RuleMetadata = RuleMetadata {
    id: "MLC124",
    summary: ".animate chains a method that does not mutate the target",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `.animate.<getter>()` where the curated method has
/// `returns_self: false` — it returns a value and leaves the target copy
/// unchanged (DESIGN §7.1 `MLC124`).
pub struct NonMutatingAnimateMethod;

impl Rule for NonMutatingAnimateMethod {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC124
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let mut seen: BTreeSet<(AllocationSite, String)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for (site, fact) in &scene.builders {
                let Some(target) = fact.target.as_ref() else {
                    continue;
                };
                let Some(state) = scene.final_heap.object(target) else {
                    continue;
                };
                let KindSet::Known(kinds) = &state.kind else {
                    continue;
                };
                if kinds.is_empty() {
                    continue;
                }
                for method in &fact.methods {
                    if !non_mutating_for_all(profile, kinds, method) {
                        continue;
                    }
                    if !seen.insert((*site, method.clone())) {
                        continue;
                    }
                    diagnostics.push(non_mutating_diagnostic(context, scene, site, method));
                }
            }
        }
        diagnostics
    }
}

/// Whether `method` resolves for every kind candidate to the same curated
/// entry with a positive `returns_self: false` fact.
fn non_mutating_for_all(
    profile: &crate::knowledge::KnowledgeProfile,
    kinds: &BTreeSet<String>,
    method: &str,
) -> bool {
    let mut canonical: Option<String> = None;
    for kind in kinds {
        let Some((id, entry)) = resolve_symbol(profile, &format!("{kind}.{method}")) else {
            return false;
        };
        if entry.returns_self != Some(false) {
            return false;
        }
        match &canonical {
            None => canonical = Some(id),
            Some(existing) if *existing == id => {}
            Some(_) => return false,
        }
    }
    canonical.is_some()
}

fn non_mutating_diagnostic(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    site: &AllocationSite,
    method: &str,
) -> Diagnostic {
    let file = context.sources().file(site.file);
    let range = site_range(site);
    let text = file.slice(range);
    let mut evidence = BTreeMap::new();
    evidence.insert("method".to_owned(), Value::String(method.to_owned()));
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    build_diagnostic(
        &MLC124,
        context,
        file,
        range,
        format!(
            "`{text}.{method}(...)` chains a method that returns a value instead of \
             mutating the mobject; the generated target stays unchanged and the \
             animation shows nothing. Chain a mutating method (`shift`, `become`, \
             `set_...`) on `.animate`, or call `{method}` on the mobject directly."
        ),
        "`.animate` method chains run against the target copy produced by \
         `generate_target()`; the built animation interpolates toward that copy \
         (DESIGN §3.2). A method curated as `returns_self: false` (a getter or \
         copy) does not modify the copy, so the animation has no visible effect."
            .to_owned(),
        evidence,
    )
}
