//! Symbolic cost model (DESIGN §4): dimensions, hot contexts, estimation.
//!
//! - [`model`]: the §4.1 dimension vocabulary and per-operation cost facts;
//! - [`contexts`]: §4.2 hot-context identification and transitive
//!   propagation over the qualified-call facts;
//! - [`estimator`]: §4.3-§4.4 frame estimation, severity scoring, and the
//!   §6.3 evidence JSON builder.
//!
//! [`CostFacts`] aggregates everything for the rule layer. The query
//! surface the `MLP2xx` rules consume:
//!
//! - [`CostFacts::is_call_in_hot_context`] / [`CostFacts::hot_contexts_for`]
//!   — whether a call fact provably runs per frame, with provenance;
//! - [`CostFacts::frames_for_play`] — the symbolic / interval frame count of
//!   a recognized `play` / `wait` call;
//! - [`CostFacts::evidence_for`] — the DESIGN §6.3 evidence JSON of a call;
//! - [`CostFacts::constructions_in_hot_contexts`] — hot mobject
//!   constructions (`MLP201` / `MLP226` consumers);
//! - [`CostFacts::scene_graph_mutations_in_hot_contexts`] — hot Scene
//!   membership mutations (`MLP204`);
//! - [`CostFacts::frame_varying_resource_keys`] — Text / TeX constructions
//!   whose cache key varies per frame, `K_resource ≈ F` (`MLP226`).
//!
//! Invariants (DESIGN §15): unknown values never collapse to `1`, no
//! fabricated frame counts, and every hotness claim is gated on curated
//! knowledge — absent facts mean silence, not a diagnostic.

pub mod contexts;
pub mod estimator;
pub mod model;

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::model::RenderProfile;
use crate::frontend::index::{ArgShape, CallArgument, ProjectIndex, QualifiedCallFacts};
use crate::knowledge::{KnowledgeProfile, SceneMembershipEffect, SymbolKind};
use crate::semantic::values::Num;
use crate::source::{FileId, SourceManager};

use contexts::{HotContext, HotContextFacts, hot_contexts, resolve_candidate};
use estimator::{Evidence, frames_across_profiles, play_run_time, symbolic_frames, wait_duration};
use model::{CostDimension, CostFact, InvocationContext, Multiplicity, OperationKind};

/// Canonical ids whose calls run the play lifecycle with a duration.
const SCENE_PLAY: &str = "manim.scene.scene.Scene.play";
const SCENE_WAIT: &str = "manim.scene.scene.Scene.wait";
const WAIT_ANIMATION: &str = "manim.animation.animation.Wait";

/// Canonical ids of constructors whose cache key is a Text / TeX / SVG /
/// Image resource key (`K_resource` tracking, DESIGN §4.1). Only ids also
/// present in the loaded knowledge profile are honored.
const RESOURCE_CONSTRUCTORS: [&str; 7] = [
    "manim.mobject.svg.svg_mobject.SVGMobject",
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
    "manim.mobject.text.text_mobject.MarkupText",
    "manim.mobject.text.text_mobject.Text",
    "manim.mobject.types.image_mobject.ImageMobject",
];

/// Canonical ids whose construction launches an external compiler on a
/// cache miss (`OperationKind::ExternalProcess`, DESIGN §4.2).
const TEX_CONSTRUCTORS: [&str; 3] = [
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
];

/// A mobject construction reachable from a per-frame entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct HotConstruction {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the constructed class.
    pub symbol: String,
    /// The per-operation cost fact of the construction.
    pub cost: CostFact,
}

/// A Scene-membership mutation reachable from a per-frame entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct HotSceneMutation {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the mutating Scene API method.
    pub symbol: String,
    /// The curated membership effect.
    pub effect: SceneMembershipEffect,
    /// Whether an argument is itself a fresh mobject construction — the
    /// growth pattern `MLP204` treats as `O(F)` family growth. `false`
    /// means "not proven", never "proven absent".
    pub adds_fresh_allocation: bool,
}

/// A hot Text / TeX construction whose cache key provably varies per frame
/// (an f-string key), so `K_resource ≈ F` (DESIGN §4.2, `MLP226`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceKeyFact {
    /// Index into [`QualifiedCallFacts::calls`].
    pub call_index: usize,
    /// Canonical id of the constructed resource class.
    pub symbol: String,
    /// Estimated distinct-key count (the symbolic `frames` quantity until a
    /// literal duration binds it).
    pub keys: Num,
}

