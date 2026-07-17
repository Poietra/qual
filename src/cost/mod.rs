//! Symbolic cost model and estimator (DESIGN §4).
//!
//! TODO(phase-3): all submodules; `manim-lint cost` and the `MLP2xx` rules
//! build on these facts. Unknown dimensions are never assumed to be 1 and
//! never produce fabricated wall-clock numbers.

pub mod contexts;
pub mod estimator;
pub mod model;
