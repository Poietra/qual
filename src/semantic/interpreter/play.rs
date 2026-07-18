//! Animation construction and the exact `Scene.play` / `Scene.wait`
//! pipeline of DESIGN §3.2: compile arguments, apply play kwargs,
//! auto-add non-introducer targets, derive the duration, introducer
//! setup-add, `begin()` starting copies with updater suspension,
//! `finish()` resume, `clean_up_from_scene()` remover / replacement
//! effects, and the §3.3 wait freeze verdict.

use std::collections::BTreeSet;

use rustpython_parser::ast::{self, Ranged};

use crate::frontend::index::QualifiedCall;
use crate::semantic::events::CleanupEffect;
use crate::semantic::state::{
    AnimationState, MobjectState, PlayGroupId, SuspendBehavior, WriteChannel,
};
use crate::semantic::values::{
    AllocationSite, CopyKind, CopyOf, KindSet, Num, ObjectId, Presence, Truth,
};

use super::dispatch::{Ctx, ResolvedAnimEffects};
use super::exec::{AbstractValue, ExecState, Machine, OpKind, literal_bool, literal_num};
use super::heap_ops::{clone_path_facts, widen_z_index_family};
use super::{PlayFact, PlayKind, PlayedAnimation, TargetRequirement, TargetRequirementFact};

/// Write channels of a curated animation class, with a completeness
/// verdict.
fn animation_channels(
    ctx: &Ctx<'_>,
    class_id: &str,
    effects: ResolvedAnimEffects,
) -> (BTreeSet<WriteChannel>, Truth) {
    let mut channels = BTreeSet::new();
    let known = if class_id.starts_with("manim.animation.fading.") {
        channels.insert(WriteChannel::Opacity);
        Truth::Yes
    } else if class_id.starts_with("manim.animation.creation.") {
        channels.insert(WriteChannel::Points);
        Truth::Yes
    } else if class_id == "manim.animation.animation.Wait" {
        Truth::Yes
    } else if ctx.reaches_base(class_id, "manim.animation.transform.Transform") {
        channels.insert(WriteChannel::Points);
        channels.insert(WriteChannel::Style);
        Truth::Yes
    } else {
        Truth::Maybe
    };
    if effects.introducer == Truth::Yes
        || effects.remover == Truth::Yes
        || effects.replacement == Truth::Yes
    {
        channels.insert(WriteChannel::Membership);
    }
    (channels, known)
}

