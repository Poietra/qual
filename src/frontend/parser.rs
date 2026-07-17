//! Higher-level parse facade over [`crate::source::SourceManager`].
//!
//! TODO(phase-1): per-module AST views (module docstring, top-level defs,
//! decoded literal model backed by the token stream for rules like
//! `MLR103` that need raw string prefixes and escapes).

/// Placeholder for the per-module syntactic view consumed by the frontend.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct ModuleSyntax {}
