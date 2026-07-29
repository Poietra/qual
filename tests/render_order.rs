//! Cairo effective display order and moving-suffix tests (DESIGN §3.4,
//! §4.3 Cairo stage, §7.3 `MLP209` prose): family flatten + `z_index`
//! stable sort + foreground, re-add order effects, Unknown poisoning, and
//! the quantification refusal on unknown orders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use qual::application::manim_surface;
use qual::frontend::index::{FrontendFacts, analyze as frontend_analyze};
use qual::knowledge;
use qual::render_order::{
    DisplayOrder, MemberFacts, MemberProvenance, MovingReason, MovingScope, OrderUnknownReason,
    RenderOrderInputs, SuffixFact, display_order_at_play, inputs_at_play, is_order_known,
    moving_scope_at_play, moving_suffix_at_play, moving_suffix_evidence,
};
use qual::semantic::events::Event;
use qual::semantic::interpreter::{self, LifecycleFacts, PlayKind, SceneLifecycle};
use qual::semantic::values::{AllocationSite, CallContextId, Cardinality, Num, ObjectId, Truth};
use qual::source::{FileId, SourceManager};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/render_order")
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
    let mut found: Option<ObjectId> = None;
    for traced in &scene.events {
        if let Event::Alloc(alloc) = &traced.event {
            let slice = slice_at(
                sources,
                traced.site.file,
                traced.site.start,
                traced.site.end,
            );
            if slice == text {
                assert!(
                    found.is_none() || found.as_ref() == Some(&alloc.object),
                    "allocation slice {text} must be unique"
                );
                found = Some(alloc.object.clone());
            }
        }
    }
    found.unwrap_or_else(|| panic!("allocation {text} found"))
}

/// A synthetic singleton object id for pure display-order tests.
fn mk_id(file: FileId, offset: u32) -> ObjectId {
    ObjectId::new(
        AllocationSite {
            file,
            start: offset,
            end: offset + 1,
        },
        CallContextId::default(),
        Cardinality::Singleton,
    )
}

fn any_file_id() -> FileId {
    let (sources, _, _) = analyze(&["order_scenes.py"]);
    sources.files()[0].id()
}

fn ids(order: &DisplayOrder) -> Vec<ObjectId> {
    order
        .members()
        .expect("order known")
        .iter()
        .map(|member| member.id.clone())
        .collect()
}

fn fill_z(inputs: &mut RenderOrderInputs, z: i64) {
    for facts in inputs.members.values_mut() {
        facts.z_index = Num::int(z);
    }
}

// ---------------------------------------------------------------------------
// Pure display-order computation.
// ---------------------------------------------------------------------------

#[test]
fn known_scene_order_is_deterministic() {
    let file = any_file_id();
    let a = mk_id(file, 1);
    let group = mk_id(file, 2);
    let c1 = mk_id(file, 3);
    let c2 = mk_id(file, 4);
    let fg = mk_id(file, 5);
    let mut members = BTreeMap::new();
    members.insert(a.clone(), MemberFacts::leaf(Num::int(0)));
    members.insert(
        group.clone(),
        MemberFacts {
            children: vec![c1.clone(), c2.clone()],
            children_order_known: Truth::Yes,
            z_index: Num::int(0),
            own_updaters: Truth::No,
            foreground: Truth::No,
        },
    );
    members.insert(c1.clone(), MemberFacts::leaf(Num::int(0)));
    // The one z_index set: c2 sorts after every z=0 member.
    members.insert(c2.clone(), MemberFacts::leaf(Num::int(1)));
    let mut fg_facts = MemberFacts::leaf(Num::int(0));
    fg_facts.foreground = Truth::Yes;
    members.insert(fg.clone(), fg_facts);
    let inputs = RenderOrderInputs {
        roots: vec![a.clone(), group.clone(), fg.clone()],
        roots_order_known: Truth::Yes,
        foreground: vec![fg.clone()],
        foreground_order_known: Truth::Yes,
        members,
    };

    let order = DisplayOrder::compute(&inputs);
    assert!(is_order_known(&order));
    // Flatten [a, group, c1, c2] + foreground [fg], stable z sort moves
    // only c2 (z=1) to the end.
    assert_eq!(
        ids(&order),
        vec![a.clone(), group.clone(), c1.clone(), fg.clone(), c2.clone()]
    );
    let members = order.members().expect("known");
    assert_eq!(
        members[0].provenance,
        MemberProvenance::Root { position: 0 }
    );
    assert_eq!(
        members[2].provenance,
        MemberProvenance::Submobject {
            root: group.clone(),
            parent: group.clone(),
        }
    );
    assert_eq!(
        members[3].provenance,
        MemberProvenance::ForegroundRoot { position: 0 }
    );
    // Determinism: identical inputs, identical order.
    assert_eq!(order, DisplayOrder::compute(&inputs));
}