impl<'a> Machine<'a, '_> {
    // -- animation construction --------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the play compile stage is inherently long"
    )]
    pub(super) fn create_animation(
        &mut self,
        kind: &KindSet,
        class_label: Option<&str>,
        effects: ResolvedAnimEffects,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let positional = self.eval_call_args(call, state);
        let site = self.site(call.range());

        // Composition groups: targets and effects come from the children.
        let child_states: Vec<AnimationState> = positional
            .iter()
            .filter_map(|value| match value {
                AbstractValue::Animation(id) => state.animations.get(id).cloned(),
                _ => None,
            })
            .collect();
        let is_composition = !child_states.is_empty()
            && positional
                .iter()
                .all(|value| matches!(value, AbstractValue::Animation(_) | AbstractValue::Unknown));

        let mut animation = AnimationState::unknown(kind.clone());
        animation.introducer = effects.introducer;
        animation.remover = effects.remover;
        animation.replacement = effects.replacement;
        animation.suspend = effects.suspend;

        let mut replacement_target = None;
        if is_composition {
            let mut targets = BTreeSet::new();
            let mut introducer: Option<Truth> = None;
            let mut remover: Option<Truth> = None;
            let mut replacement: Option<Truth> = None;
            for child in &child_states {
                targets.extend(child.targets.iter().cloned());
                introducer =
                    Some(introducer.map_or(child.introducer, |t| t.join(child.introducer)));
                remover = Some(remover.map_or(child.remover, |t| t.join(child.remover)));
                replacement =
                    Some(replacement.map_or(child.replacement, |t| t.join(child.replacement)));
            }
            animation.targets = targets;
            animation.introducer = introducer.unwrap_or(Truth::Maybe);
            animation.remover = remover.unwrap_or(Truth::Maybe);
            animation.replacement = replacement.unwrap_or(Truth::Maybe);
            animation.write_channels = child_states
                .iter()
                .flat_map(|child| child.write_channels.iter().copied())
                .collect();
        } else {
            if let Some(AbstractValue::Object(target)) = positional.first() {
                animation.targets.insert(target.clone());
            }
            if let Some(AbstractValue::Object(second)) = positional.get(1) {
                replacement_target = Some(second.clone());
            }
            if let Some(label) = class_label {
                let (channels, _known) = animation_channels(self.ctx, label, effects);
                animation.write_channels = channels;
            }
        }

        if let Some(argument) = fact.and_then(|fact| fact.keyword("run_time")) {
            if let Some(run_time) = literal_num(argument) {
                animation.run_time = run_time;
            }
        }
        if let Some(argument) = fact.and_then(|fact| fact.keyword("suspend_mobject_updating")) {
            // The DESIGN §3.2 escape hatch: only a literal proves the
            // behavior; a non-literal value makes suspension Unknown —
            // never "definitely suspends" (DESIGN §15 invariant 2).
            animation.suspend = match literal_bool(argument) {
                Some(true) => SuspendBehavior::SuspendsLiveTargets,
                Some(false) => SuspendBehavior::LeavesUpdatersRunning,
                None => SuspendBehavior::Unknown,
            };
        } else if fact.is_some_and(|fact| fact.has_star_star_kwargs) {
            // A `**kwargs` splat may carry `suspend_mobject_updating`.
            animation.suspend = SuspendBehavior::Unknown;
        }

        // MoveToTarget / Restore requirements against the current state
        // (all-paths-absent vs maybe-present, MLC107 / MLC120).
        if self.emit && (effects.requires_target || effects.requires_saved_state) {
            let target = animation.targets.iter().next().cloned();
            let requirement = if effects.requires_target {
                TargetRequirement::GeneratedTarget
            } else {
                TargetRequirement::SavedState
            };
            let presence = target.as_ref().map_or(Presence::Maybe, |target| {
                state.heap.object(target).map_or(Presence::Maybe, |object| {
                    if effects.requires_target {
                        object.generated_target.presence
                    } else {
                        object.saved_state
                    }
                })
            });
            self.sink.target_requirements.push(TargetRequirementFact {
                site,
                requirement,
                target,
                presence,
            });
        }

        let id = ObjectId::new(site, self.call_context.clone(), self.block.cardinality());
        state.animations.insert(id.clone(), animation.clone());
        if let Some(target) = &replacement_target {
            state.replacement_targets.insert(id.clone(), target.clone());
        }
        let targets: Vec<ObjectId> = animation.targets.iter().cloned().collect();
        self.record(
            site,
            OpKind::CreateAnimation {
                animation: id.clone(),
                state: animation,
                targets,
                replacement_target: replacement_target.clone(),
                requires_target: effects.requires_target,
                requires_saved_state: effects.requires_saved_state,
            },
        );
        AbstractValue::Animation(id)
    }

    // -- play / wait ---------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "the DESIGN §3.2 event order is one sequence"
    )]
    pub(super) fn do_play(
        &mut self,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let group = self.play_counter;
        self.play_counter += 1;
        let call_site = self.site(call.range());
        let certainty = self.certainty();

        // 1. compile arguments.
        let mut compiled: Vec<PlayedAnimation> = Vec::new();
        for arg in &call.args {
            if let ast::Expr::Starred(starred) = arg {
                self.eval_expr(&starred.value, state);
                continue;
            }
            let value = self.eval_expr(arg, state);
            let arg_site = self.site(arg.range());
            match value {
                AbstractValue::Animation(id) => {
                    let animation_state = state.animations.get(&id).cloned();
                    let channels_known =
                        animation_state
                            .as_ref()
                            .map_or(Truth::Maybe, |st| match &st.kind {
                                KindSet::Known(kinds) if kinds.len() == 1 => {
                                    let kind = kinds.iter().next().expect("len checked");
                                    let effects = ResolvedAnimEffects {
                                        introducer: st.introducer,
                                        remover: st.remover,
                                        replacement: st.replacement,
                                        suspend: st.suspend,
                                        requires_target: false,
                                        requires_saved_state: false,
                                    };
                                    animation_channels(self.ctx, kind, effects).1
                                }
                                _ => Truth::Maybe,
                            });
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: Some(id.clone()),
                        replacement_target: state.replacement_targets.get(&id).cloned(),
                        state: animation_state,
                        from_builder: false,
                        convertible: Truth::Yes,
                        channels_known,
                    });
                }
                AbstractValue::Builder(builder) => {
                    let mut animation = AnimationState::unknown(KindSet::single(
                        "manim.animation.transform._MethodAnimation",
                    ));
                    animation.introducer = Truth::No;
                    animation.remover = Truth::No;
                    animation.replacement = Truth::No;
                    animation.suspend = SuspendBehavior::SuspendsLiveTargets;
                    animation.write_channels.clone_from(&builder.channels);
                    if let Some(target) = &builder.target {
                        animation.targets.insert(target.clone());
                    }
                    let id = ObjectId::new(
                        arg_site,
                        self.call_context.clone(),
                        self.block.cardinality(),
                    );
                    state.animations.insert(id.clone(), animation.clone());
                    if self.emit {
                        if let Some(fact) = self.sink.builders.get_mut(&builder.site) {
                            fact.played = if certainty == Presence::Present {
                                Truth::Yes
                            } else {
                                fact.played.join(Truth::Yes)
                            };
                            fact.target_epoch_at_play = builder
                                .target
                                .as_ref()
                                .and_then(|target| state.heap.object(target))
                                .map(|object| object.mutation_epoch);
                        }
                    }
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: Some(id),
                        state: Some(animation),
                        replacement_target: None,
                        from_builder: true,
                        convertible: Truth::Yes,
                        channels_known: builder.channels_known,
                    });
                }
                AbstractValue::Object(id) => {
                    // A bare mobject cannot be converted to an animation
                    // (runtime TypeError; MLC102 evidence).
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::No,
                        channels_known: Truth::No,
                    });
                    let _ = id;
                }
                AbstractValue::Literal(_) => {
                    // A literal constant cannot be converted to an
                    // animation either (MLC102 evidence).
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::No,
                        channels_known: Truth::No,
                    });
                }
                _ => {
                    compiled.push(PlayedAnimation {
                        site: arg_site,
                        animation: None,
                        state: None,
                        replacement_target: None,
                        from_builder: false,
                        convertible: Truth::Maybe,
                        channels_known: Truth::Maybe,
                    });
                }
            }
        }
        for keyword in &call.keywords {
            self.eval_expr(&keyword.value, state);
        }

        // 2. apply play kwargs to every animation (setattr semantics,
        //    scene.py compile_animations).
        if let Some(run_time) = fact
            .and_then(|fact| fact.keyword("run_time"))
            .and_then(literal_num)
        {
            for played in &mut compiled {
                if let Some(animation_state) = &mut played.state {
                    animation_state.run_time = run_time.clone();
                }
                if let Some(id) = &played.animation {
                    if let Some(animation_state) = state.animations.get_mut(id) {
                        animation_state.run_time = run_time.clone();
                    }
                }
            }
        }
        // A play-level `suspend_mobject_updating` overrides every
        // animation's constructor value; a literal proves the behavior, a
        // non-literal (or a `**kwargs` splat that may carry the key)
        // degrades it to Unknown — never "definitely suspends".
        let play_suspend = match fact.and_then(|fact| fact.keyword("suspend_mobject_updating")) {
            Some(argument) => Some(match literal_bool(argument) {
                Some(true) => SuspendBehavior::SuspendsLiveTargets,
                Some(false) => SuspendBehavior::LeavesUpdatersRunning,
                None => SuspendBehavior::Unknown,
            }),
            None if fact.is_some_and(|fact| fact.has_star_star_kwargs) => {
                Some(SuspendBehavior::Unknown)
            }
            None => None,
        };
        if let Some(suspend) = play_suspend {
            for played in &mut compiled {
                if let Some(animation_state) = &mut played.state {
                    animation_state.suspend = suspend;
                }
            }
        }

        // 3. duration = max(run_time).
        let mut duration: Option<Num> = None;
        for played in &compiled {
            let run_time = played
                .state
                .as_ref()
                .map_or(Num::Unknown, |st| st.run_time.clone());
            duration = Some(match duration {
                None => run_time,
                Some(current) => current.max_with(&run_time),
            });
        }
        let duration = duration.unwrap_or(Num::Unknown);

        // 4. auto-add non-introducer targets that are not in the family.
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            match animation_state.introducer {
                Truth::No => {
                    for target in &animation_state.targets {
                        let membership = state
                            .heap
                            .object(target)
                            .map_or(Presence::Absent, |object| object.family_membership);
                        if membership == Presence::Present {
                            continue;
                        }
                        let add_certainty =
                            if membership == Presence::Absent && certainty == Presence::Present {
                                Presence::Present
                            } else {
                                Presence::Maybe
                            };
                        self.scene_add(
                            std::slice::from_ref(target),
                            played.site,
                            false,
                            false,
                            add_certainty,
                            state,
                        );
                    }
                }
                Truth::Yes => {}
                Truth::Maybe => {
                    // Unknown introducer status: the target is added
                    // either way during the play, but never certainly
                    // *here*.
                    for target in &animation_state.targets {
                        self.scene_add(
                            std::slice::from_ref(target),
                            played.site,
                            false,
                            false,
                            Presence::Maybe,
                            state,
                        );
                    }
                }
            }
        }

        let animation_ids: Vec<ObjectId> = compiled
            .iter()
            .filter_map(|played| played.animation.clone())
            .collect();
        self.record(
            call_site,
            OpKind::BeginPlay {
                play_group: group,
                animations: animation_ids,
                duration: duration.clone(),
            },
        );

        // 5. introducer setup-add (`_setup_scene`).
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            if animation_state.introducer == Truth::Yes {
                for target in &animation_state.targets {
                    let membership = state
                        .heap
                        .object(target)
                        .map_or(Presence::Absent, |object| object.family_membership);
                    if membership == Presence::Present {
                        continue;
                    }
                    let add_certainty =
                        if membership == Presence::Absent && certainty == Presence::Present {
                            Presence::Present
                        } else {
                            Presence::Maybe
                        };
                    self.scene_add(
                        std::slice::from_ref(target),
                        played.site,
                        false,
                        false,
                        add_certainty,
                        state,
                    );
                }
            }
        }

        // 6. begin(): starting copies + updater suspension.
        let mut suspended: Vec<ObjectId> = Vec::new();
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            for target in &animation_state.targets {
                // The starting copy is a fresh identity (DESIGN §15
                // invariant 6).
                let copy = ObjectId::new(
                    played.site,
                    self.call_context.push(call_site),
                    self.block.cardinality(),
                );
                let copy_state = state.heap.object(target).map_or_else(
                    || MobjectState::fresh(KindSet::Unknown),
                    |object| {
                        let mut fresh = MobjectState::fresh(object.kind.clone());
                        fresh.copy_provenance = Some(CopyKind::AnimationStartingCopy);
                        clone_path_facts(&mut fresh, object);
                        fresh
                    },
                );
                state.heap.insert_object(copy.clone(), copy_state);
                state.heap.record_copy(
                    copy,
                    CopyOf {
                        original: target.clone(),
                        kind: CopyKind::AnimationStartingCopy,
                    },
                );
                match animation_state.suspend {
                    SuspendBehavior::SuspendsLiveTargets => {
                        if let Some(object) = state.heap.object_mut(target) {
                            object.updating_suspended = if certainty == Presence::Present {
                                Truth::Yes
                            } else {
                                object.updating_suspended.join(Truth::Yes)
                            };
                        }
                        suspended.push(target.clone());
                        self.record(
                            played.site,
                            OpKind::SuspendUpdater {
                                target: target.clone(),
                            },
                        );
                    }
                    SuspendBehavior::Unknown => {
                        if let Some(object) = state.heap.object_mut(target) {
                            object.updating_suspended = object.updating_suspended.join(Truth::Yes);
                        }
                    }
                    SuspendBehavior::LeavesUpdatersRunning => {}
                }
            }
        }

        // 7. finish(): suspended updaters resume.
        for target in &suspended {
            if let Some(object) = state.heap.object_mut(target) {
                object.updating_suspended = if certainty == Presence::Present {
                    Truth::No
                } else {
                    object.updating_suspended.join(Truth::No)
                };
            }
            self.record(
                call_site,
                OpKind::ResumeUpdater {
                    target: target.clone(),
                },
            );
        }

        // 8. clean_up_from_scene(): removers remove, replacements replace.
        let mut cleanup: Vec<CleanupEffect> = Vec::new();
        for played in &compiled {
            let Some(animation_state) = &played.state else {
                continue;
            };
            let targets: Vec<ObjectId> = animation_state.targets.iter().cloned().collect();
            match animation_state.remover {
                Truth::Yes => {
                    self.scene_remove(&targets, played.site, certainty, state);
                    cleanup.push(CleanupEffect::SceneRemove(targets.clone()));
                }
                Truth::Maybe => {
                    self.scene_remove(&targets, played.site, Presence::Maybe, state);
                }
                Truth::No => {}
            }
            match animation_state.replacement {
                Truth::Yes => {
                    if let (Some(old), Some(new)) =
                        (targets.first(), played.replacement_target.as_ref())
                    {
                        self.scene_replace(old, new, certainty, state);
                        cleanup.push(CleanupEffect::SceneReplace {
                            old: old.clone(),
                            new: new.clone(),
                        });
                    }
                }
                Truth::Maybe => {
                    if let (Some(old), Some(new)) =
                        (targets.first(), played.replacement_target.as_ref())
                    {
                        self.scene_replace(old, new, Presence::Maybe, state);
                    }
                }
                Truth::No => {}
            }
            // Interpolation wrote the animation's channels on its live
            // targets.
            if !animation_state.write_channels.is_empty() || played.from_builder {
                for target in &targets {
                    if let Some(object) = state.heap.object_mut(target) {
                        object.mutation_epoch += 1;
                    }
                }
            }
            // Only a complete channel classification proves the animation
            // left `z_index` alone (curated families never write it); a
            // custom `interpolate` may set it on any live target.
            if played.channels_known != Truth::Yes {
                for target in &targets {
                    widen_z_index_family(state, target);
                }
            }
        }
        if !suspended.is_empty() {
            cleanup.push(CleanupEffect::ResumeUpdaters(suspended));
        }
        cleanup.push(CleanupEffect::FinalUpdaterPass);
        self.record(
            call_site,
            OpKind::FinishPlay {
                play_group: group,
                cleanup,
            },
        );

        if let Some(scene) = self.scene_state_mut(state) {
            scene.elapsed_time = scene.elapsed_time.add(&duration);
        }
        if self.emit {
            let always_update = state
                .heap
                .scenes
                .get(&self.scene_id)
                .map_or(Truth::Maybe, |scene| scene.always_update_mobjects);
            self.sink.plays.push(PlayFact {
                site: call_site,
                play_group: PlayGroupId(group),
                kind: PlayKind::Play,
                duration,
                animations: compiled,
                dynamic_wait: Truth::No,
                has_stop_condition: false,
                frozen_frame: None,
                always_update_mobjects: always_update,
                star_args: call
                    .args
                    .iter()
                    .any(|arg| matches!(arg, ast::Expr::Starred(_))),
                certainty,
                repetitions: self.block.repetition_bound(),
                call_path: self.inline_call_path(),
            });
        }
        AbstractValue::Unknown
    }

    /// The helper call sites currently on the inline stack, outermost
    /// first ([`PlayFact::call_path`]).
    fn inline_call_path(&self) -> Vec<AllocationSite> {
        self.inline_stack.iter().map(|(_, site)| *site).collect()
    }

    pub(super) fn do_wait(
        &mut self,
        call: &'a ast::ExprCall,
        fact: Option<&'a QualifiedCall>,
        state: &mut ExecState,
    ) -> AbstractValue {
        let group = self.play_counter;
        self.play_counter += 1;
        self.eval_call_args(call, state);
        let call_site = self.site(call.range());
        let duration = fact
            .and_then(|fact| {
                fact.positional(0)
                    .or_else(|| fact.keyword("duration"))
                    .and_then(literal_num)
            })
            .unwrap_or(Num::Unknown);
        let has_stop_condition = fact.is_some_and(|fact| fact.keyword("stop_condition").is_some());
        let frozen_frame = fact
            .and_then(|fact| fact.keyword("frozen_frame"))
            .and_then(literal_bool);

        // Wait freeze verdict (DESIGN §3.3 / `should_update_mobjects`).
        let scene_has_updaters = state
            .heap
            .scenes
            .get(&self.scene_id)
            .is_some_and(|scene| !scene.scene_updaters.is_empty());
        let always_update = state
            .heap
            .scenes
            .get(&self.scene_id)
            .map_or(Truth::Maybe, |scene| scene.always_update_mobjects);
        let mut dynamic = if has_stop_condition || scene_has_updaters {
            Truth::Yes
        } else {
            Truth::No
        };
        if dynamic == Truth::No {
            // Any time-based updater in the scene family makes the wait
            // dynamic; a one-argument updater alone does not.
            for object in state.heap.objects.values() {
                if !object.family_membership.may_be_present() {
                    continue;
                }
                for updater in &object.updaters {
                    match (updater.time_based, object.family_membership) {
                        (Truth::Yes, Presence::Present) => {
                            dynamic = Truth::Yes;
                        }
                        (Truth::Yes | Truth::Maybe, _) => {
                            if dynamic == Truth::No {
                                dynamic = Truth::Maybe;
                            }
                        }
                        (Truth::No, _) => {}
                    }
                }
                if dynamic == Truth::Yes {
                    break;
                }
            }
        }
        // Tracked `always_update_mobjects` (DESIGN §3.3): a literal True
        // makes every wait dynamic; a non-literal write is a maybe-fact.
        match always_update {
            Truth::Yes => dynamic = Truth::Yes,
            Truth::Maybe => {
                if dynamic == Truth::No {
                    dynamic = Truth::Maybe;
                }
            }
            Truth::No => {}
        }

        if let Some(scene) = self.scene_state_mut(state) {
            scene.elapsed_time = scene.elapsed_time.add(&duration);
        }
        if self.emit {
            self.sink.plays.push(PlayFact {
                site: call_site,
                play_group: PlayGroupId(group),
                kind: PlayKind::Wait,
                duration,
                animations: Vec::new(),
                dynamic_wait: dynamic,
                has_stop_condition,
                frozen_frame,
                always_update_mobjects: always_update,
                star_args: false,
                certainty: self.certainty(),
                repetitions: self.block.repetition_bound(),
                call_path: self.inline_call_path(),
            });
        }
        AbstractValue::Unknown
    }
}
