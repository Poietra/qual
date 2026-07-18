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
    self, CameraKind, DefMap, FixedAction, FixedKind, LifecycleFacts, PlayKind, SceneLifecycle,
    TargetRequirement, UpdaterHost,
};
use manim_lint::semantic::state::CallbackRef;
use manim_lint::semantic::summaries::{self, SummaryEffect};
use manim_lint::semantic::values::{Cardinality, Num, ObjectId, Presence, Truth};
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

// ---------------------------------------------------------------------------
// Module-alias constructors resolve through the qualified-call facts.
// ---------------------------------------------------------------------------

/// `import manim as mn; mn.Square()` must allocate a tracked object and
/// `mn.FadeOut(sq)` must resolve to the curated remover animation — the
/// interpreter bridges attribute callees whose receiver the frontend
/// classified as `ModuleAlias` into the same candidate machinery as
/// direct names instead of widening them to Unknown (which silenced the
/// state rules under module-alias import style).
#[test]
fn module_alias_constructor_allocates_tracked_object() {
    let (sources, _facts, lifecycle) = analyze(&["module_alias.py"]);
    let alias = scene(&lifecycle, "module_alias.AliasScene");
    let sq = alloc_id(alias, &sources, "mn.Square()");
    assert_eq!(sq.cardinality, Cardinality::Singleton);

    // Membership just before the play: present (self.add tracked it).
    let at_play = offset_of(&sources, alias.file, "self.play");
    assert_eq!(
        alias.membership_at(&sq, alias.file, at_play),
        Some((Presence::Present, Presence::Present))
    );

    // `mn.FadeOut` resolved to the curated remover: cleanup removes sq.
    assert_eq!(alias.plays.len(), 1);
    assert_eq!(alias.plays[0].kind, PlayKind::Play);
    assert_eq!(
        alias.final_membership(&sq),
        Some((Presence::Absent, Presence::Absent))
    );
}

// ---------------------------------------------------------------------------
// Own-path point counts and path_state_at (MLR116).
// ---------------------------------------------------------------------------

#[test]
fn path_state_tracks_empty_and_growing_paths() {
    let (sources, _facts, lifecycle) = analyze(&["path_state.py"]);
    let path_scene = scene(&lifecycle, "path_state.PathState");
    let file = path_scene.file;
    let path = alloc_id(path_scene, &sources, "VMobject()");

    // A fresh VMobject provably starts with an empty own path.
    let at_start = path_scene
        .path_state_at(
            &path,
            file,
            offset_of(&sources, file, "path.start_new_path"),
        )
        .expect("state");
    assert_eq!(at_start.point_count, Num::int(0));
    assert_eq!(at_start.empty, Truth::Yes);

    // start_new_path appended exactly one point.
    let at_line = path_scene
        .path_state_at(&path, file, offset_of(&sources, file, "path.add_line_to"))
        .expect("state");
    assert_eq!(at_line.point_count, Num::int(1));
    assert_eq!(at_line.empty, Truth::No);

    // add_line_to completed the started curve: 4 points, 1 curve.
    let at_add = path_scene
        .path_state_at(&path, file, offset_of(&sources, file, "self.add(path)"))
        .expect("state");
    assert_eq!(at_add.point_count, Num::int(4));
    assert_eq!(at_add.curve_count, Num::int(1));

    // Square starts provably NON-empty: add_line_to on it is fine.
    let sq = alloc_id(path_scene, &sources, "Square()");
    let sq_state = path_scene
        .path_state_at(&sq, file, offset_of(&sources, file, "sq.add_line_to"))
        .expect("state");
    assert_eq!(sq_state.empty, Truth::No);

    // add_line_to on a provably empty path (the MLR116 defect).
    let bug_scene = scene(&lifecycle, "path_state.EmptyPathBug");
    let bug = alloc_id(bug_scene, &sources, "VMobject()");
    let bug_state = bug_scene
        .path_state_at(&bug, file, offset_of(&sources, file, "bug.add_line_to"))
        .expect("state");
    assert_eq!(bug_state.point_count, Num::int(0));
    assert_eq!(bug_state.empty, Truth::Yes);

    // An unknown mutation widens the counts: never a wrong certain fact.
    let widened_scene = scene(&lifecycle, "path_state.WidenedPath");
    let fuzzy = alloc_id(widened_scene, &sources, "VMobject()");
    let fuzzy_state = widened_scene
        .path_state_at(&fuzzy, file, offset_of(&sources, file, "fuzzy.add_line_to"))
        .expect("state");
    assert_eq!(fuzzy_state.point_count, Num::Unknown);
    assert_eq!(fuzzy_state.empty, Truth::Maybe);
}

// ---------------------------------------------------------------------------
// always_update_mobjects literal tracking (MLP227).
// ---------------------------------------------------------------------------

#[test]
fn always_update_mobjects_literal_tracking() {
    let (_sources, _facts, lifecycle) = analyze(&["always_update.py"]);
    let wait_of = |name: &str| {
        scene(&lifecycle, name)
            .plays
            .iter()
            .find(|play| play.kind == PlayKind::Wait)
            .expect("wait fact")
            .clone()
    };

    let on = wait_of("always_update.AlwaysOn");
    assert_eq!(on.always_update_mobjects, Truth::Yes);
    assert_eq!(on.dynamic_wait, Truth::Yes);

    let off = wait_of("always_update.AlwaysOff");
    assert_eq!(off.always_update_mobjects, Truth::No);
    assert_eq!(off.dynamic_wait, Truth::No);

    // A non-literal assignment is a maybe-fact, never widened away and
    // never a certain one.
    let maybe = wait_of("always_update.AlwaysMaybe");
    assert_eq!(maybe.always_update_mobjects, Truth::Maybe);
    assert_eq!(maybe.dynamic_wait, Truth::Maybe);

    // A literal constructor kwarg through super().__init__.
    let init = wait_of("always_update.AlwaysInit");
    assert_eq!(init.always_update_mobjects, Truth::Yes);
    assert_eq!(init.dynamic_wait, Truth::Yes);
}

