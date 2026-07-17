//! Lifecycle / definite-correctness rules (`MLC1xx`, DESIGN §7.1).
//!
//! Phase 1 implements the direct-call and literal rules over qualified call
//! facts: `MLC101`-`MLC106`, `MLC109`, `MLC122`, `MLC126`, `MLC127`.
//! TODO(phase-2): state-dependent lifecycle rules over the abstract
//! interpreter facts (`MLC107`-`MLC129`).

mod membership;
mod play_args;
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
        Box::new(play_args::EmptyAnimationGroup),
        Box::new(play_args::ApplyMethodCallResult),
        Box::new(membership::InvalidFamilyChild),
        Box::new(membership::DuplicateChild),
    ]
}
