//! Lifecycle abstract-interpreter tests (DESIGN §11.2 layer 4): membership
//! traces, play event order, branch joins, loop widening, helper
//! summaries, MRO composition, `.animate` builders, updater facts, and the
//! §3.4 remove/re-add restructuring semantics.

use std::path::{Path, PathBuf};

use manim_lint::application::manim_surface;
use manim_lint::frontend::index::{FrontendFacts, analyze as frontend_analyze};
use manim_lint::knowledge;
use manim_lint::semantic::events::Event;
use manim_lint::semantic::interpreter::{
    self, DefMap, LifecycleFacts, PlayKind, SceneLifecycle, TargetRequirement,
};
use manim_lint::semantic::state::CallbackRef;
use manim_lint::semantic::summaries::{self, SummaryEffect};
use manim_lint::semantic::values::{Cardinality, ObjectId, Presence, Truth};
use manim_lint::source::{FileId, SourceManager};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lifecycle")
}

fn analyze(files: &[&str]) -> (SourceManager, FrontendFacts, LifecycleFacts) {
    let root = fixture_root();
    let mut sources = SourceManager::new(&root);
    for file in files {
        sources.load_file(&root.join(file));
    }
    for file in sources.files() {
        assert!(
            file.is_parsed(),
            "fixture must parse: {}",
            file.relative_path()
        );
    }
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).expect("profile loads");
    let surface = manim_surface(&profile);
    let roots = vec![".".to_owned()];
    let facts = frontend_analyze(&sources, &roots, &surface);
    let lifecycle = interpreter::analyze(&sources, &facts.index, &facts.calls, Some(&profile));
    (sources, facts, lifecycle)
}

fn scene<'a>(lifecycle: &'a LifecycleFacts, name: &str) -> &'a SceneLifecycle {
    lifecycle
        .scene(name)
        .unwrap_or_else(|| panic!("scene {name} analyzed"))
}

fn slice_at(sources: &SourceManager, file: FileId, start: u32, end: u32) -> &str {
    &sources.file(file).text()[start as usize..end as usize]
}

/// The object allocated at the (unique) `Alloc` event whose source slice
/// equals `text`.
fn alloc_id(scene: &SceneLifecycle, sources: &SourceManager, text: &str) -> ObjectId {
    for traced in &scene.events {
        if let Event::Alloc(alloc) = &traced.event {
            if slice_at(
                sources,
                traced.site.file,
                traced.site.start,
                traced.site.end,
            ) == text
            {
                return alloc.object.clone();
            }
        }
    }
    panic!("no Alloc event with source `{text}`");
}

fn offset_of(sources: &SourceManager, file: FileId, needle: &str) -> u32 {
    let position = sources
        .file(file)
        .text()
        .find(needle)
        .unwrap_or_else(|| panic!("fixture contains `{needle}`"));
    u32::try_from(position).expect("offset fits u32")
}

// ---------------------------------------------------------------------------
// Straight-line membership trace (add / re-add order / remove).
// ---------------------------------------------------------------------------

