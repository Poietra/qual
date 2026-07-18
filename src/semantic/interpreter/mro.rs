//! Scene composition along the Python MRO (DESIGN §5.1 step 5): C3
//! linearization over project classes, `super()` dispatch, the
//! `__init__ → setup → construct → tear_down` lifecycle run per scene,
//! the camera-contract verdict, and the op → public event conversion.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast;

use crate::frontend::index::{ClassRecord, ProjectIndex, QualifiedCall};
use crate::semantic::events::{self, Event, InvocationContext, MutationKind};
use crate::semantic::heap::AbstractHeap;
use crate::semantic::state::SceneState;
use crate::semantic::values::{
    AllocationSite, CallContextId, Cardinality, KindSet, ObjectId, Presence, Truth,
};

use super::dispatch::Ctx;
use super::exec::{
    AbstractValue, BlockCtx, ExecState, Machine, OpKind, SinkOp, TraceSink, literal_bool,
};
use super::{CameraKind, SceneLifecycle, TracedEvent};

/// `super().<method>(...)` detection.
pub(super) fn super_call_method(call: &ast::ExprCall) -> Option<&str> {
    let ast::Expr::Attribute(attribute) = call.func.as_ref() else {
        return None;
    };
    let ast::Expr::Call(inner) = attribute.value.as_ref() else {
        return None;
    };
    let ast::Expr::Name(name) = inner.func.as_ref() else {
        return None;
    };
    (name.id.as_str() == "super" && inner.args.is_empty()).then(|| attribute.attr.as_str())
}

impl<'a> Machine<'a, '_> {
    // -- super() dispatch ---------------------------------------------------

    pub(super) fn dispatch_super(
        &mut self,
        method: &str,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        if method == self.current_method {
            state.super_called = Presence::Present;
        }
        let Some(current) = self.current_class.clone() else {
            self.eval_call_args(call, state);
            return AbstractValue::Unknown;
        };
        let start = self
            .mro
            .iter()
            .position(|class| class == &current)
            .map_or(0, |position| position + 1);
        let mro = self.mro.clone();
        for class in &mro[start.min(mro.len())..] {
            let qualified = format!("{class}.{method}");
            if self.ctx.defs.defs.contains_key(&qualified) {
                return self.call_scene_helper(&qualified, call, fact, state);
            }
        }
        // External base: curated effect if any, else the empty base
        // implementation (`Scene.__init__` / `setup` / `tear_down`).
        if method == "__init__" && matches!(state.env.get("self"), Some(AbstractValue::SelfScene)) {
            // `super().__init__(always_update_mobjects=...)` reaching the
            // external Scene constructor: a literal kwarg is an exact
            // SceneState fact, anything non-literal is Maybe (MLP227).
            if let Some(fact) = fact {
                if let Some(argument) = fact.keyword("always_update_mobjects") {
                    let tracked = literal_bool(argument).map_or(Truth::Maybe, Truth::from);
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.always_update_mobjects = tracked;
                    }
                } else if fact.has_star_star_kwargs {
                    if let Some(scene) = self.scene_state_mut(state) {
                        scene.always_update_mobjects =
                            scene.always_update_mobjects.join(Truth::Maybe);
                    }
                }
            }
        }
        let resolved = self
            .reached_bases
            .iter()
            .find_map(|base| self.ctx.resolve_method(base, method));
        if let Some((canonical, entry)) = resolved {
            return self.apply_scene_effect(&canonical, entry, call, fact, state);
        }
        self.eval_call_args(call, state);
        AbstractValue::Unknown
    }
}

// ---------------------------------------------------------------------------
// Op → public event conversion.
// ---------------------------------------------------------------------------

