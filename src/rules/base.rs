//! Rule engine contract (DESIGN §6.3).
//!
//! Rules never walk the AST themselves; they query facts exposed by
//! [`RuleContext`]. Phase 0 exposes the parsed project and resolved
//! configuration; later phases add fact layers behind the placeholder
//! accessors without changing the [`Rule`] trait.

use crate::config::model::{RenderProfile, ResolvedConfig};
use crate::diagnostic::{Diagnostic, RuleMetadata};
use crate::source::SourceManager;

/// A single diagnostic rule.
///
/// Implementations must be deterministic: identical inputs produce an
/// identical diagnostic list (order included).
pub trait Rule {
    /// Static metadata (ID, severity, phase, capabilities).
    fn metadata(&self) -> &'static RuleMetadata;

    /// Runs the rule over the whole analyzed project.
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic>;
}

/// Qualified Manim call facts, resolved by the frontend
/// (`frontend::index::QualifiedCallFacts`): per-call candidate target sets,
/// receiver classification, argument summaries, and enclosing contexts.
pub use crate::frontend::index::QualifiedCallFacts;

/// Project-wide module/import/symbol/class-hierarchy index built by the
/// frontend (`frontend::index::ProjectIndex`): module tree, namespaces,
/// export surfaces, class hierarchy, and Scene / Mobject / Animation
/// subclass discovery.
pub use crate::frontend::index::ProjectIndex as ProjectIndexFacts;

/// Lifecycle events and abstract state snapshots.
///
/// TODO(phase-2): populated by `semantic::interpreter` with the event IR of
/// DESIGN §5.6 (`SceneAdd`, `RegisterUpdater`, `BeginPlay`, ...).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct LifecycleFacts {}

/// Symbolic cost facts (invocation contexts and multiplicities).
///
/// TODO(phase-3): populated by `cost::estimator` with per-operation
/// `Cost(operation, dimensions, invocation_context, multiplicity)` facts.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct CostFacts {}

/// Everything a rule may query about the analyzed project.
///
/// The struct only grows: later phases add fact layers and accessors, so
/// rules written against Phase 0 keep compiling unchanged.
#[derive(Debug)]
pub struct RuleContext<'a> {
    sources: &'a SourceManager,
    config: &'a ResolvedConfig,
    qualified_calls: QualifiedCallFacts,
    project_index: ProjectIndexFacts,
    lifecycle_facts: LifecycleFacts,
    cost_facts: CostFacts,
}

impl<'a> RuleContext<'a> {
    /// Builds a context over the parsed project and resolved configuration.
    #[must_use]
    pub fn new(sources: &'a SourceManager, config: &'a ResolvedConfig) -> Self {
        Self {
            sources,
            config,
            qualified_calls: QualifiedCallFacts::default(),
            project_index: ProjectIndexFacts::default(),
            lifecycle_facts: LifecycleFacts::default(),
            cost_facts: CostFacts::default(),
        }
    }

    /// Attaches frontend facts (project index and qualified calls).
    ///
    /// Built by `frontend::index::analyze`; without this call the context
    /// keeps empty default facts, so Phase 0 callers stay unchanged.
    #[must_use]
    pub fn with_frontend(mut self, index: ProjectIndexFacts, calls: QualifiedCallFacts) -> Self {
        self.project_index = index;
        self.qualified_calls = calls;
        self
    }

    /// Parsed files, tokens, comments, and span conversions.
    #[must_use]
    pub const fn sources(&self) -> &'a SourceManager {
        self.sources
    }

    /// The resolved effective configuration.
    #[must_use]
    pub const fn config(&self) -> &'a ResolvedConfig {
        self.config
    }

    /// Render profiles analyzed in this run.
    #[must_use]
    pub fn active_profiles(&self) -> &'a [RenderProfile] {
        &self.config.active_profiles
    }

    /// Resolved qualified Manim call facts.
    ///
    /// Empty unless attached via [`RuleContext::with_frontend`].
    #[must_use]
    pub const fn qualified_calls(&self) -> &QualifiedCallFacts {
        &self.qualified_calls
    }

    /// Project-wide symbol and class-hierarchy index.
    ///
    /// Empty unless attached via [`RuleContext::with_frontend`].
    #[must_use]
    pub const fn project_index(&self) -> &ProjectIndexFacts {
        &self.project_index
    }

    /// Lifecycle events from the abstract interpreter.
    ///
    /// TODO(phase-2): currently empty.
    #[must_use]
    pub const fn lifecycle_facts(&self) -> &LifecycleFacts {
        &self.lifecycle_facts
    }

    /// Symbolic cost facts.
    ///
    /// TODO(phase-3): currently empty.
    #[must_use]
    pub const fn cost_facts(&self) -> &CostFacts {
        &self.cost_facts
    }
}