#[test]
fn unknown_z_index_poisons_order_downstream() {
    let file = any_file_id();
    let a = mk_id(file, 1);
    let b = mk_id(file, 2);
    let c = mk_id(file, 3);
    let mut members = BTreeMap::new();
    members.insert(a.clone(), MemberFacts::leaf(Num::int(0)));
    members.insert(b.clone(), MemberFacts::leaf(Num::Unknown));
    members.insert(c.clone(), MemberFacts::leaf(Num::int(0)));
    let inputs = RenderOrderInputs {
        roots: vec![a, b.clone(), c],
        roots_order_known: Truth::Yes,
        foreground: Vec::new(),
        foreground_order_known: Truth::Yes,
        members,
    };
    let order = DisplayOrder::compute(&inputs);
    // The first flatten-order member with unknown z poisons the order.
    assert_eq!(
        order,
        DisplayOrder::Unknown(OrderUnknownReason::ZIndexUnknown { member: b })
    );
    assert!(!is_order_known(&order));
    // No quantitative fact can be built on an unknown order.
    let scope = MovingScope {
        animation_targets: Vec::new(),
        camera_moving: Truth::No,
        unattributed_movers: false,
    };
    assert_eq!(SuffixFact::compute(&order, &inputs, &scope), None);
}

#[test]
fn updater_at_front_yields_large_static_suffix() {
    let file = any_file_id();
    let updater_bearer = mk_id(file, 1);
    let mut members = BTreeMap::new();
    let mut front = MemberFacts::leaf(Num::int(0));
    front.own_updaters = Truth::Yes;
    members.insert(updater_bearer.clone(), front);
    let mut roots = vec![updater_bearer.clone()];
    for offset in 2..=6 {
        let id = mk_id(file, offset);
        members.insert(id.clone(), MemberFacts::leaf(Num::int(0)));
        roots.push(id);
    }
    let inputs = RenderOrderInputs {
        roots,
        roots_order_known: Truth::Yes,
        foreground: Vec::new(),
        foreground_order_known: Truth::Yes,
        members,
    };
    let order = DisplayOrder::compute(&inputs);
    let scope = MovingScope {
        animation_targets: Vec::new(),
        camera_moving: Truth::No,
        unattributed_movers: false,
    };
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    assert_eq!(suffix.total_members, 6);
    assert_eq!(suffix.first_moving_index, Some(Num::int(0)));
    assert_eq!(suffix.suffix_len, Num::int(5));
    assert_eq!(suffix.members_evidence.len(), 1);
    assert_eq!(suffix.members_evidence[0].index, 0);
    assert_eq!(
        suffix.members_evidence[0].reason,
        MovingReason::FamilyUpdater
    );
    assert_eq!(suffix.members_evidence[0].certainty, Truth::Yes);
    let evidence = moving_suffix_evidence(&order, &suffix).expect("evidence");
    assert!(evidence.contains("position 1/6"), "{evidence}");
    assert!(
        evidence.contains("Approximately 5 later family members"),
        "{evidence}"
    );
}

#[test]
fn maybe_camera_motion_widens_bounds_to_interval() {
    let file = any_file_id();
    let mut members = BTreeMap::new();
    let mut roots = Vec::new();
    for offset in 1..=4 {
        let id = mk_id(file, offset);
        let mut facts = MemberFacts::leaf(Num::int(0));
        if offset == 3 {
            facts.own_updaters = Truth::Yes;
        }
        members.insert(id.clone(), facts);
        roots.push(id);
    }
    let inputs = RenderOrderInputs {
        roots,
        roots_order_known: Truth::Yes,
        foreground: Vec::new(),
        foreground_order_known: Truth::Yes,
        members,
    };
    let order = DisplayOrder::compute(&inputs);
    let scope = MovingScope {
        animation_targets: Vec::new(),
        camera_moving: Truth::Maybe,
        unattributed_movers: false,
    };
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    // The certain first mover is index 2 (family updater); a maybe-moving
    // camera could make index 0 the first mover.
    assert_eq!(
        suffix.first_moving_index,
        Some(Num::Interval {
            lo: Some(0.0),
            hi: Some(2.0),
        })
    );
    assert_eq!(
        suffix.suffix_len,
        Num::Interval {
            lo: Some(1.0),
            hi: Some(3.0),
        }
    );
    let evidence = moving_suffix_evidence(&order, &suffix).expect("evidence");
    assert!(
        evidence.contains("position between 1 and 3 of 4"),
        "{evidence}"
    );
    assert!(
        evidence.contains("Between 1 and 3 later family members"),
        "{evidence}"
    );
}