// ---------------------------------------------------------------------------
// Callback return facts (MLC123).
// ---------------------------------------------------------------------------

#[test]
fn callback_return_facts_distinguish_no_and_maybe() {
    let (sources, _facts, lifecycle) = analyze(&["callbacks.py"]);
    let returns = &lifecycle.callback_returns;

    // Fluent helper returning its (mobject) parameter on the only path.
    let good = returns.functions.get("callbacks.good").expect("fact");
    assert_eq!(good.returns_mobject, Truth::Yes);
    assert_eq!(good.has_no_return_path, Truth::No);
    assert_eq!(good.has_bare_return_path, Truth::No);

    // No return statement at all: a definite No, not a Maybe.
    let forgot = returns
        .functions
        .get("callbacks.forgot_return")
        .expect("fact");
    assert_eq!(forgot.has_no_return_path, Truth::Yes);
    assert_eq!(forgot.returns_mobject, Truth::No);

    // A bare `return` on one path.
    let bare = returns.functions.get("callbacks.bare").expect("fact");
    assert_eq!(bare.has_bare_return_path, Truth::Yes);
    assert_eq!(bare.returns_mobject, Truth::No);
    assert_eq!(bare.has_no_return_path, Truth::No);

    // Returning an untracked value stays Maybe — never conflated with a
    // missing return.
    let obscure = returns.functions.get("callbacks.obscure").expect("fact");
    assert_eq!(obscure.returns_mobject, Truth::Maybe);
    assert_eq!(obscure.has_no_return_path, Truth::No);
    assert_eq!(obscure.has_bare_return_path, Truth::No);

    // Lambdas resolve by their source span (ApplyFunction arguments).
    let callbacks = scene(&lifecycle, "callbacks.Callbacks");
    let file = callbacks.file;
    let lambda_fact = |needle: &str| {
        let start = offset_of(&sources, file, needle);
        let end = start + u32::try_from(needle.len()).expect("fits");
        *returns.lambda_at(file, start, end).expect("lambda fact")
    };
    assert_eq!(lambda_fact("lambda m: None").returns_mobject, Truth::No);
    assert_eq!(lambda_fact("lambda m: m").returns_mobject, Truth::Yes);
    assert_eq!(lambda_fact("lambda: Square()").returns_mobject, Truth::Yes);
}

// ---------------------------------------------------------------------------
// Updater-body dataflow (MLC112 / MLP218).
// ---------------------------------------------------------------------------

#[test]
fn updater_body_dataflow_classification() {
    let (_sources, _facts, lifecycle) = analyze(&["updater_bodies.py"]);
    let bodies = scene(&lifecycle, "updater_bodies.UpdaterBodies");
    assert_eq!(bodies.updaters.len(), 5);

    // dt-scaled affine updater.
    let spin = &bodies.updaters[0].body;
    assert_eq!(spin.uses_dt, Truth::Yes);
    assert_eq!(spin.reads_frame_varying, Truth::No);
    assert_eq!(spin.mutates_target, Truth::Yes);
    assert_eq!(spin.pure_affine_on_target, Truth::Yes);
    assert_eq!(spin.calls_unknown, Truth::No);

    // One-argument updater provably reading a ValueTracker (MLC112).
    let follow = &bodies.updaters[1].body;
    assert_eq!(follow.uses_dt, Truth::No);
    assert_eq!(follow.reads_frame_varying, Truth::Yes);
    assert_eq!(follow.mutates_target, Truth::Yes);
    assert_eq!(follow.pure_affine_on_target, Truth::Yes);
    assert_eq!(follow.calls_unknown, Truth::No);

    // A call to an unresolvable helper: calls_unknown Yes and everything
    // downstream degrades to Maybe.
    let helper = &bodies.updaters[2].body;
    assert_eq!(helper.calls_unknown, Truth::Yes);
    assert_eq!(helper.reads_frame_varying, Truth::Maybe);
    assert_eq!(helper.mutates_target, Truth::Maybe);
    assert_eq!(helper.pure_affine_on_target, Truth::Maybe);

    // Inline lambda body.
    let inline = &bodies.updaters[3].body;
    assert_eq!(inline.uses_dt, Truth::No);
    assert_eq!(inline.mutates_target, Truth::Yes);
    assert_eq!(inline.pure_affine_on_target, Truth::Yes);
    assert_eq!(inline.calls_unknown, Truth::No);

    // Scene-level updater: reads the tracker, has no mobject target.
    let scene_updater = &bodies.updaters[4];
    assert_eq!(scene_updater.host, UpdaterHost::Scene);
    assert_eq!(scene_updater.body.reads_frame_varying, Truth::Yes);
    assert_eq!(scene_updater.body.uses_dt, Truth::No);
    assert_eq!(scene_updater.body.mutates_target, Truth::No);
}

// ---------------------------------------------------------------------------
// Ownership intervals (MLC111).
// ---------------------------------------------------------------------------

