//! Frame-count estimation, severity scoring, and evidence building
//! (DESIGN §4.3-§4.4, §6.3).
//!
//! Frame counts come only from literal durations multiplied by the profile
//! frame rate, expressed as an interval around `ceil(run_time × fps)`
//! (DESIGN §3.3: approximately that count, never "the exact frame count").
//! When the duration is unknown the estimate stays the symbolic `frames`
//! quantity — no fabricated invocation count ever appears (DESIGN §4.2, §15
//! invariant 9).

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::config::model::{RenderProfile, Renderer};
use crate::frontend::index::{
    ArgShape, CallArgument, LiteralFact, QualifiedCall, QualifiedCallFacts,
};
use crate::semantic::events::{InvocationContext, Multiplicity, OperationKind};
use crate::semantic::values::Num;

use super::contexts::HotContext;
use super::model::{CostDimension, CostFact};

/// The symbolic per-frame quantity used when no literal duration binds `F`.
#[must_use]
pub fn symbolic_frames() -> Num {
    Num::Symbol("frames".to_owned())
}

/// Estimates the rendered frame count `F` of a play from its duration and
/// one profile frame rate.
///
/// A bounded duration yields an interval whose ends are
/// `ceil(duration × fps)` — approximately the `np.arange` grid length
/// (DESIGN §3.3); it is deliberately kept an interval, never an "exact
/// frame count". A `Symbol` / `Unknown` duration (or a non-positive frame
/// rate) yields the symbolic `frames` quantity — per-frame only, no
/// fabricated number.
#[must_use]
pub fn frames_for_duration(duration: &Num, frame_rate: f64) -> Num {
    if !frame_rate.is_finite() || frame_rate <= 0.0 {
        return symbolic_frames();
    }
    match duration.bounds() {
        Some((lo, hi)) => Num::Interval {
            lo: lo.map(|seconds| (seconds * frame_rate).ceil().max(0.0)),
            hi: hi.map(|seconds| (seconds * frame_rate).ceil().max(0.0)),
        },
        None => symbolic_frames(),
    }
}

/// Frame count across every analyzed profile: the per-profile estimates
/// joined into one hull. Without profiles (or with an unknown duration) the
/// result stays symbolic.
#[must_use]
pub fn frames_across_profiles(duration: &Num, profiles: &[RenderProfile]) -> Num {
    let mut estimates = profiles
        .iter()
        .map(|profile| frames_for_duration(duration, profile.frame_rate));
    let Some(first) = estimates.next() else {
        return symbolic_frames();
    };
    estimates.fold(first, |joined, next| joined.join(&next))
}

/// Frame total of a sequence of plays: each play renders its **own** frame
/// grid (`np.arange(0, run_time, 1/frame_rate)` in scene.py
/// `get_time_progression`), so `ceil(duration × fps)` applies per play and
/// the per-play counts are summed — never applied once to the summed
/// duration, which undercounts sequences of short plays (two 1 ms plays at
/// 60 fps render 1 + 1 = 2 frames, not `ceil(0.002 × 60) = 1`).
///
/// A duration without a lower bound contributes `0` below; a missing upper
/// bound opens the total above — no fabricated count either way (DESIGN
/// §15 invariant 9).
#[must_use]
pub fn sum_frames_across_profiles<'d>(
    durations: impl IntoIterator<Item = &'d Num>,
    profiles: &[RenderProfile],
) -> Num {
    let mut lower = 0.0_f64;
    let mut upper = Some(0.0_f64);
    for duration in durations {
        let frames = frames_across_profiles(duration, profiles);
        if let Some(bound) = frames.lower_bound() {
            lower += bound;
        }
        upper = match (upper, frames.upper_bound()) {
            (Some(current), Some(bound)) => Some(current + bound),
            _ => None,
        };
    }
    Num::Interval {
        lo: Some(lower),
        hi: upper,
    }
}

