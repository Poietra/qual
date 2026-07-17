//! The Manim lifecycle abstract interpreter (DESIGN §3, §5.5-§5.7).
//!
//! TODO(phase-2): MRO-composed `__init__ → setup → construct → tear_down`
//! walks, the exact `Scene.play` event sequence (compile args, auto-add,
//! begin, per-frame grid, finish, cleanup), branch joins to `MAYBE`, and
//! loop widening after bounded iteration.

/// Placeholder for the abstract interpreter entry point.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct AbstractInterpreter {}
