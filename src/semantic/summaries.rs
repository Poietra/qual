//! Interprocedural effect summaries for project-local helpers
//! (DESIGN §5.7).
//!
//! A [`MethodSummary`] records what one project function or method does to
//! its parameters, its `self` receiver, and the scene, expressed as
//! [`SummaryEffect`]s over [`SummaryOperand`]s. Summaries are computed by
//! running the abstract interpreter's body executor with placeholder
//! objects bound to the parameters (see `semantic::interpreter`), and are
//! applied at call sites by substituting the actual argument values.
//!
//! Summaries are computed bottom-up over the strongly connected components
//! of the project call graph (derived from the qualified call facts and the
//! project index). A recursive SCC iterates to a fixpoint with a bounded
//! iteration count; if it does not converge, only the still-unstable
//! knowledge is widened — the summary keeps the effects that were stable
//! and gains one [`SummaryEffect::UnknownMutation`] over its parameters
//! instead of being discarded wholesale (DESIGN §5.7).
//!
//! Deliberate scope limits (everything outside is an explicit `Unknown`):
//!
//! - A `self.play(...)` inside a helper contributes its *membership*
//!   effects (auto-add, introducer add, remover remove) to the summary;
//!   the full play pipeline is not summarizable (its auto-add certainty
//!   and wait dynamics depend on the caller's state at the call point).
//!   Scene runs therefore *inline* resolvable helper bodies (Scene
//!   methods and project module-level functions alike) instead of
//!   applying their summaries, which materializes helper plays as real
//!   per-call-site `PlayFact`s. The summary path remains for summary runs
//!   and for the recursion / depth-cap fallback; there the summary's
//!   [`SummaryPlay`] records — the play sites with their *syntactic*
//!   facts (literal duration, argument sites, stop condition, frozen
//!   frame) — are rehydrated into conservative maybe-certainty
//!   `PlayFact`s so plays behind the frontier stay visible without
//!   re-running the DESIGN §3.2 pipeline.
//! - Effects on values that are neither parameters, `self`, nor local
//!   allocations become [`SummaryOperand::Opaque`] and are applied as
//!   unknown mutations, never as certain facts.

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::frontend::index::{ProjectIndex, QualifiedCallFacts};
use crate::knowledge::KnowledgeProfile;
use crate::semantic::events::MutationKind;
use crate::semantic::interpreter::{DefMap, FnDef, PlayKind, ReturnFact};
use crate::semantic::state::{AnimationState, CallbackRef, UpdaterFact};
use crate::semantic::values::{AllocationSite, KindSet, Num, Presence};
use crate::source::SourceManager;

/// Maximum fixpoint iterations for one recursive SCC before widening.
pub const MAX_SCC_ITERATIONS: usize = 3;

/// What a summary effect operates on, relative to the summarized callable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SummaryOperand {
    /// The `self` receiver of the summarized method.
    SelfRef,
    /// The declared parameter at this zero-based index (`self` included in
    /// the numbering for methods).
    Param(u32),
    /// An object allocated inside the summarized body at this site.
    Fresh(AllocationSite),
    /// A literal `True` / `False` value (drives literal
    /// `self.always_update_mobjects = ...` tracking through helpers).
    LiteralBool(bool),
    /// A value the summary cannot name (globals, attribute loads, results
    /// of unknown calls). Applying an effect on it never yields a certain
    /// fact.
    Opaque,
}

