//! Updater signature rule: `MLC105`.
//!
//! Manim's calling conventions (DESIGN §3.3):
//!
//! - `Mobject.add_updater(callback)` inspects the callback signature; when
//!   any parameter is *named* `dt` the callback is called positionally as
//!   `callback(mobject, dt)`, otherwise as `callback(mobject)`.
//! - `Scene.add_updater(callback)` always calls `callback(dt)` with one
//!   positional argument.
//!
//! The rule simulates Python positional binding over the declared
//! signature; it fires only when the simulated call provably cannot bind.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::index::{CallArgument, CallableSignature, ParamKind, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};

use super::support::{
    MOBJECT_ADD_UPDATER, SCENE_ADD_UPDATER, bound_receiver, build_diagnostic, candidates_value,
    conclusive_target,
};

/// Metadata for [`UpdaterCannotBind`].
pub const MLC105: RuleMetadata = RuleMetadata {
    id: "MLC105",
    summary: "Updater callback cannot bind to Manim's positional invocation",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// Which updater registration contract applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Contract {
    /// `Mobject.add_updater`: `(mobject, dt)` or `(mobject)` depending on
    /// whether a parameter is named `dt`.
    Mobject,
    /// `Scene.add_updater`: always `(dt)`.
    Scene,
}

/// Whether `signature` accepts `count` positional arguments and no
/// keywords, mirroring `inspect.Signature.bind`.
fn binds_positionally(signature: &CallableSignature, count: usize) -> bool {
    let mut remaining = count;
    for parameter in &signature.params {
        match parameter.kind {
            ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword => {
                if remaining > 0 {
                    remaining -= 1;
                } else if !parameter.has_default {
                    // A required parameter receives no value.
                    return false;
                }
            }
            ParamKind::VarArgs => {
                remaining = 0;
            }
            ParamKind::KeywordOnly => {
                if !parameter.has_default {
                    // No keywords are passed; a required keyword-only
                    // parameter can never be satisfied.
                    return false;
                }
            }
            ParamKind::KwArgs => {}
        }
    }
    // Leftover positional arguments have no slot.
    remaining == 0
}

/// The callback argument of a confirmed `add_updater` call.
fn callback_argument(call: &QualifiedCall, contract: Contract) -> Option<&CallArgument> {
    let keyword = match contract {
        Contract::Mobject => "update_function",
        Contract::Scene => "func",
    };
    call.positional(0).or_else(|| call.keyword(keyword))
}

/// Updater callbacks that cannot bind to Manim's actual invocation
/// (DESIGN §7.1 `MLC105`).
pub struct UpdaterCannotBind;

impl Rule for UpdaterCannotBind {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC105
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if !bound_receiver(call) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            let contract = match canonical.as_str() {
                MOBJECT_ADD_UPDATER => Contract::Mobject,
                SCENE_ADD_UPDATER => Contract::Scene,
                _ => continue,
            };
            let Some(callback) = callback_argument(call, contract) else {
                continue;
            };
            let Some(signature) = &callback.callable_signature else {
                // Unknown callable: silence, never a guess.
                continue;
            };
            // A leading `self` parameter means the reference may be an
            // unbound method; binding cannot be proven either way.
            if signature
                .params
                .first()
                .is_some_and(|parameter| parameter.name == "self")
            {
                continue;
            }
            let has_dt = signature
                .params
                .iter()
                .any(|parameter| parameter.name == "dt");
            let (count, invocation) = match contract {
                Contract::Mobject if has_dt => (2, "callback(mobject, dt)"),
                Contract::Mobject => (1, "callback(mobject)"),
                Contract::Scene => (1, "callback(dt)"),
            };
            if binds_positionally(signature, count) {
                continue;
            }
            let file = context.sources().file(call.file);
            diagnostics.push(cannot_bind_diagnostic(
                context, file, call, callback, signature, contract, has_dt, invocation, &canonical,
            ));
        }
        diagnostics
    }
}

