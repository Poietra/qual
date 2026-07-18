//! Hot-context identification and propagation (DESIGN §4.2).
//!
//! Works over [`QualifiedCallFacts`] and the [`ProjectIndex`] alone — the
//! lifecycle interpreter is not required. A hot context proves only
//! **reachability** from a per-frame entry point; whether the callback
//! actually executes during a given play (registration active, host in
//! the scene family, not suspended, dynamic wait) is the
//! [`super::liveness`] layer, resolved against the lifecycle facts.
//!
//! Recognized per-frame entry points (DESIGN §4.2):
//!
//! - `Mobject.add_updater` / `Scene.add_updater` callbacks,
//! - the `always_redraw` factory (runs once immediately *and* per frame),
//! - `UpdateFromFunc` / `UpdateFromAlphaFunc` update functions,
//! - `Wait(stop_condition=...)` / `Scene.wait(stop_condition=...)`,
//! - `TracedPath(traced_point_func=...)`,
//! - project classes overriding `interpolate` / `interpolate_mobject` /
//!   `interpolate_submobject` of a knowledge Animation base.
//!
//! Entry points are gated on the knowledge profile (`per_frame_callback`
//! effects, curated `stop_condition` signature params); without a profile no
//! call is marked hot — silence over false positives.
//!
//! Hotness propagates transitively through project-local helper calls, but
//! **per call context**: a helper reached from an updater carries a
//! [`HotContext`] whose provenance chain goes through the updater, while the
//! same helper called from `construct` carries none. Callers must therefore
//! treat the *absence* of a context as "cold at this call site", never as a
//! global property of the helper.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rustpython_parser::text_size::TextRange;

use crate::frontend::index::{
    ArgShape, CallArgument, CallContext, LiteralFact, ProjectIndex, QualifiedCall,
    QualifiedCallFacts,
};
use crate::frontend::names::Binding;
use crate::knowledge::{KnowledgeProfile, SymbolEntry, SymbolKind};
use crate::semantic::events::{InvocationContext, Multiplicity};
use crate::semantic::values::{AllocationSite, Num};
use crate::source::{FileId, SourceManager};

/// Maximum provenance-chain depth kept while propagating hotness through
/// helpers; deeper chains are cut off (conservatively dropping hotness for
/// the tail, never inventing it).
pub const MAX_HOT_CHAIN_DEPTH: usize = 8;

/// Bound on the knowledge base-class walk when mapping `Class.method`
/// candidates onto curated ancestor methods.
const MAX_BASE_WALK: usize = 64;

/// Method names whose project-class overrides of a knowledge Animation base
/// run on the per-frame grid (DESIGN §3.2 sample loop).
const INTERPOLATE_OVERRIDES: [&str; 3] = [
    "interpolate",
    "interpolate_mobject",
    "interpolate_submobject",
];

/// Which per-frame entry point roots a hot context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HotEntryKind {
    /// `Mobject.add_updater` callback.
    MobjectUpdater,
    /// `Scene.add_updater` callback.
    SceneUpdater,
    /// `always_redraw` factory (once immediately, then per frame).
    AlwaysRedraw,
    /// `UpdateFromFunc` / `UpdateFromAlphaFunc` update function.
    UpdateFunctionAnimation,
    /// `Wait(stop_condition=...)` / `Scene.wait(stop_condition=...)`.
    StopCondition,
    /// `TracedPath(traced_point_func=...)`.
    TracedPointFunc,
    /// Project override of a knowledge Animation `interpolate*` method.
    AnimationInterpolateOverride,
}

impl HotEntryKind {
    /// Short label used in provenance / `state_path` strings.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::MobjectUpdater => "updater",
            Self::SceneUpdater => "scene-updater",
            Self::AlwaysRedraw => "always_redraw",
            Self::UpdateFunctionAnimation => "update-function",
            Self::StopCondition => "stop-condition",
            Self::TracedPointFunc => "traced-point-func",
            Self::AnimationInterpolateOverride => "interpolate-override",
        }
    }
}

