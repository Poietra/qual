//! Static analysis for Manim source projects.
//!
//! The analyzer never imports Manim and never executes analyzed user code.

pub mod application;
pub mod cli;
pub mod config;
pub mod cost;
pub mod diagnostic;
pub mod frontend;
pub mod knowledge;
pub mod reporting;
pub mod rules;
pub mod semantic;
pub mod source;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
