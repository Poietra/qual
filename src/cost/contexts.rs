//! Invocation contexts and multiplicities (DESIGN §4.2).
//!
//! TODO(phase-3): the `InvocationContext` phases (`MODULE_IMPORT` ...
//! `FRAME_CALLBACK` ... `UNKNOWN`), per-call-site multiplicity products,
//! and hot-context propagation into transitively called project helpers.

/// Placeholder for invocation-context facts.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct InvocationContexts {}