/// One step of a hot-context provenance chain (entry point first, then the
/// helper call sites the hotness flowed through).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceStep {
    /// File the step's source span lives in.
    pub file: FileId,
    /// Byte range of the entry expression or helper call.
    pub range: TextRange,
    /// Human-readable label, e.g. `updater:12` or `call:demo.helper`.
    pub label: String,
    /// Project function id entered by this step, when the step enters one.
    /// Used for cycle detection during propagation.
    pub symbol: Option<String>,
}

/// One hot invocation context of a call site or project function.
#[derive(Debug, Clone, PartialEq)]
pub struct HotContext {
    /// The entry point rooting this context.
    pub entry: HotEntryKind,
    /// The lifecycle phase; always [`InvocationContext::FrameCallback`] for
    /// contexts produced by this module.
    pub context: InvocationContext,
    /// Symbolic invocation-count factors. `frames` is the symbolic
    /// `Symbol("frames")` — never a fabricated number; the estimator binds
    /// it to a real interval only when a literal duration is known.
    pub multiplicity: Multiplicity,
    /// Function enclosing the entry point (e.g. `construct`), if any.
    pub origin: Option<String>,
    /// Source site of the registering expression: the whole
    /// `add_updater` / `always_redraw` / `UpdateFromFunc` / `wait` call
    /// for call-site entries, the overriding `def` for interpolate
    /// overrides. Liveness resolution matches this against the lifecycle
    /// facts (registration sites, play sites, played-animation sites).
    pub entry_call: AllocationSite,
    /// Provenance chain, entry step first.
    pub chain: Vec<ProvenanceStep>,
}

impl HotContext {
    /// The DESIGN §6.3 `state_path` strings: origin function first, then the
    /// chain labels.
    #[must_use]
    pub fn state_path(&self) -> Vec<String> {
        let mut path = Vec::new();
        if let Some(origin) = &self.origin {
            path.push(origin.clone());
        }
        path.extend(self.chain.iter().map(|step| step.label.clone()));
        path
    }
}

/// Hot-context facts for the whole project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HotContextFacts {
    /// Call-fact index (into [`QualifiedCallFacts::calls`]) → the hot
    /// contexts the call runs under, one entry per distinct context chain.
    /// A call absent from the map is not provably hot.
    pub call_contexts: BTreeMap<usize, Vec<HotContext>>,
    /// Project function id → the hot contexts its body runs under. The same
    /// function may also be called from cold sites; those simply contribute
    /// no entry here.
    pub function_contexts: BTreeMap<String, Vec<HotContext>>,
}

impl HotContextFacts {
    /// Hot contexts of one call fact (empty slice when cold / unknown).
    #[must_use]
    pub fn contexts_for_call(&self, call_index: usize) -> &[HotContext] {
        self.call_contexts
            .get(&call_index)
            .map_or(&[], Vec::as_slice)
    }

    /// Whether the call fact is provably reachable from a per-frame entry.
    #[must_use]
    pub fn is_hot(&self, call_index: usize) -> bool {
        self.call_contexts.contains_key(&call_index)
    }
}

/// Resolves a call candidate against the knowledge profile, mapping
/// `Class.method` candidates onto curated ancestor methods via the curated
/// base chain (e.g. `...Square.add_updater` →
/// `manim.mobject.mobject.Mobject.add_updater`).
///
/// Returns the canonical curated id and its entry, or `None` — never a
/// guess.
#[must_use]
pub fn resolve_candidate<'k>(
    knowledge: &'k KnowledgeProfile,
    candidate: &str,
) -> Option<(String, &'k SymbolEntry)> {
    if let Some(entry) = knowledge.symbol(candidate) {
        return Some((candidate.to_owned(), entry));
    }
    let (class_id, member) = candidate.rsplit_once('.')?;
    // Breadth-first over the curated base chain of the class.
    let mut queue: VecDeque<&str> = VecDeque::from([class_id]);
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut steps = 0;
    while let Some(current) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        steps += 1;
        if steps > MAX_BASE_WALK {
            return None;
        }
        let entry = knowledge.symbol(current)?;
        let qualified = format!("{current}.{member}");
        if let Some(method) = knowledge.symbol(&qualified) {
            return Some((qualified, method));
        }
        for base in &entry.bases {
            queue.push_back(base.as_str());
        }
    }
    None
}

