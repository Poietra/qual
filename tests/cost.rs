//! Symbolic cost model tests (DESIGN §4, roadmap Phase 3 foundations):
//! hot-context identification and transitive propagation, frame estimation
//! from literal durations, and the §6.3 evidence JSON shape.

use std::path::Path;

use serde_json::json;

use manim_lint::application::manim_surface;
use manim_lint::config::model::{Platform, RenderProfile, Renderer};
use manim_lint::cost::CostFacts;
use manim_lint::cost::contexts::HotEntryKind;
use manim_lint::cost::estimator::{frames_for_duration, symbolic_frames};
use manim_lint::cost::model::{frame_buffer_bytes, pixel_frames};
use manim_lint::frontend::index::{FrontendFacts, QualifiedCall, analyze};
use manim_lint::knowledge::{self, KnowledgeProfile};
use manim_lint::semantic::events::Multiplicity;
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

    // Hot MathTex: frame-callback context, symbolic frames (null), no
    // fabricated family size, provenance in state_path.
    let math_tex = call_index(&facts, MATH_TEX, 0);
    assert_eq!(
        cost.evidence_for(math_tex),
        json!({
            "invocation_context": "frame-callback",
            "multiplicity": ["frames"],
            "frames": null,
            "family_size": null,
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
            "family_size": null,
            "renderer": ["cairo"],
            "state_path": [],
        })
    );
}
