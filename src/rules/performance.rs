//! Performance / cost-multiplicity rules (`MLP2xx`, DESIGN §7.3).
//!
//! TODO(phase-3): high-confidence hot-context rules first (`MLP201`,
//! `MLP204`, `MLP205`, `MLP217`, `MLP218`, `MLP220`, `MLP226`, `MLP227`),
//! then cardinality-dependent rules once estimation is stable.

use crate::rules::base::Rule;

/// Every implemented performance rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a performance rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    Vec::new()
}
