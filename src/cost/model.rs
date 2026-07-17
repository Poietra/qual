//! Symbolic cost dimensions and values (DESIGN §4.1).
//!
//! TODO(phase-3): `Exact` / `Interval` / `Symbol` / `Unknown` counts, the
//! stage decomposition of `T_scene`, and dimension symbols (`F`, `N_family`,
//! `P`, `C`, `A_px`, `K_resource`, ...).

/// Placeholder for the symbolic cost model.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct CostModel {}
