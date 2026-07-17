//! Autofix application (DESIGN §6.3, Phase 5).
//!
//! TODO(phase-5): apply non-overlapping `TextEdit` sets, keep SAFE and
//! UNSAFE strictly separated, re-parse every edited file with the
//! configured `target-python`, and roll back all edits on failure.

/// Placeholder for the fix applier.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct FixApplier {}
