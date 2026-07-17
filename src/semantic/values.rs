//! Abstract value lattices (DESIGN §5.5).
//!
//! TODO(phase-2): `Presence` / `Truth` / `Visibility` three-valued lattices,
//! kind candidate sets, and interval values. Unknown never collapses to a
//! definite value, and `MAYBE` alone never justifies an error.

/// Placeholder for the abstract value domain.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct AbstractValue {}