#[test]
fn certain_camera_motion_moves_the_whole_scene() {
    let file = any_file_id();
    let mut members = BTreeMap::new();
    let mut roots = Vec::new();
    for offset in 1..=3 {
        let id = mk_id(file, offset);
        members.insert(id.clone(), MemberFacts::leaf(Num::int(0)));
        roots.push(id);
    }
    let inputs = RenderOrderInputs {
        roots,
        roots_order_known: Truth::Yes,
        foreground: Vec::new(),
        foreground_order_known: Truth::Yes,
        members,
    };
    let order = DisplayOrder::compute(&inputs);
    let scope = MovingScope {
        animation_targets: Vec::new(),
        camera_moving: Truth::Yes,
        unattributed_movers: false,
    };
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    assert_eq!(suffix.first_moving_index, Some(Num::int(0)));
    assert_eq!(suffix.suffix_len, Num::int(2));
}

// ---------------------------------------------------------------------------
// Lifecycle-fact adapter.
// ---------------------------------------------------------------------------

#[test]
fn re_add_moves_member_to_end_of_display_order() {
    let (sources, _facts, lifecycle) = analyze(&["order_scenes.py"]);
    let sc = scene(&lifecycle, "order_scenes.ReAdd");
    let a = alloc_id(sc, &sources, "Square()");
    let b = alloc_id(sc, &sources, "Circle()");
    let play = &sc.plays[0];
    assert_eq!(play.kind, PlayKind::Play);

    // Curated constructors prove the Mobject default `z_index == 0`, so
    // the raw adapter order is Known for a plain scene — and reflects the
    // §3.4 re-add-to-end effect.
    let inputs = inputs_at_play(sc, play).expect("inputs");
    assert_eq!(inputs.roots, vec![b.clone(), a.clone()]);
    assert_eq!(inputs.members[&a].z_index, Num::int(0));
    assert_eq!(inputs.members[&b].z_index, Num::int(0));
    let order = display_order_at_play(sc, play);
    assert!(is_order_known(&order));
    assert_eq!(ids(&order), vec![b, a]);
}

#[test]
fn set_z_index_literal_reorders_display_order() {
    let (sources, _facts, lifecycle) = analyze(&["z_scenes.py"]);
    let sc = scene(&lifecycle, "z_scenes.ZReorder");
    let a = alloc_id(sc, &sources, "Square()");
    let b = alloc_id(sc, &sources, "Circle()");
    let play = &sc.plays[0];
    assert_eq!(play.kind, PlayKind::Wait);

    let inputs = inputs_at_play(sc, play).expect("inputs");
    assert_eq!(inputs.roots, vec![a.clone(), b.clone()]);
    assert_eq!(inputs.members[&a].z_index, Num::int(0));
    assert_eq!(inputs.members[&b].z_index, Num::int(-1));
    // The stable z sort puts the z == -1 member first despite its later
    // root position.
    let order = display_order_at_play(sc, play);
    assert_eq!(ids(&order), vec![b, a]);
}

#[test]
fn non_literal_set_z_index_poisons_downstream_order_only() {
    let (_sources, _facts, lifecycle) = analyze(&["z_scenes.py"]);
    let sc = scene(&lifecycle, "z_scenes.ZPoison");
    assert_eq!(sc.plays.len(), 2);

    // Before the non-literal write the order is Known...
    let before = display_order_at_play(sc, &sc.plays[0]);
    assert!(is_order_known(&before));

    // ...after it the poisoned member yields the z reason, and no
    // quantitative claim survives (MLP209 prose).
    match display_order_at_play(sc, &sc.plays[1]) {
        DisplayOrder::Unknown(OrderUnknownReason::ZIndexUnknown { .. }) => {}
        other => panic!("expected z-index-unknown order, got {other:?}"),
    }
    assert_eq!(moving_suffix_at_play(sc, &sc.plays[1], Truth::No), None);
}