/// One effect of the summarized callable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SummaryEffect {
    /// A mobject (or animation) allocation inside the body.
    Alloc {
        /// Allocation site inside the summarized body.
        site: AllocationSite,
        /// Candidate classes.
        kind: KindSet,
    },
    /// Objects added to the scene (`Scene.add`, play auto-add, introducer
    /// setup-add, foreground add).
    SceneAdd {
        /// Added objects.
        objects: Vec<SummaryOperand>,
        /// `Scene.add` semantics re-add a present object at the end; the
        /// play auto-add / introducer path only adds absent objects.
        reorders_existing: bool,
        /// Also registers the objects as foreground.
        foreground: bool,
    },
    /// Objects removed from the scene root list (restructuring semantics).
    SceneRemove {
        /// Removed objects.
        objects: Vec<SummaryOperand>,
    },
    /// `parent.add(child)`.
    AddChild {
        /// The structural parent.
        parent: SummaryOperand,
        /// The new submobject.
        child: SummaryOperand,
    },
    /// `parent.remove(child)`.
    RemoveChild {
        /// The structural parent.
        parent: SummaryOperand,
        /// The detached submobject.
        child: SummaryOperand,
    },
    /// An updater registration on a mobject or the scene.
    RegisterUpdater {
        /// Target; [`SummaryOperand::SelfRef`] with `scene_level` set means
        /// `Scene.add_updater`.
        target: SummaryOperand,
        /// Scene-level (`Scene.add_updater`) registration.
        scene_level: bool,
        /// Callback identity and signature facts.
        updater: UpdaterFact,
    },
    /// `remove_updater` with the given callback identity.
    RemoveUpdater {
        /// Target of the removal.
        target: SummaryOperand,
        /// Scene-level removal.
        scene_level: bool,
        /// The callback identity passed at the call site.
        callback: CallbackRef,
    },
    /// `clear_updaters()`.
    ClearUpdaters {
        /// Target whose updaters are cleared.
        target: SummaryOperand,
    },
    /// A write to one fact dimension of the target.
    Mutate {
        /// Mutated object.
        target: SummaryOperand,
        /// Dimension written.
        kind: MutationKind,
    },
    /// `mob.generate_target()`.
    GenerateTarget {
        /// The mobject gaining a target copy.
        target: SummaryOperand,
    },
    /// `mob.save_state()`.
    SaveState {
        /// The mobject saving its state.
        target: SummaryOperand,
    },
    /// An animation constructed inside the body (no scene effect by
    /// itself; DESIGN §15 invariant 4).
    CreateAnimation {
        /// Construction site inside the summarized body.
        site: AllocationSite,
        /// Animation state template; `targets` inside it is empty — the
        /// live targets are carried as operands so application can
        /// substitute the caller's objects.
        state: AnimationState,
        /// Live-target operands.
        targets: Vec<SummaryOperand>,
        /// Replacement target (second argument of Transform-family
        /// constructors), when present.
        replacement_target: Option<SummaryOperand>,
        /// The animation requires `generate_target()` on every path
        /// (`MoveToTarget`).
        requires_target: bool,
        /// The animation requires `save_state()` on every path
        /// (`Restore`).
        requires_saved_state: bool,
    },
    /// `self.<name> = value` on the scene instance: instance attributes
    /// assigned in one lifecycle method are visible in later ones
    /// (`__init__` → `construct` composition).
    SetSelfAttr {
        /// Attribute name.
        name: String,
        /// The assigned value ([`SummaryOperand::Opaque`] for unmodeled
        /// values).
        value: SummaryOperand,
    },
    /// An unresolved call may have mutated these values (DESIGN §5.3).
    UnknownMutation {
        /// The affected values.
        values: Vec<SummaryOperand>,
        /// The scene itself (via `self`) may have been mutated.
        includes_scene: bool,
    },
}

/// One recorded effect with its path certainty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryEvent {
    /// [`Presence::Present`] when the effect happens on every path through
    /// the callable, [`Presence::Maybe`] when only on some paths.
    pub certainty: Presence,
    /// The effect happened inside a loop (fresh allocations are then not
    /// singletons).
    pub in_loop: bool,
    /// The effect happened inside a loop that provably iterates at least
    /// twice.
    pub in_definite_loop: bool,
    /// Source site of the operation inside the summarized body.
    pub site: AllocationSite,
    /// The effect.
    pub effect: SummaryEffect,
}

