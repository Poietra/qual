//! Versioned Manim knowledge profiles (DESIGN §5.4).
//!
//! The linter never imports Manim; everything it knows about Manim symbols,
//! effects, and renderer capabilities comes from reviewed JSON profiles under
//! `knowledge/profiles/` (e.g. `v0_20.json`, plus local fork overlays that
//! replace entries via `base_profile` / `deleted_symbols`).
//!
//! TODO(phase-1): profile schema, loading, digest checks, and overlay
//! resolution.

pub mod model;