/// Builds hot-context facts for the project.
///
/// `knowledge` gates every entry point; `None` yields empty facts (no
/// hotness is ever asserted without curated per-frame semantics).
#[must_use]
pub fn hot_contexts(
    sources: &SourceManager,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    knowledge: Option<&KnowledgeProfile>,
) -> HotContextFacts {
    let Some(knowledge) = knowledge else {
        return HotContextFacts::default();
    };
    let builder = Builder {
        sources,
        index,
        calls,
        knowledge,
        enclosing: calls
            .calls
            .iter()
            .map(|call| enclosing_function_id(&call.context))
            .collect(),
        facts: HotContextFacts::default(),
    };
    builder.build()
}

/// A region of code whose call facts run under one hot context.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Region {
    /// The body of a lambda expression (calls located by range containment).
    Lambda {
        /// File containing the lambda.
        file: FileId,
        /// Byte range of the lambda expression.
        range: TextRange,
    },
    /// The body of a project function or method (calls located by their
    /// enclosing function id).
    Function(String),
}

struct Builder<'a> {
    sources: &'a SourceManager,
    index: &'a ProjectIndex,
    calls: &'a QualifiedCallFacts,
    knowledge: &'a KnowledgeProfile,
    /// Enclosing function id of every call fact, aligned by index.
    enclosing: Vec<Option<String>>,
    facts: HotContextFacts,
}