#[test]
fn ownership_intervals_expose_unowned_updater_spans() {
    let (sources, _facts, lifecycle) = analyze(&["ownership.py"]);
    let ownership = scene(&lifecycle, "ownership.Ownership");
    let sq = alloc_id(ownership, &sources, "Square()");
    let intervals = ownership.ownership_intervals(&sq);
    assert_eq!(intervals.len(), 7, "one interval per construct statement");

    // Before registration: provably no updaters.
    assert_eq!(intervals[0].has_updaters, Truth::No);

    // First wait: an updater-bearing object provably outside the scene
    // family and not owned by any animation — the MLC111 interval.
    let unowned = &intervals[2];
    assert_eq!(
        slice_at(
            &sources,
            unowned.site.file,
            unowned.site.start,
            unowned.site.end
        ),
        "self.wait()"
    );
    assert_eq!(unowned.in_family, Presence::Absent);
    assert_eq!(unowned.animation_target, Presence::Absent);
    assert_eq!(unowned.has_updaters, Truth::Maybe);

    // After self.add: in the family (not a violation interval).
    assert_eq!(intervals[4].in_family, Presence::Present);

    // The play statement: a live target of an in-flight animation.
    assert_eq!(intervals[6].animation_target, Presence::Present);
}

// ---------------------------------------------------------------------------
// Fixed-object registrations and camera kind (renderer-wave groundwork).
// ---------------------------------------------------------------------------

#[test]
fn fixed_registrations_and_camera_kind() {
    let (sources, _facts, lifecycle) = analyze(&["fixed_camera.py"]);
    let fixed = scene(&lifecycle, "fixed_camera.FixedScene");
    assert_eq!(fixed.camera_kind, CameraKind::ThreeD);

    let label = alloc_id(fixed, &sources, "Square()");
    assert_eq!(fixed.fixed_registrations.len(), 2);
    let register = &fixed.fixed_registrations[0];
    assert_eq!(register.object, label);
    assert_eq!(register.kind, FixedKind::InFrame);
    assert_eq!(register.action, FixedAction::Register);
    assert!(!register.renderer_divergent);
    assert_eq!(register.certainty, Presence::Present);

    // The removal carries the curated §3.5 renderer divergence.
    let removal = &fixed.fixed_registrations[1];
    assert_eq!(removal.object, label);
    assert_eq!(removal.action, FixedAction::Remove);
    assert!(removal.renderer_divergent);

    assert_eq!(
        scene(&lifecycle, "fixed_camera.MovingScene").camera_kind,
        CameraKind::MovingCamera
    );
    assert_eq!(
        scene(&lifecycle, "fixed_camera.FlatScene").camera_kind,
        CameraKind::Standard
    );
}

// ---------------------------------------------------------------------------
// z_index tracking (DESIGN §3.4 / MLP209 groundwork).
// ---------------------------------------------------------------------------

/// The final-heap `z_index` fact of the object allocated at `text`.
fn z_of(scene: &SceneLifecycle, sources: &SourceManager, text: &str) -> Num {
    let id = alloc_id(scene, sources, text);
    let resolved = scene.final_heap.resolve(&id);
    scene
        .final_heap
        .object(&resolved)
        .unwrap_or_else(|| panic!("state for {text}"))
        .z_index
        .clone()
}

#[test]
fn z_index_defaults_kwargs_and_literal_set() {
    let (sources, _facts, lifecycle) = analyze(&["z_index_scenes.py"]);
    let facts = scene(&lifecycle, "z_index_scenes.ZFacts");
    // A curated constructor with no z_index kwarg proves the Mobject
    // default (mobject.py `z_index: float = 0`).
    assert_eq!(z_of(facts, &sources, "Square()"), Num::int(0));
    // A literal kwarg is exact.
    assert_eq!(z_of(facts, &sources, "Square(z_index=2)"), Num::int(2));
    // A literal (negated) `set_z_index` argument is exact.
    assert_eq!(z_of(facts, &sources, "Circle()"), Num::int(-1));
}

#[test]
fn z_index_family_write_joins_children() {
    let (sources, _facts, lifecycle) = analyze(&["z_index_scenes.py"]);
    let family = scene(&lifecycle, "z_index_scenes.ZFamily");
    // The receiver takes the literal exactly.
    assert_eq!(z_of(family, &sources, "VGroup(a, b)"), Num::int(2));
    // Children are may-children, so their fact is the hull — never the
    // asserted exact value, never a stale `0`.
    assert_eq!(
        z_of(family, &sources, "Square()"),
        Num::Interval {
            lo: Some(0.0),
            hi: Some(2.0),
        }
    );
}

// ---------------------------------------------------------------------------
// Helper plays materialize as real lifecycle facts (DESIGN §2.1 "Scene
// helper", §5.1 step 5): bounded inlining of `self.<helper>()` calls.
// ---------------------------------------------------------------------------