/// One `play` / `wait` site reached by the summarized callable (directly
/// or through further summarized callees), reduced to its *syntactic*
/// facts.
///
/// A record is deliberately weaker than a traced
/// [`PlayFact`](crate::semantic::interpreter::PlayFact): it keeps only
/// what does not depend on the caller's state at the call point — the
/// call site, play vs wait, the literal-derived duration, the argument
/// expression sites, and the written `stop_condition` / `frozen_frame` /
/// `*args` flags. Everything the DESIGN §3.2 pipeline would judge against
/// live state (auto-add, wait dynamics, suspension, animation identity)
/// is *absent* and must be rehydrated as `Maybe` / `Unknown`, never
/// re-derived. Records exist so recursion and the inline depth cap do not
/// lose plays entirely; each site appears once per kind (executions are
/// unbounded, so per-site multiplicity carries no information).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryPlay {
    /// Source range of the whole `play` / `wait` call inside the helper.
    pub site: AllocationSite,
    /// Play vs wait.
    pub kind: PlayKind,
    /// The duration derivable from literals alone (`run_time` kwargs,
    /// curated constructor literals, literal wait arguments); `Unknown`
    /// whenever any contribution is not a literal.
    pub duration: Num,
    /// Source ranges of the non-starred play arguments (empty for waits).
    pub animation_sites: Vec<AllocationSite>,
    /// A `stop_condition` keyword was written at the call.
    pub has_stop_condition: bool,
    /// Literal `frozen_frame` keyword, when written.
    pub frozen_frame: Option<bool>,
    /// A `*args` splat appeared among the play arguments.
    pub star_args: bool,
    /// Path certainty *within* the summarized body (`Present` = on every
    /// path through the callable). Rehydration composes this downward —
    /// a rehydrated fact is never more certain than `Maybe`.
    pub certainty: Presence,
}

/// What the callable's return value aliases (DESIGN §5.6 `ReturnAlias`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SummaryReturn {
    /// Returns `self` on every return path.
    SelfValue,
    /// Returns the declared parameter at this index on every return path.
    Param(u32),
    /// Returns the allocation made at this site on every return path.
    Fresh(AllocationSite),
    /// Return paths disagree, return an unmodeled value, or the callable
    /// falls off the end.
    Unknown,
}

/// Effect summary of one project callable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodSummary {
    /// Qualified name (`module.helper` / `module.Class.method`).
    pub qualified_name: String,
    /// File that owns the callable. Cached summaries use this stable
    /// source-layout index to shard entries by incremental component.
    pub file: Option<crate::source::FileId>,
    /// Declared parameter names in order (`self` included for methods).
    pub params: Vec<String>,
    /// Effects in body order.
    pub events: Vec<SummaryEvent>,
    /// Play / wait sites reached by the callable (directly or through
    /// summarized callees), one record per site and kind, in body order.
    /// These are syntactic-fact records only; see [`SummaryPlay`].
    pub plays: Vec<SummaryPlay>,
    /// Return-value alias fact.
    pub returns: SummaryReturn,
    /// Return-path classification (all-paths mobject return, bare-return
    /// paths, fall-off-the-end paths; MLC123).
    pub return_fact: ReturnFact,
    /// `false` when the SCC fixpoint did not converge and the unstable
    /// knowledge was widened.
    pub converged: bool,
}

impl MethodSummary {
    /// A summary with nothing known (used as the fixpoint seed).
    #[must_use]
    pub fn seed(
        qualified_name: &str,
        file: Option<crate::source::FileId>,
        params: Vec<String>,
    ) -> Self {
        Self {
            qualified_name: qualified_name.to_owned(),
            file,
            params,
            events: Vec::new(),
            plays: Vec::new(),
            returns: SummaryReturn::Unknown,
            return_fact: ReturnFact::unknown(),
            converged: true,
        }
    }
}