/// Aggregated symbolic cost facts of the analyzed project.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostFacts {
    /// Hot-context facts (per call site and per project function).
    pub hot: HotContextFacts,
    /// Per-operation cost facts of hot calls, keyed by call-fact index.
    pub call_costs: BTreeMap<usize, Vec<CostFact>>,
    /// Frame-count estimates of recognized `play` / `wait` calls, keyed by
    /// call-fact index: an interval when a literal duration is known, the
    /// symbolic `frames` quantity otherwise.
    pub frames_by_call: BTreeMap<usize, Num>,
    /// Hot mobject constructions, ascending by call index.
    pub hot_constructions: Vec<HotConstruction>,
    /// Hot Scene-membership mutations, ascending by call index.
    pub hot_scene_mutations: Vec<HotSceneMutation>,
    /// Frame-varying resource keys, ascending by call index.
    pub frame_varying_resources: Vec<ResourceKeyFact>,
    /// Render profiles analyzed in this run (frame rates, renderers,
    /// resolutions for the pixel helpers).
    pub profiles: Vec<RenderProfile>,
}

impl CostFacts {
    /// Builds the cost facts for the whole project.
    ///
    /// Without a knowledge profile every collection stays empty — no
    /// hotness or cost claim is ever made from name strings alone.
    #[must_use]
    pub fn compute(
        sources: &SourceManager,
        index: &ProjectIndex,
        calls: &QualifiedCallFacts,
        knowledge: Option<&KnowledgeProfile>,
        profiles: &[RenderProfile],
    ) -> Self {
        let mut facts = Self {
            hot: hot_contexts(sources, index, calls, knowledge),
            profiles: profiles.to_vec(),
            ..Self::default()
        };
        let Some(knowledge) = knowledge else {
            return facts;
        };
        facts.collect_play_frames(calls, knowledge);
        facts.collect_hot_call_costs(sources, calls, knowledge);
        facts
    }

    /// The first (deterministically ordered) hot context of a call fact, or
    /// `None` when the call is not provably hot.
    #[must_use]
    pub fn is_call_in_hot_context(&self, call_index: usize) -> Option<&HotContext> {
        self.hot.contexts_for_call(call_index).first()
    }

    /// Every hot context of a call fact (empty when cold / unknown).
    #[must_use]
    pub fn hot_contexts_for(&self, call_index: usize) -> &[HotContext] {
        self.hot.contexts_for_call(call_index)
    }

    /// Frame-count estimate of a recognized `play` / `wait` call fact.
    ///
    /// An interval when a literal duration bound it, the symbolic `frames`
    /// quantity when the duration is unknown, and [`Num::Unknown`] when the
    /// call is not a recognized play at all.
    #[must_use]
    pub fn frames_for_play(&self, call_index: usize) -> Num {
        self.frames_by_call
            .get(&call_index)
            .cloned()
            .unwrap_or(Num::Unknown)
    }

    /// The DESIGN §6.3 evidence JSON for a call fact. Unknown facts are
    /// `null`, never fabricated numbers.
    #[must_use]
    pub fn evidence_for(&self, call_index: usize) -> Value {
        let hot = self.is_call_in_hot_context(call_index);
        let frames = self
            .frames_by_call
            .get(&call_index)
            .cloned()
            .or_else(|| hot.map(|context| context.multiplicity.frames.clone()));
        let evidence = Evidence {
            invocation_context: hot.map(|context| context.context),
            multiplicity: hot
                .map(|context| estimator::multiplicity_factor_names(&context.multiplicity))
                .unwrap_or_default(),
            frames,
            family_size: None,
            renderers: estimator::renderers_of(&self.profiles),
            state_path: hot.map(HotContext::state_path).unwrap_or_default(),
        };
        evidence.to_json()
    }

    /// Hot mobject constructions (`MLP201` / `MLP226` consumers).
    pub fn constructions_in_hot_contexts(&self) -> impl Iterator<Item = &HotConstruction> {
        self.hot_constructions.iter()
    }

