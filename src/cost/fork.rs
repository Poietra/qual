//! Fork fast-path gate evaluation (DESIGN §7.3, `MLP225`).
//!
//! Walks each scene's plays in program order against the local fork
//! overlay's curated gate facts ([`CairoForkGate`], [`CairoStaticLayers`],
//! [`CairoBulkInterpolation`]) and the analyzed render profiles'
//! `cairo_fork_workers` / `cairo_static_layers` requests, producing one
//! per-play verdict per gate:
//!
//! - **clear** — no statically detectable blocker (the runtime audit
//!   still applies; eligibility is never *proven* here);
//! - **blocked** — a curated [`ForkBlocker`] is statically proven, with
//!   the causing span (the causality the cost report explains);
//! - **monotonically disabled** — the fork gate only: the overlay curates
//!   `monotonic_disable`, so the first play that provably renders
//!   serially opens the parent partial-movie encoder when written and
//!   every later play — eligible or not — renders serially too. Per-play
//!   independence is deliberately **not** assumed.
//! - **not applicable / unaudited** — honest refusals: threshold not
//!   reached, or a fact the static analysis cannot verify.
//!
//! Inertness gate (DESIGN §7.3): everything here evaluates to nothing
//! unless the loaded knowledge profile declares `fork_capabilities` — the
//! upstream profile does not, so fork-path interpretation never leaks into
//! `upstream_0_20` runs. A profile whose `cairo_fork_workers` is below the
//! curated `min_workers` is **unrequested**, never a reported loss
//! (workers 0 is not a blocker).
//!
//! Everything reported is causal: the report explains which feature
//! closes which fast path and what the render-path consequence is. It
//! never advises removing a feature — Scene updaters, foreground
//! registration, custom rate functions and the rest can be correct
//! expression (DESIGN §7.3 `MLP225` prose).

use std::collections::BTreeMap;

use crate::config::model::{Platform, RenderProfile, Renderer};
use crate::frontend::index::{ArgShape, QualifiedCall, QualifiedCallFacts};
use crate::knowledge::{
    CairoBulkInterpolation, CairoForkGate, CairoStaticLayers, ForkBlocker, KnowledgeProfile,
    SceneMembershipEffect,
};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::interpreter::{LifecycleFacts, PlayFact, PlayKind, SceneLifecycle};
use crate::semantic::state::CallbackRef;
use crate::semantic::values::{AllocationSite, KindSet, ObjectId, Presence, Truth};
use crate::source::FileId;

use super::contexts::resolve_candidate;
use super::estimator::frames_across_profiles;

/// Which fork fast path a verdict is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GateKind {
    /// The fork-per-play Cairo pipeline (`cairo_fork_workers`).
    ForkPerPlay,
    /// The z-ordered static layer plan (`cairo_static_layers`).
    StaticLayers,
    /// The packed (bulk) interpolation fast path.
    BulkInterpolation,
}

impl GateKind {
    /// Human label used by the cost report and `MLP225` messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ForkPerPlay => "fork-per-play",
            Self::StaticLayers => "static layers",
            Self::BulkInterpolation => "packed interpolation",
        }
    }

    /// The measured calibration evidence the DESIGN quotes for this gate
    /// (`docs/research/perf-evidence.md`), when it quotes any. These are
    /// machine-specific measurements, cited as evidence — never portable
    /// wall-time truth.
    #[must_use]
    pub const fn measured_evidence(self) -> Option<&'static str> {
        match self {
            Self::ForkPerPlay => Some(
                "measured fork-per-play A/B on the calibration machine at 1080p: \
                 Bayes 7.55 -> 3.95 s, Algorithm 12.13 -> 7.50 s \
                 (docs/research/perf-evidence.md)",
            ),
            Self::StaticLayers => None,
            Self::BulkInterpolation => Some(
                "measured packed interpolation on the calibration machine, \
                 300 members / 60 frames: 130.658 -> 33.004 ms/play, \
                 steady state 2.0761 -> 0.1890 ms/frame \
                 (docs/research/perf-evidence.md)",
            ),
        }
    }

    /// What the play keeps paying when this gate stays closed.
    #[must_use]
    const fn loss_phrase(self) -> &'static str {
        match self {
            Self::ForkPerPlay => "serial fallback",
            Self::StaticLayers => "legacy static path",
            Self::BulkInterpolation => "canonical per-member interpolation",
        }
    }

    /// The clear-verdict wording (never an eligibility *proof*).
    #[must_use]
    const fn clear_phrase(self) -> &'static str {
        match self {
            Self::ForkPerPlay => {
                "no static blocker found (fork-eligible pending the runtime audit)"
            }
            Self::StaticLayers => "no static blocker found",
            Self::BulkInterpolation => {
                "no closing blocker found (member-count thresholds not statically audited)"
            }
        }
    }
}

