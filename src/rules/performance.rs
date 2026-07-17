//! Performance / cost-multiplicity rules (`MLP2xx`, DESIGN §7.3).
//!
//! TODO(phase-3): high-confidence hot-context rules first (`MLP201`,
//! `MLP204`, `MLP205`, `MLP217`, `MLP218`, `MLP220`, `MLP226`, `MLP227`),
//! then cardinality-dependent rules once estimation is stable.

/// Placeholder owner type for the performance rule set.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct PerformanceRules {}
