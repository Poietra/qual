//! Manim abstract interpreter: values, heap, state, events, summaries
//! (DESIGN §5.5-§5.7).
//!
//! This layer is the pure data model of the abstract interpreter:
//!
//! - [`values`]: three-valued lattices, the symbolic numeric domain,
//!   allocation-site object identity, and copy provenance.
//! - [`state`]: per-mobject / animation / scene / camera / output /
//!   resource abstract state records with `join` and `widen`.
//! - [`heap`]: the abstract heap tying object ids to states, singleton-only
//!   alias classes, and copy edges.
//! - [`events`]: the event IR shared by lifecycle and cost rules.
//! - [`dependency`]: forward/reverse semantic dependency edges shared by
//!   cache partitioning and source-change impact analysis.
//! - [`summaries`]: fixpoint effect summaries of project-local helpers
//!   over the call-graph SCCs (DESIGN §5.7).
//! - [`interpreter`]: the Manim lifecycle abstract interpreter producing
//!   the event traces and state snapshots that lifecycle rules consume —
//!   never per-rule AST visitors.

pub mod dependency;
pub mod events;
pub mod heap;
pub mod interpreter;
pub mod state;
pub mod summaries;
pub mod values;