/// One statically proven blocking cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockCause {
    /// The curated blocker, exactly as the overlay lists it.
    pub blocker: ForkBlocker,
    /// Span of the causing feature (`None` when the cause is the play
    /// itself or renderer-wide configuration).
    pub site: Option<AllocationSite>,
    /// Human phrase naming the feature (no location; the renderer appends
    /// the resolved span).
    pub detail: String,
}

/// Per-play verdict of one gate.
#[derive(Debug, Clone, PartialEq)]
pub enum GateVerdict {
    /// No statically detectable blocker (the runtime audit still applies).
    Clear,
    /// Statically proven blocking causes, most causal first.
    Blocked(Vec<BlockCause>),
    /// Fork gate only: the play itself carries no blocker, but an earlier
    /// play provably rendered serially and — per the curated
    /// `monotonic_disable` — opened the parent encoder, disabling forking
    /// renderer-wide for every later play.
    MonotonicallyDisabled {
        /// 1-based ordinal of the first serially rendered play.
        first_ordinal: usize,
        /// Site of that play.
        first_site: AllocationSite,
        /// The blocker that forced it onto the serial path.
        first_blocker: ForkBlocker,
    },
    /// The fast path does not apply to this play (threshold not reached,
    /// or the play is already on its optimal path).
    NotApplicable {
        /// Why the gate does not apply.
        reason: String,
    },
    /// The static analysis cannot audit this play either way; no loss and
    /// no eligibility is claimed.
    Unaudited {
        /// What could not be verified.
        reason: String,
    },
}

/// One play's outcome under one gate.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayGateOutcome {
    /// 1-based position among the scene's play executions.
    pub ordinal: usize,
    /// Source site of the play call.
    pub site: AllocationSite,
    /// The verdict.
    pub verdict: GateVerdict,
}

/// One gate's evaluation for one render profile.
#[derive(Debug, Clone, PartialEq)]
pub enum GateEvaluation {
    /// The overlay does not curate this capability; nothing to say.
    NotDeclared,
    /// The profile does not request the fast path; never a reported loss.
    Unrequested {
        /// Why the gate is unrequested (e.g. workers below the minimum).
        reason: String,
    },
    /// Per-play verdicts in program order.
    Plays(Vec<PlayGateOutcome>),
}

impl GateEvaluation {
    /// Whether any outcome reports a proven loss (blocked or disabled).
    #[must_use]
    pub fn has_loss(&self) -> bool {
        match self {
            Self::NotDeclared | Self::Unrequested { .. } => false,
            Self::Plays(outcomes) => outcomes.iter().any(|outcome| {
                matches!(
                    outcome.verdict,
                    GateVerdict::Blocked(_) | GateVerdict::MonotonicallyDisabled { .. }
                )
            }),
        }
    }
}

/// All three gates for one render profile.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileForkPaths {
    /// Render profile name.
    pub profile: String,
    /// `cairo_fork_workers` value of the profile.
    pub fork_workers: u32,
    /// `cairo_static_layers` value of the profile.
    pub static_layers_requested: bool,
    /// Fork-per-play gate.
    pub fork: GateEvaluation,
    /// Static layer plan gate.
    pub static_layers: GateEvaluation,
    /// Packed interpolation gate.
    pub bulk: GateEvaluation,
}

/// Fork fast-path verdicts of one scene.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneForkPaths {
    /// Qualified scene class name.
    pub scene: String,
    /// One entry per analyzed Cairo render profile.
    pub profiles: Vec<ProfileForkPaths>,
}

/// Fork fast-path verdicts of the whole analyzed project.
///
/// Empty unless the loaded knowledge profile declares `fork_capabilities`
/// (the DESIGN §7.3 inertness gate for `upstream_0_20`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForkPathFacts {
    /// Per-scene verdicts, in lifecycle scene order.
    pub scenes: Vec<SceneForkPaths>,
}