#[test]
fn helper_play_materializes_at_the_helper_site() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let helper = scene(&lifecycle, "helper_plays.HelperPlay");
    assert_eq!(helper.plays.len(), 1, "the helper play is a real fact");
    let play = &helper.plays[0];
    assert_eq!(play.kind, PlayKind::Play);
    assert_eq!(play.certainty, Presence::Present);
    // The literal FadeIn run_time flows through: MLC104's zero-duration
    // evidence.
    assert_eq!(play.duration, Num::int(0));
    // The site is the play call inside the helper body, not the call
    // statement in construct.
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "self.play(FadeIn(mob, run_time=0))"
    );
    assert_eq!(play.call_path.len(), 1);
    assert_eq!(
        slice_at(
            &sources,
            play.call_path[0].file,
            play.call_path[0].start,
            play.call_path[0].end,
        ),
        "self.show(Square())"
    );

    // The parameter bound to the caller's square: introducer effects
    // applied exactly once against the live caller state.
    let sq = alloc_id(helper, &sources, "Square()");
    let state = play.animations[0].state.as_ref().expect("state");
    assert_eq!(state.introducer, Truth::Yes);
    assert!(state.targets.contains(&sq));
    assert_eq!(state.run_time, Num::int(0));
    assert_eq!(
        helper.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );

    // The pre-play state is queryable at the helper play site (the
    // liveness anchor): the introducer's target is not yet a member.
    assert_eq!(
        helper.membership_at(&sq, play.site.file, play.site.start),
        Some((Presence::Absent, Presence::Absent))
    );
}

#[test]
fn two_call_sites_produce_one_play_fact_each() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let two = scene(&lifecycle, "helper_plays.TwoCalls");
    assert_eq!(two.plays.len(), 2, "one fact per call site");
    let (first, second) = (&two.plays[0], &two.plays[1]);
    // Same play span in the helper, distinguished by call-site context.
    assert_eq!(first.site, second.site);
    assert_ne!(first.call_path, second.call_path);
    assert_eq!(
        slice_at(
            &sources,
            first.call_path[0].file,
            first.call_path[0].start,
            first.call_path[0].end,
        ),
        "self.flash(a)"
    );
    assert_eq!(
        slice_at(
            &sources,
            second.call_path[0].file,
            second.call_path[0].start,
            second.call_path[0].end,
        ),
        "self.flash(b)"
    );
    // Each fact carries its own call's live target and identity.
    let a = alloc_id(two, &sources, "Square()");
    let b = alloc_id(two, &sources, "Circle()");
    let first_state = first.animations[0].state.as_ref().expect("state");
    let second_state = second.animations[0].state.as_ref().expect("state");
    assert!(first_state.targets.contains(&a));
    assert!(second_state.targets.contains(&b));
    assert_ne!(
        first.animations[0].animation,
        second.animations[0].animation
    );
    // The play-level run_time kwarg applies inside the helper.
    assert_eq!(first.duration, Num::int(2));
}

#[test]
fn branch_reached_helper_play_is_a_maybe_fact() {
    let (_sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let branch = scene(&lifecycle, "helper_plays.BranchCall");
    assert_eq!(branch.plays.len(), 1);
    // The call site's branch dependence composes into the play fact:
    // never a certain claim from a conditional helper call.
    assert_eq!(branch.plays[0].certainty, Presence::Maybe);
}

#[test]
fn helper_chain_inlines_with_the_full_call_path() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let chain = scene(&lifecycle, "helper_plays.ChainCall");
    assert_eq!(chain.plays.len(), 1);
    let play = &chain.plays[0];
    let path: Vec<&str> = play
        .call_path
        .iter()
        .map(|site| slice_at(&sources, site.file, site.start, site.end))
        .collect();
    assert_eq!(path, vec!["self.outer(Square())", "self.inner(mob)"]);
    assert_eq!(play.certainty, Presence::Present);
}

#[test]
fn recursive_helper_fallback_rehydrates_a_summary_derived_play() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let recursive = scene(&lifecycle, "helper_plays.RecursivePlay");
    // The first inline level materializes its play as a traced fact; the
    // cycle-closing recursive call falls back to the effect summary,
    // whose play record rehydrates as a conservative summary-derived
    // fact — the recursion no longer loses its plays, and the analysis
    // still terminates.
    assert_eq!(recursive.plays.len(), 2);
    let traced = &recursive.plays[0];
    assert_eq!(traced.certainty, Presence::Present);
    assert_eq!(traced.call_path.len(), 1);
    assert!(!recursive.summary_derived_plays.contains(&traced.play_group));

    let derived = &recursive.plays[1];
    assert!(
        recursive
            .summary_derived_plays
            .contains(&derived.play_group)
    );
    // Same play span in the helper file, reached through the recursive
    // call: construct's call plus the cycle-closing call.
    assert_eq!(derived.site, traced.site);
    assert_eq!(derived.call_path.len(), 2);
    assert_eq!(
        slice_at(
            &sources,
            derived.call_path[1].file,
            derived.call_path[1].start,
            derived.call_path[1].end,
        ),
        "self.ping(mob)"
    );
    // Caller-state-dependent judgments are degraded, never fabricated:
    // the §3.2 pipeline did not run for the rehydrated fact.
    assert_eq!(derived.certainty, Presence::Maybe);
    assert_eq!(
        derived.repetitions,
        Num::Interval {
            lo: Some(0.0),
            hi: None
        }
    );
    assert_eq!(derived.animations.len(), 1);
    assert!(derived.animations[0].animation.is_none());
    assert!(derived.animations[0].state.is_none());
    assert_eq!(derived.animations[0].convertible, Truth::Maybe);
    assert_eq!(derived.animations[0].channels_known, Truth::Maybe);
    // FadeIn without a literal run_time: the duration is honestly
    // unknown (no fabricated numbers).
    assert_eq!(derived.duration, Num::Unknown);

    // The fallback frontier is reported for coverage.
    let fallback = lifecycle
        .inline_fallbacks
        .iter()
        .find(|fact| fact.callee == "helper_plays.RecursivePlay.ping")
        .expect("recursion fallback recorded");
    assert_eq!(fallback.reason, interpreter::FallbackReason::Recursion);
    assert_eq!(
        slice_at(
            &sources,
            fallback.site.file,
            fallback.site.start,
            fallback.site.end,
        ),
        "self.ping(mob)"
    );
}

