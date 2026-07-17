//! Lifecycle / definite-correctness rules (`MLC1xx`, DESIGN §7.1).
//!
//! TODO(phase-1): direct-call and literal rules (`MLC101`-`MLC106`,
//! `MLC109`, `MLC122`, `MLC126`, `MLC127`).
//! TODO(phase-2): state-dependent lifecycle rules over the abstract
//! interpreter facts (`MLC107`-`MLC129`).

/// Placeholder owner type for the lifecycle rule set.
///
/// Replaced by concrete `Rule` implementations in later phases; kept so the
/// module has a stable public anchor for registration.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct LifecycleRules {}