/// Renders the text after `play #N: ` for one outcome — shared between
/// the cost report and `MLP225` diagnostics so causal phrasing exists
/// exactly once. `locate` resolves a site to `path:line:column`.
#[must_use]
pub fn describe_outcome(
    gate: GateKind,
    outcome: &PlayGateOutcome,
    locate: &dyn Fn(AllocationSite) -> String,
) -> String {
    match &outcome.verdict {
        GateVerdict::Clear => gate.clear_phrase().to_owned(),
        GateVerdict::Blocked(causes) => {
            let rendered: Vec<String> = causes
                .iter()
                .map(|cause| {
                    let at = cause
                        .site
                        .map(|site| format!(" at {}", locate(site)))
                        .unwrap_or_default();
                    format!(
                        "{detail}{at} (blocker {name})",
                        detail = cause.detail,
                        name = cause.blocker.as_str(),
                    )
                })
                .collect();
            format!(
                "{loss} because {causes}",
                loss = gate.loss_phrase(),
                causes = rendered.join("; "),
            )
        }
        GateVerdict::MonotonicallyDisabled {
            first_ordinal,
            first_site,
            first_blocker,
        } => format!(
            "no static blocker found, but play #{first_ordinal} ({location}) fell back \
             to the serial path ({blocker}) and a rendered serial play opens the parent \
             encoder: fork disabling is renderer-wide and monotonic \
             (blocker parent_encoder_opened)",
            location = locate(*first_site),
            blocker = first_blocker.as_str(),
        ),
        GateVerdict::NotApplicable { reason } => format!("not applicable ({reason})"),
        GateVerdict::Unaudited { reason } => format!("not statically auditable ({reason})"),
    }
}

/// Evaluates every fork fast-path gate over the lifecycle facts.
///
/// Returns the empty default when no knowledge profile is loaded or the
/// profile declares no `fork_capabilities` — fork-path interpretation is
/// local-fork-overlay only and must stay inert under `upstream_0_20`.
#[must_use]
pub fn evaluate(
    lifecycle: &LifecycleFacts,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    profiles: &[RenderProfile],
) -> ForkPathFacts {
    let Some(knowledge) = knowledge else {
        return ForkPathFacts::default();
    };
    let Some(capabilities) = knowledge.fork_capabilities() else {
        return ForkPathFacts::default();
    };
    let call_index = index_calls(calls);
    // `Scene.remove_foreground_*` and `Scene.clear` leave no ordered event
    // in the trace, so any such call anywhere makes foreground liveness
    // unprovable (a claim would need ordering we do not have).
    let foreground_removal_possible = calls.calls.iter().any(|call| {
        call.candidates.iter().any(|candidate| {
            resolve_candidate(knowledge, candidate).is_some_and(|(_, entry)| {
                matches!(
                    entry
                        .effects
                        .as_ref()
                        .and_then(|effects| effects.scene_membership),
                    Some(SceneMembershipEffect::RemoveForeground | SceneMembershipEffect::Clear)
                )
            })
        })
    });
    let mut scenes = Vec::new();
    for scene in &lifecycle.scenes {
        let states = play_states(
            scene,
            calls,
            knowledge,
            &call_index,
            foreground_removal_possible,
        );
        let mut profile_reports = Vec::new();
        for profile in profiles {
            if profile.renderer != Renderer::Cairo {
                // Every gate here is a Cairo render path.
                continue;
            }
            let context = EvalContext {
                scene,
                states: &states,
                calls,
                call_index: &call_index,
                profile,
            };
            profile_reports.push(ProfileForkPaths {
                profile: profile.name.clone(),
                fork_workers: profile.cairo_fork_workers,
                static_layers_requested: profile.cairo_static_layers,
                fork: evaluate_fork_gate(capabilities.cairo_fork_gate.as_ref(), &context),
                static_layers: evaluate_static_gate(
                    capabilities.cairo_static_layers.as_ref(),
                    &context,
                ),
                bulk: evaluate_bulk_gate(capabilities.cairo_bulk_interpolation.as_ref(), &context),
            });
        }
        if !profile_reports.is_empty() {
            scenes.push(SceneForkPaths {
                scene: scene.qualified_name.clone(),
                profiles: profile_reports,
            });
        }
    }
    ForkPathFacts { scenes }
}

// ---------------------------------------------------------------------------
// Pre-play state (the OutputState-style walk over the event trace).
// ---------------------------------------------------------------------------

/// One live updater registration in the walk state.
#[derive(Debug, Clone)]
struct UpdaterEntry {
    /// Registration site (the cause span).
    site: AllocationSite,
    /// Host object; `None` for scene-level updaters.
    host: Option<ObjectId>,
    /// Registered callback identity (removal matching).
    callback: CallbackRef,
    /// `false` means may-live (branch-dependent registration or a
    /// possible removal) — never claimed as a blocker.
    proven: bool,
}