/// Interval sum of durations in seconds: known lower bounds add up (an
/// unknown duration contributes `0` below) and any missing upper bound
/// opens the total above.
#[must_use]
pub fn sum_seconds<'d>(durations: impl IntoIterator<Item = &'d Num>) -> Num {
    let mut lower = 0.0_f64;
    let mut upper = Some(0.0_f64);
    for duration in durations {
        if let Some(bound) = duration.lower_bound() {
            lower += bound;
        }
        upper = match (upper, duration.upper_bound()) {
            (Some(current), Some(bound)) => Some(current + bound),
            _ => None,
        };
    }
    Num::Interval {
        lo: Some(lower),
        hi: upper,
    }
}

/// A positive literal duration in seconds carried by one argument.
///
/// Non-positive and non-numeric literals yield `None` (invalid durations
/// are `MLC104` territory, not frame estimation).
#[must_use]
pub fn literal_seconds(argument: &CallArgument) -> Option<Num> {
    match argument.literal {
        Some(LiteralFact::Int(value)) if value > 0 => Some(Num::int(value)),
        Some(LiteralFact::Float(value)) if value > 0.0 => Some(Num::float(value)),
        _ => None,
    }
}

/// Literal duration of a `Scene.wait(...)` / `Wait(...)` style call: the
/// first matching keyword in `keywords`, else the first positional
/// argument.
///
/// `*args` splats make positions unreliable and `**kwargs` splats make
/// keyword absence unprovable; both degrade to [`Num::Unknown`].
#[must_use]
pub fn wait_duration(call: &QualifiedCall, keywords: &[&str]) -> Num {
    for keyword in keywords {
        if let Some(argument) = call.keyword(keyword) {
            return literal_seconds(argument).unwrap_or(Num::Unknown);
        }
    }
    if call.has_star_star_kwargs || call.has_star_args {
        return Num::Unknown;
    }
    call.positional(0)
        .and_then(literal_seconds)
        .unwrap_or(Num::Unknown)
}

/// Literal duration of a `Scene.play(...)` call.
///
/// An explicit `run_time=` play kwarg wins (play kwargs are applied to
/// every animation). Otherwise the duration is `max` over the animation
/// arguments' own literal `run_time=` values; an animation without a
/// provable run time opens the upper bound instead of being assumed to be
/// the 1-second default — the lower bound from the known ones survives.
#[must_use]
pub fn play_run_time(call: &QualifiedCall, calls: &QualifiedCallFacts) -> Num {
    if let Some(argument) = call.keyword("run_time") {
        return literal_seconds(argument).unwrap_or(Num::Unknown);
    }
    if call.has_star_star_kwargs || call.has_star_args || call.positional_count == 0 {
        return Num::Unknown;
    }
    let mut duration: Option<Num> = None;
    for position in 0..call.positional_count {
        let child_run_time = call
            .positional(position)
            .and_then(|argument| match argument.shape {
                ArgShape::Call(child_index) => calls.calls.get(child_index),
                _ => None,
            })
            .map_or(Num::Unknown, animation_literal_run_time);
        duration = Some(match duration {
            None => child_run_time,
            Some(current) => current.max_with(&child_run_time),
        });
    }
    duration.unwrap_or(Num::Unknown)
}

/// Literal `run_time=` of one animation constructor call; `Unknown` when
/// not explicitly written (the curated default is not fabricated here).
fn animation_literal_run_time(call: &QualifiedCall) -> Num {
    match call.keyword("run_time") {
        Some(argument) => literal_seconds(argument).unwrap_or(Num::Unknown),
        None => Num::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Severity scoring scaffold (DESIGN §4.4).
// ---------------------------------------------------------------------------

/// The four factors of the DESIGN §4.4 severity score
/// (`score = operation_weight × frequency_weight × known_size_weight ×
/// profile_multiplier`).
///
/// This is a scaffold: the weights are deterministic rank classes, not
/// calibrated milliseconds. Unknown facts never raise a weight above its
/// baseline (DESIGN §15 invariant 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeverityScore {
    /// Intrinsic weight of the operation kind.
    pub operation_weight: f64,
    /// Weight of the invocation context and bounded multiplicity.
    pub frequency_weight: f64,
    /// Weight of provably large size dimensions.
    pub known_size_weight: f64,
    /// Profile-supplied multiplier (resolution / renderer emphasis).
    pub profile_multiplier: f64,
}

impl SeverityScore {
    /// Scores one cost fact under a profile multiplier.
    #[must_use]
    pub fn of(fact: &CostFact, profile_multiplier: f64) -> Self {
        Self {
            operation_weight: operation_weight(&fact.operation),
            frequency_weight: frequency_weight(fact.context, &fact.multiplicity),
            known_size_weight: known_size_weight(&fact.dimensions),
            profile_multiplier,
        }
    }

    /// The combined score.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.operation_weight
            * self.frequency_weight
            * self.known_size_weight
            * self.profile_multiplier
    }
}