/// Builds the `MLC105` diagnostic for one confirmed non-binding callback.
#[allow(clippy::too_many_arguments, reason = "plain diagnostic assembly")]
fn cannot_bind_diagnostic(
    context: &RuleContext<'_>,
    file: &crate::source::SourceFile,
    call: &QualifiedCall,
    callback: &CallArgument,
    signature: &CallableSignature,
    contract: Contract,
    has_dt: bool,
    invocation: &str,
    canonical: &str,
) -> Diagnostic {
    let parameters = signature
        .params
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let (message, explanation) = match contract {
        Contract::Mobject => (
            format!(
                "Adjust this updater's parameters (`{parameters}`): because \
                         {reason}, `Mobject.add_updater` will call it as \
                         `{invocation}` and that call cannot bind.",
                reason = if has_dt {
                    "a parameter is named `dt`"
                } else {
                    "no parameter is named `dt`"
                },
            ),
            "Manim inspects the callback signature for a parameter *named* \
                     `dt`: if present the callback is invoked positionally as \
                     `callback(mobject, dt)`, otherwise as `callback(mobject)`. A \
                     signature that cannot bind that exact positional call raises \
                     `TypeError` on the first rendered frame (DESIGN §3.3)."
                .to_owned(),
        ),
        Contract::Scene => (
            format!(
                "Adjust this Scene updater's parameters (`{parameters}`): \
                         `Scene.add_updater` always calls it as `{invocation}` with \
                         one positional argument, and that call cannot bind."
            ),
            "Scene-level updaters have a fixed contract: every frame the \
                     callback receives exactly one positional argument, the frame \
                     time delta `dt`. A signature that cannot bind one positional \
                     argument raises `TypeError` on the first rendered frame \
                     (DESIGN §3.3)."
                .to_owned(),
        ),
    };
    let mut evidence = BTreeMap::new();
    evidence.insert("resolved".to_owned(), Value::String(canonical.to_owned()));
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence.insert(
        "simulated_call".to_owned(),
        Value::String(invocation.to_owned()),
    );
    evidence.insert(
        "callback_parameters".to_owned(),
        Value::Array(
            signature
                .params
                .iter()
                .map(|parameter| Value::String(parameter.name.clone()))
                .collect(),
        ),
    );
    build_diagnostic(
        &MLC105,
        context,
        file,
        callback.range,
        message,
        explanation,
        evidence,
    )
}

// ---------------------------------------------------------------------------
// MLC121: timeline re-entry from a per-frame callback.
// ---------------------------------------------------------------------------