/// A recursive helper whose play carries a literal duration: the
/// summary-derived fact keeps the literal (MLC104-style checks still
/// work) while the wait-freeze verdict honestly degrades to `Maybe`.
#[test]
fn recursive_wait_keeps_the_literal_duration_with_maybe_liveness() {
    let (_sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let recursive = scene(&lifecycle, "helper_plays.RecursiveWait");
    assert_eq!(recursive.plays.len(), 2);
    let derived = recursive
        .plays
        .iter()
        .find(|play| recursive.summary_derived_plays.contains(&play.play_group))
        .expect("summary-derived wait");
    assert_eq!(derived.kind, PlayKind::Wait);
    assert_eq!(derived.duration, Num::int(3));
    assert_eq!(derived.certainty, Presence::Maybe);
    assert_eq!(derived.dynamic_wait, Truth::Maybe);
    assert_eq!(derived.always_update_mobjects, Truth::Maybe);
}

/// Inlining is governed by cycle detection, not a small fixed depth:
/// acyclic helper chains 4, 6, and 10 deep all materialize the deepest
/// play as a real fact with the full call chain.
#[test]
fn deep_acyclic_helper_chains_materialize_the_deepest_play() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);

    let four = scene(&lifecycle, "helper_plays.DeepFour");
    assert_eq!(four.plays.len(), 1, "4-deep chain keeps its play");
    assert_eq!(four.plays[0].call_path.len(), 4);
    assert_eq!(four.plays[0].certainty, Presence::Present);

    // At depth 6 the literal run_time=0 still flows through exactly:
    // MLC104's zero-duration evidence survives the whole chain.
    let six = scene(&lifecycle, "helper_plays.DeepSix");
    assert_eq!(six.plays.len(), 1, "6-deep chain keeps its play");
    let play = &six.plays[0];
    assert_eq!(play.call_path.len(), 6);
    assert_eq!(play.duration, Num::int(0));
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "self.play(FadeIn(mob, run_time=0))"
    );

    let ten = scene(&lifecycle, "helper_plays.DeepTen");
    assert_eq!(ten.plays.len(), 1, "10-deep chain keeps its play");
    assert_eq!(ten.plays[0].call_path.len(), 10);
    assert_eq!(ten.plays[0].certainty, Presence::Present);
}

/// Mutual recursion (ping calls pong calls ping) terminates at the cycle
/// guard: each method inlines once as a traced fact, and the
/// cycle-closing call rehydrates the summary's play records — one
/// conservative fact per frontier site, none fabricated further down the
/// alternating chain. If the safety depth cap were deciding instead,
/// dozens of traced plays would materialize.
#[test]
fn mutual_recursion_hits_the_cycle_guard_not_the_depth_cap() {
    let (_sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let mutual = scene(&lifecycle, "helper_plays.MutualPlay");
    let traced: Vec<_> = mutual
        .plays
        .iter()
        .filter(|play| !mutual.summary_derived_plays.contains(&play.play_group))
        .collect();
    let derived: Vec<_> = mutual
        .plays
        .iter()
        .filter(|play| mutual.summary_derived_plays.contains(&play.play_group))
        .collect();
    assert_eq!(traced.len(), 2, "one traced play per inlined level");
    let mut depths: Vec<usize> = traced.iter().map(|play| play.call_path.len()).collect();
    depths.sort_unstable();
    assert_eq!(depths, vec![1, 2]);
    // The frontier records cover both sites of the ping/pong cycle,
    // reached through the cycle-closing call (depth 3), all maybe-facts.
    assert_eq!(derived.len(), 2, "one record per frontier play site");
    assert!(derived.iter().all(|play| play.certainty == Presence::Maybe));
    assert!(derived.iter().all(|play| play.call_path.len() == 3));
}

/// One helper reached through another from five call sites: exactly one
/// play fact per site (no exponential re-expansion), each with its own
/// two-level call path.
#[test]
fn wide_chain_produces_exactly_one_play_fact_per_call_site() {
    let (_sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let wide = scene(&lifecycle, "helper_plays.WideChain");
    assert_eq!(wide.plays.len(), 5, "one fact per call site, no blowup");
    let distinct_paths: std::collections::BTreeSet<_> = wide
        .plays
        .iter()
        .map(|play| play.call_path.clone())
        .collect();
    assert_eq!(distinct_paths.len(), 5);
    assert!(wide.plays.iter().all(|play| play.call_path.len() == 2));
    // The shared play span in the glow helper anchors every fact.
    let distinct_sites: std::collections::BTreeSet<_> =
        wide.plays.iter().map(|play| play.site).collect();
    assert_eq!(distinct_sites.len(), 1);
}

#[test]
fn module_level_helper_play_materializes_like_a_method_helper() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let module = scene(&lifecycle, "helper_plays.ModuleHelper");
    // `flourish(self, sq)` resolves to the project module-level function:
    // scene runs inline it exactly like a self-method helper — the scene
    // argument flows as the live scene state and the play materializes as
    // a real traced fact anchored at the play call in the helper body.
    assert_eq!(module.plays.len(), 1);
    let play = &module.plays[0];
    assert!(!module.summary_derived_plays.contains(&play.play_group));
    assert_eq!(play.kind, PlayKind::Play);
    assert_eq!(play.certainty, Presence::Present);
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "scene.play(FadeIn(mob))"
    );
    assert_eq!(play.call_path.len(), 1);
    assert_eq!(
        slice_at(
            &sources,
            play.call_path[0].file,
            play.call_path[0].start,
            play.call_path[0].end,
        ),
        "flourish(self, sq)"
    );
    // The caller's square is the introducer target and ends as a member:
    // helper effects applied exactly once against the live caller state.
    let sq = alloc_id(module, &sources, "Square()");
    let state = play.animations[0].state.as_ref().expect("state");
    assert_eq!(state.introducer, Truth::Yes);
    assert!(state.targets.contains(&sq));
    assert_eq!(
        module.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
}

#[test]
fn helper_wait_dynamics_use_the_caller_state() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let waits = scene(&lifecycle, "helper_plays.WaitHelper");
    let wait = waits
        .plays
        .iter()
        .find(|play| play.kind == PlayKind::Wait)
        .expect("wait fact");
    // The dt updater registered in construct makes the helper's wait
    // dynamic: the freeze verdict is judged on the live caller heap.
    assert_eq!(wait.dynamic_wait, Truth::Yes);
    assert_eq!(wait.duration, Num::int(2));
    assert_eq!(
        slice_at(&sources, wait.site.file, wait.site.start, wait.site.end),
        "self.wait(2)"
    );
    assert_eq!(wait.call_path.len(), 1);
}