impl Builder<'_> {
    #[allow(
        clippy::too_many_lines,
        reason = "the entry-point seeding and worklist propagation form one algorithm"
    )]
    fn build(mut self) -> HotContextFacts {
        // The fact inputs are `&'a` fields; hoisting them to locals keeps
        // their borrows independent of the `&mut self` recording calls.
        let calls = self.calls;
        let index = self.index;
        let knowledge = self.knowledge;
        let mut worklist: VecDeque<(Region, HotContext)> = VecDeque::new();

        // 1. Call-site entry points, in deterministic call-fact order.
        for call in &calls.calls {
            for (kind, argument) in entry_callbacks(call, knowledge) {
                let Some(region) = self.resolve_callback(call, argument) else {
                    continue;
                };
                let step = ProvenanceStep {
                    file: call.file,
                    range: argument.range,
                    label: format!(
                        "{}:{}",
                        kind.label(),
                        self.line_of(call.file, argument.range)
                    ),
                    symbol: match &region {
                        Region::Function(id) => Some(id.clone()),
                        Region::Lambda { .. } => None,
                    },
                };
                let context = HotContext {
                    entry: kind,
                    context: InvocationContext::FrameCallback,
                    multiplicity: per_frame_multiplicity(),
                    origin: call.context.function.clone(),
                    entry_call: AllocationSite::new(call.file, call.call_range),
                    chain: vec![step],
                };
                if let Region::Function(id) = &region {
                    self.record_function(id, &context);
                }
                worklist.push_back((region, context));
            }
        }

        // 2. Project overrides of knowledge Animation interpolate methods.
        for class_id in &index.animation_classes {
            let Some(class) = index.classes.get(class_id) else {
                continue;
            };
            let class_file = class.file;
            for method in INTERPOLATE_OVERRIDES {
                let Some(range) = class.methods.get(method).copied() else {
                    continue;
                };
                let function_id = format!("{class_id}.{method}");
                let step = ProvenanceStep {
                    file: class_file,
                    range,
                    label: format!(
                        "{}:{}",
                        HotEntryKind::AnimationInterpolateOverride.label(),
                        self.line_of(class_file, range)
                    ),
                    symbol: Some(function_id.clone()),
                };
                let context = HotContext {
                    entry: HotEntryKind::AnimationInterpolateOverride,
                    context: InvocationContext::FrameCallback,
                    multiplicity: per_frame_multiplicity(),
                    origin: None,
                    entry_call: AllocationSite::new(class_file, range),
                    chain: vec![step],
                };
                self.record_function(&function_id, &context);
                worklist.push_back((Region::Function(function_id), context));
            }
        }

        // 3. Transitive propagation through project-local helper calls,
        //    keeping one context per distinct provenance chain.
        while let Some((region, context)) = worklist.pop_front() {
            for call_index in self.region_members(&region) {
                if !self.record_call(call_index, &context) {
                    continue;
                }
                if context.chain.len() >= MAX_HOT_CHAIN_DEPTH {
                    continue;
                }
                let call = &calls.calls[call_index];
                for candidate in &call.candidates {
                    if !index.function_signatures.contains_key(candidate) {
                        continue;
                    }
                    // Cycle: this chain already went through the callee.
                    if context
                        .chain
                        .iter()
                        .any(|step| step.symbol.as_deref() == Some(candidate))
                    {
                        continue;
                    }
                    let mut child = context.clone();
                    child.chain.push(ProvenanceStep {
                        file: call.file,
                        range: call.call_range,
                        label: format!("call:{candidate}"),
                        symbol: Some(candidate.clone()),
                    });
                    if self.record_function(candidate, &child) {
                        worklist.push_back((Region::Function(candidate.clone()), child));
                    }
                }
            }
        }

        self.facts
    }

    /// Call-fact indices belonging to a region, ascending.
    fn region_members(&self, region: &Region) -> Vec<usize> {
        match region {
            Region::Lambda { file, range } => self
                .calls
                .calls
                .iter()
                .enumerate()
                .filter(|(_, call)| {
                    call.file == *file
                        && call.call_range.start() >= range.start()
                        && call.call_range.end() <= range.end()
                })
                .map(|(call_index, _)| call_index)
                .collect(),
            Region::Function(id) => self
                .enclosing
                .iter()
                .enumerate()
                .filter(|(_, enclosing)| enclosing.as_deref() == Some(id.as_str()))
                .map(|(call_index, _)| call_index)
                .collect(),
        }
    }

    /// Records a context on a call; `false` when the identical chain was
    /// already present (dedup keeps propagation finite).
    fn record_call(&mut self, call_index: usize, context: &HotContext) -> bool {
        let contexts = self.facts.call_contexts.entry(call_index).or_default();
        if contexts.contains(context) {
            return false;
        }
        contexts.push(context.clone());
        true
    }

    /// Records a context on a project function; `false` on duplicates.
    fn record_function(&mut self, function_id: &str, context: &HotContext) -> bool {
        let contexts = self
            .facts
            .function_contexts
            .entry(function_id.to_owned())
            .or_default();
        if contexts.contains(context) {
            return false;
        }
        contexts.push(context.clone());
        true
    }

    /// Resolves a callback argument to the region whose calls become hot.
    ///
    /// Resolution never guesses: an unresolvable callback contributes no
    /// hotness (silence over false positives).
    fn resolve_callback(&self, call: &QualifiedCall, argument: &CallArgument) -> Option<Region> {
        match &argument.shape {
            ArgShape::Lambda => Some(Region::Lambda {
                file: call.file,
                range: argument.range,
            }),
            ArgShape::Name => {
                let name = self.sources.file(call.file).slice(argument.range);
                let module = self.index.modules.get(&call.context.module)?;
                let Some(Binding::LocalFunction(id)) = module.namespace.get(name) else {
                    return None;
                };
                // The module-level binding may be shadowed at the call site;
                // require the locally resolved signature to match the def
                // the name would reach, otherwise stay silent.
                let declared = self.index.function_signature(id)?;
                if argument.callable_signature.as_ref() != Some(declared) {
                    return None;
                }
                Some(Region::Function(id.clone()))
            }
            ArgShape::Attribute(attribute) => attribute
                .candidates
                .iter()
                .find(|candidate| self.index.function_signatures.contains_key(*candidate))
                .map(|candidate| Region::Function(candidate.clone())),
            ArgShape::Call(_) | ArgShape::Literal | ArgShape::Other => None,
        }
    }

    /// 1-based line of a byte range start (for provenance labels).
    fn line_of(&self, file: FileId, range: TextRange) -> usize {
        self.sources
            .file(file)
            .position_of_byte(usize::from(range.start()))
            .line
    }
}

