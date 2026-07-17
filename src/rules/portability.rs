//! Determinism / portability / cache-stability rules (`MLD3xx`, DESIGN §7.4).
//!
//! TODO(phase-4): `MLD301`-`MLD307` alongside the renderer/asset/font
//! portability facts.

/// Placeholder owner type for the portability rule set.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct PortabilityRules {}
