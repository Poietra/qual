//! Cost estimation over lifecycle events (DESIGN §4.3-§4.4).
//!
//! TODO(phase-3): per-stage dominant terms (begin copy/align, interpolation,
//! family walks, Text/TeX/SVG, Cairo suffix raster, OpenGL readback, 3D),
//! severity scoring by multiplicity, and the `manim-lint cost` breakdown.

/// Placeholder for the cost estimator.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct CostEstimator {}
