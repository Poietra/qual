//! Rule engine contract (DESIGN §6.3).
//!
//! Rules never walk the AST themselves; they query facts exposed by
//! [`RuleContext`]. Phase 0 exposes the parsed project and resolved
//! configuration; later phases add fact layers behind the placeholder
//! accessors without changing the [`Rule`] trait.

use crate::config::model::{RenderProfile, ResolvedConfig};
use crate::diagnostic::{Diagnostic, RuleMetadata};
use crate::knowledge::KnowledgeProfile;
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

/// Statement-position facts (enclosing-statement span and role of every
/// call expression) and conservative per-file import binding facts,
/// collected by the frontend alongside the qualified calls
/// (`frontend::statements`, DESIGN §5.3 / §5.6).
pub use crate::frontend::statements::{BindingFacts, StatementFacts};

/// Lifecycle events, membership snapshots, play groups, updater and
/// builder facts, produced by the abstract interpreter
/// (`semantic::interpreter::analyze`, DESIGN §5.6 / §3).
///
/// See [`LifecycleFacts`] for the per-rule query map (which facts each
/// Phase-2 rule should read).
pub use crate::semantic::interpreter::LifecycleFacts;

/// Capability-pack fact types for the reserved rules (see the
/// [`LifecycleFacts`] query map): per-callable return facts (MLC123),
/// updater-body dataflow (MLC112 / MLP218), own-path state (MLR116),
/// per-statement ownership intervals (MLC111), fixed-object registration
/// facts and the scene camera kind (renderer wave, DESIGN §3.5).
pub use crate::semantic::interpreter::{
    CallbackReturnFacts, CameraKind, FixedAction, FixedKind, FixedRegistrationFact,
    OwnershipInterval, PathStateFact, ReturnFact, UpdaterBodyFact,
};

/// Symbolic cost facts (hot contexts, multiplicities, frame estimates, and
/// per-operation cost facts) produced by [`crate::cost::CostFacts::compute`]
/// (DESIGN §4).
pub use crate::cost::CostFacts;

/// Everything a rule may query about the analyzed project.
///
/// The struct only grows: later phases add fact layers and accessors, so
/// rules written against Phase 0 keep compiling unchanged.
#[derive(Debug)]
pub struct RuleContext<'a> {
    sources: &'a SourceManager,
    config: &'a ResolvedConfig,
    knowledge: Option<&'a KnowledgeProfile>,
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
            knowledge: None,
            qualified_calls: QualifiedCallFacts::default(),
            project_index: ProjectIndexFacts::default(),
            lifecycle_facts: LifecycleFacts::default(),
            cost_facts: CostFacts::default(),
        }
    }

    /// Attaches the loaded Manim knowledge profile so rules can query
    /// symbol effects, `returns_self`, signatures, and renderer facts.
    #[must_use]
    pub const fn with_knowledge(mut self, knowledge: &'a KnowledgeProfile) -> Self {
        self.knowledge = Some(knowledge);
        self
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

    /// Attaches lifecycle facts from the abstract interpreter
    /// (`semantic::interpreter::analyze`).
    ///
    /// Without this call the context keeps an empty default — no scene was
    /// analyzed — and lifecycle rules must stay silent.
    #[must_use]
    pub fn with_lifecycle(mut self, lifecycle: LifecycleFacts) -> Self {
        self.lifecycle_facts = lifecycle;
        self
    }

    /// Attaches symbolic cost facts (`cost::CostFacts::compute`).
    ///
    /// Without this call the context keeps an empty default — no cost
    /// analysis ran — and cost-driven rules must stay silent.
    #[must_use]
    pub fn with_cost(mut self, cost: CostFacts) -> Self {
        self.cost_facts = cost;
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

    /// The loaded Manim knowledge profile.
    ///
    /// `None` unless attached via [`RuleContext::with_knowledge`].
    #[must_use]
    pub const fn knowledge(&self) -> Option<&'a KnowledgeProfile> {
        self.knowledge
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

    /// Statement-position facts: the enclosing-statement span and role of
    /// every call expression (`frontend::statements::StatementFacts`).
    ///
    /// Empty unless attached via [`RuleContext::with_frontend`].
    #[must_use]
    pub const fn statement_facts(&self) -> &StatementFacts {
        &self.qualified_calls.statements
    }

    /// Conservative per-file import-derived binding facts with rebind
    /// poisoning (`frontend::statements::BindingFacts`).
    ///
    /// Empty unless attached via [`RuleContext::with_frontend`].
    #[must_use]
    pub const fn binding_facts(&self) -> &BindingFacts {
        &self.qualified_calls.bindings
    }

    /// Lifecycle events and state snapshots from the abstract
    /// interpreter.
    ///
    /// Empty unless attached via [`RuleContext::with_lifecycle`]; an empty
    /// fact set means "not analyzed", never "no scenes exist".
    #[must_use]
    pub const fn lifecycle_facts(&self) -> &LifecycleFacts {
        &self.lifecycle_facts
    }

    /// Symbolic cost facts (hot contexts, frame estimates, evidence).
    ///
    /// Empty unless attached via [`RuleContext::with_cost`]; an empty fact
    /// set means "not analyzed", never "provably cold".
    #[must_use]
    pub const fn cost_facts(&self) -> &CostFacts {
        &self.cost_facts
    }
}
