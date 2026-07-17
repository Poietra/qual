//! Interprocedural method effect summaries (DESIGN §5.7).
//!
//! TODO(phase-2): fixpoint effect summaries per strongly connected
//! component of project-local helpers, widening only the unresolved effects
//! of non-converging recursive SCCs.

/// Placeholder for one callable's effect summary.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct MethodSummary {}