fn op_to_event(op: &SinkOp, scene_id: &ObjectId) -> Option<Event> {
    match &op.op {
        OpKind::Alloc { object, kind } => Some(Event::Alloc(events::Alloc {
            object: object.clone(),
            kind: kind.clone(),
        })),
        OpKind::SceneAdd {
            objects,
            order_effect,
            ..
        } => Some(Event::SceneAdd(events::SceneAdd {
            scene: scene_id.clone(),
            objects: objects.clone(),
            order_effect: *order_effect,
        })),
        OpKind::SceneRemove { objects } => Some(Event::SceneRemove(events::SceneRemove {
            scene: scene_id.clone(),
            objects: objects.clone(),
        })),
        OpKind::AddChild { parent, child } => Some(Event::AddChild(events::AddChild {
            parent: parent.clone(),
            child: child.clone(),
        })),
        OpKind::RemoveChild { parent, .. } => Some(Event::Mutate(events::Mutate {
            target: parent.clone(),
            kind: MutationKind::Membership,
        })),
        OpKind::RegisterUpdater {
            target, updater, ..
        } => Some(Event::RegisterUpdater(events::RegisterUpdater {
            target: target.clone().unwrap_or_else(|| scene_id.clone()),
            updater: updater.clone(),
        })),
        OpKind::RemoveUpdater { target, .. } => Some(Event::Mutate(events::Mutate {
            target: target.clone().unwrap_or_else(|| scene_id.clone()),
            kind: MutationKind::Updaters,
        })),
        OpKind::ClearUpdaters { target } => Some(Event::Mutate(events::Mutate {
            target: target.clone(),
            kind: MutationKind::Updaters,
        })),
        OpKind::Mutate { target, kind } => Some(Event::Mutate(events::Mutate {
            target: target.clone(),
            kind: *kind,
        })),
        OpKind::GenerateTarget { copy, .. } => Some(Event::Alloc(events::Alloc {
            object: copy.clone(),
            kind: KindSet::Unknown,
        })),
        OpKind::SaveState { .. } | OpKind::SetSelfAttr { .. } => None,
        OpKind::CreateAnimation {
            animation, state, ..
        } => Some(Event::CreateAnimation(events::CreateAnimation {
            animation: animation.clone(),
            state: state.clone(),
        })),
        OpKind::BeginPlay {
            play_group,
            animations,
            duration,
        } => Some(Event::BeginPlay(events::BeginPlay {
            play_group: *play_group,
            animations: animations.clone(),
            duration: duration.clone(),
        })),
        OpKind::SuspendUpdater { target } => Some(Event::SuspendUpdater(events::SuspendUpdater {
            target: target.clone(),
        })),
        OpKind::ResumeUpdater { target } => Some(Event::ResumeUpdater(events::ResumeUpdater {
            target: target.clone(),
        })),
        OpKind::FinishPlay {
            play_group,
            cleanup,
        } => Some(Event::FinishPlay(events::FinishPlay {
            play_group: *play_group,
            cleanup: cleanup.clone(),
        })),
        OpKind::UnknownMutation { values, .. } => {
            Some(Event::UnknownMutation(events::UnknownMutation {
                values: values.clone(),
            }))
        }
        OpKind::RendererDivergentMembership => {
            Some(Event::RendererRequirement(events::RendererRequirement {
                kind: events::RendererRequirementKind::DivergesBetweenRenderers,
                note: "fixed-object removal membership diverges between renderers".to_owned(),
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// MRO linearization (C3 over project classes).
// ---------------------------------------------------------------------------

pub(super) fn linearize_project(index: &ProjectIndex, class_id: &str) -> (Vec<String>, bool) {
    fn linearize(
        index: &ProjectIndex,
        id: &str,
        visiting: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        let Some(record) = index.classes.get(id) else {
            // External terminal.
            return Some(vec![id.to_owned()]);
        };
        if !visiting.insert(id.to_owned()) {
            return None;
        }
        let mut parents = Vec::new();
        for base in &record.bases {
            match base {
                crate::frontend::index::BaseRef::Resolved(base_id) => {
                    parents.push(base_id.clone());
                }
                crate::frontend::index::BaseRef::Unresolved(_) => {
                    visiting.remove(id);
                    return None;
                }
            }
        }
        let mut sequences: Vec<Vec<String>> = Vec::new();
        for parent in &parents {
            sequences.push(linearize(index, parent, visiting)?);
        }
        if !parents.is_empty() {
            sequences.push(parents);
        }
        visiting.remove(id);
        let merged = c3_merge(sequences)?;
        let mut result = vec![id.to_owned()];
        result.extend(merged);
        Some(result)
    }

    let mut visiting = BTreeSet::new();
    match linearize(index, class_id, &mut visiting) {
        Some(full) => {
            let project: Vec<String> = full
                .into_iter()
                .filter(|id| index.classes.contains_key(id))
                .collect();
            (project, false)
        }
        None => (vec![class_id.to_owned()], true),
    }
}

/// Standard C3 merge; `None` when no consistent linearization exists.
fn c3_merge(mut sequences: Vec<Vec<String>>) -> Option<Vec<String>> {
    let mut result = Vec::new();
    loop {
        sequences.retain(|sequence| !sequence.is_empty());
        if sequences.is_empty() {
            return Some(result);
        }
        let mut chosen: Option<String> = None;
        for sequence in &sequences {
            let head = &sequence[0];
            let in_tail = sequences
                .iter()
                .any(|other| other.iter().skip(1).any(|item| item == head));
            if !in_tail {
                chosen = Some(head.clone());
                break;
            }
        }
        let head = chosen?;
        result.push(head.clone());
        for sequence in &mut sequences {
            sequence.retain(|item| item != &head);
        }
    }
}

const LIFECYCLE_PHASES: [(&str, InvocationContext); 4] = [
    ("__init__", InvocationContext::SceneInit),
    ("setup", InvocationContext::Setup),
    ("construct", InvocationContext::Construct),
    ("tear_down", InvocationContext::TearDown),
];

/// Canonical scene base ids that decide the camera contract.
const THREE_D_SCENE_ID: &str = "manim.scene.three_d_scene.ThreeDScene";
const MOVING_CAMERA_SCENE_ID: &str = "manim.scene.moving_camera_scene.MovingCameraScene";
const SCENE_ID: &str = "manim.scene.scene.Scene";

/// The camera contract a scene class commits to (DESIGN §3.5), derived
/// from its reached external bases through the curated base chain. An
/// unresolved or mixed chain is `Unknown` — never guessed.
fn scene_camera_kind(ctx: &Ctx<'_>, record: &ClassRecord) -> CameraKind {
    if !record.bases_fully_resolved {
        return CameraKind::Unknown;
    }
    let mut three_d = false;
    let mut moving = false;
    let mut plain = false;
    for base in &record.reached_bases {
        if ctx.reaches_base(base, THREE_D_SCENE_ID) {
            three_d = true;
        } else if ctx.reaches_base(base, MOVING_CAMERA_SCENE_ID) {
            moving = true;
        } else if ctx.reaches_base(base, SCENE_ID) {
            plain = true;
        }
        // Non-scene mixins do not change the camera kind.
    }
    match (three_d, moving) {
        (true, false) => CameraKind::ThreeD,
        (false, true) => CameraKind::MovingCamera,
        (false, false) if plain => CameraKind::Standard,
        // Mixed 3D + moving chains and non-scene chains stay unresolved.
        _ => CameraKind::Unknown,
    }
}

pub(super) fn run_scene(ctx: &Ctx<'_>, record: &ClassRecord) -> SceneLifecycle {
    let (mro, constructor_state_unknown) = linearize_project(ctx.index, &record.qualified_name);
    let scene_id = ObjectId::new(
        AllocationSite::new(record.file, record.range),
        CallContextId::empty(),
        Cardinality::Singleton,
    );
    let mut heap = AbstractHeap::new();
    heap.insert_scene(
        scene_id.clone(),
        SceneState::initial(KindSet::single(&record.qualified_name)),
    );
    let mut state = ExecState::new(heap);
    let mut sink = TraceSink::default();
    let mut play_counter = 0u64;
    let mut super_calls = BTreeMap::new();

    for (method, phase) in LIFECYCLE_PHASES {
        let Some((def_class, def)) = mro.iter().find_map(|class| {
            ctx.defs
                .defs
                .get(&format!("{class}.{method}"))
                .map(|def| (class.clone(), def))
        }) else {
            continue;
        };
        if let Some(scene) = state.heap.scenes.get_mut(&scene_id) {
            scene.phase = phase;
        }
        let mut method_state = state.clone();
        method_state.env.clear();
        method_state
            .env
            .insert("self".to_owned(), AbstractValue::SelfScene);
        method_state.super_called = Presence::Absent;

        let mut machine = Machine {
            ctx,
            sink: &mut sink,
            file: def.file,
            module: def.module.clone(),
            scene_id: scene_id.clone(),
            mro: mro.clone(),
            reached_bases: record.reached_bases.clone(),
            current_class: Some(def_class),
            current_method: method.to_owned(),
            call_context: CallContextId::empty(),
            play_counter,
            emit: true,
            snapshot: true,
            block: BlockCtx::default(),
            forced_certainty: None,
            scene_run: true,
            inline_stack: Vec::new(),
            base_block: BlockCtx::default(),
            body_site: AllocationSite::new(def.file, def.range),
        };
        machine.record_entry_snapshot(&method_state);
        let exit = machine.run_body(def.body, &method_state);
        play_counter = machine.play_counter;
        super_calls.insert(method.to_owned(), exit.super_called);
        state = exit;
        state.env.clear();
    }

    let events = sink
        .ops
        .iter()
        .filter_map(|op| {
            op_to_event(op, &scene_id).map(|event| TracedEvent {
                site: op.site,
                certainty: op.certainty,
                event,
            })
        })
        .collect();
    // Play groups rehydrated from effect summaries during this scene's
    // lifecycle runs (drained per scene: the context is shared across
    // scenes, which run strictly sequentially).
    let summary_derived_plays = ctx
        .take_summary_play_groups()
        .into_iter()
        .map(crate::semantic::state::PlayGroupId)
        .collect();
    SceneLifecycle {
        qualified_name: record.qualified_name.clone(),
        file: record.file,
        scene_id,
        mro,
        constructor_state_unknown,
        events,
        snapshots: sink.snapshots,
        plays: sink.plays,
        summary_derived_plays,
        updaters: sink.updaters,
        updater_removals: sink.updater_removals,
        builders: sink.builders,
        target_requirements: sink.target_requirements,
        scene_removals: sink.scene_removals,
        camera_kind: scene_camera_kind(ctx, record),
        fixed_registrations: sink.fixed_registrations,
        super_calls,
        final_heap: state.heap,
    }
}