#[test]
fn family_flatten_and_animation_target_suffix() {
    let (sources, _facts, lifecycle) = analyze(&["order_scenes.py"]);
    let sc = scene(&lifecycle, "order_scenes.Grouped");
    let a = alloc_id(sc, &sources, "Square()");
    let b = alloc_id(sc, &sources, "Circle()");
    let group = alloc_id(sc, &sources, "VGroup(a, b)");
    let extra = alloc_id(sc, &sources, "Dot()");
    let play = &sc.plays[0];

    let mut inputs = inputs_at_play(sc, play).expect("inputs");
    let group_facts = &inputs.members[&group];
    assert_eq!(group_facts.children, vec![a.clone(), b.clone()]);
    assert_eq!(group_facts.children_order_known, Truth::Yes);

    fill_z(&mut inputs, 0);
    let order = DisplayOrder::compute(&inputs);
    assert_eq!(
        ids(&order),
        vec![group.clone(), a.clone(), b.clone(), extra.clone()]
    );

    // The played `group.animate` moves the whole group family; `extra`
    // sits after it, so the suffix spans the three remaining members.
    let scope = moving_scope_at_play(sc, play, Truth::No);
    assert!(!scope.unattributed_movers);
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    assert_eq!(suffix.total_members, 4);
    assert_eq!(suffix.first_moving_index, Some(Num::int(0)));
    assert_eq!(suffix.suffix_len, Num::int(3));
    assert_eq!(
        suffix.members_evidence[0].reason,
        MovingReason::AnimationTarget
    );
}

#[test]
fn updater_bearing_front_member_via_lifecycle_facts() {
    let (sources, _facts, lifecycle) = analyze(&["order_scenes.py"]);
    let sc = scene(&lifecycle, "order_scenes.UpdaterFront");
    let background = alloc_id(sc, &sources, "Square()");
    let play = &sc.plays[0];
    assert_eq!(play.kind, PlayKind::Wait);

    let mut inputs = inputs_at_play(sc, play).expect("inputs");
    assert_eq!(inputs.members[&background].own_updaters, Truth::Yes);
    fill_z(&mut inputs, 0);
    let order = DisplayOrder::compute(&inputs);
    let scope = moving_scope_at_play(sc, play, Truth::No);
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    assert_eq!(suffix.total_members, 3);
    assert_eq!(suffix.first_moving_index, Some(Num::int(0)));
    assert_eq!(suffix.suffix_len, Num::int(2));
    let evidence = moving_suffix_evidence(&order, &suffix).expect("evidence");
    assert!(evidence.contains("position 1/3"), "{evidence}");
}

#[test]
fn foreground_object_widens_moving_scope() {
    let (sources, _facts, lifecycle) = analyze(&["order_scenes.py"]);
    let sc = scene(&lifecycle, "order_scenes.ForegroundScene");
    let b = alloc_id(sc, &sources, "Circle()");
    let play = &sc.plays[0];

    let mut inputs = inputs_at_play(sc, play).expect("inputs");
    assert_eq!(inputs.foreground, vec![b.clone()]);
    fill_z(&mut inputs, 0);
    let order = DisplayOrder::compute(&inputs);
    let members = order.members().expect("known");
    assert_eq!(members.len(), 2);
    assert_eq!(members[1].id, b);
    assert_eq!(
        members[1].provenance,
        MemberProvenance::ForegroundRoot { position: 0 }
    );

    // With the foreground fact the wait has a moving member...
    let scope = moving_scope_at_play(sc, play, Truth::No);
    let suffix = SuffixFact::compute(&order, &inputs, &scope).expect("suffix fact");
    assert_eq!(suffix.first_moving_index, Some(Num::int(1)));
    assert_eq!(suffix.suffix_len, Num::int(0));
    assert_eq!(suffix.members_evidence[0].reason, MovingReason::Foreground);

    // ...without it, nothing would move at all.
    let mut still = inputs.clone();
    still.foreground.clear();
    if let Some(facts) = still.members.get_mut(&b) {
        facts.foreground = Truth::No;
    }
    let still_order = DisplayOrder::compute(&still);
    let still_suffix = SuffixFact::compute(&still_order, &still, &scope).expect("suffix fact");
    assert_eq!(still_suffix.first_moving_index, None);
    assert_eq!(still_suffix.suffix_len, Num::int(0));
    let text = moving_suffix_evidence(&still_order, &still_suffix).expect("evidence");
    assert!(text.contains("no moving family member"), "{text}");
}

#[test]
fn unknown_order_stays_quantification_free() {
    let (_sources, _facts, lifecycle) = analyze(&["order_scenes.py"]);
    let sc = scene(&lifecycle, "order_scenes.Branchy");
    let play = &sc.plays[0];

    let order = display_order_at_play(sc, play);
    assert_eq!(
        order,
        DisplayOrder::Unknown(OrderUnknownReason::RootOrderUnknown)
    );
    assert!(!is_order_known(&order));
    assert_eq!(moving_suffix_at_play(sc, play, Truth::No), None);

    // Even a hand-built suffix fact cannot be rendered into numbers when
    // the order is Unknown (MLP209 prose).
    let fake = SuffixFact {
        total_members: 84,
        first_moving_index: Some(Num::int(0)),
        suffix_len: Num::int(83),
        camera_moving: Truth::No,
        members_evidence: Vec::new(),
    };
    assert_eq!(moving_suffix_evidence(&order, &fake), None);
}