/// All computed summaries, keyed by qualified callable name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SummaryTable {
    /// Summaries keyed by qualified name.
    pub summaries: BTreeMap<String, MethodSummary>,
}

impl SummaryTable {
    /// The summary of `qualified_name`, if one was computed.
    #[must_use]
    pub fn get(&self, qualified_name: &str) -> Option<&MethodSummary> {
        self.summaries.get(qualified_name)
    }

    /// Whether every source handle nested anywhere in these summaries is
    /// owned by `files`. Incremental cache lookup uses this linear typed walk
    /// instead of reserializing a hit through `serde_json::Value`.
    #[must_use]
    pub(crate) fn belongs_to_files(&self, files: &BTreeSet<crate::source::FileId>) -> bool {
        self.summaries
            .values()
            .all(|summary| method_summary_belongs_to(summary, files))
    }
}

fn method_summary_belongs_to(
    summary: &MethodSummary,
    files: &BTreeSet<crate::source::FileId>,
) -> bool {
    summary.file.is_some_and(|file| files.contains(&file))
        && summary.events.iter().all(|event| {
            site_belongs_to(event.site, files) && effect_belongs_to(&event.effect, files)
        })
        && summary.plays.iter().all(|play| {
            site_belongs_to(play.site, files)
                && play
                    .animation_sites
                    .iter()
                    .all(|site| site_belongs_to(*site, files))
        })
        && match summary.returns {
            SummaryReturn::Fresh(site) => site_belongs_to(site, files),
            SummaryReturn::SelfValue | SummaryReturn::Param(_) | SummaryReturn::Unknown => true,
        }
}

fn effect_belongs_to(effect: &SummaryEffect, files: &BTreeSet<crate::source::FileId>) -> bool {
    match effect {
        SummaryEffect::Alloc { site, .. } => site_belongs_to(*site, files),
        SummaryEffect::SceneAdd { objects, .. }
        | SummaryEffect::SceneRemove { objects }
        | SummaryEffect::UnknownMutation {
            values: objects, ..
        } => operands_belong_to(objects, files),
        SummaryEffect::AddChild { parent, child }
        | SummaryEffect::RemoveChild { parent, child } => {
            operand_belongs_to(parent, files) && operand_belongs_to(child, files)
        }
        SummaryEffect::RegisterUpdater {
            target, updater, ..
        } => operand_belongs_to(target, files) && callback_belongs_to(&updater.callback, files),
        SummaryEffect::RemoveUpdater {
            target, callback, ..
        } => operand_belongs_to(target, files) && callback_belongs_to(callback, files),
        SummaryEffect::ClearUpdaters { target }
        | SummaryEffect::Mutate { target, .. }
        | SummaryEffect::GenerateTarget { target }
        | SummaryEffect::SaveState { target } => operand_belongs_to(target, files),
        SummaryEffect::CreateAnimation {
            site,
            state,
            targets,
            replacement_target,
            ..
        } => {
            site_belongs_to(*site, files)
                && state.targets.iter().all(|object| {
                    site_belongs_to(object.site, files)
                        && object
                            .context
                            .frames()
                            .iter()
                            .all(|site| site_belongs_to(*site, files))
                })
                && operands_belong_to(targets, files)
                && replacement_target
                    .as_ref()
                    .is_none_or(|target| operand_belongs_to(target, files))
        }
        SummaryEffect::SetSelfAttr { value, .. } => operand_belongs_to(value, files),
    }
}

fn operands_belong_to(
    operands: &[SummaryOperand],
    files: &BTreeSet<crate::source::FileId>,
) -> bool {
    operands
        .iter()
        .all(|operand| operand_belongs_to(operand, files))
}

