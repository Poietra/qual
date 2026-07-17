//! Configuration model, loading, and precedence resolution (DESIGN §8.2).
//!
//! Precedence: `CLI > selected profile > pyproject base > manim.cfg >
//! builtin defaults`, encoded as [`model::ConfigFragment`] tiers and merged
//! by [`model::ConfigFragment::merge`]. All validation errors are
//! [`loader::ConfigError`] and map to exit code 2.

pub mod loader;
pub mod model;

pub use loader::{
    ConfigError, ManimCfgValues, ProfileSelection, ResolutionInput, find_pyproject, load_manim_cfg,
    load_pyproject, parse_manim_cfg, parse_pyproject, resolve,
};
pub use model::{ConfigFragment, ConfigSource, Platform, RenderProfile, Renderer, ResolvedConfig};