/// Metadata for [`TimelineReentry`].
pub const MLC121: RuleMetadata = RuleMetadata {
    id: "MLC121",
    summary: "Scene.play / wait / pause called from a per-frame callback",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// `self.play(...)` / `self.wait(...)` / `self.pause(...)` provably
/// reachable from a per-frame entry point (DESIGN §7.1 `MLC121`).
pub struct TimelineReentry;

impl Rule for TimelineReentry {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC121
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        use super::support::{SCENE_PAUSE, SCENE_PLAY, SCENE_WAIT};
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let cost = context.cost_facts();
        let mut diagnostics = Vec::new();
        for (call_index, call) in context.qualified_calls().calls.iter().enumerate() {
            if !bound_receiver(call) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != SCENE_PLAY && canonical != SCENE_WAIT && canonical != SCENE_PAUSE {
                continue;
            }
            let contexts = cost.hot_contexts_for(call_index);
            let Some(hot) = contexts.first() else {
                continue;
            };
            let method = canonical.rsplit('.').next().unwrap_or("play");
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(canonical.clone()));
            evidence.insert(
                "hot_entry".to_owned(),
                Value::String(hot.entry.label().to_owned()),
            );
            evidence.insert(
                "state_path".to_owned(),
                Value::Array(hot.state_path().into_iter().map(Value::String).collect()),
            );
            evidence.insert("cost".to_owned(), cost.evidence_for(call_index));
            diagnostics.push(build_diagnostic(
                &MLC121,
                context,
                file,
                call.call_range,
                format!(
                    "`Scene.{method}` is called from a per-frame callback \
                     ({entry}); the render loop cannot be re-entered while a frame \
                     is being produced. Move timeline control (`play` / `wait` / \
                     `pause`) into `construct` and let the callback only mutate \
                     mobject state.",
                    entry = hot.entry.label(),
                ),
                "Updaters, `always_redraw` factories, update-function animations, \
                 and stop conditions run inside the per-frame sample loop of an \
                 active play (DESIGN §3.2/§3.3). Calling `Scene.play` / `wait` / \
                 `pause` there re-enters the renderer's timeline machinery \
                 mid-frame and breaks rendering."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLC125: remove_updater with a never-matching callback identity.
// ---------------------------------------------------------------------------

/// Metadata for [`RemoveUpdaterIdentityMismatch`].
pub const MLC125: RuleMetadata = RuleMetadata {
    id: "MLC125",
    summary: "remove_updater callback identity matches no registered updater",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// `remove_updater(...)` whose argument identity definitely matches no
/// registered updater (DESIGN §7.1 `MLC125`).
pub struct RemoveUpdaterIdentityMismatch;

impl Rule for RemoveUpdaterIdentityMismatch {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC125
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        use crate::semantic::interpreter::UpdaterHost;
        use crate::semantic::state::CallbackRef;
        use crate::semantic::values::{AllocationSite, Truth};
        use std::collections::BTreeSet;

        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for removal in &scene.updater_removals {
                if removal.matched != Truth::No || seen.contains(&removal.site) {
                    continue;
                }
                let removal_range = super::support::site_range(&removal.site);
                // An inline lambda literal is a fresh function object: it can
                // never be identical to anything registered earlier.
                let inline_lambda = matches!(
                    &removal.callback,
                    CallbackRef::Lambda(site)
                        if site.file == removal.site.file
                            && site.start >= removal.site.start
                            && site.end <= removal.site.end
                );
                if !inline_lambda && !named_mismatch_is_reliable(scene, removal) {
                    continue;
                }
                seen.insert(removal.site);
                let file = context.sources().file(removal.site.file);
                let text = file.slice(removal_range);
                let mut evidence = BTreeMap::new();
                evidence.insert(
                    "identity".to_owned(),
                    Value::String(
                        if inline_lambda {
                            "fresh-lambda"
                        } else {
                            "unmatched-reference"
                        }
                        .to_owned(),
                    ),
                );
                evidence.insert(
                    "scene".to_owned(),
                    Value::String(scene.qualified_name.clone()),
                );
                let host = match &removal.host {
                    UpdaterHost::Scene => "the scene",
                    UpdaterHost::Mobject(_) => "this mobject",
                };
                diagnostics.push(build_diagnostic(
                    &MLC125,
                    context,
                    file,
                    removal_range,
                    format!(
                        "`{text}` passes a callback whose identity matches no \
                         updater registered on {host}; Manim removes updaters by \
                         function identity, so this call removes nothing. Store the \
                         callback in a variable at registration time and pass the \
                         same reference here."
                    ),
                    "`remove_updater` compares the argument to the registered \
                     callbacks by object identity (mobject.py \
                     `Mobject.remove_updater`). A lambda or different function \
                     object — even with identical code — is a distinct identity and \
                     silently removes nothing (DESIGN §3.3)."
                        .to_owned(),
                    evidence,
                ));
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLC112: default wait freezes while a frame-varying updater is active.
// ---------------------------------------------------------------------------

/// Metadata for [`FrozenWaitFrameVaryingUpdater`].
pub const MLC112: RuleMetadata = RuleMetadata {
    id: "MLC112",
    summary: "Default wait() freezes while a frame-varying one-argument updater is active",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// A provably static default `wait()` while a one-argument updater that
/// provably reads frame-varying state is active on a scene-family member:
/// the updater's visual change never renders (DESIGN §7.1 `MLC112`).
///
/// Every gate is a definite fact (DESIGN §15): the body dataflow must be
/// `reads_frame_varying == Yes` and `uses_dt == No`, the wait must be
/// `dynamic_wait == No` with no literal `frozen_frame`, the registration
/// `Present`-certain, and the host provably in the scene family with the
/// updater still attached at the wait. `Maybe` anywhere stays silent.
pub struct FrozenWaitFrameVaryingUpdater;

impl Rule for FrozenWaitFrameVaryingUpdater {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC112
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        use crate::semantic::interpreter::{PlayKind, UpdaterHost};
        use crate::semantic::values::{AllocationSite, Presence, Truth};
        use std::collections::BTreeSet;

        let mut seen: BTreeSet<AllocationSite> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for play in &scene.plays {
                if play.kind != PlayKind::Wait
                    || play.dynamic_wait != Truth::No
                    || play.frozen_frame.is_some()
                    || seen.contains(&play.site)
                {
                    continue;
                }
                for registration in &scene.updaters {
                    let UpdaterHost::Mobject(object) = &registration.host else {
                        continue;
                    };
                    if registration.certainty != Presence::Present
                        || registration.body.reads_frame_varying != Truth::Yes
                        || registration.body.uses_dt != Truth::No
                    {
                        continue;
                    }
                    // A removal that may match this host makes "still
                    // active at the wait" unprovable: silence.
                    let maybe_removed = scene.updater_removals.iter().any(|removal| {
                        removal.matched != Truth::No
                            && matches!(
                                &removal.host,
                                UpdaterHost::Mobject(host) if host.definitely_same(object)
                            )
                    });
                    if maybe_removed {
                        continue;
                    }
                    // The updater must still be attached and its host in
                    // the scene family at the wait.
                    let Some(snapshot) = scene.state_at(play.site.file, play.site.start) else {
                        continue;
                    };
                    let Some(state) = snapshot.heap.object(object) else {
                        continue;
                    };
                    if state.family_membership != Presence::Present
                        || !state.updaters.contains(&registration.fact)
                    {
                        continue;
                    }
                    seen.insert(play.site);
                    diagnostics.push(frozen_wait_diagnostic(context, scene, play, registration));
                    break;
                }
            }
        }
        diagnostics
    }
}

/// Assembles the `MLC112` diagnostic for one confirmed static wait with
/// its frame-varying registration, including the unsafe
/// `frozen_frame=False` insertion when the call text allows it.
fn frozen_wait_diagnostic(
    context: &RuleContext<'_>,
    scene: &crate::semantic::interpreter::SceneLifecycle,
    play: &crate::semantic::interpreter::PlayFact,
    registration: &crate::semantic::interpreter::UpdaterRegistration,
) -> Diagnostic {
    use crate::diagnostic::{Fix, FixApplicability, RelatedLocation, TextEdit};
    use rustpython_parser::text_size::TextRange;

    let file = context.sources().file(play.site.file);
    let registration_file = context.sources().file(registration.site.file);
    let registration_span =
        registration_file.span_of_range(super::support::site_range(&registration.site));
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    evidence.insert("dynamic_wait".to_owned(), Value::String("no".to_owned()));
    evidence.insert(
        "frozen_frame".to_owned(),
        Value::String("default".to_owned()),
    );
    evidence.insert(
        "reads_frame_varying".to_owned(),
        Value::String("yes".to_owned()),
    );
    evidence.insert("uses_dt".to_owned(), Value::String("no".to_owned()));
    evidence.insert(
        "registration".to_owned(),
        Value::String(format!(
            "{}:{}",
            registration_file.relative_path(),
            registration_span.start.line
        )),
    );
    let mut diagnostic = build_diagnostic(
        &MLC112,
        context,
        file,
        super::support::site_range(&play.site),
        format!(
            "This `wait()` renders a single frozen frame: nothing makes it \
             dynamic, and the updater registered at line {line} reads \
             frame-varying state without a `dt` parameter, so its visual change \
             never renders during the wait. Pass `frozen_frame=False`, or \
             declare a `dt` parameter on the updater.",
            line = registration_span.start.line,
        ),
        "A default `Scene.wait()` re-renders every frame only when a scene \
         updater, `stop_condition`, `always_update_mobjects`, or a *time-based* \
         family updater (one whose signature names a `dt` parameter) exists. A \
         one-argument updater never makes a wait dynamic — even one that reads \
         a `ValueTracker`, random source, or clock every frame — so the wait \
         freezes and the updater's changes are invisible (DESIGN §3.3)."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: registration_file.relative_path().to_owned(),
        span: registration_span,
        message: "frame-varying one-argument updater registered here".to_owned(),
    });
    // Unsafe fix: force the wait dynamic. Inserting the keyword needs a
    // plain `...(...)` call text.
    let call_text = file.slice(super::support::site_range(&play.site));
    if call_text.ends_with(')') && play.site.end > play.site.start {
        let inner = &call_text[..call_text.len() - 1];
        let replacement = if inner.trim_end().ends_with('(') {
            "frozen_frame=False"
        } else {
            ", frozen_frame=False"
        };
        let insertion = TextRange::new((play.site.end - 1).into(), (play.site.end - 1).into());
        diagnostic.fix = Some(Fix {
            applicability: FixApplicability::Unsafe,
            message: "Pass `frozen_frame=False` so the wait renders dynamically".to_owned(),
            edits: vec![TextEdit {
                path: file.relative_path().to_owned(),
                span: file.span_of_range(insertion),
                replacement: replacement.to_owned(),
            }],
        });
    }
    diagnostic
}

// ---------------------------------------------------------------------------
// MLC118: a normal animation suspends an active overlapping updater.
// ---------------------------------------------------------------------------

/// Metadata for [`UpdaterSuspendResumeDivergence`].
pub const MLC118: RuleMetadata = RuleMetadata {
    id: "MLC118",
    summary: "Normal animation suspends an active updater whose resumed write overlaps its result",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["lifecycle"],
    supersedes: &[],
};

/// An animation that definitely suspends a live target with a definitely
/// active updater whose unconditional, fully-classified write overlaps the
/// animation's write channels (DESIGN §7.1 `MLC118`).
pub struct UpdaterSuspendResumeDivergence;

impl Rule for UpdaterSuspendResumeDivergence {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC118
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        use crate::semantic::state::{SuspendBehavior, WriteChannel};
        use crate::semantic::values::{AllocationSite, Presence, Truth};

        let mut seen: BTreeSet<(AllocationSite, AllocationSite)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for play in &scene.plays {
                if play.certainty != Presence::Present {
                    continue;
                }
                let Some(snapshot) = scene.state_at(play.site.file, play.site.start) else {
                    continue;
                };
                for animation in &play.animations {
                    let Some(animation_state) = animation.state.as_ref() else {
                        continue;
                    };
                    if animation.channels_known != Truth::Yes
                        || animation_state.suspend != SuspendBehavior::SuspendsLiveTargets
                        || animation_state.write_channels.is_empty()
                    {
                        continue;
                    }
                    for target in &animation_state.targets {
                        let Some(target_state) = snapshot.heap.object(target) else {
                            continue;
                        };
                        if target_state.updating_suspended != Truth::No {
                            continue;
                        }
                        for registration in &scene.updaters {
                            if !active_overlapping_registration(
                                scene,
                                registration,
                                target,
                                target_state,
                                &animation_state.write_channels,
                            ) {
                                continue;
                            }
                            if !seen.insert((animation.site, registration.site)) {
                                continue;
                            }
                            let overlap: BTreeSet<WriteChannel> = registration
                                .body
                                .write_channels
                                .intersection(&animation_state.write_channels)
                                .copied()
                                .collect();
                            diagnostics.push(suspend_resume_diagnostic(
                                context,
                                scene,
                                animation,
                                registration,
                                &overlap,
                            ));
                        }
                    }
                }
            }
        }
        diagnostics
    }
}

fn active_overlapping_registration(
    scene: &crate::semantic::interpreter::SceneLifecycle,
    registration: &crate::semantic::interpreter::UpdaterRegistration,
    target: &crate::semantic::values::ObjectId,
    target_state: &crate::semantic::state::MobjectState,
    animation_channels: &BTreeSet<crate::semantic::state::WriteChannel>,
) -> bool {
    use crate::semantic::interpreter::UpdaterHost;
    use crate::semantic::values::{Presence, Truth};

    if registration.certainty != Presence::Present
        || registration.body.mutates_target != Truth::Yes
        || registration.body.calls_unknown != Truth::No
        || registration.body.channels_known != Truth::Yes
        || registration
            .body
            .write_channels
            .is_disjoint(animation_channels)
        || !matches!(
            &registration.host,
            UpdaterHost::Mobject(host) if host.definitely_same(target)
        )
        || !target_state.updaters.contains(&registration.fact)
    {
        return false;
    }
    // The heap's updater set is a may-set after branch joins. Any removal
    // of this identity before the play makes definite liveness unprovable.
    !scene.updater_removals.iter().any(|removal| {
        removal.matched != Truth::No
            && matches!(
                &removal.host,
                UpdaterHost::Mobject(host) if host.definitely_same(target)
            )
    })
}

fn suspend_resume_diagnostic(
    context: &RuleContext<'_>,
    scene: &crate::semantic::interpreter::SceneLifecycle,
    animation: &crate::semantic::interpreter::PlayedAnimation,
    registration: &crate::semantic::interpreter::UpdaterRegistration,
    overlap: &BTreeSet<crate::semantic::state::WriteChannel>,
) -> Diagnostic {
    let file = context.sources().file(animation.site.file);
    let registration_file = context.sources().file(registration.site.file);
    let channels: Vec<&str> = overlap
        .iter()
        .map(|channel| super::support::channel_label(*channel))
        .collect();
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "scene".to_owned(),
        Value::String(scene.qualified_name.clone()),
    );
    evidence.insert(
        "overlapping_channels".to_owned(),
        Value::Array(
            channels
                .iter()
                .map(|channel| Value::String((*channel).to_owned()))
                .collect(),
        ),
    );
    evidence.insert(
        "suspend_behavior".to_owned(),
        Value::String("suspends-live-targets".to_owned()),
    );
    evidence.insert("final_updater_pass".to_owned(), Value::Bool(true));
    let mut diagnostic = build_diagnostic(
        &MLC118,
        context,
        file,
        super::support::site_range(&animation.site),
        format!(
            "This animation suspends an active updater that also writes {channels}; \
             `finish()` resumes it and the same play immediately runs the updater \
             once with `dt=0`, so the animation's final state is immediately \
             followed by an overlapping write. Animate the updater's driver \
             (often a `ValueTracker`), or make the suspension choice explicit.",
            channels = channels.join(", "),
        ),
        "Normal Animation.begin() suspends the live target's updaters, finish() \
         interpolates alpha=1 and resumes them, and Scene.play then calls \
         update_mobjects(0) before returning. An active updater with a proven \
         overlapping write therefore observes neither intermediate animation \
         frames nor a stable post-animation interval (animation.py \
         Animation.begin/finish; scene.py play_internal; DESIGN §3.2)."
            .to_owned(),
        evidence,
    );
    diagnostic.related_locations.push(RelatedLocation {
        path: registration_file.relative_path().to_owned(),
        span: registration_file.span_of_range(super::support::site_range(&registration.site)),
        message: "active overlapping updater registered here".to_owned(),
    });
    diagnostic
}

/// For a non-lambda mismatch the interpreter's registered-updater set must
/// be provably complete: no registration on the same host with an unknown
/// callback identity, and no unresolved call that may have registered one
/// (an `UnknownMutation` touching the host) before the removal.
fn named_mismatch_is_reliable(
    scene: &crate::semantic::interpreter::SceneLifecycle,
    removal: &crate::semantic::interpreter::UpdaterRemoval,
) -> bool {
    use crate::semantic::events::Event;
    use crate::semantic::interpreter::UpdaterHost;
    use crate::semantic::state::CallbackRef;

    let same_host = |host: &UpdaterHost| match (host, &removal.host) {
        (UpdaterHost::Scene, UpdaterHost::Scene) => true,
        (UpdaterHost::Mobject(a), UpdaterHost::Mobject(b)) => a.definitely_same(b),
        _ => false,
    };
    if scene.updaters.iter().any(|registration| {
        same_host(&registration.host) && registration.fact.callback == CallbackRef::Unknown
    }) {
        return false;
    }
    let Some(removal_index) = scene.events.iter().position(|traced| {
        traced.site == removal.site
            && matches!(
                &traced.event,
                Event::Mutate(mutate)
                    if mutate.kind == crate::semantic::events::MutationKind::Updaters
            )
    }) else {
        return false;
    };
    let host_object = match &removal.host {
        UpdaterHost::Mobject(id) => Some(id),
        UpdaterHost::Scene => None,
    };
    !scene.events[..removal_index].iter().any(|traced| {
        matches!(
            &traced.event,
            Event::UnknownMutation(unknown)
                if host_object.is_some_and(|host| {
                    unknown.values.iter().any(|value| value.definitely_same(host))
                })
        )
    })
}
