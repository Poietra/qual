//! Lifecycle / definite-correctness rules (`MLC1xx`, DESIGN §7.1).
//!
//! TODO(phase-1): direct-call and literal rules (`MLC101`-`MLC106`,
//! `MLC109`, `MLC122`, `MLC126`, `MLC127`).
//! TODO(phase-2): state-dependent lifecycle rules over the abstract
//! interpreter facts (`MLC107`-`MLC129`).

use crate::rules::base::Rule;

/// Every implemented lifecycle rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a lifecycle rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    Vec::new()
}