#[test]
fn module_alias_helper_play_materializes_identically() {
    let (sources, _facts, lifecycle) = analyze(&["helper_plays.py"]);
    let alias = scene(&lifecycle, "helper_plays.AliasHelper");
    assert_eq!(alias.plays.len(), 1);
    let play = &alias.plays[0];
    assert_eq!(play.certainty, Presence::Present);
    assert_eq!(play.duration, Num::int(0));
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "self.play(mn.FadeIn(mob, run_time=0))"
    );
}

#[test]
fn z_index_unknown_on_unproven_writes() {
    let (sources, _facts, lifecycle) = analyze(&["z_index_scenes.py"]);
    let unknown = scene(&lifecycle, "z_index_scenes.ZUnknown");
    // A non-literal `set_z_index` argument widens.
    assert_eq!(z_of(unknown, &sources, "Square()"), Num::Unknown);
    // A `**kwargs` constructor splat may carry z_index.
    assert_eq!(
        z_of(unknown, &sources, "Square(**{\"z_index\": 2})"),
        Num::Unknown
    );
    // A summary-applied helper's mutate widens instead of staying stale
    // (the recursive helper's cycle-closing call falls back to the §5.7
    // summary; its Style mutate must not leave the inline-traced exact
    // write in place).
    assert_eq!(z_of(unknown, &sources, "Circle()"), Num::Unknown);
    // `family=False` still writes the receiver exactly.
    assert_eq!(z_of(unknown, &sources, "Dot()"), Num::int(4));
}

// ---------------------------------------------------------------------------
// Helper execution identity: `.animate` on a helper parameter (per-call-site
// durations, play-kwarg run_time decomposition, builder-fact merging).
// ---------------------------------------------------------------------------

/// The play-level `run_time` literal decides the whole-play duration for
/// every execution of a helper play, even when the call sites pass
/// *different* mobjects to a `.animate` builder on the parameter
/// (scene.py `compile_animations` `setattr`s the kwarg onto every
/// animation, so `get_run_time` is the kwarg itself).
#[test]
fn animate_helper_keeps_exact_duration_across_differing_call_site_targets() {
    let (sources, _facts, lifecycle) = analyze(&["animate_helper.py"]);
    let two = scene(&lifecycle, "animate_helper.TwoTargets");
    assert_eq!(two.plays.len(), 2, "one play fact per call site");
    let a = alloc_id(two, &sources, "Square()");
    let b = alloc_id(two, &sources, "Circle()");
    for play in &two.plays {
        assert_eq!(play.duration, Num::int(2), "the play kwarg survives");
        assert_eq!(play.repetitions, Num::int(1));
        assert_eq!(play.certainty, Presence::Present);
    }
    // Each execution resolves its own builder target.
    let first_state = two.plays[0].animations[0].state.as_ref().expect("state");
    let second_state = two.plays[1].animations[0].state.as_ref().expect("state");
    assert!(first_state.targets.contains(&a));
    assert!(second_state.targets.contains(&b));
    assert_eq!(first_state.run_time, Num::int(2));
    assert_eq!(second_state.run_time, Num::int(2));
    assert_ne!(two.plays[0].call_path, two.plays[1].call_path);

    // The per-site builder view never mixes the executions: with two
    // different live targets it makes no cross-execution identity claim,
    // and the disagreeing epoch pairs ((0, 0) for the fresh `a`, (1, 1)
    // for the pre-mutated `b`) drop the play epoch instead of pairing
    // `a`'s creation epoch with `b`'s play epoch. "Some execution
    // definitely played" survives.
    let builder = two.builders.values().next().expect("builder fact");
    assert_eq!(builder.target, None, "no cross-execution identity claim");
    assert_eq!(builder.target_epoch_at_play, None, "no mixed epoch pair");
    assert_eq!(builder.played, Truth::Yes);
}

