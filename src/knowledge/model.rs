//! Data model for versioned Manim knowledge profiles (DESIGN §5.4).
//!
//! TODO(phase-1): symbol entries (kind, accepted targets, introducer /
//! remover / suspend / replacement effects, `returns_self`, renderer point
//! layouts), overlay merging by qualified symbol, and `deleted_symbols`.

/// Placeholder for a loaded, overlay-resolved knowledge profile.
///
/// The real model carries `schema_version`, `name`, `manim_version` range,
/// `source_digest`, optional `base_profile`, and the curated symbol table.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct KnowledgeProfile {}
