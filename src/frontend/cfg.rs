//! Intra-procedural control-flow graphs (DESIGN §5.7).
//!
//! TODO(phase-2): `if` / `for` / `while` / `break` / `continue` / `return` /
//! `try`-`finally` / `with` / `match`, bounded comprehension summaries, and
//! reachable-method SCC ordering for summary fixpoints.

/// Placeholder for one function's control-flow graph.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct ControlFlowGraph {}