    /// Hot Scene-membership mutations (`MLP204` consumer).
    pub fn scene_graph_mutations_in_hot_contexts(&self) -> impl Iterator<Item = &HotSceneMutation> {
        self.hot_scene_mutations.iter()
    }

    /// Frame-varying Text / TeX resource keys (`MLP226` consumer).
    pub fn frame_varying_resource_keys(&self) -> impl Iterator<Item = &ResourceKeyFact> {
        self.frame_varying_resources.iter()
    }

    /// Records frame estimates for every recognized play / wait call.
    fn collect_play_frames(&mut self, calls: &QualifiedCallFacts, knowledge: &KnowledgeProfile) {
        for (call_index, call) in calls.calls.iter().enumerate() {
            let mut duration: Option<Num> = None;
            for candidate in &call.candidates {
                let Some((canonical, _)) = resolve_candidate(knowledge, candidate) else {
                    continue;
                };
                let candidate_duration = match canonical.as_str() {
                    SCENE_PLAY => play_run_time(call, calls),
                    SCENE_WAIT => wait_duration(call, &["duration"]),
                    WAIT_ANIMATION => wait_duration(call, &["run_time"]),
                    _ => continue,
                };
                duration = Some(match duration {
                    None => candidate_duration,
                    // Ambiguous candidates: hull over both interpretations.
                    Some(current) => current.join(&candidate_duration),
                });
            }
            if let Some(duration) = duration {
                self.frames_by_call.insert(
                    call_index,
                    frames_across_profiles(&duration, &self.profiles),
                );
            }
        }
    }

