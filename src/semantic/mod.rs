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
//!
//! TODO(phase-2): [`interpreter`] and [`summaries`] produce the event IR
//! that lifecycle and cost rules consume, never per-rule AST visitors.

pub mod events;
pub mod heap;
pub mod interpreter;
pub mod state;
pub mod summaries;
pub mod values;