/// The per-frame base multiplicity: every factor neutral except `frames`,
/// which is the symbolic `frames` quantity (DESIGN §4.2: when the duration
/// is unknown, report "per-frame" — never a fabricated invocation count).
fn per_frame_multiplicity() -> Multiplicity {
    let mut multiplicity = Multiplicity::one();
    multiplicity.frames = Num::Symbol("frames".to_owned());
    multiplicity
}

/// Qualified id of the function whose body directly contains a call.
///
/// Nested (closure) function paths keep their dotted form and therefore
/// never collide with the module-level / method ids used for propagation.
fn enclosing_function_id(context: &CallContext) -> Option<String> {
    let function = context.function.as_ref()?;
    match &context.class_name {
        Some(class) => Some(format!("{class}.{function}")),
        None => Some(format!("{}.{function}", context.module)),
    }
}

/// Entry-point callbacks of one call, gated on the knowledge profile.
fn entry_callbacks<'c>(
    call: &'c QualifiedCall,
    knowledge: &KnowledgeProfile,
) -> Vec<(HotEntryKind, &'c CallArgument)> {
    let mut entries: Vec<(HotEntryKind, &'c CallArgument)> = Vec::new();
    let mut seen: BTreeSet<(HotEntryKind, u32, u32)> = BTreeSet::new();
    let mut push = |kind: HotEntryKind, argument: &'c CallArgument| {
        let key = (
            kind,
            argument.range.start().into(),
            argument.range.end().into(),
        );
        if seen.insert(key) {
            entries.push((kind, argument));
        }
    };
    for candidate in &call.candidates {
        let Some((canonical, entry)) = resolve_candidate(knowledge, candidate) else {
            continue;
        };
        if entry
            .effects
            .as_ref()
            .and_then(|effects| effects.per_frame_callback)
            == Some(true)
        {
            match entry.kind {
                SymbolKind::Method => {
                    let kind = if canonical.starts_with("manim.scene.") {
                        HotEntryKind::SceneUpdater
                    } else {
                        HotEntryKind::MobjectUpdater
                    };
                    let named = entry
                        .signature
                        .as_ref()
                        .and_then(|signature| signature.params.first())
                        .and_then(|param| call.keyword(&param.name));
                    if let Some(argument) = named.or_else(|| positional(call, 0)) {
                        push(kind, argument);
                    }
                }
                SymbolKind::Function => {
                    if let Some(argument) = positional(call, 0) {
                        push(HotEntryKind::AlwaysRedraw, argument);
                    }
                }
                SymbolKind::Animation => {
                    let argument = call
                        .keyword("update_function")
                        .or_else(|| positional(call, 1));
                    if let Some(argument) = argument {
                        push(HotEntryKind::UpdateFunctionAnimation, argument);
                    }
                }
                SymbolKind::Mobject | SymbolKind::Vmobject => {
                    let argument = call
                        .keyword("traced_point_func")
                        .or_else(|| positional(call, 0));
                    if let Some(argument) = argument {
                        push(HotEntryKind::TracedPointFunc, argument);
                    }
                }
                SymbolKind::Scene | SymbolKind::Camera | SymbolKind::Constant => {}
            }
        }
        // `stop_condition` is a separate contract: the parameter is curated
        // on `Wait` / `Scene.wait` without a `per_frame_callback` effect.
        if has_curated_param(entry, "stop_condition") {
            if let Some(argument) = call.keyword("stop_condition") {
                if !matches!(argument.literal, Some(LiteralFact::NoneLit)) {
                    push(HotEntryKind::StopCondition, argument);
                }
            }
        }
    }
    entries
}

/// The positional argument at `index`, unless a `*args` splat makes
/// positions unreliable (count-based reads must then be skipped).
fn positional(call: &QualifiedCall, index: usize) -> Option<&CallArgument> {
    if call.has_star_args {
        return None;
    }
    call.positional(index)
}

/// Whether the curated signature declares a parameter named `name`.
fn has_curated_param(entry: &SymbolEntry, name: &str) -> bool {
    entry
        .signature
        .as_ref()
        .is_some_and(|signature| signature.params.iter().any(|param| param.name == name))
}