/// Same-argument calls keep the full per-site builder identity.
#[test]
fn animate_helper_same_argument_calls_keep_builder_identity() {
    let (sources, _facts, lifecycle) = analyze(&["animate_helper.py"]);
    let same = scene(&lifecycle, "animate_helper.SameTarget");
    assert_eq!(same.plays.len(), 2);
    let a = alloc_id(same, &sources, "Square()");
    for play in &same.plays {
        assert_eq!(play.duration, Num::int(2));
        let state = play.animations[0].state.as_ref().expect("state");
        assert!(state.targets.contains(&a));
    }
    let builder = same.builders.values().next().expect("builder fact");
    assert_eq!(builder.target, Some(a));
    assert_eq!(builder.played, Truth::Yes);
}

/// A helper call site inside a literal `range(3)` loop composes into the
/// play fact's repetition interval (`[0, 3]`), while the direct call
/// site stays an exact single execution — frame totals multiply per
/// execution downstream (DESIGN §4.1).
#[test]
fn call_site_loop_trip_counts_compose_into_helper_play_repetitions() {
    let (_sources, _facts, lifecycle) = analyze(&["animate_helper.py"]);
    let looped = scene(&lifecycle, "animate_helper.LoopedCall");
    assert_eq!(looped.plays.len(), 2, "one fact per call site");
    let loop_play = looped
        .plays
        .iter()
        .find(|play| play.certainty == Presence::Maybe)
        .expect("the looped call site is branch-dependent");
    assert_eq!(
        loop_play.repetitions,
        Num::Interval {
            lo: Some(0.0),
            hi: Some(3.0),
        },
        "the caller's literal trip count threads through inlining"
    );
    assert_eq!(loop_play.duration, Num::int(2));
    let direct_play = looped
        .plays
        .iter()
        .find(|play| play.certainty == Presence::Present)
        .expect("the direct call site is certain");
    assert_eq!(direct_play.repetitions, Num::int(1));
    assert_eq!(direct_play.duration, Num::int(2));
}

/// Even a genuinely unknown helper argument (an unresolvable factory
/// call) keeps the literal play-level `run_time`: the kwarg overrides
/// every animation regardless of target identity, so the duration is
/// exact while the animation itself honestly stays untracked.
#[test]
fn play_kwarg_run_time_survives_an_unknown_animate_target() {
    let (_sources, _facts, lifecycle) = analyze(&["animate_helper.py"]);
    let unknown = scene(&lifecycle, "animate_helper.UnknownArg");
    assert_eq!(unknown.plays.len(), 1);
    let play = &unknown.plays[0];
    assert_eq!(play.duration, Num::int(2));
    assert_eq!(play.certainty, Presence::Present);
    // The argument value stays honestly untracked (convertible: Maybe).
    assert_eq!(play.animations[0].convertible, Truth::Maybe);
    assert!(play.animations[0].state.is_none());
}

/// The reverse direction of the same decomposition (DESIGN §15
/// invariant 2): a *non-literal* play-level `run_time` — or a `**kwargs`
/// splat that may carry one — overrides every animation's run time with
/// an unknowable value, so constructor literals must not survive as an
/// exact duration.
#[test]
fn non_literal_play_run_time_widens_constructor_literals() {
    let (_sources, _facts, lifecycle) = analyze(&["animate_helper.py"]);

    let override_scene = scene(&lifecycle, "animate_helper.NonLiteralOverride");
    assert_eq!(override_scene.plays.len(), 1);
    let play = &override_scene.plays[0];
    assert_eq!(
        play.duration,
        Num::Unknown,
        "run_time=t overrides FadeIn's 2"
    );
    let state = play.animations[0].state.as_ref().expect("state");
    assert_eq!(state.run_time, Num::Unknown);

    let splat_scene = scene(&lifecycle, "animate_helper.SplatOverride");
    assert_eq!(splat_scene.plays.len(), 1);
    let play = &splat_scene.plays[0];
    assert_eq!(play.duration, Num::Unknown, "**kw may carry run_time");
    let state = play.animations[0].state.as_ref().expect("state");
    assert_eq!(state.run_time, Num::Unknown);
}

// ---------------------------------------------------------------------------
// Imported helpers (cross-file), module-level binding forms, and the
// depth-cap play-visibility guarantee.
// ---------------------------------------------------------------------------

#[test]
fn imported_helper_play_materializes_in_the_helper_file() {
    let (sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);
    let imported = scene(&lifecycle, "imported_helpers.ImportedHelper");
    assert_eq!(imported.plays.len(), 1);
    let play = &imported.plays[0];
    assert!(!imported.summary_derived_plays.contains(&play.play_group));
    assert_eq!(play.certainty, Presence::Present);
    // The play-level run_time kwarg written in the helper applies.
    assert_eq!(play.duration, Num::int(2));
    // The fact is anchored at the play call in the *other* file.
    assert_ne!(play.site.file, imported.file);
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "scene.play(FadeIn(mob), run_time=2)"
    );
    assert_eq!(play.call_path.len(), 1);
    assert_eq!(play.call_path[0].file, imported.file);
    assert_eq!(
        slice_at(
            &sources,
            play.call_path[0].file,
            play.call_path[0].start,
            play.call_path[0].end,
        ),
        "flourish(self, sq)"
    );
    // The caller's square is the target and ends as a member.
    let sq = alloc_id(imported, &sources, "Square()");
    let state = play.animations[0].state.as_ref().expect("state");
    assert!(state.targets.contains(&sq));
    assert_eq!(
        imported.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
}