/// Intrinsic weight class of an operation kind (DESIGN §4.3 dominant
/// terms; external processes dominate).
#[must_use]
pub fn operation_weight(operation: &OperationKind) -> f64 {
    match operation {
        OperationKind::ExternalProcess => 10.0,
        OperationKind::Construction | OperationKind::SvgParse => 4.0,
        OperationKind::Copy | OperationKind::AlignData | OperationKind::Raster => 3.0,
        OperationKind::Interpolation
        | OperationKind::FamilyWalk
        | OperationKind::Readback
        | OperationKind::Encode
        | OperationKind::Allocation => 2.0,
        OperationKind::Other(_) => 1.0,
    }
}

/// Frequency weight: the invocation-context class, raised only when the
/// multiplicity product has a provable large lower bound. `Unknown`
/// products never raise the weight (never assume an unknown factor).
#[must_use]
pub fn frequency_weight(context: InvocationContext, multiplicity: &Multiplicity) -> f64 {
    let base = if context.is_per_frame() {
        8.0
    } else {
        match context {
            InvocationContext::PlayBegin | InvocationContext::PlayCleanup => 2.0,
            _ => 1.0,
        }
    };
    let boost = match multiplicity.product().lower_bound() {
        Some(lower) if lower >= 100.0 => 2.0,
        Some(lower) if lower >= 10.0 => 1.5,
        _ => 1.0,
    };
    base * boost
}

/// Size weight: the largest provable lower bound across the fact's
/// dimensions, classed into ranks. Unmeasured or unknown dimensions weigh
/// the neutral `1.0`.
#[must_use]
pub fn known_size_weight(dimensions: &BTreeMap<CostDimension, Num>) -> f64 {
    dimensions
        .values()
        .filter_map(Num::lower_bound)
        .fold(1.0, |weight, lower| {
            let rank = if lower >= 1024.0 {
                3.0
            } else if lower >= 256.0 {
                2.0
            } else if lower >= 32.0 {
                1.5
            } else {
                1.0
            };
            weight.max(rank)
        })
}

// ---------------------------------------------------------------------------
// Evidence building (DESIGN §6.3).
// ---------------------------------------------------------------------------

/// The DESIGN §6.3 `evidence.invocation_context` label of a context.
#[must_use]
pub const fn invocation_context_label(context: InvocationContext) -> &'static str {
    match context {
        InvocationContext::ModuleImport => "module-import",
        InvocationContext::SceneInit => "scene-init",
        InvocationContext::Setup => "setup",
        InvocationContext::Construct => "construct",
        InvocationContext::PlayBegin => "play-begin",
        InvocationContext::FrameCallback => "frame-callback",
        InvocationContext::Raster => "raster",
        InvocationContext::PlayCleanup => "play-cleanup",
        InvocationContext::TearDown => "tear-down",
        InvocationContext::Unknown => "unknown",
    }
}

