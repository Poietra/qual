//! Baseline files (DESIGN §8.3, Phase 5).
//!
//! TODO(phase-5): fingerprints built from rule ID + relative path +
//! qualified Scene + surrounding token hash (never line numbers, so adding
//! lines does not invalidate a whole baseline), plus `--baseline` /
//! `--write-baseline` handling against `schemas/baseline-v1.json`.

/// Placeholder for a loaded baseline.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct Baseline {}