#[test]
fn straight_line_membership_trace() {
    let (sources, _facts, lifecycle) = analyze(&["straight_line.py"]);
    let scene = scene(&lifecycle, "straight_line.StraightLine");
    let sq = alloc_id(scene, &sources, "Square()");
    let circle = alloc_id(scene, &sources, "Circle()");

    // Event sequence: two allocs, three adds, one remove.
    let adds: Vec<_> = scene
        .events
        .iter()
        .filter_map(|traced| match &traced.event {
            Event::SceneAdd(add) => Some(add),
            _ => None,
        })
        .collect();
    assert_eq!(adds.len(), 3);
    assert_eq!(adds[0].objects, vec![sq.clone()]);
    assert!(!adds[0].order_effect);
    assert_eq!(adds[1].objects, vec![circle.clone()]);
    // The re-add of an existing member moves it to the end: an order
    // effect (DESIGN §3.4).
    assert_eq!(adds[2].objects, vec![sq.clone()]);
    assert!(adds[2].order_effect);
    // Every straight-line event is path-certain.
    assert!(
        scene
            .events
            .iter()
            .all(|traced| traced.certainty == Presence::Present)
    );

    // Snapshot just before the remove: circle still a member.
    let before_remove = offset_of(&sources, scene.file, "self.remove(circle)");
    assert_eq!(
        scene.membership_at(&circle, scene.file, before_remove),
        Some((Presence::Present, Presence::Present))
    );

    // Final state: circle removed, sq last (and only) root.
    assert_eq!(
        scene.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
    assert_eq!(
        scene.final_membership(&circle),
        Some((Presence::Absent, Presence::Absent))
    );
    let roots = &scene.final_heap.scenes[&scene.scene_id].roots;
    assert_eq!(roots.items, vec![sq]);
    assert_eq!(roots.order_known, Truth::Yes);
}

// ---------------------------------------------------------------------------
// FadeIn introducer setup-add vs FadeOut auto-add + remover cleanup.
// ---------------------------------------------------------------------------

#[test]
fn fade_in_setup_add_and_fade_out_cleanup() {
    let (sources, _facts, lifecycle) = analyze(&["fades.py"]);
    let fades = scene(&lifecycle, "fades.Fades");
    let sq = alloc_id(fades, &sources, "Square()");

    let index_of = |predicate: &dyn Fn(&Event) -> bool| -> usize {
        fades
            .events
            .iter()
            .position(|traced| predicate(&traced.event))
            .expect("event present")
    };
    let first_begin = index_of(&|event| matches!(event, Event::BeginPlay(_)));
    let add = index_of(&|event| matches!(event, Event::SceneAdd(_)));
    // FadeIn is an introducer: the scene add happens at play setup, after
    // BeginPlay — never at animation construction (DESIGN §15 inv. 4).
    assert!(add > first_begin);
    let create = index_of(&|event| matches!(event, Event::CreateAnimation(_)));
    assert!(create < first_begin);

    // FadeOut cleanup removes the target again.
    let remove = index_of(&|event| matches!(event, Event::SceneRemove(_)));
    let finish_indices: Vec<usize> = fades
        .events
        .iter()
        .enumerate()
        .filter(|(_, traced)| matches!(traced.event, Event::FinishPlay(_)))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(finish_indices.len(), 2);
    assert!(remove > first_begin);
    assert_eq!(
        fades.final_membership(&sq),
        Some((Presence::Absent, Presence::Absent))
    );

    // FadeOut on a never-added mobject: auto-added at play preparation
    // (before BeginPlay), then removed by cleanup (DESIGN §1 / §3.2).
    let fresh = scene(&lifecycle, "fades.FadeOutFresh");
    let sq = alloc_id(fresh, &sources, "Square()");
    let begin = fresh
        .events
        .iter()
        .position(|traced| matches!(traced.event, Event::BeginPlay(_)))
        .expect("begin");
    let auto_add = fresh
        .events
        .iter()
        .position(|traced| matches!(traced.event, Event::SceneAdd(_)))
        .expect("auto-add");
    assert!(auto_add < begin, "auto-add happens during play compilation");
    assert_eq!(
        fresh.final_membership(&sq),
        Some((Presence::Absent, Presence::Absent))
    );
    let played = &fresh.plays[0].animations[0];
    assert_eq!(played.state.as_ref().expect("state").remover, Truth::Yes);
    assert_eq!(played.convertible, Truth::Yes);
}

// ---------------------------------------------------------------------------
// ReplacementTransform replaces; plain Transform does not.
// ---------------------------------------------------------------------------

#[test]
fn replacement_transform_replaces_and_transform_does_not() {
    let (sources, _facts, lifecycle) = analyze(&["replacement.py"]);
    let replace = scene(&lifecycle, "replacement.Replace");
    let sq = alloc_id(replace, &sources, "Square()");
    let circle = alloc_id(replace, &sources, "Circle()");
    assert_eq!(
        replace.final_membership(&sq),
        Some((Presence::Absent, Presence::Absent))
    );
    assert_eq!(
        replace.final_membership(&circle),
        Some((Presence::Present, Presence::Present))
    );
    let roots = &replace.final_heap.scenes[&replace.scene_id].roots;
    assert_eq!(roots.items, vec![circle.clone()]);
    // The cleanup carries the replace effect.
    let cleanup_has_replace = replace.events.iter().any(|traced| {
        matches!(
            &traced.event,
            Event::FinishPlay(finish) if finish.cleanup.iter().any(|effect| matches!(
                effect,
                manim_lint::semantic::events::CleanupEffect::SceneReplace { old, new }
                    if *old == sq && *new == circle
            ))
        )
    });
    assert!(cleanup_has_replace);

    // Plain Transform: the source keeps its membership; the target stays
    // out of the scene (DESIGN §3.2).
    let plain = scene(&lifecycle, "replacement.PlainTransform");
    let sq = alloc_id(plain, &sources, "Square()");
    let circle = alloc_id(plain, &sources, "Circle()");
    assert_eq!(
        plain.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
    assert_eq!(
        plain.final_membership(&circle),
        Some((Presence::Absent, Presence::Absent))
    );
    let played = &plain.plays[0].animations[0];
    assert_eq!(played.replacement_target.as_ref(), Some(&circle));
}

// ---------------------------------------------------------------------------
// Branch join → MAYBE membership.
// ---------------------------------------------------------------------------

#[test]
fn branch_join_weakens_membership_to_maybe() {
    let (sources, _facts, lifecycle) = analyze(&["branchy.py"]);
    let branchy = scene(&lifecycle, "branchy.Branchy");
    let sq = alloc_id(branchy, &sources, "Square()");
    assert_eq!(
        branchy.final_membership(&sq),
        Some((Presence::Maybe, Presence::Maybe))
    );
    // The conditional add is a maybe-event, never a certain one.
    let add = branchy
        .events
        .iter()
        .find(|traced| matches!(traced.event, Event::SceneAdd(_)))
        .expect("add event");
    assert_eq!(add.certainty, Presence::Maybe);

    // try/except: the handler may run from any prefix of the body, so the
    // membership is a maybe-fact after the join.
    let trying = scene(&lifecycle, "branchy.Trying");
    let sq = alloc_id(trying, &sources, "Square()");
    let (root, family) = trying.final_membership(&sq).expect("state");
    assert_eq!(root, Presence::Maybe);
    assert_eq!(family, Presence::Maybe);
}

// ---------------------------------------------------------------------------
// Loop widening → Many cardinality.
// ---------------------------------------------------------------------------

#[test]
fn loop_allocation_widens_to_many() {
    let (sources, _facts, lifecycle) = analyze(&["loops.py"]);
    let loops = scene(&lifecycle, "loops.Loops");
    let sq = alloc_id(loops, &sources, "Square()");
    // A literal range(3) loop provably iterates: the allocation stands for
    // many runtime objects (DESIGN §5.5).
    assert_eq!(sq.cardinality, Cardinality::Many);
    // Two Many ids are never provably the same object.
    assert_eq!(sq.may_be_same(&sq.clone()), Truth::Maybe);
    // Loop-body effects are maybe-facts after 0/1-iteration evaluation.
    let (root, _family) = loops.final_membership(&sq).expect("state");
    assert_eq!(root, Presence::Maybe);
}

// ---------------------------------------------------------------------------
// Helper summaries: self.add inside a project helper, factory returns.
// ---------------------------------------------------------------------------

#[test]
fn helper_summary_applies_scene_add() {
    let (sources, _facts, lifecycle) = analyze(&["helpers.py"]);
    let uses = scene(&lifecycle, "helpers.UsesHelper");
    let sq = alloc_id(uses, &sources, "Square()");
    assert_eq!(
        uses.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
    // The event is anchored inside the helper body.
    let add = uses
        .events
        .iter()
        .find(|traced| matches!(traced.event, Event::SceneAdd(_)))
        .expect("add via summary");
    assert_eq!(
        slice_at(&sources, add.site.file, add.site.start, add.site.end),
        "self.add(mob)"
    );
    assert_eq!(add.certainty, Presence::Present);
}

#[test]
fn factory_return_alias_tracks_allocation() {
    let (sources, _facts, lifecycle) = analyze(&["helpers.py"]);
    let factory = scene(&lifecycle, "helpers.UsesFactory");
    // The allocation site lives inside make_square; the returned alias is
    // what construct adds.
    let sq = alloc_id(factory, &sources, "Square()");
    assert_eq!(
        factory.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
}

#[test]
fn recursive_scc_widens_only_unknown_effects() {
    let (sources, facts, _lifecycle) = analyze(&["helpers.py"]);
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).expect("profile loads");
    let defs = DefMap::build(&sources, &facts.index);
    let table = summaries::build(&sources, &facts.index, &facts.calls, Some(&profile), &defs);
    let ping = table.get("helpers.RecScene.ping").expect("summary");
    assert!(
        !ping.converged,
        "mutual recursion must not claim convergence"
    );
    // The stable knowledge (the direct self.add) survives; only the
    // unstable part widened to an unknown mutation.
    assert!(
        ping.events
            .iter()
            .any(|event| matches!(event.effect, SummaryEffect::SceneAdd { .. }))
    );
    assert!(matches!(
        ping.events.last().map(|event| &event.effect),
        Some(SummaryEffect::UnknownMutation { .. })
    ));
}

// ---------------------------------------------------------------------------
// MRO composition with a project-local intermediate Scene base.
// ---------------------------------------------------------------------------

#[test]
fn mro_composition_runs_super_chain() {
    let (sources, _facts, lifecycle) = analyze(&["mro_chain.py"]);
    let child = scene(&lifecycle, "mro_chain.Child");
    assert_eq!(
        child.mro,
        vec!["mro_chain.Child".to_owned(), "mro_chain.Base".to_owned()]
    );
    assert!(!child.constructor_state_unknown);
    assert_eq!(child.super_calls.get("setup"), Some(&Presence::Present));

    // Base.setup ran through super(): the background exists and construct
    // removed it; the child's own dot stays.
    let background = alloc_id(child, &sources, "Square()");
    let dot = alloc_id(child, &sources, "Circle()");
    assert_eq!(
        child.final_membership(&background),
        Some((Presence::Absent, Presence::Absent))
    );
    assert_eq!(
        child.final_membership(&dot),
        Some((Presence::Present, Presence::Present))
    );

    // NoSuper skips the base setup: no background allocation at all, and
    // the omitted super() is a recorded fact (MLC128 evidence).
    let no_super = scene(&lifecycle, "mro_chain.NoSuper");
    assert_eq!(no_super.super_calls.get("setup"), Some(&Presence::Absent));
    assert!(!no_super.events.iter().any(|traced| {
        matches!(&traced.event, Event::Alloc(_))
            && slice_at(
                &sources,
                traced.site.file,
                traced.site.start,
                traced.site.end,
            ) == "Square()"
    }));
    let dot = alloc_id(no_super, &sources, "Circle()");
    assert_eq!(
        no_super.final_membership(&dot),
        Some((Presence::Present, Presence::Present))
    );
}

// ---------------------------------------------------------------------------
// .animate builders.
// ---------------------------------------------------------------------------

#[test]
fn animate_builder_generates_target_and_plays() {
    let (sources, _facts, lifecycle) = analyze(&["animate_builder.py"]);
    let builder_scene = scene(&lifecycle, "animate_builder.Builder");
    let sq = alloc_id(builder_scene, &sources, "Square()");

    assert_eq!(builder_scene.builders.len(), 1);
    let fact = builder_scene.builders.values().next().expect("builder");
    assert_eq!(fact.target.as_ref(), Some(&sq));
    assert_eq!(fact.methods, vec!["shift".to_owned(), "rotate".to_owned()]);
    assert!(
        fact.channels
            .contains(&manim_lint::semantic::state::WriteChannel::Points)
    );
    assert_eq!(fact.played, Truth::Yes);
    assert_eq!(fact.overwritten_by_later_builder, Truth::No);

    // generate_target ran at builder creation: the target copy is a fresh
    // allocation anchored at `sq.animate`.
    assert!(builder_scene.events.iter().any(|traced| {
        matches!(traced.event, Event::Alloc(_))
            && slice_at(
                &sources,
                traced.site.file,
                traced.site.start,
                traced.site.end,
            ) == "sq.animate"
    }));

    let played = &builder_scene.plays[0].animations[0];
    assert!(played.from_builder);
    let state = played.state.as_ref().expect("state");
    assert_eq!(state.introducer, Truth::No);
    assert!(state.targets.contains(&sq));
}

#[test]
fn builder_records_stale_target_epochs() {
    let (_sources, _facts, lifecycle) = analyze(&["animate_builder.py"]);
    let stale = scene(&lifecycle, "animate_builder.StaleBuilder");
    let fact = stale.builders.values().next().expect("builder");
    assert_eq!(fact.played, Truth::Yes);
    let at_play = fact.target_epoch_at_play.expect("played epoch");
    // The live target mutated between builder creation and play
    // (MLC117 evidence).
    assert!(at_play > fact.target_epoch_at_creation);

    let overwritten = scene(&lifecycle, "animate_builder.Overwritten");
    assert_eq!(overwritten.builders.len(), 2);
    let first = overwritten
        .builders
        .values()
        .find(|fact| fact.methods == vec!["shift".to_owned()])
        .expect("first builder");
    assert_eq!(first.overwritten_by_later_builder, Truth::Yes);
}

// ---------------------------------------------------------------------------
// Updater registration signatures, identity removal, wait dynamics.
// ---------------------------------------------------------------------------

#[test]
fn updater_signatures_and_identity_matching() {
    let (_sources, _facts, lifecycle) = analyze(&["updaters.py"]);
    let updaters = scene(&lifecycle, "updaters.Updaters");

    assert_eq!(updaters.updaters.len(), 2);
    let spin = &updaters.updaters[0];
    assert_eq!(
        spin.fact.callback,
        CallbackRef::Named("updaters.spin".to_owned())
    );
    assert_eq!(spin.fact.signature.positional_params, Some(2));
    assert_eq!(spin.fact.signature.has_dt_named_param, Truth::Yes);
    assert_eq!(spin.fact.signature.binds_positionally, Truth::Yes);
    // The *name* `dt` makes the updater time-based (DESIGN §3.3).
    assert_eq!(spin.fact.time_based, Truth::Yes);

    let follow = &updaters.updaters[1];
    assert_eq!(
        follow.fact.callback,
        CallbackRef::Named("updaters.follow".to_owned())
    );
    assert_eq!(follow.fact.time_based, Truth::No);
    assert_eq!(follow.fact.signature.binds_positionally, Truth::Yes);

    // remove_updater(spin) matches its registration identity; the fresh
    // lambda definitely matches nothing (MLC125 evidence).
    assert_eq!(updaters.updater_removals.len(), 2);
    assert_eq!(updaters.updater_removals[0].matched, Truth::Yes);
    assert_eq!(updaters.updater_removals[1].matched, Truth::No);

    // Only the one-argument updater remains: the default wait stays a
    // static frame (DESIGN §3.3).
    let wait = updaters
        .plays
        .iter()
        .find(|play| play.kind == PlayKind::Wait)
        .expect("wait fact");
    assert_eq!(wait.dynamic_wait, Truth::No);

    // With the dt updater still registered the wait renders dynamically.
    let dynamic = scene(&lifecycle, "updaters.DynamicWait");
    let wait = dynamic
        .plays
        .iter()
        .find(|play| play.kind == PlayKind::Wait)
        .expect("wait fact");
    assert_eq!(wait.dynamic_wait, Truth::Yes);
}

// ---------------------------------------------------------------------------
// Scene.remove restructuring and re-add reappearance (DESIGN §3.4).
// ---------------------------------------------------------------------------

#[test]
fn remove_child_then_re_add_parent_reappears() {
    let (sources, _facts, lifecycle) = analyze(&["regroup.py"]);
    let regroup = scene(&lifecycle, "regroup.Regroup");
    let a = alloc_id(regroup, &sources, "Square()");
    let b = alloc_id(regroup, &sources, "Circle()");
    let group = alloc_id(regroup, &sources, "VGroup(a, b)");

    // After remove(a): the group dissolves out of the root list, b is
    // promoted to a root, and a keeps its surviving parent link.
    let after_remove = offset_of(&sources, regroup.file, "self.remove(a)")
        + u32::try_from("self.remove(a)".len()).expect("fits");
    assert_eq!(
        regroup.membership_at(&a, regroup.file, after_remove),
        Some((Presence::Absent, Presence::Absent))
    );
    assert_eq!(
        regroup.membership_at(&b, regroup.file, after_remove),
        Some((Presence::Present, Presence::Present))
    );
    assert_eq!(
        regroup.membership_at(&group, regroup.file, after_remove),
        Some((Presence::Absent, Presence::Absent))
    );
    let removal = &regroup.scene_removals[0];
    assert_eq!(removal.removed, a);
    assert!(removal.surviving_parents.contains(&group));

    // Re-adding the parent brings a back (the §3.4 temporary-removal
    // hazard behind MLC115).
    let (root, family) = regroup.final_membership(&a).expect("state");
    assert_eq!(root, Presence::Absent, "a itself is not a root");
    assert_eq!(family, Presence::Present, "a reappears through the group");
    let roots = &regroup.final_heap.scenes[&regroup.scene_id].roots;
    assert_eq!(roots.items, vec![b, group]);
}

// ---------------------------------------------------------------------------
// Project overrides distrust curated animation effects (DESIGN §5.4).
// ---------------------------------------------------------------------------

#[test]
fn project_override_distrusts_curated_effects() {
    let (sources, _facts, lifecycle) = analyze(&["distrust.py"]);
    // MyFade overrides begin(): the curated FadeOut remover effect is no
    // longer trusted, so membership is only a maybe-fact.
    let distrust = scene(&lifecycle, "distrust.Distrust");
    let sq = alloc_id(distrust, &sources, "Square()");
    let (_, family) = distrust.final_membership(&sq).expect("state");
    assert_eq!(family, Presence::Maybe);
    let played = &distrust.plays[0].animations[0];
    assert_eq!(played.state.as_ref().expect("state").remover, Truth::Maybe);

    // A pure subclass without lifecycle overrides keeps the curated
    // effects: the target is definitely removed.
    let trusting = scene(&lifecycle, "distrust.Trusting");
    let sq = alloc_id(trusting, &sources, "Square()");
    assert_eq!(
        trusting.final_membership(&sq),
        Some((Presence::Absent, Presence::Absent))
    );
}

// ---------------------------------------------------------------------------
// generate_target / save_state path facts (MLC107 / MLC120).
// ---------------------------------------------------------------------------

#[test]
fn target_requirements_track_all_paths() {
    let (_sources, _facts, lifecycle) = analyze(&["targets.py"]);
    let targets = scene(&lifecycle, "targets.Targets");
    let presences: Vec<(TargetRequirement, Presence)> = targets
        .target_requirements
        .iter()
        .map(|fact| (fact.requirement, fact.presence))
        .collect();
    assert_eq!(
        presences,
        vec![
            // MoveToTarget before generate_target: absent on all paths.
            (TargetRequirement::GeneratedTarget, Presence::Absent),
            // After generate_target: satisfied.
            (TargetRequirement::GeneratedTarget, Presence::Present),
            // Restore after save_state: satisfied.
            (TargetRequirement::SavedState, Presence::Present),
        ]
    );

    // A branch-only generate_target is maybe-present: the rule layer must
    // stay silent (silence over false positives).
    let maybe = scene(&lifecycle, "targets.MaybeTarget");
    assert_eq!(maybe.target_requirements.len(), 1);
    assert_eq!(maybe.target_requirements[0].presence, Presence::Maybe);
}
