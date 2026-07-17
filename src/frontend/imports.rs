//! Module and import graph construction (DESIGN §5.3).
//!
//! TODO(phase-1): explicit / star / relative import edges, `__all__`
//! handling, and unresolved-star widening to `Unknown`.

/// Placeholder for the project import graph.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct ImportGraph {}