fn operand_belongs_to(operand: &SummaryOperand, files: &BTreeSet<crate::source::FileId>) -> bool {
    match operand {
        SummaryOperand::Fresh(site) => site_belongs_to(*site, files),
        SummaryOperand::SelfRef
        | SummaryOperand::Param(_)
        | SummaryOperand::LiteralBool(_)
        | SummaryOperand::Opaque => true,
    }
}

fn callback_belongs_to(callback: &CallbackRef, files: &BTreeSet<crate::source::FileId>) -> bool {
    match callback {
        CallbackRef::Lambda(site) => site_belongs_to(*site, files),
        CallbackRef::Named(_) | CallbackRef::Unknown => true,
    }
}

fn site_belongs_to(site: AllocationSite, files: &BTreeSet<crate::source::FileId>) -> bool {
    files.contains(&site.file)
}

/// Builds summaries for every project callable in the definition map.
///
/// Processing order is bottom-up over the strongly connected components of
/// the call graph so that non-recursive callees are final before their
/// callers are summarized. Independent non-recursive components in one
/// bottom-up layer run in parallel; recursive SCCs iterate sequentially at
/// most [`MAX_SCC_ITERATIONS`] times and then widen only their unstable
/// knowledge.
#[must_use]
pub fn build(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    defs: &DefMap<'_>,
) -> SummaryTable {
    build_with_seed(
        sources,
        index,
        calls,
        knowledge,
        defs,
        None,
        SummaryTable::default(),
    )
}

/// Recomputes summaries whose definitions live in `recompute_files`, while
/// retaining dependency-validated summaries from the other incremental
/// components. Callers must ensure the seed was built under the same cache
/// schema, analyzer build, resolved configuration, knowledge profile, and
/// source layout; the persistent cache key enforces that contract.
#[must_use]
pub fn build_incremental(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    defs: &DefMap<'_>,
    recompute_files: &BTreeSet<crate::source::FileId>,
    seed: SummaryTable,
) -> SummaryTable {
    build_with_seed(
        sources,
        index,
        calls,
        knowledge,
        defs,
        Some(recompute_files),
        seed,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the incremental builder extends the immutable summary inputs with selection and seed"
)]
fn build_with_seed(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    defs: &DefMap<'_>,
    recompute_files: Option<&BTreeSet<crate::source::FileId>>,
    mut table: SummaryTable,
) -> SummaryTable {
    table
        .summaries
        .retain(|name, _| defs.defs.contains_key(name));
    let nodes: Vec<String> = defs
        .defs
        .iter()
        .filter(|(_, def)| recompute_files.is_none_or(|files| files.contains(&def.file)))
        .map(|(name, _)| name.clone())
        .collect();
    for name in &nodes {
        table.summaries.remove(name);
    }
    let node_set: BTreeSet<&str> = nodes.iter().map(String::as_str).collect();
    let edges = call_edges(calls, &node_set);
    let components = strongly_connected_components(&nodes, &edges);
    let dependencies = component_dependencies(&components, &edges);

    let mut remaining: BTreeSet<usize> = (0..components.len()).collect();
    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|component| dependencies[*component].is_disjoint(&remaining))
            .collect();
        assert!(!ready.is_empty(), "SCC condensation graph must be acyclic");

        let independent: Vec<usize> = ready
            .iter()
            .copied()
            .filter(|component| {
                components[*component].len() == 1
                    && !is_self_recursive(&components[*component][0], &edges)
            })
            .collect();
        let computed: Vec<(String, MethodSummary)> = independent
            .par_iter()
            .map(|component| {
                let name = &components[*component][0];
                let summary = crate::semantic::interpreter::summarize_callable(
                    sources, index, calls, knowledge, defs, &table, name,
                );
                (name.clone(), summary)
            })
            .collect();
        for (name, summary) in computed {
            table.summaries.insert(name, summary);
        }
        for component in &independent {
            remaining.remove(component);
        }

        for component in ready {
            if !remaining.remove(&component) {
                continue;
            }
            summarize_recursive_component(
                &components[component],
                sources,
                index,
                calls,
                knowledge,
                defs,
                &mut table,
            );
        }
    }
    table
}

