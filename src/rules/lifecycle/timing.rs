//! Duration and wait rules: `MLC104`, `MLC106`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, TextEdit,
};
use crate::frontend::index::{CallArgument, LiteralFact, QualifiedCall};
use crate::knowledge::SymbolKind;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayFact, PlayKind};
use crate::semantic::values::{AllocationSite, Num};
use crate::source::FileId;

use super::support::{
    SCENE_WAIT, WAIT_ANIMATION, bound_receiver, build_diagnostic, candidates_value,
    conclusive_target,
};

/// The literal numeric value of an argument, when statically known.
fn literal_number(argument: &CallArgument) -> Option<f64> {
    match argument.literal {
        #[allow(clippy::cast_precision_loss, reason = "durations are small numbers")]
        Some(LiteralFact::Int(value)) => Some(value as f64),
        Some(LiteralFact::Float(value)) => Some(value),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// MLC104: literal non-positive durations on *played* calls.
// ---------------------------------------------------------------------------

/// Metadata for [`NonPositiveDuration`].
pub const MLC104: RuleMetadata = RuleMetadata {
    id: "MLC104",
    summary: "Literal non-positive run_time or wait duration",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

/// Literal non-positive durations on played calls (DESIGN §7.1 `MLC104`,
/// §3.2).
///
/// Manim validates durations when a play *executes*, not when an animation
/// is constructed (`Scene.validate_run_time` on the compiled whole-play
/// `max(run_time)`, and on `Scene.wait` / `Scene.pause` arguments). The
/// rule therefore diagnoses from lifecycle play facts:
///
/// - a played wait whose literal duration is `<= 0`,
/// - a played `play(..., run_time=<literal <= 0>)` (the kwarg is
///   `setattr` onto every animation, so it *is* the whole-play duration),
/// - a literal `run_time <= 0` on a curated animation constructor written
///   directly as an argument of a played `play` (including `Wait`'s first
///   positional parameter). Such an animation either drives the whole-play
///   duration to zero or less (`ValueError`) or divides by zero in the
///   per-frame interpolation (`alpha = t / animation.run_time`).
///
/// An animation constructed with `run_time=0` but never played stays
/// silent; a play-level `run_time` kwarg (or a `**kwargs` splat) overrides
/// every per-animation run time, so constructor literals are skipped then.
/// Only syntactic literal proofs on the played call fire — anything
/// tracked through reassignable state stays silent.
pub struct NonPositiveDuration;

/// Dedupe / lookup key of a byte range in a file.
type SiteKey = (FileId, u32, u32);

/// The key of an interpreter site.
const fn site_key(site: &AllocationSite) -> SiteKey {
    (site.file, site.start, site.end)
}

impl Rule for NonPositiveDuration {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC104
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let calls: BTreeMap<SiteKey, &QualifiedCall> = context
            .qualified_calls()
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
        let mut sink = DurationSink {
            context,
            // The same helper play reached from two scenes is one defect.
            emitted: BTreeSet::new(),
            diagnostics: Vec::new(),
        };
        for scene in &context.lifecycle_facts().scenes {
            for play in &scene.plays {
                match play.kind {
                    PlayKind::Wait => sink.check_wait(&calls, play),
                    PlayKind::Play => sink.check_play(&calls, play),
                }
            }
        }
        sink.diagnostics
    }
}

/// Emission state of one [`NonPositiveDuration`] run.
struct DurationSink<'r, 'a> {
    context: &'r RuleContext<'a>,
    emitted: BTreeSet<SiteKey>,
    diagnostics: Vec<Diagnostic>,
}

impl DurationSink<'_, '_> {
    /// A played `wait` / `pause` whose literal duration is non-positive:
    /// `Scene.validate_run_time` raises `ValueError` when the call runs.
    fn check_wait(&mut self, calls: &BTreeMap<SiteKey, &QualifiedCall>, play: &PlayFact) {
        let Num::Exact(literal) = &play.duration else {
            return;
        };
        let value = literal.as_f64();
        if value > 0.0 {
            return;
        }
        let Some(call) = calls.get(&site_key(&play.site)) else {
            return;
        };
        // The interpreter derives an exact wait duration only from a
        // literal first positional / `duration` keyword argument; anchor
        // the diagnostic on that literal.
        let argument = call
            .positional(0)
            .filter(|argument| literal_number(argument).is_some())
            .or_else(|| {
                call.keyword("duration")
                    .filter(|argument| literal_number(argument).is_some())
            });
        let Some(argument) = argument else {
            return;
        };
        self.emit(call, argument, "duration", value, "wait-duration");
    }

    /// A played `play`: a non-positive literal `run_time` kwarg (the
    /// whole-play duration), or non-positive constructor literals on its
    /// animation arguments when no play-level override exists.
    fn check_play(&mut self, calls: &BTreeMap<SiteKey, &QualifiedCall>, play: &PlayFact) {
        let Some(play_call) = calls.get(&site_key(&play.site)) else {
            return;
        };
        if let Some(argument) = play_call.keyword("run_time") {
            // The kwarg is set onto every animation, so it decides the
            // whole-play duration alone: a non-literal value is Unknown
            // (silence), a positive literal makes constructor run times
            // irrelevant.
            let Some(value) = literal_number(argument) else {
                return;
            };
            if value <= 0.0 {
                self.emit(play_call, argument, "run_time", value, "play-run-time");
            }
            return;
        }
        if play_call.has_star_star_kwargs {
            // A `**kwargs` splat may supply `run_time` and override every
            // constructor literal: silence over a false positive.
            return;
        }
        let Some(profile) = self.context.knowledge() else {
            return;
        };
        let index = self.context.project_index();
        for played in &play.animations {
            // Only a curated animation constructor written directly as a
            // play argument provably forwards its literal `run_time` to
            // `Animation.run_time`; project subclasses may consume the
            // parameter differently and values routed through variables
            // may be reassigned before the play.
            let Some(constructor) = calls.get(&site_key(&played.site)) else {
                continue;
            };
            let Some((canonical, entry)) = conclusive_target(profile, index, constructor) else {
                continue;
            };
            if entry.kind != SymbolKind::Animation {
                continue;
            }
            if let Some(argument) = constructor.keyword("run_time") {
                if let Some(value) = literal_number(argument) {
                    if value <= 0.0 {
                        self.emit(
                            constructor,
                            argument,
                            "run_time",
                            value,
                            "animation-run-time",
                        );
                    }
                }
                continue;
            }
            // `Wait(0)`: the first positional parameter *is* `run_time`.
            if canonical == WAIT_ANIMATION {
                if let Some(argument) = constructor.positional(0) {
                    if let Some(value) = literal_number(argument) {
                        if value <= 0.0 {
                            self.emit(
                                constructor,
                                argument,
                                "run_time",
                                value,
                                "animation-run-time",
                            );
                        }
                    }
                }
            }
        }
    }

    /// Builds the diagnostic once per anchored literal argument.
    fn emit(
        &mut self,
        call: &QualifiedCall,
        argument: &CallArgument,
        parameter: &str,
        value: f64,
        model: &str,
    ) {
        let key = (
            call.file,
            argument.range.start().into(),
            argument.range.end().into(),
        );
        if !self.emitted.insert(key) {
            return;
        }
        let file = self.context.sources().file(call.file);
        let text = file.slice(argument.range);
        let mut evidence = BTreeMap::new();
        evidence.insert("candidates".to_owned(), candidates_value(call));
        evidence.insert("parameter".to_owned(), Value::String(parameter.to_owned()));
        evidence.insert("model".to_owned(), Value::String(model.to_owned()));
        evidence.insert(
            "literal_value".to_owned(),
            serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number),
        );
        self.diagnostics.push(build_diagnostic(
            &MLC104,
            self.context,
            file,
            argument.range,
            format!(
                "Use a positive `{parameter}`: the literal `{text}` is non-positive \
                 and playing it aborts the render."
            ),
            "Manim validates durations when a play executes, not when an animation \
             is constructed: `Scene.validate_run_time` rejects a whole-play or wait \
             duration of zero or less with `ValueError`, and an animation with a \
             non-positive `run_time` inside a longer play divides by zero in the \
             per-frame interpolation (`alpha = t / animation.run_time`). Only \
             played calls are reported; an unplayed construction is harmless."
                .to_owned(),
            evidence,
        ));
    }
}

// ---------------------------------------------------------------------------
// MLC106: stop_condition combined with frozen_frame=True.
// ---------------------------------------------------------------------------

/// Metadata for [`FrozenFrameStopCondition`].
pub const MLC106: RuleMetadata = RuleMetadata {
    id: "MLC106",
    summary: "wait() combines stop_condition with frozen_frame=True",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `self.wait(stop_condition=..., frozen_frame=True)` (DESIGN §7.1
/// `MLC106`).
pub struct FrozenFrameStopCondition;

impl Rule for FrozenFrameStopCondition {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC106
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
            if canonical != SCENE_WAIT {
                continue;
            }
            let Some(stop_condition) = call.keyword("stop_condition") else {
                continue;
            };
            // An explicit `stop_condition=None` restates the default.
            if matches!(stop_condition.literal, Some(LiteralFact::NoneLit)) {
                continue;
            }
            let Some(frozen_frame) = call.keyword("frozen_frame") else {
                continue;
            };
            if !matches!(frozen_frame.literal, Some(LiteralFact::Bool(true))) {
                continue;
            }
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(SCENE_WAIT.to_owned()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            let mut diagnostic = build_diagnostic(
                &MLC106,
                context,
                file,
                frozen_frame.range,
                "Remove `frozen_frame=True` (or the `stop_condition`): a frozen wait \
                 renders one static frame, so the stop condition is never evaluated \
                 against changing state and Manim rejects the combination."
                    .to_owned(),
                "`Scene.wait(stop_condition=...)` needs a dynamic wait that re-renders \
                 and re-evaluates the condition every frame; `frozen_frame=True` \
                 explicitly freezes the wait to a single repeated frame. The two \
                 options contradict each other and `Wait` raises `ValueError`."
                    .to_owned(),
                evidence,
            );
            diagnostic.fix = Some(Fix {
                applicability: FixApplicability::Unsafe,
                message: "Change `frozen_frame=True` to `frozen_frame=False`".to_owned(),
                edits: vec![TextEdit {
                    path: file.relative_path().to_owned(),
                    span: file.span_of_range(frozen_frame.range),
                    replacement: "False".to_owned(),
                }],
            });
            diagnostics.push(diagnostic);
        }
        diagnostics
    }
}
