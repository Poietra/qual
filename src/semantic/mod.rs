//! Manim abstract interpreter: values, heap, state, events, summaries
//! (DESIGN §5.5-§5.7).
//!
//! TODO(phase-2): all submodules; the interpreter produces the event IR that
//! lifecycle and cost rules consume, never per-rule AST visitors.

pub mod events;
pub mod heap;
pub mod interpreter;
pub mod state;
pub mod summaries;
pub mod values;
