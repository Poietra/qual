//! Rendering / geometry / renderer-compatibility rules (`MLR1xx`, DESIGN §7.2).
//!
//! TODO(phase-1): literal rules (`MLR101`, `MLR103`-`MLR106`, `MLR115`,
//! `MLR117`, `MLR124`, `MLR126`).
//! TODO(phase-2): state-dependent rules (`MLR102`, `MLR113`, `MLR114`,
//! `MLR116`, `MLR125`, `MLR127`).
//! TODO(phase-4): renderer-dependent rules (`MLR107`-`MLR112`,
//! `MLR118`-`MLR123`).

/// Placeholder owner type for the rendering rule set.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct RenderingRules {}
