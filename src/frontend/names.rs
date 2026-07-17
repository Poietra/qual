//! Name resolution to qualified Manim symbols (DESIGN §5.3).
//!
//! TODO(phase-1): local assignment / shadowing, alias chains
//! (`import manim as mn`, `Square as Sq`), and qualified call target
//! candidate sets. Never decide "is a Scene" by name string alone.

/// Placeholder for the per-module symbol table and resolver.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct NameResolver {}
