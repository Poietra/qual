//! Determinism / portability / cache-stability rules (`MLD3xx`, DESIGN §7.4).
//!
//! TODO(phase-4): `MLD301`-`MLD307` alongside the renderer/asset/font
//! portability facts.

use crate::rules::base::Rule;

/// Every implemented portability rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a portability rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    Vec::new()
}
