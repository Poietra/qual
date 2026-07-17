//! Rule engine: contract, registry, and per-domain rule modules.

pub mod base;
pub mod lifecycle;
pub mod performance;
pub mod portability;
pub mod registry;
pub mod rendering;

pub use base::{Rule, RuleContext};
