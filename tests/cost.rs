//! Symbolic cost model tests (DESIGN §4, roadmap Phase 3 foundations):
//! hot-context identification and transitive propagation, frame estimation
//! from literal durations, and the §6.3 evidence JSON shape.

use std::path::Path;

use serde_json::json;

use manim_lint::application::manim_surface;
use manim_lint::config::model::{Platform, RenderProfile, Renderer};
use manim_lint::cost::contexts::HotEntryKind;
use manim_lint::cost::estimator::{frames_for_duration, symbolic_frames};
use manim_lint::cost::model::{frame_buffer_bytes, pixel_frames};
use manim_lint::cost::thresholds::{
    LARGE_FAMILY_GATE, LARGE_POINTS_GATE, PER_FRAME_ALLOCATION_GATE,
    TRANSFORM_CURVE_INSERTION_GATE, TRANSFORM_FAMILY_GATE, transform_begin_gate_met,
};
use manim_lint::cost::{CostFacts, HotTargetKind};
use manim_lint::frontend::index::{FrontendFacts, QualifiedCall, analyze};
use manim_lint::knowledge::{self, KnowledgeProfile};
use manim_lint::semantic::events::Multiplicity;
use manim_lint::semantic::interpreter::LifecycleFacts;
use manim_lint::semantic::values::Num;
use manim_lint::source::SourceManager;

const MATH_TEX: &str = "manim.mobject.text.tex_mobject.MathTex";
const SCENE_PLAY: &str = "manim.scene.scene.Scene.play";
const SCENE_WAIT: &str = "manim.scene.scene.Scene.wait";
const SCENE_ADD: &str = "manim.scene.scene.Scene.add";
const DOT: &str = "manim.mobject.geometry.arc.Dot";

fn render_profile(fps: f64) -> RenderProfile {
    RenderProfile {
        name: "production".to_owned(),
        renderer: Renderer::Cairo,
        platform: Platform::Linux,
        working_directory: ".".to_owned(),
        pixel_width: 1920,
        pixel_height: 1080,
        frame_rate: fps,
        assets_dir: ".".to_owned(),
        allowed_fonts: Vec::new(),
        cairo_fork_workers: 0,
        cairo_static_layers: false,
        video_encoder: "libx264".to_owned(),
        transparent: false,
        antialias: "default".to_owned(),
        opengl_readback: "auto".to_owned(),
    }
}

/// Parses inline project files and computes cost facts against the shipped
/// upstream 0.20 knowledge profile at 60 FPS.
fn analyzed(files: &[(&str, &str)]) -> (SourceManager, FrontendFacts, KnowledgeProfile) {
    let profile = knowledge::load("upstream_0_20").expect("shipped profile loads");
    let surface = manim_surface(&profile);
    let mut sources = SourceManager::new(".");
    for (path, text) in files {
        sources.load_bytes(Path::new(path), text.as_bytes());
    }
    for file in sources.files() {
        assert!(
            file.is_parsed(),
            "fixture must parse: {}",
            file.relative_path()
        );
    }
    let facts = analyze(&sources, &[".".to_owned()], &surface);
    (sources, facts, profile)
}

fn cost_facts(
    sources: &SourceManager,
    facts: &FrontendFacts,
    profile: &KnowledgeProfile,
    profiles: &[RenderProfile],
) -> CostFacts {
    CostFacts::compute(sources, &facts.index, &facts.calls, Some(profile), profiles)
}

/// Cost facts with the lifecycle cardinalities bridged in, plus the
/// lifecycle facts themselves (for play-level queries).
fn cost_facts_with_lifecycle(
    sources: &SourceManager,
    facts: &FrontendFacts,
    profile: &KnowledgeProfile,
    profiles: &[RenderProfile],
) -> (CostFacts, LifecycleFacts) {
    let lifecycle = manim_lint::semantic::interpreter::analyze(
        sources,
        &facts.index,
        &facts.calls,
        Some(profile),
    );
    let cost = CostFacts::compute_with_lifecycle(
        sources,
        &facts.index,
        &facts.calls,
        Some(profile),
        profiles,
        &lifecycle,
    );
    (cost, lifecycle)
}

/// Index of the `nth` call fact whose candidates contain `candidate`.
fn call_index(facts: &FrontendFacts, candidate: &str, nth: usize) -> usize {
    facts
        .calls
        .calls
        .iter()
        .enumerate()
        .filter(|(_, call)| call.candidates.contains(candidate))
        .map(|(index, _)| index)
        .nth(nth)
        .unwrap_or_else(|| panic!("missing call fact #{nth} for {candidate}"))
}

