//! Abstract heap and object identity (DESIGN §5.5).
//!
//! TODO(phase-2): object IDs keyed by `(allocation site, bounded call
//! context, cardinality)`, alias propagation through `returns_self`
//! mutators, and fresh identities (with `copy_of` relations only) for
//! `copy` / `deepcopy` / `generate_target` / animation copies.

/// Placeholder for the abstract heap.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct AbstractHeap {}