/// Registration liveness observed at one play's begin.
#[derive(Debug, Clone, Default)]
struct PlayState {
    /// Scene-level updater registrations live so far.
    scene_updaters: Vec<UpdaterEntry>,
    /// Foreground registrations live so far: `(site, proven)`.
    foreground: Vec<(AllocationSite, bool)>,
    /// Mobject updater registrations live so far.
    mobject_updaters: Vec<UpdaterEntry>,
}

/// Walks the scene's event trace once and snapshots the registration
/// state at every `BeginPlay`, keyed by play group.
///
/// Removal events (`Mutate` with [`MutationKind::Updaters`]) are
/// cross-referenced with the interpreter's [`UpdaterRemoval`] facts at
/// the same site: an identity-matched, all-paths removal deletes exactly
/// the registrations with that callback (the updater is provably gone —
/// later plays carry no cause from it), while anything weaker only
/// degrades entries to may-live. A claim survives only when the trace
/// proves it.
fn play_states(
    scene: &SceneLifecycle,
    calls: &QualifiedCallFacts,
    knowledge: &KnowledgeProfile,
    call_index: &BTreeMap<(FileId, u32, u32), usize>,
    foreground_removal_possible: bool,
) -> BTreeMap<u64, PlayState> {
    let mut current = PlayState::default();
    let mut states = BTreeMap::new();
    for event in &scene.events {
        let proven = event.certainty == Presence::Present;
        match &event.event {
            Event::RegisterUpdater(register) if register.target == scene.scene_id => {
                current.scene_updaters.push(UpdaterEntry {
                    site: event.site,
                    host: None,
                    callback: register.updater.callback.clone(),
                    proven,
                });
            }
            Event::RegisterUpdater(register) => {
                current.mobject_updaters.push(UpdaterEntry {
                    site: event.site,
                    host: Some(register.target.clone()),
                    callback: register.updater.callback.clone(),
                    proven,
                });
            }
            Event::Mutate(mutate) if mutate.kind == MutationKind::Updaters => {
                let scene_level = mutate.target == scene.scene_id;
                let entries = if scene_level {
                    &mut current.scene_updaters
                } else {
                    &mut current.mobject_updaters
                };
                let removed = proven
                    .then(|| removed_callback(scene, event.site))
                    .flatten();
                if let Some(callback) = removed {
                    entries.retain(|entry| entry.callback != callback);
                } else {
                    for entry in entries.iter_mut() {
                        if scene_level || entry.host.as_ref() == Some(&mutate.target) {
                            entry.proven = false;
                        }
                    }
                }
            }
            Event::SceneAdd(_) => {
                if let Some(all_foreground) =
                    foreground_add(calls, knowledge, call_index, event.site)
                {
                    current.foreground.push((
                        event.site,
                        proven && all_foreground && !foreground_removal_possible,
                    ));
                }
            }
            Event::BeginPlay(begin) => {
                states.insert(begin.play_group, current.clone());
            }
            _ => {}
        }
    }
    states
}

/// The callback a `remove_updater` at `site` provably removes: the
/// interpreter's removal fact must be identity-matched (`matched: Yes`)
/// and on every path. `None` means nothing is provably removed (the
/// caller degrades instead of deleting).
fn removed_callback(scene: &SceneLifecycle, site: AllocationSite) -> Option<CallbackRef> {
    let removal = scene
        .updater_removals
        .iter()
        .find(|removal| removal.site == site)?;
    (removal.matched == Truth::Yes && removal.certainty == Presence::Present)
        .then(|| removal.callback.clone())
}

/// Classifies a `SceneAdd` event site as a foreground registration:
/// `Some(true)` when every resolved candidate of the call at that site is
/// a curated `AddForeground` API, `Some(false)` when only some are (a
/// may-foreground add), `None` when the site is a plain add.
fn foreground_add(
    calls: &QualifiedCallFacts,
    knowledge: &KnowledgeProfile,
    call_index: &BTreeMap<(FileId, u32, u32), usize>,
    site: AllocationSite,
) -> Option<bool> {
    let index = *call_index.get(&(site.file, site.start, site.end))?;
    let call = &calls.calls[index];
    let mut resolved = 0_usize;
    let mut foreground = 0_usize;
    for candidate in &call.candidates {
        let Some((_, entry)) = resolve_candidate(knowledge, candidate) else {
            continue;
        };
        resolved += 1;
        if entry
            .effects
            .as_ref()
            .and_then(|effects| effects.scene_membership)
            == Some(SceneMembershipEffect::AddForeground)
        {
            foreground += 1;
        }
    }
    if foreground == 0 {
        return None;
    }
    Some(foreground == resolved)
}