fn line_of(sources: &SourceManager, call: &QualifiedCall) -> usize {
    sources
        .file(call.file)
        .position_of_byte(usize::from(call.call_range.start()))
        .line
}

const UPDATER_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        sq = Square()
        sq.add_updater(lambda m: m.become(MathTex(f\"x={tracker.get_value()}\")))
        self.play(FadeIn(sq), run_time=2)
        self.wait(3)
        self.wait()
";

#[test]
fn updater_lambda_marks_inner_construction_hot_with_frames_factor() {
    let (sources, facts, profile) = analyzed(&[("scene.py", UPDATER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let hot = cost
        .is_call_in_hot_context(math_tex)
        .expect("MathTex inside the updater lambda must be hot");
    assert_eq!(hot.entry, HotEntryKind::MobjectUpdater);
    assert_eq!(hot.multiplicity.frames, symbolic_frames());
    // Every other factor stays the neutral ×1.
    for (name, value) in hot.multiplicity.factors() {
        if name != "frames" {
            assert_eq!(*value, Num::int(1), "factor {name} must stay neutral");
        }
    }
    assert_eq!(hot.origin.as_deref(), Some("construct"));

    // The construction is queryable for MLP201, and its f-string key marks
    // frame-varying resource keys for MLP226 (K_resource ≈ F).
    assert!(
        cost.constructions_in_hot_contexts()
            .any(|construction| construction.call_index == math_tex
                && construction.symbol == MATH_TEX)
    );
    let resource = cost
        .frame_varying_resource_keys()
        .find(|fact| fact.call_index == math_tex)
        .expect("f-string MathTex key must be frame-varying");
    assert_eq!(resource.keys, symbolic_frames());
}

#[test]
fn play_run_time_literal_binds_frames_interval() {
    let (sources, facts, profile) = analyzed(&[("scene.py", UPDATER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    // play(..., run_time=2) at 60 FPS → about ceil(2 × 60) frames.
    let play = call_index(&facts, SCENE_PLAY, 0);
    assert_eq!(
        cost.frames_for_play(play),
        Num::Interval {
            lo: Some(120.0),
            hi: Some(120.0),
        }
    );

    // wait(3) → about ceil(3 × 60) frames.
    let wait = call_index(&facts, SCENE_WAIT, 0);
    assert_eq!(
        cost.frames_for_play(wait),
        Num::Interval {
            lo: Some(180.0),
            hi: Some(180.0),
        }
    );

    // Evidence carries the real bounds.
    let evidence = cost.evidence_for(play);
    assert_eq!(evidence["frames"], json!({"lower": 120, "upper": 120}));
}

#[test]
fn unknown_duration_stays_per_frame_symbolic_without_numbers() {
    let (sources, facts, profile) = analyzed(&[("scene.py", UPDATER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    // The bare wait() has no literal duration: per-frame symbolic only.
    let bare_wait = call_index(&facts, SCENE_WAIT, 1);
    assert_eq!(cost.frames_for_play(bare_wait), symbolic_frames());

    // Its evidence carries no fabricated frame numbers.
    let evidence = cost.evidence_for(bare_wait);
    assert!(evidence["frames"].is_null(), "unknown frames must be null");

    // A call that is no play at all yields Unknown, not a default.
    let math_tex = call_index(&facts, MATH_TEX, 0);
    assert_eq!(cost.frames_for_play(math_tex), Num::Unknown);
}

const HELPER_SCENE: &str = "\
from manim import *


def helper():
    return MathTex(\"42\")


class Demo(Scene):
    def construct(self):
        helper()
        sq = Square()
        sq.add_updater(lambda m: helper())
        self.wait(1)
";

#[test]
fn transitive_helper_hotness_is_keyed_per_call_context() {
    let (sources, facts, profile) = analyzed(&[("updaters.py", HELPER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    // helper() is called cold from construct (line 10) and hot from the
    // updater lambda (line 12).
    let cold_call = call_index(&facts, "updaters.helper", 0);
    let hot_call = call_index(&facts, "updaters.helper", 1);
    assert_eq!(line_of(&sources, &facts.calls.calls[cold_call]), 10);
    assert_eq!(line_of(&sources, &facts.calls.calls[hot_call]), 12);
    assert!(
        cost.is_call_in_hot_context(cold_call).is_none(),
        "the construct call site stays cold"
    );
    assert!(cost.is_call_in_hot_context(hot_call).is_some());

    // The MathTex inside helper carries exactly the one context chain that
    // goes through the updater — not a blanket "helper is hot" fact.
    let math_tex = call_index(&facts, MATH_TEX, 0);
    let contexts = cost.hot_contexts_for(math_tex);
    assert_eq!(contexts.len(), 1);
    let labels: Vec<&str> = contexts[0]
        .chain
        .iter()
        .map(|step| step.label.as_str())
        .collect();
    assert_eq!(labels, vec!["updater:12", "call:updaters.helper"]);

    // The helper function itself is recorded hot under that chain only.
    let function_contexts = cost
        .hot
        .function_contexts
        .get("updaters.helper")
        .expect("helper must carry a hot function context");
    assert_eq!(function_contexts.len(), 1);
    assert_eq!(function_contexts[0].entry, HotEntryKind::MobjectUpdater);

    // The static "42" key is a hot construction but not frame-varying.
    assert!(
        cost.constructions_in_hot_contexts()
            .any(|construction| construction.call_index == math_tex)
    );
    assert!(
        cost.frame_varying_resource_keys()
            .all(|fact| fact.call_index != math_tex),
        "a static TeX key is not a frame-varying resource"
    );
}

const STOP_CONDITION_SCENE: &str = "\
from manim import *


def expensive():
    return MathTex(\"y\")


class Demo(Scene):
    def construct(self):
        self.wait(stop_condition=lambda: expensive())
";

#[test]
fn wait_stop_condition_is_a_per_frame_entry() {
    let (sources, facts, profile) = analyzed(&[("stops.py", STOP_CONDITION_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let hot = cost
        .is_call_in_hot_context(math_tex)
        .expect("stop_condition helpers run per frame");
    assert_eq!(hot.entry, HotEntryKind::StopCondition);
    assert_eq!(hot.multiplicity.frames, symbolic_frames());
    let labels: Vec<&str> = hot.chain.iter().map(|step| step.label.as_str()).collect();
    assert_eq!(labels, vec!["stop-condition:10", "call:stops.expensive"]);
}

const SCENE_UPDATER_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        self.add_updater(lambda dt: self.add(Dot()))
";

#[test]
fn scene_updater_add_of_fresh_mobject_is_a_hot_graph_mutation() {
    let (sources, facts, profile) = analyzed(&[("mutations.py", SCENE_UPDATER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    let add = call_index(&facts, SCENE_ADD, 0);
    let hot = cost
        .is_call_in_hot_context(add)
        .expect("Scene.add inside a scene updater must be hot");
    assert_eq!(hot.entry, HotEntryKind::SceneUpdater);

    let mutation = cost
        .scene_graph_mutations_in_hot_contexts()
        .find(|mutation| mutation.call_index == add)
        .expect("Scene.add must be recorded as a hot graph mutation");
    assert_eq!(mutation.symbol, SCENE_ADD);
    assert!(
        mutation.adds_fresh_allocation,
        "Dot() argument is a fresh allocation (O(F) growth pattern)"
    );

    // The Dot construction itself is also hot.
    let dot = call_index(&facts, DOT, 0);
    assert!(
        cost.constructions_in_hot_contexts()
            .any(|construction| construction.call_index == dot)
    );
}

#[test]
fn unknown_never_collapses_to_one_in_multiplicity_products() {
    // A symbolic frames factor keeps the product unknown — never ×1.
    let mut per_frame = Multiplicity::one();
    per_frame.frames = symbolic_frames();
    assert_eq!(per_frame.product(), Num::Unknown);

    let mut unknown_factor = Multiplicity::one();
    unknown_factor.frames = Num::Unknown;
    assert_eq!(unknown_factor.product(), Num::Unknown);

    // The pixel-bandwidth helpers inherit the same invariant.
    let profile = render_profile(60.0);
    assert_eq!(pixel_frames(&Num::Unknown, &profile), Num::Unknown);
    assert_eq!(
        frame_buffer_bytes(&profile),
        Num::int(4 * 1920 * 1080),
        "B_frame = 4 × pixel_width × pixel_height"
    );

    // Frame estimation never fabricates a bound from an unknown duration.
    assert_eq!(frames_for_duration(&Num::Unknown, 60.0), symbolic_frames());
    assert_eq!(
        frames_for_duration(&Num::Symbol("run_time".to_owned()), 60.0),
        symbolic_frames()
    );
    // DESIGN §4.2 example: 60 FPS × 8 s ≈ 480 invocations.
    assert_eq!(
        frames_for_duration(&Num::int(8), 60.0),
        Num::Interval {
            lo: Some(480.0),
            hi: Some(480.0),
        }
    );
}

#[test]
fn evidence_json_matches_the_design_shape() {
    let (sources, facts, profile) = analyzed(&[("scene.py", UPDATER_SCENE)]);
    let cost = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);

    // Hot MathTex: frame-callback context, symbolic frames (null),
    // provenance in state_path. Unknown sizes are omitted entirely —
    // never a fabricated number.
    let math_tex = call_index(&facts, MATH_TEX, 0);
    assert_eq!(
        cost.evidence_for(math_tex),
        json!({
            "invocation_context": "frame-callback",
            "multiplicity": ["frames"],
            "frames": null,
            "renderer": ["cairo"],
            "state_path": ["construct", "updater:8"],
        })
    );

    // Cold play with a literal run_time: real frame bounds, no context.
    let play = call_index(&facts, SCENE_PLAY, 0);
    assert_eq!(
        cost.evidence_for(play),
        json!({
            "invocation_context": null,
            "multiplicity": [],
            "frames": {"lower": 120, "upper": 120},
            "renderer": ["cairo"],
            "state_path": [],
        })
    );
}

// ---------------------------------------------------------------------------
// Lifecycle-bridged size facts (DESIGN §4.1 N_family / P / C).
// ---------------------------------------------------------------------------

const SQUARE: &str = "manim.mobject.geometry.polygram.Square";
const CIRCLE: &str = "manim.mobject.geometry.arc.Circle";
const LINE: &str = "manim.mobject.geometry.line.Line";
const RECTANGLE: &str = "manim.mobject.geometry.polygram.Rectangle";
const VGROUP: &str = "manim.mobject.types.vectorized_mobject.VGroup";
const TRANSFORM: &str = "manim.animation.transform.Transform";
const NUMPY_ZEROS: &str = "numpy.zeros";

const GEOMETRY_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        line = Line()
        circle = Circle()
        gridded = Rectangle(grid_xstep=1.0)
        self.add(sq, line, circle, gridded)
";

#[test]
fn curated_geometry_resolves_exact_curve_and_point_counts() {
    let (sources, facts, profile) = analyzed(&[("scene.py", GEOMETRY_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    // polygram.py: Square → Rectangle → Polygon(UR, UL, DL, DR): 4 cubic
    // curves, 4 points each on the Cairo layout.
    let square = call_index(&facts, SQUARE, 0);
    assert_eq!(cost.curves_of_target(square), Num::int(4));
    assert_eq!(cost.points_of_target(square), Num::int(16));
    assert_eq!(cost.family_size_of_target(square), Num::int(1));

    // line.py: default path_arc=0 → one cubic segment.
    let line = call_index(&facts, LINE, 0);
    assert_eq!(cost.curves_of_target(line), Num::int(1));
    assert_eq!(cost.points_of_target(line), Num::int(4));

    // arc.py: Circle → Arc(angle=TAU, num_components=9) → 8 cubic curves.
    let circle = call_index(&facts, CIRCLE, 0);
    assert_eq!(cost.curves_of_target(circle), Num::int(8));
    assert_eq!(cost.points_of_target(circle), Num::int(32));

    // grid_xstep adds Line submobjects (polygram.py Rectangle.__init__):
    // the curated default-path claim is void.
    let gridded = call_index(&facts, RECTANGLE, 0);
    assert_eq!(cost.curves_of_target(gridded), Num::Unknown);
    assert_eq!(cost.points_of_target(gridded), Num::Unknown);

    // Without the lifecycle bridge every size query stays Unknown.
    let plain = cost_facts(&sources, &facts, &profile, &[render_profile(60.0)]);
    assert_eq!(plain.curves_of_target(square), Num::Unknown);
    assert_eq!(plain.family_size_of_target(square), Num::Unknown);
}

const LOOP_GROUP_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        group = VGroup()
        for i in range(10):
            group.add(Square())
        self.add(group)
";

#[test]
fn literal_loop_family_widens_to_open_interval_and_never_confirms_gates() {
    let (sources, facts, profile) = analyzed(&[("scene.py", LOOP_GROUP_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    // The loop-created Square id has Many cardinality and its AddChild is
    // branch-dependent: the family widens to an open-above interval with
    // no fake exact bound.
    let group = call_index(&facts, VGROUP, 0);
    let family = cost.family_size_of_target(group);
    assert_eq!(
        family,
        Num::Interval {
            lo: Some(1.0),
            hi: None,
        }
    );
    let curves = cost.curves_of_target(group);
    assert_eq!(
        curves,
        Num::Interval {
            lo: Some(0.0),
            hi: None,
        }
    );

    // An open-above interval proves no minimum: no gate is confirmed.
    assert!(!LARGE_FAMILY_GATE.confirmed_by(&family));
    assert!(!TRANSFORM_FAMILY_GATE.confirmed_by(&family));
    assert!(!TRANSFORM_CURVE_INSERTION_GATE.confirmed_by(&curves));
}

/// A construct body building `VGroup(Square(), ... × 40)` in a straight
/// line: every member edge is definite and singleton.
fn large_group_scene(updater_line: &str) -> String {
    let members = std::iter::repeat_n("Square()", 40)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "from manim import *\n\n\nclass Demo(Scene):\n    def construct(self):\n        group = VGroup({members})\n{updater_line}        self.add(group)\n        self.wait(1)\n"
    )
}

#[test]
fn straight_line_group_confirms_large_family_with_exact_counts() {
    let source = large_group_scene("");
    let (sources, facts, profile) = analyzed(&[("scene.py", source.as_str())]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let group = call_index(&facts, VGROUP, 0);
    let family = cost.family_size_of_target(group);
    assert_eq!(family, Num::int(41), "group + 40 definite members");
    assert_eq!(cost.curves_of_target(group), Num::int(160));
    let points = cost.points_of_target(group);
    assert_eq!(points, Num::int(640));

    assert!(LARGE_FAMILY_GATE.confirmed_by(&family));
    assert!(!LARGE_POINTS_GATE.confirmed_by(&points), "640 < 1024");

    // Evidence carries the real bounds and round-trips through JSON.
    let evidence = cost.evidence_for(group);
    assert_eq!(evidence["family_size"], json!({"lower": 41, "upper": 41}));
    assert_eq!(evidence["points"], json!({"lower": 640, "upper": 640}));
}

#[test]
fn hot_copy_and_family_walk_resolve_updater_host_sizes() {
    let copy_scene = large_group_scene("        group.add_updater(lambda m: m.copy())\n");
    let (sources, facts, profile) = analyzed(&[("scene.py", copy_scene.as_str())]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let copy: Vec<_> = cost.copy_align_targets_in_hot_contexts().collect();
    assert_eq!(copy.len(), 1, "exactly one hot copy fact");
    assert_eq!(copy[0].kind, HotTargetKind::Copy);
    assert_eq!(copy[0].method, "copy");
    assert_eq!(copy[0].scene, "scene.Demo");
    assert_eq!(copy[0].sizes.family, Num::int(41));
    assert!(LARGE_FAMILY_GATE.confirmed_by(&copy[0].sizes.family));
    assert_eq!(
        LARGE_FAMILY_GATE.citation(),
        "N_family >= 32 (MLP202/MLP203 emission gate, DESIGN 7.3)"
    );

    let walk_scene = large_group_scene("        group.add_updater(lambda m: m.get_family())\n");
    let (sources, facts, profile) = analyzed(&[("scene.py", walk_scene.as_str())]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);
    let walks: Vec<_> = cost.family_walk_queries_in_hot_contexts().collect();
    assert_eq!(walks.len(), 1);
    assert_eq!(walks[0].kind, HotTargetKind::FamilyWalk);
    assert_eq!(walks[0].method, "get_family");
    assert_eq!(walks[0].sizes.family, Num::int(41));
}

const TRANSFORM_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        circle = Circle()
        self.play(Transform(sq, circle))
";

#[test]
fn transform_between_known_sizes_yields_exact_curve_delta() {
    let (sources, facts, profile) = analyzed(&[("scene.py", TRANSFORM_SCENE)]);
    let (cost, lifecycle) =
        cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let scene = lifecycle.scene("scene.Demo").expect("scene analyzed");
    assert_eq!(scene.plays.len(), 1);
    let play = &scene.plays[0];
    assert_eq!(play.animations.len(), 1);
    let delta = cost.curve_delta_for_transform("scene.Demo", play, &play.animations[0]);
    assert_eq!(delta, Num::int(4), "|C(Square)=4 − C(Circle)=8|");

    // Small family + small insertion: the MLP207/208 begin gate stays shut.
    let transform = call_index(&facts, TRANSFORM, 0);
    let source_family = cost.family_size_of_target(call_index(&facts, SQUARE, 0));
    assert!(!transform_begin_gate_met(&source_family, &delta));
    let _ = transform;
}

const POISONED_TRANSFORM_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        circle = Circle()
        sq.become(circle)
        self.play(Transform(sq, circle))
";

#[test]
fn count_changing_mutation_before_play_voids_the_curve_delta() {
    let (sources, facts, profile) = analyzed(&[("scene.py", POISONED_TRANSFORM_SCENE)]);
    let (cost, lifecycle) =
        cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let scene = lifecycle.scene("scene.Demo").expect("scene analyzed");
    let play = &scene.plays[0];
    // `become` rewrote sq's geometry before the play: no fake delta.
    assert_eq!(
        cost.curve_delta_for_transform("scene.Demo", play, &play.animations[0]),
        Num::Unknown
    );
    let _ = facts;
}

const ALLOCATION_SCENE: &str = "\
from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        n = 100
        sq = Square()
        sq.add_updater(lambda m: np.zeros(100000))
        sq.add_updater(lambda m: np.zeros(3))
        sq.add_updater(lambda m: np.zeros(n))
        sq.add_updater(lambda m: np.zeros(100000, dtype=\"float32\"))
        self.wait(1)
";

#[test]
fn per_frame_allocation_bytes_from_literal_ndarray_sizes_only() {
    let (sources, facts, profile) = analyzed(&[("scene.py", ALLOCATION_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    // 100_000 float64 elements = 800_000 bytes: above the 64 KiB gate.
    let large = call_index(&facts, NUMPY_ZEROS, 0);
    let bytes = cost.per_frame_allocation_bytes(large);
    assert_eq!(bytes, Num::int(800_000));
    assert!(PER_FRAME_ALLOCATION_GATE.confirmed_by(&bytes));
    assert!(cost.is_call_in_hot_context(large).is_some());

    // A small coordinate-sized vector is provable but below the gate.
    let small = call_index(&facts, NUMPY_ZEROS, 1);
    assert_eq!(cost.per_frame_allocation_bytes(small), Num::int(24));
    assert!(!PER_FRAME_ALLOCATION_GATE.confirmed_by(&cost.per_frame_allocation_bytes(small)));

    // A non-literal length is Unknown and never passes the gate.
    let dynamic = call_index(&facts, NUMPY_ZEROS, 2);
    assert_eq!(cost.per_frame_allocation_bytes(dynamic), Num::Unknown);
    assert!(!PER_FRAME_ALLOCATION_GATE.confirmed_by(&Num::Unknown));

    // dtype="float32" halves the element size.
    let typed = call_index(&facts, NUMPY_ZEROS, 3);
    assert_eq!(cost.per_frame_allocation_bytes(typed), Num::int(400_000));
}

const TUPLE_ALLOCATION_SCENE: &str = "\
from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        n = 100
        sq = Square()
        sq.add_updater(lambda m: np.zeros((400, 400)))
        sq.add_updater(lambda m: np.zeros(()))
        sq.add_updater(lambda m: np.zeros((n, 400)))
        sq.add_updater(lambda m: np.zeros((400, -1)))
        sq.add_updater(lambda m: np.ones([64, 128], dtype=\"float32\"))
        self.wait(1)
";

#[test]
fn per_frame_allocation_bytes_multiplies_literal_tuple_shapes() {
    let (sources, facts, profile) = analyzed(&[("scene.py", TUPLE_ALLOCATION_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    // (400, 400) float64: 160_000 × 8 bytes, above the 64 KiB gate.
    let matrix = call_index(&facts, NUMPY_ZEROS, 0);
    let bytes = cost.per_frame_allocation_bytes(matrix);
    assert_eq!(bytes, Num::int(1_280_000));
    assert!(PER_FRAME_ALLOCATION_GATE.confirmed_by(&bytes));

    // An empty shape is a 0-d array holding exactly one element.
    let scalar = call_index(&facts, NUMPY_ZEROS, 1);
    assert_eq!(cost.per_frame_allocation_bytes(scalar), Num::int(8));

    // A non-literal dimension voids the whole shape — never guessed.
    let dynamic = call_index(&facts, NUMPY_ZEROS, 2);
    assert_eq!(cost.per_frame_allocation_bytes(dynamic), Num::Unknown);

    // A negative dimension is a runtime error, not an allocation proof.
    let negative = call_index(&facts, NUMPY_ZEROS, 3);
    assert_eq!(cost.per_frame_allocation_bytes(negative), Num::Unknown);

    // List shapes multiply the same way, and the dtype argument applies.
    let listed = call_index(&facts, "numpy.ones", 0);
    assert_eq!(cost.per_frame_allocation_bytes(listed), Num::int(32_768));
}

#[test]
fn unknown_and_symbolic_sizes_never_confirm_thresholds() {
    for gate in &manim_lint::cost::thresholds::ALL_GATES {
        assert!(!gate.confirmed_by(&Num::Unknown), "{}", gate.name);
        assert!(
            !gate.confirmed_by(&Num::Symbol("family".to_owned())),
            "{}",
            gate.name
        );
        assert!(
            !gate.confirmed_by(&Num::Interval {
                lo: None,
                hi: Some(1e9),
            }),
            "open-below never confirms {}",
            gate.name
        );
    }
    // A proven lower bound at the minimum confirms; just below does not.
    assert!(TRANSFORM_FAMILY_GATE.confirmed_by(&Num::int(32)));
    assert!(!TRANSFORM_FAMILY_GATE.confirmed_by(&Num::int(31)));
    assert!(TRANSFORM_FAMILY_GATE.confirmed_by(&Num::Interval {
        lo: Some(50.0),
        hi: None,
    }));
    assert!(transform_begin_gate_met(&Num::int(40), &Num::Unknown));
    assert!(transform_begin_gate_met(&Num::Unknown, &Num::int(256)));
    assert!(!transform_begin_gate_met(&Num::Unknown, &Num::Unknown));

    // The citable evidence carries the gate identity and real bounds.
    let evidence = TRANSFORM_FAMILY_GATE.evidence(&Num::int(40));
    assert_eq!(
        evidence,
        json!({
            "threshold": "transform-family-gate",
            "quantity": "N_family",
            "minimum": 32.0,
            "value": {"lower": 40, "upper": 40},
            "confirmed": true,
        })
    );
    assert_eq!(
        TRANSFORM_FAMILY_GATE.evidence(&Num::Unknown),
        json!({
            "threshold": "transform-family-gate",
            "quantity": "N_family",
            "minimum": 32.0,
            "value": null,
            "confirmed": false,
        })
    );
}

// ---------------------------------------------------------------------------
// Execution liveness (DESIGN §3.2 suspension, §3.3 wait dynamics, §15).
// ---------------------------------------------------------------------------

/// The review's canonical false positive: `FadeIn(label)` suspends the
/// host's updaters (animation.py `begin` → `suspend_updating`) and the
/// un-`dt`'d factory leaves `wait(3)` static, so the callback provably
/// never runs per frame — no play, no maybe, no fabricated quantity.
const SUSPENDED_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(label)
        self.play(FadeIn(label), run_time=2)
        self.wait(3)
";

#[test]
fn suspended_updater_has_no_execution_plays_and_no_quantity() {
    let (sources, facts, profile) = analyzed(&[("scene.py", SUSPENDED_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    // Reachability (hotness) is still a fact — the callback WOULD be hot.
    assert!(cost.is_call_in_hot_context(math_tex).is_some());
    // But liveness proves it never executes per frame.
    let execution = cost.call_execution(math_tex);
    assert!(!execution.has_proven(), "suspended host: no proven play");
    assert!(
        !execution.possibly_executes(),
        "FadeIn suspension + static wait is provable, not a maybe"
    );
    assert_eq!(cost.proven_frames_for_call(math_tex), None);
}

/// The same callback becomes provably live when the play animates an
/// unrelated mobject: the host stays un-suspended and in the family, so
/// exactly the play's ~120 frames are proven; the static wait(3) is
/// excluded.
const LIVE_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        square = Square()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(square, label)
        self.play(FadeIn(square), run_time=2)
        self.wait(3)
";

#[test]
fn untargeted_host_proves_execution_during_the_play_only() {
    let (sources, facts, profile) = analyzed(&[("scene.py", LIVE_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert_eq!(execution.proven.len(), 1, "exactly the FadeIn(square) play");
    assert!(execution.maybe.is_empty(), "the static wait is excluded");
    assert!(!execution.unresolved);
    // 2 s at 60 fps: the wait's 180 frames must NOT be added.
    assert_eq!(
        cost.proven_frames_for_call(math_tex),
        Some(Num::Interval {
            lo: Some(120.0),
            hi: Some(120.0),
        })
    );
}

/// `suspend_mobject_updating=False` is the DESIGN §3.2 escape hatch: the
/// animation targets the host but leaves its updaters running, so the
/// play still proves execution.
const ESCAPE_HATCH_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(label)
        self.play(FadeIn(label, suspend_mobject_updating=False), run_time=2)
";

#[test]
fn suspend_mobject_updating_false_keeps_the_targeted_host_live() {
    let (sources, facts, profile) = analyzed(&[("scene.py", ESCAPE_HATCH_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert_eq!(execution.proven.len(), 1);
    assert_eq!(
        cost.proven_frames_for_call(math_tex),
        Some(Num::Interval {
            lo: Some(120.0),
            hi: Some(120.0),
        })
    );
}

/// A `dt`-named parameter makes the updater time-based, so the plain
/// `wait(3)` renders dynamically (scene.py `should_update_mobjects`) and
/// proves the callback's execution across its ~180 frames.
const DT_WAIT_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(lambda m, dt: m.become(MathTex(f\"t={dt}\")))
        self.add(sq)
        self.wait(3)
";

#[test]
fn time_based_updater_proves_execution_during_dynamic_waits() {
    let (sources, facts, profile) = analyzed(&[("scene.py", DT_WAIT_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert_eq!(execution.proven.len(), 1, "the dynamic wait counts");
    assert_eq!(
        cost.proven_frames_for_call(math_tex),
        Some(Num::Interval {
            lo: Some(180.0),
            hi: Some(180.0),
        })
    );
}

/// A branch-dependent registration proves nothing: the play is only a
/// maybe-execution, so no quantity and no proven play exist (a high-
/// confidence rule must stay silent on this evidence).
const BRANCH_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        other = Circle()
        if unknowable():
            sq.add_updater(lambda m: m.become(MathTex(\"x\")))
        self.add(sq, other)
        self.play(FadeIn(other), run_time=2)
";

#[test]
fn branch_dependent_registration_yields_maybe_evidence_only() {
    let (sources, facts, profile) = analyzed(&[("scene.py", BRANCH_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert!(!execution.has_proven(), "maybe-registration never proves");
    assert!(
        execution.possibly_executes(),
        "the play may run the maybe-registered callback"
    );
    assert_eq!(cost.proven_frames_for_call(math_tex), None);
}

/// An all-paths `remove_updater` before the play deactivates the
/// registration: the pre-play updater set no longer carries it, so the
/// play proves nothing.
const REMOVED_SCENE: &str = "\
from manim import *


def spin(m):
    m.become(MathTex(\"x\"))


class Demo(Scene):
    def construct(self):
        sq = Square()
        other = Circle()
        sq.add_updater(spin)
        sq.remove_updater(spin)
        self.add(sq, other)
        self.play(FadeIn(other), run_time=2)
";

#[test]
fn all_paths_removal_before_the_play_deactivates_the_registration() {
    let (sources, facts, profile) = analyzed(&[("scene.py", REMOVED_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert!(!execution.has_proven());
    assert!(!execution.possibly_executes());
    assert_eq!(cost.proven_frames_for_call(math_tex), None);
}

/// A non-literal `suspend_mobject_updating` value proves nothing about
/// suspension: the targeted host's execution becomes a maybe, never a
/// proven claim in either direction (DESIGN §15 invariant 2).
const NONLITERAL_SUSPEND_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        flag = unknowable()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(label)
        self.play(FadeIn(label, suspend_mobject_updating=flag), run_time=2)
";

#[test]
fn nonliteral_suspend_kwarg_degrades_to_maybe_execution() {
    let (sources, facts, profile) = analyzed(&[("scene.py", NONLITERAL_SUSPEND_SCENE)]);
    let (cost, _) = cost_facts_with_lifecycle(&sources, &facts, &profile, &[render_profile(60.0)]);

    let math_tex = call_index(&facts, MATH_TEX, 0);
    let execution = cost.call_execution(math_tex);
    assert!(
        !execution.has_proven(),
        "unknown suspension never proves execution"
    );
    assert!(
        execution.possibly_executes(),
        "unknown suspension never proves absence either"
    );
    assert_eq!(cost.proven_frames_for_call(math_tex), None);
}
