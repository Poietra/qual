//! Rendering / geometry / renderer-compatibility rules (`MLR1xx`, DESIGN §7.2).
//!
//! TODO(phase-1): literal rules (`MLR101`, `MLR103`-`MLR106`, `MLR115`,
//! `MLR117`, `MLR124`, `MLR126`).
//! TODO(phase-2): state-dependent rules (`MLR102`, `MLR113`, `MLR114`,
//! `MLR116`, `MLR125`, `MLR127`).
//! TODO(phase-4): renderer-dependent rules (`MLR107`-`MLR112`,
//! `MLR118`-`MLR123`).

use crate::rules::base::Rule;

/// Every implemented rendering rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a rendering rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    Vec::new()
}