    /// Derives per-operation cost facts for every hot call.
    fn collect_hot_call_costs(
        &mut self,
        sources: &SourceManager,
        calls: &QualifiedCallFacts,
        knowledge: &KnowledgeProfile,
    ) {
        let hot_indices: Vec<usize> = self.hot.call_contexts.keys().copied().collect();
        for call_index in hot_indices {
            let call = &calls.calls[call_index];
            let multiplicity = self.joined_multiplicity(call_index);
            let context = self
                .is_call_in_hot_context(call_index)
                .map_or(InvocationContext::Unknown, |hot| hot.context);
            for candidate in &call.candidates {
                let Some((canonical, entry)) = resolve_candidate(knowledge, candidate) else {
                    continue;
                };
                match entry.kind {
                    SymbolKind::Mobject | SymbolKind::Vmobject => {
                        self.record_hot_construction(
                            sources,
                            calls,
                            call_index,
                            &canonical,
                            context,
                            &multiplicity,
                        );
                    }
                    SymbolKind::Method => {
                        let mutation = entry
                            .effects
                            .as_ref()
                            .and_then(|effects| effects.scene_membership)
                            .filter(|effect| is_graph_mutation(*effect));
                        if let Some(effect) = mutation {
                            self.record_hot_mutation(
                                calls,
                                knowledge,
                                call_index,
                                &canonical,
                                effect,
                                context,
                                &multiplicity,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Records one hot mobject construction (plus TeX external-process and
    /// frame-varying key facts where they apply).
    fn record_hot_construction(
        &mut self,
        sources: &SourceManager,
        calls: &QualifiedCallFacts,
        call_index: usize,
        canonical: &str,
        context: InvocationContext,
        multiplicity: &Multiplicity,
    ) {
        if self
            .hot_constructions
            .iter()
            .any(|existing| existing.call_index == call_index && existing.symbol == canonical)
        {
            return;
        }
        let call = &calls.calls[call_index];
        let frame_varying_key = RESOURCE_CONSTRUCTORS.contains(&canonical)
            && key_argument_is_fstring(sources, call.file, call.positional(0));
        let cost = CostFact::new(OperationKind::Construction, context)
            .with_multiplicity(multiplicity.clone());
        self.push_cost(call_index, cost.clone());
        self.hot_constructions.push(HotConstruction {
            call_index,
            symbol: canonical.to_owned(),
            cost,
        });
        if TEX_CONSTRUCTORS.contains(&canonical) {
            // A cache miss launches the TeX compiler; how many distinct
            // keys occur is unknown unless the key provably varies per
            // frame. Unknown stays unknown — never assumed to be 1.
            let keys = if frame_varying_key {
                symbolic_frames()
            } else {
                Num::Unknown
            };
            let mut external_multiplicity = multiplicity.clone();
            external_multiplicity.distinct_resource_keys = keys.clone();
            let external = CostFact::new(OperationKind::ExternalProcess, context)
                .with_dimension(CostDimension::ResourceKeys, keys)
                .with_multiplicity(external_multiplicity);
            self.push_cost(call_index, external);
        }
        if frame_varying_key {
            self.frame_varying_resources.push(ResourceKeyFact {
                call_index,
                symbol: canonical.to_owned(),
                keys: symbolic_frames(),
            });
        }
    }

    /// Records one hot Scene-membership mutation.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal builder step; grouping into a struct adds no clarity"
    )]
    fn record_hot_mutation(
        &mut self,
        calls: &QualifiedCallFacts,
        knowledge: &KnowledgeProfile,
        call_index: usize,
        canonical: &str,
        effect: SceneMembershipEffect,
        context: InvocationContext,
        multiplicity: &Multiplicity,
    ) {
        if self
            .hot_scene_mutations
            .iter()
            .any(|existing| existing.call_index == call_index && existing.symbol == canonical)
        {
            return;
        }
        let call = &calls.calls[call_index];
        let adds_fresh_allocation = call.arguments.iter().any(|argument| {
            let ArgShape::Call(child_index) = argument.shape else {
                return false;
            };
            calls.calls.get(child_index).is_some_and(|child| {
                child.candidates.iter().any(|candidate| {
                    resolve_candidate(knowledge, candidate).is_some_and(|(_, entry)| {
                        matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                    })
                })
            })
        });
        let cost = CostFact::new(
            OperationKind::Other("scene-graph-mutation".to_owned()),
            context,
        )
        .with_multiplicity(multiplicity.clone());
        self.push_cost(call_index, cost);
        self.hot_scene_mutations.push(HotSceneMutation {
            call_index,
            symbol: canonical.to_owned(),
            effect,
            adds_fresh_allocation,
        });
    }

    /// Factor-wise join of every hot context multiplicity of a call.
    fn joined_multiplicity(&self, call_index: usize) -> Multiplicity {
        let contexts = self.hot.contexts_for_call(call_index);
        let mut joined: Option<Multiplicity> = None;
        for context in contexts {
            joined = Some(match joined {
                None => context.multiplicity.clone(),
                Some(current) => current.join(&context.multiplicity),
            });
        }
        joined.unwrap_or_default()
    }

    fn push_cost(&mut self, call_index: usize, cost: CostFact) {
        self.call_costs.entry(call_index).or_default().push(cost);
    }
}

/// Whether a curated membership effect mutates the scene graph in the
/// `MLP204` sense (adds / removes / replaces / reorders members).
const fn is_graph_mutation(effect: SceneMembershipEffect) -> bool {
    matches!(
        effect,
        SceneMembershipEffect::Add
            | SceneMembershipEffect::Remove
            | SceneMembershipEffect::Replace
            | SceneMembershipEffect::ReorderToFront
            | SceneMembershipEffect::ReorderToBack
            | SceneMembershipEffect::Clear
            | SceneMembershipEffect::AddForeground
            | SceneMembershipEffect::RemoveForeground
            | SceneMembershipEffect::AddFixedInFrame
            | SceneMembershipEffect::AddFixedOrientation
            | SceneMembershipEffect::RemoveFixedInFrame
            | SceneMembershipEffect::RemoveFixedOrientation
    )
}

/// Whether the resource-key argument is an f-string literal (a
/// frame-varying cache key candidate). Detection uses the lexer's string
/// kind, never a source-text heuristic; anything unresolvable is `false`
/// (not varying is never asserted — this is only the *proven varying*
/// gate).
fn key_argument_is_fstring(
    sources: &SourceManager,
    file: FileId,
    argument: Option<&CallArgument>,
) -> bool {
    let Some(argument) = argument else {
        return false;
    };
    // A fully static string produces a literal fact; only a Literal-shaped
    // argument without one can be an f-string.
    if !matches!(argument.shape, ArgShape::Literal) || argument.literal.is_some() {
        return false;
    }
    sources.file(file).tokens().iter().any(|(token, range)| {
        range.start() >= argument.range.start()
            && range.end() <= argument.range.end()
            && matches!(
                token,
                rustpython_parser::Tok::String { kind, .. } if kind.is_any_fstring()
            )
    })
}