#[allow(
    clippy::too_many_arguments,
    reason = "the recursive SCC uses the same immutable analysis inputs as build"
)]
fn summarize_recursive_component(
    component: &[String],
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
    defs: &DefMap<'_>,
    table: &mut SummaryTable,
) {
    for name in component {
        let params = defs
            .defs
            .get(name)
            .map(FnDef::param_names)
            .unwrap_or_default();
        table.summaries.insert(
            name.clone(),
            MethodSummary::seed(name, Some(defs.defs[name].file), params),
        );
    }
    let mut converged = false;
    for _ in 0..MAX_SCC_ITERATIONS {
        let mut changed = false;
        for name in component {
            let next = crate::semantic::interpreter::summarize_callable(
                sources, index, calls, knowledge, defs, table, name,
            );
            if table.get(name) != Some(&next) {
                changed = true;
                table.summaries.insert(name.clone(), next);
            }
        }
        if !changed {
            converged = true;
            break;
        }
    }
    if converged {
        return;
    }
    // Widen only the unknown part: keep the last iterate's effects and add
    // one unknown mutation over everything reachable through parameters.
    for name in component {
        let Some(def) = defs.defs.get(name) else {
            continue;
        };
        let site = AllocationSite::new(def.file, def.range);
        if let Some(summary) = table.summaries.get_mut(name) {
            summary.converged = false;
            let values: Vec<SummaryOperand> = (0..summary.params.len())
                .map(|position| SummaryOperand::Param(u32::try_from(position).unwrap_or(u32::MAX)))
                .collect();
            summary.events.push(SummaryEvent {
                certainty: Presence::Maybe,
                in_loop: false,
                in_definite_loop: false,
                site,
                effect: SummaryEffect::UnknownMutation {
                    values,
                    includes_scene: true,
                },
            });
            summary.returns = SummaryReturn::Unknown;
            summary.return_fact = ReturnFact::unknown();
        }
    }
}