/// Names of the non-neutral multiplicity factors, in canonical factor
/// order. A factor is listed when it is not exactly `×1` — including
/// symbolic and unknown factors, which are precisely the ones a reader
/// must not ignore.
#[must_use]
pub fn multiplicity_factor_names(multiplicity: &Multiplicity) -> Vec<String> {
    multiplicity
        .factors()
        .into_iter()
        .filter(|(_, value)| **value != Num::int(1))
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Machine-readable evidence in the DESIGN §6.3 shape.
///
/// Unknown frame counts serialize as `null`, never as a fabricated
/// number; empty collections serialize as `[]`. Size facts
/// (`family_size`, `points`) appear **only** when a real lower or upper
/// bound is known — an unknown size is omitted entirely rather than
/// carrying a fake number.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    /// Lifecycle phase of the diagnosed operation.
    pub invocation_context: Option<InvocationContext>,
    /// Non-neutral multiplicity factor names.
    pub multiplicity: Vec<String>,
    /// Frame-count estimate; symbolic / unknown serializes as `null`.
    pub frames: Option<Num>,
    /// Family-size estimate; omitted unless real bounds are known.
    pub family_size: Option<Num>,
    /// Point-count estimate; omitted unless real bounds are known.
    pub points: Option<Num>,
    /// Renderers of the analyzed profiles.
    pub renderers: Vec<Renderer>,
    /// State path (origin function, then provenance labels).
    pub state_path: Vec<String>,
}

impl Evidence {
    /// Evidence for an operation inside one hot context.
    #[must_use]
    pub fn for_hot_context(hot: &HotContext, frames: Num, profiles: &[RenderProfile]) -> Self {
        Self {
            invocation_context: Some(hot.context),
            multiplicity: multiplicity_factor_names(&hot.multiplicity),
            frames: Some(frames),
            family_size: None,
            points: None,
            renderers: renderers_of(profiles),
            state_path: hot.state_path(),
        }
    }

    /// Serializes into the DESIGN §6.3 JSON shape.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut json = json!({
            "invocation_context": match self.invocation_context {
                Some(InvocationContext::Unknown) | None => Value::Null,
                Some(context) => Value::String(invocation_context_label(context).to_owned()),
            },
            "multiplicity": self.multiplicity,
            "frames": self.frames.as_ref().map_or(Value::Null, num_bounds_json),
            "renderer": self
                .renderers
                .iter()
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>(),
            "state_path": self.state_path,
        });
        let fields = json
            .as_object_mut()
            .expect("evidence JSON is always an object");
        // Size facts carry real bounds or are absent — never a fabricated
        // number and never a null placeholder.
        for (key, value) in [("family_size", &self.family_size), ("points", &self.points)] {
            if let Some(value) = value {
                let bounds = num_bounds_json(value);
                if !bounds.is_null() {
                    fields.insert(key.to_owned(), bounds);
                }
            }
        }
        json
    }
}

/// Deduplicated renderers of the analyzed profiles, in stable
/// (alphabetical label) order.
#[must_use]
pub fn renderers_of(profiles: &[RenderProfile]) -> Vec<Renderer> {
    let mut seen = BTreeSet::new();
    let mut renderers = Vec::new();
    for profile in profiles {
        if seen.insert(profile.renderer.to_string()) {
            renderers.push(profile.renderer);
        }
    }
    renderers.sort_by_key(std::string::ToString::to_string);
    renderers
}

/// `{"lower": ..., "upper": ...}` for a bounded value, `null` for
/// `Symbol` / `Unknown` — a missing bound is `null`, never a fake number.
#[must_use]
pub fn num_bounds_json(num: &Num) -> Value {
    match num.bounds() {
        Some((lo, hi)) if lo.is_some() || hi.is_some() => json!({
            "lower": lo.map_or(Value::Null, bound_json),
            "upper": hi.map_or(Value::Null, bound_json),
        }),
        _ => Value::Null,
    }
}

/// One numeric bound, as an integer when it is integral.
#[allow(
    clippy::cast_possible_truncation,
    reason = "integrality and 2^53 range are checked before the cast"
)]
fn bound_json(bound: f64) -> Value {
    const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53
    if bound.fract() == 0.0 && bound.abs() < EXACT_INT_LIMIT {
        json!(bound as i64)
    } else {
        json!(bound)
    }
}