// ---------------------------------------------------------------------------
// Per-gate evaluation.
// ---------------------------------------------------------------------------

/// Borrowed inputs shared by the per-gate evaluators.
struct EvalContext<'a> {
    scene: &'a SceneLifecycle,
    states: &'a BTreeMap<u64, PlayState>,
    calls: &'a QualifiedCallFacts,
    call_index: &'a BTreeMap<(FileId, u32, u32), usize>,
    profile: &'a RenderProfile,
}

fn evaluate_fork_gate(gate: Option<&CairoForkGate>, context: &EvalContext<'_>) -> GateEvaluation {
    let Some(gate) = gate else {
        return GateEvaluation::NotDeclared;
    };
    if gate.linux_only == Some(true) && context.profile.platform != Platform::Linux {
        return GateEvaluation::Unrequested {
            reason: format!(
                "the fork pipeline is Linux-only; profile platform is {}",
                context.profile.platform
            ),
        };
    }
    if context.profile.cairo_fork_workers < gate.min_workers {
        return GateEvaluation::Unrequested {
            reason: format!(
                "{key} {workers} is below the enabling minimum of {min}",
                key = gate.config_key,
                workers = context.profile.cairo_fork_workers,
                min = gate.min_workers,
            ),
        };
    }
    let mut outcomes: Vec<(PlayGateOutcome, Presence)> = Vec::new();
    for (index, play) in context.scene.plays.iter().enumerate() {
        let verdict = match audited_state(context, play) {
            Err(reason) => GateVerdict::Unaudited { reason },
            Ok(state) => {
                let causes = collect_causes(
                    context,
                    play,
                    state,
                    Some(&gate.animation_allowlist),
                    Some(&gate.composition_allowlist),
                    false,
                );
                verdict_from_causes(causes, &gate.blockers)
            }
        };
        outcomes.push((
            PlayGateOutcome {
                ordinal: index + 1,
                site: play.site,
                verdict,
            },
            play.certainty,
        ));
    }
    if gate.monotonic_disable == Some(true) {
        apply_monotonic_disable(&mut outcomes);
    }
    GateEvaluation::Plays(outcomes.into_iter().map(|(outcome, _)| outcome).collect())
}

fn evaluate_static_gate(
    gate: Option<&CairoStaticLayers>,
    context: &EvalContext<'_>,
) -> GateEvaluation {
    let Some(gate) = gate else {
        return GateEvaluation::NotDeclared;
    };
    if !context.profile.cairo_static_layers {
        return GateEvaluation::Unrequested {
            reason: format!("{key} is off", key = gate.config_key),
        };
    }
    let mut outcomes = Vec::new();
    for (index, play) in context.scene.plays.iter().enumerate() {
        let verdict = match audited_state(context, play) {
            Err(reason) => GateVerdict::Unaudited { reason },
            Ok(state) => {
                if frozen_static_wait(play) {
                    GateVerdict::NotApplicable {
                        reason: "a frozen static wait already renders one frame; a layer \
                                 plan would add nothing"
                            .to_owned(),
                    }
                } else if let Some(reason) = below_frame_floor(context, play, gate.min_play_frames)
                {
                    GateVerdict::NotApplicable { reason }
                } else {
                    // The overlay curates no animation allowlist for the
                    // layer plan, so animation-type causes are never
                    // claimed here (absent facts stay silent).
                    let causes = collect_causes(context, play, state, None, None, false);
                    verdict_from_causes(causes, &gate.blockers)
                }
            }
        };
        outcomes.push(PlayGateOutcome {
            ordinal: index + 1,
            site: play.site,
            verdict,
        });
    }
    GateEvaluation::Plays(outcomes)
}

fn evaluate_bulk_gate(
    gate: Option<&CairoBulkInterpolation>,
    context: &EvalContext<'_>,
) -> GateEvaluation {
    let Some(gate) = gate else {
        return GateEvaluation::NotDeclared;
    };
    let mut outcomes = Vec::new();
    for (index, play) in context.scene.plays.iter().enumerate() {
        let verdict = match audited_state(context, play) {
            Err(reason) => GateVerdict::Unaudited { reason },
            Ok(state) => {
                if play.kind == PlayKind::Wait {
                    GateVerdict::NotApplicable {
                        reason: "a wait play has no per-member interpolation".to_owned(),
                    }
                } else if let Some(reason) = below_frame_floor(context, play, gate.min_frames) {
                    GateVerdict::NotApplicable { reason }
                } else {
                    let causes = collect_causes(
                        context,
                        play,
                        state,
                        Some(&gate.animation_allowlist),
                        None,
                        true,
                    );
                    verdict_from_causes(causes, &gate.blockers)
                }
            }
        };
        outcomes.push(PlayGateOutcome {
            ordinal: index + 1,
            site: play.site,
            verdict,
        });
    }
    GateEvaluation::Plays(outcomes)
}