fn component_dependencies(
    components: &[Vec<String>],
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<BTreeSet<usize>> {
    let mut component_of = BTreeMap::new();
    for (index, component) in components.iter().enumerate() {
        for node in component {
            component_of.insert(node.as_str(), index);
        }
    }
    let mut dependencies = vec![BTreeSet::new(); components.len()];
    for (caller, callees) in edges {
        let Some(&dependent) = component_of.get(caller.as_str()) else {
            continue;
        };
        for callee in callees {
            let Some(&dependency) = component_of.get(callee.as_str()) else {
                continue;
            };
            if dependent != dependency {
                dependencies[dependent].insert(dependency);
            }
        }
    }
    dependencies
}

/// The qualified caller node a call fact belongs to: the outermost
/// project-level def enclosing it (nested `def`s attribute to their
/// enclosing definition for graph purposes).
fn caller_node(
    module: &str,
    class_name: Option<&str>,
    function: &str,
    node_set: &BTreeSet<&str>,
) -> Option<String> {
    let base = class_name.map_or_else(|| module.to_owned(), ToOwned::to_owned);
    let mut path = function.split('.').collect::<Vec<_>>();
    while !path.is_empty() {
        let candidate = format!("{base}.{}", path.join("."));
        if node_set.contains(candidate.as_str()) {
            return Some(candidate);
        }
        path.pop();
    }
    None
}

/// Caller → callee edges over the project callable nodes.
fn call_edges(
    calls: &QualifiedCallFacts,
    node_set: &BTreeSet<&str>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for call in &calls.calls {
        let Some(function) = call.context.function.as_deref() else {
            continue;
        };
        let Some(caller) = caller_node(
            &call.context.module,
            call.context.class_name.as_deref(),
            function,
            node_set,
        ) else {
            continue;
        };
        for candidate in &call.candidates {
            if node_set.contains(candidate.as_str()) {
                edges
                    .entry(caller.clone())
                    .or_default()
                    .insert(candidate.clone());
            }
        }
    }
    edges
}

fn is_self_recursive(node: &str, edges: &BTreeMap<String, BTreeSet<String>>) -> bool {
    edges
        .get(node)
        .is_some_and(|callees| callees.contains(node))
}

/// Deterministic Tarjan SCC over the sorted node list.
///
/// Components are returned in reverse topological order (callees before
/// callers), each with its members sorted.
fn strongly_connected_components(
    nodes: &[String],
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<Vec<String>> {
    struct Tarjan<'a> {
        edges: &'a BTreeMap<String, BTreeSet<String>>,
        indices: BTreeMap<&'a str, usize>,
        lowlinks: BTreeMap<&'a str, usize>,
        on_stack: BTreeSet<&'a str>,
        stack: Vec<&'a str>,
        next_index: usize,
        components: Vec<Vec<String>>,
    }

    impl<'a> Tarjan<'a> {
        fn visit(&mut self, node: &'a str) {
            self.indices.insert(node, self.next_index);
            self.lowlinks.insert(node, self.next_index);
            self.next_index += 1;
            self.stack.push(node);
            self.on_stack.insert(node);

            if let Some(callees) = self.edges.get(node) {
                for callee in callees {
                    let callee = callee.as_str();
                    if !self.indices.contains_key(callee) {
                        self.visit(callee);
                        let low = self.lowlinks[callee].min(self.lowlinks[node]);
                        self.lowlinks.insert(node, low);
                    } else if self.on_stack.contains(callee) {
                        let low = self.indices[callee].min(self.lowlinks[node]);
                        self.lowlinks.insert(node, low);
                    }
                }
            }

            if self.lowlinks[node] == self.indices[node] {
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(member);
                    component.push(member.to_owned());
                    if member == node {
                        break;
                    }
                }
                component.sort();
                self.components.push(component);
            }
        }
    }

    let mut tarjan = Tarjan {
        edges,
        indices: BTreeMap::new(),
        lowlinks: BTreeMap::new(),
        on_stack: BTreeSet::new(),
        stack: Vec::new(),
        next_index: 0,
        components: Vec::new(),
    };
    for node in nodes {
        if !tarjan.indices.contains_key(node.as_str()) {
            tarjan.visit(node);
        }
    }
    tarjan.components
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(pairs: &[(&str, &str)]) -> BTreeMap<String, BTreeSet<String>> {
        let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (from, to) in pairs {
            map.entry((*from).to_owned())
                .or_default()
                .insert((*to).to_owned());
        }
        map
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn scc_orders_callees_before_callers() {
        let nodes = names(&["a", "b", "c"]);
        // a -> b -> c (no recursion).
        let edges = edges(&[("a", "b"), ("b", "c")]);
        let components = strongly_connected_components(&nodes, &edges);
        assert_eq!(
            components,
            vec![names(&["c"]), names(&["b"]), names(&["a"])]
        );
    }

    #[test]
    fn scc_groups_mutual_recursion() {
        let nodes = names(&["a", "b", "c"]);
        let edges = edges(&[("a", "b"), ("b", "a"), ("a", "c")]);
        let components = strongly_connected_components(&nodes, &edges);
        assert_eq!(components, vec![names(&["c"]), names(&["a", "b"])]);
    }

    #[test]
    fn caller_node_trims_nested_paths() {
        let mut set = BTreeSet::new();
        set.insert("m.C.construct");
        set.insert("m.helper");
        assert_eq!(
            caller_node("m", Some("m.C"), "construct.inner", &set),
            Some("m.C.construct".to_owned())
        );
        assert_eq!(
            caller_node("m", None, "helper", &set),
            Some("m.helper".to_owned())
        );
        assert_eq!(caller_node("m", None, "unknown", &set), None);
    }
}
