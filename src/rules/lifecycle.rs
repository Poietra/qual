//! Lifecycle / definite-correctness rules (`MLC1xx`, DESIGN §7.1).
//!
//! Phase 1 implements the direct-call and literal rules over qualified call
//! facts: `MLC101`-`MLC106`, `MLC109`, `MLC122`, `MLC126`, `MLC127`.
//! Phase 2 adds the state-dependent rules over the abstract-interpreter
//! facts: `MLC107`, `MLC108`, `MLC110`, `MLC111`, `MLC112`, `MLC113`,
//! `MLC114`, `MLC115`, `MLC117`, `MLC119`, `MLC120`, `MLC121`, `MLC123`,
//! `MLC124`, `MLC125`, `MLC128`, `MLC129`.

mod builder_rules;
mod callbacks;
mod constructors;
mod identity;
mod membership;
mod ownership;
mod play_args;
mod play_conflicts;
mod state_targets;
mod structure;
mod support;
mod timing;
mod updaters;

use crate::rules::base::Rule;

/// Every implemented lifecycle rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a lifecycle rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(play_args::EmptyPlay),
        Box::new(play_args::NonAnimationPlayArgument),
        Box::new(play_args::BoundMethodPlayArgument),
        Box::new(timing::NonPositiveDuration),
        Box::new(updaters::UpdaterCannotBind),
        Box::new(timing::FrozenFrameStopCondition),
        Box::new(state_targets::MissingGeneratedTarget),
        Box::new(play_conflicts::ConflictingPlayWrites),
        Box::new(play_args::EmptyAnimationGroup),
        Box::new(structure::SelfOrCyclicChild),
        Box::new(ownership::OrphanedUpdaterObject),
        Box::new(updaters::FrozenWaitFrameVaryingUpdater),
        Box::new(builder_rules::AnimateKwargsAfterMethod),
        Box::new(builder_rules::UnsupportedOverrideAnimateChain),
        Box::new(structure::RemovedChildReappears),
        Box::new(identity::PostTransformTargetConfusion),
        Box::new(builder_rules::StaleAnimateBuilder),
        Box::new(updaters::UpdaterSuspendResumeDivergence),
        Box::new(structure::ReplaceMissingOld),
        Box::new(state_targets::MissingSavedState),
        Box::new(updaters::TimelineReentry),
        Box::new(play_args::ApplyMethodCallResult),
        Box::new(callbacks::ApplyFunctionCallbackNoMobject),
        Box::new(builder_rules::NonMutatingAnimateMethod),
        Box::new(updaters::RemoveUpdaterIdentityMismatch),
        Box::new(membership::InvalidFamilyChild),
        Box::new(membership::DuplicateChild),
        Box::new(constructors::MissingSuperInit),
        Box::new(play_conflicts::PlayLagRatioStagger),
    ]
}