/// The monotonic renderer-wide disable (fork gate only): once a play
/// provably renders serially, its written frames open the parent encoder
/// and every later otherwise-clear play cannot fork. A branch-dependent
/// serial play cannot *prove* the poisoning, so later clear plays degrade
/// to unaudited instead — never a fabricated loss and never an assumed
/// per-play independence.
fn apply_monotonic_disable(outcomes: &mut [(PlayGateOutcome, Presence)]) {
    let mut first_serial: Option<(usize, AllocationSite, ForkBlocker)> = None;
    let mut maybe_serial = false;
    for (outcome, certainty) in outcomes.iter_mut() {
        if let Some((first_ordinal, first_site, first_blocker)) = first_serial {
            if matches!(outcome.verdict, GateVerdict::Clear) {
                outcome.verdict = GateVerdict::MonotonicallyDisabled {
                    first_ordinal,
                    first_site,
                    first_blocker,
                };
                continue;
            }
        } else if maybe_serial && matches!(outcome.verdict, GateVerdict::Clear) {
            outcome.verdict = GateVerdict::Unaudited {
                reason: "an earlier play may have rendered serially and opened the \
                         parent encoder (fork disabling is renderer-wide and monotonic)"
                    .to_owned(),
            };
            continue;
        }
        match &outcome.verdict {
            GateVerdict::Blocked(causes) => {
                let blocker = causes.first().map(|cause| cause.blocker);
                if *certainty == Presence::Present {
                    if first_serial.is_none() {
                        if let Some(blocker) = blocker {
                            first_serial = Some((outcome.ordinal, outcome.site, blocker));
                        }
                    }
                } else {
                    maybe_serial = true;
                }
            }
            GateVerdict::Unaudited { .. } => {
                maybe_serial = true;
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Cause collection.
// ---------------------------------------------------------------------------

/// Proven causes plus reasons the audit could not complete.
#[derive(Debug, Default)]
struct CauseSet {
    proven: Vec<BlockCause>,
    unproven: Vec<(ForkBlocker, String)>,
}

impl CauseSet {
    fn prove(&mut self, blocker: ForkBlocker, site: Option<AllocationSite>, detail: &str) {
        let cause = BlockCause {
            blocker,
            site,
            detail: detail.to_owned(),
        };
        if !self.proven.contains(&cause) {
            self.proven.push(cause);
        }
    }

    fn cannot_audit(&mut self, blocker: ForkBlocker, reason: &str) {
        if !self
            .unproven
            .iter()
            .any(|(existing, text)| *existing == blocker && text == reason)
        {
            self.unproven.push((blocker, reason.to_owned()));
        }
    }
}

/// The pre-play registration state, or why the play cannot be audited.
fn audited_state<'a>(
    context: &'a EvalContext<'_>,
    play: &PlayFact,
) -> Result<&'a PlayState, String> {
    if context
        .scene
        .summary_derived_plays
        .contains(&play.play_group)
    {
        return Err(
            "the enclosing helper was not inlined; play effects are summary-derived".to_owned(),
        );
    }
    context.states.get(&play.play_group.0).map_or_else(
        || Err("no traced begin state exists for this play".to_owned()),
        Ok,
    )
}

/// Collects every statically checkable cause for one play. `allowlist`
/// gates animation-type checking (a gate without a curated allowlist
/// never claims `unsupported_animation_type`); `updater_family` enables
/// the bulk gate's updater-bearing-family check.
fn collect_causes(
    context: &EvalContext<'_>,
    play: &PlayFact,
    state: &PlayState,
    allowlist: Option<&[String]>,
    composition_allowlist: Option<&[String]>,
    updater_family: bool,
) -> CauseSet {
    let mut causes = CauseSet::default();

    // Renderer-wide configuration causes.
    if context.profile.transparent {
        causes.prove(
            ForkBlocker::TransparentOutput,
            None,
            "the profile renders transparent output",
        );
    }
    if context.profile.video_encoder != "libx264" {
        let detail = format!(
            "the profile's video encoder is {encoder}, not libx264",
            encoder = context.profile.video_encoder
        );
        causes.prove(ForkBlocker::NonLibx264Encoder, None, &detail);
    }

    // Scene-level registrations live at this play.
    for entry in &state.scene_updaters {
        if entry.proven {
            causes.prove(
                ForkBlocker::SceneUpdaters,
                Some(entry.site),
                "a Scene updater is registered",
            );
        } else {
            causes.cannot_audit(
                ForkBlocker::SceneUpdaters,
                "a Scene updater may be registered on some path",
            );
        }
    }
    for (site, proven) in &state.foreground {
        if *proven {
            causes.prove(
                ForkBlocker::ForegroundMobjects,
                Some(*site),
                "foreground mobjects are registered",
            );
        } else {
            causes.cannot_audit(
                ForkBlocker::ForegroundMobjects,
                "a foreground registration is branch-dependent or may be removed",
            );
        }
    }

    // Play-level facts.
    if play.has_stop_condition {
        causes.prove(
            ForkBlocker::StopCondition,
            None,
            "this play passes a stop_condition callback",
        );
    }
    match play.always_update_mobjects {
        Truth::Yes => causes.prove(
            ForkBlocker::AlwaysUpdateMobjects,
            None,
            "always_update_mobjects is enabled for this play",
        ),
        Truth::Maybe => causes.cannot_audit(
            ForkBlocker::AlwaysUpdateMobjects,
            "always_update_mobjects is not statically known",
        ),
        Truth::No => {}
    }

    if let Some(allowlist) = allowlist {
        collect_animation_type_causes(play, allowlist, composition_allowlist, &mut causes);
    }
    collect_rate_func_causes(context, play, &mut causes);

    if updater_family {
        collect_updater_family_causes(context, play, state, &mut causes);
    }

    causes
}

/// Exact-type audit against the gate's curated allowlist(s). A claim is
/// made only when every kind candidate of an animation is known and
/// outside both lists; anything unresolved stays an audit gap.
fn collect_animation_type_causes(
    play: &PlayFact,
    allowlist: &[String],
    composition_allowlist: Option<&[String]>,
    causes: &mut CauseSet,
) {
    let in_list = |kinds: &[String], kind: &str| kinds.iter().any(|entry| entry == kind);
    if play.kind == PlayKind::Wait {
        // A wait plays the Wait animation.
        if !in_list(allowlist, "manim.animation.animation.Wait") {
            causes.prove(
                ForkBlocker::UnsupportedAnimationType,
                None,
                "the Wait animation is outside the audited allowlist",
            );
        }
        return;
    }
    if play.star_args {
        causes.cannot_audit(
            ForkBlocker::UnsupportedAnimationType,
            "a *args splat hides animation arguments",
        );
    }
    for animation in &play.animations {
        let Some(state) = &animation.state else {
            causes.cannot_audit(
                ForkBlocker::UnsupportedAnimationType,
                "an animation type is not statically resolved",
            );
            continue;
        };
        let KindSet::Known(kinds) = &state.kind else {
            causes.cannot_audit(
                ForkBlocker::UnsupportedAnimationType,
                "an animation type is not statically resolved",
            );
            continue;
        };
        if kinds.is_empty() {
            causes.cannot_audit(
                ForkBlocker::UnsupportedAnimationType,
                "an animation type is not statically resolved",
            );
            continue;
        }
        let mut all_allowed = true;
        let mut all_outside = true;
        let mut any_composition = false;
        for kind in kinds {
            let allowed = in_list(allowlist, kind);
            let composition =
                composition_allowlist.is_some_and(|compositions| in_list(compositions, kind));
            all_allowed &= allowed;
            all_outside &= !allowed && !composition;
            any_composition |= composition;
        }
        if all_allowed {
            continue;
        }
        if all_outside {
            let names: Vec<&str> = kinds
                .iter()
                .map(|kind| kind.rsplit('.').next().unwrap_or(kind))
                .collect();
            let detail = format!(
                "the animation type {names} is outside the audited allowlist",
                names = names.join(" / "),
            );
            causes.prove(
                ForkBlocker::UnsupportedAnimationType,
                Some(animation.site),
                &detail,
            );
        } else if any_composition {
            causes.cannot_audit(
                ForkBlocker::UnsupportedAnimationType,
                "a composition container's children are audited recursively at render \
                 time and are not statically verified here",
            );
        } else {
            causes.cannot_audit(
                ForkBlocker::UnsupportedAnimationType,
                "an animation type is not statically resolved",
            );
        }
    }
}

/// `rate_func` keyword audit: the fork accepts rate functions by
/// identity, so a lambda argument is a proven mismatch; any other
/// explicit `rate_func` cannot be identity-checked statically and stays
/// an audit gap.
fn collect_rate_func_causes(context: &EvalContext<'_>, play: &PlayFact, causes: &mut CauseSet) {
    let mut sites = vec![play.site];
    sites.extend(play.animations.iter().map(|animation| animation.site));
    for site in sites {
        let Some(call) = call_at(context, site) else {
            continue;
        };
        let Some(argument) = call.keyword("rate_func") else {
            continue;
        };
        if matches!(argument.shape, ArgShape::Lambda) {
            causes.prove(
                ForkBlocker::CustomRateFunc,
                Some(AllocationSite::new(call.file, argument.range)),
                "a lambda rate_func replaces the identity-audited default",
            );
        } else {
            causes.cannot_audit(
                ForkBlocker::CustomRateFunc,
                "a rate_func argument's identity is not statically verified",
            );
        }
    }
}

/// Bulk gate: any updater anywhere in the scene's mobject families keeps
/// the play on canonical interpolation. Proven only when the registration
/// is certain, never degraded, and the host is provably in the scene
/// family at this play.
fn collect_updater_family_causes(
    context: &EvalContext<'_>,
    play: &PlayFact,
    state: &PlayState,
    causes: &mut CauseSet,
) {
    for entry in &state.mobject_updaters {
        let membership = entry.host.as_ref().and_then(|host| {
            context
                .scene
                .membership_at(host, play.site.file, play.site.start)
                .map(|(_, family)| family)
        });
        match (entry.proven, membership) {
            (true, Some(Presence::Present)) => {
                causes.prove(
                    ForkBlocker::UpdaterBearingFamily,
                    Some(entry.site),
                    "an updater-bearing mobject is in the scene family (updater \
                     registered here)",
                );
            }
            (_, Some(Presence::Absent)) => {
                // Provably outside the scene family: no blocker from this
                // registration.
            }
            _ => {
                causes.cannot_audit(
                    ForkBlocker::UpdaterBearingFamily,
                    "a mobject updater registration or its scene-family membership \
                     is not statically proven",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared small helpers.
// ---------------------------------------------------------------------------

/// The verdict implied by a cause set under one gate's curated blocker
/// list: causes outside the list never count (the overlay is the only
/// authority on what gates each fast path).
fn verdict_from_causes(causes: CauseSet, blockers: &[ForkBlocker]) -> GateVerdict {
    let proven: Vec<BlockCause> = causes
        .proven
        .into_iter()
        .filter(|cause| blockers.contains(&cause.blocker))
        .collect();
    if !proven.is_empty() {
        return GateVerdict::Blocked(proven);
    }
    let unproven: Vec<String> = causes
        .unproven
        .into_iter()
        .filter(|(blocker, _)| blockers.contains(blocker))
        .map(|(_, reason)| reason)
        .collect();
    if unproven.is_empty() {
        GateVerdict::Clear
    } else {
        GateVerdict::Unaudited {
            reason: unproven.join("; "),
        }
    }
}

/// Whether the play is a provably frozen static wait (`Wait` that renders
/// one frozen frame: static verdict proven and `frozen_frame` not forced
/// off).
fn frozen_static_wait(play: &PlayFact) -> bool {
    play.kind == PlayKind::Wait
        && play.dynamic_wait == Truth::No
        && play.frozen_frame != Some(false)
}

/// `Some(reason)` when the play provably spans fewer frames than `floor`
/// under this profile's frame rate; `None` when the floor is reached or
/// the duration is unknown (an unknown never proves either side).
fn below_frame_floor(context: &EvalContext<'_>, play: &PlayFact, floor: u32) -> Option<String> {
    let frames = frames_across_profiles(&play.duration, std::slice::from_ref(context.profile));
    let upper = frames.upper_bound()?;
    if upper < f64::from(floor) {
        Some(format!(
            "the play spans fewer than {floor} frames; the fast path cannot amortize"
        ))
    } else {
        None
    }
}

/// The qualified call whose range is exactly `site`, if any.
fn call_at<'a>(context: &EvalContext<'a>, site: AllocationSite) -> Option<&'a QualifiedCall> {
    context
        .call_index
        .get(&(site.file, site.start, site.end))
        .map(|index| &context.calls.calls[*index])
}

/// Indexes call facts by `(file, start, end)` for site lookups.
fn index_calls(calls: &QualifiedCallFacts) -> BTreeMap<(FileId, u32, u32), usize> {
    calls
        .calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            (
                (
                    call.file,
                    u32::from(call.call_range.start()),
                    u32::from(call.call_range.end()),
                ),
                index,
            )
        })
        .collect()
}