#[test]
fn updater_registered_inside_an_imported_helper_appears_in_facts() {
    let (sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);
    let updater = scene(&lifecycle, "imported_helpers.ImportedUpdater");
    let sq = alloc_id(updater, &sources, "Square()");
    assert_eq!(updater.updaters.len(), 1);
    let registration = &updater.updaters[0];
    assert_eq!(registration.host, UpdaterHost::Mobject(sq.clone()));
    assert_eq!(registration.certainty, Presence::Present);
    // Registered at the add_updater call inside the helper file.
    assert_ne!(registration.site.file, updater.file);
    assert!(
        slice_at(
            &sources,
            registration.site.file,
            registration.site.start,
            registration.site.end,
        )
        .starts_with("mob.add_updater")
    );
    // The dt-lambda classifies through the shared callback machinery.
    assert!(matches!(registration.fact.callback, CallbackRef::Lambda(_)));
    assert_eq!(registration.body.uses_dt, Truth::Yes);
    // scene.add(mob) inside the helper applied against the caller state.
    assert_eq!(
        updater.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );
}

#[test]
fn scene_bound_by_keyword_in_second_position_still_inlines() {
    let (sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);
    let second = scene(&lifecycle, "imported_helpers.SceneSecond");
    // `tag(sq, scene=self)`: the scene binds by keyword into the second
    // parameter and still flows as the live scene state.
    assert_eq!(second.plays.len(), 1);
    let play = &second.plays[0];
    assert_eq!(play.certainty, Presence::Present);
    assert_eq!(
        slice_at(&sources, play.site.file, play.site.start, play.site.end),
        "scene.play(FadeIn(mob))"
    );
    let sq = alloc_id(second, &sources, "Square()");
    let state = play.animations[0].state.as_ref().expect("state");
    assert!(state.targets.contains(&sq));
}

#[test]
fn imported_factory_and_animation_returns_keep_identity() {
    let (sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);

    // Factory: the allocation site lives inside make_square (other file);
    // the returned alias is what construct adds.
    let factory = scene(&lifecycle, "imported_helpers.ImportedFactory");
    let sq = alloc_id(factory, &sources, "Square()");
    assert_ne!(sq.site.file, factory.file);
    assert_eq!(
        factory.final_membership(&sq),
        Some((Presence::Present, Presence::Present))
    );

    // Animation factory: the FadeIn created inside the helper is the
    // played animation, targeting the caller's square.
    let animation = scene(&lifecycle, "imported_helpers.ImportedAnimation");
    assert_eq!(animation.plays.len(), 1);
    let play = &animation.plays[0];
    assert!(!animation.summary_derived_plays.contains(&play.play_group));
    let played = &play.animations[0];
    assert!(played.animation.is_some());
    let state = played.state.as_ref().expect("animation state tracked");
    assert_eq!(state.introducer, Truth::Yes);
    let target = alloc_id(animation, &sources, "Square()");
    assert!(state.targets.contains(&target));
}

#[test]
fn star_args_forwarding_stays_conservative() {
    let (_sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);
    let star = scene(&lifecycle, "imported_helpers.StarForward");
    // `flourish(self, *args)` voids positional binding: the scene
    // parameter stays Unknown inside the helper, so no play fact is
    // fabricated and nothing becomes a certain effect.
    assert!(star.plays.is_empty());
}

#[test]
fn third_party_imported_call_stays_unknown() {
    let (sources, _facts, lifecycle) = analyze(&["imported_helpers.py", "helper_lib.py"]);
    let third = scene(&lifecycle, "imported_helpers.ThirdParty");
    // `boost` comes from an unresolvable third-party module: never
    // inlined, no plays fabricated, and the scene state widens.
    assert!(third.plays.is_empty());
    let sq = alloc_id(third, &sources, "Square()");
    let (root, _family) = third.final_membership(&sq).expect("tracked");
    assert_ne!(root, Presence::Present);
    // A third-party call is not an inline fallback: it was never an
    // inline candidate (the frontier lists only resolved project defs).
    assert!(lifecycle.inline_fallbacks.is_empty());
}

// Depth-cap play visibility: the `FallbackReason::DepthCap` frontier
// takes the *same* fallback branch as `Recursion` above (one shared
// reason check in `inline_fallback_reason`, one shared summary
// application and play-record rehydration), so the recursion tests cover
// the rehydration semantics. An end-to-end 32-deep acyclic chain is not
// exercisable as a test: each inline level runs the callee body once in
// the caller's fixpoint pass and once in the emitting pass, so a chain of
// depth d costs ~2^d body executions (measured ×1.9 per level) — which is
// exactly why the cap is a work-bounding backstop that never decides
// ordinary programs.

/// Summary tables expose the play records directly: a recursive helper's
/// summary keeps its play site with the literal-derived duration.
#[test]
fn method_summaries_carry_play_records() {
    let (sources, facts, _lifecycle) = analyze(&["helper_plays.py"]);
    let profile = knowledge::load(knowledge::DEFAULT_PROFILE).expect("profile loads");
    let defs = DefMap::build(&sources, &facts.index);
    let table = summaries::build(&sources, &facts.index, &facts.calls, Some(&profile), &defs);
    let pulse = table
        .get("helper_plays.RecursiveWait.pulse")
        .expect("summary");
    assert_eq!(pulse.plays.len(), 1, "one record per site and kind");
    let record = &pulse.plays[0];
    assert_eq!(record.kind, PlayKind::Wait);
    assert_eq!(record.duration, Num::int(3));
    assert_eq!(
        slice_at(
            &sources,
            record.site.file,
            record.site.start,
            record.site.end
        ),
        "self.wait(3)"
    );
}
